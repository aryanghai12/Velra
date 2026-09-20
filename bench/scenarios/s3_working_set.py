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
from .targets import TargetFact

NAME = "s3-working-set"

TARGET = "src/ledger/importers/legacy_tsv_v2.py"
SIBLING = "src/ledger/importers/legacy_tsv_v1.py"

# All three legacy_tsv modules split on whitespace, and all three are wrong to.
#
# The v0.1.1 fixture mutated only v2, which left the answer readable: v2 was the
# single tab-named importer with `DELIMITER = None`, so an agent that had
# forgotten the conversation entirely could grep the importers, find the one odd
# module, and "resolve" the anaphor without recalling anything. That is a
# ceiling effect, and it would have been scored as a success for whichever arm
# happened to grep.
#
# With all three broken the tree offers three indistinguishable candidates. Only
# turn 3 says which one we agreed to fix, and turn 3 is on the far side of the
# compaction boundary. Fixing all three is a perfectly reasonable thing for a
# forgetful agent to do, and it is exactly what the precision half of the metric
# is there to catch: the question is whether the agent resolved the reference,
# not whether it repaired the package.
TARGET_SIBLINGS = (
    "src/ledger/importers/legacy_tsv_v1.py",
    "src/ledger/importers/legacy_tsv_v3.py",
)
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

    # Mutate all three legacy_tsv modules so they split on whitespace. The
    # generated module declares DELIMITER and branches on it; setting it to None
    # is the documented "split on whitespace" path, so each module stays
    # plausible code and none of them stands out.
    for rel in (TARGET, *TARGET_SIBLINGS):
        path = repo / rel
        source = path.read_text(encoding="utf-8")
        assert TAB_DELIMITER in source, f"generated {rel} lost its delimiter"
        write(path, source.replace(TAB_DELIMITER, NO_DELIMITER, 1))

    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "Ledger platform: adapters, reporting, importers, validation")

    return {
        "failing_test": None,
        "failing_assertion_line": None,
        "true_fix_file": TARGET,
        "true_fix_symbol": "DELIMITER",
        # What the final tree must show for the task to be done.
        "required_final_state": [{"file": TARGET, "contains": TAB_DELIMITER}],
        # Editing these is the signature of a guess. They are broken in
        # exactly the same visible way as the target, so an agent that repairs
        # them is repairing the package rather than answering the question it
        # was asked, which is which module *we agreed on*.
        "sibling_files": list(TARGET_SIBLINGS),
        "decoy_count": len(TARGET_SIBLINGS),
        "dead_end_files": [],
        "dead_end_file": None,
        "anaphor": "the module we agreed to fix",
        "uncommitted_files": [],
        "noise_module_count": len(noise_files),
        "suite_must_stay_green": True,
    }


TARGET_FACTS = (
    TargetFact(
        id="s3-agreed-file",
        what=f"{TARGET} is the module turn 3 named as the one to fix.",
        probe="Four turns in, before the audit, we agreed on one specific "
              "module to fix and deferred the change. Which module was it? "
              "Reply with just the file path.",
        recalled_markers=("legacy_tsv_v2",),
        ledger_table="file_stats",
        ledger_sql=(
            "SELECT f.path, f.reads, f.edits, f.first_touch_ms, f.last_touch_ms "
            "FROM file_stats f "
            "WHERE f.path LIKE '%legacy_tsv_v2.py' AND f.reads >= 1 "
            "  AND f.last_touch_ms <= (SELECT COALESCE(MAX(ts_ms), 0) FROM events "
            "                          WHERE id <= ?1)"),
        capsule_markers=("legacy_tsv_v2",),
        necessary_because=(
            "the measured turn is 'Now make that fix', and 'that' is never "
            "written into the repository. Three importers are broken in the "
            "same visible way, so the tree cannot disambiguate them; there is "
            "no failing test pointing anywhere, because the suite is green "
            "throughout. The reference resolves from the conversation, from the "
            "capsule, or not at all."),
    ),
)


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

    # This turn used to hand the answer over: when v2 was the only tab-named
    # importer splitting on whitespace, "which ones split on whitespace" named
    # it. With all three legacy_tsv modules mutated it is what it was meant to
    # be, which is a reading task across thirty modules.
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
    # Precision is part of the primary metric, not a footnote beside it.
    #
    # The three candidates are broken identically, so "repair every importer
    # that splits on whitespace" fixes the target as a side effect without ever
    # resolving the reference. Scoring the target alone would count that as a
    # hit and the scenario would measure thoroughness instead of recall. It is
    # reported separately too, so a run where both arms repair everything is
    # visible as such rather than as a tie.
    resolved = bool(done and not siblings)
    return {
        "primary": "resolved_the_reference",
        "resolved_the_reference": resolved,
        "target_fixed": done,
        "hit_the_agreed_file": done,  # kept: the v0.1.1 artifacts use this name
        "first_edit_on_target": first.endswith(TARGET.lower()),
        "siblings_edited": siblings,
        "repaired_every_candidate": bool(done and len(siblings) == len(
            manifest["sibling_files"])),
        "made_no_edit": measured["edit_count"] == 0,
        # How much work it took to get back to a file it had already read.
        "tool_calls_before_first_edit": measured.get("first_edit_index"),
        "search_calls": measured["buckets"]["search"],
        "read_calls": measured["buckets"]["read"],
        "suite_green": final_tree["pytest_exit"] == 0,
        "success": bool(resolved and final_tree["pytest_exit"] == 0),
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
        "read earlier -- fixing that module and not its two identically broken "
        "siblings -- more often than an agent carrying only Claude Code's own "
        "summary."),
    build_tree=build_tree,
    turns=TURNS,
    compact_index=COMPACT_INDEX,
    measured_index=MEASURED_INDEX,
    variants=(),
    leak_terms=(
        # Phrases that would name the target without recall. `legacy_tsv_v2`
        # itself cannot go on this list: turn 3 has to say it once, and the wide
        # scan reports that as a non-fatal `pre_prompts` hit by design.
        "splits on whitespace even though", "should be a tab",
        "the bug we are going to fix", "agreed to fix", "TODO(importers)",
        "the module we agreed", "v2 is the one", "fix v2",
    ),
    target_facts=TARGET_FACTS,
    canary={
        "file": "src/ledger/adapters/adyen_uk.py",
        "const": "API_KEY_PREFIX",
        "value": "AQE1",
        "seed_turn": 4,
    },
    score=score,
    expect_initial="green",
)
