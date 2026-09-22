#!/usr/bin/env python3
"""Drive one live trial: a source session, a transition, a destination session.

**Nothing in this module has been executed.** It is built so that the live
evaluation is a matter of authorization rather than of writing code under time
pressure, and every function in it is reachable only through
:mod:`run` in ``--live`` mode, behind :func:`safety.require_live`. Running it
directly refuses for the same reason.

What it does when it is eventually allowed to run
-------------------------------------------------

Both arms share everything except one step::

    1  build the fixture repository and verify its ground truth
    2  generate the context-load rung into <repo>/logs/
    3  set the arm:  `velra enable`  |  `velra disable`
    4  drive the source session through the scenario's turn script

    Benchmark A -- new_session
    5a VELRA ARM ONLY: after the source session ends,
       `velra restore --session <source> --json`
    6a start a brand-new session

    Benchmark B -- clear
    5b VELRA ARM ONLY: with the source process still alive and one turn
       short of `/clear`, `velra restore --session <source> --json`
    6b send `/clear` into that same session -- both arms, same turn, same
       transition point
    7b start a brand-new destination session, because a staged capsule is
       delivered on SessionStart(startup) and on nothing else

    8  drive the destination with the continuation prompt, identical on
       both arms
    9  capture the end state and run the target test

Step 5 is the only asymmetry, and it is the one the benchmark is about. The
baseline keeps full repository access, every native feature, the same
permission mode and the same prompts; nothing is hidden from it and nothing is
removed.

Why 5b comes *before* 6b, measured rather than assumed
------------------------------------------------------

`SessionStart(source="clear")` bumps the session epoch (`reducer.rs`, the
`SESSION_START` arm), and `restore::build` reads the session's *current* epoch
by way of `snapshot::build`. A restore issued after the clear therefore renders
the new, empty epoch and returns `RestoreError::NoState` -- confirmed against
the release binary: the same session restores a 566-token capsule before the
clear hook fires and `session ... has no task state to restore` after it.

Staging before the clear is also the only honest reading of the workflow. A
developer who is about to clear stages what they mean to carry first; asking
the tool to recover state after telling it to drop that state is not a thing
anyone does. Phase 2's clear/resume behaviour is correct as it stands and is
not changed here.

What it writes
--------------

Exactly the artifact set :mod:`mock_adapter` writes, in the same names, because
:mod:`parse` is the only reader of either and a divergence would mean the
selftest was exercising a different pipeline from the live run::

    trial_meta.json  stream.jsonl  transcript.jsonl  final_state.json
    context_fixture.json  velra_restore.json (velra arm)

Telemetry
---------

``usage`` objects are copied out of the stream verbatim and are never
post-processed. If Claude Code's ``result`` events do not carry
``cache_read_input_tokens``, this driver writes what it got and the metric
comes out UNAVAILABLE. It does not look at stdout, stderr or any status
display to fill the gap, and :func:`telemetry.scan_for_terminal_scraping`
fails the build if anyone adds code that does.
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
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    sys.path.insert(0, str(REPO_ROOT / "bench" / "harness"))
    import context_fixture  # type: ignore[no-redef]
    import prereg  # type: ignore[no-redef]
    import safety  # type: ignore[no-redef]
    import scenarios  # type: ignore[no-redef]
else:
    from . import context_fixture, prereg, safety, scenarios

ARMS = ("baseline", "velra")

#: The turn that ends a `clear`-transition source session. Named rather
#: than matched by index so a script edit cannot silently move the
#: transition point out from under the restore that must precede it.
CLEAR_TURN = "/clear"

#: The flags both arms are driven with. Identical by construction: one tuple,
#: defined in `safety.py`, used for all four sessions of a pair.
CLAUDE_FLAGS = safety.CLAUDE_FLAGS


def velra_binary(explicit: str | None = None) -> pathlib.Path:
    if explicit:
        return pathlib.Path(explicit).resolve()
    name = "velra.exe" if os.name == "nt" else "velra"
    return REPO_ROOT / "target" / "release" / name


def _run(argv, **kw) -> subprocess.CompletedProcess:
    kw.setdefault("encoding", "utf-8")
    kw.setdefault("errors", "replace")
    return subprocess.run([str(a) for a in argv], capture_output=True,
                          text=True, **kw)


def trial_env(velra_home: pathlib.Path, fixture: pathlib.Path) -> dict:
    env = dict(os.environ)
    for key in safety.NESTED_MARKERS:
        env.pop(key, None)
    env["VELRA_HOME"] = str(velra_home)
    env["VELRA_LOG"] = "debug"
    env["CLAUDE_PROJECT_DIR"] = str(fixture)
    return env


# --------------------------------------------------------------------------
# driving one session
# --------------------------------------------------------------------------


def drive_session(claude: pathlib.Path, model: str, fixture: pathlib.Path,
                  env: dict, turns, log_path: pathlib.Path,
                  max_budget_usd: float, *,
                  pause_before_turn: int | None = None, on_pause=None) -> dict:
    """Send every turn to one Claude Code process; capture the stream verbatim.

    Nothing is summarised at capture time and nothing is filtered. The only
    lines this adds to the capture are the ``_velra_bench turn_start``
    markers, because the raw stream carries no turn numbering of its own and
    the analysis needs to know which turn an event belongs to.

    ``pause_before_turn`` runs ``on_pause(session_id)`` once the preceding turn
    has completed and before the named turn is sent, with the process still
    alive. Benchmark B needs it: `velra restore` has to read the session's
    pre-`/clear` epoch, and there is no way to reach that epoch from outside a
    session that has already been cleared.
    """
    cmd = [str(claude), *CLAUDE_FLAGS, "--model", model,
           "--max-budget-usd", str(max_budget_usd)]
    started = time.time()
    proc = subprocess.Popen(
        cmd, cwd=str(fixture), env=env, stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        encoding="utf-8", errors="replace", bufsize=1)

    session_id = None
    turn_index = 0
    marks: list[dict] = []

    def send(text: str) -> None:
        proc.stdin.write(json.dumps({
            "type": "user",
            "message": {"role": "user",
                        "content": [{"type": "text", "text": text}]}}) + "\n")
        proc.stdin.flush()

    with open(log_path, "w", encoding="utf-8", newline="") as log:
        log.write(json.dumps({"_velra_bench": "turn_start", "turn": 0,
                              "text": turns[0], "t": 0.0}) + "\n")
        send(turns[0])
        for raw in proc.stdout:
            line = raw.strip()
            if not line:
                continue
            log.write(line + "\n")
            log.flush()
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if session_id is None and isinstance(event.get("session_id"), str):
                session_id = event["session_id"]
            if event.get("type") != "result":
                continue
            marks.append({"turn": turn_index,
                          "elapsed_s": round(time.time() - started, 3),
                          "subtype": event.get("subtype"),
                          "total_cost_usd": event.get("total_cost_usd"),
                          "usage": event.get("usage")})
            turn_index += 1
            if turn_index == pause_before_turn and on_pause is not None:
                log.write(json.dumps({
                    "_velra_bench": "pause", "before_turn": turn_index,
                    "reason": "velra restore, source session still live",
                    "t": round(time.time() - started, 3)}) + "\n")
                log.flush()
                on_pause(session_id)
            if turn_index < len(turns):
                log.write(json.dumps({
                    "_velra_bench": "turn_start", "turn": turn_index,
                    "text": turns[turn_index],
                    "t": round(time.time() - started, 3)}) + "\n")
                send(turns[turn_index])
            else:
                proc.stdin.close()
                break

    stderr = proc.stderr.read()
    try:
        proc.wait(timeout=120)
    except subprocess.TimeoutExpired:
        proc.kill()
    return {"session_id": session_id, "turns_sent": turn_index,
            "turn_marks": marks, "stderr": stderr[-4000:],
            "wall_seconds": round(time.time() - started, 3)}


# --------------------------------------------------------------------------
# the end state
# --------------------------------------------------------------------------


def capture_final_state(fixture: pathlib.Path, manifest: dict) -> dict:
    """Everything correctness is scored from, as bytes on disk."""
    wanted: set[str] = set()
    for entry in (manifest.get("final_correctness") or {}).get(
            "required_final_state") or []:
        wanted.add(entry["file"])
    invariant = manifest.get("invariant") or {}
    if invariant.get("must_hold_in_file"):
        wanted.add(invariant["must_hold_in_file"])
    for rel in (manifest.get("state_recovery") or {}).get("relevant_files") or []:
        wanted.add(rel)

    files = {}
    for rel in sorted(wanted):
        path = fixture / rel
        if path.is_file():
            files[rel] = path.read_text(encoding="utf-8", errors="replace")

    target = manifest.get("target_test")
    target_run = _run([sys.executable, "-m", "pytest", "-q", target],
                      cwd=str(fixture)) if target else None
    suite = _run([sys.executable, "-m", "pytest", "-q"], cwd=str(fixture))
    return {
        "files": files,
        "target_test": target,
        "target_exit": target_run.returncode if target_run else None,
        "target_tail": ((target_run.stdout or "").strip().splitlines()[-3:]
                        if target_run else []),
        "suite_exit": suite.returncode,
        "suite_failures": scenarios.failure_count(suite.stdout or ""),
        "suite_tail": (suite.stdout or "").strip().splitlines()[-3:],
        "git_status": _run(["git", "status", "--porcelain"],
                           cwd=str(fixture)).stdout,
        "git_diff": _run(["git", "diff"], cwd=str(fixture)).stdout[:20000],
    }


# --------------------------------------------------------------------------
# the restore step
# --------------------------------------------------------------------------


def do_restore(velra: pathlib.Path, env: dict, fixture: pathlib.Path,
               source_session: str, manifest: dict) -> dict:
    """Step 6: stage the source session's state, and record the whole lifecycle.

    Everything the causal chain's links C and D need is written here, from the
    tool's own JSON rather than from an assumption about what it did.
    """
    listing = _run([velra, "restore", "--list", "--json"], cwd=str(fixture),
                   env=env)
    staged = _run([velra, "restore", "--session", source_session, "--json"],
                  cwd=str(fixture), env=env)

    def parsed(proc):
        try:
            return json.loads((proc.stdout or "").strip())
        except json.JSONDecodeError:
            return None

    listing_json = parsed(listing) or {}
    staged_json = parsed(staged) or {}
    workspace_id = staged_json.get("workspace_id") or listing_json.get("workspace_id")

    record: dict = {
        "restore_invoked": True,
        "restore_exit": staged.returncode,
        "restore_stdout": (staged.stdout or "")[:4000],
        "restore_stderr": (staged.stderr or "")[-1000:],
        "listing": listing_json,
        "source_session_id": source_session,
        "workspace_id": workspace_id,
        "workspace_root": listing_json.get("workspace_root"),
        "staged_path": staged_json.get("staged_path"),
        "stale": False,
    }

    path = staged_json.get("staged_path")
    if path and pathlib.Path(path).exists():
        try:
            on_disk = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            on_disk = None
        record["staged"] = on_disk
        if isinstance(on_disk, dict):
            age = int(time.time() * 1000) - int(on_disk.get("created_ms") or 0)
            record["age_ms"] = age
            record["stale"] = age > 7 * 24 * 3600 * 1000
    else:
        record["staged"] = None

    # Link C: what the ledger actually held, checked against the scenario's
    # declared markers, before anything was delivered.
    capsule = ((record.get("staged") or {}).get("capsule") or "")
    markers = list(manifest.get("capsule_markers") or [])
    low = capsule.replace("\\", "/").lower()
    record["ledger_evidence"] = {
        "markers_present": [m for m in markers
                            if m.replace("\\", "/").lower() in low],
        "markers_missing": [m for m in markers
                            if m.replace("\\", "/").lower() not in low],
    }
    # The claim lifecycle is observed from the destination session's own hook
    # responses, not asserted here. One staging is one claimable capsule.
    record["claim"] = {"attempts": []}
    return record


# --------------------------------------------------------------------------
# one trial
# --------------------------------------------------------------------------


def run_trial(*, scenario_name: str, arm: str, pair_id: str, replicate: int,
              out: pathlib.Path, fixture: pathlib.Path, rung: int,
              model: str, claude: pathlib.Path, velra: pathlib.Path,
              max_budget_usd: float, seed: int = 0,
              arm_order_index: int = 0,
              arm_order: list | None = None) -> dict:
    """One arm of one pair, start to finish. Requires an authorized live run."""
    scenario = scenarios.get(scenario_name)
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)

    provenance = repo_provenance()
    manifest = scenario.build(fixture, verify=True)
    load = context_fixture.generate(fixture, rung, scenario_name, seed)
    check = context_fixture.verify(fixture, load)
    load["verification"] = check
    (out / "context_fixture.json").write_text(
        json.dumps(load, indent=2), encoding="utf-8", newline="")

    velra_home = out / "velra_home"
    shutil.rmtree(velra_home, ignore_errors=True)
    velra_home.mkdir(parents=True, exist_ok=True)
    env = trial_env(velra_home, fixture)

    # The arm. Both arms write the settings file, so its prior state is never
    # a confound: the baseline removes the hooks rather than assuming they are
    # absent.
    setup = _run([velra, "enable" if arm == "velra" else "disable"], env=env,
                 cwd=str(fixture))
    (out / "arm_setup.txt").write_text(
        f"velra {'enable' if arm == 'velra' else 'disable'}\n"
        f"exit={setup.returncode}\n{setup.stdout}\n{setup.stderr}\n",
        encoding="utf-8", newline="")

    # -- the source session, and where `velra restore` sits in it ---------
    #
    # For a `clear` transition the restore MUST happen before the `/clear`
    # turn is sent, and this is not a stylistic preference. Measured against
    # the real binary: `SessionStart(source="clear")` bumps the session epoch
    # (`reducer.rs`, SESSION_START arm), `restore::build` reads the session's
    # *current* epoch through `snapshot::build`, and so a restore issued after
    # the clear renders the new, empty epoch and returns
    # `RestoreError::NoState`. Every Velra-arm B trial would have come out a
    # capture failure, and the scenario would have measured nothing.
    #
    # It is also the honest workflow. A developer who is about to clear stages
    # what they mean to carry *first*; staging after the clear is asking the
    # tool to recover state the user has just told it to drop.
    # The staging must also happen *inside* the live source process, not
    # between two of them: `/clear` is a turn sent to a running session, and
    # splitting the script into two `drive_session` calls would put the clear
    # in a different session from the work it is supposed to be clearing.
    source_turns = list(scenario.turns)
    restore_at = (source_turns.index(CLEAR_TURN)
                  if scenario.transition == "clear" and CLEAR_TURN in source_turns
                  else None)

    restore: dict | None = None

    def stage_now(session_id: str | None) -> None:
        """Run `velra restore` against the still-running source session."""
        nonlocal restore
        if arm != "velra" or restore is not None:
            return
        restore = do_restore(velra, env, fixture, session_id or "", manifest)
        restore["staged_before_turn_index"] = restore_at
        restore["staged_while_source_session_live"] = True
        (out / "velra_restore.json").write_text(
            json.dumps(restore, indent=2), encoding="utf-8", newline="")

    source = drive_session(claude, model, fixture, env, source_turns,
                           out / "source_stream.jsonl", max_budget_usd,
                           pause_before_turn=restore_at, on_pause=stage_now)

    # For a `new_session` transition there is no mid-script pause: the source
    # session simply ends, and the restore happens after it, which is when a
    # developer who has walked away would run it.
    if arm == "velra" and restore is None:
        restore = do_restore(velra, env, fixture, source["session_id"] or "",
                             manifest)
        restore["staged_before_turn_index"] = None
        restore["staged_while_source_session_live"] = False
        (out / "velra_restore.json").write_text(
            json.dumps(restore, indent=2), encoding="utf-8", newline="")

    # -- the destination session -----------------------------------------
    destination = drive_session(claude, model, fixture, env,
                                [scenario.continuation_prompt],
                                out / "stream.jsonl", max_budget_usd)

    transcript_src = None
    projects = pathlib.Path(os.path.expanduser("~/.claude/projects"))
    if destination["session_id"] and projects.exists():
        for candidate in projects.rglob(f"{destination['session_id']}.jsonl"):
            transcript_src = candidate
            shutil.copy2(candidate, out / "transcript.jsonl")
            break

    final_state = capture_final_state(fixture, manifest)
    (out / "final_state.json").write_text(
        json.dumps(final_state, indent=2), encoding="utf-8", newline="")

    meta = {
        "scenario": scenario_name,
        "arm": arm,
        "replicate": replicate,
        "pair_id": pair_id,
        "pair_key": {
            "benchmark": scenario_name,
            "scenario": scenario_name,
            "pair_id": pair_id,
            "fixture_seed": manifest.get("fixture_seed"),
            "model": model,
            "claude_version": claude_version(claude),
            "git_head": provenance["git_head"],
            "turn_script_hash": manifest.get("turn_script_hash"),
            "permission_mode": "bypassPermissions",
            "context_ladder_rung": rung,
        },
        "model": model,
        "session_id": destination["session_id"],
        "source_session_id": source["session_id"],
        "destination_session_id": destination["session_id"],
        "source_session": {
            "session_id": source["session_id"],
            "turns": source["turns_sent"],
            "transition": scenario.transition,
            "turn_marks": source["turn_marks"],
        },
        "destination_session": {
            "turns": destination["turns_sent"],
            "turn_marks": destination["turn_marks"],
        },
        "repo_provenance": provenance,
        # Which arm ran first in this pair, recorded so the analysis can ask
        # whether a shared server-side prompt cache favoured the second one.
        "arm_order_index": arm_order_index,
        "arm_order": list(arm_order or ARMS),
        "transition_lifecycle": {
            "transition": scenario.transition,
            "restore_before_turn_index": restore_at,
            "restore_while_source_session_live": restore_at is not None,
            "source_turns_total": len(source_turns),
            "clear_turn_index": (source_turns.index(CLEAR_TURN)
                                 if CLEAR_TURN in source_turns else None),
            "destination_is_a_new_process": True,
            "continuation_prompt_identical_on_both_arms": True,
        },
        "old_transcript_replayed": False,
        "transcript_source": str(transcript_src) if transcript_src else None,
        "manifest": manifest,
        "leak_scan": manifest.get("leak_scan"),
        "context_fixture": load,
        "fixture": str(fixture),
        "velra_home": str(velra_home),
        "claude_binary": str(claude),
        "claude_version": claude_version(claude),
        "velra_binary": str(velra),
        "wall_seconds": source["wall_seconds"] + destination["wall_seconds"],
        "stderr": (source["stderr"][-2000:] + destination["stderr"][-2000:]),
        **prereg.stamp(),
    }
    (out / "trial_meta.json").write_text(
        json.dumps(meta, indent=2, default=str), encoding="utf-8", newline="")
    return meta


def repo_provenance() -> dict:
    """The commit this trial is evidence about, recorded in the trial itself.

    Without it a trial directory is a set of numbers with no way back to the
    source they describe: the readiness report names a commit, but a trial
    resumed days later against a moved HEAD would be silently attributed to
    whichever commit the report happened to be regenerated under.
    """
    head = _run(["git", "rev-parse", "HEAD"], cwd=str(REPO_ROOT))
    branch = _run(["git", "rev-parse", "--abbrev-ref", "HEAD"],
                  cwd=str(REPO_ROOT))
    status = _run(["git", "status", "--porcelain"], cwd=str(REPO_ROOT))
    changes = [line for line in (status.stdout or "").splitlines()
               if line.strip()]
    return {
        "git_head": (head.stdout or "").strip(),
        "git_branch": (branch.stdout or "").strip(),
        "working_tree_changes": len(changes),
        "working_tree_clean": not changes,
        "captured_at_trial_start": True,
    }


def claude_version(claude: pathlib.Path) -> str:
    proc = _run([claude, "--version"])
    return (proc.stdout or proc.stderr or "").strip()[:100]


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--scenario", required=True,
                    choices=list(scenarios.SCENARIOS))
    ap.add_argument("--arm", required=True, choices=ARMS)
    ap.add_argument("--pair-id", required=True)
    ap.add_argument("--replicate", type=int, default=1)
    ap.add_argument("--out", required=True)
    ap.add_argument("--fixture", required=True)
    ap.add_argument("--rung", type=int, default=250_000)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--claude", default=None)
    ap.add_argument("--velra-binary", default=None)
    ap.add_argument("--max-budget-usd", type=float, default=25.0)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--live", action="store_true",
                    help="required, together with VELRA_ALLOW_LIVE_BENCHMARK=1")
    args = ap.parse_args()

    # The same gate the runner uses, checked again here, because a module that
    # starts Claude processes must not depend on its caller having been
    # careful.
    safety.require_live(safety.MODE_LIVE, live_flag=args.live)

    sys.path.insert(0, str(REPO_ROOT / "bench" / "harness"))
    import claude_binary  # noqa: E402

    meta = run_trial(
        scenario_name=args.scenario, arm=args.arm, pair_id=args.pair_id,
        replicate=args.replicate, out=pathlib.Path(args.out),
        fixture=pathlib.Path(args.fixture), rung=args.rung, model=args.model,
        claude=pathlib.Path(args.claude) if args.claude
        else pathlib.Path(claude_binary.resolve()),
        velra=velra_binary(args.velra_binary),
        max_budget_usd=args.max_budget_usd, seed=args.seed)
    print(json.dumps({k: meta[k] for k in
                      ("scenario", "arm", "pair_id", "source_session_id",
                       "destination_session_id", "wall_seconds")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
