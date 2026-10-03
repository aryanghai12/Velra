"""A2 state recovery: a category is complete only when ALL its markers are.

The v0.1.2 qualification review found the analyser scoring a category as
identified on a single marker hit. These tests pin the corrected rule and the
partial-coverage fields that replace it as the place to read "some but not all".
"""

from __future__ import annotations

import pathlib
import sys
from types import SimpleNamespace

BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))

from tokenburn import metrics  # noqa: E402

MARKERS = ["src/payments/retry.py", "test_retry_preserves_idempotency_key",
           "retry_backoff"]


def _parsed(text: str, targets: tuple[str, ...] = ()) -> SimpleNamespace:
    return SimpleNamespace(
        assistant_text=text,
        actions=[SimpleNamespace(kind="tool", tool="Read", target=t)
                 for t in targets])


def test_partial_marker_coverage_is_not_complete():
    got = metrics.category_coverage(
        "I'll open src/payments/retry.py and look at retry_backoff.", MARKERS)
    assert got["identified"] is False
    assert got["markers_found"] == ["src/payments/retry.py", "retry_backoff"]
    assert got["markers_missing"] == ["test_retry_preserves_idempotency_key"]
    assert abs(got["coverage"] - 2 / 3) < 1e-9


def test_full_marker_coverage_is_complete():
    got = metrics.category_coverage(
        "test_retry_preserves_idempotency_key fails; retry_backoff in "
        "src\\payments\\retry.py builds the key inside the loop.", MARKERS)
    assert got["identified"] is True
    assert got["markers_missing"] == []
    assert got["coverage"] == 1.0


def test_state_recovery_complete_requires_every_category_fully_covered():
    manifest = {"state_recovery": {
        "current_task": ["test_march_window_totals", "reconcile"],
        "next_action": ["in_window", "src/payments/reconcile.py"],
    }}
    partial = metrics.state_recovery(
        _parsed("Looking at reconcile now.",
                ("src/payments/reconcile.py",)), manifest)
    assert partial["per_item"]["current_task"]["identified"] is False
    assert partial["per_item"]["next_action"]["identified"] is False
    assert partial["identified_items"] == 0
    assert partial["complete"] is False

    full = metrics.state_recovery(
        _parsed("test_march_window_totals fails in reconcile; in_window is next.",
                ("src/payments/reconcile.py",)), manifest)
    assert full["identified_items"] == 2
    assert full["complete"] is True


def test_undeclared_categories_are_not_scored():
    got = metrics.state_recovery(_parsed("anything"), {"state_recovery": {}})
    assert got["declared_items"] == 0
    assert got["complete"] is False


# --------------------------------------------------------------------------
# Link C: what was measured, and where the loss was
# --------------------------------------------------------------------------

from tokenburn import causal  # noqa: E402
from tokenburn import live_trial  # noqa: E402


def _restore(scan: dict | None) -> SimpleNamespace:
    restore = {"restore_exit": 0, "source_session_id": "s",
               "ledger_evidence": {"measured_on": "staged_capsule",
                                   "markers_present": ["a"],
                                   "markers_missing": ["b"]}}
    if scan is not None:
        restore["ledger_scan"] = scan
    return SimpleNamespace(restore=restore)


def test_link_c_does_not_call_a_selection_loss_a_capture_failure():
    got = causal.link_c(_restore({"available": True,
                                  "markers_present": ["a", "b"],
                                  "markers_missing": []}))
    assert got["status"] == "fail"
    assert "selection or rendering" in got["why"]
    assert "CAPTURE failure" not in got["why"]


def test_link_c_reports_a_real_capture_failure_as_one():
    got = causal.link_c(_restore({"available": True,
                                  "markers_present": ["a"],
                                  "markers_missing": ["b"]}))
    assert got["status"] == "fail"
    assert "CAPTURE failure" in got["why"]


def test_link_c_without_a_ledger_scan_says_it_cannot_tell():
    got = causal.link_c(_restore(None))
    assert got["status"] == "fail"
    assert "cannot be told apart" in got["why"]


def test_ledger_scan_reads_projections_not_raw_events(tmp_path):
    import sqlite3
    db = tmp_path / "velra.db"
    conn = sqlite3.connect(db)
    for table, cols in live_trial.LEDGER_COLUMNS.items():
        conn.execute(f"CREATE TABLE {table} (session_id TEXT, "
                     + ", ".join(f"{c} TEXT" for c in cols) + ")")
    conn.execute("CREATE TABLE events (session_id TEXT, payload TEXT)")
    conn.execute("INSERT INTO intents VALUES ('s', 'look at retry_backoff next')")
    conn.execute("INSERT INTO commands VALUES ('s', 'pytest', ?, NULL)",
                 (r"FAILED tests\test_retry.py::test_key",))
    conn.execute("INSERT INTO events VALUES ('s', 'only_in_events')")
    conn.execute("INSERT INTO intents VALUES ('other', 'only_elsewhere')")
    conn.commit()
    conn.close()
    got = live_trial.ledger_scan(
        db, "s", ["retry_backoff", "tests/test_retry.py::test_key",
                  "only_in_events", "only_elsewhere"])
    assert got["available"] is True
    assert got["markers_present"] == ["retry_backoff",
                                      "tests/test_retry.py::test_key"]
    assert got["markers_missing"] == ["only_in_events", "only_elsewhere"]
    assert live_trial.ledger_scan(None, "s", ["x"])["available"] is False
