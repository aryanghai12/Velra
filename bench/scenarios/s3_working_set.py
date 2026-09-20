#!/usr/bin/env python3
"""s3 — the file we agreed on, after eighty-four others.

**The mechanism under test: `[FILE_ACTIVITY]`.** The capsule is supposed to
carry the files the task is actually about, ranked so that a late sweep across
an unrelated part of the codebase cannot evict them. That ranking was one of
the five defects the v0.1 benchmark found and §15 fixed, and it has never been
measured end to end — only in a unit test that replays a sweep.

The scenario is built around an anaphor. Four turns in, the agent reads two
importer modules and the user names one of them as the thing to fix, then
explicitly defers the change. Eight audit turns then read every one of the 84
surrounding modules, which is exactly the eviction pressure the ranking fix
exists to survive. After compaction the measured turn is:

    "Now make that fix."

Nothing in the repository says which module "that" is. The failing-assertion
escape hatch that rescued both arms in the v0.1 benchmark does not exist here,
because there is no failing test pointing anywhere — the suite is green
throughout, and stays green, which is itself a check.

**This scenario is expected to be hard for Velra as it stands.** §18 of the
v0.1 report records `[FILE_ACTIVITY]` absent from 4 of 4 delivered capsules:
the ladder steps from four files to zero with nothing in between. If that
holds, s3 measures a miss rather than a win. That is the point of running it —
an inferred defect becomes a measured one, and the v0.2 `working_max = 2` rung
gets a number to beat.
"""

from __future__ import annotations

import pathlib

from . import base
from .base import (CONFTEST, ENGINE_TRUE_FIX, INIT, MONEY, RULES, TESTS,
                   Scenario, Variant, git, write)

NAME = "s3-working-set"

TARGET = "src/ledger/importers/legacy_tsv_v2.py"
SIBLING = "src/ledger/importers/legacy_tsv_v1.py"

# The three legacy_tsv modules are generated with an explicit tab delimiter.
# v2 is mutated to split on whitespace instead, which contradicts nothing
# written down anywhere: it is simply wrong, and only a human says so.
TAB_DELIMITER = 'DELIMITER = "\\t"'
NO_DELIMITER = "DELIMITER = None"


def build_tree(repo: pathlib.Path) -> dict:
    write(repo / "src" / "ledger" / "__init__.py", INIT)
    write(repo / "src" / "ledger" / "money.py", MONEY)
    write(repo / "src" / "ledger" / "rules.py", RULES)
    # The engine is already correct here: s3 is not about the one-cent defect,
    # and a failing suite would give the agent somewhere else to go.
    write(repo / "src" / "ledger" / "engine.py", ENGINE_TRUE_FIX)
    write(repo / "conftest.py", CONFTEST)
    write(repo / ".gitignore", "__pycache__/\n*.pyc\n.pytest_cache/\n")
    noise_files = base.build_noise(repo)
    write(repo / "tests" / "test_engine.py", TESTS)

    # Mutate v2 so it splits on whitespace. The generated module declares
    # DELIMITER and branches on it; setting it to None is the documented
    # "split on whitespace" path, so the module stays plausible code.
    target = repo / TARGET
    source = target.read_text(encoding="utf-8")
    assert TAB_DELIMITER in source, "generated legacy_tsv_v2 lost its delimiter"
    write(target, source.replace(TAB_DELIMITER, NO_DELIMITER, 1))

    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "Ledger platform: adapters, reporting, importers, validation")

    return {
        "failing_test": None,
        "failing_assertion_line": None,
        "true_fix_file": TARGET,
        "true_fix_symbol": "DELIMITER",
        # What the final tree must show for the task to be done.
        "required_final_state": [{"file": TARGET, "contains": TAB_DELIMITER}],
        # Editing these is collateral damage: the sibling importers are not
        # broken, and changing them is a symptom of guessing.
        "sibling_files": [SIBLING, "src/ledger/importers/legacy_tsv_v3.py"],
        "dead_end_files": [],
        "dead_end_file": None,
        "anaphor": "the module we agreed to fix",
        "uncommitted_files": [],
        "noise_module_count": len(noise_files),
        "suite_must_stay_green": True,
    }


TURNS = [
    "Orientation first. Run `python -m pytest -q` and tell me whether the "
    "suite is green, then give me the top-level layout of src/ledger.",

    "Read src/ledger/money.py and src/ledger/rules.py and summarise how a "
    "promotional discount is computed.",

    f"Read {SIBLING} and {TARGET} and tell me, for each one, how it splits a "
    "record into fields and what its RECORD_KIND is.",

    # The identification. This is the only place the target is named, and it
    # is never written into the repository.
    f"Right. {TARGET} splits on whitespace even though it is a tab-separated "
    "importer, and that is the bug we are going to fix: its DELIMITER should "
    "be a tab, like its siblings. Do not change anything yet — I want the "
    "audit finished first.",

    # --- saturation: eighty-four modules of eviction pressure --------------
    "Read every module under src/ledger/adapters and list, for each one, the "
    "exception classes it defines and the currencies it supports.",

    "Now read every module under src/ledger/reporting and list, for each one, "
    "its COLUMNS tuple and its grouping key.",

    "Now read every module under src/ledger/importers and tell me which ones "
    "use an explicit delimiter and which split on whitespace, with the "
    "RECORD_KIND of each.",

    "Now read every module under src/ledger/validation and list each one's "
    "MAX_LENGTH and the label it validates.",

    "Across all the adapter modules, group them by their DECLINE_STATUS value "
    "and tell me which ones share each status.",

    "List every call site of Money.from_str across the whole codebase, with "
    "the file and the line number for each.",

    "Which modules import ledger.money directly? Give me the complete list, "
    "grouped by package.",

    "Summarise the audit findings so far in a short table.",

    "/compact",

    # Deliberately anaphoric. Resolvable from the conversation, from the
    # capsule, or not at all.
    "Now make that fix.",
]

COMPACT_INDEX = 12
MEASURED_INDEX = 13


def score(final_tree: dict, measured: dict, manifest: dict) -> dict:
    files = final_tree["files"]
    target_text = files.get(TARGET, "")
    done = TAB_DELIMITER in target_text

    edits = [t.replace("\\", "/").lower() for t in measured["edit_targets"]]

    def touched(rel: str) -> bool:
        return any(t.endswith(rel.lower()) for t in edits)

    siblings = [f for f in manifest["sibling_files"] if touched(f)]
    first = (measured.get("first_edit_file") or "").lower()
    return {
        "primary": "hit_the_agreed_file",
        "hit_the_agreed_file": done,
        "first_edit_on_target": first.endswith(TARGET.lower()),
        "siblings_edited": siblings,
        "made_no_edit": measured["edit_count"] == 0,
        # How much work it took to get back to a file it had already read.
        "tool_calls_before_first_edit": measured.get("first_edit_index"),
        "search_calls": measured["buckets"]["search"],
        "read_calls": measured["buckets"]["read"],
        "suite_green": final_tree["pytest_exit"] == 0,
        "success": bool(done and not siblings and final_tree["pytest_exit"] == 0),
    }


SCENARIO = Scenario(
    name=NAME,
    title="The file we agreed on, after eighty-four others",
    # The hypothesis id, held stable against the hash-locked preregistration.
    # The capsule section itself is now rendered as `[FILE_ACTIVITY]`.
    mechanism="[WORKING_FILES]",
    hypothesis=(
        "After compaction and a sweep across 84 unrelated modules, an agent "
        "carrying the capsule resolves an anaphoric reference to a file it "
        "read earlier in fewer tool calls, and more often at all, than an "
        "agent carrying only Claude Code's own summary."),
    build_tree=build_tree,
    turns=TURNS,
    compact_index=COMPACT_INDEX,
    measured_index=MEASURED_INDEX,
    variants=(),
    leak_terms=(
        "splits on whitespace even though", "should be a tab",
        "the bug we are going to fix", "agreed to fix", "TODO(importers)",
    ),
    canary={
        "file": "src/ledger/adapters/adyen_uk.py",
        "const": "API_KEY_PREFIX",
        "value": "AQE1",
        "seed_turn": 4,
    },
    score=score,
    expect_initial="green",
)
