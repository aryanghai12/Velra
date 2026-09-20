#!/usr/bin/env python3
"""s1 — two indistinguishable candidates, one of them already eliminated.

**Why this fixture was rebuilt.** The v0.1.1 run scored `avoided_dead_ends` at
3/3 for the baseline and 4/4 for Velra: both arms avoided the burned ground
every single time, so E1 measured nothing. The reason is visible in the
transcripts. The two dead ends in that fixture — banker's rounding in
`money.py`, and re-spelling `PROMO_RATE` in `rules.py` — were never plausible
places to return to once the failing assertion pointed at the settlement
total. An agent that had forgotten them entirely still would not have gone
back, because nothing invited it to. The metric could only ever come out
level, which is a defect in the fixture, not a result about the capsule.

**What the defect is now.** Two sibling modules aggregate a per-line money
figure, and they are deliberately the same module twice over:

    promos.discount_total(invoice)      surcharges.handling_total(invoice)

Same shape, same loop, same rounding call per line, same docstring skeleton.
Exactly one of them loses a cent to per-line rounding on the invoice under
test, and which one it is cannot be read off the page: it depends on how
`0.074` and `0.0185` happen to land against three specific line amounts. The
figures are chosen so that

  * collapsing `promos.discount_total` to a single rounding on the subtotal
    moves the total by one cent and turns the suite green — the true fix;
  * collapsing `surcharges.handling_total` the same way moves nothing at all,
    because its three per-line roundings already sum to the subtotal's — a
    genuine dead end, and one an agent cannot dismiss by looking.

The protocol burns `surcharges.py` before the compaction boundary: the agent
collapses it, sees the identical failure, and reverts through git. After the
boundary the measured turn is "fix the remaining test failure", and the agent
faces two identical-looking candidates with no way to choose between them
except arithmetic it has already done once and cannot see any more.

That is the discrimination the v0.1.1 fixture lacked. An agent that has kept
the record goes to `promos.py`. An agent that has not has a coin to flip, and
`surcharges.py` is half the coin.

**The mechanism under test: `[REVERTED_EDITS]`.** The capsule records which
*file* was reverted, never which *idea* — Velra derives everything from tool
events and does not retain edit bodies. This fixture is therefore built so
that the file is the whole answer: the two candidates live in two files, and
naming the burned one is sufficient to pick the other. `money.py` is kept as a
second, weaker dead end so the section still has to carry more than one entry.
"""

from __future__ import annotations

import pathlib

from . import base
from .base import (CONFTEST, MONEY, MONEY_DEAD_END, RULES, Scenario, Variant,
                   git, write)
from .targets import TargetFact

NAME = "s1-dead-end-pair"

# --------------------------------------------------------------------------
# The arithmetic this fixture turns on
# --------------------------------------------------------------------------
#
# Line items:  126.00        476.00        826.00     subtotal 1428.00
#
# PROMO_RATE = 0.074
#   per line   932.4→932    3522.4→3522   6112.4→6112   sum 105.66
#   on subtotal                                      10567.2→105.67
#   -> the two disagree by one cent. This is the defect.
#
# HANDLING_RATE = 0.0185
#   per line   233.1→233     880.6→881    1528.1→1528   sum  26.42
#   on subtotal                                       2641.8→ 26.42
#   -> the two agree exactly. Collapsing this loop is a no-op, and the
#      roundings go in both directions (one up, two down) so it cannot be
#      waved away as "obviously exact" on inspection either.
#
# total_due as generated = 1428.00 - 105.66 + 26.42 + 2.50 = 1351.26
# total_due once promos is fixed = 1428.00 - 105.67 + 26.42 + 2.50 = 1351.25
#
# The suite pays exactly 1351.25, so it fails by one cent until, and only
# until, `promos.discount_total` stops rounding per line.

HANDLING_RATE_LINE = 'HANDLING_RATE = Decimal("0.0185")'

# `rules.py` gains a second rate and a second lookup, written as closely as
# possible to the first so that neither reads as the special one.
RULES_S1 = RULES.replace(
    '# Flat processing fee charged once per invoice.\n'
    'FLAT_FEE = Money(250)\n',
    '# Handling surcharge rate for the current billing period.\n'
    f'{HANDLING_RATE_LINE}\n'
    '\n'
    '# Flat processing fee charged once per invoice.\n'
    'FLAT_FEE = Money(250)\n',
).replace(
    'def fee_for(amount: Money) -> Money:\n'
    '    """The processing fee for an invoice of ``amount``."""\n'
    '    return FLAT_FEE\n',
    'def handling_for(amount: Money) -> Money:\n'
    '    """The handling surcharge owed on ``amount``, to the nearest cent."""\n'
    '    return amount.scaled(HANDLING_RATE)\n'
    '\n'
    '\n'
    'def fee_for(amount: Money) -> Money:\n'
    '    """The processing fee for an invoice of ``amount``."""\n'
    '    return FLAT_FEE\n',
)
assert "handling_for" in RULES_S1 and HANDLING_RATE_LINE in RULES_S1, \
    "s1's rules variant did not apply"

# --------------------------------------------------------------------------
# The two candidates, written to be the same module twice
# --------------------------------------------------------------------------
#
# Anything that distinguishes them here — a different helper name for the
# accumulator, a comment on one and not the other, a different docstring
# rhythm — is a hint, and a hint is the thing this fixture exists to withhold.

_AGGREGATOR = '''"""{title} totals for an invoice."""

from __future__ import annotations

from . import rules
from .money import Money, ZERO


def {func}(invoice) -> Money:
    """The {noun} owed on ``invoice``.

    Each line is taken in turn and the results are added together.
    """
    total = ZERO
    for item in invoice.items:
        total = total + rules.{lookup}(item.amount)
    return total
'''

_AGGREGATOR_COLLAPSED = '''"""{title} totals for an invoice."""

from __future__ import annotations

from . import rules
from .money import Money


def {func}(invoice) -> Money:
    """The {noun} owed on ``invoice``.

    The invoice is taken as a whole.
    """
    return rules.{lookup}(invoice.subtotal())
'''

PROMOS = _AGGREGATOR.format(title="Promotional discount", func="discount_total",
                            noun="promotional discount", lookup="discount_for")
SURCHARGES = _AGGREGATOR.format(title="Handling surcharge", func="handling_total",
                                noun="handling surcharge", lookup="handling_for")

# The true fix: one rounding on the subtotal instead of three on the lines.
PROMOS_TRUE_FIX = _AGGREGATOR_COLLAPSED.format(
    title="Promotional discount", func="discount_total",
    noun="promotional discount", lookup="discount_for")

# The dead end: the identical change to the identical shape, which moves
# nothing, because 0.0185 against these three lines rounds to the same total
# either way. The protocol burns this before the boundary.
SURCHARGES_DEAD_END = _AGGREGATOR_COLLAPSED.format(
    title="Handling surcharge", func="handling_total",
    noun="handling surcharge", lookup="handling_for")

ENGINE_S1 = '''"""Settlement engine: decides whether a payment clears an invoice."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import List

from . import promos, rules, surcharges
from .money import Money, ZERO


@dataclass
class LineItem:
    description: str
    amount: Money


@dataclass
class Invoice:
    number: str
    items: List[LineItem] = field(default_factory=list)

    def subtotal(self) -> Money:
        """The sum of every line item, before discount, surcharge and fee."""
        total = ZERO
        for item in self.items:
            total = total + item.amount
        return total


@dataclass
class SettlementResult:
    status: str
    balance: Money
    total_due: Money


def settle(invoice: Invoice, payment: Money) -> SettlementResult:
    """Apply the discount, surcharge and fee, and compare against ``payment``.

    Returns SETTLED when the payment exactly clears the amount due,
    UNDERPAID when it falls short, and OVERPAID when it exceeds it.
    """
    subtotal = invoice.subtotal()

    discount = promos.discount_total(invoice)
    handling = surcharges.handling_total(invoice)

    total_due = subtotal - discount + handling + rules.fee_for(subtotal)
    balance = payment - total_due

    if balance.cents < 0:
        status = "UNDERPAID"
    elif balance.cents > 0:
        status = "OVERPAID"
    else:
        status = "SETTLED"

    return SettlementResult(status=status, balance=balance, total_due=total_due)
'''

INIT_S1 = '''"""A small billing ledger."""

from .money import Money, ZERO
from .rules import discount_for, fee_for, handling_for
from .promos import discount_total
from .surcharges import handling_total
from .engine import Invoice, LineItem, SettlementResult, settle

__all__ = [
    "Money",
    "ZERO",
    "discount_for",
    "fee_for",
    "handling_for",
    "discount_total",
    "handling_total",
    "Invoice",
    "LineItem",
    "SettlementResult",
    "settle",
]
'''

# The suite. Every assertion about the two rates is deliberately symmetric:
# each gets one rounding test, on a value that is a genuine half-cent tie, and
# both land on the same answer. Nothing here says which aggregator is wrong,
# and nothing pins either aggregator's total, which would give it away.
TESTS_S1 = '''"""Settlement engine tests."""

from decimal import Decimal

import pytest

from ledger.engine import Invoice, LineItem, settle
from ledger.money import Money
from ledger import rules


def build_invoice():
    """The invoice from the March promotion, three lines."""
    return Invoice(
        number="INV-3041",
        items=[
            LineItem("Implementation retainer", Money.from_str("126.00")),
            LineItem("Platform licence, annual", Money.from_str("476.00")),
            LineItem("Onboarding and migration", Money.from_str("826.00")),
        ],
    )


def test_subtotal_sums_line_items():
    assert build_invoice().subtotal() == Money.from_str("1428.00")


def test_flat_fee_is_independent_of_amount():
    assert rules.fee_for(Money.from_str("10.00")) == Money(250)
    assert rules.fee_for(Money.from_str("9999.00")) == Money(250)


def test_discount_rounds_half_up():
    """18.5 cents must round away from zero, not to even."""
    assert rules.discount_for(Money(250)) == Money(19)


def test_handling_rounds_half_up():
    """18.5 cents must round away from zero, not to even."""
    assert rules.handling_for(Money(1000)) == Money(19)


def test_underpayment_is_reported():
    invoice = build_invoice()
    result = settle(invoice, Money.from_str("1000.00"))
    assert result.status == "UNDERPAID"


def test_overpayment_is_reported():
    invoice = build_invoice()
    result = settle(invoice, Money.from_str("2000.00"))
    assert result.status == "OVERPAID"


def test_exact_payment_settles_invoice():
    """An exact payment of the promotional total settles the invoice."""
    invoice = build_invoice()
    result = settle(invoice, Money.from_str("1351.25"))
    assert result.status == "SETTLED"
'''

# Mid-refactor debris in the working tree, carried over from the previous
# fixture: realistic, and it gives nothing away.
RULES_S1_DIRTY = RULES_S1.replace(
    'def fee_for(amount: Money) -> Money:\n'
    '    """The processing fee for an invoice of ``amount``."""\n'
    '    return FLAT_FEE\n',
    'def fee_for(amount: Money) -> Money:\n'
    '    """The processing fee for an invoice of ``amount``."""\n'
    '    return FLAT_FEE\n'
    '\n'
    '\n'
    '# Late-payment surcharge. Not wired into settlement yet.\n'
    'LATE_FEE_RATE = Decimal("0.015")\n'
    '\n'
    '\n'
    'def late_fee_for(amount: Money) -> Money:\n'
    '    """Surcharge for an invoice that is past its due date.\n'
    '\n'
    '    TODO(billing): the dunning job should call this; nothing does yet.\n'
    '    """\n'
    '    return amount.scaled(LATE_FEE_RATE)\n',
)
assert RULES_S1_DIRTY != RULES_S1, "s1's working-tree debris did not apply"


def build_tree(repo: pathlib.Path) -> dict:
    # ---- commit 1: the ledger before the promotion existed ---------------
    #
    # The surcharge is already there. Only the promotion is new, so the
    # history offers no reason to look at one aggregator rather than the
    # other -- `git log` is not a back door into the answer.
    rules_v1 = RULES_S1.replace(
        '# Promotional rate for the current billing period.\n'
        'PROMO_RATE = Decimal("0.074")\n\n', ""
    ).replace(
        'def discount_for(amount: Money) -> Money:\n'
        '    """The promotional discount owed on ``amount``, to the nearest cent."""\n'
        '    return amount.scaled(PROMO_RATE)\n\n\n', ""
    )
    assert "discount_for" not in rules_v1, "commit 1 must predate the promotion"
    assert "handling_for" in rules_v1, "the surcharge predates the promotion"

    engine_v1 = ENGINE_S1.replace(
        "from . import promos, rules, surcharges",
        "from . import rules, surcharges",
    ).replace(
        "    discount = promos.discount_total(invoice)\n"
        "    handling = surcharges.handling_total(invoice)\n\n"
        "    total_due = subtotal - discount + handling + rules.fee_for(subtotal)\n",
        "    handling = surcharges.handling_total(invoice)\n\n"
        "    total_due = subtotal + handling + rules.fee_for(subtotal)\n",
    )
    assert "promos" not in engine_v1, "commit 1's engine must not discount"

    init_v1 = INIT_S1.replace(
        "from .rules import discount_for, fee_for, handling_for",
        "from .rules import fee_for, handling_for",
    ).replace("from .promos import discount_total\n", "").replace(
        '    "discount_for",\n', "").replace('    "discount_total",\n', "")

    write(repo / "src" / "ledger" / "__init__.py", init_v1)
    write(repo / "src" / "ledger" / "money.py", MONEY)
    write(repo / "src" / "ledger" / "rules.py", rules_v1)
    write(repo / "src" / "ledger" / "surcharges.py", SURCHARGES)
    write(repo / "src" / "ledger" / "engine.py", engine_v1)
    write(repo / "conftest.py", CONFTEST)
    write(repo / ".gitignore", "__pycache__/\n*.pyc\n.pytest_cache/\n")
    noise_files = base.build_noise(repo)

    tests_v1 = "\n\n".join(
        block for block in TESTS_S1.split("\n\n\n")
        if "test_discount_rounds_half_up" not in block
        and "test_exact_payment_settles_invoice" not in block
    ) + "\n"
    write(repo / "tests" / "test_engine.py", tests_v1)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m",
        "Ledger core: money, handling surcharge, flat fee, settlement engine")

    # ---- commit 2: the promotion lands, half-wired ------------------------
    write(repo / "src" / "ledger" / "__init__.py", INIT_S1)
    write(repo / "src" / "ledger" / "rules.py", RULES_S1)
    write(repo / "src" / "ledger" / "promos.py", PROMOS)
    write(repo / "src" / "ledger" / "engine.py", ENGINE_S1)
    write(repo / "tests" / "test_engine.py", TESTS_S1)
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
    write(repo / "src" / "ledger" / "rules.py", RULES_S1_DIRTY)

    lines = (repo / "tests" / "test_engine.py").read_text(encoding="utf-8").splitlines()
    failing_line = next(i + 1 for i, line in enumerate(lines)
                        if 'assert result.status == "SETTLED"' in line)

    return {
        "failing_test": "tests/test_engine.py::test_exact_payment_settles_invoice",
        "failing_assertion_line": failing_line,
        "failing_assertion_text": lines[failing_line - 1].strip(),
        "true_fix_file": "src/ledger/promos.py",
        "true_fix_symbol": "discount_total",
        # Every file the protocol burns before the boundary. Editing any of
        # them on the measured turn is re-exploration.
        #
        # `surcharges.py` is the one that carries the experiment: it is the
        # true fix's identical twin, so an agent that cannot recall the
        # elimination has no way to rule it out from the source.
        "dead_end_files": ["src/ledger/surcharges.py", "src/ledger/money.py"],
        "dead_end_file": "src/ledger/surcharges.py",
        "dead_end_symbol": "handling_total",
        # The twin the dead end is indistinguishable from, recorded so the
        # analysis can say plainly what the agent was choosing between.
        "twin_files": ["src/ledger/promos.py", "src/ledger/surcharges.py"],
        "historical_dead_end_commit": dead_end_sha,
        "historical_revert_commit": revert_sha,
        "uncommitted_files": ["src/ledger/rules.py"],
        "noise_module_count": len(noise_files),
        "expected_status": "SETTLED",
        "observed_status": "UNDERPAID",
    }


# --------------------------------------------------------------------------
# What compaction has to destroy for this scenario to mean anything
# --------------------------------------------------------------------------

TARGET_FACTS = (
    TargetFact(
        id="s1-burned-twin",
        what="src/ledger/surcharges.py was collapsed and reverted before the "
             "boundary, and collapsing it changed nothing.",
        probe="Earlier in this session we changed one of the two per-line "
              "aggregator modules so it rounded once on the subtotal, saw the "
              "suite fail in exactly the same way, and reverted it with git. "
              "Which module was that? Reply with just the file path.",
        recalled_markers=("surcharges",),
        ledger_table="dead_ends",
        ledger_sql=(
            "SELECT d.id, d.path, d.mechanism, d.command_text, d.resolved_ms, "
            "       e.id AS event_id "
            "FROM dead_ends d "
            "JOIN edits x ON x.id = (SELECT MIN(value) FROM json_each(d.edit_ids)) "
            "JOIN events e ON e.id = x.event_id "
            "WHERE d.path LIKE '%surcharges.py' AND d.reapplied = 0 "
            "  AND e.id <= ?1"),
        capsule_markers=("surcharges.py",),
        necessary_because=(
            "promos.discount_total and surcharges.handling_total are written to "
            "be the same module twice: same loop, same per-line rounding call, "
            "same docstring skeleton. Which of them loses the cent depends on "
            "how 0.074 and 0.0185 happen to land against three specific line "
            "amounts, and nothing in the tree says which. An agent that has lost "
            "the elimination is choosing between two candidates it cannot tell "
            "apart; one that still has it is choosing between one."),
    ),
)


# Turns 0-6 establish the failure and burn both dead ends. Turns 7-14 are a
# real, unrelated audit across the 84 surrounding modules: not padding, and
# large enough that by the boundary the failing test is ten turns old.
#
# Nothing before the boundary ever asks the agent to diagnose the defect, and
# nothing states which aggregator is sound. Turn 4 names the *hypothesis* that
# per-line rounding is the problem, which is true of both modules; turn 5
# records only that collapsing one of them changed nothing.
TURNS = [
    "This repository has a failing test. Run the test suite with "
    "`python -m pytest -q` and tell me exactly which test fails, at which "
    "line, and what the assertion says. Do not change any code yet.",

    "Read src/ledger/money.py, src/ledger/rules.py, src/ledger/promos.py, "
    "src/ledger/surcharges.py and src/ledger/engine.py, then summarise in a "
    "few lines how a settlement total is computed.",

    "The gap is exactly one cent, so let's start with the rounding "
    "hypothesis. Edit src/ledger/money.py so that Money.scaled uses "
    "ROUND_HALF_EVEN instead of ROUND_HALF_UP, then run the test suite again.",

    "That made things worse. Discard that change with "
    "`git restore src/ledger/money.py`, then run the suite again to confirm "
    "we are back to a single failure.",

    # The dead end that carries the experiment. The instruction is phrased as
    # a hypothesis about per-line rounding in general, which is exactly as
    # true of promos.py as it is of surcharges.py.
    "Second idea: one of the per-line aggregators is rounding three times "
    "where it should round once. Try src/ledger/surcharges.py first — edit "
    "handling_total so it calls rules.handling_for on invoice.subtotal() "
    "once instead of accumulating per line, then run the suite again.",

    "Identical failure, so surcharges.py is not where the cent is going. "
    "Discard it with `git restore src/ledger/surcharges.py` and run the "
    "suite once more.",

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
    twin = manifest["dead_end_file"].replace("\\", "/")
    edits = [t.replace("\\", "/").lower() for t in measured["edit_targets"]]

    def touched(rel: str) -> bool:
        return any(t.endswith(rel.lower()) for t in edits)

    first = (measured.get("first_edit_file") or "").lower()
    reexplored = [f for f in dead_files if touched(f)]
    return {
        "primary": "avoided_dead_ends",
        "avoided_dead_ends": not reexplored,
        "dead_end_files_edited": reexplored,
        # Reported separately because this is the one that discriminates: the
        # other dead end is a file no agent had reason to return to, and the
        # v0.1.1 run showed both arms avoiding it without effort.
        "retried_the_twin": touched(twin),
        "first_edit_on_dead_end": any(first.endswith(f.lower()) for f in dead_files),
        "first_edit_on_true_file": first.endswith(manifest["true_fix_file"].lower()),
        "suite_green": final_tree["pytest_exit"] == 0,
        # A run is only a success if it avoided the burned ground *and* landed
        # the fix. Avoiding the dead ends by doing nothing is not a win.
        "success": (not reexplored) and final_tree["pytest_exit"] == 0,
    }


SCENARIO = Scenario(
    name=NAME,
    title="Two indistinguishable candidates, one already eliminated",
    # The hypothesis id, held stable against the hash-locked preregistration.
    # The capsule section itself is now rendered as `[REVERTED_EDITS]`.
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
        Variant("dead_end_surcharges_collapse_the_loop",
                {"src/ledger/surcharges.py": SURCHARGES_DEAD_END}, "red",
                "Collapsing handling_total must leave the failure exactly as "
                "it was. If it changes anything, the two aggregators are no "
                "longer interchangeable from the outside and the measured "
                "turn is a reading exercise rather than a recall one."),
        Variant("true_fix_promos_collapse_the_loop",
                {"src/ledger/promos.py": PROMOS_TRUE_FIX}, "green",
                "Collapsing discount_total must make the whole suite pass, "
                "or the declared true fix is wrong."),
        Variant("both_collapsed_is_still_green",
                {"src/ledger/promos.py": PROMOS_TRUE_FIX,
                 "src/ledger/surcharges.py": SURCHARGES_DEAD_END}, "green",
                "The dead end must be inert, not merely insufficient: "
                "applying it alongside the true fix must not break anything, "
                "or an agent that tries both would be penalised for the "
                "wrong reason."),
    ),
    leak_terms=(
        # Phrases that would hand over which aggregator is the wrong one.
        "not of each line", "per line item", "each line item",
        "discount the subtotal", "subtotal once", "discount_for(subtotal)",
        "round once", "rounds three times",
        "discount_total is", "promos is the", "handling_total is correct",
        "surcharges is correct", "not surcharges", "not promos",
    ),
    target_facts=TARGET_FACTS,
    canary={
        "file": "src/ledger/adapters/adyen_uk.py",
        "const": "API_KEY_PREFIX",
        "value": "AQE1",
        "seed_turn": 7,
    },
    score=score,
)
