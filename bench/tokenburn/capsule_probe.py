#!/usr/bin/env python3
"""Prove the qualification capsule comes from the production path, offline.

    python bench/tokenburn/capsule_probe.py --binary target/release/velra.exe

The claim this exists to support, from §6 of the pre-qualification audit:

    source session → Velra restore/state → production capsule generation
    → staged capsule → SessionStart delivery

and the claim it exists to *refute*, which is the one that would quietly void
the whole benchmark:

    the scenario manually injects its own ground truth into the capsule

Nothing here writes capsule text. The probe replays each scenario's declared
activity through the real ``velra hook`` binary — prompts, reads, edits, a
failing test — so the state reaches the ledger the only way a session's state
ever reaches it. Then it runs the real ``velra restore`` and reads back what
the production renderer produced. The declared ``capsule_markers`` are checked
*against that output*; they are never inserted into it.

Why it runs in the readiness gate
---------------------------------

``causal.link_f`` fails a trial whose delivered capsule does not carry every
declared marker. If the production renderer cannot produce those markers from
this scenario's activity, every Velra-arm trial comes out
``INCONCLUSIVE(RECEIPT_FAILURE)`` and the qualification run is wasted. That is
a thing worth knowing for the price of two seconds and no API call, so the
check blocks rather than warns.

What it does not prove
----------------------

That a *live* session's ledger will hold the same rows. The probe replays a
faithful but hand-written approximation of the turn script's tool activity; a
real session may read more files, run the test differently, or phrase things
its own way. It proves the renderer and the restore path can carry these
markers, not that a given live run will. That is what qualification is for.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import safety  # type: ignore[no-redef]
    import scenarios  # type: ignore[no-redef]
else:
    from . import safety, scenarios

#: Per scenario, the tool activity a source session performs. Each entry is a
#: hook event and its payload fragment, replayed through the real binary.
#:
#: These mirror the scenario's own turn script. They are deliberately *not*
#: derived from `capsule_markers`: if they were, the probe would be arranging
#: for the markers to appear and would prove nothing.
ACTIVITY = {
    "A_cold_continuation": [
        ("prompt", "We're on the payments service. Run the suite and tell me "
                   "what's red. I care about the idempotency one only."),
        ("read", "src/payments/retry.py"),
        ("read", "tests/test_retry.py"),
        ("test_fail", ("python -m pytest -q tests/test_retry.py",
                       "FAILED tests/test_retry.py::"
                       "test_retry_preserves_idempotency_key\n1 failed")),
        ("edit", "src/payments/retry.py"),
        ("prompt", "That passes, but it's the process-wide state I told you to "
                   "avoid. Revert it with git restore."),
        ("prompt", "The next thing to look at is retry_backoff and where the "
                   "key is built relative to the loop. Don't change anything "
                   "yet."),
    ],
    "B_clear_survival": [
        ("prompt", "Payments again. I want the reconcile failure this time, "
                   "not the other two."),
        ("read", "src/payments/reconcile.py"),
        ("read", "src/payments/feed.py"),
        ("test_fail", ("python -m pytest -q tests/test_reconcile.py",
                       "FAILED tests/test_reconcile.py::"
                       "test_march_window_totals\n1 failed")),
        ("edit", "src/payments/reconcile.py"),
        ("prompt", "Green, but wrong. Revert it with git restore. A window has "
                   "to close on the booking date."),
        ("prompt", "Next step is in_window in reconcile.py. Don't change it "
                   "yet."),
    ],
}


def binary_path(explicit: str | None) -> pathlib.Path:
    if explicit:
        return pathlib.Path(explicit).resolve()
    name = "velra.exe" if os.name == "nt" else "velra"
    return REPO_ROOT / "target" / "release" / name


def _env(home: pathlib.Path, workspace: pathlib.Path) -> dict:
    env = dict(os.environ)
    for key in safety.NESTED_MARKERS:
        env.pop(key, None)
    env["VELRA_HOME"] = str(home)
    env["CLAUDE_PROJECT_DIR"] = str(workspace)
    return env


def _hook(binary: pathlib.Path, env: dict, workspace: pathlib.Path,
          event: str, payload: dict) -> None:
    subprocess.run([str(binary), "hook", event], input=json.dumps(payload),
                   cwd=str(workspace), env=env, capture_output=True, text=True,
                   encoding="utf-8", errors="replace")


def replay(binary: pathlib.Path, env: dict, workspace: pathlib.Path,
           session: str, activity) -> None:
    """Drive the scenario's activity through the real hook path."""
    cwd = str(workspace)
    _hook(binary, env, workspace, "session-start", {
        "session_id": session, "hook_event_name": "SessionStart",
        "source": "startup", "cwd": cwd})
    for kind, value in activity:
        if kind == "prompt":
            _hook(binary, env, workspace, "user-prompt-submit", {
                "session_id": session, "hook_event_name": "UserPromptSubmit",
                "cwd": cwd, "prompt": value})
        elif kind == "read":
            _hook(binary, env, workspace, "post-tool-use", {
                "session_id": session, "hook_event_name": "PostToolUse",
                "cwd": cwd, "tool_name": "Read",
                "tool_input": {"file_path": value}})
        elif kind == "edit":
            _hook(binary, env, workspace, "post-tool-use", {
                "session_id": session, "hook_event_name": "PostToolUse",
                "cwd": cwd, "tool_name": "Edit",
                "tool_input": {"file_path": value, "old_string": "a",
                               "new_string": "b"}})
        elif kind == "test_fail":
            command, output = value
            _hook(binary, env, workspace, "post-tool-use", {
                "session_id": session, "hook_event_name": "PostToolUse",
                "cwd": cwd, "tool_name": "Bash",
                "tool_input": {"command": command},
                "tool_response": {"exit_code": 1, "stdout": output}})
    _hook(binary, env, workspace, "stop", {
        "session_id": session, "hook_event_name": "Stop", "cwd": cwd})
    subprocess.run([str(binary), "reduce"], cwd=str(workspace), env=env,
                   input="", capture_output=True, text=True)


def probe_one(binary: pathlib.Path, name: str, root: pathlib.Path) -> dict:
    """Replay one scenario and read back the production capsule."""
    scenario = scenarios.get(name)
    manifest = scenarios.manifest_for(name)
    workspace = root / name
    home = root / f"{name}-home"
    scenario.build(workspace, verify=False)
    home.mkdir(parents=True, exist_ok=True)
    env = _env(home, workspace)
    session = f"probe-{name}"

    replay(binary, env, workspace, session, ACTIVITY.get(name, []))

    staged = subprocess.run(
        [str(binary), "restore", "--session", session, "--dry-run"],
        cwd=str(workspace), env=env, capture_output=True, text=True,
        encoding="utf-8", errors="replace")
    capsule = (staged.stdout or "").strip()

    markers = list(manifest.get("capsule_markers") or [])
    haystack = capsule.replace("\\", "/").lower()
    found = [m for m in markers if m.replace("\\", "/").lower() in haystack]
    missing = [m for m in markers if m not in found]

    return {
        "scenario": name,
        "restore_exit": staged.returncode,
        "capsule_chars": len(capsule),
        "capsule_is_production_output": capsule.startswith(
            "<VELRA_WORKSPACE_STATE"),
        "declared_markers": markers,
        "markers_found": found,
        "markers_missing": missing,
        "ok": (staged.returncode == 0 and not missing
               and capsule.startswith("<VELRA_WORKSPACE_STATE")),
        "sections": [line for line in capsule.splitlines()
                     if line.startswith("[")],
        "stderr": (staged.stderr or "")[-300:],
        "capsule": capsule,
    }


def run(binary: pathlib.Path, names=None, root: pathlib.Path | None = None) -> dict:
    """Probe every scenario. Returns a record; raises nothing."""
    names = list(names or scenarios.DEFAULT_ORDER)
    tmp = None
    if root is None:
        tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(tmp.name) / "capsule-probe"
    root.mkdir(parents=True, exist_ok=True)
    try:
        results = {}
        for name in names:
            if not binary.exists():
                results[name] = {"scenario": name, "ok": False,
                                 "error": f"no binary at {binary}"}
                continue
            results[name] = probe_one(binary, name, pathlib.Path(root))
        return {
            "ok": all(r.get("ok") for r in results.values()),
            "binary": str(binary),
            "scenarios": results,
            "capsule_written_by": "velra restore (production renderer)",
            "capsule_written_by_benchmark": False,
            "note": ("the benchmark replays activity through the real hook "
                     "path and reads back what the production renderer "
                     "produced; it never composes capsule text"),
            "offline": safety.assert_offline(safety.MODE_DRY_RUN),
        }
    finally:
        if tmp:
            tmp.cleanup()


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", default=None)
    ap.add_argument("--only", nargs="*", choices=list(scenarios.SCENARIOS))
    ap.add_argument("--show-capsule", action="store_true")
    args = ap.parse_args()

    result = run(binary_path(args.binary), args.only)
    for name, entry in sorted(result["scenarios"].items()):
        mark = "PASS" if entry.get("ok") else "FAIL"
        print(f"  [{mark}] {name}: "
              f"{len(entry.get('markers_found') or [])}"
              f"/{len(entry.get('declared_markers') or [])} declared markers "
              f"in the production capsule ({entry.get('capsule_chars', 0)} chars)")
        if entry.get("markers_missing"):
            print(f"         missing: {entry['markers_missing']}")
            print(f"         sections present: {entry.get('sections')}")
        if args.show_capsule and entry.get("capsule"):
            print(entry["capsule"])
    print("\nCAPSULE PROVENANCE " + ("PASSED" if result["ok"] else "FAILED"))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
