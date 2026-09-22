#!/usr/bin/env python3
"""Run the whole v0.1.1 efficacy benchmark, end to end.

    python bench/legacy/run_efficacy_benchmark.py

Everything the v0.1.1 report claims is produced by this script: the offline
regression gate, the provenance record, the per-scenario information-loss
controls, both arms of every replicate of every scenario, the telemetry
extraction, the token measurements for both the capsule and the native summary,
the hook-overhead benchmark, the aggregate, and the verdicts.

Success criteria are read from ``bench/harness/preregistration.json``, which is
hashed into every artifact. Changing that file after a run invalidates the run
rather than reinterpreting it.

**Cost.** Each replicate is two sessions of fourteen to seventeen turns. On
Sonnet, budget roughly $2.50 to $3.50 per replicate per scenario, plus about
$3 per control replicate and about $0.50 per trial for the tokenizer
measurements. The registered minimum of four replicates across three scenarios
is therefore on the order of $40-60. ``--max-budget-usd`` caps each individual
session; ``--scenarios`` and ``--replicates`` narrow the run.

**This script writes to your real Claude Code settings file.** That is the
thing under test: the trials register and unregister Velra's hooks in the
user-level ``settings.json``. The file is restored between arms and a
timestamped backup is written to ``~/.velra/backups`` regardless.

Run it from an ordinary terminal, not from inside a Claude Code session: the
trials drive their own Claude Code processes and the harness strips
``CLAUDECODE``/``CLAUDE_CODE_*`` from their environment, but a nested session
still shares the settings file it is mutating.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent
# This runner was archived into bench/legacy in v0.1.2 Phase 3. Its results,
# harness and binary paths still resolve against the original bench/ tree so
# that every historical artifact keeps the path it was recorded under.
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
HARNESS = BENCH / "harness"
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HARNESS))

from scenarios import registry  # noqa: E402

RESULTS = BENCH / "results" / "v0.1.1"
TRIALS = RESULTS / "trials"
CONTROLS = RESULTS / "controls"

# The portable MinGW toolchain this machine uses for the release build. Absent
# elsewhere, in which case the normal toolchain is already fine.
WINLIBS = pathlib.Path(os.path.expanduser(
    "~/AppData/Local/Programs/winlibs-mingw64/mingw64/bin"))


def step(title: str) -> None:
    print()
    print("=" * 78)
    print(f"  {title}")
    print("=" * 78, flush=True)


def sh(cmd: list, **kw) -> subprocess.CompletedProcess:
    print("$ " + " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run([str(c) for c in cmd], **kw)


def build_env() -> dict:
    env = dict(os.environ)
    if WINLIBS.is_dir():
        env["PATH"] = str(WINLIBS) + os.pathsep + env.get("PATH", "")
    return env


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--replicates", type=int, default=4,
                    help="per arm, per scenario. The pre-registration's "
                         "minimum is 4: below that a perfect split cannot "
                         "reach p < 0.05.")
    ap.add_argument("--scenarios", nargs="*", default=list(registry.DEFAULT_ORDER),
                    choices=list(registry.SCENARIOS) + [],
                    metavar="NAME")
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--max-budget-usd", type=float, default=25.0)
    ap.add_argument("--control-replicates", type=int, default=2)
    ap.add_argument("--fixture-root", default=None)
    ap.add_argument("--skip-build", action="store_true")
    ap.add_argument("--skip-gate", action="store_true",
                    help="skip the offline regression gate (not recommended)")
    ap.add_argument("--skip-cargo-test", action="store_true",
                    help="run the gate but not `cargo test --workspace`")
    ap.add_argument("--allow-dirty", action="store_true",
                    help="proceed with a dirty tree or a stale binary commit, "
                         "recording the provenance failure in every artifact")
    ap.add_argument("--skip-env", action="store_true",
                    help="skip the settings round-trip check, which enables "
                         "and disables Velra in the real user-level "
                         "settings.json. Use it to rehearse the runner's "
                         "phases without touching that file.")
    ap.add_argument("--skip-trials", action="store_true",
                    help="re-analyse the trials already on disk")
    ap.add_argument("--skip-controls", action="store_true")
    ap.add_argument("--skip-tokens", action="store_true")
    ap.add_argument("--skip-native-tokens", action="store_true")
    ap.add_argument("--skip-overhead", action="store_true")
    ap.add_argument("--overhead-runs", type=int, default=200)
    ap.add_argument("--overhead-events", type=int, default=100000)
    args = ap.parse_args()

    started = time.time()
    py = sys.executable
    binary = REPO_ROOT / "target" / "release" / (
        "velra.exe" if os.name == "nt" else "velra")
    fixture_root = pathlib.Path(
        args.fixture_root
        or (pathlib.Path(tempfile.gettempdir()) / "velra-efficacy-fixtures"))
    fixture_root.mkdir(parents=True, exist_ok=True)
    TRIALS.mkdir(parents=True, exist_ok=True)
    CONTROLS.mkdir(parents=True, exist_ok=True)

    if args.replicates < 4:
        print(f"NOTE: --replicates {args.replicates} is below the registered "
              f"minimum of 4. A perfect split cannot reach p < 0.05 at this n, "
              f"so every behavioural hypothesis will be reported INCONCLUSIVE "
              f"however it turns out.", flush=True)

    # ---- phase 0: build ---------------------------------------------------
    if not args.skip_build:
        step("Phase 0 — build the release binary")
        if sh(["cargo", "build", "--release", "-p", "velra"],
              cwd=str(REPO_ROOT), env=build_env()).returncode != 0:
            print("build failed", file=sys.stderr)
            return 1
    if not binary.exists():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 1

    # ---- phase 1: offline gate -------------------------------------------
    if not args.skip_gate:
        step("Phase 1 — offline regression gate (free; no API calls)")
        cmd = [py, HARNESS / "regression_gate.py", "--binary", binary,
               "--out", RESULTS / "regression_gate.json"]
        if args.skip_cargo_test:
            cmd.append("--skip-cargo-test")
        if args.allow_dirty:
            cmd.append("--allow-dirty")
        if sh(cmd, cwd=str(REPO_ROOT)).returncode != 0:
            print("\nThe regression gate failed. Nothing measured after this "
                  "point would be evidence about the binary you think you are "
                  "testing. Fix the failures, or re-run with --allow-dirty if "
                  "the only failure is provenance and you accept that the "
                  "results cannot be attributed to a commit.", file=sys.stderr)
            return 1

    step("Phase 1b — environment and settings round-trip")
    if args.skip_env:
        print("skipped: --skip-env, so the real settings.json is untouched here",
              flush=True)
    else:
        sh([py, HARNESS / "verify_env.py", "--binary", binary,
            "--out", RESULTS / "environment.json"], cwd=str(REPO_ROOT))
    sh([py, HARNESS / "provenance.py", "--binary", binary,
        "--out", RESULTS / "provenance.json"], cwd=str(REPO_ROOT))

    # ---- phase 2: the controls -------------------------------------------
    # Run before the trials: if compaction turns out not to be lossy for a
    # scenario, its behavioural hypothesis is INCONCLUSIVE no matter what the
    # arms do, and you may want to stop rather than pay for eight sessions
    # that cannot decide anything.
    if not args.skip_controls:
        step("Phase 2 — per-scenario information-loss controls (Velra disabled)")
        sh([str(binary), "disable"], cwd=str(REPO_ROOT))
        for name in args.scenarios:
            if sh([py, HARNESS / "loss_probe.py", "--scenario", name,
                   "--fixture", fixture_root / f"control-{name}",
                   "--replicates", args.control_replicates,
                   "--model", args.model,
                   "--max-budget-usd", args.max_budget_usd,
                   "--out", CONTROLS / f"{name}.json"],
                  cwd=str(REPO_ROOT)).returncode != 0:
                print(f"control for {name} failed", file=sys.stderr)
                return 1
        for name in args.scenarios:
            path = CONTROLS / f"{name}.json"
            if path.exists():
                data = json.loads(path.read_text(encoding="utf-8"))
                if not data["compaction_is_lossy"]:
                    print(f"\nNOTE: {name}'s control reports compaction as NOT "
                          f"lossy ({data['lossy_replicates']}/"
                          f"{data['valid_replicates']} runs). Its hypothesis "
                          f"will be INCONCLUSIVE regardless of how the arms "
                          f"score. The trials still run — the capsule-content "
                          f"and token measures do not depend on the control — "
                          f"but do not expect a behavioural answer from it.",
                          flush=True)

    # ---- phase 3: the trials ---------------------------------------------
    trial_dirs: list[pathlib.Path] = []
    for name in args.scenarios:
        for replicate in range(1, args.replicates + 1):
            for arm in ("baseline", "velra"):
                trial = f"{name}-{arm}-r{replicate}"
                out = TRIALS / trial
                trial_dirs.append(out)
                if args.skip_trials:
                    continue
                step(f"Phase 3 — trial {trial}")
                if sh([py, HARNESS / "scenario_trial.py",
                       "--scenario", name, "--arm", arm,
                       "--replicate", replicate, "--model", args.model,
                       "--out", out, "--fixture", fixture_root / trial,
                       "--max-budget-usd", args.max_budget_usd],
                      cwd=str(REPO_ROOT)).returncode != 0:
                    print(f"trial {trial} failed", file=sys.stderr)
                    return 1

    present = [d for d in trial_dirs if (d / "trial_meta.json").exists()]
    if not present:
        print("no trials on disk to analyse", file=sys.stderr)
        return 1

    # ---- phase 4: analysis ------------------------------------------------
    step("Phase 4a — extract telemetry from every trial")
    for d in present:
        sh([py, HARNESS / "scenario_analyze.py", "--trial", d], cwd=str(REPO_ROOT))

    if not args.skip_tokens:
        step("Phase 4b — measure the delivered capsule with Anthropic's tokenizer")
        for d in present:
            if (d / "velra.db").exists():
                sh([py, HARNESS / "measure_tokens.py", "--trial", d,
                    "--model", args.model], cwd=str(REPO_ROOT))

    if not args.skip_native_tokens:
        step("Phase 4c — measure the native compaction summary, same tokenizer")
        for d in present:
            if (d / "native_summary.txt").exists():
                sh([py, HARNESS / "native_tokens.py", "--trial", d,
                    "--model", args.model], cwd=str(REPO_ROOT))

    if not args.skip_overhead:
        step("Phase 4d — hook overhead against a loaded database")
        sh([py, HARNESS / "hook_overhead.py", "--binary", binary,
            "--runs", args.overhead_runs, "--events", args.overhead_events,
            "--out", RESULTS / "hook_overhead.json"], cwd=str(REPO_ROOT))

    step("Phase 4e — aggregate")
    sh([py, HARNESS / "scenario_aggregate.py", *[str(d) for d in present],
        "--out", RESULTS / "aggregate.json"], cwd=str(REPO_ROOT))

    # ---- phase 5: verdicts ------------------------------------------------
    step("Phase 5 — evaluate every registered hypothesis")
    code = sh([py, HARNESS / "scenario_verdict.py", "--results", RESULTS],
              cwd=str(REPO_ROOT)).returncode

    print(f"\ntotal wall time: {time.time() - started:.0f}s")
    print(f"results:         {RESULTS}")
    print(f"verdicts:        {RESULTS / 'verdicts.json'}")
    print("\nNothing here writes the report. Read the raw artifacts and "
          "recompute the headline numbers before believing the summary.")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
