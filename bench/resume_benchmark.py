#!/usr/bin/env python3
"""Resume the v0.1.1 efficacy benchmark after a quota exhaustion.

    python bench/resume_benchmark.py --dry-run      # classify only; free
    python bench/resume_benchmark.py                # resume trials, then analyse
    python bench/resume_benchmark.py --phase4-only  # re-run the analysis only

What happened on 2026-09-16. The runner did not stop when the Claude Code
session limit was hit. Claude Code answers a rate-limited turn with a
``<synthetic>`` assistant message and a ``result`` line whose subtype is still
``success`` (with ``is_error: true``), so ``scenario_trial.py`` sent every
remaining turn of every remaining trial, and all sixteen trial folders exist.
Fourteen of them contain no model behaviour at all, or none on the turn that is
scored:

  * s1 baseline-r2 compacted normally, but its measured turn 16 was a
    synthetic "You've hit your session limit" reply. Its recorded "failure" is
    the rate limit, not the model. ``validity.py`` passes it, because it only
    looks at the subtype.
  * s1 velra-r2..r4, baseline-r3..r4 and all eight s2 trials: every turn
    synthetic, $0 spent.

This script replaces exactly those, and nothing else:

1. **Classify** all sixteen trials from the bytes on disk. A trial is
   *poisoned* when any assistant message is ``<synthetic>``, any result line has
   ``is_error: true``, or compaction failed on a usage limit. Poisoned or
   incomplete trials are the only ones ever moved.
2. **Guard** against the pre-registration's prohibitions. A trial with a clean
   stream is a result that has been seen, and "an invalid trial is never ...
   re-run to replace a result that was already seen". So a clean trial is
   always kept, even when it is on the resume list and even when it is invalid.
   s1 baseline-r1 is exactly that case: clean, but it never compacted, so it
   stays on disk as an invalid, counted trial.
3. **Quarantine** instead of deleting. Poisoned trials move to
   ``results/v0.1.1/quarantine/<timestamp>/`` and every move is written to
   ``results/v0.1.1/resume_log.json`` with its evidence. The pre-registration
   forbids silently dropping a trial, and a quarantined folder can be audited.
   ``scenario_verdict.py`` reads only ``trials/``.
4. **Pin** the environment the kept trials ran under: the same Velra commit, the
   same pre-registration hash, the same Claude Code binary. If any of them has
   drifted, stop rather than mix binaries within one comparison.
5. **Preflight** one tiny ``claude -p`` call to confirm the limit has reset.
6. **Run** the missing trials in the original runner's order. After each one,
   classify it again. If the limit hits again, quarantine that trial and stop.
   Re-running the script later picks up where it stopped.
7. **Phase 4**: analysis over all sixteen trials, both tokenizer measurements,
   hook overhead, aggregate, verdicts. A token measurement that comes back as
   zero (which is what a rate-limited measurement looks like) fails loudly.

It writes to your real Claude Code settings file, exactly as the original
runner does. Run it from an ordinary terminal, not from inside Claude Code.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent
HARNESS = HERE / "harness"
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HARNESS))

import prereg  # noqa: E402
from scenarios import registry  # noqa: E402

RESULTS = HERE / "results" / "v0.1.1"
TRIALS = RESULTS / "trials"
QUARANTINE = RESULTS / "quarantine"
LOG = RESULTS / "resume_log.json"
BINARY = REPO_ROOT / "target" / "release" / (
    "velra.exe" if os.name == "nt" else "velra")

SCENARIOS = ("s1-dead-end-pair", "s2-hidden-constraint")
REPLICATES = (1, 2, 3, 4)
ARMS = ("baseline", "velra")

# Completed before the limit hit. Never moved; the run stops if either is
# poisoned, because then the premise of this script is wrong.
PROTECTED = ("s1-dead-end-pair-baseline-r1", "s1-dead-end-pair-velra-r1")


# Every other trial is re-run only if classify() finds it poisoned. s1
# baseline-r2 qualifies because its measured turn was rate limited, not because
# its result was unwelcome.

LIMIT_MARKERS = ("session limit", "usage limit", "rate limit", "limit reached",
                 "credit balance", "overloaded")


# ---------------------------------------------------------------------------
# small helpers
# ---------------------------------------------------------------------------

def step(title: str) -> None:
    print()
    print("=" * 78)
    print(f"  {title}")
    print("=" * 78, flush=True)


def sh(cmd: list, **kw) -> subprocess.CompletedProcess:
    print("$ " + " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run([str(c) for c in cmd], **kw)


def now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def load_json(path: pathlib.Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def log_event(event: dict) -> None:
    """Append to resume_log.json. Never rewrites earlier entries."""
    data = load_json(LOG) or {
        "purpose": "Every trial this resume moved out of trials/, and why. "
                   "Quarantined folders are kept under quarantine/ for audit.",
        "events": [],
    }
    data["events"].append({"at": now(), **event})
    LOG.write_text(json.dumps(data, indent=2), encoding="utf-8", newline="")


def child_env(claude_binary: str | None) -> dict:
    env = dict(os.environ)
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)
    if claude_binary:
        env["VELRA_BENCH_CLAUDE"] = claude_binary
    return env


# ---------------------------------------------------------------------------
# classification
# ---------------------------------------------------------------------------

def classify(trial: pathlib.Path) -> dict:
    """clean | poisoned | incomplete | missing, with the evidence."""
    if not trial.is_dir():
        return {"state": "missing"}
    meta = load_json(trial / "trial_meta.json")
    stream = trial / "stream.jsonl"
    if meta is None or not stream.exists():
        return {"state": "incomplete",
                "reason": "no trial_meta.json or stream.jsonl"}

    synthetic_turns: list[int] = []
    error_turns: list[int] = []
    turn = None
    with open(stream, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if obj.get("_velra_bench") == "turn_start":
                turn = obj.get("turn")
            elif (obj.get("type") == "assistant"
                  and (obj.get("message") or {}).get("model") == "<synthetic>"):
                synthetic_turns.append(turn)
            elif obj.get("type") == "result" and obj.get("is_error"):
                error_turns.append(turn)

    compact_error = ((meta.get("compact_status") or {}).get("compact_error") or "")
    compact_limited = any(m in compact_error.lower() for m in LIMIT_MARKERS)
    measured = meta.get("measured_turn_index")
    evidence = {
        "synthetic_turns": sorted(set(t for t in synthetic_turns if t is not None)),
        "is_error_turns": sorted(set(t for t in error_turns if t is not None)),
        "compact_error": compact_error or None,
        "measured_turn_index": measured,
        "turns_sent": meta.get("turns_sent"),
        "turns_expected": meta.get("turns_expected"),
        "claude_version": meta.get("claude_version"),
        "prior_validity": ((load_json(trial / "analysis.json") or {})
                           .get("validity") or {}).get("failed_criteria"),
    }
    poisoned = bool(synthetic_turns or error_turns or compact_limited)
    if poisoned:
        evidence["measured_turn_poisoned"] = (
            measured in evidence["synthetic_turns"]
            or measured in evidence["is_error_turns"])
        return {"state": "poisoned", **evidence}
    if meta.get("turns_sent") != meta.get("turns_expected"):
        return {"state": "incomplete", "reason": "not every turn was sent",
                **evidence}
    return {"state": "clean", **evidence}


def quarantine(trial: pathlib.Path, stamp: str, why: dict) -> pathlib.Path:
    dest = QUARANTINE / stamp / trial.name
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(trial), str(dest))
    log_event({"action": "quarantined", "trial": trial.name,
               "moved_to": str(dest.relative_to(REPO_ROOT)), "evidence": why})
    return dest


# ---------------------------------------------------------------------------
# guards
# ---------------------------------------------------------------------------

def pinned_environment() -> dict:
    """What the protected trials ran under; the resume must match it."""
    metas = [load_json(TRIALS / t / "trial_meta.json") for t in PROTECTED]
    if any(m is None for m in metas):
        raise SystemExit(f"a protected trial is missing its trial_meta.json: "
                         f"{PROTECTED}")
    def one(key_fn, label):
        values = {key_fn(m) for m in metas}
        if len(values) != 1:
            raise SystemExit(f"protected trials disagree on {label}: {values}")
        return values.pop()
    return {
        "claude_binary": one(lambda m: m["claude_binary"], "claude binary"),
        "claude_version": one(lambda m: m["claude_version"], "claude version"),
        "commit": one(lambda m: m["provenance"]["embedded_commit"], "commit"),
        "prereg_sha256": one(lambda m: m["preregistration_sha256"],
                             "preregistration hash"),
        "model": one(lambda m: m["model"], "model"),
    }


def check_drift(pin: dict) -> list[str]:
    problems = []
    if not BINARY.exists():
        problems.append(f"missing release binary {BINARY}")
    else:
        reported = subprocess.run([str(BINARY), "--version"], capture_output=True,
                                  text=True, encoding="utf-8", errors="replace").stdout
        if pin["commit"] not in reported:
            problems.append(f"velra binary reports {reported.strip()!r}; the kept "
                            f"trials ran commit {pin['commit']}")
    head = subprocess.run(["git", "rev-parse", "--short=9", "HEAD"], cwd=str(REPO_ROOT),
                          capture_output=True, text=True).stdout.strip()
    if head != pin["commit"]:
        problems.append(f"git HEAD is {head}; the kept trials ran {pin['commit']}. "
                        f"Do not commit anything until the resume has finished.")
    sha = prereg.stamp()["preregistration_sha256"]
    if sha != pin["prereg_sha256"]:
        problems.append("bench/harness/preregistration.json has changed since the "
                        "kept trials ran; that invalidates the run")
    if not pathlib.Path(pin["claude_binary"]).is_file():
        problems.append(f"Claude Code {pin['claude_version']} is no longer installed "
                        f"at {pin['claude_binary']} (the extension auto-updated?)")
    return problems


def preflight(pin: dict, env: dict) -> tuple[bool, str]:
    """One minimal call. Proves the limit has reset before anything is moved."""
    cmd = [pin["claude_binary"], "-p", "Reply with the single word OK.",
           "--output-format", "json", "--model", pin["model"], "--tools", "",
           "--strict-mcp-config", "--no-session-persistence"]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8",
                              errors="replace", env=env, timeout=180)
    except subprocess.TimeoutExpired:
        return False, "preflight timed out after 180s"
    text = (proc.stdout or "") + (proc.stderr or "")
    try:
        obj = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return False, f"exit {proc.returncode}: {text.strip()[:300]}"
    usage = obj.get("usage") or {}
    billed = sum(usage.get(k) or 0 for k in (
        "input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))
    if obj.get("is_error") or billed == 0 or any(
            m in text.lower() for m in LIMIT_MARKERS):
        return False, str(obj.get("result") or text)[:300]
    return True, f"ok ({billed} input tokens, ${obj.get('total_cost_usd')})"


# ---------------------------------------------------------------------------
# phase 4 output checks
# ---------------------------------------------------------------------------

def check_token_file(path: pathlib.Path) -> str | None:
    """A rate-limited tokenizer call measures 0. Say so rather than record it."""
    data = load_json(path)
    if data is None:
        return f"{path.parent.name}: {path.name} missing or unreadable"
    if data.get("present") is False or data.get("measured") is False:
        return None
    control = data.get("control") or {}
    if not control.get("a") or not control.get("b"):
        return f"{path.parent.name}: {path.name} control measured 0 tokens"
    if control.get("delta") != 0:
        return f"{path.parent.name}: {path.name} control delta {control.get('delta')}"
    values = ([i.get("measured_tokens") for i in data.get("injections") or []]
              if "injections" in data else [data.get("measured_tokens")])
    if not values or any((v or 0) <= 0 for v in values):
        return f"{path.parent.name}: {path.name} has a non-positive measurement {values}"
    return None


def phase4(args, pin: dict, env: dict) -> int:
    py = sys.executable
    dirs = [TRIALS / f"{s}-{arm}-r{r}"
            for s in SCENARIOS for r in REPLICATES for arm in ARMS]
    states = {d.name: classify(d) for d in dirs}
    bad = {n: s["state"] for n, s in states.items() if s["state"] != "clean"}
    if bad:
        print(f"refusing to analyse: not every trial is clean: {bad}", file=sys.stderr)
        return 1
    failures: list[str] = []

    step("Phase 4a - extract telemetry from all 16 trials")
    for d in dirs:
        if sh([py, HARNESS / "scenario_analyze.py", "--trial", d],
              cwd=str(REPO_ROOT), env=env).returncode != 0:
            failures.append(f"scenario_analyze failed for {d.name}")

    if not args.skip_tokens:
        step("Phase 4b - measure the delivered capsule with Anthropic's tokenizer")
        for d in dirs:
            if not (d / "velra.db").exists():
                continue
            code = sh([py, HARNESS / "measure_tokens.py", "--trial", d,
                       "--model", pin["model"]], cwd=str(REPO_ROOT), env=env).returncode
            if code != 0:
                # Exit 1 with no injection is itself a finding for a velra
                # trial; it is recorded, not hidden.
                failures.append(f"measure_tokens exited {code} for {d.name}")
            elif (problem := check_token_file(d / "token_measurement.json")):
                failures.append(problem)

    if not args.skip_native_tokens:
        step("Phase 4c - measure the native compaction summary, same tokenizer")
        for d in dirs:
            if not (d / "native_summary.txt").exists():
                continue
            code = sh([py, HARNESS / "native_tokens.py", "--trial", d,
                       "--model", pin["model"]], cwd=str(REPO_ROOT), env=env).returncode
            if code != 0:
                failures.append(f"native_tokens exited {code} for {d.name}")
            elif (problem := check_token_file(d / "native_summary_tokens.json")):
                failures.append(problem)

    if not args.skip_overhead:
        step("Phase 4d - hook overhead against a loaded database")
        if sh([py, HARNESS / "hook_overhead.py", "--binary", BINARY,
               "--runs", args.overhead_runs, "--events", args.overhead_events,
               "--out", RESULTS / "hook_overhead.json"],
              cwd=str(REPO_ROOT), env=env).returncode != 0:
            failures.append("hook_overhead failed")

    step("Phase 4e - aggregate")
    if sh([py, HARNESS / "scenario_aggregate.py", *[str(d) for d in dirs],
           "--out", RESULTS / "aggregate.json"],
          cwd=str(REPO_ROOT), env=env).returncode != 0:
        failures.append("scenario_aggregate failed")

    step("Phase 5 - evaluate every registered hypothesis")
    verdict_code = sh([py, HARNESS / "scenario_verdict.py", "--results", RESULTS],
                      cwd=str(REPO_ROOT), env=env).returncode

    log_event({"action": "phase4_finished", "verdict_exit": verdict_code,
               "failures": failures})
    if failures:
        print("\nPHASE 4 PROBLEMS - the verdicts below may rest on bad inputs:",
              file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        print("If these are quota errors, wait for the reset and run "
              "`python bench/resume_benchmark.py --phase4-only`.", file=sys.stderr)
        return 2
    return verdict_code


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dry-run", action="store_true",
                    help="classify every trial and print the plan; change nothing")
    ap.add_argument("--phase4-only", action="store_true",
                    help="skip cleanup and trials; re-run the analysis")
    ap.add_argument("--skip-preflight", action="store_true")
    ap.add_argument("--max-budget-usd", type=float, default=25.0)
    ap.add_argument("--fixture-root", default=None)
    ap.add_argument("--skip-tokens", action="store_true")
    ap.add_argument("--skip-native-tokens", action="store_true")
    ap.add_argument("--skip-overhead", action="store_true")
    ap.add_argument("--overhead-runs", type=int, default=200)
    ap.add_argument("--overhead-events", type=int, default=100000)
    args = ap.parse_args()

    started = time.time()
    if os.environ.get("CLAUDECODE"):
        print("NOTE: running inside a Claude Code session. The trials strip its "
              "variables, but they share the settings file this mutates. An "
              "ordinary terminal is safer.", flush=True)

    step("Guard - pinned environment of the kept trials")
    pin = pinned_environment()
    print(json.dumps(pin, indent=2))
    problems = check_drift(pin)
    for p in problems:
        print(f"  DRIFT: {p}", file=sys.stderr)
    if problems and not args.dry_run:
        return 1
    env = child_env(pin["claude_binary"])

    if args.phase4_only:
        return phase4(args, pin, env)

    step("Classify all 16 trials")
    plan: list[str] = []
    to_quarantine: dict[str, dict] = {}
    for s in SCENARIOS:
        for r in REPLICATES:
            for arm in ARMS:
                name = f"{s}-{arm}-r{r}"
                c = classify(TRIALS / name)
                detail = ""
                if c["state"] == "poisoned":
                    detail = (f"synthetic turns {c['synthetic_turns'] or '-'}, "
                              f"measured turn poisoned: {c['measured_turn_poisoned']}")
                elif c.get("prior_validity"):
                    detail = f"clean but invalid: {c['prior_validity']}"
                if name in PROTECTED:
                    action = "keep (protected)"
                    if c["state"] != "clean":
                        print(f"{name:36} {c['state']:10} {detail}")
                        print(f"\nABORT: protected trial {name} is {c['state']}. "
                              f"The premise of this resume is wrong; stop and "
                              f"look before spending anything.", file=sys.stderr)
                        return 1
                elif c["state"] == "clean":
                    # A seen result. The pre-registration forbids replacing it.
                    action = "keep (clean result already seen)"
                else:
                    action = "quarantine + re-run" if c["state"] != "missing" else "run"
                    if c["state"] != "missing":
                        to_quarantine[name] = c
                    plan.append(name)
                print(f"{name:36} {c['state']:10} -> {action:34} {detail}")

    print(f"\n{len(plan)} trial(s) to run, {len(to_quarantine)} to quarantine.")
    if args.dry_run:
        print("--dry-run: nothing changed.")
        return 0
    if not plan:
        print("No trials to run; going straight to phase 4.")
        return phase4(args, pin, env)

    if not args.skip_preflight:
        step("Preflight - has the usage limit reset?")
        ok, detail = preflight(pin, env)
        print(f"  {detail}")
        if not ok:
            print("\nABORT: Claude Code is still refusing requests. Nothing was "
                  "moved. Run again after the reset.", file=sys.stderr)
            return 3

    step("Quarantine poisoned trials")
    stamp = datetime.datetime.now().strftime("%Y%m%dT%H%M%S")
    log_event({"action": "resume_started", "pinned": pin, "plan": plan})
    for name, evidence in to_quarantine.items():
        dest = quarantine(TRIALS / name, stamp, evidence)
        print(f"  {name} -> {dest.relative_to(REPO_ROOT)}")

    py = sys.executable
    fixture_root = pathlib.Path(args.fixture_root or (
        pathlib.Path(tempfile.gettempdir()) / "velra-efficacy-fixtures"))
    fixture_root.mkdir(parents=True, exist_ok=True)

    for i, name in enumerate(plan, 1):
        scenario, rest = name.rsplit("-", 2)[0], name.rsplit("-", 2)[1:]
        arm, replicate = rest[0], int(rest[1].lstrip("r"))
        registry.get(scenario)  # fail fast on a typo, before spending
        step(f"Phase 3 - trial {name}  ({i}/{len(plan)})")
        out = TRIALS / name
        code = sh([py, HARNESS / "scenario_trial.py",
                   "--scenario", scenario, "--arm", arm,
                   "--replicate", replicate, "--model", pin["model"],
                   "--out", out, "--fixture", fixture_root / name,
                   "--max-budget-usd", args.max_budget_usd],
                  cwd=str(REPO_ROOT), env=env).returncode
        c = classify(out)
        if code != 0 or c["state"] != "clean":
            why = {"exit_code": code, **c}
            if out.exists():
                quarantine(out, stamp + "-resume", why)
            print(f"\nSTOP: {name} came back {c['state']} (exit {code}). It has "
                  f"been quarantined and logged. If this is the usage limit "
                  f"again, wait for the reset and run this script again; it "
                  f"resumes from here.", file=sys.stderr)
            return 3
        log_event({"action": "trial_completed", "trial": name,
                   "claude_version": c.get("claude_version")})
        print(f"  {name}: clean", flush=True)

    code = phase4(args, pin, env)
    print(f"\ntotal wall time: {time.time() - started:.0f}s")
    print(f"verdicts:        {RESULTS / 'verdicts.json'}")
    print(f"resume log:      {LOG}")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
