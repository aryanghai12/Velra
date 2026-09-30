#!/usr/bin/env python3
"""The fixed cost of starting a hook process, measured the way run.sh does.

    python bench/legacy/spawn_floor.py <results dir> [--runs 500] [--warmup 20]

run.sh times each hook from spawn to exit. Without hyperfine it uses a
Python timer, so every number includes process creation and Python's own
subprocess overhead, which on some machines is most of it. This script
times ``velra hook post-tool-use`` with ``VELRA_DISABLE=1``: the binary
starts, sees the kill switch and exits without reading its input or opening
the database. The same timer and loop as run.sh's ``measure_builtin``, so
the result is the floor under every hook row run.sh reports on this machine.

It writes ``kill-switch-floor.json`` (run.sh's JSON shape) and
``environment.json`` (binary identity, machine, parameters) into the
results directory.
"""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=pathlib.Path)
    ap.add_argument("--runs", type=int, default=int(os.environ.get("VELRA_BENCH_RUNS", 500)))
    ap.add_argument("--warmup", type=int, default=int(os.environ.get("VELRA_BENCH_WARMUP", 20)))
    args = ap.parse_args()

    binary = ROOT / "target" / "release" / ("velra.exe" if os.name == "nt" else "velra")
    if not binary.exists():
        print("build the release binary first: cargo build --release -p velra", file=sys.stderr)
        return 1
    payload = json.dumps({"session_id": "bench-session", "cwd": str(ROOT),
                          "hook_event_name": "PostToolUse", "tool_name": "Read",
                          "tool_use_id": "toolu_floor",
                          "tool_response": {"content": "x" * 2000}}).encode()
    env = dict(os.environ, VELRA_DISABLE="1")

    times = []
    for i in range(args.warmup + args.runs):
        start = time.perf_counter()
        subprocess.run([str(binary), "hook", "post-tool-use"], input=payload, env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        elapsed = time.perf_counter() - start
        if i >= args.warmup:
            times.append(elapsed)

    args.out.mkdir(parents=True, exist_ok=True)
    with open(args.out / "kill-switch-floor.json", "w", encoding="utf-8", newline="\n") as f:
        json.dump({"results": [{"times": times}]}, f)

    version = subprocess.run([str(binary), "--version"], capture_output=True, text=True).stdout.strip()
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    env_record = {
        "binary": {"version": version, "sha256": digest},
        "platform": f"{platform.system()} {platform.release()} {platform.version()} {platform.machine()}",
        "processor": platform.processor(),
        "cpu_count": os.cpu_count(),
        "python": platform.python_version(),
        "timer": "builtin (Python time.perf_counter around subprocess.run)",
        "runs": args.runs,
        "warmup": args.warmup,
    }
    with open(args.out / "environment.json", "w", encoding="utf-8", newline="\n") as f:
        json.dump(env_record, f, indent=2)
        f.write("\n")

    ms = sorted(t * 1000 for t in times)
    pct = lambda p: ms[min(len(ms) - 1, int(round(p / 100 * (len(ms) - 1))))]
    print(f"kill-switch floor: p50 {pct(50):.2f} ms | p99 {pct(99):.2f} ms  ({version})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
