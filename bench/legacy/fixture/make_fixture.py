#!/usr/bin/env python3
"""Build the benchmark fixture repository.

A deterministic, self-contained Git repository representing a realistic
mid-refactor state:

  * three interdependent modules (money -> rules -> engine);
  * a pytest suite in which exactly one test fails;
  * the failing assertion pinned to a deterministic line number;
  * a dead-end commit in history that was tried and reverted;
  * uncommitted modifications in the working tree.

The bug is deliberately designed so that the *intuitive* hypothesis is wrong:
the settlement is off by exactly one cent, which smells like a rounding
problem in ``money.Money.scaled``, but the real defect is in
``engine.settle``, which takes the promotional discount per line item (so each
line rounds separately) instead of once on the invoice subtotal.

Numbers: at a rate of 0.074 each line amount ($126, $476, $826) produces a
product whose fractional part is exactly 0.4, so every line rounds *down* and
the three lines together under-discount the invoice by one cent. There is no
tie anywhere in the calculation, which is what makes the rounding hypothesis a
true dead end: switching Money.scaled to ROUND_HALF_EVEN changes none of these
three values, leaves the failure exactly as it was, and additionally breaks
test_discount_rounds_half_up, whose 18.5-cent case is a genuine tie.

Usage:  python make_fixture.py <destination>
"""

from __future__ import annotations

import json
import pathlib
import argparse
import os
import shutil
import stat
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import noise

NL = "\n"


def write(path: pathlib.Path, text: str) -> None:
    """Write with LF endings, never CRLF.

    Claude Code sends the pre-edit file bytes in its hook payload; if the bytes
    on disk are CRLF and the payload is LF, Velra's content-hash revert
    detection silently matches nothing.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def rmtree_force(path: pathlib.Path) -> None:
    """Remove a tree even when Git has left read-only objects behind.

    Git's object store is read-only on Windows, which makes a plain rmtree
    fail; clearing the read-only bit and retrying is the documented fix.
    """
    def on_error(func, target, _exc):
        os.chmod(target, stat.S_IWRITE)
        func(target)

    if path.exists():
        shutil.rmtree(path, onexc=on_error)


def git(repo: pathlib.Path, *args: str) -> str:
    out = subprocess.run(
        ["git", *args],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    return out.stdout.strip()


# --------------------------------------------------------------------------
# Module sources
# --------------------------------------------------------------------------

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

# The uncommitted working-tree edit: unfinished work on a *different* feature.
#
# An earlier version of this fixture added a `discount_for_total()` helper here
# whose TODO said, in as many words, that the engine should be calling it
# instead of summing per line item. That is the answer written down in the
# working tree: both arms found it by running `git diff` and were done in one
# step, which measured nothing except how quickly an agent reads a TODO.
#
# What stays is realistic mid-refactor debris that gives nothing away: a
# late-payment surcharge that no caller uses yet. Finding the real defect now
# requires noticing that discounting three line items rounds three times.
RULES_DIRTY = RULES.replace(
    'def fee_for(amount: Money) -> Money:\n    """The processing fee for an invoice of ``amount``."""\n    return FLAT_FEE\n',
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

# The historical dead end: banker's rounding instead of half-up.
MONEY_DEAD_END = MONEY.replace(
    "from decimal import Decimal, ROUND_HALF_UP",
    "from decimal import Decimal, ROUND_HALF_EVEN",
).replace("rounding=ROUND_HALF_UP", "rounding=ROUND_HALF_EVEN").replace(
    '"""Multiply by a rate, rounding half up to the nearest whole cent."""',
    '"""Multiply by a rate, rounding half to even to the nearest whole cent."""',
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination")
    parser.add_argument(
        "--noise", action="store_true",
        help="surround the three buggy modules with a realistic billing "
             "platform, so that a session working across the codebase "
             "accumulates enough context for compaction to be lossy")
    opts = parser.parse_args()

    repo = pathlib.Path(opts.destination).resolve()
    rmtree_force(repo)
    repo.mkdir(parents=True)

    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.email", "fixture@velra.bench")
    git(repo, "config", "user.name", "Velra Fixture")
    git(repo, "config", "core.autocrlf", "false")
    git(repo, "config", "commit.gpgsign", "false")

    # ---- commit 1: the ledger before the promotion existed ----------------
    write(repo / "src" / "ledger" / "__init__.py", INIT)
    write(repo / "src" / "ledger" / "money.py", MONEY)
    write(repo / "conftest.py", CONFTEST)
    write(repo / ".gitignore", "__pycache__/\n*.pyc\n.pytest_cache/\n")

    # The first commit predates the promotion, so it has neither the rate nor
    # the discount helper. An earlier version of this replacement quoted a
    # docstring this module does not contain, so it matched nothing and the
    # "before the promotion" commit silently shipped `discount_for` anyway --
    # which also meant the next commit, the one whose message says it applies
    # the promotion, did not touch `rules.py` at all.
    rules_v1 = RULES.replace(
        '# Promotional rate for the current billing period.\n'
        'PROMO_RATE = Decimal("0.074")\n\n', ""
    ).replace(
        'def discount_for(amount: Money) -> Money:\n'
        '    """The promotional discount owed on ``amount``, to the nearest cent."""\n'
        '    return amount.scaled(PROMO_RATE)\n\n\n',
        "",
    ).replace("from decimal import Decimal\n\n", "")
    assert "discount_for" not in rules_v1, "commit 1 must predate the promotion"
    engine_v1 = ENGINE.replace(
        "    discount = ZERO\n"
        "    for item in invoice.items:\n"
        "        discount = discount + rules.discount_for(item.amount)\n\n"
        "    total_due = subtotal - discount + rules.fee_for(subtotal)\n",
        "    total_due = subtotal + rules.fee_for(subtotal)\n",
    )
    init_v1 = INIT.replace("    \"discount_for\",\n", "")
    write(repo / "src" / "ledger" / "__init__.py", init_v1.replace(
        "from .rules import discount_for, fee_for", "from .rules import fee_for"))
    write(repo / "src" / "ledger" / "rules.py", rules_v1)
    write(repo / "src" / "ledger" / "engine.py", engine_v1)
    noise_files: list[str] = []
    if opts.noise:
        noise_files = noise.build(repo / "src" / "ledger", write)
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

    # ---- commit 3 + revert: the dead end already tried --------------------
    write(repo / "src" / "ledger" / "money.py", MONEY_DEAD_END)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m",
        "Try banker's rounding in Money.scaled to fix the one-cent gap")
    dead_end_sha = git(repo, "rev-parse", "--short", "HEAD")
    git(repo, "revert", "--no-edit", "HEAD")
    revert_sha = git(repo, "rev-parse", "--short", "HEAD")

    # ---- uncommitted working-tree modifications ---------------------------
    write(repo / "src" / "ledger" / "rules.py", RULES_DIRTY)

    # ---- locate the failing assertion deterministically -------------------
    test_path = repo / "tests" / "test_engine.py"
    lines = test_path.read_text(encoding="utf-8").splitlines()
    failing_line = next(
        i + 1 for i, line in enumerate(lines)
        if 'assert result.status == "SETTLED"' in line
    )

    manifest = {
        "repo": str(repo),
        "failing_test": "tests/test_engine.py::test_exact_payment_settles_invoice",
        "failing_assertion_line": failing_line,
        "failing_assertion_text": lines[failing_line - 1].strip(),
        "true_fix_symbol": "settle",
        "true_fix_file": "src/ledger/engine.py",
        "dead_end_symbol": "scaled",
        "dead_end_file": "src/ledger/money.py",
        "historical_dead_end_commit": dead_end_sha,
        "historical_revert_commit": revert_sha,
        "uncommitted_files": ["src/ledger/rules.py"],
        "noise": bool(opts.noise),
        "noise_module_count": len(noise_files),
        "expected_status": "SETTLED",
        "observed_status": "UNDERPAID",
    }
    # The manifest is deliberately written OUTSIDE the repository: it names the
    # true fix symbol, and an agent working in the repo must never be able to
    # read the answer out of its own working tree.
    write(repo.parent / (repo.name + ".manifest.json"), json.dumps(manifest, indent=2) + NL)

    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
