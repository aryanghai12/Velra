#!/usr/bin/env python3
"""The causal chain, one stage at a time.

The v0.1.1 benchmark could not distinguish "Velra never captured it" from
"Velra captured it and the capsule dropped it" from "the capsule carried it and
the agent ignored it" from "the agent used it and still got the task wrong".
Those need four different fixes and it reported them as one number.

Five stages, evaluated independently, in order. Each one names what it needs
from the trial and says so when it does not have it:

  1 COMPACTION_LOSS   Did native compaction actually lose the target fact?
                      Scenario-level, measured by the control arm with Velra
                      disabled. If the baseline still recalls the fact, the
                      scenario is NOT CAUSALLY TESTABLE and nothing downstream
                      may be credited to Velra -- not a win, not a loss.

  2 CAPTURE           Velra trials only. Was the fact in the ledger *before*
                      the checkpoint? Read from the trial's own SQLite, bounded
                      by `checkpoints.event_watermark`, so a row written after
                      the boundary cannot be mistaken for one written before it.

  3 DELIVERY          Did the rendered capsule carry the fact, reach the
                      session after compaction, exactly once, within budget?
                      Read from the raw stream, never inferred from the
                      database.

  4 ACCEPTANCE        Did the agent treat the block as trustworthy workspace
                      state, ignore it, or reject it as injected?

  5 UTILISATION       Did the preserved state change what the agent did, and
                      was the outcome correct? Correctness first; the tool-call
                      counts are secondary and are never the verdict.

A stage that cannot be evaluated returns ``status: "n/a"`` with a reason and
does not fail the chain. A stage that is evaluated and fails stops it, and
``classification`` names where.
"""

from __future__ import annotations

import json
import pathlib
import re
import sqlite3
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO_ROOT / "bench"))

import behaviour  # noqa: E402

STAGES = ("compaction_loss", "capture", "delivery", "acceptance", "utilisation")

#: The registered ceiling, in real Anthropic tokenizer tokens.
TOKEN_CEILING = 800

FAILURE_CLASS = {
    "compaction_loss": "NOT_CAUSALLY_TESTABLE",
    "capture": "CAPTURE_FAILURE",
    "delivery": "DELIVERY_FAILURE",
    "acceptance": "ACCEPTANCE_FAILURE",
    "utilisation": "UTILISATION_FAILURE",
}


def _ok(status: str) -> bool:
    return status == "pass"


# ---------------------------------------------------------------------------
# Stage 1 -- did compaction lose it?
# ---------------------------------------------------------------------------


def stage_compaction_loss(control: dict | None) -> dict:
    """Read from the scenario's control run; never from a Velra trial.

    ``control`` is the JSON written by ``loss_probe.py``. The rule is
    pre-registered: the fact counts as lost when a majority of valid control
    replicates failed to recall it *and* the context measurably shrank. Both
    halves matter -- a session that did not compact proves nothing, and a
    session that compacted but kept the fact proves the opposite of what the
    scenario needs.
    """
    if not control:
        return {"status": "n/a", "reason": "no control run on disk",
                "blocks_chain": True}
    facts = control.get("target_facts") or {}
    if not facts:
        return {"status": "n/a",
                "reason": "the control did not probe any target fact",
                "blocks_chain": True}
    per_fact = {}
    for fact_id, rows in sorted(facts.items()):
        valid = [r for r in rows if r.get("valid")]
        lost = [r for r in valid if not r["recalled"]]
        per_fact[fact_id] = {
            "replicates": len(rows),
            "valid_replicates": len(valid),
            "lost_replicates": len(lost),
            "recalled_each": [r["recalled"] for r in valid],
            "answers": [r.get("answer", "")[:200] for r in valid],
            # Majority, not unanimity: one lucky recall out of four does not
            # make a fact survivable, and demanding unanimity would let a single
            # flaky reply veto a scenario.
            "lost": bool(valid) and len(lost) * 2 > len(valid),
        }
    all_lost = bool(per_fact) and all(f["lost"] for f in per_fact.values())
    return {
        "status": "pass" if all_lost else "fail",
        "rule": ("a target fact counts as lost when it is not recalled in a "
                 "majority of valid control replicates, and the control's own "
                 "context-reduction gate also holds"),
        "context_reduction_pct_median": control.get("reduction_pct_median"),
        "compaction_is_lossy": control.get("compaction_is_lossy"),
        "per_fact": per_fact,
        "reason": None if all_lost else (
            "at least one target fact survived native compaction, so this "
            "scenario cannot attribute anything to Velra"),
    }


# ---------------------------------------------------------------------------
# Stage 2 -- was it captured, before the boundary?
# ---------------------------------------------------------------------------


def checkpoint_before_compaction(db: sqlite3.Connection) -> dict | None:
    """The checkpoint the PreCompact hook wrote, with its event watermark."""
    row = db.execute(
        'SELECT checkpoint_id, session_id, epoch, created_ms, "trigger", '
        "       event_watermark, partial, capsule_tokens_est, capsule "
        "FROM checkpoints ORDER BY created_ms DESC, rowid DESC LIMIT 1"
    ).fetchone()
    return dict(row) if row else None


def stage_capture(trial: pathlib.Path, arm: str, facts: list[dict]) -> dict:
    """Query the trial's own ledger for each target fact, bounded by the
    checkpoint watermark."""
    if arm != "velra":
        return {"status": "n/a", "reason": "baseline arm has no ledger",
                "blocks_chain": False}
    db_path = trial / "velra.db"
    if not db_path.exists():
        return {"status": "fail", "reason": "no velra.db captured for a Velra trial",
                "per_fact": {}}
    con = sqlite3.connect(f"file:{db_path.as_posix()}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    try:
        checkpoint = checkpoint_before_compaction(con)
        if not checkpoint:
            return {"status": "fail", "reason": "no checkpoint row in the ledger",
                    "per_fact": {}}
        watermark = checkpoint["event_watermark"]
        per_fact = {}
        for fact in facts:
            try:
                rows = [dict(r) for r in con.execute(fact["ledger_sql"], (watermark,))]
                error = None
            except sqlite3.Error as exc:
                rows, error = [], str(exc)
            per_fact[fact["id"]] = {
                "captured": bool(rows),
                "table": fact["ledger_table"],
                "rows": rows[:5],
                "row_count": len(rows),
                "sql_error": error,
                "checkpoint_id": checkpoint["checkpoint_id"],
                "event_watermark": watermark,
            }
    finally:
        con.close()
    captured = bool(per_fact) and all(f["captured"] for f in per_fact.values())
    return {
        "status": "pass" if captured else "fail",
        "checkpoint": {k: checkpoint[k] for k in
                       ("checkpoint_id", "created_ms", "trigger",
                        "event_watermark", "partial", "capsule_tokens_est")},
        "per_fact": per_fact,
        "reason": None if captured else (
            "the target state was not in the ledger at the checkpoint "
            "watermark: this is a CAPTURE failure, not a delivery one"),
    }


# ---------------------------------------------------------------------------
# Stage 3 -- delivery and budget
# ---------------------------------------------------------------------------


#: Hook labels that only ever fire on the far side of a compaction or a resume.
POST_BOUNDARY_HOOKS = ("compact", "resume")


def delivery_events(stream_path: pathlib.Path) -> tuple[list[dict], int | None]:
    """Every hook response carrying a capsule, and where the boundary was.

    Read from the raw capture rather than from `injections`: the claim is about
    what Claude Code received, and a row in the database is not that.

    "After compaction" is not simply "later in the stream than
    `compact_result: success`". Measured on the v0.1.1 captures, the
    `SessionStart:compact` hook response lands one line *before* that status
    message -- the hook is part of how compaction finishes, so the delivery
    precedes the announcement. Ordering alone would score every real delivery as
    pre-boundary. A delivery therefore counts as post-boundary when Claude Code
    itself labels the hook `compact` or `resume`, or when it is later in the
    stream than the boundary marker.
    """
    out = []
    boundary = None
    for index, raw in enumerate(stream_path.read_text(encoding="utf-8").splitlines()):
        if not raw.strip():
            continue
        try:
            obj = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if boundary is None and (obj.get("compact_result") == "success"
                                 or obj.get("subtype") == "compact_boundary"):
            boundary = index
        if obj.get("subtype") != "hook_response":
            continue
        text = obj.get("stdout") or obj.get("output") or ""
        if not any(tag in text for tag in behaviour.TAG_NAMES):
            continue
        try:
            capsule = json.loads(text)["hookSpecificOutput"]["additionalContext"]
        except (json.JSONDecodeError, KeyError, TypeError):
            continue
        out.append({
            "stream_index": index,
            "hook_name": obj.get("hook_name"),
            "hook_event": obj.get("hook_event"),
            "exit_code": obj.get("exit_code"),
            "chars": len(capsule),
            "text": capsule,
        })
    for d in out:
        label = (d["hook_name"] or "").lower()
        d["after_compaction"] = (
            any(h in label for h in POST_BOUNDARY_HOOKS)
            or (boundary is not None and d["stream_index"] > boundary))
    return out, boundary


def stage_delivery(trial: pathlib.Path, arm: str, facts: list[dict]) -> dict:
    if arm != "velra":
        return {"status": "n/a", "reason": "baseline arm delivers no capsule",
                "blocks_chain": False}
    stream = trial / "stream.jsonl"
    if not stream.exists():
        return {"status": "fail", "reason": "no stream captured"}
    deliveries, boundary = delivery_events(stream)
    after = [d for d in deliveries if d["after_compaction"]]

    measured = None
    tm = trial / "token_measurement.json"
    if tm.exists():
        data = json.loads(tm.read_text(encoding="utf-8"))
        injections = data.get("injections") or []
        control = data.get("control") or {}
        measured = {
            "tokens_each": [i["measured_tokens"] for i in injections],
            "worst": max((i["measured_tokens"] for i in injections), default=None),
            "control_delta": control.get("delta"),
            "control_valid": control.get("delta") == 0,
        }

    text = after[0]["text"] if after else None
    from scenarios.targets import TargetFact  # noqa: E402  (late: path set by caller)
    per_fact = {}
    for fact in facts:
        probe = TargetFact(
            id=fact["id"], what=fact["what"], probe=fact["probe"],
            recalled_markers=fact["recalled_markers"],
            ledger_sql=fact["ledger_sql"], ledger_table=fact["ledger_table"],
            capsule_markers=fact["capsule_markers"],
            necessary_because=fact["necessary_because"])
        per_fact[fact["id"]] = probe.in_capsule(text)

    checks = {
        "delivered_after_compaction": bool(after),
        "delivered_exactly_once": len(after) == 1,
        "all_exits_zero": all(d["exit_code"] in (0, None) for d in deliveries),
        "target_facts_present": bool(per_fact) and all(
            f["present"] for f in per_fact.values()),
        # Unmeasured is not a pass: the ceiling is about real tokens.
        "within_token_ceiling": bool(
            measured and measured["worst"] is not None
            and measured["worst"] <= TOKEN_CEILING),
        "token_measurement_valid": bool(measured and measured["control_valid"]),
    }
    failed = sorted(k for k, ok in checks.items() if not ok)
    if text:
        (trial / "stage3_capsule.txt").write_text(text, encoding="utf-8", newline="")
    return {
        "status": "pass" if not failed else "fail",
        "checks": checks,
        "failed_checks": failed,
        "deliveries": [{k: v for k, v in d.items() if k != "text"} for d in deliveries],
        "deliveries_after_compaction": len(after),
        "boundary_stream_index": boundary,
        "token_ceiling": TOKEN_CEILING,
        "measured_tokens": measured,
        "sections": behaviour.canonical_sections(text) if text else [],
        "per_fact": per_fact,
        "capsule_chars": len(text) if text else 0,
        "reason": None if not failed else f"delivery checks failed: {failed}",
    }


# ---------------------------------------------------------------------------
# Stage 4 -- acceptance
# ---------------------------------------------------------------------------

#: Language that shows the agent read the block as real workspace state.
ACCEPTANCE_RE = re.compile(
    r"(workspace\s+state|session\s+ledger|the\s+record\s+(?:shows|says|notes)"
    r"|according\s+to\s+the\s+(?:record|capsule|workspace)"
    r"|previously\s+(?:reverted|tried|attempted)"
    r"|we\s+(?:already\s+)?(?:tried|reverted|eliminated|ruled\s+out))",
    re.I)


def stage_acceptance(trial: pathlib.Path, arm: str, analysis: dict,
                     delivery: dict) -> dict:
    """Accepted, ignored, or rejected -- machine-detected, and separated.

    Only an explicit rejection fails the stage. An agent that uses the state
    without narrating that it did is not a failure of acceptance, and treating
    silence as one would turn a prose-style difference into a verdict.
    """
    if arm != "velra":
        return {"status": "n/a", "verdict": "N/A",
                "reason": "nothing was delivered to accept", "blocks_chain": False}
    if not delivery.get("checks", {}).get("delivered_after_compaction"):
        return {"status": "n/a", "verdict": "N/A",
                "reason": "no capsule reached the session after compaction",
                "blocks_chain": False}

    rejection = analysis.get("prompt_rejection") or {}
    measured = analysis.get("measured_turn") or {}
    prose = measured.get("assistant_text") or ""
    acknowledged = [m.group(0) for m in ACCEPTANCE_RE.finditer(prose)]

    if rejection.get("rejected"):
        verdict = "REJECTED"
    elif acknowledged:
        verdict = "ACKNOWLEDGED"
    else:
        verdict = "SILENT"
    return {
        "status": "fail" if verdict == "REJECTED" else "pass",
        "verdict": verdict,
        "rejection_hits": rejection.get("hits", []),
        "acknowledgement_hits": acknowledged[:5],
        "note": ("SILENT is not a failure: an agent may use the state without "
                 "saying that it did. Only an explicit rejection fails."),
        "reason": None if verdict != "REJECTED" else (
            "the agent treated the block as injected, fabricated or untrusted"),
    }


# ---------------------------------------------------------------------------
# Stage 5 -- utilisation and outcome
# ---------------------------------------------------------------------------


def stage_utilisation(analysis: dict) -> dict:
    """Correctness first. Everything else is reported and none of it decides.

    The registered primary metric is the scenario's own, and `success` requires
    the task to be right. Tool-call counts, re-reads and search volume are
    recorded beside it because they describe *how* the work went, not whether it
    was done: an agent that spends more calls and gets the answer right has done
    better than one that spends fewer and gets it wrong.
    """
    behaviour_score = analysis.get("behaviour")
    if not behaviour_score:
        return {"status": "n/a", "reason": "the measured turn was not scored",
                "blocks_chain": True}
    measured = analysis.get("measured_turn") or {}
    primary = behaviour_score["primary"]
    return {
        "status": "pass" if behaviour_score.get("success") else "fail",
        "primary_metric": primary,
        "primary_value": behaviour_score.get(primary),
        "correct": bool(behaviour_score.get("success")),
        "suite_green": behaviour_score.get("suite_green"),
        "secondary": {
            "tool_calls": measured.get("tool_call_count"),
            "buckets": measured.get("buckets"),
            "reads_before_first_edit": measured.get("reads_before_first_edit"),
            "source_rereads_before_first_edit": measured.get(
                "source_rereads_before_first_edit"),
            "first_edit_file": measured.get("first_edit_file"),
            "first_edit_index": measured.get("first_edit_index"),
            "edit_targets": measured.get("edit_targets"),
        },
        "scenario_detail": {k: v for k, v in behaviour_score.items()
                            if k not in ("primary", "success")},
        "reason": None if behaviour_score.get("success") else (
            f"the task outcome was wrong on the registered metric {primary!r}"),
    }


# ---------------------------------------------------------------------------
# The chain
# ---------------------------------------------------------------------------


def evaluate(trial: pathlib.Path, control: dict | None) -> dict:
    """Every stage for one trial, and where the chain first broke."""
    meta = json.loads((trial / "trial_meta.json").read_text(encoding="utf-8"))
    analysis_path = trial / "analysis.json"
    analysis = json.loads(analysis_path.read_text(encoding="utf-8")) \
        if analysis_path.exists() else {}
    arm = meta["arm"]
    facts = (meta.get("manifest") or {}).get("target_facts") or []

    stages = {}
    stages["compaction_loss"] = stage_compaction_loss(control)
    stages["capture"] = stage_capture(trial, arm, facts)
    stages["delivery"] = stage_delivery(trial, arm, facts)
    stages["acceptance"] = stage_acceptance(trial, arm, analysis,
                                            stages["delivery"])
    stages["utilisation"] = stage_utilisation(analysis)

    first_failed = None
    for name in STAGES:
        stage = stages[name]
        if stage["status"] == "fail":
            first_failed = name
            break
        if stage["status"] == "n/a" and stage.get("blocks_chain"):
            first_failed = name
            break

    if first_failed is None:
        classification = "CHAIN_COMPLETE"
    elif stages[first_failed]["status"] == "n/a":
        classification = "NOT_EVALUABLE"
    else:
        classification = FAILURE_CLASS[first_failed]

    return {
        "trial": trial.name,
        "scenario": meta["scenario"],
        "arm": arm,
        "replicate": meta["replicate"],
        "pair_id": meta.get("pair_id"),
        "fixture_seed": (meta.get("manifest") or {}).get("fixture_seed"),
        "target_facts": [f["id"] for f in facts],
        "stages": stages,
        "first_failed_stage": first_failed,
        "classification": classification,
    }


def report(result: dict) -> str:
    lines = [f"=== {result['trial']} [{result['arm']}] -> {result['classification']}"]
    for name in STAGES:
        stage = result["stages"][name]
        mark = {"pass": "PASS", "fail": "FAIL", "n/a": " -- "}[stage["status"]]
        detail = stage.get("reason") or ""
        lines.append(f"  {mark}  {name}{(': ' + detail) if detail else ''}")
    return "\n".join(lines)


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--trial", required=True)
    ap.add_argument("--control", help="the scenario's loss_probe.py JSON")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    trial = pathlib.Path(args.trial).resolve()
    control = json.loads(pathlib.Path(args.control).read_text(encoding="utf-8")) \
        if args.control and pathlib.Path(args.control).exists() else None
    result = evaluate(trial, control)
    out = pathlib.Path(args.out) if args.out else trial / "stages.json"
    out.write_text(json.dumps(result, indent=2), encoding="utf-8", newline="")
    print(report(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
