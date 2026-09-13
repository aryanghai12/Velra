#!/usr/bin/env python3
"""Drive one controlled Claude Code trial against the benchmark fixture.

Both arms of the benchmark run this same script with the same turn script and
the same flags. The *only* difference is ``--arm``:

  baseline   Velra's hooks are removed from the Claude Code settings file.
  velra      Velra's hooks are registered before the session starts.

The session is driven non-interactively through ``--input-format stream-json``,
which is what makes the two arms genuinely comparable: every user turn is a
fixed string, sent in a fixed order, so the only free variable is the agent's
own behaviour.

Everything the session emits is written verbatim to ``stream.jsonl`` in the
output directory; nothing is summarised at capture time, so the analysis in
Phase 4 can be re-run against the raw record.
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

CLAUDE = pathlib.Path(os.path.expanduser(
    "~/.vscode/extensions/anthropic.claude-code-2.1.269-win32-x64"
    "/resources/native-binary/claude.exe"
))

# --------------------------------------------------------------------------
# The turn script. Identical for both arms, byte for byte.
# --------------------------------------------------------------------------
#
# Turns 1-7 build a realistic working session: the failure is found, the three
# modules are read, the intuitive rounding fix is tried and fails, and it is
# discarded with `git restore`. Turn 8 compacts. Turn 9 is the measured turn.

TURNS = [
    # 1. establish the objective and the failure
    "This repository has a failing test. Run the test suite with "
    "`python -m pytest -q` and tell me exactly which test fails, at which "
    "line, and what the assertion says. Do not change any code yet.",

    # 2. build file context
    "Read src/ledger/money.py, src/ledger/rules.py and src/ledger/engine.py, "
    "then summarise in a few lines how a settlement total is computed.",

    # 3. the dead end, on purpose
    "The gap is exactly one cent, so let's start with the rounding "
    "hypothesis. Edit src/ledger/money.py so that Money.scaled uses "
    "ROUND_HALF_EVEN instead of ROUND_HALF_UP, then run the test suite again.",

    # 4. discard it through git, which is what Velra observes
    "That made things worse. Discard that change with "
    "`git restore src/ledger/money.py`, then run the suite again to confirm "
    "we are back to a single failure.",

    # 5-7. more context, no code changes
    "Read tests/test_engine.py and list every test in it and what each one "
    "asserts.",

    "Check `git log --oneline` and tell me whether anyone has attempted a fix "
    "for this before, and what they tried.",

    "Summarise in two sentences where we stand right now.",

    # 8. the compaction boundary
    "/compact",

    # 9. THE MEASURED TURN
    "Fix the remaining test failure.",
]

# --------------------------------------------------------------------------
# The saturated protocol.
# --------------------------------------------------------------------------
#
# The short protocol above turned out not to test the hypothesis at all: nine
# turns of work over three tiny modules compacts into a summary that still
# holds every relevant fact, so there is no amnesia for anything to fix. A fair
# test needs the conversation to be genuinely larger than a summary can carry.
#
# So turns 4-11 are a real, unrelated audit across ~84 modules. They are not
# padding: each one is work an engineer might actually ask for, and each one
# pushes tens of thousands of tokens of file content into the conversation.
# By the time `/compact` arrives, the failing test is eleven turns old and the
# summariser has an audit to describe as well.
#
# Nothing in this script ever asks the agent to diagnose the settlement bug
# before compaction. The short protocol did (its "summarise where we stand"
# turn), which handed the summariser a ready-made answer and flattered the
# baseline.

SATURATION_TURNS = [
    "Park the bug for a moment, I need an audit first. Read every module "
    "under src/ledger/adapters and list, for each one, the exception classes "
    "it defines and the currencies it supports.",

    "Now read every module under src/ledger/reporting and list, for each one, "
    "its COLUMNS tuple and its grouping key.",

    "Now read every module under src/ledger/importers and tell me which ones "
    "use an explicit delimiter and which split on whitespace, with the "
    "RECORD_KIND of each.",

    "Now read every module under src/ledger/validation and list each one's "
    "MAX_LENGTH and the label it validates.",

    "Across all the adapter modules, group them by their DECLINE_STATUS value "
    "and tell me which ones share each status.",

    "List every call site of Money.from_str across the whole codebase, with "
    "the file and the line number for each.",

    "Which modules import ledger.money directly? Give me the complete list, "
    "grouped by package.",

    "Summarise the audit findings so far in a short table.",
]

PROTOCOLS = {
    # name: (turns, compact index, measured index)
    #
    # "saturated" keeps turns 0-5 (find the failure, read the three modules,
    # try the rounding fix, discard it, read the tests, check the history),
    # drops the old turn 6 ("summarise where we stand") because that turn made
    # the agent state the diagnosis immediately before compaction, and then
    # runs the eight audit turns before the compaction boundary.
    "short": (TURNS, 7, 8),
    "saturated": (TURNS[:6] + SATURATION_TURNS + TURNS[7:], 14, 15),
}


def run(cmd, **kw):
    # Velra prints U+2713 and U+26A1. Decoding with the locale encoding
    # writes mojibake into the evidence files on Windows, which would make
    # the captures misrepresent the binary they are evidence for.
    kw.setdefault("encoding", "utf-8")
    kw.setdefault("errors", "replace")
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def velra(binary: pathlib.Path, env: dict, *args: str):
    return run([str(binary), *args], env=env)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", choices=("baseline", "velra"), required=True)
    ap.add_argument("--out", required=True, help="output directory for this trial")
    ap.add_argument("--fixture", required=True, help="where to build the fixture repo")
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--replicate", type=int, default=1)
    ap.add_argument("--protocol", choices=tuple(PROTOCOLS), default="saturated")
    ap.add_argument("--noise", action="store_true", default=None,
                    help="build the fixture with its surrounding codebase "
                         "(default: on for the saturated protocol)")
    ap.add_argument("--max-budget-usd", type=float, default=12.0)
    ap.add_argument("--velra-binary",
                    default=str(REPO_ROOT / "target" / "release" / "velra.exe"))
    args = ap.parse_args()
    turns, compact_idx, measured_idx = PROTOCOLS[args.protocol]
    use_noise = args.noise if args.noise is not None else (args.protocol == "saturated")

    out = pathlib.Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    fixture = pathlib.Path(args.fixture).resolve()
    velra_bin = pathlib.Path(args.velra_binary).resolve()

    # ---- 1. a fresh fixture repository, identical for every trial ---------
    mk = run([sys.executable, str(REPO_ROOT / "bench" / "fixture" / "make_fixture.py"),
              str(fixture)] + (["--noise"] if use_noise else []))
    if mk.returncode != 0:
        print(mk.stdout, mk.stderr, file=sys.stderr)
        return 1
    manifest = json.loads(mk.stdout)
    (out / "fixture_manifest.json").write_text(
        json.dumps(manifest, indent=2), encoding="utf-8", newline="")

    # ---- 2. an isolated Velra state directory for this trial --------------
    velra_home = out / "velra_home"
    if velra_home.exists():
        shutil.rmtree(velra_home, ignore_errors=True)
    velra_home.mkdir(parents=True, exist_ok=True)

    env = dict(os.environ)
    # Never let the outer Claude Code session leak into the trial.
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)
    env["VELRA_HOME"] = str(velra_home)
    env["VELRA_LOG"] = "debug"          # phase timings for the overhead analysis
    env["CLAUDE_PROJECT_DIR"] = str(fixture)

    # ---- 3. set the arm ---------------------------------------------------
    #
    # Both arms touch the settings file so that the file's own state is never
    # a confound: the baseline explicitly *removes* the hooks rather than
    # assuming they are absent.
    if args.arm == "velra":
        r = velra(velra_bin, env, "enable")
    else:
        r = velra(velra_bin, env, "disable")
    (out / "arm_setup.txt").write_text(
        f"$ velra {'enable' if args.arm == 'velra' else 'disable'}\n"
        f"exit={r.returncode}\n{r.stdout}\n{r.stderr}\n",
        encoding="utf-8", newline="")

    status = velra(velra_bin, env, "status")
    (out / "velra_status_before.txt").write_text(
        f"exit={status.returncode}\n{status.stdout}\n{status.stderr}",
        encoding="utf-8", newline="")

    # ---- 4. drive the session --------------------------------------------
    cmd = [
        str(CLAUDE),
        "-p",
        "--input-format", "stream-json",
        "--output-format", "stream-json",
        "--verbose",
        "--include-hook-events",
        "--model", args.model,
        "--permission-mode", "bypassPermissions",
        "--permission-prompts", "none",
        "--strict-mcp-config",          # no MCP servers: keeps the arms comparable
        "--max-budget-usd", str(args.max_budget_usd),
        "--autocompact", "auto",
    ]
    (out / "command.txt").write_text(" ".join(cmd), encoding="utf-8", newline="")

    stream_path = out / "stream.jsonl"
    # Arrival timestamps are kept in a sidecar so that stream.jsonl stays a
    # verbatim record. Pairing hook_started with its hook_response by hook_id
    # gives an in-session, wall-clock measurement of hook overhead.
    timing_path = out / "stream_timing.jsonl"
    started = time.time()
    proc = subprocess.Popen(
        cmd, cwd=str(fixture), env=env,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", errors="replace", bufsize=1,
    )

    turn_index = 0
    turn_marks = []          # (turn_index, wall_seconds) at each turn boundary
    session_id = None
    compact_status = None

    def send(text: str) -> None:
        payload = {"type": "user",
                   "message": {"role": "user",
                               "content": [{"type": "text", "text": text}]}}
        proc.stdin.write(json.dumps(payload) + "\n")
        proc.stdin.flush()

    timing = open(timing_path, "w", encoding="utf-8", newline="")
    with open(stream_path, "w", encoding="utf-8", newline="") as log:
        # A sidecar record of which turn each stream line belongs to. The raw
        # stream has no turn numbering of its own.
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
                compact_status = {
                    "compact_result": obj.get("compact_result"),
                    "compact_error": obj.get("compact_error"),
                }

            if obj.get("type") == "result":
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
                print(f"  turn {turn_index} done "
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
    proc.wait(timeout=120)
    wall = time.time() - started

    # ---- 5. collect the evidence -----------------------------------------
    status_after = velra(velra_bin, env, "status")
    (out / "velra_status_after.txt").write_text(
        f"exit={status_after.returncode}\n{status_after.stdout}\n{status_after.stderr}",
        encoding="utf-8", newline="")

    doctor = velra(velra_bin, env, "doctor")
    (out / "velra_doctor_after.txt").write_text(
        f"exit={doctor.returncode}\n{doctor.stdout}\n{doctor.stderr}",
        encoding="utf-8", newline="")

    if args.arm == "velra":
        for name in ("velra.db", "velra.db-wal", "velra.db-shm"):
            src = velra_home / name
            if src.exists():
                shutil.copy2(src, out / name)
        logs = velra_home / "logs" / "debug.log"
        if logs.exists():
            shutil.copy2(logs, out / "velra_debug.log")

    # The Claude Code transcript for this session.
    transcript_src = None
    projects = pathlib.Path(os.path.expanduser("~/.claude/projects"))
    if session_id and projects.exists():
        for candidate in projects.rglob(f"{session_id}.jsonl"):
            transcript_src = candidate
            shutil.copy2(candidate, out / "transcript.jsonl")
            break

    # The final state of the fixture working tree: did the agent actually fix it?
    pytest_after = run([sys.executable, "-m", "pytest", "-q"], cwd=str(fixture))
    (out / "pytest_after.txt").write_text(
        f"exit={pytest_after.returncode}\n{pytest_after.stdout}\n{pytest_after.stderr}",
        encoding="utf-8", newline="")
    diff = run(["git", "diff"], cwd=str(fixture))
    (out / "final_git_diff.txt").write_text(diff.stdout, encoding="utf-8", newline="")

    meta = {
        "arm": args.arm,
        "replicate": args.replicate,
        "model": args.model,
        "session_id": session_id,
        "transcript_source": str(transcript_src) if transcript_src else None,
        "fixture": str(fixture),
        "velra_home": str(velra_home),
        "wall_seconds": round(wall, 3),
        "turns_sent": turn_index,
        "protocol": args.protocol,
        "noise": use_noise,
        "turns_expected": len(turns),
        "compact_turn_index": compact_idx,
        "measured_turn_index": measured_idx,
        "compact_status": compact_status,
        "turn_marks": turn_marks,
        "final_pytest_exit": pytest_after.returncode,
        "final_pytest_tail": pytest_after.stdout.strip().splitlines()[-1:] or [""],
        "stderr": stderr[-4000:],
        "manifest": manifest,
        "claude_binary": str(CLAUDE),
        "velra_binary": str(velra_bin),
    }
    (out / "trial_meta.json").write_text(
        json.dumps(meta, indent=2), encoding="utf-8", newline="")

    print(json.dumps({k: meta[k] for k in
                      ("arm", "replicate", "session_id", "wall_seconds",
                       "turns_sent", "compact_status", "final_pytest_exit")},
                     indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
