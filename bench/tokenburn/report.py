#!/usr/bin/env python3
"""The report, with every number still attached to where it came from.

§17 of the specification fixes the columns a trial row must expose. They are
listed once, in :data:`TRIAL_COLUMNS`, and the row builder below is the only
thing that fills them, so a column cannot quietly disappear from the report by
being forgotten at one call site.

Three rules the formatter enforces rather than documents:

* a value that was not measured prints as its status word — ``unavailable`` or
  ``inconclusive`` — and never as ``0``, ``-`` or a blank cell;
* a proxy prints with a ``~`` and is listed in the provenance table with what
  it stands in for;
* nothing is averaged across benchmarks, because Benchmark A's headline is
  input burden and Benchmark B's is effort, and their medians are not the same
  quantity.
"""

from __future__ import annotations

import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import telemetry  # type: ignore[no-redef]
    import verdict as verdict_mod  # type: ignore[no-redef]
else:
    from . import telemetry
    from . import verdict as verdict_mod

#: Exactly the columns §17 requires, in that order.
TRIAL_COLUMNS = (
    "scenario", "pair_id", "arm", "source_session", "destination_session",
    "target_context_size", "achieved_context_size", "cache_condition",
    "input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens",
    "output_tokens", "capsule_tokens", "turns", "tool_calls", "file_reads",
    "first_correct_action", "final_correctness", "causal_validity",
    "telemetry_source", "measurement_status", "verdict",
)


def _cell(metrics: dict, name: str):
    """A metric's value, or its status word when there is no value."""
    raw = metrics.get(name)
    if raw is None:
        return telemetry.STATUS_UNAVAILABLE
    if raw.get("value") is None:
        return raw.get("measurement_status") or telemetry.STATUS_UNAVAILABLE
    if raw.get("measurement_status") == telemetry.STATUS_PROXY:
        return f"~{raw['value']}"
    return raw["value"]


def trial_row(evaluation: dict, chain: dict, verdict: str | None = None) -> dict:
    """One trial, as §17 asks for it."""
    m = evaluation.get("metrics") or {}
    first = evaluation.get("first_correct_action") or {}
    return {
        "scenario": evaluation.get("scenario"),
        "pair_id": evaluation.get("pair_id"),
        "arm": evaluation.get("arm"),
        "source_session": evaluation.get("source_session"),
        "destination_session": evaluation.get("destination_session"),
        "target_context_size": _cell(m, "target_context_size"),
        "achieved_context_size": _cell(m, "achieved_context_size"),
        "synthetic_context_size": _cell(m, "synthetic_context_size"),
        "cache_condition": (evaluation.get("cache_condition") or {}).get(
            "condition", telemetry.CACHE_UNKNOWN),
        "input_tokens": _cell(m, "input_tokens"),
        "cache_read_input_tokens": _cell(m, "cache_read_input_tokens"),
        "cache_creation_input_tokens": _cell(m, "cache_creation_input_tokens"),
        "total_input_tokens": _cell(m, "total_input_tokens"),
        "output_tokens": _cell(m, "output_tokens"),
        "capsule_tokens": _cell(m, "capsule_tokens"),
        "turns": _cell(m, "turns"),
        "tool_calls": _cell(m, "tool_calls"),
        "file_reads": _cell(m, "file_reads"),
        "first_correct_action": (first.get("steps") if first.get("found")
                                 else "not reached" if first.get("declared")
                                 else "not declared"),
        "final_correctness": bool(
            (evaluation.get("final_correctness") or {}).get("correct")),
        "causal_validity": chain.get("causal_validity"),
        "first_broken_link": chain.get("first_broken_link"),
        "telemetry_source": evaluation.get("telemetry_source"),
        "measurement_status": evaluation.get("measurement_status"),
        "verdict": verdict or "(pair-level)",
    }


def provenance_rows(evaluation: dict) -> list[dict]:
    """Every metric of one trial, with the five provenance fields."""
    return [
        {"metric_name": name,
         "value": metric.get("value"),
         "unit": metric.get("unit"),
         "source": metric.get("source"),
         "raw_artifact_reference": metric.get("raw_artifact_reference"),
         "measurement_status": metric.get("measurement_status"),
         "note": metric.get("note")}
        for name, metric in (evaluation.get("metrics") or {}).items()
    ]


def _table(rows: list[dict], columns: tuple[str, ...]) -> str:
    if not rows:
        return "_(no rows)_\n"
    head = "| " + " | ".join(columns) + " |"
    rule = "|" + "|".join("---" for _ in columns) + "|"
    body = ["| " + " | ".join(str(row.get(c, "")) for c in columns) + " |"
            for row in rows]
    return "\n".join([head, rule, *body]) + "\n"


def summary(result: dict) -> str:
    """A few lines for a terminal, with nothing rounded up."""
    pooled = result.get("pooled") or {}
    lines = [f"{result['n_trials']} trials, "
             f"{result['pairing']['n_pairs']} matched pairs"]
    for dropped in result["pairing"]["dropped"]:
        lines.append(f"  dropped {dropped['pair_id']}: {dropped['reason']}")
    for name, group in (pooled.get("groups") or {}).items():
        counts = group["verdict_counts"]
        lines.append(
            f"  {name}: " + ", ".join(f"{k}={v}" for k, v in counts.items()
                                      if v))
        pct = group["total_input_tokens_pct_change"]
        if pct["median"] is not None:
            lines.append(f"      total input change, median over "
                         f"{pct['n_measured_pairs']} comparable pairs: "
                         f"{pct['median']:+.1f}%")
        else:
            lines.append("      total input change: not measurable on both "
                         "arms in any pair")
    return "\n".join(lines)


def markdown(result: dict) -> str:
    """The full report. Long on provenance, short on adjectives."""
    pooled = result.get("pooled") or {}
    out = ["# Velra v0.1.2 — Token-Burn benchmark", "",
           f"Pre-registration `{result.get('preregistration_id')}` "
           f"v{result.get('preregistration_version')}, "
           f"sha256 `{(result.get('preregistration_sha256') or '')[:16]}`.", "",
           f"Trials: {result['n_trials']}. "
           f"Matched pairs: {result['pairing']['n_pairs']}.", ""]
    scored = result.get("scored_by") or {}
    if scored.get("git_head"):
        out += [f"Scored by the pipeline at `{scored['git_head']}`"
                + (" (with uncommitted pipeline changes)"
                   if scored.get("pipeline_dirty") else "") + ".", ""]

    if result["pairing"]["dropped"]:
        out += ["## Pairs dropped", "",
                _table([{"pair_id": d["pair_id"], "reason": d["reason"],
                         "detail": str(d.get("differences")
                                       or d.get("invalid_arms")
                                       or d.get("missing_arms"))[:160]}
                        for d in result["pairing"]["dropped"]],
                       ("pair_id", "reason", "detail")), ""]

    out += ["## Verdicts", ""]
    out.append(_table(
        [{"pair_id": p["pair_id"], "benchmark": p.get("benchmark"),
          "verdict": p["verdict"],
          "basis": p.get("basis") or p.get("failure_class") or "",
          "burden_change_pct": (f"{p['burden_change_pct']:+.1f}%"
                                if p.get("burden_change_pct") is not None
                                else telemetry.STATUS_INCONCLUSIVE),
          "baseline_correct": p["arms"]["baseline"]["final_correctness"],
          "velra_correct": p["arms"]["velra"]["final_correctness"],
          "why": p.get("why", "")[:320]}
         for p in result["pairs"]],
        ("pair_id", "benchmark", "verdict", "basis", "burden_change_pct",
         "baseline_correct", "velra_correct", "why")))

    out += ["", "## Per-trial rows", "",
            _table(result["trial_rows"], TRIAL_COLUMNS), ""]

    out += ["## Pooled", ""]
    for name, group in sorted((pooled.get("groups") or {}).items()):
        pct = group["total_input_tokens_pct_change"]
        out += [f"### {name}", "",
                f"* pairs: {group['n_pairs']}, decided: {group['n_decided']}",
                f"* verdicts: {group['verdict_counts']}",
                f"* failure classes: {group['failure_classes'] or 'none'}",
                f"* correct: baseline {group['correctness']['baseline_correct']}"
                f"/{group['correctness']['of']}, "
                f"velra {group['correctness']['velra_correct']}"
                f"/{group['correctness']['of']}",
                (f"* total input change, median over "
                 f"{pct['n_measured_pairs']} comparable pairs "
                 f"({', '.join(f'{v:+.2f}%' for v in pct['values'])}): "
                 f"{pct['median']:+.2f}%" if pct["median"] is not None
                 else "* total input change: **not measurable** on both arms "
                      "in any pair of this group"),
                ""]

    out += ["## How to read the cells", "",
            "* `unavailable` — the structured data did not carry the field. "
            "It is not zero, and it was not recovered from terminal output.",
            "* `inconclusive` — the value depends on something that was not "
            "measured.",
            "* `~n` — a proxy. The provenance table in each trial's "
            "`analysis.json` says what it stands in for.",
            "* `achieved_context_size` is what the runtime reported. A "
            "synthetic fixture's size appears only under "
            "`synthetic_context_size` and is never an observation of Claude.",
            "* `cache_condition: UNKNOWN` means the telemetry did not say. It "
            "does not mean the cache expired.", ""]
    return "\n".join(out)
