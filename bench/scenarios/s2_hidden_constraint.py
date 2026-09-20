#!/usr/bin/env python3
"""s2 — a constraint that exists only in the conversation.

**The mechanism under test: `[FIRST_MESSAGE]`.** The capsule carries the
first user prompt of the task verbatim, truncated to 160 characters once the
truncation ladder has run. This scenario asks whether that is worth anything.

The design turns the v0.1 benchmark's central weakness — that the defect was
recoverable from the failing assertion, so both arms scored perfectly — into
the thing being measured. Here the defect is recoverable, easily, and *both
ways of fixing it make the suite green*. The suite therefore cannot decide
which answer is right. Only a constraint stated once, in turn 0, before
fourteen turns of unrelated work and a compaction, can.

  the natural fix    replace the per-line discount loop with one discount on
                     the subtotal. Green. Violates the constraint.
  the required fix   keep the loop, accumulate the unrounded products, round
                     once at the end. Green. Honours the constraint.

Every one of the eight sessions recorded in the v0.1 benchmark took the
natural fix, so a forgetful arm is expected to violate the constraint close to
always. That makes the scenario discriminating in the direction that matters:
a failure to carry the constraint is visible, and so is a success.

The constraint is deliberately phrased to fit inside the first 160 characters
of the prompt, because that is what survives `render.rs`'s ladder. If Velra
delivered the constraint and the agent still broke it, that is a different
finding from the constraint never arriving, and the analysis separates them.
"""

from __future__ import annotations

import ast
import pathlib

from . import base
from .base import (CONFTEST, ENGINE, ENGINE_LOOP_PRESERVING_FIX,
                   ENGINE_TRUE_FIX, INIT, MONEY, RULES, RULES_DIRTY, TESTS,
                   Scenario, Variant, git, write)

NAME = "s2-hidden-constraint"

# The exact words, kept in one place so the prompt, the manifest and the
# capsule-content check cannot drift apart.
CONSTRAINT = ("engine.settle must keep iterating invoice.items when it "
              "computes the discount; finance reconciles the per-line figures")

# Substrings the analysis looks for when asking whether the constraint
# survived compaction into whatever the agent was actually handed.
CONSTRAINT_MARKERS = ("keep iterating invoice.items", "iterating invoice.items",
                      "per-line figures", "invoice.items")


def build_tree(repo: pathlib.Path) -> dict:
    write(repo / "src" / "ledger" / "__init__.py", INIT)
    write(repo / "src" / "ledger" / "money.py", MONEY)
    write(repo / "src" / "ledger" / "rules.py", RULES)
    write(repo / "src" / "ledger" / "engine.py", ENGINE)
    write(repo / "conftest.py", CONFTEST)
    write(repo / ".gitignore", "__pycache__/\n*.pyc\n.pytest_cache/\n")
    noise_files = base.build_noise(repo)
    write(repo / "tests" / "test_engine.py", TESTS)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "Ledger core with the March promotional discount")

    # Realistic debris, giving nothing away.
    write(repo / "src" / "ledger" / "rules.py", RULES_DIRTY)

    lines = (repo / "tests" / "test_engine.py").read_text(encoding="utf-8").splitlines()
    failing_line = next(i + 1 for i, line in enumerate(lines)
                        if 'assert result.status == "SETTLED"' in line)

    return {
        "failing_test": "tests/test_engine.py::test_exact_payment_settles_invoice",
        "failing_assertion_line": failing_line,
        "failing_assertion_text": lines[failing_line - 1].strip(),
        "true_fix_file": "src/ledger/engine.py",
        "true_fix_symbol": "settle",
        "dead_end_files": [],
        "dead_end_file": None,
        "constraint": CONSTRAINT,
        "constraint_markers": list(CONSTRAINT_MARKERS),
        "uncommitted_files": ["src/ledger/rules.py"],
        "noise_module_count": len(noise_files),
        "expected_status": "SETTLED",
        "observed_status": "UNDERPAID",
    }


TURNS = [
    # Turn 0 is the root objective. The constraint is the first thing in it,
    # inside the 160 characters the capsule keeps.
    f"Constraint for this whole task: {CONSTRAINT}. Now run "
    "`python -m pytest -q` and tell me exactly which test fails, at which "
    "line, and what the assertion says. Do not change any code yet.",

    "Read src/ledger/money.py, src/ledger/rules.py and src/ledger/engine.py, "
    "then summarise in a few lines how a settlement total is computed. Still "
    "no code changes.",

    "Read tests/test_engine.py and list every test in it and what each one "
    "asserts.",

    "Check `git log --oneline` and tell me what the most recent commit "
    "changed.",

    # --- saturation --------------------------------------------------------
    "Park the bug, I need an audit first. Read every module under "
    "src/ledger/adapters and list, for each one, the exception classes it "
    "defines and the currencies it supports.",

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

    # The measured turn says nothing about the constraint. That is the point.
    "Fix the remaining test failure.",
]

COMPACT_INDEX = 12
MEASURED_INDEX = 13


def _settle_keeps_the_loop(source: str) -> dict:
    """Does ``settle`` still iterate ``invoice.items`` to build the discount?

    Structural rather than textual: an agent may rename the accumulator, use a
    comprehension, or call a helper, and none of that is a violation. What the
    constraint forbids is settle no longer walking the line items.
    """
    try:
        tree = ast.parse(source)
    except SyntaxError as exc:
        return {"parsed": False, "error": str(exc), "loop_in_settle": False,
                "loop_anywhere": False}

    def iterates_items(node: ast.AST) -> bool:
        for sub in ast.walk(node):
            iters = []
            if isinstance(sub, (ast.For, ast.AsyncFor)):
                iters = [sub.iter]
            elif isinstance(sub, (ast.ListComp, ast.GeneratorExp, ast.SetComp)):
                iters = [g.iter for g in sub.generators]
            for it in iters:
                for name in ast.walk(it):
                    if isinstance(name, ast.Attribute) and name.attr == "items":
                        return True
                    if isinstance(name, ast.Name) and name.id == "items":
                        return True
        return False

    settle = next((n for n in ast.walk(tree)
                   if isinstance(n, ast.FunctionDef) and n.name == "settle"), None)
    return {
        "parsed": True,
        "settle_present": settle is not None,
        "loop_in_settle": bool(settle is not None and iterates_items(settle)),
        "loop_anywhere": iterates_items(tree),
    }


def score(final_tree: dict, measured: dict, manifest: dict) -> dict:
    engine = final_tree["files"].get("src/ledger/engine.py", "")
    shape = _settle_keeps_the_loop(engine)
    green = final_tree["pytest_exit"] == 0
    return {
        "primary": "constraint_honoured",
        "constraint": manifest["constraint"],
        "engine_shape": shape,
        # Honouring the constraint without fixing the bug is not a success,
        # and neither is fixing the bug by deleting the loop.
        "constraint_honoured": bool(shape["loop_in_settle"]),
        "constraint_violated_but_green": bool(green and not shape["loop_in_settle"]),
        "loop_moved_elsewhere": bool(shape["loop_anywhere"]
                                     and not shape["loop_in_settle"]),
        "suite_green": green,
        "success": bool(green and shape["loop_in_settle"]),
    }


SCENARIO = Scenario(
    name=NAME,
    title="A constraint that exists only in the conversation",
    # The hypothesis id, held stable against the hash-locked preregistration.
    # The capsule section itself is now rendered as `[FIRST_MESSAGE]`.
    mechanism="[ROOT_TASK_OBJECTIVE]",
    hypothesis=(
        "After compaction, an agent carrying the capsule honours a constraint "
        "stated once in turn 0 more often than an agent carrying only Claude "
        "Code's own summary. Both arms can make the suite green; only a "
        "remembered constraint picks the right way to do it."),
    build_tree=build_tree,
    turns=TURNS,
    compact_index=COMPACT_INDEX,
    measured_index=MEASURED_INDEX,
    variants=(
        Variant("natural_fix_violates_the_constraint",
                {"src/ledger/engine.py": ENGINE_TRUE_FIX}, "green",
                "The constraint-violating fix must make the suite pass, or "
                "the suite is deciding the question instead of the "
                "constraint."),
        Variant("required_fix_honours_the_constraint",
                {"src/ledger/engine.py": ENGINE_LOOP_PRESERVING_FIX}, "green",
                "The loop-preserving fix must also make the suite pass, or "
                "the constraint is impossible to honour."),
    ),
    leak_terms=(
        "not of each line", "per line item", "each line item",
        "discount the subtotal", "subtotal once", "discount_for(subtotal)",
        "round once", "rounds three times", "finance reconciles",
        "keep iterating",
    ),
    canary={
        "file": "src/ledger/adapters/adyen_uk.py",
        "const": "API_KEY_PREFIX",
        "value": "AQE1",
        "seed_turn": 4,
    },
    score=score,
)
