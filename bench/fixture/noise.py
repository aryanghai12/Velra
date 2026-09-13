#!/usr/bin/env python3
"""Generate the fixture's surrounding codebase.

The three modules that carry the bug are tiny on purpose. A three-file
repository, though, is not a fair test of compaction: a summariser can hold the
whole thing in a paragraph, so nothing is ever lost and there is no amnesia to
measure.

This module surrounds those three files with a plausible billing platform —
adapters, importers, reporting, validation — so that a session which genuinely
works across the codebase accumulates real context. The generated code is
deterministic, imports the real ``money`` module, and deliberately contains no
second site at which the settlement bug could be fixed: nothing here computes a
discount or a settlement total.
"""

from __future__ import annotations

import pathlib

NL = "\n"

GATEWAYS = [
    ("stripe", "Stripe", "sk_live", 402, "card_declined"),
    ("adyen", "Adyen", "AQE1", 422, "refused"),
    ("braintree", "Braintree", "bt_prod", 409, "processor_declined"),
    ("worldpay", "Worldpay", "wp_live", 400, "invalid_request"),
    ("checkout", "CheckoutCom", "ck_live", 403, "risk_blocked"),
    ("mollie", "Mollie", "live_", 410, "expired"),
    ("gocardless", "GoCardless", "live_gc", 429, "rate_limited"),
    ("paypal", "PayPal", "A21AA", 503, "unavailable"),
]

REPORTS = [
    ("aged_receivables", "AgedReceivables", "days_outstanding", "bucket"),
    ("revenue_by_product", "RevenueByProduct", "product_code", "period"),
    ("tax_summary", "TaxSummary", "jurisdiction", "quarter"),
    ("dunning_funnel", "DunningFunnel", "attempt_number", "outcome"),
    ("churn_cohorts", "ChurnCohorts", "cohort_month", "retained"),
    ("settlement_timing", "SettlementTiming", "settled_on", "lag_days"),
]

IMPORTERS = [
    ("bank_csv", "BankCsv", ",", "statement"),
    ("gateway_json", "GatewayJson", None, "payout"),
    ("erp_fixed_width", "ErpFixedWidth", None, "journal"),
    ("legacy_tsv", "LegacyTsv", "\\t", "invoice"),
    ("ledger_ofx", "LedgerOfx", None, "transaction"),
]

VALIDATORS = [
    ("iban", "Iban", "IBAN", 34),
    ("vat_number", "VatNumber", "VAT registration", 14),
    ("postal_address", "PostalAddress", "postal address", 240),
    ("currency_code", "CurrencyCode", "ISO 4217 currency", 3),
    ("invoice_number", "InvoiceNumber", "invoice number", 24),
]


def gateway_module(slug: str, cls: str, prefix: str, code: int, reason: str) -> str:
    return f'''"""{cls} payment gateway adapter.

Translates the platform's internal payment intents into {cls}'s wire format and
back again. The adapter never computes amounts of its own: it receives a
:class:`~ledger.money.Money` and renders it, so that rounding lives in exactly
one place.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Dict, Optional

from ..money import Money

API_KEY_PREFIX = "{prefix}"
DECLINE_STATUS = {code}
DECLINE_REASON = "{reason}"
SUPPORTED_CURRENCIES = ("GBP", "EUR", "USD", "CAD", "AUD")


class {cls}Error(RuntimeError):
    """Raised when {cls} rejects a request outright."""


class {cls}Declined({cls}Error):
    """Raised when {cls} accepts the request but declines the payment."""


@dataclass
class {cls}Charge:
    reference: str
    amount: Money
    currency: str
    captured: bool = False

    def as_payload(self) -> Dict[str, Any]:
        """Render this charge in {cls}'s request format."""
        return {{
            "reference": self.reference,
            "amount_minor": self.amount.cents,
            "currency": self.currency,
            "capture": self.captured,
        }}


class {cls}Adapter:
    """A thin, synchronous {cls} client."""

    def __init__(self, api_key: str, *, timeout: float = 12.0) -> None:
        if not api_key.startswith(API_KEY_PREFIX):
            raise {cls}Error("api key does not look like a {cls} key")
        self.api_key = api_key
        self.timeout = timeout
        self._last_status: Optional[int] = None

    def authorise(self, charge: {cls}Charge) -> str:
        """Authorise a charge and return the gateway's own reference."""
        if charge.currency not in SUPPORTED_CURRENCIES:
            raise {cls}Error("unsupported currency: " + charge.currency)
        if charge.amount.cents <= 0:
            raise {cls}Error("refusing to authorise a non-positive amount")
        self._last_status = 201
        return "{slug}_" + charge.reference

    def capture(self, reference: str, amount: Money) -> Money:
        """Capture a previously authorised charge."""
        if not reference.startswith("{slug}_"):
            raise {cls}Error("not a {cls} reference: " + reference)
        if amount.cents <= 0:
            raise {cls}Error("refusing to capture a non-positive amount")
        self._last_status = 200
        return amount

    def refund(self, reference: str, amount: Money) -> Money:
        """Refund all or part of a captured charge."""
        if amount.cents <= 0:
            raise {cls}Error("refund must be positive")
        self._last_status = 200
        return amount

    def parse_error(self, status: int, body: Dict[str, Any]) -> {cls}Error:
        """Map a {cls} error response onto this module's exceptions."""
        if status == DECLINE_STATUS and body.get("reason") == DECLINE_REASON:
            return {cls}Declined(body.get("message", "declined"))
        return {cls}Error(body.get("message", "unknown {cls} failure"))

    @property
    def last_status(self) -> Optional[int]:
        return self._last_status


def from_config(config: Dict[str, Any]) -> {cls}Adapter:
    """Build a {cls} adapter from a configuration mapping."""
    key = config.get("{slug}_api_key")
    if not key:
        raise {cls}Error("missing {slug}_api_key")
    return {cls}Adapter(key, timeout=float(config.get("timeout", 12.0)))


def minimum_charge(currency: str) -> Money:
    """The smallest amount {cls} will accept in ``currency``."""
    table = {{"GBP": Money.from_str("0.30"), "EUR": Money.from_str("0.50"),
             "USD": Money.from_str("0.50"), "CAD": Money.from_str("0.50"),
             "AUD": Money.from_str("0.50")}}
    if currency not in table:
        raise {cls}Error("unsupported currency: " + currency)
    return table[currency]
'''


def report_module(slug: str, cls: str, group_field: str, period_field: str) -> str:
    return f'''"""The {slug.replace("_", " ")} report.

Pure aggregation over rows the caller has already fetched. Nothing in this
module talks to a database, so the report can be unit tested against literal
rows.
"""

from __future__ import annotations

from collections import OrderedDict
from dataclasses import dataclass, field
from typing import Dict, Iterable, List, Tuple

from ..money import Money, ZERO

COLUMNS = ("{group_field}", "{period_field}", "gross", "net", "count")
DEFAULT_PRECISION = 2
EMPTY_PLACEHOLDER = "-"


@dataclass
class {cls}Row:
    {group_field}: str
    {period_field}: str
    gross: Money = field(default_factory=lambda: ZERO)
    net: Money = field(default_factory=lambda: ZERO)
    count: int = 0

    def merge(self, other: "{cls}Row") -> "{cls}Row":
        """Combine two rows that share a grouping key."""
        return {cls}Row(
            {group_field}=self.{group_field},
            {period_field}=self.{period_field},
            gross=self.gross + other.gross,
            net=self.net + other.net,
            count=self.count + other.count,
        )


@dataclass
class {cls}Report:
    rows: List[{cls}Row] = field(default_factory=list)

    def grouped(self) -> "OrderedDict[Tuple[str, str], {cls}Row]":
        """Fold the rows onto their ({group_field}, {period_field}) key."""
        out: "OrderedDict[Tuple[str, str], {cls}Row]" = OrderedDict()
        for row in self.rows:
            key = (row.{group_field}, row.{period_field})
            out[key] = out[key].merge(row) if key in out else row
        return out

    def totals(self) -> {cls}Row:
        """A single row summing every row in the report."""
        total = {cls}Row({group_field}="TOTAL", {period_field}=EMPTY_PLACEHOLDER)
        for row in self.rows:
            total = total.merge(row)
        total.{group_field} = "TOTAL"
        return total

    def to_table(self) -> List[List[str]]:
        """Render the report as a list of string cells, header included."""
        table = [list(COLUMNS)]
        for (group, period), row in self.grouped().items():
            table.append([group, period, format_money(row.gross),
                          format_money(row.net), str(row.count)])
        return table


def format_money(amount: Money) -> str:
    """Render an amount with a fixed number of decimal places."""
    sign = "-" if amount.cents < 0 else ""
    whole, part = divmod(abs(amount.cents), 100)
    return "{{0}}{{1}}.{{2:02d}}".format(sign, whole, part)


def build(rows: Iterable[Dict[str, object]]) -> {cls}Report:
    """Build the report from raw mappings."""
    parsed: List[{cls}Row] = []
    for raw in rows:
        parsed.append({cls}Row(
            {group_field}=str(raw.get("{group_field}", EMPTY_PLACEHOLDER)),
            {period_field}=str(raw.get("{period_field}", EMPTY_PLACEHOLDER)),
            gross=Money.from_str(str(raw.get("gross", "0.00"))),
            net=Money.from_str(str(raw.get("net", "0.00"))),
            count=int(raw.get("count", 0) or 0),
        ))
    return {cls}Report(rows=parsed)
'''


def importer_module(slug: str, cls: str, delim, record: str) -> str:
    delim_literal = '"%s"' % delim if delim else "None"
    return f'''"""Import {record} records from the {slug.replace("_", " ")} format."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable, Iterator, List, Optional

from ..money import Money

DELIMITER = {delim_literal}
RECORD_KIND = "{record}"
SKIP_PREFIXES = ("#", "//", "REM ")
MAX_FIELD_LENGTH = 512


class {cls}ParseError(ValueError):
    """Raised when a line cannot be understood as a {record} record."""

    def __init__(self, line_number: int, message: str) -> None:
        super().__init__("line {{0}}: {{1}}".format(line_number, message))
        self.line_number = line_number


@dataclass
class {cls}Record:
    line_number: int
    reference: str
    amount: Money
    description: str = ""

    def is_credit(self) -> bool:
        return self.amount.cents > 0

    def is_debit(self) -> bool:
        return self.amount.cents < 0


def _should_skip(line: str) -> bool:
    stripped = line.strip()
    if not stripped:
        return True
    return any(stripped.startswith(prefix) for prefix in SKIP_PREFIXES)


def split_fields(line: str) -> List[str]:
    """Split one raw line into its fields."""
    if DELIMITER is None:
        return [chunk for chunk in line.split() if chunk]
    return [chunk.strip() for chunk in line.split(DELIMITER)]


def parse_line(line_number: int, line: str) -> Optional[{cls}Record]:
    """Parse one line, or return None when the line should be skipped."""
    if _should_skip(line):
        return None
    fields = split_fields(line)
    if len(fields) < 2:
        raise {cls}ParseError(line_number, "expected at least two fields")
    reference = fields[0][:MAX_FIELD_LENGTH]
    try:
        amount = Money.from_str(fields[1])
    except Exception as exc:
        raise {cls}ParseError(line_number, "bad amount: " + str(exc)) from exc
    description = fields[2][:MAX_FIELD_LENGTH] if len(fields) > 2 else ""
    return {cls}Record(line_number=line_number, reference=reference,
                       amount=amount, description=description)


def parse(lines: Iterable[str]) -> Iterator[{cls}Record]:
    """Parse an iterable of lines into {record} records."""
    for number, line in enumerate(lines, start=1):
        record = parse_line(number, line)
        if record is not None:
            yield record


def totals(records: Iterable[{cls}Record]) -> Money:
    """Sum the amounts of every record."""
    total = Money(0)
    for record in records:
        total = total + record.amount
    return total
'''


def validator_module(slug: str, cls: str, label: str, max_len: int) -> str:
    return f'''"""Validate a {label}."""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import List, Optional

MAX_LENGTH = {max_len}
PATTERN = re.compile(r"^[A-Za-z0-9 .,/-]{{1,{max_len}}}$")
NORMALISE = re.compile(r"\\s+")


class {cls}Invalid(ValueError):
    """Raised when a {label} does not validate."""


@dataclass
class {cls}Result:
    value: str
    normalised: str
    problems: List[str]

    @property
    def ok(self) -> bool:
        return not self.problems


def normalise(value: str) -> str:
    """Collapse whitespace and trim a candidate {label}."""
    return NORMALISE.sub(" ", (value or "")).strip()


def check(value: str) -> {cls}Result:
    """Validate ``value`` without raising, reporting every problem found."""
    normalised = normalise(value)
    problems: List[str] = []
    if not normalised:
        problems.append("{label} is empty")
    if len(normalised) > MAX_LENGTH:
        problems.append("{label} is longer than {max_len} characters")
    if normalised and not PATTERN.match(normalised):
        problems.append("{label} contains unsupported characters")
    return {cls}Result(value=value, normalised=normalised, problems=problems)


def validate(value: str) -> str:
    """Validate ``value`` and return it normalised, or raise."""
    result = check(value)
    if not result.ok:
        raise {cls}Invalid("; ".join(result.problems))
    return result.normalised


def first_problem(value: str) -> Optional[str]:
    """The first validation problem with ``value``, or None."""
    result = check(value)
    return result.problems[0] if result.problems else None
'''


# Each family is emitted once per variant below. The point is volume: a
# session that genuinely works across this tree accumulates tens of thousands
# of tokens of real file content, which is the only condition under which
# Claude Code's own compaction has to start throwing detail away.
GATEWAY_REGIONS = ("eu", "uk", "us", "apac")
REPORT_GRAINS = ("daily", "monthly", "quarterly")
IMPORTER_VERSIONS = ("v1", "v2", "v3")
VALIDATOR_LOCALES = ("gb", "de", "fr")


def _camel(text: str) -> str:
    return "".join(part.capitalize() for part in text.replace("-", "_").split("_"))


def build(root: pathlib.Path, write) -> list[str]:
    """Write the surrounding codebase under ``root`` (the ledger package)."""
    written: list[str] = []

    def emit(relative: str, text: str) -> None:
        write(root / relative, text)
        written.append(relative)

    for package, doc in (
        ("adapters", "Payment gateway adapters."),
        ("reporting", "Reporting and aggregation."),
        ("importers", "File importers for external statements."),
        ("validation", "Field-level validators."),
    ):
        emit(f"{package}/__init__.py", f'"""{doc}"""{NL}')

    for slug, cls, prefix, code, reason in GATEWAYS:
        for region in GATEWAY_REGIONS:
            s = f"{slug}_{region}"
            emit(f"adapters/{s}.py",
                 gateway_module(s, cls + _camel(region), prefix, code, reason))
    for slug, cls, group_field, period_field in REPORTS:
        for grain in REPORT_GRAINS:
            s = f"{slug}_{grain}"
            emit(f"reporting/{s}.py",
                 report_module(s, cls + _camel(grain), group_field, period_field))
    for slug, cls, delim, record in IMPORTERS:
        for version in IMPORTER_VERSIONS:
            s = f"{slug}_{version}"
            emit(f"importers/{s}.py",
                 importer_module(s, cls + _camel(version), delim, record))
    for slug, cls, label, max_len in VALIDATORS:
        for locale in VALIDATOR_LOCALES:
            s = f"{slug}_{locale}"
            emit(f"validation/{s}.py",
                 validator_module(s, cls + _camel(locale), label, max_len))

    return written
