#!/usr/bin/env python3
"""Shared machinery every scenario fixture is built from.

The three scenarios share one codebase — the billing ledger the v0.1 benchmark
already used, plus the 84 generated noise modules that make a session large
enough for compaction to have something to throw away. What differs between
them is the defect planted, the git history, and the turn script.

Two rules are enforced here rather than left to each scenario:

**Ground truth is verified, never asserted.** ``Scenario.build`` runs the real
pytest suite against the fixture as generated, against the scenario's declared
dead ends, and against its declared true fix. A scenario whose dead end
accidentally fixes the bug, or whose "true fix" does not, fails the build
rather than producing a trial that measures nothing. The v0.1 fixture's
one-cent defect was verified this way by hand once; doing it every build is
what stops it rotting.

**The answer is never written in the tree.** ``leak_terms`` names the phrases
that would give the fix away, and ``bench/fixture/leak_lint.py`` fails the
build if any of them appears in a generated file. The v0.1 fixture shipped two
such leaks — a ``TODO`` naming the fix, caught mid-run, and a docstring on the
failing test saying "The discount is a property of the invoice, not of each
line", which was not caught and which at least one recorded replicate quotes
back as its reasoning. A lint is cheaper than another invalidated run.
"""

from __future__ import annotations

import dataclasses
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
from typing import Callable, Sequence

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
REPO_ROOT = BENCH.parent

sys.path.insert(0, str(BENCH / "fixture"))
import noise  # noqa: E402  (path set above)

NL = "\n"


# --------------------------------------------------------------------------
# Filesystem and git
# --------------------------------------------------------------------------


def write(path: pathlib.Path, text: str) -> None:
    """Write with LF endings, never CRLF.

    Claude Code sends the pre-edit file bytes in its hook payload. If the bytes
    on disk are CRLF and the payload is LF, Velra's content-hash revert
    detection silently matches nothing and the whole dead-end mechanism is
    untestable on Windows.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def rmtree_force(path: pathlib.Path) -> None:
    """Remove a tree even when git has left read-only objects behind."""

    def on_error(func, target, _exc):
        os.chmod(target, stat.S_IWRITE)
        func(target)

    if path.exists():
        shutil.rmtree(path, onexc=on_error)


def git(repo: pathlib.Path, *args: str) -> str:
    out = subprocess.run(["git", *args], cwd=repo, check=True,
                         capture_output=True, text=True,
                         encoding="utf-8", errors="replace")
    return out.stdout.strip()


def init_repo(repo: pathlib.Path) -> None:
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.email", "fixture@velra.bench")
    git(repo, "config", "user.name", "Velra Fixture")
    git(repo, "config", "core.autocrlf", "false")
    git(repo, "config", "commit.gpgsign", "false")


def pytest_in(repo: pathlib.Path) -> tuple[int, str]:
    """Run the fixture's own suite. Returns (exit code, combined output)."""
    proc = subprocess.run([sys.executable, "-m", "pytest", "-q"], cwd=str(repo),
                          capture_output=True, text=True,
                          encoding="utf-8", errors="replace")
    return proc.returncode, (proc.stdout or "") + (proc.stderr or "")


def counts(output: str) -> dict:
    """Parse pytest's summary line into {'passed': n, 'failed': n}."""
    out = {"passed": 0, "failed": 0, "errors": 0}
    for line in reversed(output.strip().splitlines()):
        # The summary line looks like "1 failed, 5 passed in 0.11s".
        parts = line.replace(",", " ").split()
        for i, token in enumerate(parts):
            if token in ("passed", "failed") and i and parts[i - 1].isdigit():
                out[token] = int(parts[i - 1])
            if token.startswith("error") and i and parts[i - 1].isdigit():
                out["errors"] = int(parts[i - 1])
        if out["passed"] or out["failed"] or out["errors"]:
            break
    return out


# --------------------------------------------------------------------------
# The shared ledger sources
# --------------------------------------------------------------------------
#
# These are the v0.1 fixture's modules with one change: the docstring on
# ``test_exact_payment_settles_invoice`` no longer states the fix. Everything
# else — the 0.074 rate, the three line amounts, the exactly-one-cent gap —
# is kept byte for byte, because its arithmetic is what makes the rounding
# hypothesis a true dead end and that property was verified experimentally.

MONEY = '''"""Exact monetary arithmetic in integer cents."""

from __future__ import annotations

from decimal import Decimal, ROUND_HALF_UP


class Money:
    """An amount of money, stored as a whole number of cents."""

    __slots__ = ("cents",)

    def __init__(self, cents: int) -> None:
        self.cents = int(cents)

    @classmethod
    def from_str(cls, text: str) -> "Money":
        """Parse a decimal string such as "1425.00" into cents."""
        value = Decimal(text) * 100
        return cls(int(value.to_integral_value(rounding=ROUND_HALF_UP)))

    def scaled(self, rate: Decimal) -> "Money":
        """Multiply by a rate, rounding half up to the nearest whole cent."""
        value = Decimal(self.cents) * rate
        return Money(int(value.to_integral_value(rounding=ROUND_HALF_UP)))

    def __add__(self, other: "Money") -> "Money":
        return Money(self.cents + other.cents)

    def __sub__(self, other: "Money") -> "Money":
        return Money(self.cents - other.cents)

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Money) and other.cents == self.cents

    def __hash__(self) -> int:
        return hash(self.cents)

    def __repr__(self) -> str:
        return "Money({0}.{1:02d})".format(self.cents // 100, abs(self.cents) % 100)


ZERO = Money(0)
'''

RULES = '''"""Fee and discount rules applied at settlement time."""

from __future__ import annotations

from decimal import Decimal

from .money import Money

# Promotional rate for the current billing period.
PROMO_RATE = Decimal("0.074")

# Flat processing fee charged once per invoice.
FLAT_FEE = Money(250)


def discount_for(amount: Money) -> Money:
    """The promotional discount owed on ``amount``, to the nearest cent."""
    return amount.scaled(PROMO_RATE)


def fee_for(amount: Money) -> Money:
    """The processing fee for an invoice of ``amount``."""
    return FLAT_FEE
'''

ENGINE = '''"""Settlement engine: decides whether a payment clears an invoice."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import List

from . import rules
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
        """The sum of every line item, before discount and fee."""
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
    """Apply the discount and fee to ``invoice`` and compare against ``payment``.

    Returns SETTLED when the payment exactly clears the amount due,
    UNDERPAID when it falls short, and OVERPAID when it exceeds it.
    """
    subtotal = invoice.subtotal()

    discount = ZERO
    for item in invoice.items:
        discount = discount + rules.discount_for(item.amount)

    total_due = subtotal - discount + rules.fee_for(subtotal)
    balance = payment - total_due

    if balance.cents < 0:
        status = "UNDERPAID"
    elif balance.cents > 0:
        status = "OVERPAID"
    else:
        status = "SETTLED"

    return SettlementResult(status=status, balance=balance, total_due=total_due)
'''

INIT = '''"""A small billing ledger."""

from .money import Money, ZERO
from .rules import discount_for, fee_for
from .engine import Invoice, LineItem, SettlementResult, settle

__all__ = [
    "Money",
    "ZERO",
    "discount_for",
    "fee_for",
    "Invoice",
    "LineItem",
    "SettlementResult",
    "settle",
]
'''

CONFTEST = '''import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "src"))
'''

# The docstring on the last test is deliberately about the *expectation*, not
# the mechanism. The v0.1 fixture said "The discount is a property of the
# invoice, not of each line", which is the fix in one sentence, sitting in the
# one file every trial reads first.
TESTS = '''"""Settlement engine tests."""

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
    result = settle(invoice, Money.from_str("1324.83"))
    assert result.status == "SETTLED"
'''

# Mid-refactor debris in the working tree. Realistic, and it gives nothing
# away: a late-payment surcharge nothing calls yet.
RULES_DIRTY = RULES.replace(
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

# Dead end 1: banker's rounding instead of half up. Changes none of the three
# discounts (no value in the calculation is a tie), so the failure survives it
# untouched, and it additionally breaks test_discount_rounds_half_up.
MONEY_DEAD_END = (
    MONEY.replace("from decimal import Decimal, ROUND_HALF_UP",
                  "from decimal import Decimal, ROUND_HALF_EVEN")
    .replace("rounding=ROUND_HALF_UP", "rounding=ROUND_HALF_EVEN")
    .replace('"""Multiply by a rate, rounding half up to the nearest whole cent."""',
             '"""Multiply by a rate, rounding half to even to the nearest whole cent."""')
)

# Dead end 2: spelling the rate to four places. Numerically identical, so the
# failure is unchanged — a true dead end that costs a turn to eliminate.
RULES_DEAD_END = RULES.replace('PROMO_RATE = Decimal("0.074")',
                               'PROMO_RATE = Decimal("0.0740")')

# The structural fix: discount the subtotal once instead of each line.
ENGINE_TRUE_FIX = ENGINE.replace(
    "    discount = ZERO\n"
    "    for item in invoice.items:\n"
    "        discount = discount + rules.discount_for(item.amount)\n",
    "    discount = rules.discount_for(subtotal)\n",
)

# The fix that keeps the per-line loop: accumulate unrounded and round once.
# Both fixes make the suite green; only this one honours s2's constraint.
ENGINE_LOOP_PRESERVING_FIX = ENGINE.replace(
    "from dataclasses import dataclass, field",
    "from decimal import Decimal, ROUND_HALF_UP\nfrom dataclasses import dataclass, field",
).replace(
    "    discount = ZERO\n"
    "    for item in invoice.items:\n"
    "        discount = discount + rules.discount_for(item.amount)\n",
    "    raw = Decimal(0)\n"
    "    for item in invoice.items:\n"
    "        raw = raw + Decimal(item.amount.cents) * rules.PROMO_RATE\n"
    "    discount = Money(int(raw.to_integral_value(rounding=ROUND_HALF_UP)))\n",
)


# --------------------------------------------------------------------------
# The scenario contract
# --------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class Variant:
    """One alternative tree state, used to verify ground truth at build time.

    ``files`` maps repo-relative paths to their contents. ``expect`` is the
    pytest outcome the variant must produce: "green" (exit 0) or "red" (exit
    non-zero). ``why`` is printed when the expectation is not met.
    """

    name: str
    files: dict
    expect: str
    why: str


@dataclasses.dataclass(frozen=True)
class Scenario:
    """Everything the harness needs to run one experiment.

    ``turns`` is the complete script. ``compact_index`` is the index of the
    ``/compact`` turn and ``measured_index`` the turn scored afterwards; both
    are indices into ``turns`` so a scenario can place the boundary where its
    own design needs it.
    """

    name: str
    title: str
    mechanism: str
    hypothesis: str
    build_tree: Callable[[pathlib.Path], dict]
    turns: Sequence[str]
    compact_index: int
    measured_index: int
    variants: Sequence[Variant]
    leak_terms: Sequence[str]
    canary: dict
    score: Callable[..., dict]
    # What the suite must do on the fixture as generated. Most scenarios plant
    # a failing test; s3's task is not expressed as one, and its suite staying
    # green throughout is itself a check.
    expect_initial: str = "red"

    def build(self, dest: pathlib.Path, *, verify: bool = True) -> dict:
        """Generate the fixture and return its manifest.

        The manifest is written *outside* the repository. It names the true fix
        symbol, and an agent working in the repo must never be able to read the
        answer out of its own working tree.
        """
        repo = pathlib.Path(dest).resolve()
        rmtree_force(repo)
        repo.mkdir(parents=True)
        init_repo(repo)
        manifest = self.build_tree(repo)
        manifest.update({
            "scenario": self.name,
            "title": self.title,
            "mechanism": self.mechanism,
            "repo": str(repo),
            "leak_terms": list(self.leak_terms),
            "canary": dict(self.canary),
            "compact_turn_index": self.compact_index,
            "measured_turn_index": self.measured_index,
            "turn_count": len(self.turns),
        })
        if verify:
            manifest["ground_truth"] = self.verify(repo)
            manifest["leak_scan"] = lint_tree(repo, self.leak_terms)
            if not manifest["leak_scan"]["clean"]:
                raise SystemExit(
                    f"{self.name}: the fixture names its own answer:\n"
                    + json.dumps(manifest["leak_scan"]["hits"], indent=2))
        write(repo.parent / (repo.name + ".manifest.json"),
              json.dumps(manifest, indent=2) + NL)
        return manifest

    def verify(self, repo: pathlib.Path) -> dict:
        """Run the suite as generated and against every declared variant.

        The tree is restored to its generated state afterwards, so a scenario
        can be verified and then handed straight to a trial.
        """
        saved = {}
        for variant in self.variants:
            for rel in variant.files:
                path = repo / rel
                if rel not in saved:
                    saved[rel] = path.read_text(encoding="utf-8") if path.exists() else None

        results = {}
        code, output = pytest_in(repo)
        observed = "green" if code == 0 else "red"
        results["as_generated"] = {"exit": code, "counts": counts(output),
                                   "expected": self.expect_initial,
                                   "observed": observed,
                                   "tail": output.strip().splitlines()[-1:]}
        if observed != self.expect_initial:
            raise SystemExit(
                f"{self.name}: the fixture's suite is {observed} as generated "
                f"and the scenario declares {self.expect_initial}. "
                + ("There is no defect to measure."
                   if self.expect_initial == "red"
                   else "A failing suite would give the agent somewhere else to go."))

        for variant in self.variants:
            for rel, text in variant.files.items():
                write(repo / rel, text)
            code, output = pytest_in(repo)
            got = "green" if code == 0 else "red"
            results[variant.name] = {"exit": code, "counts": counts(output),
                                     "expected": variant.expect, "observed": got,
                                     "tail": output.strip().splitlines()[-1:]}
            for rel, text in saved.items():
                if text is None:
                    (repo / rel).unlink(missing_ok=True)
                else:
                    write(repo / rel, text)
            if got != variant.expect:
                raise SystemExit(
                    f"{self.name}: variant {variant.name!r} was expected to be "
                    f"{variant.expect} and is {got}. {variant.why}")
        # Anything pytest wrote while probing must not reach the trial.
        rmtree_force(repo / ".pytest_cache")
        for cache in repo.rglob("__pycache__"):
            rmtree_force(cache)
        return results


def lint_tree(repo: pathlib.Path, terms: Sequence[str]) -> dict:
    """Fail-closed scan for the fixture naming its own answer.

    Every tracked text file is searched, case-insensitively, for each term.
    ``.git`` is skipped: a term appearing only in git history is a *planted*
    dead end, which is the point, not a leak.
    """
    hits = []
    lowered = [t.lower() for t in terms]
    for path in sorted(repo.rglob("*")):
        if not path.is_file() or ".git" in path.parts:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for number, line in enumerate(text.splitlines(), 1):
            low = line.lower()
            for term, original in zip(lowered, terms):
                if term in low:
                    hits.append({
                        "file": str(path.relative_to(repo)).replace("\\", "/"),
                        "line": number, "term": original, "text": line.strip()[:160],
                    })
    return {"clean": not hits, "terms": list(terms), "hits": hits}


def build_noise(repo: pathlib.Path) -> list[str]:
    """The 84 surrounding modules that make a session worth compacting."""
    return noise.build(repo / "src" / "ledger", write)
