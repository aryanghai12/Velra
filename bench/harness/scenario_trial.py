#!/usr/bin/env python3
"""Drive one controlled session for one scenario, one arm.

Both arms run this script with the same scenario, the same turn script, the
same model and the same flags. The only difference is ``--arm``:

  baseline   `velra disable` — the hooks are removed from the Claude Code
             settings file, so Claude Code's own compaction summary is the
             only continuation.
  velra      `velra enable` — the hooks are registered.

Both arms *write* to the settings file, so its state is never a confound: the
baseline explicitly removes the hooks rather than assuming they are absent.

Everything the session emits is written verbatim to ``stream.jsonl``; nothing
is summarised at capture time. The end state of the fixture — every file the
scenario cares about, the suite's exit code, the diff — is captured into
``final_state.json`` so that the behavioural scoring is a pure function of
bytes on disk and never needs the fixture directory again.

This is the v0.1 ``run_trial.py`` generalised: the turn script comes from the
scenario rather than being hard-coded, the compaction boundary moves with it,
and the final tree is captured rather than left in a temporary directory that
the next replicate overwrites.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO_ROOT / "bench"))

import claude_binary  # noqa: E402
import provenance  # noqa: E402
import prereg  # noqa: E402
from scenarios import registry  # noqa: E402

CLAUDE = claude_binary.resolve()


def run(cmd, **kw):
    # Velra prints U+2713 and U+26A1. Decoding with the locale encoding writes
    # mojibake into the evidence on Windows, which would make the captures
    # misrepresent the binary they are evidence for.
    kw.setdefault("encoding", "utf-8")
    kw.setdefault("errors", "replace")
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def capture_final_state(fixture: pathlib.Path, manifest: dict) -> dict:
    """Everything the behavioural scoring needs from the end of the session."""
    wanted: set[str] = set()
    for key in ("true_fix_file", "dead_end_file"):
        if manifest.get(key):
            wanted.add(manifest[key])
    for key in ("dead_end_files", "sibling_files", "uncommitted_files"):
        wanted.update(manifest.get(key) or [])
    for entry in manifest.get("required_final_state") or []:
        wanted.add(entry["file"])
    if manifest.get("failing_test"):
        wanted.add(manifest["failing_test"].split("::")[0])

    files = {}
    for rel in sorted(wanted):
        path = fixture / rel
        if path.is_file():
            files[rel] = path.read_text(encoding="utf-8", errors="replace")

    pytest_after = run([sys.executable, "-m", "pytest", "-q"], cwd=str(fixture))
    diff = run(["git", "diff"], cwd=str(fixture))
    status = run(["git", "status", "--porcelain"], cwd=str(fixture))
    log = run(["git", "log", "--oneline", "-20"], cwd=str(fixture))
    return {
        "files": files,
        "pytest_exit": pytest_after.returncode,
        "pytest_tail": (pytest_after.stdout or "").strip().splitlines()[-3:],
        "pytest_output": (pytest_after.stdout or "")[-8000:],
        "git_diff": diff.stdout,
        "git_status": status.stdout,
        "git_log": log.stdout,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--scenario", required=True)
    ap.add_argument("--arm", choices=("baseline", "velra"), required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--fixture", required=True)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--replicate", type=int, default=1)
    ap.add_argument("--max-budget-usd", type=float, default=12.0)
    ap.add_argument("--velra-binary", default=str(
        REPO_ROOT / "target" / "release" /
        ("velra.exe" if os.name == "nt" else "velra")))
    args = ap.parse_args()

    scenario = registry.get(args.scenario)
    turns = list(scenario.turns)
    out = pathlib.Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    fixture = pathlib.Path(args.fixture).resolve()
    velra_bin = pathlib.Path(args.velra_binary).resolve()

    # ---- 1. a fresh, verified fixture, identical for every trial ----------
    manifest = scenario.build(fixture)
    (out / "fixture_manifest.json").write_text(
        json.dumps(manifest, indent=2), encoding="utf-8", newline="")

    # ---- 2. an isolated Velra state directory for this trial ---------------
    velra_home = out / "velra_home"
    shutil.rmtree(velra_home, ignore_errors=True)
    velra_home.mkdir(parents=True, exist_ok=True)

    env = dict(os.environ)
    # Never let the outer Claude Code session leak into the trial.
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)
    env["VELRA_HOME"] = str(velra_home)
    env["VELRA_LOG"] = "debug"
    env["CLAUDE_PROJECT_DIR"] = str(fixture)

    # ---- 3. set the arm ---------------------------------------------------
    action = "enable" if args.arm == "velra" else "disable"
    setup = run([str(velra_bin), action], env=env)
    (out / "arm_setup.txt").write_text(
        f"$ velra {action}\nexit={setup.returncode}\n{setup.stdout}\n{setup.stderr}\n",
        encoding="utf-8", newline="")
    status_before = run([str(velra_bin), "status"], env=env)
    (out / "velra_status_before.txt").write_text(
        f"exit={status_before.returncode}\n{status_before.stdout}\n{status_before.stderr}",
        encoding="utf-8", newline="")

    # ---- 4. drive the session ---------------------------------------------
    cmd = [
        str(CLAUDE), "-p",
        "--input-format", "stream-json",
        "--output-format", "stream-json",
        "--verbose",
        "--include-hook-events",
        "--model", args.model,
        "--permission-mode", "bypassPermissions",
        "--permission-prompts", "none",
        "--strict-mcp-config",
        "--max-budget-usd", str(args.max_budget_usd),
        "--autocompact", "auto",
    ]
    (out / "command.txt").write_text(" ".join(cmd), encoding="utf-8", newline="")

    stream_path = out / "stream.jsonl"
    timing_path = out / "stream_timing.jsonl"
    started = time.time()
    proc = subprocess.Popen(
        cmd, cwd=str(fixture), env=env,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", errors="replace", bufsize=1)

    turn_index = 0
    turn_marks: list[dict] = []
    session_id = None
    compact_status = None

    def send(text: str) -> None:
        proc.stdin.write(json.dumps({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        }) + "\n")
        proc.stdin.flush()

    timing = open(timing_path, "w", encoding="utf-8", newline="")
    with open(stream_path, "w", encoding="utf-8", newline="") as log:
        # The raw stream has no turn numbering of its own; these markers are
        # the only thing added to it.
        log.write(json.dumps({"_velra_bench": "turn_start", "turn": 0,
                              "text": turns[0], "t": 0.0}) + "\n")
        send(turns[0])

        for line in proc.stdout:
            recv = time.time() - started
            line = line.strip()
            if not line:
                continue
            log.write(line + "\n")
            log.flush()
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue

            if obj.get("type") == "system" and str(obj.get("subtype", "")).startswith("hook_"):
                timing.write(json.dumps({
                    "t_recv": round(recv, 6),
                    "turn": turn_index,
                    "subtype": obj.get("subtype"),
                    "hook_id": obj.get("hook_id"),
                    "hook_name": obj.get("hook_name"),
                    "hook_event": obj.get("hook_event"),
                    "exit_code": obj.get("exit_code"),
                    "outcome": obj.get("outcome"),
                    "stdout_len": len(obj.get("stdout") or ""),
                    "stderr_len": len(obj.get("stderr") or ""),
                    "output_len": len(obj.get("output") or ""),
                }) + "\n")
                timing.flush()

            if obj.get("session_id") and session_id is None:
                session_id = obj["session_id"]

            if obj.get("type") == "system" and obj.get("compact_result"):
                compact_status = {"compact_result": obj.get("compact_result"),
                                  "compact_error": obj.get("compact_error"),
                                  "at_turn": turn_index}

            if obj.get("type") != "result":
                continue

            elapsed = time.time() - started
            turn_marks.append({
                "turn": turn_index,
                "elapsed_s": round(elapsed, 3),
                "duration_ms": obj.get("duration_ms"),
                "total_cost_usd": obj.get("total_cost_usd"),
                "num_turns": obj.get("num_turns"),
                "subtype": obj.get("subtype"),
                "usage": obj.get("usage"),
            })
            print(f"  turn {turn_index}/{len(turns) - 1} done "
                  f"({obj.get('subtype')}, {elapsed:.1f}s, "
                  f"${obj.get('total_cost_usd')})", flush=True)
            turn_index += 1
            if turn_index < len(turns):
                log.write(json.dumps({
                    "_velra_bench": "turn_start", "turn": turn_index,
                    "text": turns[turn_index],
                    "t": round(time.time() - started, 3)}) + "\n")
                send(turns[turn_index])
            else:
                proc.stdin.close()
                break

    timing.close()
    stderr = proc.stderr.read()
    try:
        proc.wait(timeout=120)
    except subprocess.TimeoutExpired:
        proc.kill()
    wall = time.time() - started

    # ---- 5. collect the evidence ------------------------------------------
    for name, argv in (("velra_status_after.txt", ["status"]),
                       ("velra_doctor_after.txt", ["doctor"])):
        result = run([str(velra_bin), *argv], env=env)
        (out / name).write_text(
            f"exit={result.returncode}\n{result.stdout}\n{result.stderr}",
            encoding="utf-8", newline="")

    if args.arm == "velra":
        for name in ("velra.db", "velra.db-wal", "velra.db-shm"):
            src = velra_home / name
            if src.exists():
                shutil.copy2(src, out / name)
        debug = velra_home / "logs" / "debug.log"
        if debug.exists():
            shutil.copy2(debug, out / "velra_debug.log")

    transcript_src = None
    projects = pathlib.Path(os.path.expanduser("~/.claude/projects"))
    if session_id and projects.exists():
        for candidate in projects.rglob(f"{session_id}.jsonl"):
            transcript_src = candidate
            shutil.copy2(candidate, out / "transcript.jsonl")
            break

    final_state = capture_final_state(fixture, manifest)
    (out / "final_state.json").write_text(
        json.dumps(final_state, indent=2), encoding="utf-8", newline="")

    meta = {
        "scenario": args.scenario,
        "arm": args.arm,
        "replicate": args.replicate,
        "model": args.model,
        "session_id": session_id,
        "transcript_source": str(transcript_src) if transcript_src else None,
        "fixture": str(fixture),
        "velra_home": str(velra_home),
        "wall_seconds": round(wall, 3),
        "turns_sent": turn_index,
        "turns_expected": len(turns),
        "compact_turn_index": scenario.compact_index,
        "measured_turn_index": scenario.measured_index,
        "compact_status": compact_status,
        "turn_marks": turn_marks,
        "final_pytest_exit": final_state["pytest_exit"],
        "stderr": stderr[-4000:],
        "manifest": manifest,
        "claude_binary": str(CLAUDE),
        "claude_version": claude_binary.version_of(CLAUDE),
        "velra_binary": str(velra_bin),
        "provenance": provenance.collect(velra_bin),
        **prereg.stamp(),
    }
    (out / "trial_meta.json").write_text(
        json.dumps(meta, indent=2), encoding="utf-8", newline="")

    print(json.dumps({k: meta[k] for k in
                      ("scenario", "arm", "replicate", "session_id",
                       "wall_seconds", "turns_sent", "turns_expected",
                       "compact_status", "final_pytest_exit")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
