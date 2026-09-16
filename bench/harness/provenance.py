#!/usr/bin/env python3
"""Record exactly what was under test, from outside the binary.

The v0.1 report attributes its four-replicate dataset to `velra 0.1.0
(e9f40151c)`. That string comes from `build.rs`, which stamps `git rev-parse
HEAD` into the binary and declares `rerun-if-changed=../../.git/HEAD`. On a
branch, `.git/HEAD` holds `ref: refs/heads/main` and does not change when you
commit — only the ref file does — so cargo does not re-run the build script and
the embedded sha goes stale. The trials dated 2026-09-14 were run from a tree
at `6dd3da2`/`c66beb5` while the binary still reported `e9f40151c`.

So provenance is taken here, from git, and cross-checked against what the
binary says. A mismatch does not stop the run; it is recorded, loudly, in every
artifact, because a benchmark that cannot say which code it measured is not
evidence.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import platform
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent

sys.path.insert(0, str(HERE))
import claude_binary  # noqa: E402


def _git(*args: str) -> str | None:
    try:
        out = subprocess.run(["git", *args], cwd=str(REPO_ROOT), capture_output=True,
                             text=True, encoding="utf-8", errors="replace")
    except OSError:
        return None
    return out.stdout.strip() if out.returncode == 0 else None


def collect(binary: pathlib.Path) -> dict:
    version = subprocess.run([str(binary), "--version"], capture_output=True,
                             text=True, encoding="utf-8", errors="replace")
    reported = version.stdout.strip()
    head = _git("rev-parse", "HEAD")
    short = _git("rev-parse", "--short=9", "HEAD")
    dirty = _git("status", "--porcelain")

    embedded = None
    if "(" in reported and "," in reported:
        embedded = reported.split("(", 1)[1].split(",", 1)[0].strip()

    record = {
        "binary": str(binary),
        "binary_bytes": binary.stat().st_size if binary.exists() else None,
        "reported_version": reported,
        "embedded_commit": embedded,
        "git_head": head,
        "git_head_short9": short,
        "git_branch": _git("rev-parse", "--abbrev-ref", "HEAD"),
        "git_describe": _git("describe", "--tags", "--always", "--dirty"),
        "working_tree_clean": dirty == "",
        "working_tree_changes": (dirty or "").splitlines(),
        "commit_matches_binary": bool(embedded and short and embedded == short),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": sys.version.split()[0],
            "processor": platform.processor(),
            "cpu_count": os.cpu_count(),
        },
    }
    try:
        agent = claude_binary.resolve()
        record["agent"] = {
            "binary": str(agent),
            "version": claude_binary.version_of(agent),
        }
    except SystemExit as exc:
        record["agent"] = {"error": str(exc)}
    record["warnings"] = warnings(record)
    return record


def warnings(record: dict) -> list[str]:
    out = []
    if not record["working_tree_clean"]:
        out.append(
            "the working tree is dirty: results cannot be attributed to a commit")
    if not record["commit_matches_binary"]:
        out.append(
            f"the binary reports commit {record['embedded_commit']} but HEAD is "
            f"{record['git_head_short9']}; the binary is stale or was built from "
            "a different tree. Rebuild with `cargo build --release -p velra` "
            "after touching crates/velra/build.rs, or trust git_head over "
            "reported_version.")
    if record.get("agent", {}).get("error"):
        out.append("no Claude Code binary could be resolved")
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    record = collect(pathlib.Path(args.binary).resolve())
    text = json.dumps(record, indent=2)
    if args.out:
        out = pathlib.Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(text, encoding="utf-8", newline="")
    print(text)
    for warning in record["warnings"]:
        print(f"  WARNING: {warning}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
