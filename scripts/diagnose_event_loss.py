#!/usr/bin/env python3
"""Reproduce the c1 event-loss flake outside cargo, keeping the evidence.

Drives the real velra binary exactly as `c1_parallel_processes_lose_no_events`
does -- 8 threads x 25 PostToolUse events into one fresh VELRA_HOME -- then
reports, for any run that comes up short, where the missing events went.
"""

import concurrent.futures
import json
import os
import pathlib
import shutil
import sqlite3
import subprocess
import sys
import tempfile

REPO = pathlib.Path(r"C:\Users\aryan\Videos\Velra")
BIN = REPO / "target" / "debug" / "velra.exe"
THREADS, PER = 8, 25


def one_run(run_index):
    root = pathlib.Path(tempfile.mkdtemp(prefix="velra-c1-"))
    home, project, config = root / "home", root / "project", root / "config"
    for d in (home, project, config):
        d.mkdir(parents=True)
    (project / "src").mkdir()
    (project / "src" / "a.rs").write_text("v0\n", encoding="utf-8", newline="")
    session = "0199b0f2-4c31-7c8a-9f1e-2b6d5a0e7c11"

    env = dict(os.environ)
    env.update({
        "VELRA_HOME": str(home),
        "CLAUDE_CONFIG_DIR": str(config),
        "CLAUDE_PROJECT_DIR": str(project),
        "TZ": "UTC",
        "VELRA_TEST_WATCHDOG_MS": "60000",
    })

    def fire(index):
        payload = json.dumps({
            "session_id": session,
            "hook_event_name": "PostToolUse",
            "cwd": str(project),
            "tool_name": "Read",
            "tool_use_id": "toolu_%d" % index,
            "tool_input": {"file_path": str(project / "src" / "a.rs")},
            "tool_response": {"ok": True},
        })
        p = subprocess.run([str(BIN), "hook", "post-tool-use"], input=payload,
                           capture_output=True, text=True, env=env)
        return index, p.returncode, p.stderr

    indices = [p * 10_000 + i for p in range(THREADS) for i in range(PER)]
    bad = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=THREADS) as pool:
        for index, code, err in pool.map(fire, indices):
            if code != 0 or err:
                bad.append((index, code, err[:200]))

    # Drain, the way the harness does.
    subprocess.run([str(BIN), "reduce"], capture_output=True, text=True, env=env)

    db = home / "velra.db"
    con = sqlite3.connect("file:%s?mode=ro" % db.as_posix(), uri=True)
    events = con.execute(
        "SELECT COUNT(*) FROM events WHERE tool_name = 'Read'").fetchone()[0]
    present = {r[0] for r in con.execute(
        "SELECT tool_use_id FROM events WHERE tool_name = 'Read'")}
    con.close()

    spool = home / "spool"
    backlog = sorted(spool.glob("*.jsonl")) if spool.is_dir() else []
    missing = [i for i in indices if ("toolu_%d" % i) not in present]

    ok = events == len(indices)
    print("run %d: events=%d/%d  spool_backlog=%d  hook_failures=%d  %s"
          % (run_index, events, len(indices), len(backlog), len(bad),
             "OK" if ok else "SHORT"))
    if not ok:
        print("   missing tool_use_ids:", ["toolu_%d" % i for i in missing][:10])
        print("   spool files left:", [f.name for f in backlog][:10])
        log = home / "logs" / "debug.log"
        if log.exists():
            tail = log.read_text(encoding="utf-8", errors="replace").splitlines()[-25:]
            print("   debug.log tail:")
            for line in tail:
                print("     ", line[:220])
        else:
            print("   no debug.log")
        keep = REPO.parent / ("velra-c1-evidence-%d" % run_index)
        shutil.rmtree(keep, ignore_errors=True)
        shutil.copytree(root, keep)
        print("   evidence kept at", keep)
    shutil.rmtree(root, ignore_errors=True)
    return ok


def main():
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 8
    short = sum(0 if one_run(i) else 1 for i in range(1, runs + 1))
    print("\n%d of %d runs lost events" % (short, runs))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
