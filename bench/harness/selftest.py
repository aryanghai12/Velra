#!/usr/bin/env python3
"""Drive the whole analysis pipeline offline, on synthetic captures.

    python bench/harness/selftest.py

Builds a complete results tree — trials for every scenario and both arms,
controls, token measurements — out of hand-written captures with known
outcomes, then runs the real ``scenario_analyze``, ``scenario_aggregate`` and
``scenario_verdict`` against it and checks the verdicts are the ones the
scripted outcomes imply.

No Claude Code, no API calls, no fixture repository, nothing that costs money.
Its job is to catch the failure mode where a benchmark's plumbing is broken and
nobody finds out until after the expensive part: a metric that never fires, an
aggregate that silently drops an arm, a verdict that reports PASSED because a
key was missing rather than because an arm won.

The scripted run is deliberately mixed, so every branch of the verdict logic is
exercised at least once:

  s1  Velra 4/4, baseline 0/4, control lossy      -> PASSED
  s2  Velra 2/4, baseline 2/4                     -> FAILED (no separation)
  s3  Velra 4/4, baseline 0/4, control NOT lossy  -> INCONCLUSIVE (control gate)

plus one invalid trial that must be excluded from the behavioural denominator
and still counted in the run's totals.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO_ROOT / "bench"))

import prereg  # noqa: E402
from scenarios import base, registry, s2_hidden_constraint, s3_working_set  # noqa: E402

CAPSULE = (
    '<VELRA_WORKSPACE_STATE v="1" checkpoint="ckpt_01SELFTEST00000000000000" '
    'captured="2026-09-15T10:00:00Z" trigger="manual">\n'
    "[ABOUT_THIS_RECORD]\nVelra is a local tool that recorded this task state.\n"
    "[FIRST_MESSAGE] (OBSERVED | user prompt | 10:00)\n"
    "{root}\n"
    "[WORKSPACE_STATE]\nmain | 2 edits this task | last test run: FAIL\n"
    "[REVERTED_EDITS] (OBSERVED)\n"
    "- src/ledger/money.py | 2 edit(s) | reverted via `git restore "
    "src/ledger/money.py` at 10:14\n"
    "- src/ledger/rules.py | 1 edit(s) | reverted via `git restore "
    "src/ledger/rules.py` at 10:20\n"
    "[RECORD_DETAIL]\nFull detail for any section: `velra inspect`\n"
    "</VELRA_WORKSPACE_STATE>"
)


def stream_lines(scenario, measured_index: int, tools: list[dict],
                 arm: str, prose: str, root: str) -> list[str]:
    """A capture in the shape Claude Code's stream-json output takes."""
    lines: list[str] = []
    for index, turn in enumerate(scenario.turns):
        lines.append(json.dumps({"_velra_bench": "turn_start", "turn": index,
                                 "text": turn, "t": float(index)}))
        if index == measured_index:
            if arm == "velra":
                lines.append(json.dumps({
                    "type": "system", "subtype": "hook_response",
                    "hook_id": "h-capsule", "hook_name": "SessionStart:compact",
                    "hook_event": "SessionStart", "exit_code": 0,
                    "stdout": json.dumps({"hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": CAPSULE.format(root=root)}}),
                    "stderr": "", "session_id": "sess-selftest"}))
            lines.append(json.dumps({
                "type": "assistant", "session_id": "sess-selftest",
                "message": {"role": "assistant", "content":
                            [{"type": "text", "text": prose}] + tools}}))
        if index == scenario.compact_index:
            lines.append(json.dumps({"type": "system", "subtype": "compact_boundary",
                                     "compact_result": "success",
                                     "compact_error": None}))
        lines.append(json.dumps({
            "type": "result", "subtype": "success", "session_id": "sess-selftest",
            "duration_ms": 1000, "total_cost_usd": 0.05, "num_turns": 1,
            "usage": {"input_tokens": 100}}))
    return lines


def tool_use(name: str, **inputs) -> dict:
    return {"type": "tool_use", "id": f"toolu_{name}", "name": name,
            "input": inputs, "caller": {"type": "direct"}}


def write_trial(root: pathlib.Path, scenario, arm: str, replicate: int,
                *, tools: list[dict], final_files: dict, pytest_exit: int,
                prose: str = "Working on it.", valid: bool = True,
                capsule_tokens: int = 718, native_tokens: int = 3100) -> pathlib.Path:
    trial = root / "trials" / f"{scenario.name}-{arm}-r{replicate}"
    trial.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory() as tmp:
        manifest = scenario.build(pathlib.Path(tmp) / "fx", verify=False)

    measured = scenario.measured_index
    root_text = (manifest.get("constraint") or "Fix the failing test")[:160]
    lines = stream_lines(scenario, measured, tools, arm, prose, root_text)
    if not valid:
        # A session that reached the measured turn without compacting: the
        # exact shape of the v0.1 replicate that was caught by hand.
        lines = [l for l in lines if '"compact_result"' not in l]
    (trial / "stream.jsonl").write_text("\n".join(lines) + "\n",
                                        encoding="utf-8", newline="")

    timing = []
    if arm == "velra":
        for i in range(40):
            timing.append(json.dumps({"t_recv": i * 0.01, "turn": 0,
                                      "subtype": "hook_started", "hook_id": f"h{i}",
                                      "hook_name": "PostToolUse",
                                      "hook_event": "PostToolUse"}))
            timing.append(json.dumps({"t_recv": i * 0.01 + 0.004, "turn": 0,
                                      "subtype": "hook_response", "hook_id": f"h{i}",
                                      "hook_name": "PostToolUse",
                                      "hook_event": "PostToolUse", "exit_code": 0,
                                      "stdout_len": 0, "stderr_len": 0,
                                      "output_len": 0}))
    (trial / "stream_timing.jsonl").write_text(
        ("\n".join(timing) + "\n") if timing else "", encoding="utf-8", newline="")

    transcript = []
    if valid:
        transcript.append(json.dumps({
            "isCompactSummary": True,
            "message": {"role": "user", "content": "x" * (native_tokens * 4)}}))
    (trial / "transcript.jsonl").write_text(
        ("\n".join(transcript) + "\n") if transcript else "",
        encoding="utf-8", newline="")

    (trial / "final_state.json").write_text(json.dumps({
        "files": final_files, "pytest_exit": pytest_exit,
        "pytest_tail": ["ok" if pytest_exit == 0 else "failed"],
        "pytest_output": "", "git_diff": "", "git_status": "", "git_log": "",
    }, indent=2), encoding="utf-8", newline="")

    # Pair identity, exactly as `scenario_trial.py` records it. The synthetic
    # captures have to carry it or the selftest stops exercising the pairing
    # contract it exists to check.
    pair_id = f"{scenario.name}#r{replicate}"
    pair_key = {
        "scenario": scenario.name,
        "pair_id": pair_id,
        "fixture_seed": manifest.get("fixture_seed"),
        "model": "selftest",
        "turn_count": len(scenario.turns),
        "compact_turn_index": scenario.compact_index,
        "measured_turn_index": measured,
        "claude_version": "0.0.0",
        "velra_commit": "selftest",
    }
    (trial / "trial_meta.json").write_text(json.dumps({
        "scenario": scenario.name, "arm": arm, "replicate": replicate,
        "pair_id": pair_id, "pair_key": pair_key,
        "model": "selftest", "session_id": f"sess-{scenario.name}-{arm}-{replicate}",
        "wall_seconds": 200.0, "turns_sent": len(scenario.turns),
        "turns_expected": len(scenario.turns),
        "compact_turn_index": scenario.compact_index,
        "measured_turn_index": measured,
        "compact_status": {"compact_result": "success"} if valid else None,
        "turn_marks": [{"total_cost_usd": 0.05}] * len(scenario.turns),
        "final_pytest_exit": pytest_exit, "stderr": "", "manifest": manifest,
        "claude_binary": "selftest", "claude_version": "0.0.0",
        "velra_binary": "selftest",
        "provenance": {"reported_version": "velra 0.1.1 (selftest)",
                       "git_head_short9": "selftest", "working_tree_clean": True,
                       "commit_matches_binary": True, "warnings": []},
        **prereg.stamp(),
    }, indent=2), encoding="utf-8", newline="")

    # The native summary is measured on the same tokenizer as the capsule, in
    # both arms: E4's comparison half depends on it existing for the baseline
    # too, and the selftest would not notice if it silently did not.
    (trial / "native_summary.txt").write_text(
        "x" * (native_tokens * 4), encoding="utf-8", newline="")
    (trial / "native_summary_tokens.json").write_text(json.dumps({
        "present": True, "measured": True, "model": "selftest",
        "control": {"a": 1000, "b": 1000, "delta": 0}, "control_valid": True,
        "chars": native_tokens * 4, "measured_tokens": native_tokens,
        "chars_per_token": 4.0,
    }, indent=2), encoding="utf-8", newline="")

    if arm == "velra":
        (trial / "token_measurement.json").write_text(json.dumps({
            "trial": trial.name, "model": "selftest", "budget_tokens": 800,
            "control": {"a": 1000, "b": 1000, "delta": 0}, "control_valid": True,
            "injections": [{"index": 0, "hook_name": "SessionStart:compact",
                            "exit_code": 0, "context_chars": 1500,
                            "measured_tokens": capsule_tokens,
                            "budget_tokens": 800, "within_budget": True}],
        }, indent=2), encoding="utf-8", newline="")
    return trial


def control(root: pathlib.Path, name: str, lossy: bool) -> None:
    (root / "controls").mkdir(parents=True, exist_ok=True)
    (root / "controls" / f"{name}.json").write_text(json.dumps({
        "scenario": name, "model": "selftest", "replicates": 2,
        "valid_replicates": 2, "lossy_replicates": 2 if lossy else 0,
        "reduction_pct_each": [41.3, 38.0] if lossy else [12.0, 9.5],
        "reduction_pct_median": 39.6 if lossy else 10.8,
        "canary_recalled_each": [False, False] if lossy else [True, True],
        "compaction_is_lossy": lossy, "lossy_rule": "selftest", "runs": [],
        **prereg.stamp(),
    }, indent=2), encoding="utf-8", newline="")


def build_results(root: pathlib.Path) -> None:
    s1 = registry.get("s1-dead-end-pair")
    s2 = registry.get("s2-hidden-constraint")
    s3 = registry.get("s3-working-set")

    engine_fixed = {"src/ledger/engine.py": base.ENGINE_TRUE_FIX}
    engine_loop = {"src/ledger/engine.py": base.ENGINE_LOOP_PRESERVING_FIX}

    # s1: Velra never touches a burned file; baseline always does. 4 vs 4.
    for r in range(1, 5):
        write_trial(root, s1, "velra", r,
                    tools=[tool_use("Edit", file_path="/fx/src/ledger/engine.py",
                                    old_string="a", new_string="b"),
                           tool_use("Bash", command="python -m pytest -q")],
                    final_files=engine_fixed, pytest_exit=0)
        write_trial(root, s1, "baseline", r,
                    tools=[tool_use("Edit", file_path="/fx/src/ledger/money.py",
                                    old_string="a", new_string="b"),
                           tool_use("Edit", file_path="/fx/src/ledger/engine.py",
                                    old_string="a", new_string="b"),
                           tool_use("Bash", command="python -m pytest -q")],
                    final_files=engine_fixed, pytest_exit=0)

    # s2: no separation — both arms honour the constraint half the time.
    for r in range(1, 5):
        honoured = r <= 2
        for arm in ("velra", "baseline"):
            write_trial(root, s2, arm, r,
                        tools=[tool_use("Edit", file_path="/fx/src/ledger/engine.py",
                                        old_string="a", new_string="b")],
                        final_files=engine_loop if honoured else engine_fixed,
                        pytest_exit=0)

    # s3: a clean sweep for Velra, but the control says nothing was forgotten.
    for r in range(1, 5):
        write_trial(root, s3, "velra", r,
                    tools=[tool_use("Edit", file_path=f"/fx/{s3_working_set.TARGET}",
                                    old_string="a", new_string="b")],
                    final_files={s3_working_set.TARGET: s3_working_set.TAB_DELIMITER},
                    pytest_exit=0)
        write_trial(root, s3, "baseline", r,
                    tools=[tool_use("Grep", pattern="DELIMITER", path="src")],
                    final_files={s3_working_set.TARGET: s3_working_set.NO_DELIMITER},
                    pytest_exit=0)

    # One invalid replicate: reached the measured turn without compacting.
    write_trial(root, s1, "baseline", 5,
                tools=[tool_use("Bash", command="python -m pytest -q")],
                final_files=engine_fixed, pytest_exit=0, valid=False)

    control(root, s1.name, lossy=True)
    control(root, s2.name, lossy=True)
    control(root, s3.name, lossy=False)


def run(cmd: list, cwd: pathlib.Path) -> subprocess.CompletedProcess:
    return subprocess.run([str(c) for c in cmd], cwd=str(cwd),
                          capture_output=True, text=True,
                          encoding="utf-8", errors="replace")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", default=None,
                    help="write the synthetic results tree here instead of a "
                         "temporary directory, and leave it behind")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    tmp = None
    if args.keep:
        root = pathlib.Path(args.keep).resolve()
        shutil.rmtree(root, ignore_errors=True)
    else:
        tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(tmp.name) / "v0.1.1"
    root.mkdir(parents=True, exist_ok=True)

    print("building a synthetic results tree ...", flush=True)
    build_results(root)
    trials = sorted((root / "trials").iterdir())

    failures: list[str] = []
    print(f"analysing {len(trials)} synthetic trials ...", flush=True)
    for trial in trials:
        proc = run([sys.executable, HERE / "scenario_analyze.py", "--trial", trial],
                   REPO_ROOT)
        if proc.returncode != 0:
            failures.append(f"scenario_analyze failed on {trial.name}:\n{proc.stderr}")
        elif args.verbose:
            print(proc.stdout)

    proc = run([sys.executable, HERE / "scenario_aggregate.py",
                *[str(t) for t in trials], "--out", root / "aggregate.json"],
               REPO_ROOT)
    if proc.returncode != 0:
        failures.append(f"scenario_aggregate failed:\n{proc.stderr}")
    elif args.verbose:
        print(proc.stdout)

    proc = run([sys.executable, HERE / "scenario_verdict.py", "--results", root],
               REPO_ROOT)
    print(proc.stdout)
    if proc.stderr:
        print(proc.stderr, file=sys.stderr)

    verdicts_path = root / "verdicts.json"
    if not verdicts_path.exists():
        failures.append("scenario_verdict wrote no verdicts.json")
    else:
        payload = json.loads(verdicts_path.read_text(encoding="utf-8"))
        got = {k: v["verdict"] for k, v in payload["verdicts"].items()}
        expected = {
            "E1": "PASSED",         # 4/4 against 0/4, control lossy
            "E2": "FAILED",         # no separation
            "E3": "INCONCLUSIVE",   # control says nothing was forgotten
            "E4": "PASSED",         # 718 tokens, under both the ceiling and the summary
            "E5": "PASSED",         # 160 clean invocations
            "E6": "PASSED",         # no rejection language
        }
        for hid, want in expected.items():
            if got.get(hid) != want:
                failures.append(
                    f"{hid}: expected {want}, got {got.get(hid)} "
                    f"({payload['verdicts'].get(hid, {}).get('why')})")

        validity = payload["validity"]
        if validity["invalid"] != 1:
            failures.append(f"expected exactly 1 invalid trial, got "
                            f"{validity['invalid']}")
        s1_base = json.loads((root / "aggregate.json").read_text(
            encoding="utf-8"))["groups"]["s1-dead-end-pair/baseline"]
        if s1_base["n_usable_behavioural"] != 4 or s1_base["n_trials"] != 5:
            failures.append(
                f"the invalid trial must be counted but not scored: "
                f"n_trials={s1_base['n_trials']} "
                f"usable={s1_base['n_usable_behavioural']}")

    if failures:
        print("\nSELFTEST FAILED")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("\nSELFTEST PASSED - the pipeline runs end to end and every branch "
          "of the verdict logic returns what the scripted outcomes imply.")
    if args.keep:
        print(f"synthetic results left at {root}")
    if tmp:
        tmp.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
