#!/usr/bin/env python3
"""Measure hook overhead directly, against a loaded database.

The in-session numbers taken from Claude Code's event stream are useful but
coarse: they include Claude Code's own dispatch, and for the handlers Velra
registers with ``"async": true`` they measure when a background job finished
rather than how long the agent waited. This script measures the binary itself.

Three quantities are reported per hook:

  spawn control   the same binary run with VELRA_DISABLE=1, which makes it exit
                  immediately without touching the database. This is the cost
                  of starting a process on this machine and nothing else.
  total           the real invocation, end to end.
  marginal        total - spawn control: the work Velra actually does.

The spec's budget is written against total wall time, so ``total`` is the
number that decides the hypothesis; ``marginal`` is reported because on Windows
process creation dominates and it is worth showing separately.

The database is seeded first, because an empty database is not a fair test:
every measurement below runs against 100,000 events unless --events says
otherwise.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import statistics
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
FIXTURES = REPO_ROOT / "tests" / "fixtures" / "claude-code" / "2.1.268"

# Windows allowance on Velra's own work, over and above starting a process.
# See DECISIONS.md D54: the spec's 15 ms is wall time, which on Windows an empty
# process can already exceed, so it is not a statement about this program.
MARGINAL_BUDGET_P99_MS = 15

# hook subcommand -> recorded payload
CASES = [
    ("post-tool-use", "post_tool_use_read.json", "PostToolUse, Read (small)"),
    ("post-tool-use", "post_tool_use_edit.json", "PostToolUse, Edit"),
    ("post-tool-use", "post_tool_use_bash.json", "PostToolUse, Bash"),
    ("post-tool-use-failure", "post_tool_use_failure_bash.json", "PostToolUseFailure, Bash"),
    ("pre-tool-use", "pre_tool_use_edit.json", "PreToolUse, Edit"),
    ("user-prompt-submit", "user_prompt_submit.json", "UserPromptSubmit"),
    ("session-start", "session_start_startup.json", "SessionStart, startup"),
    ("stop", "stop.json", "Stop"),
    ("pre-compact", "pre_compact_auto.json", "PreCompact"),
]


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    k = max(0, min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1)))))
    return round(xs[k], 3)


def time_runs(binary: pathlib.Path, argv: list[str], payload: str,
              env: dict, runs: int, warmup: int) -> list[float]:
    samples: list[float] = []
    for i in range(runs + warmup):
        start = time.perf_counter()
        proc = subprocess.run([str(binary), *argv], input=payload,
                              capture_output=True, text=True,
                              encoding="utf-8", errors="replace", env=env)
        elapsed = (time.perf_counter() - start) * 1000.0
        if i >= warmup:
            samples.append(elapsed)
        if proc.returncode != 0:
            raise SystemExit(
                f"hook exited {proc.returncode} (the contract says always 0): "
                f"{proc.stderr[:400]}")
        if proc.stderr:
            raise SystemExit(f"hook wrote to stderr: {proc.stderr[:400]}")
    return samples


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="bench/results/hook_overhead.json")
    ap.add_argument("--runs", type=int, default=200)
    ap.add_argument("--warmup", type=int, default=25)
    ap.add_argument("--events", type=int, default=100000)
    ap.add_argument("--binary",
                    default=str(REPO_ROOT / "target" / "release" / "velra.exe"))
    ap.add_argument("--skip-seed", action="store_true")
    args = ap.parse_args()

    binary = pathlib.Path(args.binary).resolve()
    if not binary.exists():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 1

    work = pathlib.Path(os.environ.get("TEMP", "/tmp")) / "velra-hook-overhead"
    if work.exists():
        shutil.rmtree(work, ignore_errors=True)
    home = work / "home"
    project = work / "project"
    home.mkdir(parents=True)
    project.mkdir(parents=True)

    env = dict(os.environ)
    env["VELRA_HOME"] = str(home)
    env["CLAUDE_PROJECT_DIR"] = str(project)
    env.pop("VELRA_LOG", None)
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)

    if not args.skip_seed:
        print(f"seeding {args.events} events ...", flush=True)
        seed = subprocess.run(
            ["cargo", "run", "--release", "--quiet", "-p", "velra",
             "--example", "seed", "--", str(home), str(args.events), str(project)],
            cwd=str(REPO_ROOT), capture_output=True, text=True,
                              encoding="utf-8", errors="replace", env=env)
        if seed.returncode != 0:
            print("seeding failed:", seed.stderr[-2000:], file=sys.stderr)
            return 1
    db = home / "velra.db"
    db_bytes = db.stat().st_size if db.exists() else 0
    print(f"database: {db_bytes/1024/1024:.1f} MiB", flush=True)

    # ---- the spawn control ------------------------------------------------
    disabled_env = dict(env)
    disabled_env["VELRA_DISABLE"] = "1"
    payload = (FIXTURES / "post_tool_use_read.json").read_text(encoding="utf-8")
    print("measuring the spawn control (VELRA_DISABLE=1) ...", flush=True)
    control = time_runs(binary, ["hook", "post-tool-use"], payload,
                        disabled_env, args.runs, args.warmup)
    control_p50 = pct(control, 50)
    print(f"  spawn control: p50 {control_p50} ms, p99 {pct(control, 99)} ms",
          flush=True)

    results = []
    for subcommand, fixture, label in CASES:
        path = FIXTURES / fixture
        if not path.exists():
            print(f"  (missing fixture {fixture}, skipped)", file=sys.stderr)
            continue
        body = path.read_text(encoding="utf-8")
        samples = time_runs(binary, ["hook", subcommand], body,
                            env, args.runs, args.warmup)
        row = {
            "label": label,
            "subcommand": subcommand,
            "fixture": fixture,
            "payload_bytes": len(body.encode("utf-8")),
            "runs": len(samples),
            "p50_ms": pct(samples, 50),
            "p95_ms": pct(samples, 95),
            "p99_ms": pct(samples, 99),
            "max_ms": round(max(samples), 3),
            "mean_ms": round(statistics.fmean(samples), 3),
            "marginal_p50_ms": round(pct(samples, 50) - control_p50, 3),
            "marginal_p99_ms": round(pct(samples, 99) - pct(control, 99), 3),
        }
        results.append(row)
        print(f"  {label:34} p50 {row['p50_ms']:7.3f} ms   "
              f"p99 {row['p99_ms']:7.3f} ms   "
              f"marginal p50 {row['marginal_p50_ms']:7.3f} ms", flush=True)

    worst_p99 = max((r["p99_ms"] for r in results), default=None)
    worst_marginal_p99 = max((r["marginal_p99_ms"] for r in results), default=None)
    # Two verdicts, because the two numbers answer different questions.
    #
    # `total` is what the spec's budget is written against, and on Windows it is
    # dominated by process creation rather than by Velra: the spawn control
    # above starts the same binary and exits before opening the database, and
    # its own p99 is frequently a large fraction of the whole allowance. A
    # budget that an empty process cannot meet says nothing about the program.
    #
    # `marginal` is total minus that control: the work Velra actually does. It
    # is the number README and DECISIONS.md D54 state the Windows budget
    # against. Both are reported; neither is hidden.
    out = {
        "platform": sys.platform,
        "binary": str(binary),
        "database_bytes": db_bytes,
        "seeded_events": 0 if args.skip_seed else args.events,
        "runs_per_case": args.runs,
        "warmup_per_case": args.warmup,
        "spawn_control": {
            "p50_ms": control_p50, "p95_ms": pct(control, 95),
            "p99_ms": pct(control, 99), "max_ms": round(max(control), 3),
        },
        "cases": results,
        "worst_case_p99_ms": worst_p99,
        "worst_case_marginal_p99_ms": worst_marginal_p99,
        "windows_budget_p99_ms": 15,
        "windows_marginal_budget_p99_ms": MARGINAL_BUDGET_P99_MS,
        "within_windows_budget": (worst_p99 is not None and worst_p99 <= 15),
        "within_windows_marginal_budget": (
            worst_marginal_p99 is not None
            and worst_marginal_p99 <= MARGINAL_BUDGET_P99_MS
        ),
    }
    target = pathlib.Path(args.out)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(out, indent=2), encoding="utf-8", newline="")
    print(f"\nspawn control alone:        p99 {pct(control, 99)} ms")
    print(f"worst-case p99, total:      {worst_p99} ms "
          f"(wall-time budget 15 ms) -> "
          f"{'WITHIN' if out['within_windows_budget'] else 'OVER'}")
    print(f"worst-case p99, marginal:   {worst_marginal_p99} ms "
          f"(marginal budget {MARGINAL_BUDGET_P99_MS} ms) -> "
          f"{'WITHIN' if out['within_windows_marginal_budget'] else 'OVER'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
