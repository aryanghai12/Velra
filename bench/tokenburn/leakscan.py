#!/usr/bin/env python3
"""Scan every surface a destination session can read, for the answer.

A scenario measures recall only if the thing it claims was lost is not simply
lying around somewhere. The archived scanner covered four surfaces. §8 of the
Phase 3 specification names nine, and adds one the archived version explicitly
skipped: **git history**. S1 planted its dead ends as commits, which is fine
when the scenario is about compaction inside one session and wrong when the
claim is that a fresh session cannot recover the state — ``git log -p`` is two
tool calls away.

Surfaces
--------

``tree``            every readable text file's contents
``filenames``       paths, which are read as readily as contents
``git_history``     every commit message and every patch in the log
``git_refs``        branch, tag and ref names
``claude_md``       ``CLAUDE.md`` anywhere in the tree, at any depth
``project_memory``  ``.claude/`` directory contents
``auto_memory``     Claude Code's per-project auto-memory directory,
                    ``<config>/projects/<slug>/memory/`` -- outside the repo
``environment``     variables visible to the trial process
``post_prompt``     the continuation prompt, identical on both arms
``pre_prompts``     the turn script before the transition

Everything but ``pre_prompts`` is fatal. The turn script *is* where the state
is established — the constraint is stated aloud once, the dead end is tried
once — so a hit there is the setup working, not a leak. Those turns never reach
the destination session on either arm.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
from typing import Sequence

#: A hit on any of these means the destination could read the answer.
FATAL_SURFACES = ("tree", "filenames", "git_history", "git_refs", "claude_md",
                  "project_memory", "auto_memory", "environment",
                  "post_prompt")

ALL_SURFACES = FATAL_SURFACES + ("pre_prompts",)

#: Variables that legitimately contain the fixture path and would otherwise
#: trip the scan on every run.
ENV_ALLOW = ("PATH", "PATHEXT", "PSMODULEPATH", "TEMP", "TMP", "TMPDIR",
             "PWD", "OLDPWD", "VIRTUAL_ENV", "CONDA_PREFIX")

#: Files that are never text, whatever their extension claims.
SKIP_SUFFIXES = (".png", ".jpg", ".jpeg", ".gif", ".ico", ".pdf", ".zip",
                 ".gz", ".db", ".wal", ".shm", ".pyc", ".exe", ".dll")


def _hits(haystack: str, terms: Sequence[str], surface: str,
          label: str) -> list[dict]:
    low = haystack.lower()
    out = []
    for term in terms:
        needle = term.lower()
        if needle and needle in low:
            index = low.index(needle)
            out.append({
                "surface": surface,
                "where": label,
                "term": term,
                "context": haystack[max(0, index - 60):
                                    index + len(term) + 60].strip()[:200],
            })
    return out


def scan_tree(repo: pathlib.Path, terms: Sequence[str]) -> list[dict]:
    """Contents and paths of everything outside ``.git``."""
    out: list[dict] = []
    for path in sorted(repo.rglob("*")):
        if ".git" in path.parts:
            continue
        rel = str(path.relative_to(repo)).replace("\\", "/")
        out.extend(_hits(rel, terms, "filenames", rel))
        if not path.is_file() or path.suffix.lower() in SKIP_SUFFIXES:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for number, line in enumerate(text.splitlines(), 1):
            out.extend(_hits(line.strip(), terms, "tree", f"{rel}:{number}"))
    return out


def _git(repo: pathlib.Path, *args: str) -> str:
    try:
        proc = subprocess.run(["git", *args], cwd=str(repo),
                              capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=120)
    except (OSError, subprocess.SubprocessError):
        return ""
    return proc.stdout or ""


def scan_git(repo: pathlib.Path, terms: Sequence[str]) -> list[dict]:
    """Commit messages, patches, refs and stashes.

    ``git log --all -p`` is the surface that matters: it is what an agent runs
    when it wants to know what was tried before, and a dead end recorded as a
    commit is fully readable from it. The reflog is included because a reverted
    commit is still reachable through it.
    """
    out: list[dict] = []
    history = _git(repo, "log", "--all", "-p", "--format=%H%n%s%n%b")
    for number, line in enumerate(history.splitlines(), 1):
        out.extend(_hits(line.strip(), terms, "git_history", f"git log:{number}"))
    reflog = _git(repo, "reflog", "--all")
    for number, line in enumerate(reflog.splitlines(), 1):
        out.extend(_hits(line.strip(), terms, "git_history", f"reflog:{number}"))
    stash = _git(repo, "stash", "list")
    for number, line in enumerate(stash.splitlines(), 1):
        out.extend(_hits(line.strip(), terms, "git_history", f"stash:{number}"))
    refs = _git(repo, "for-each-ref", "--format=%(refname)")
    for line in refs.splitlines():
        out.extend(_hits(line.strip(), terms, "git_refs", line.strip()))
    return out


def scan_agent_context(repo: pathlib.Path, terms: Sequence[str]) -> list[dict]:
    """``CLAUDE.md`` at any depth, and anything under ``.claude/``.

    Both are loaded into a session automatically. A scenario whose state leaked
    into either would be handing the destination the answer before its first
    turn, on both arms, and the benchmark would be measuring nothing.
    """
    out: list[dict] = []
    for path in sorted(repo.rglob("CLAUDE.md")):
        rel = str(path.relative_to(repo)).replace("\\", "/")
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for number, line in enumerate(text.splitlines(), 1):
            out.extend(_hits(line.strip(), terms, "claude_md", f"{rel}:{number}"))
    memory = repo / ".claude"
    if memory.is_dir():
        for path in sorted(memory.rglob("*")):
            rel = str(path.relative_to(repo)).replace("\\", "/")
            out.extend(_hits(rel, terms, "project_memory", rel))
            if not path.is_file() or path.suffix.lower() in SKIP_SUFFIXES:
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            for number, line in enumerate(text.splitlines(), 1):
                out.extend(_hits(line.strip(), terms, "project_memory",
                                 f"{rel}:{number}"))
    return out


def scan_auto_memory(directory: pathlib.Path | None,
                     terms: Sequence[str]) -> list[dict]:
    """Claude Code's auto-memory for the fixture, paths and contents.

    It lives outside the repository, which is why every other surface missed
    it: in the v0.1.2 qualification it carried the scenario's unwritten
    constraint into both arms' destination sessions. The trial harness also
    requires the directory to be *empty* (`isolation.scan_memory`); term hits
    here are the evidence of what a leak said.
    """
    out: list[dict] = []
    if directory is None or not pathlib.Path(directory).is_dir():
        return out
    directory = pathlib.Path(directory)
    for path in sorted(directory.rglob("*")):
        rel = str(path.relative_to(directory)).replace("\\", "/")
        out.extend(_hits(rel, terms, "auto_memory", f"memory/{rel}"))
        if not path.is_file() or path.suffix.lower() in SKIP_SUFFIXES:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for number, line in enumerate(text.splitlines(), 1):
            out.extend(_hits(line.strip(), terms, "auto_memory",
                             f"memory/{rel}:{number}"))
    return out


def scan_prompts(turns: Sequence[str], continuation: str,
                 terms: Sequence[str]) -> list[dict]:
    out: list[dict] = []
    for index, text in enumerate(turns):
        out.extend(_hits(text, terms, "pre_prompts", f"turn {index}"))
    out.extend(_hits(continuation, terms, "post_prompt", "continuation prompt"))
    return out


def scan_environment(terms: Sequence[str], env: dict | None = None) -> list[dict]:
    env = os.environ if env is None else env
    out: list[dict] = []
    for key, value in sorted(env.items()):
        if key.upper() in ENV_ALLOW:
            continue
        out.extend(_hits(f"{key}={value}", terms, "environment", key))
    return out


def scan(repo: pathlib.Path, turns: Sequence[str], continuation: str,
         terms: Sequence[str], env: dict | None = None,
         memory_dir: pathlib.Path | None = None) -> dict:
    """Every surface at once. ``clean`` is false when any fatal surface hits."""
    repo = pathlib.Path(repo)
    hits = (scan_tree(repo, terms)
            + scan_git(repo, terms)
            + scan_agent_context(repo, terms)
            + scan_auto_memory(memory_dir, terms)
            + scan_prompts(turns, continuation, terms)
            + scan_environment(terms, env))
    fatal = [h for h in hits if h["surface"] in FATAL_SURFACES]
    by_surface: dict[str, int] = {}
    for hit in hits:
        by_surface[hit["surface"]] = by_surface.get(hit["surface"], 0) + 1
    return {
        "clean": not fatal,
        "terms": list(terms),
        "surfaces_scanned": list(ALL_SURFACES),
        "auto_memory_dir": str(memory_dir) if memory_dir else None,
        "fatal_surfaces": list(FATAL_SURFACES),
        "hits_by_surface": by_surface,
        "fatal_hits": fatal,
        "hits": hits[:400],
        "n_hits": len(hits),
    }


def format_report(result: dict) -> str:
    if result["clean"]:
        counts = result["hits_by_surface"]
        return (f"leak scan clean across {len(result['surfaces_scanned'])} "
                f"surfaces" + (f" (non-fatal: {counts})" if counts else ""))
    lines = ["leak scan FAILED -- the state is readable without the capsule:"]
    for hit in result["fatal_hits"][:20]:
        lines.append(f"  [{hit['surface']}] {hit['where']}: {hit['term']!r}")
        lines.append(f"      {hit['context']}")
    return "\n".join(lines)
