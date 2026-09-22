#!/usr/bin/env python3
"""The two Token-Burn scenarios, and the realistic repository they run in.

    python bench/tokenburn/scenarios.py --list
    python bench/tokenburn/scenarios.py A_cold_continuation /tmp/fx

Both benchmarks share one fixture: a small payments service with a real test
suite, three genuinely failing tests, and enough surface for a session to spend
fifteen-plus turns on real engineering. What differs between them is the turn
script, which operational state the transition is supposed to carry, and how
the transition happens — a brand-new session for A, ``/clear`` for B.

Why one fixture and not two
---------------------------

Fairness is easier to hold than to argue about. Both arms of both benchmarks
get the same tree, the same tests, the same permissions and the same prompts;
the only intended difference anywhere is whether the destination session
receives the Velra capsule. Reusing the fixture removes one more axis on which
the two benchmarks could accidentally differ.

Where the difficulty actually comes from
----------------------------------------

Not from hiding things. The suite has **three** failing tests, all real, all
unrelated. Nothing in the tree says which one the developer was working on,
which one they had already ruled out, or what they had decided about the fix —
because those are facts about a conversation, not about a repository. A fresh
session can read every file and still not know them, and it can also
legitimately *reconstruct* them by reasoning, which is a baseline success and
is scored as one.

The dead end is never committed
-------------------------------

Both scenarios' abandoned approach is an edit made during the session and
reverted with ``git restore``. It leaves no commit, no branch and no reflog
entry that a destination session can read, which is what makes
``[REVERTED_EDITS]`` the only place it survives. The archived S1 scenario
planted its dead ends as commits; that is readable from ``git log``, and §8 of
the Phase 3 specification rules it out for Benchmark B.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import pathlib
import subprocess
import sys
from typing import Callable, Sequence

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import leakscan  # type: ignore[no-redef]
    import prereg  # type: ignore[no-redef]
else:
    from . import leakscan, prereg

NL = "\n"


# --------------------------------------------------------------------------
# the repository
# --------------------------------------------------------------------------

PYTEST_INI = """[pytest]
testpaths = tests
python_files = test_*.py
"""

README = """# payments

Settlement and retry plumbing for the payments service.

    python -m pytest -q

Three tests are currently red. They are unrelated to each other and are being
worked through one at a time.
"""

IDEMPOTENCY = '''"""Idempotency keys for outbound payment attempts."""

import hashlib
import itertools

_sequence = itertools.count(1)


def make_key(account: str, amount_minor: int) -> str:
    """A key for one outbound request.

    Each call mints a fresh key. Whether a retry is a new request or the same
    one is a question about the caller, not about this function.
    """
    blob = f"{account}:{amount_minor}:{next(_sequence)}".encode("utf-8")
    return hashlib.sha256(blob).hexdigest()[:24]
'''

RETRY_BROKEN = '''"""Outbound send with backoff."""

from . import idempotency


def retry_backoff(attempt: int, base_ms: int = 50) -> int:
    """Delay before ``attempt``, in milliseconds."""
    if attempt <= 0:
        return 0
    return base_ms * (2 ** (attempt - 1))


def send_with_retry(transport, account, amount_minor, attempts=3):
    """Send, retrying transport failures."""
    last = None
    for attempt in range(attempts):
        key = idempotency.make_key(account, amount_minor)
        try:
            return transport.send(account, amount_minor, key)
        except TransportError as exc:
            last = exc
            transport.sleep(retry_backoff(attempt))
    raise last


class TransportError(Exception):
    pass
'''

#: The fix: one key for one logical request, minted before the loop.
RETRY_FIXED = RETRY_BROKEN.replace(
    """    last = None
    for attempt in range(attempts):
        key = idempotency.make_key(account, amount_minor)
""",
    """    last = None
    key = idempotency.make_key(account, amount_minor)
    for attempt in range(attempts):
""")

#: The abandoned approach: memoise the key in a module-level dict. It turns the
#: target test green and is wrong for the reason the session recorded and the
#: tree does not -- the dict is process-wide, so it leaks keys between requests
#: and across threads.
RETRY_DEAD_END = RETRY_BROKEN.replace(
    "        key = idempotency.make_key(account, amount_minor)",
    "        key = _memo.setdefault("
    "(account, amount_minor), "
    "idempotency.make_key(account, amount_minor))"
).replace("from . import idempotency",
          "from . import idempotency" + NL + NL + "_memo = {}")

LEDGER = '''"""Ledger totals in minor units."""


def total_minor(lines):
    """Sum line amounts, already in minor units."""
    return sum(int(line["amount_minor"]) for line in lines)


def apply_fee(total, fee_bps):
    """Apply a fee in basis points, rounding half up at the total."""
    fee = (total * fee_bps + 5000) // 10000
    return total + fee
'''

FEED = '''"""The upstream settlement feed."""

import datetime


def parse_row(row):
    """One feed row into a dict.

    The feed carries both dates. Which one a window is closed on is a policy
    decision and is not made here.
    """
    return {
        "reference": row["reference"],
        "amount_minor": int(row["amount_minor"]),
        "booking_date": datetime.date.fromisoformat(row["booking_date"]),
        "value_date": datetime.date.fromisoformat(row["value_date"]),
    }
'''

RECONCILE_BROKEN = '''"""Close a reconciliation window over feed rows."""


def in_window(row, start, end):
    """Whether a row belongs to the window [start, end]."""
    stamp = row["value_date"]
    return start <= stamp <= end


def close_window(rows, start, end):
    """Total the rows that belong to this window."""
    return sum(r["amount_minor"] for r in rows if in_window(r, start, end))
'''

RECONCILE_FIXED = RECONCILE_BROKEN.replace(
    '    stamp = row["value_date"]', '    stamp = row["booking_date"]')

#: The abandoned approach for B: widen the window by a day on each side. It
#: makes the red test green and silently double-counts the rows that the
#: adjacent window also claims.
RECONCILE_DEAD_END = RECONCILE_BROKEN.replace(
    "    return start <= stamp <= end",
    "    return (start - _SLACK) <= stamp <= (end + _SLACK)"
).replace('"""Close a reconciliation window over feed rows."""',
          '"""Close a reconciliation window over feed rows."""\n\n'
          "import datetime\n\n_SLACK = datetime.timedelta(days=1)")

TEST_RETRY = '''import pytest

from payments import retry


class FakeTransport:
    def __init__(self, fail_times):
        self.fail_times = fail_times
        self.keys = []
        self.slept = []

    def send(self, account, amount_minor, key):
        self.keys.append(key)
        if len(self.keys) <= self.fail_times:
            raise retry.TransportError("upstream refused")
        return {"ok": True, "key": key}

    def sleep(self, ms):
        self.slept.append(ms)


def test_backoff_doubles():
    assert [retry.retry_backoff(i) for i in range(4)] == [0, 50, 100, 200]


def test_send_returns_on_first_success():
    transport = FakeTransport(fail_times=0)
    assert retry.send_with_retry(transport, "acct-1", 1250)["ok"]


def test_retry_preserves_idempotency_key():
    transport = FakeTransport(fail_times=2)
    retry.send_with_retry(transport, "acct-1", 1250)
    assert len(set(transport.keys)) == 1, transport.keys
'''

TEST_LEDGER = '''from payments import ledger


def test_total_minor():
    assert ledger.total_minor([{"amount_minor": 100},
                               {"amount_minor": 250}]) == 350


def test_apply_fee_rounds():
    assert ledger.apply_fee(1000, 150) == 1015


def test_apply_fee_on_empty_total():
    assert ledger.apply_fee(0, 150) == 1
'''

TEST_RECONCILE = '''import datetime

from payments import feed, reconcile

ROWS = [
    {"reference": "a", "amount_minor": 100,
     "booking_date": datetime.date(2026, 3, 31),
     "value_date": datetime.date(2026, 4, 1)},
    {"reference": "b", "amount_minor": 250,
     "booking_date": datetime.date(2026, 4, 1),
     "value_date": datetime.date(2026, 4, 2)},
]


def test_parse_row_keeps_both_dates():
    row = feed.parse_row({"reference": "c", "amount_minor": "7",
                          "booking_date": "2026-04-01",
                          "value_date": "2026-04-02"})
    assert row["booking_date"] != row["value_date"]


def test_march_window_totals():
    total = reconcile.close_window(ROWS, datetime.date(2026, 3, 1),
                                   datetime.date(2026, 3, 31))
    assert total == 100
'''

CONFTEST = '''import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "src"))
'''

#: Every generated source file. ``build`` writes exactly this and nothing else,
#: so the tree is a pure function of this module.
TREE: dict[str, str] = {
    "pytest.ini": PYTEST_INI,
    "README.md": README,
    "src/payments/__init__.py": '"""Payments service."""\n',
    "src/payments/idempotency.py": IDEMPOTENCY,
    "src/payments/retry.py": RETRY_BROKEN,
    "src/payments/ledger.py": LEDGER,
    "src/payments/feed.py": FEED,
    "src/payments/reconcile.py": RECONCILE_BROKEN,
    "tests/conftest.py": CONFTEST,
    "tests/test_retry.py": TEST_RETRY,
    "tests/test_ledger.py": TEST_LEDGER,
    "tests/test_reconcile.py": TEST_RECONCILE,
}

#: What the suite does on the tree as generated, verified rather than asserted.
EXPECTED_INITIAL_FAILURES = 3


# --------------------------------------------------------------------------
# filesystem and git
# --------------------------------------------------------------------------


def write(path: pathlib.Path, text: str) -> None:
    """LF endings, always.

    Claude Code sends pre-edit file bytes in its hook payload. CRLF on disk
    against an LF payload makes Velra's content-hash revert detection match
    nothing, and the whole reverted-edit mechanism becomes untestable on
    Windows without anything failing loudly.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def git(repo: pathlib.Path, *args: str) -> str:
    out = subprocess.run(["git", *args], cwd=repo, check=True,
                         capture_output=True, text=True,
                         encoding="utf-8", errors="replace")
    return out.stdout.strip()


def rmtree_force(path: pathlib.Path) -> None:
    import os
    import shutil
    import stat

    def on_error(func, target, _exc):
        os.chmod(target, stat.S_IWRITE)
        func(target)

    if path.exists():
        shutil.rmtree(path, onexc=on_error)


def pytest_in(repo: pathlib.Path) -> tuple[int, str]:
    proc = subprocess.run([sys.executable, "-m", "pytest", "-q"], cwd=str(repo),
                          capture_output=True, text=True,
                          encoding="utf-8", errors="replace")
    return proc.returncode, (proc.stdout or "") + (proc.stderr or "")


def pytest_one(repo: pathlib.Path, node_id: str) -> tuple[int, str]:
    """Run exactly one test node, so a variant is judged on the test it is
    about and not on the two unrelated reds standing beside it."""
    proc = subprocess.run([sys.executable, "-m", "pytest", "-q", node_id],
                          cwd=str(repo), capture_output=True, text=True,
                          encoding="utf-8", errors="replace")
    return proc.returncode, (proc.stdout or "") + (proc.stderr or "")


def failure_count(output: str) -> int:
    for line in reversed(output.strip().splitlines()):
        parts = line.replace(",", " ").split()
        for index, token in enumerate(parts):
            if token == "failed" and index and parts[index - 1].isdigit():
                return int(parts[index - 1])
    return 0


# --------------------------------------------------------------------------
# the scenario
# --------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class Variant:
    """A tree state whose effect is declared before the build and then verified.

    Both scenarios have the property that makes them worth running: the
    abandoned approach *also* turns the target test green. Judging a variant by
    the suite's exit code would therefore score the dead end as a fix, so a
    variant declares two things instead — what the target test does, and
    whether the manifest's invariant still holds. The true fix passes both; the
    dead end passes the first and fails the second, and that is precisely the
    distinction no file in the repository records.
    """

    name: str
    files: dict
    #: "pass" or "fail", for the scenario's single target test.
    expect_target: str
    #: Whether the manifest's declared invariant holds under this variant.
    expect_invariant: bool


@dataclasses.dataclass(frozen=True)
class Scenario:
    """One benchmark: its turns, its transition, and what must cross it."""

    name: str
    title: str
    transition: str          # "new_session" | "clear"
    question: str
    turns: Sequence[str]
    #: Index of the turn at which the developer leaves the old session.
    transition_index: int
    #: The first turn of the destination session. Identical on both arms.
    continuation_prompt: str
    #: What the destination must end up doing, declaratively.
    manifest_extra: dict
    leak_terms: Sequence[str]
    variants: Sequence[Variant]
    #: The operational state the capsule is supposed to carry, as the strings a
    #: rendered capsule would contain.
    capsule_markers: Sequence[str]
    #: The ladder rungs this scenario is run at, in order.
    ladder: Sequence[int] = dataclasses.field(
        default_factory=lambda: list(prereg.ladder()))

    # -- identity ----------------------------------------------------------

    def turn_script_hash(self) -> str:
        h = hashlib.sha256()
        h.update(self.name.encode("utf-8"))
        for turn in self.turns:
            h.update(b"\x00")
            h.update(turn.encode("utf-8"))
        h.update(b"\x00continuation\x00")
        h.update(self.continuation_prompt.encode("utf-8"))
        return h.hexdigest()[:16]

    def fixture_seed(self) -> str:
        """Identity of the fixture generator, not of one generated tree."""
        h = hashlib.sha256()
        for rel, text in sorted(TREE.items()):
            h.update(rel.encode("utf-8"))
            h.update(text.encode("utf-8"))
        for variant in self.variants:
            h.update(variant.name.encode("utf-8"))
            h.update(variant.expect_target.encode("utf-8"))
            h.update(str(variant.expect_invariant).encode("utf-8"))
            for rel, text in sorted(variant.files.items()):
                h.update(rel.encode("utf-8"))
                h.update(text.encode("utf-8"))
        h.update(self.turn_script_hash().encode("utf-8"))
        return h.hexdigest()[:16]

    # -- building ----------------------------------------------------------

    def build(self, dest: pathlib.Path, *, verify: bool = True) -> dict:
        """Generate the fixture and return its manifest.

        The manifest is written *beside* the repository, never inside it: it
        names the operational state, and a destination session that could read
        it would be reading the answer.
        """
        repo = pathlib.Path(dest).resolve()
        rmtree_force(repo)
        repo.mkdir(parents=True)
        git(repo, "init", "-q", "-b", "main")
        git(repo, "config", "user.email", "fixture@velra.bench")
        git(repo, "config", "user.name", "Velra Fixture")
        git(repo, "config", "core.autocrlf", "false")
        git(repo, "config", "commit.gpgsign", "false")
        for rel, text in sorted(TREE.items()):
            write(repo / rel, text)
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", "payments service")

        manifest = {
            "benchmark": self.name,
            "title": self.title,
            "transition": self.transition,
            "repo": str(repo),
            "turn_count": len(self.turns),
            "transition_index": self.transition_index,
            "continuation_prompt": self.continuation_prompt,
            "turn_script_hash": self.turn_script_hash(),
            "fixture_seed": self.fixture_seed(),
            "leak_terms": list(self.leak_terms),
            "capsule_markers": list(self.capsule_markers),
            "capsule_token_ceiling": prereg.capsule_token_ceiling(),
            "ladder": list(self.ladder),
            **self.manifest_extra,
        }
        if verify:
            manifest["ground_truth"] = self.verify(repo)
            manifest["leak_scan"] = leakscan.scan(
                repo, self.turns, self.continuation_prompt, self.leak_terms)
            if not manifest["leak_scan"]["clean"]:
                raise SystemExit(
                    f"{self.name}: "
                    + leakscan.format_report(manifest["leak_scan"]))
        write(repo.parent / (repo.name + ".manifest.json"),
              json.dumps(manifest, indent=2) + NL)
        return manifest

    def verify(self, repo: pathlib.Path) -> dict:
        """Run the real suite as generated, then every declared variant.

        Three things are checked and none of them is asserted in prose:

        * the tree as generated has exactly three failing tests, and the
          scenario's target test is one of them;
        * each variant does to the target test what it said it would;
        * each variant holds or breaks the manifest's invariant as declared.

        The last is the one that matters. A dead end that quietly stopped
        violating the invariant, or a fix that quietly started, would make the
        scenario measure nothing, and it would do so silently.
        """
        target = self.manifest_extra["target_test"]
        invariant = self.manifest_extra.get("invariant") or {}
        saved = {rel: (repo / rel).read_text(encoding="utf-8")
                 for variant in self.variants for rel in variant.files}
        observed: dict[str, dict] = {}
        try:
            code, output = pytest_in(repo)
            target_code, _ = pytest_one(repo, target)
            observed["as_generated"] = {
                "suite_exit": code,
                "suite_failures": failure_count(output),
                "expected_suite_failures": EXPECTED_INITIAL_FAILURES,
                "target": target,
                "expect_target": "fail",
                "observed_target": "fail" if target_code != 0 else "pass",
            }
            for variant in self.variants:
                for rel, text in variant.files.items():
                    write(repo / rel, text)
                target_code, _ = pytest_one(repo, target)
                held = self._invariant_holds(repo, invariant)
                observed[variant.name] = {
                    "expect_target": variant.expect_target,
                    "observed_target": "pass" if target_code == 0 else "fail",
                    "expect_invariant": variant.expect_invariant,
                    "observed_invariant": held,
                }
                for rel, text in saved.items():
                    write(repo / rel, text)
        finally:
            for rel, text in saved.items():
                write(repo / rel, text)

        problems = []
        first = observed["as_generated"]
        if first["observed_target"] != "fail":
            problems.append("the target test is not failing as generated")
        if first["suite_failures"] != EXPECTED_INITIAL_FAILURES:
            problems.append(
                f"expected {EXPECTED_INITIAL_FAILURES} failing tests as "
                f"generated, got {first['suite_failures']}")
        for variant in self.variants:
            row = observed[variant.name]
            if row["observed_target"] != row["expect_target"]:
                problems.append(
                    f"{variant.name}: target test {row['observed_target']}, "
                    f"declared {row['expect_target']}")
            if row["observed_invariant"] != row["expect_invariant"]:
                problems.append(
                    f"{variant.name}: invariant held={row['observed_invariant']}, "
                    f"declared {row['expect_invariant']}")
        if problems:
            raise SystemExit(f"{self.name}: ground truth does not hold: "
                             + json.dumps(problems, indent=2))
        return observed

    @staticmethod
    def _invariant_holds(repo: pathlib.Path, invariant: dict) -> bool:
        """The manifest's invariant, evaluated against the tree as it stands."""
        rel = invariant.get("must_hold_in_file")
        if not rel:
            return True
        path = repo / rel
        if not path.is_file():
            return False
        text = path.read_text(encoding="utf-8")
        if any(s not in text for s in (invariant.get("must_contain") or [])):
            return False
        return not any(s in text for s in (invariant.get("must_not_contain") or []))


# --------------------------------------------------------------------------
# Benchmark A -- cold continuation
# --------------------------------------------------------------------------

A_TURNS = [
    # 0-3: orientation. Real reading, and the objective stated once, in chat.
    "We're on the payments service. Run the suite and tell me what's red.",
    "Read src/payments/retry.py and tests/test_retry.py. I care about the "
    "idempotency one only — leave the ledger and reconcile failures alone for "
    "now, they're someone else's.",
    "What exactly does the idempotency test assert, and what does the current "
    "code do instead?",
    "Read src/payments/idempotency.py too, so we know what make_key depends on.",
    # 4-7: the constraint, stated once and never written down.
    "Constraint, and this is the part that matters: whatever we do, the key "
    "has to stay a pure function of the request. No process-wide state. We had "
    "an incident last quarter from exactly that.",
    "Search the tree for anything else that calls make_key.",
    "Show me git log for src/payments/ so we know what's been touched.",
    "Run just the retry tests so we get a clean failure to look at.",
    # 8-11: the dead end, tried and reverted without a commit.
    "Try memoising the key in a module-level dict in retry.py and re-run the "
    "retry tests.",
    "That passes, but it's the process-wide state I told you to avoid. Revert "
    "it with git restore and confirm the tree is clean.",
    "Run the whole suite again to confirm we're back to three failures.",
    "Read src/payments/ledger.py and src/payments/reconcile.py so you know "
    "what the other two reds are; we are not fixing them.",
    # 12-15: the load ladder, as real engineering activity.
    "Read logs/build-01.log and summarise what the last CI run complained "
    "about.",
    "Read logs/test-01.log and logs/build-02.log and tell me whether any of it "
    "touches retry.py.",
    "Read logs/test-02.log as well and give me the three lines that matter.",
    # 16-17: the unresolved task, left deliberately open.
    "Right. The next thing to look at is retry_backoff and where the key is "
    "built relative to the loop. Don't change anything yet.",
    "I'm going to leave this session here. Summarise nothing — just stop.",
]

A_CONTINUATION = ("Continue where we left off and finish the task. Do not ask "
                  "me what it was.")

A_SCENARIO = Scenario(
    name="A_cold_continuation",
    title="Cold-continuation token-burn trap",
    transition="new_session",
    question=("Does a large native continuation cost more input than a fresh "
              "session plus a bounded capsule, and does the fresh session "
              "still do the work correctly?"),
    turns=A_TURNS,
    transition_index=len(A_TURNS) - 1,
    continuation_prompt=A_CONTINUATION,
    capsule_markers=["src/payments/retry.py",
                     "test_retry_preserves_idempotency_key",
                     "retry_backoff"],
    leak_terms=[
        # The shape of the fix.
        "outside the loop", "before the loop", "once per request",
        "hoist", "move the key", "compute the key once",
        # The constraint, which was said aloud once and must not be readable.
        "pure function of the request", "no process-wide state",
        "process-wide state", "incident last quarter",
        # The abandoned approach.
        "memoise", "memoize", "module-level dict", "_memo",
        # Which of the three reds is the live one.
        "the active task", "currently working on", "next action",
    ],
    variants=[
        Variant("true_fix", {"src/payments/retry.py": RETRY_FIXED},
                expect_target="pass", expect_invariant=True),
        Variant("dead_end", {"src/payments/retry.py": RETRY_DEAD_END},
                expect_target="pass", expect_invariant=False),
    ],
    manifest_extra={
        "state_recovery": {
            "current_task": ["test_retry_preserves_idempotency_key",
                             "idempotency key", "retry"],
            "active_failure": ["test_retry_preserves_idempotency_key",
                               "idempotency"],
            "relevant_files": ["src/payments/retry.py",
                               "src/payments/idempotency.py"],
            "next_action": ["retry_backoff", "src/payments/retry.py"],
        },
        "first_correct_action": {
            "tools": ["Read", "Edit", "Write", "MultiEdit", "Grep"],
            "target_contains": ["src/payments/retry.py"],
            "must_not_precede": ["src/payments/ledger.py",
                                 "src/payments/reconcile.py"],
        },
        "final_correctness": {
            "target_test_must_pass": True,
            "max_suite_failures": 2,
            "required_final_state": [{
                "file": "src/payments/retry.py",
                "must_not_contain": ["_memo", "setdefault", "global "],
            }],
        },
        "invariant": {
            "must_hold_in_file": "src/payments/retry.py",
            "must_not_contain": ["_memo", "setdefault", "global "],
        },
        "dead_end": {
            "must_not_edit": ["src/payments/ledger.py",
                              "src/payments/reconcile.py"],
            "description": "the module-level memo in retry.py, tried at turn 8 "
                           "and reverted at turn 9 without a commit",
        },
        "target_test": "tests/test_retry.py::test_retry_preserves_idempotency_key",
    },
)


# --------------------------------------------------------------------------
# Benchmark B -- /clear survival
# --------------------------------------------------------------------------

B_TURNS = [
    "Payments again. Run the suite; I want the reconcile failure this time, "
    "not the other two.",
    "Read src/payments/reconcile.py and tests/test_reconcile.py.",
    "Read src/payments/feed.py as well — I want to know what dates a row "
    "actually carries.",
    "What does test_march_window_totals expect, and what does close_window "
    "currently return for it?",
    # The invariant, stated once, in conversation, and nowhere else.
    "Here's the thing you can't get from the code: the upstream feed backdates. "
    "A window has to close on the booking date, never the value date. "
    "Operations confirmed that on the call this morning; it isn't written down "
    "anywhere yet.",
    "Search the tree for every use of value_date.",
    "Run just the reconcile tests.",
    # The dead end, tried and reverted with no commit.
    "Try widening the window by a day on each side in reconcile.py and re-run "
    "the reconcile tests.",
    "Green, but wrong — that double-counts every row the adjacent window also "
    "claims. Revert it with git restore and confirm the tree is clean.",
    "Run the full suite to confirm we're back to three failures.",
    "Read logs/build-01.log and tell me if reconcile appears anywhere in it.",
    "Read logs/test-01.log and logs/build-02.log too.",
    "Read logs/test-02.log and summarise the failures it shows.",
    "Read src/payments/ledger.py and src/payments/retry.py so you know what "
    "the other two reds are. We are not touching them.",
    "Show me git status and git log --oneline -5.",
    "Next step is in_window in reconcile.py. Don't change it yet.",
    "/clear",
]

B_CONTINUATION = "Continue the task and fix the bug."

B_SCENARIO = Scenario(
    name="B_clear_survival",
    title="/clear survival",
    transition="clear",
    question=("After /clear, does the restored operational state let the work "
              "continue, and what does a fresh native session have to spend to "
              "get back to the same point?"),
    turns=B_TURNS,
    transition_index=len(B_TURNS) - 1,
    continuation_prompt=B_CONTINUATION,
    capsule_markers=["src/payments/reconcile.py", "test_march_window_totals",
                     "booking", "in_window"],
    leak_terms=[
        # The invariant. This is the one that must not be anywhere.
        "booking date, never", "never the value date", "close on the booking",
        "backdates", "backdated", "upstream backdate",
        "window has to close on", "booking_date is the one",
        # The abandoned approach.
        "widen the window", "widening the window", "_slack", "slack of a day",
        "timedelta(days=1)", "double-count", "double counts",
        # Which red is live.
        "the active task", "currently working on", "next action",
    ],
    variants=[
        Variant("true_fix", {"src/payments/reconcile.py": RECONCILE_FIXED},
                expect_target="pass", expect_invariant=True),
        Variant("dead_end", {"src/payments/reconcile.py": RECONCILE_DEAD_END},
                expect_target="pass", expect_invariant=False),
    ],
    manifest_extra={
        "state_recovery": {
            "current_task": ["reconcile", "test_march_window_totals",
                             "close_window"],
            "active_failure": ["test_march_window_totals", "reconcile"],
            "relevant_files": ["src/payments/reconcile.py",
                               "src/payments/feed.py"],
            "next_action": ["in_window", "src/payments/reconcile.py"],
        },
        "first_correct_action": {
            "tools": ["Read", "Edit", "Write", "MultiEdit", "Grep"],
            "target_contains": ["src/payments/reconcile.py"],
            "must_not_precede": ["src/payments/retry.py",
                                 "src/payments/ledger.py"],
        },
        "final_correctness": {
            "target_test_must_pass": True,
            "max_suite_failures": 2,
            "required_final_state": [{
                "file": "src/payments/reconcile.py",
                "must_contain": ["booking_date"],
                "must_not_contain": ["_SLACK", "timedelta(days=1)"],
            }],
        },
        "invariant": {
            "must_hold_in_file": "src/payments/reconcile.py",
            "must_contain": ["booking_date"],
            "must_not_contain": ["_SLACK"],
        },
        "dead_end": {
            "must_not_edit": ["tests/test_reconcile.py",
                              "src/payments/retry.py"],
            "description": "the one-day window slack in reconcile.py, tried at "
                           "turn 7 and reverted at turn 8 without a commit",
        },
        "target_test": "tests/test_reconcile.py::test_march_window_totals",
    },
)


SCENARIOS: dict[str, Scenario] = {s.name: s for s in (A_SCENARIO, B_SCENARIO)}
DEFAULT_ORDER = tuple(SCENARIOS)


def get(name: str) -> Scenario:
    try:
        return SCENARIOS[name]
    except KeyError:
        raise SystemExit(
            f"unknown scenario {name!r}; known: {', '.join(SCENARIOS)}") from None


def manifest_for(name: str) -> dict:
    """The declarative half of the manifest, without building a tree.

    The mock adapter needs the scoring declarations and the markers; it has no
    use for a repository. Keeping this separate is what lets the offline
    selftest run without git, pytest or a filesystem fixture.
    """
    scenario = get(name)
    return {
        "benchmark": scenario.name,
        "title": scenario.title,
        "transition": scenario.transition,
        "turn_count": len(scenario.turns),
        "transition_index": scenario.transition_index,
        "continuation_prompt": scenario.continuation_prompt,
        "turn_script_hash": scenario.turn_script_hash(),
        "fixture_seed": scenario.fixture_seed(),
        "leak_terms": list(scenario.leak_terms),
        "capsule_markers": list(scenario.capsule_markers),
        "capsule_token_ceiling": prereg.capsule_token_ceiling(),
        "ladder": list(scenario.ladder),
        **scenario.manifest_extra,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("scenario", nargs="?")
    ap.add_argument("destination", nargs="?")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--no-verify", action="store_true")
    args = ap.parse_args()

    if args.list or not args.scenario:
        for name, scenario in SCENARIOS.items():
            print(f"{name:22} {scenario.transition:12} {scenario.title}")
            print(f"{'':22} turns={len(scenario.turns)} "
                  f"transition@{scenario.transition_index} "
                  f"seed={scenario.fixture_seed()} "
                  f"script={scenario.turn_script_hash()}")
        return 0
    if not args.destination:
        ap.error("destination is required when a scenario is named")
    manifest = get(args.scenario).build(pathlib.Path(args.destination),
                                        verify=not args.no_verify)
    print(json.dumps({k: v for k, v in manifest.items()
                      if k not in ("leak_terms",)}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
