#!/usr/bin/env python3
"""Run the entire Velra benchmark suite end to end.

    python bench/run_full_benchmark.py

Everything the report claims is produced by this script: the environment
checks, the fixture, both arms of every replicate, the telemetry extraction,
the token measurement, the hook-overhead benchmark, the aggregate tables and
the proof asset. It prints a verdict for each of the four hypotheses at the
end.

Requirements: a Rust toolchain, Python 3.11+, Git, pytest, and an
authenticated Claude Code installation. The script locates the Claude Code
binary itself; if it cannot, pass --claude-binary.

Cost: the trials call the Claude API. Each replicate is two sessions of
sixteen turns. Expect roughly two to three US dollars per replicate on Sonnet;
--max-budget-usd caps each session.

WARNING: the trials register and unregister Velra's hooks in the *real*
user-level Claude Code settings file, because that is the thing being tested.
The file is restored between arms and the script verifies it byte for byte;
a timestamped backup is written to ~/.velra/backups regardless.
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
import time

HERE = pathlib.Path(__file__).resolve().parent
# This runner was archived into bench/legacy in v0.1.2 Phase 3. Its results,
# harness and binary paths still resolve against the original bench/ tree so
# that every historical artifact keeps the path it was recorded under.
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
HARNESS = BENCH / "harness"
RESULTS = BENCH / "results"
TRIALS = RESULTS / "trials"

# The portable MinGW toolchain this machine uses for the release build. Absent
# elsewhere, in which case the normal toolchain is already fine.
WINLIBS = pathlib.Path(os.path.expanduser(
    "~/AppData/Local/Programs/winlibs-mingw64/mingw64/bin"))


def step(title: str) -> None:
    print()
    print("=" * 78)
    print(f"  {title}")
    print("=" * 78, flush=True)


def sh(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    print("$ " + " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run([str(c) for c in cmd], **kw)


def build_env() -> dict:
    env = dict(os.environ)
    if WINLIBS.is_dir():
        env["PATH"] = str(WINLIBS) + os.pathsep + env.get("PATH", "")
    return env


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--replicates", type=int, default=3)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--protocol", default="saturated", choices=("saturated", "short"))
    ap.add_argument("--max-budget-usd", type=float, default=25.0)
    ap.add_argument("--skip-build", action="store_true")
    ap.add_argument("--skip-trials", action="store_true",
                    help="re-analyse the trials already in bench/results/trials")
    ap.add_argument("--skip-overhead", action="store_true")
    ap.add_argument("--skip-control", action="store_true")
    ap.add_argument("--overhead-runs", type=int, default=200)
    ap.add_argument("--overhead-events", type=int, default=100000)
    ap.add_argument("--fixture-root", default=None)
    args = ap.parse_args()

    started = time.time()
    py = sys.executable
    binary = REPO_ROOT / "target" / "release" / (
        "velra.exe" if os.name == "nt" else "velra")
    fixture_root = pathlib.Path(
        args.fixture_root or (pathlib.Path(tempfile.gettempdir()) / "velra-bench-fixtures"))
    fixture_root.mkdir(parents=True, exist_ok=True)
    TRIALS.mkdir(parents=True, exist_ok=True)

    # ---- Phase 1 ---------------------------------------------------------
    if not args.skip_build:
        step("Phase 1a - build the release binary")
        r = sh(["cargo", "build", "--release", "-p", "velra"],
               cwd=str(REPO_ROOT), env=build_env())
        if r.returncode != 0:
            print("build failed", file=sys.stderr)
            return 1
    if not binary.exists():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 1

    step("Phase 1b - verify the binary and the settings round-trip")
    if sh([py, HARNESS / "verify_env.py", "--binary", binary,
           "--out", RESULTS / "phase1_environment.json"],
          cwd=str(REPO_ROOT)).returncode != 0:
        print("phase 1 verification failed", file=sys.stderr)
        return 1

    # ---- Phases 2 and 3 --------------------------------------------------
    trial_dirs: list[pathlib.Path] = []
    for replicate in range(1, args.replicates + 1):
        for arm in ("baseline", "velra"):
            name = f"{args.protocol}-{arm}-r{replicate}"
            out = TRIALS / name
            trial_dirs.append(out)
            if args.skip_trials:
                continue
            step(f"Phases 2+3 - trial {name}")
            r = sh([py, HARNESS / "run_trial.py",
                    "--arm", arm, "--replicate", replicate,
                    "--protocol", args.protocol, "--model", args.model,
                    "--out", out, "--fixture", fixture_root / name,
                    "--max-budget-usd", args.max_budget_usd],
                   cwd=str(REPO_ROOT))
            if r.returncode != 0:
                print(f"trial {name} failed", file=sys.stderr)
                return 1

    # ---- Phase 4 ---------------------------------------------------------
    step("Phase 4a - extract telemetry from every trial")
    present = [d for d in trial_dirs if (d / "trial_meta.json").exists()]
    for d in present:
        sh([py, HARNESS / "analyze.py", "--trial", d], cwd=str(REPO_ROOT))

    step("Phase 4b - measure the injected block with Anthropic's tokenizer")
    for d in present:
        if (d / "velra.db").exists():
            sh([py, HARNESS / "measure_tokens.py", "--trial", d,
                "--model", args.model], cwd=str(REPO_ROOT))

    if not args.skip_control:
        step("Phase 4c - control: was Claude Code's own compaction lossy at all?")
        # Hypotheses 1 and 2 only mean anything if compaction actually drops
        # something. Velra is disabled for this, so it measures vanilla
        # Claude Code and nothing else.
        sh([str(binary), "disable"], cwd=str(REPO_ROOT))
        sh([py, HARNESS / "compaction_probe.py",
            "--fixture", fixture_root / "compaction-probe",
            "--model", args.model,
            "--out", RESULTS / "compaction_probe.json"], cwd=str(REPO_ROOT))

    if not args.skip_overhead:
        step("Phase 4d - hook overhead against a loaded database")
        sh([py, HARNESS / "hook_overhead.py", "--binary", binary,
            "--runs", args.overhead_runs, "--events", args.overhead_events,
            "--out", RESULTS / "hook_overhead.json"], cwd=str(REPO_ROOT))

    step("Phase 4e - aggregate")
    sh([py, HARNESS / "aggregate.py", *[str(d) for d in present],
        "--out", RESULTS / "aggregate.json"], cwd=str(REPO_ROOT))

    step("Phase 4f - extract the verbatim evidence the report cites")
    sh([py, HARNESS / "make_evidence.py", "--out", RESULTS / "EVIDENCE.md"],
       cwd=str(REPO_ROOT))

    # ---- verdicts --------------------------------------------------------
    # Must run before the proof asset: make_proof.py renders its verdict
    # strip from verdicts.json, so evaluating afterwards would stamp the
    # asset with the *previous* run's verdicts.
    step("Final evaluation")
    r = sh([py, HARNESS / "verdict.py"], cwd=str(REPO_ROOT))

    # ---- Phase 5 ---------------------------------------------------------
    step("Phase 5 - proof asset")
    sh([py, HARNESS / "make_proof.py", "--out", REPO_ROOT / "assets" / "proof.svg"],
       cwd=str(REPO_ROOT))
    print(f"\ntotal wall time: {time.time() - started:.0f}s")
    return r.returncode


if __name__ == "__main__":
    raise SystemExit(main())
