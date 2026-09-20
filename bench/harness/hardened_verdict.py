#!/usr/bin/env python3
"""Decide every v0.1.2 hypothesis from the staged evidence.

Reads ``preregistration_v2.json`` rather than restating it, so the criteria and
the code cannot drift, and stamps every verdict with the pre-registration's
SHA-256 so results decided under different rules can never be pooled.

What makes this different from ``scenario_verdict.py`` (which decided the
v0.1.1 hypotheses and stays for those artifacts) is that a behavioural verdict
here has to survive the causal chain before the arms are compared at all. The
question is not "did the arms differ" but "did the arms differ *because* Velra
preserved something native compaction lost", and those need separate evidence:

  stage 1 fails  ->  NOT CAUSALLY TESTABLE. The fact survived compaction, so
                     whatever the arms did, it was not about this. Reported,
                     and counted as neither a win nor a loss.
  stage 2-4 fail ->  the arms are still compared, and the verdict carries the
                     stage that broke, because "Velra did not help" and "Velra
                     never delivered the thing" are different findings with
                     different fixes.

Five outcomes, kept strictly apart. An INCONCLUSIVE is never rounded up, and a
BASELINE WINS is reported with the same prominence as a VELRA WINS.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import prereg  # noqa: E402
import stages as stage_mod  # noqa: E402
import stats  # noqa: E402

VELRA_WINS = "VELRA WINS"
BASELINE_WINS = "BASELINE WINS"
TIE = "TIE"
INCONCLUSIVE = "INCONCLUSIVE"
UNTESTABLE = "NOT CAUSALLY TESTABLE"
PASSED, FAILED = "PASSED", "FAILED"


def rule(char="-", width=78):
    return char * width


def load(path: pathlib.Path):
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else None


def load_stages(results: pathlib.Path) -> list[dict]:
    directory = results / "stages"
    if not directory.is_dir():
        return []
    return [json.loads(p.read_text(encoding="utf-8"))
            for p in sorted(directory.glob("*.json"))]


# ---------------------------------------------------------------------------


def chain_summary(rows: list[dict]) -> dict:
    """Where the chain broke, across the Velra trials of one scenario."""
    velra = [r for r in rows if r["arm"] == "velra"]
    classes: dict[str, int] = {}
    for r in velra:
        classes[r["classification"]] = classes.get(r["classification"], 0) + 1
    per_stage = {}
    for name in stage_mod.STAGES:
        statuses = [r["stages"][name]["status"] for r in velra]
        per_stage[name] = {
            "pass": statuses.count("pass"),
            "fail": statuses.count("fail"),
            "n/a": statuses.count("n/a"),
        }
    return {
        "velra_trials": len(velra),
        "classifications": classes,
        "per_stage": per_stage,
        "complete_chains": classes.get("CHAIN_COMPLETE", 0),
        "first_break": min(
            (r["first_failed_stage"] for r in velra if r["first_failed_stage"]),
            key=lambda s: stage_mod.STAGES.index(s), default=None),
    }


def decide_behavioural(spec: dict, agg: dict, control: dict | None,
                       rows: list[dict]) -> dict:
    scenario = spec["scenario"]
    cmp = (agg.get("comparisons") or {}).get(scenario)
    out = {
        "hypothesis": spec["id"],
        "scenario": scenario,
        "mechanism": spec["mechanism"],
        "target_fact": spec.get("target_fact"),
        "statement": spec["statement"],
        "primary_metric": spec["primary_metric"],
        "chain": chain_summary(rows),
    }

    # --- stage 1 gate: is the scenario causally testable at all? ------------
    loss = stage_mod.stage_compaction_loss(control)
    out["stage1_compaction_loss"] = loss
    if loss["status"] == "n/a":
        out.update(verdict=INCONCLUSIVE, outcome=INCONCLUSIVE,
                   why=f"stage 1 could not be evaluated: {loss['reason']}")
        return out
    if loss["status"] == "fail":
        out.update(verdict=UNTESTABLE, outcome=UNTESTABLE,
                   why="the target fact survived native compaction in a "
                       "majority of control replicates, so no behavioural "
                       "difference here can be attributed to the capsule")
        return out

    if not cmp:
        out.update(verdict=INCONCLUSIVE, outcome=INCONCLUSIVE,
                   why="no trials on disk for this scenario")
        return out

    pairing = cmp.get("pairing") or {}
    out["pairing"] = pairing
    if not pairing.get("n_pairs"):
        dropped = "; ".join(
            f"{d['pair_id']}: " + ", ".join(
                f"{r['arm']} {r['reason']}" for r in d["because"])
            for d in pairing.get("dropped_pairs", []))
        out.update(verdict=INCONCLUSIVE, outcome=INCONCLUSIVE,
                   why="no pair has a usable trial on both arms"
                       + (f"; dropped: {dropped}" if dropped else ""))
        return out

    primary = cmp["primary"]
    out["comparison"] = primary
    out["secondary"] = {
        "overall_success": cmp["success"],
        "tool_calls_median": cmp["tool_calls_median"],
        "tool_calls_each": cmp["tool_calls_each"],
        "note": "tool-call counts describe how the work went. They are never "
                "the verdict; see the metric policy in the pre-registration.",
    }

    t, c = primary["treatment_rate"], primary["control_rate"]
    if t is None or c is None:
        out.update(verdict=INCONCLUSIVE, outcome=INCONCLUSIVE,
                   why="one arm produced no scored trial")
        return out

    minimum = prereg.stamp_v2()["minimum_replicates_per_arm"]
    underpowered = pairing["n_pairs"] < minimum

    if c > t:
        outcome, verdict = BASELINE_WINS, FAILED
        why = (f"the baseline beat Velra on {spec['primary_metric']}: "
               f"{primary['control_successes']}/{primary['control_n']} against "
               f"{primary['treatment_successes']}/{primary['treatment_n']}")
    elif c == t:
        outcome, verdict = TIE, FAILED
        why = (f"the arms tied on {spec['primary_metric']} at "
               f"{primary['treatment_successes']}/{primary['treatment_n']}; the "
               f"registered failure condition is baseline >= Velra")
    elif primary["significant_at_0.05"] and not underpowered:
        outcome, verdict = VELRA_WINS, PASSED
        why = (f"Velra beat the baseline on {spec['primary_metric']} "
               f"({primary['treatment_successes']}/{primary['treatment_n']} "
               f"against {primary['control_successes']}/{primary['control_n']}), "
               f"one-sided Fisher p = {primary['fisher_p_one_sided']}, and the "
               f"control reports the target fact lost")
    else:
        outcome, verdict = INCONCLUSIVE, INCONCLUSIVE
        why = (f"Velra's rate is higher but the result is not significant at "
               f"n={pairing['n_pairs']} pairs "
               f"(p = {primary['fisher_p_one_sided']}, best achievable "
               f"{primary['best_achievable_p_at_this_n']})")
        if underpowered:
            why += (f"; below the registered minimum of {minimum} pairs, so "
                    f"this is descriptive only")

    # A behavioural result is not interpretable without knowing whether the
    # mechanism under test ever reached the agent.
    chain = out["chain"]
    broke = chain.get("first_break")
    if broke in ("capture", "delivery", "acceptance"):
        why += (f"; note that the causal chain broke at {broke.upper()} in the "
                f"Velra arm, so this comparison does not test the mechanism it "
                f"names")
    out.update(verdict=verdict, outcome=outcome, why=why)
    return out


def decide_budget(spec: dict, rows: list[dict]) -> dict:
    velra = [r for r in rows if r["arm"] == "velra"]
    measured = []
    controls_valid = True
    for r in velra:
        m = (r["stages"]["delivery"] or {}).get("measured_tokens")
        if not m:
            continue
        measured.extend(m["tokens_each"])
        controls_valid = controls_valid and bool(m["control_valid"])
    worst = max(measured, default=None)
    ceiling = stage_mod.TOKEN_CEILING
    ok = bool(measured) and worst <= ceiling and controls_valid
    return {
        "hypothesis": spec["id"], "mechanism": spec["mechanism"],
        "scenario": spec.get("scenario", "all"), "statement": spec["statement"],
        "capsule_tokens_each": measured,
        "capsule_tokens_worst": worst,
        "capsule_tokens_median": stats.median(measured),
        "measurement_controls_valid": controls_valid,
        "ceiling": ceiling,
        "verdict": PASSED if ok else FAILED,
        "outcome": PASSED if ok else FAILED,
        "why": (f"{len(measured)} delivered capsules, worst {worst} tokens "
                f"against a {ceiling}-token ceiling")
        if measured else "no capsule was measured with the real tokenizer",
    }


def decide_hooks(spec: dict, agg: dict) -> dict:
    groups = (agg.get("groups") or {}).values()
    observed = sum(g.get("hook_invocations") or 0 for g in groups)
    nonzero = sum(g.get("hook_nonzero_exits") or 0 for g in groups)
    stderr = sum(g.get("hook_stderr_writes") or 0 for g in groups)
    ok = observed >= 100 and nonzero == 0 and stderr == 0
    return {
        "hypothesis": spec["id"], "mechanism": spec["mechanism"],
        "scenario": spec.get("scenario", "all"), "statement": spec["statement"],
        "invocations_observed": observed, "nonzero_exits": nonzero,
        "stderr_writes": stderr,
        "verdict": PASSED if ok else FAILED, "outcome": PASSED if ok else FAILED,
        "why": f"{observed} invocations, {nonzero} non-zero exits, "
               f"{stderr} stderr writes",
    }


def decide_acceptance(spec: dict, rows: list[dict]) -> dict:
    velra = [r for r in rows if r["arm"] == "velra"]
    verdicts = [r["stages"]["acceptance"].get("verdict") for r in velra]
    delivered = [v for v in verdicts if v and v != "N/A"]
    rejected = verdicts.count("REJECTED")
    ok = bool(delivered) and rejected == 0
    return {
        "hypothesis": spec["id"], "mechanism": spec["mechanism"],
        "scenario": spec.get("scenario", "all"), "statement": spec["statement"],
        "capsules_delivered": len(delivered),
        "verdict_counts": {v: verdicts.count(v) for v in set(verdicts) if v},
        "rejections": rejected,
        "verdict": PASSED if ok else FAILED, "outcome": PASSED if ok else FAILED,
        "why": (f"{rejected} rejection(s) across {len(delivered)} deliveries"
                if delivered else "no capsule was delivered"),
    }


# ---------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--results", default="bench/results/v0.1.2")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    results = pathlib.Path(args.results).resolve()
    agg = load(results / "aggregate.json") or {}
    rows = load_stages(results)
    spec = prereg.load_v2()
    by_id = {h["id"]: h for h in spec["hypotheses"]}

    verdicts: dict[str, dict] = {}
    for hid, hypothesis in by_id.items():
        scenario = hypothesis.get("scenario")
        if scenario and scenario != "all":
            control = load(results / "controls" / f"{scenario}.json")
            mine = [r for r in rows if r["scenario"] == scenario]
            verdicts[hid] = decide_behavioural(hypothesis, agg, control, mine)
        elif hypothesis["mechanism"].startswith("truncation"):
            verdicts[hid] = decide_budget(hypothesis, rows)
        elif hypothesis["mechanism"] == "hook contract":
            verdicts[hid] = decide_hooks(hypothesis, agg)
        else:
            verdicts[hid] = decide_acceptance(hypothesis, rows)

    out = {
        **prereg.stamp_v2(),
        "results_dir": str(results),
        "provenance": agg.get("provenance"),
        "validity": agg.get("validity"),
        "stage_rows": len(rows),
        "verdicts": verdicts,
    }
    target = pathlib.Path(args.out) if args.out else results / "verdicts_v2.json"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(out, indent=2), encoding="utf-8", newline="")

    print(rule("="))
    print("  v0.1.2 hardened evaluation")
    print(rule("="))
    for hid, v in verdicts.items():
        print()
        print(rule())
        print(f"{hid} - {v.get('mechanism', '')}")
        print(f"  {v['statement']}")
        if "chain" in v:
            per = v["chain"]["per_stage"]
            chain = "  ".join(
                f"{name}:{per[name]['pass']}P/{per[name]['fail']}F"
                for name in stage_mod.STAGES)
            print(f"  causal chain: {chain}")
        print(f"  -> {v['outcome']}: {v['why']}")

    print()
    print(rule("="))
    print("  SUMMARY")
    print(rule("="))
    for hid, v in verdicts.items():
        scenario = v.get("scenario", "all")
        print(f"  {hid}  {v.get('mechanism', ''):<24} {scenario:<22} {v['outcome']}")
    print(rule("="))
    print(f"\nverdicts: {target}")
    print("Every number above is recomputed from the raw artifacts. Read them "
          "before believing this summary.")

    behavioural = [v for v in verdicts.values() if "chain" in v]
    return 0 if all(v["outcome"] != INCONCLUSIVE for v in behavioural) else 0


if __name__ == "__main__":
    raise SystemExit(main())
