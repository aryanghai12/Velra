#!/usr/bin/env python3
"""Ceiling-effect guard: scan everything an agent can see for the answer.

A scenario measures recall only if the answer is not lying around somewhere the
agent can simply read. The v0.1 fixture shipped two such leaks — a ``TODO``
naming the fix, and a docstring on the failing test saying "The discount is a
property of the invoice, not of each line", which at least one recorded
replicate quotes back as its reasoning. The original ``lint_tree`` caught the
first class and only that class: it scans generated files.

This scans every surface the post-compaction agent has, which is a longer list:

  ``tree``            every tracked text file, as before
  ``filenames``       paths are read as often as contents
  ``post_prompt``     the measured turn's own text
  ``pre_prompts``     every turn before the boundary; a leak here is different
                      in kind (it is *supposed* to be said aloud once) so only
                      the measured-turn-adjacent terms are enforced
  ``manifest``        the ground-truth file, which must never reach the fixture
  ``environment``     variables visible to the trial process

A hit in ``tree``, ``filenames``, ``post_prompt`` or ``environment`` rejects the
scenario before a live trial is paid for.
"""

from __future__ import annotations

import os
import pathlib
from typing import Sequence

#: Surfaces whose leaks are fatal. ``pre_prompts`` is reported and not fatal:
#: the turn script is allowed to name the target once, before compaction, which
#: is the whole setup.
FATAL_SURFACES = ("tree", "filenames", "post_prompt", "environment")

#: Environment variables that legitimately contain the fixture path and would
#: otherwise trip the scan on every run.
ENV_ALLOW = ("PATH", "PATHEXT", "PSMODULEPATH", "TEMP", "TMP", "TMPDIR")


def _hits(haystack: str, terms: Sequence[str], where: str, label: str) -> list[dict]:
    low = haystack.lower()
    out = []
    for term in terms:
        if term.lower() in low:
            index = low.index(term.lower())
            out.append({
                "surface": where,
                "where": label,
                "term": term,
                "context": haystack[max(0, index - 60):index + len(term) + 60].strip()[:200],
            })
    return out


def scan_tree(repo: pathlib.Path, terms: Sequence[str]) -> list[dict]:
    """Every tracked text file, and every path, searched case-insensitively.

    ``.git`` is skipped for contents: a term appearing only in git history is a
    *planted* dead end, which is the point, not a leak. Filenames are checked
    everywhere, including inside ``.git``, because a branch or a file name is
    read as readily as a docstring.
    """
    out: list[dict] = []
    for path in sorted(repo.rglob("*")):
        rel = str(path.relative_to(repo)).replace("\\", "/")
        if not any(part == ".git" for part in path.parts):
            out.extend(_hits(rel, terms, "filenames", rel))
        if not path.is_file() or ".git" in path.parts:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for number, line in enumerate(text.splitlines(), 1):
            out.extend(_hits(line.strip(), terms, "tree", f"{rel}:{number}"))
    return out


def scan_prompts(turns: Sequence[str], measured_index: int,
                 terms: Sequence[str]) -> list[dict]:
    """The turn script, split into what the agent sees after the boundary and
    what it saw before."""
    out: list[dict] = []
    for index, text in enumerate(turns):
        surface = "post_prompt" if index >= measured_index else "pre_prompts"
        out.extend(_hits(text, terms, surface, f"turn {index}"))
    return out


def scan_environment(terms: Sequence[str], env: dict | None = None) -> list[dict]:
    env = os.environ if env is None else env
    out: list[dict] = []
    for key, value in sorted(env.items()):
        if key.upper() in ENV_ALLOW:
            continue
        out.extend(_hits(f"{key}={value}", terms, "environment", key))
    return out


def scan(repo: pathlib.Path, turns: Sequence[str], measured_index: int,
         terms: Sequence[str], env: dict | None = None) -> dict:
    """Every surface at once. ``clean`` is false when any fatal surface hits."""
    hits = (scan_tree(repo, terms)
            + scan_prompts(turns, measured_index, terms)
            + scan_environment(terms, env))
    fatal = [h for h in hits if h["surface"] in FATAL_SURFACES]
    by_surface: dict[str, int] = {}
    for h in hits:
        by_surface[h["surface"]] = by_surface.get(h["surface"], 0) + 1
    return {
        "clean": not fatal,
        "terms": list(terms),
        "surfaces_scanned": ["tree", "filenames", "pre_prompts", "post_prompt",
                             "environment"],
        "fatal_surfaces": list(FATAL_SURFACES),
        "hits_by_surface": by_surface,
        "fatal_hits": fatal,
        "hits": hits,
    }


def format_report(result: dict) -> str:
    if result["clean"]:
        counts = result["hits_by_surface"]
        note = (f" (non-fatal: {counts})" if counts else "")
        return f"leak scan clean across {len(result['surfaces_scanned'])} surfaces{note}"
    lines = ["leak scan FAILED -- the answer is readable without recall:"]
    for h in result["fatal_hits"]:
        lines.append(f"  [{h['surface']}] {h['where']}: {h['term']!r}")
        lines.append(f"      {h['context']}")
    return "\n".join(lines)
