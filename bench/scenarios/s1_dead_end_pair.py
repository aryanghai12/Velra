#!/usr/bin/env python3
"""s1 — two eliminated approaches, and a defect that is not written down.

This is the v0.1 fixture with the two flaws that invalidated it removed, plus
the second dead end §14.7 of the v0.1 report asked for.

**What changed and why.** The v0.1 fixture's failing test carried the
docstring *"The discount is a property of the invoice, not of each line"* —
the fix, in one sentence, in the first file every trial reads. The recorded
`saturated-velra-r4` transcript quotes it back ("The bug matches the test's
own hint"), so the measured turn in that run tested reading comprehension, not
recall. It is gone. The v0.1 generator also intended the first commit to
predate the promotion, but its string replacement did not match and silently
did nothing, so `discount_for` was present from the first commit and the
"apply the promotion" commit never touched `rules.py`. The history here is
what it claims to be.

**The mechanism under test: `[DEAD_ENDS]`.** Two plausible hypotheses are
tried and reverted through git before compaction — banker's rounding in
`money.py`, and re-spelling `PROMO_RATE` in `rules.py`. Both leave the failure
exactly as it was. After compaction the agent is asked to fix the remaining
failure. An agent that has forgotten which ground is already burned spends its
first edits back on `money.py` or `rules.py`; an agent carrying the capsule's
`[DEAD_ENDS]` section does not.

The metric is deliberately the one Velra can actually support. Velra records
*which file* was reverted, never *which idea* was tried — it derives everything
from tool events and does not retain edit bodies. So re-exploration is scored
on the file, and the stricter "did the capsule name ROUND_HALF_EVEN" check is
reported alongside as the thing Velra does not claim.
"""

from __future__ import annotations

import pathlib

from . import base
from .base import (ENGINE, ENGINE_TRUE_FIX, INIT, CONFTEST, MONEY,
                   MONEY_DEAD_END, RULES, RULES_DEAD_END, RULES_DIRTY, TESTS,
                   Scenario, Variant, git, write)

NAME = "s1-dead-end-pair"


def build_tree(repo: pathlib.Path) -> dict:
    # ---- commit 1: the ledger before the promotion existed ---------------
    rules_v1 = RULES.replace(
        '# Promotional rate for the current billing period.\n'
        'PROMO_RATE = Decimal("0.074")\n\n', ""
    ).replace(
        'def discount_for(amount: Money) -> Money:\n'
        '    """The promotional discount owed on ``amount``, to the nearest cent."""\n'
        '    return amount.scaled(PROMO_RATE)\n\n\n', ""
    ).replace("from decimal import Decimal\n\n", "")
    assert "discount_for" not in rules_v1, "commit 1 must predate the promotion"
    engine_v1 = ENGINE.replace(
        "    discount = ZERO\n"
        "    for item in invoice.items:\n"
        "        discount = discount + rules.discount_for(item.amount)\n\n"
        "    total_due = subtotal - discount + rules.fee_for(subtotal)\n",
        "    total_due = subtotal + rules.fee_for(subtotal)\n",
    )
    assert engine_v1 != ENGINE, "commit 1's engine must not discount"
    init_v1 = INIT.replace("from .rules import discount_for, fee_for",
                           "from .rules import fee_for").replace(
        '    "discount_for",\n', "")

    write(repo / "src" / "ledger" / "__init__.py", init_v1)
    write(repo / "src" / "ledger" / "money.py", MONEY)
    write(repo / "src" / "ledger" / "rules.py", rules_v1)
    write(repo / "src" / "ledger" / "engine.py", engine_v1)
    write(repo / "conftest.py", CONFTEST)
    write(repo / ".gitignore", "__pycache__/\n*.pyc\n.pytest_cache/\n")
    noise_files = base.build_noise(repo)

    tests_v1 = "\n\n".join(
        block for block in TESTS.split("\n\n\n")
        if "test_discount_rounds_half_up" not in block
        and "test_exact_payment_settles_invoice" not in block
    ) + "\n"
    write(repo / "tests" / "test_engine.py", tests_v1)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "Ledger core: money, flat fee, settlement engine")

    # ---- commit 2: the promotion lands, half-wired ------------------------
    write(repo / "src" / "ledger" / "__init__.py", INIT)
    write(repo / "src" / "ledger" / "rules.py", RULES)
    write(repo / "src" / "ledger" / "engine.py", ENGINE)
    write(repo / "tests" / "test_engine.py", TESTS)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "Apply the March promotional discount at settlement")

    # ---- commit 3 + revert: one dead end already in the history -----------
    write(repo / "src" / "ledger" / "money.py", MONEY_DEAD_END)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m",
        "Try banker's rounding in Money.scaled to fix the one-cent gap")
    dead_end_sha = git(repo, "rev-parse", "--short", "HEAD")
    git(repo, "revert", "--no-edit", "HEAD")
    revert_sha = git(repo, "rev-parse", "--short", "HEAD")

    # ---- uncommitted working-tree debris ----------------------------------
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
        # Every file the protocol burns before the boundary. Editing any of
        # them on the measured turn is re-exploration.
        "dead_end_files": ["src/ledger/money.py", "src/ledger/rules.py"],
        "dead_end_file": "src/ledger/money.py",
        "dead_end_symbol": "scaled",
        "historical_dead_end_commit": dead_end_sha,
        "historical_revert_commit": revert_sha,
        "uncommitted_files": ["src/ledger/rules.py"],
        "noise_module_count": len(noise_files),
        "expected_status": "SETTLED",
        "observed_status": "UNDERPAID",
    }


# Turns 0-5 establish the failure and burn both dead ends. Turns 6-13 are a
# real, unrelated audit across the 84 surrounding modules: not padding, and
# large enough that by the boundary the failing test is eleven turns old.
#
# Nothing before the boundary ever asks the agent to diagnose the defect. The
# v0.1 protocol's "summarise where we stand" turn did, which handed the native
# summariser a ready-made answer; it is not here.
TURNS = [
    "This repository has a failing test. Run the test suite with "
    "`python -m pytest -q` and tell me exactly which test fails, at which "
    "line, and what the assertion says. Do not change any code yet.",

    "Read src/ledger/money.py, src/ledger/rules.py and src/ledger/engine.py, "
    "then summarise in a few lines how a settlement total is computed.",

    "The gap is exactly one cent, so let's start with the rounding "
    "hypothesis. Edit src/ledger/money.py so that Money.scaled uses "
    "ROUND_HALF_EVEN instead of ROUND_HALF_UP, then run the test suite again.",

    "That made things worse. Discard that change with "
    "`git restore src/ledger/money.py`, then run the suite again to confirm "
    "we are back to a single failure.",

    "Second idea: the rate may be losing precision. Edit src/ledger/rules.py "
    'so that PROMO_RATE is Decimal("0.0740") instead of Decimal("0.074"), '
    "then run the suite again.",

    "No change, so that is not it either. Discard it with "
    "`git restore src/ledger/rules.py` and run the suite once more.",

    "Read tests/test_engine.py and list every test in it and what each one "
    "asserts.",

    # --- saturation: real work, across the whole codebase ------------------
    "Park the bug for a moment, I need an audit first. Read every module "
    "under src/ledger/adapters and list, for each one, the exception classes "
    "it defines and the currencies it supports.",

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

    "Fix the remaining test failure.",
]

COMPACT_INDEX = 15
MEASURED_INDEX = 16


def score(final_tree: dict, measured: dict, manifest: dict) -> dict:
    """Behavioural outcome for s1.

    ``final_tree`` is the post-trial repository state collected by the trial
    driver; ``measured`` is the analysed measured turn.
    """
    dead_files = [f.replace("\\", "/") for f in manifest["dead_end_files"]]
    edits = [t.replace("\\", "/").lower() for t in measured["edit_targets"]]

    def touched(rel: str) -> bool:
        return any(t.endswith(rel.lower()) for t in edits)

    first = (measured.get("first_edit_file") or "").lower()
    reexplored = [f for f in dead_files if touched(f)]
    return {
        "primary": "avoided_dead_ends",
        "avoided_dead_ends": not reexplored,
        "dead_end_files_edited": reexplored,
        "first_edit_on_dead_end": any(first.endswith(f.lower()) for f in dead_files),
        "first_edit_on_true_file": first.endswith(manifest["true_fix_file"].lower()),
        "suite_green": final_tree["pytest_exit"] == 0,
        # A run is only a success if it avoided the burned ground *and* landed
        # the fix. Avoiding the dead ends by doing nothing is not a win.
        "success": (not reexplored) and final_tree["pytest_exit"] == 0,
    }


SCENARIO = Scenario(
    name=NAME,
    title="Two eliminated approaches, and a defect that is not written down",
    mechanism="[DEAD_ENDS]",
    hypothesis=(
        "After compaction, an agent carrying the capsule re-edits a file "
        "already tried and reverted less often than an agent carrying only "
        "Claude Code's own summary."),
    build_tree=build_tree,
    turns=TURNS,
    compact_index=COMPACT_INDEX,
    measured_index=MEASURED_INDEX,
    variants=(
        Variant("dead_end_money_round_half_even",
                {"src/ledger/money.py": MONEY_DEAD_END}, "red",
                "Banker's rounding must not fix the one-cent gap, or the "
                "protocol's first dead end is not a dead end."),
        Variant("dead_end_rules_rate_precision",
                {"src/ledger/rules.py": RULES_DEAD_END}, "red",
                "Re-spelling PROMO_RATE is numerically identical, so the "
                "failure must survive it unchanged."),
        Variant("true_fix_discount_the_subtotal",
                {"src/ledger/engine.py": ENGINE_TRUE_FIX}, "green",
                "Discounting the subtotal once must make the whole suite "
                "pass, or the declared true fix is wrong."),
    ),
    leak_terms=(
        "not of each line", "per line item", "each line item",
        "discount the subtotal", "subtotal once", "discount_for(subtotal)",
        "round once", "rounds three times",
    ),
    canary={
        "file": "src/ledger/adapters/adyen_uk.py",
        "const": "API_KEY_PREFIX",
        "value": "AQE1",
        # The turn that puts the canary in context, replayed by the loss probe.
        "seed_turn": 7,
    },
    score=score,
)
