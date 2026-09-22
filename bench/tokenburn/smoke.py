#!/usr/bin/env python3
"""The restore/delivery path, end to end, against the real binary, offline.

    python bench/tokenburn/smoke.py --binary target/release/velra.exe

No Claude process is started and none is needed. Claude Code's side of this
contract is a documented one: it runs ``velra hook session-start`` with a JSON
payload on stdin and reads one JSON object from stdout. The payloads used here
are the recorded ones in ``tests/fixtures/claude-code/2.1.272/``, so the smoke
test exercises the real hook against the real shapes a real client sends.

The workflow, modelled exactly
------------------------------

::

    source session exists      seeded through `velra hook ...`
            |
    `velra restore`            stages the capsule
            |
    capsule staged             $VELRA_HOME/staged/<workspace>/staged_capsule
            |
    new destination session    a different session id
            |
    SessionStart startup       `velra hook session-start` with source=startup
            |
    structured additionalContext
            |
    destination receives the capsule

The tiny operational state
--------------------------

Deliberately minimal, so that "did the destination receive exactly this?" is a
question with a yes-or-no answer::

    active_file  = src/payments/retry.rs
    next_action  = inspect retry_backoff()

Those two strings are the specification's own example, and they are the only
thing the verifier looks for. A capsule that arrives carrying something else is
a delivery failure with a different name, and the smoke test says which.

What is checked
---------------

1. the source and destination session ids differ;
2. the old transcript is not replayed;
3. the capsule is bounded;
4. the capsule arrives through ``SessionStart``;
5. it is delivered exactly once;
6. a second ``SessionStart`` does not re-inject it;
7. the delivery intent is startup-only;
8. a ``SessionStart`` from the wrong source does not consume it.

This module builds and validates that path. **It never starts a live Claude
smoke test.** Doing so needs an explicit authorization from the orchestrator
that this phase does not have, and there is no code path here that could.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import parse  # type: ignore[no-redef]
    import prereg  # type: ignore[no-redef]
    import safety  # type: ignore[no-redef]
else:
    from . import parse, prereg, safety

#: The recorded Claude Code payloads the hook is driven with.
FIXTURES = REPO_ROOT / "tests" / "fixtures" / "claude-code" / "2.1.272"

#: The tiny operational state, as §6 of the Phase 3 specification states it.
ACTIVE_FILE = "src/payments/retry.rs"
NEXT_ACTION = "inspect retry_backoff()"

#: The prompt that puts both into the source session's ledger. It is one
#: sentence, because the point of the smoke test is the delivery path and not
#: the renderer's judgement about what to keep.
SOURCE_PROMPT = f"{NEXT_ACTION} in {ACTIVE_FILE}"

SOURCE_SESSION = "smoke-source-0001"
DEST_SESSION = "smoke-dest-0001"
SECOND_DEST = "smoke-dest-0002"
THIRD_DEST = "smoke-dest-0003"

#: SessionStart sources that continue an existing conversation and must leave a
#: startup capsule staged and untouched.
REFUSING_SOURCES = ("clear", "resume", "compact", "fork")

#: A generous character ceiling. The registered token ceiling is the real one;
#: this catches the case where the whole transcript ends up in the capsule,
#: which would be two orders of magnitude over either.
CAPSULE_CHAR_CEILING = 8_000


def binary_path(explicit: str | None) -> pathlib.Path:
    if explicit:
        return pathlib.Path(explicit).resolve()
    name = "velra.exe" if os.name == "nt" else "velra"
    return REPO_ROOT / "target" / "release" / name


def git(repo: pathlib.Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=str(repo), check=True,
                   capture_output=True, text=True)


class Harness:
    """An isolated workspace and Velra home, and the hook calls against them."""

    def __init__(self, binary: pathlib.Path, root: pathlib.Path) -> None:
        self.binary = binary
        self.root = root
        self.home = root / "velra_home"
        self.workspace = root / "workspace"
        self.calls: list[dict] = []

    # -- setup ------------------------------------------------------------

    def build(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)
        (self.workspace / "src" / "payments").mkdir(parents=True)
        self.home.mkdir(parents=True)
        (self.workspace / "src" / "payments" / "retry.rs").write_text(
            "pub fn retry_backoff(attempt: u32) -> u64 {\n"
            "    50 << attempt\n}\n", encoding="utf-8", newline="")
        git(self.workspace, "init", "-q", "-b", "main")
        git(self.workspace, "config", "user.email", "smoke@velra.bench")
        git(self.workspace, "config", "user.name", "Velra Smoke")
        git(self.workspace, "config", "commit.gpgsign", "false")
        git(self.workspace, "add", "-A")
        git(self.workspace, "commit", "-q", "-m", "payments retry")

    def env(self) -> dict:
        env = dict(os.environ)
        # A nested Claude Code session must not leak into the harness, and the
        # harness must not touch the user's real Velra state.
        for key in safety.NESTED_MARKERS:
            env.pop(key, None)
        env["VELRA_HOME"] = str(self.home)
        env["CLAUDE_PROJECT_DIR"] = str(self.workspace)
        return env

    # -- invocation -------------------------------------------------------

    def hook(self, event: str, payload: dict) -> dict:
        """One hook call. Returns stdout parsed, plus the raw text."""
        proc = subprocess.run(
            [str(self.binary), "hook", event],
            input=json.dumps(payload), cwd=str(self.workspace), env=self.env(),
            capture_output=True, text=True, encoding="utf-8", errors="replace")
        stdout = (proc.stdout or "").strip()
        parsed = None
        if stdout:
            try:
                parsed = json.loads(stdout)
            except json.JSONDecodeError:
                parsed = None
        record = {"event": event, "payload": payload, "exit": proc.returncode,
                  "stdout": stdout, "stderr": (proc.stderr or "")[-500:],
                  "parsed": parsed}
        self.calls.append(record)
        return record

    def cli(self, *args: str) -> dict:
        proc = subprocess.run(
            [str(self.binary), *args], cwd=str(self.workspace), env=self.env(),
            capture_output=True, text=True, encoding="utf-8", errors="replace")
        stdout = (proc.stdout or "").strip()
        try:
            parsed = json.loads(stdout) if stdout else None
        except json.JSONDecodeError:
            parsed = None
        record = {"argv": list(args), "exit": proc.returncode,
                  "stdout": stdout[:4000], "stderr": (proc.stderr or "")[-500:],
                  "parsed": parsed}
        self.calls.append(record)
        return record

    def session_start(self, session_id: str, source: str) -> dict:
        """A ``SessionStart`` built from the recorded Claude Code payload."""
        fixture = FIXTURES / f"session_start_{source}.json"
        payload = json.loads(fixture.read_text(encoding="utf-8")) \
            if fixture.exists() else {"hook_event_name": "SessionStart"}
        payload.update({"session_id": session_id, "source": source,
                        "cwd": str(self.workspace)})
        payload["transcript_path"] = str(
            self.root / "transcripts" / f"{session_id}.jsonl")
        return self.hook("session-start", payload)

    # -- the source session -----------------------------------------------

    def seed_source_session(self) -> None:
        """Put the tiny operational state into the ledger, through the hooks."""
        cwd = str(self.workspace)
        self.session_start(SOURCE_SESSION, "startup")
        self.hook("user-prompt-submit", {
            "session_id": SOURCE_SESSION, "hook_event_name": "UserPromptSubmit",
            "cwd": cwd, "prompt": SOURCE_PROMPT})
        self.hook("post-tool-use", {
            "session_id": SOURCE_SESSION, "hook_event_name": "PostToolUse",
            "cwd": cwd, "tool_name": "Edit",
            "tool_input": {"file_path": ACTIVE_FILE,
                           "old_string": "50 << attempt",
                           "new_string": "50u64 << attempt"}})
        self.hook("post-tool-use", {
            "session_id": SOURCE_SESSION, "hook_event_name": "PostToolUse",
            "cwd": cwd, "tool_name": "Bash",
            "tool_input": {"command": "cargo test retry"},
            "tool_response": {"exit_code": 1,
                              "stdout": "FAILED tests/retry.rs::backoff_doubles"}})
        self.hook("stop", {"session_id": SOURCE_SESSION,
                           "hook_event_name": "Stop", "cwd": cwd})
        # `reduce` is a hook-path subcommand, not a CLI one: it takes no
        # arguments and dispatches straight from argv, so it is invoked here
        # the way the settings file invokes it.
        subprocess.run([str(self.binary), "reduce"], cwd=str(self.workspace),
                       env=self.env(), input="", capture_output=True, text=True)

    def staged_file(self, workspace_id: str) -> pathlib.Path:
        return self.home / "staged" / workspace_id / "staged_capsule"


# --------------------------------------------------------------------------
# the checks
# --------------------------------------------------------------------------


def capsule_of(call: dict) -> str | None:
    parsed = call.get("parsed")
    if not isinstance(parsed, dict):
        return None
    try:
        return parsed["hookSpecificOutput"]["additionalContext"]
    except (KeyError, TypeError):
        return None


def run(binary: pathlib.Path, root: pathlib.Path) -> dict:
    """The whole smoke path. Returns a result record; raises nothing."""
    checks: list[dict] = []

    def record(name: str, ok: bool, detail: str = "") -> bool:
        checks.append({"check": name, "ok": bool(ok), "detail": detail})
        return bool(ok)

    harness = Harness(binary, root)
    if not binary.exists():
        record("velra binary present", False, str(binary))
        return {"ok": False, "checks": checks, "binary": str(binary)}
    record("velra binary present", True, str(binary))

    harness.build()
    harness.seed_source_session()

    listing = harness.cli("restore", "--list", "--json")
    sessions = ((listing.get("parsed") or {}).get("sessions") or [])
    workspace_id = (listing.get("parsed") or {}).get("workspace_id") or ""
    record("the source session is restorable",
           any(s.get("session_id") == SOURCE_SESSION and s.get("has_state")
               for s in sessions),
           json.dumps(sessions)[:300])

    staged = harness.cli("restore", "--session", SOURCE_SESSION, "--json")
    staged_record = staged.get("parsed") or {}
    record("velra restore stages a capsule",
           bool(staged_record.get("staged")), staged.get("stdout", "")[:200])
    tokens = staged_record.get("tokens")
    ceiling = prereg.capsule_token_ceiling()
    record("the staged capsule is within the registered token ceiling",
           isinstance(tokens, int) and 0 < tokens <= ceiling,
           f"{tokens} tokens, ceiling {ceiling}")

    staged_path = harness.staged_file(workspace_id)
    on_disk = None
    if staged_path.exists():
        try:
            on_disk = json.loads(staged_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            on_disk = None
    record("the staged record declares startup-only delivery",
           isinstance(on_disk, dict)
           and on_disk.get("deliver_on") == ["startup"],
           json.dumps((on_disk or {}).get("deliver_on")))
    record("the staged record names the source session, not a destination",
           isinstance(on_disk, dict)
           and on_disk.get("source_session_id") == SOURCE_SESSION,
           str((on_disk or {}).get("source_session_id")))

    # -- the destination session -----------------------------------------
    first = harness.session_start(DEST_SESSION, "startup")
    capsule = capsule_of(first)
    record("the capsule arrives through SessionStart",
           capsule is not None
           and (first.get("parsed") or {}).get("hookSpecificOutput", {})
           .get("hookEventName") == "SessionStart",
           f"exit={first['exit']}, {len(capsule or '')} chars")
    record("the source and destination session ids differ",
           SOURCE_SESSION != DEST_SESSION,
           f"{SOURCE_SESSION} -> {DEST_SESSION}")
    record("the destination received the active file",
           bool(capsule) and ACTIVE_FILE in capsule, ACTIVE_FILE)
    record("the destination received the next action",
           bool(capsule) and "retry_backoff" in capsule, NEXT_ACTION)
    record("the capsule is bounded",
           bool(capsule) and len(capsule) <= CAPSULE_CHAR_CEILING,
           f"{len(capsule or '')} chars, ceiling {CAPSULE_CHAR_CEILING}")
    record("the old transcript was not replayed into the destination",
           bool(capsule) and SOURCE_PROMPT.lower() in capsule.lower()
           and len(capsule) <= CAPSULE_CHAR_CEILING
           and capsule.count("<VELRA_WORKSPACE_STATE") == 1,
           "the capsule is one bounded record, not a conversation")

    # -- exactly once -----------------------------------------------------
    second = harness.session_start(SECOND_DEST, "startup")
    record("a second SessionStart does not re-inject it",
           capsule_of(second) is None,
           f"exit={second['exit']}, stdout={second['stdout'][:80]!r}")
    third = harness.session_start(THIRD_DEST, "startup")
    record("nor does a third",
           capsule_of(third) is None, f"exit={third['exit']}")
    record("the staged capsule is gone once claimed",
           not staged_path.exists(), str(staged_path))

    # -- the wrong source -------------------------------------------------
    restaged = harness.cli("restore", "--session", SOURCE_SESSION, "--json")
    record("the capsule can be staged again",
           bool((restaged.get("parsed") or {}).get("staged")),
           restaged.get("stdout", "")[:120])
    for source in REFUSING_SOURCES:
        call = harness.session_start(f"smoke-{source}-session", source)
        record(f"SessionStart({source}) does not consume a startup capsule",
               capsule_of(call) is None,
               f"exit={call['exit']}, stdout={call['stdout'][:80]!r}")
    record("the capsule is still staged after every wrong source",
           staged_path.exists(), str(staged_path))

    final = harness.session_start("smoke-dest-0009", "startup")
    record("and startup still gets it afterwards",
           capsule_of(final) is not None, f"exit={final['exit']}")

    failed = [c for c in checks if not c["ok"]]
    return {
        "ok": not failed,
        "binary": str(binary),
        "workspace_id": workspace_id,
        "staged_tokens": tokens,
        "capsule_chars": len(capsule or ""),
        "operational_state": {"active_file": ACTIVE_FILE,
                              "next_action": NEXT_ACTION},
        "checks": checks,
        "failed": failed,
        "calls": harness.calls,
        "offline": safety.assert_offline(safety.MODE_SMOKE),
        **prereg.stamp(),
    }


def write_trial_shaped(result: dict, into: pathlib.Path) -> pathlib.Path:
    """Write the smoke run as a trial directory the production parser reads.

    This is the part that makes the smoke test infrastructure rather than a
    script: the hook responses it captured are written as ``stream.jsonl`` in
    the same shape a live capture has, so :func:`parse.parse_trial` runs over
    them and the delivery it finds is found by the same code that will find the
    live one.
    """
    into = pathlib.Path(into)
    into.mkdir(parents=True, exist_ok=True)
    lines = [json.dumps({"_velra_bench": "turn_start", "turn": 0,
                         "text": "smoke", "t": 0.0})]
    for index, call in enumerate(result["calls"]):
        if call.get("event") != "session-start" or not call.get("stdout"):
            continue
        payload = call.get("payload") or {}
        lines.append(json.dumps({
            "type": "system", "subtype": "hook_response",
            "uuid": f"smoke-hook-{index}",
            "hook_name": f"SessionStart:{payload.get('source')}",
            "hook_event": "SessionStart",
            "session_start_source": payload.get("source"),
            "exit_code": call.get("exit"),
            "stdout": call["stdout"], "stderr": "",
            "session_id": payload.get("session_id"),
        }))
    (into / "stream.jsonl").write_text("\n".join(lines) + "\n",
                                       encoding="utf-8", newline="")
    (into / "trial_meta.json").write_text(json.dumps({
        "scenario": "smoke", "arm": "velra", "pair_id": None,
        "source_session_id": SOURCE_SESSION,
        "destination_session_id": DEST_SESSION,
        "smoke": True, **prereg.stamp()}, indent=2),
        encoding="utf-8", newline="")
    (into / "smoke_result.json").write_text(
        json.dumps(result, indent=2, default=str), encoding="utf-8",
        newline="")
    return into


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", default=None)
    ap.add_argument("--root", default=None,
                    help="where to build the isolated workspace")
    ap.add_argument("--out", default=None,
                    help="also write the run as a trial directory here")
    args = ap.parse_args()

    offline = safety.assert_offline(safety.MODE_SMOKE)
    print(f"smoke: offline={offline['offline']}, claude processes permitted="
          f"{offline['claude_processes_permitted']}")

    import tempfile
    tmp = None
    if args.root:
        root = pathlib.Path(args.root).resolve()
    else:
        tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(tmp.name) / "tokenburn-smoke"

    result = run(binary_path(args.binary), root)
    for check in result["checks"]:
        mark = "PASS" if check["ok"] else "FAIL"
        print(f"  [{mark}] {check['check']}"
              + (f" -- {check['detail']}" if check["detail"] else ""))

    if args.out:
        out = write_trial_shaped(result, pathlib.Path(args.out))
        parsed = parse.parse_trial(out)
        print(f"\n  parsed by the production parser: "
              f"{len(parsed.deliveries)} delivery/deliveries, "
              f"{sum(1 for d in parsed.deliveries if d.on_startup)} on startup")
        print(f"  trial-shaped artifacts: {out}")

    print("\nSMOKE " + ("PASSED" if result["ok"] else "FAILED")
          + f" -- {sum(1 for c in result['checks'] if c['ok'])}"
          f"/{len(result['checks'])} checks")
    if tmp:
        tmp.cleanup()
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
