#!/usr/bin/env python3
"""Locate the Claude Code binary the trials should be driven through.

The VS Code extension auto-updates, so the version in its directory name
changes underneath the harness. Pinning one is how a trial ends up pointing at
a binary that is no longer installed -- and, worse, how two trials in the same
comparison end up driven by different binaries without anything on disk saying
so. Every script here resolves the binary through :func:`resolve` and records
what it resolved to.

Resolution order:

1. ``$VELRA_BENCH_CLAUDE`` if it is set, so a specific build can be pinned
   deliberately for a re-run.
2. The highest-versioned ``anthropic.claude-code-*`` VS Code extension. This
   is first because on the machine the benchmark was developed on, that is the
   only install: ``claude`` is not on PATH.
3. ``shutil.which("claude")``.
"""

from __future__ import annotations

import os
import pathlib
import re
import shutil

EXTENSIONS = pathlib.Path(os.path.expanduser("~/.vscode/extensions"))
_VERSION = re.compile(r"^anthropic\.claude-code-(\d+(?:\.\d+)*)")


def _version_key(directory: pathlib.Path) -> tuple:
    """Sort key that orders 2.1.270 above 2.1.69, which a string sort does not."""
    m = _VERSION.match(directory.name)
    return tuple(int(p) for p in m.group(1).split(".")) if m else ()


def candidates() -> list[pathlib.Path]:
    """Every installed extension binary, oldest version first."""
    if not EXTENSIONS.is_dir():
        return []
    found = []
    for ext in EXTENSIONS.iterdir():
        if not _VERSION.match(ext.name):
            continue
        for name in ("claude.exe", "claude"):
            binary = ext / "resources" / "native-binary" / name
            if binary.is_file():
                found.append(binary)
                break
    return sorted(found, key=lambda p: _version_key(p.parents[2]))


def resolve() -> pathlib.Path:
    pinned = os.environ.get("VELRA_BENCH_CLAUDE")
    if pinned:
        path = pathlib.Path(os.path.expanduser(pinned))
        if not path.is_file():
            raise SystemExit(f"VELRA_BENCH_CLAUDE={pinned} is not a file")
        return path

    installed = candidates()
    if installed:
        return installed[-1]

    on_path = shutil.which("claude")
    if on_path:
        return pathlib.Path(on_path)

    raise SystemExit(
        "no Claude Code binary found: nothing on PATH and no "
        f"anthropic.claude-code-* extension under {EXTENSIONS}. "
        "Set VELRA_BENCH_CLAUDE to point at one.")


def version_of(binary: pathlib.Path) -> str | None:
    """The version the resolved path advertises, for the trial record."""
    for parent in binary.parents:
        m = _VERSION.match(parent.name)
        if m:
            return m.group(1)
    return None


if __name__ == "__main__":
    b = resolve()
    print(f"{b}  (version {version_of(b) or 'unknown'})")
