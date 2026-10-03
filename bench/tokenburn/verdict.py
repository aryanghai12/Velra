#!/usr/bin/env python3
"""One verdict per matched pair, read out of the pre-registration.

Four values and nothing else: ``VELRA_WIN``, ``BASELINE_WIN``, ``TIE``,
``INCONCLUSIVE``. The order the rules are applied in is registered in
``preregistration_tokenburn.json`` and repeated here only as code, so the two
cannot drift without the hash changing.

The two rules that carry the weight:

**Correctness is primary.** A Velra arm that spent a tenth of the input and
wrote the wrong fix loses to a baseline that spent everything and got it right.
That case is decided before the token comparison is even looked at.

**Unavailable telemetry is INCONCLUSIVE, never zero and never a win.** A Velra
arm that is correct, in a pair whose input burden could not be measured on both
sides, does not produce a Velra win — link I of the causal chain was not
demonstrated, and the pair says so. The one asymmetry is deliberate and runs
*against* Velra: a baseline that is correct where Velra is not wins regardless
of telemetry, because correctness was fully observed on both arms and nothing
about the token accounting could rescue a wrong answer.

**A TIE needs two correct arms.** The mirror case -- Velra correct, the
baseline not, and no material reduction -- has no registered rule: VELRA_WIN
needs the reduction and TIE needs both arms correct. It is INCONCLUSIVE with
link I named, the same way an undemonstrated link rounds down everywhere else.
"""

from __future__ import annotations

import pathlib
import statistics
from typing import Sequence

if __package__ in (None, ""):
    import sys
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import causal  # type: ignore[no-redef]
    import prereg  # type: ignore[no-redef]
    import telemetry  # type: ignore[no-redef]
else:
    from . import causal, prereg, telemetry

Metric = telemetry.Metric

VELRA_WIN = "VELRA_WIN"
BASELINE_WIN = "BASELINE_WIN"
TIE = "TIE"
INCONCLUSIVE = "INCONCLUSIVE"

#: Links whose failure on the Velra arm means the trial never got far enough to
#: be evidence either way. G and H are outcomes, not preconditions.
VELRA_PRECONDITIONS = ("A_large_state", "B_absent_natively", "C_retained",
                       "D_staged", "E_delivered", "F_received")

#: On the baseline arm only A and B are preconditions. H failing on a baseline
#: is the measurement: it is what "the fresh native session could not continue
#: the work" looks like, and turning it into an invalid trial would delete the
#: benchmark's own result.
BASELINE_PRECONDITIONS = ("A_large_state", "B_absent_natively")


def _metric(evaluation: dict, name: str, unit: str = "tokens") -> Metric:
    raw = (evaluation.get("metrics") or {}).get(name)
    if raw is None:
        return Metric.unavailable(name, unit, evaluation.get("trial"),
                                  "the pipeline produced no reading")
    return Metric.from_json(raw)


def _correct(evaluation: dict) -> bool:
    return bool((evaluation.get("final_correctness") or {}).get("correct"))


def _precondition_failure(chain: dict, preconditions: Sequence[str]) -> str | None:
    for link in preconditions:
        if chain["links"][link]["status"] == "fail":
            return link
    return None


def compare_burden(baseline: dict, velra: dict) -> dict:
    """A1, as a pair: the total, the parts, and what each comparison is worth.

    Unlike categories are never combined. ``total_input_tokens`` is the
    headline because it is the only figure that describes what the session
    actually paid for; ``input_tokens`` alone is reported beside it and
    explicitly labelled as uncached input, because quoting it as "input" when
    one arm read 300k tokens from cache would be the single most misleading
    number this benchmark could print.
    """
    out: dict[str, dict] = {}
    for name in ("total_input_tokens", "input_tokens", "cache_read_input_tokens",
                 "cache_creation_input_tokens", "output_tokens"):
        b = _metric(baseline, name)
        v = _metric(velra, name)
        out[name] = {
            "baseline": b.to_json(),
            "velra": v.to_json(),
            "absolute_difference": telemetry.difference(
                f"{name}_delta", v, b,
                note="velra minus baseline; negative means Velra spent less"
            ).to_json(),
            "percentage_difference": telemetry.percent_change(
                f"{name}_pct", v, b,
                note="relative to the baseline arm").to_json(),
        }
    return out


def compare_effort(baseline: dict, velra: dict) -> dict:
    out: dict[str, dict] = {}
    for name, unit in (("turns", "turns"), ("tool_calls", "calls"),
                       ("file_reads", "reads"), ("searches", "searches"),
                       ("steps_to_first_correct_action", "steps")):
        b = _metric(baseline, name, unit)
        v = _metric(velra, name, unit)
        out[name] = {
            "baseline": b.to_json(),
            "velra": v.to_json(),
            "absolute_difference": telemetry.difference(
                f"{name}_delta", v, b, note="velra minus baseline").to_json(),
        }
    return out


def pair_verdict(pair: dict, chains: dict) -> dict:
    """Decide one pair. ``chains`` maps trial name to its causal chain."""
    baseline = pair["arms"]["baseline"]
    velra = pair["arms"]["velra"]
    b_chain = chains[baseline["trial"]]
    v_chain = chains[velra["trial"]]

    burden = compare_burden(baseline, velra)
    effort = compare_effort(baseline, velra)
    b_correct = _correct(baseline)
    v_correct = _correct(velra)

    result = {
        "pair_id": pair["pair_id"],
        "benchmark": pair.get("benchmark"),
        "scenario": pair.get("scenario"),
        "pair_key": pair.get("pair_key"),
        "arms": {
            "baseline": {"trial": baseline["trial"],
                         "source_session": baseline.get("source_session"),
                         "destination_session": baseline.get("destination_session"),
                         "telemetry_source": baseline.get("telemetry_source"),
                         "measurement_status": baseline.get("measurement_status"),
                         "cache_condition": (baseline.get("cache_condition") or {}).get("condition"),
                         "final_correctness": b_correct,
                         "causal_validity": b_chain["causal_validity"]},
            "velra": {"trial": velra["trial"],
                      "source_session": velra.get("source_session"),
                      "destination_session": velra.get("destination_session"),
                      "telemetry_source": velra.get("telemetry_source"),
                      "measurement_status": velra.get("measurement_status"),
                      "cache_condition": (velra.get("cache_condition") or {}).get("condition"),
                      "final_correctness": v_correct,
                      "causal_validity": v_chain["causal_validity"]},
        },
        "burden": burden,
        "effort": effort,
        "thresholds": {
            "material_burden_change_pct": prereg.material_burden_pct(),
            "material_effort_change_steps": prereg.material_effort_steps(),
        },
    }

    # Attached before any early return. A pair decided on correctness still
    # reports what the burden did -- "a tenth of the tokens and the wrong
    # answer" is the most informative row this benchmark can produce, and
    # hiding the number behind the verdict would waste it.
    headline = (burden["total_input_tokens"]["percentage_difference"] or {})
    if headline.get("value") is not None:
        result["burden_change_pct"] = headline["value"]

    # 1 -- an unusable capture is not evidence about anything.
    for arm, evaluation in (("baseline", baseline), ("velra", velra)):
        if not (evaluation.get("capture") or {}).get("usable", True):
            result.update(verdict=INCONCLUSIVE, failure_class="UNUSABLE_CAPTURE",
                          why=f"the {arm} capture is more than a quarter "
                              f"malformed and cannot be analysed")
            return result

    # 2 -- preconditions, baseline first.
    broken = _precondition_failure(b_chain, BASELINE_PRECONDITIONS)
    if broken:
        result.update(verdict=INCONCLUSIVE,
                      failure_class=prereg.failure_class(broken),
                      why=f"baseline arm: {b_chain['links'][broken]['why']}",
                      broken_link=f"baseline/{broken}")
        return result
    broken = _precondition_failure(v_chain, VELRA_PRECONDITIONS)
    if broken:
        result.update(verdict=INCONCLUSIVE,
                      failure_class=prereg.failure_class(broken),
                      why=f"velra arm: {v_chain['links'][broken]['why']}",
                      broken_link=f"velra/{broken}")
        return result

    # Past this point both arms ran the scenario as designed and the Velra
    # mechanism held through F, so the burden is a comparison of the two
    # arms whatever the verdict turns out to be. `pool` takes its median over
    # exactly these pairs: pooling only the decided ones would drop a pair
    # because its burden went against Velra.
    result["burden_comparable"] = True

    # 3 -- correctness, which outranks every token comparison.
    if b_correct and not v_correct:
        result.update(verdict=BASELINE_WIN, failure_class="TASK_FAILURE",
                      basis="correctness",
                      why="the Velra arm did not complete the task correctly "
                          "and the baseline did; no token reduction can "
                          "outweigh that")
        return result

    # 4 -- from here a verdict needs the burden actually measured on both arms.
    total = burden["total_input_tokens"]
    pct = total["percentage_difference"]
    if pct["measurement_status"] not in (telemetry.STATUS_MEASURED,
                                         telemetry.STATUS_PROXY):
        result.update(
            verdict=INCONCLUSIVE, failure_class="TELEMETRY_UNAVAILABLE",
            basis="telemetry",
            why="total input burden was not measurable on both arms, so causal "
                "link I was not demonstrated: " + (pct.get("note") or ""),
            correctness_outcome={"baseline": b_correct, "velra": v_correct})
        return result

    change = pct["value"]
    threshold = prereg.material_burden_pct()

    if not v_correct and not b_correct:
        result.update(verdict=INCONCLUSIVE, failure_class="TASK_FAILURE",
                      basis="correctness",
                      why="neither arm completed the task correctly; the token "
                          "difference between two wrong answers is not a result")
        return result

    if v_correct and change <= -threshold:
        result.update(verdict=VELRA_WIN, basis="burden+correctness",
                      burden_change_pct=change,
                      why=f"the Velra arm was correct and spent {abs(change):.1f}% "
                          f"less total input than the baseline")
        return result
    if b_correct and change >= threshold:
        result.update(verdict=BASELINE_WIN, basis="burden+correctness",
                      burden_change_pct=change,
                      why=f"the baseline was correct and the Velra arm spent "
                          f"{change:.1f}% more total input")
        return result
    if not b_correct:
        # Only the Velra arm was correct, and the reduction a VELRA_WIN needs
        # did not happen. The registered order has no rule for this: TIE is
        # registered for *both arms correct*, and the arms here did not reach
        # the same correctness state. Link I is the one not demonstrated, so
        # the pair rounds down, as an undemonstrated link does everywhere
        # else, and the correctness split is kept on the record.
        result.update(verdict=INCONCLUSIVE, basis="no registered rule",
                      burden_change_pct=change,
                      broken_link="pair/I_burden_reduced",
                      correctness_outcome={"baseline": b_correct,
                                           "velra": v_correct},
                      why=f"only the Velra arm was correct, but its total "
                          f"input changed by {change:+.1f}%, short of the "
                          f"registered {threshold:.0f}% reduction a VELRA_WIN "
                          f"requires; a TIE requires both arms correct, so no "
                          f"registered rule decides this pair")
        return result
    result.update(verdict=TIE, basis="burden+correctness",
                  burden_change_pct=change,
                  why=f"both arms were correct and the total input burden "
                      f"differed by {change:.1f}%, inside the registered "
                      f"{threshold:.0f}% materiality threshold")
    return result


# --------------------------------------------------------------------------
# pooling
# --------------------------------------------------------------------------


def _measured_values(pairs: Sequence[dict], metric: str, field: str) -> list:
    out = []
    for pair in pairs:
        entry = (pair.get("burden") or {}).get(metric) or {}
        cell = entry.get(field) or {}
        if cell.get("measurement_status") in (telemetry.STATUS_MEASURED,
                                              telemetry.STATUS_PROXY) \
                and cell.get("value") is not None:
            out.append(cell["value"])
    return out


def pool(verdicts: Sequence[dict]) -> dict:
    """Count the verdicts; summarise only what is comparable.

    No mean is taken over a mixture of measured and unavailable values, and no
    metric is averaged across benchmarks: A is about input burden and B is
    about effort after ``/clear``, and their medians are not the same quantity.
    The counts are the headline; the medians are described, with their
    denominators, as a secondary.
    """
    by_benchmark: dict[str, list[dict]] = {}
    for verdict in verdicts:
        by_benchmark.setdefault(verdict.get("benchmark") or "?", []).append(verdict)

    groups = {}
    for name, pairs in sorted(by_benchmark.items()):
        counts = {v: 0 for v in (VELRA_WIN, BASELINE_WIN, TIE, INCONCLUSIVE)}
        classes: dict[str, int] = {}
        for pair in pairs:
            counts[pair["verdict"]] = counts.get(pair["verdict"], 0) + 1
            if pair.get("failure_class"):
                classes[pair["failure_class"]] = classes.get(
                    pair["failure_class"], 0) + 1
        decided = [p for p in pairs if p["verdict"] != INCONCLUSIVE]
        comparable = [p for p in pairs if p.get("burden_comparable")]
        pcts = _measured_values(comparable, "total_input_tokens",
                                "percentage_difference")
        deltas = _measured_values(comparable, "total_input_tokens",
                                  "absolute_difference")
        groups[name] = {
            "n_pairs": len(pairs),
            "n_decided": len(decided),
            "verdict_counts": counts,
            "failure_classes": classes,
            "total_input_tokens_pct_change": {
                "n_measured_pairs": len(pcts),
                "median": round(statistics.median(pcts), 3) if pcts else None,
                "values": [round(p, 3) for p in pcts],
                "note": ("median over every pair whose Velra mechanism held "
                         "(no unusable capture, no broken precondition link) "
                         "and whose total was measured on both arms, "
                         "whatever its verdict; other pairs are excluded and "
                         "counted in n_pairs"),
            },
            "total_input_tokens_absolute_delta": {
                "n_measured_pairs": len(deltas),
                "median": statistics.median(deltas) if deltas else None,
                "values": deltas,
            },
            "correctness": {
                "baseline_correct": sum(
                    1 for p in pairs if p["arms"]["baseline"]["final_correctness"]),
                "velra_correct": sum(
                    1 for p in pairs if p["arms"]["velra"]["final_correctness"]),
                "of": len(pairs),
            },
        }

    overall = {v: 0 for v in (VELRA_WIN, BASELINE_WIN, TIE, INCONCLUSIVE)}
    for group in groups.values():
        for key, value in group["verdict_counts"].items():
            overall[key] = overall.get(key, 0) + value
    return {"groups": groups, "overall_verdict_counts": overall,
            "n_pairs": len(verdicts), **prereg.stamp()}
