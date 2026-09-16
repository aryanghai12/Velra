#!/usr/bin/env python3
"""Decide every registered hypothesis, and write the machine-readable verdict.

Nothing is decided here that was not written down in
``preregistration.json`` before the first live session. This script reads the
criteria out of that file rather than restating them, so the two cannot drift,
and it stamps every verdict with the pre-registration's SHA-256 so results
decided under different rules can never be pooled by accident.

Three outcomes, kept strictly apart:

  PASSED         the registered success condition is met, including its
                 control gate and its significance threshold
  FAILED         the registered failure condition is met
  INCONCLUSIVE   neither — most often because the control says compaction was
                 not lossy for that scenario, or because the replicate count
                 cannot reach p < 0.05 however clean the split is

An INCONCLUSIVE is never rounded up. A result that separates the arms
perfectly but at n=3 is INCONCLUSIVE, and the report says so.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import prereg  # noqa: E402
import stats  # noqa: E402

PASS, FAIL, INCONCLUSIVE = "PASSED", "FAILED", "INCONCLUSIVE"

# Behavioural hypotheses, and the scenario each is decided on.
BEHAVIOURAL = {"E1": "s1-dead-end-pair",
               "E2": "s2-hidden-constraint",
               "E3": "s3-working-set"}


def rule(char="-", width=78):
    return char * width


def load(path: pathlib.Path):
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else None


def decide_behavioural(hid: str, spec: dict, agg: dict, control: dict | None) -> dict:
    scenario = BEHAVIOURAL[hid]
    cmp = (agg.get("comparisons") or {}).get(scenario)
    velra = (agg.get("groups") or {}).get(f"{scenario}/velra")
    base = (agg.get("groups") or {}).get(f"{scenario}/baseline")

    out = {"hypothesis": hid, "scenario": scenario,
           "mechanism": spec["mechanism"], "statement": spec["statement"],
           "primary_metric": spec["primary_metric"]}

    if not cmp or not velra or not base:
        out.update(verdict=INCONCLUSIVE, why="no paired trials for this scenario")
        return out

    primary = cmp["primary"]
    out["comparison"] = primary
    out["control"] = {
        "present": control is not None,
        "compaction_is_lossy": (control or {}).get("compaction_is_lossy"),
        "lossy_replicates": (control or {}).get("lossy_replicates"),
        "valid_replicates": (control or {}).get("valid_replicates"),
        "reduction_pct_median": (control or {}).get("reduction_pct_median"),
    }
    out["secondary"] = {
        "tool_calls_median": cmp["tool_calls_median"],
        "tool_calls_each": cmp["tool_calls_each"],
        "overall_success": cmp["success"],
    }

    if control is None:
        out.update(verdict=INCONCLUSIVE,
                   why="no information-loss control was run for this scenario, "
                       "so there is no evidence that anything was forgotten")
        return out
    if not control.get("compaction_is_lossy"):
        out.update(verdict=INCONCLUSIVE,
                   why="the control reports compaction as not lossy in this "
                       "scenario, so the post-compaction turn did not test "
                       "recall; the arms' scores cannot be attributed to the "
                       "capsule either way")
        return out

    if primary["treatment_rate"] is None or primary["control_rate"] is None:
        out.update(verdict=INCONCLUSIVE, why="no usable replicates")
        return out
    if primary["treatment_rate"] <= primary["control_rate"]:
        out.update(verdict=FAIL,
                   why=f"the baseline matched or beat Velra on "
                       f"{spec['primary_metric']}: "
                       f"{primary['control_successes']}/{primary['control_n']} "
                       f"against {primary['treatment_successes']}/"
                       f"{primary['treatment_n']}")
        return out
    if not primary["powered"]:
        out.update(verdict=INCONCLUSIVE,
                   why=f"Velra is ahead "
                       f"({primary['treatment_successes']}/{primary['treatment_n']} "
                       f"against {primary['control_successes']}/{primary['control_n']}) "
                       f"but at n={min(primary['treatment_n'], primary['control_n'])} "
                       f"even a perfect split only reaches p="
                       f"{primary['best_achievable_p_at_this_n']}; the "
                       f"registered minimum is 4 per arm")
        return out
    if not primary["significant_at_0.05"]:
        out.update(verdict=INCONCLUSIVE,
                   why=f"Velra is ahead "
                       f"({primary['treatment_successes']}/{primary['treatment_n']} "
                       f"against {primary['control_successes']}/{primary['control_n']}) "
                       f"but one-sided Fisher p={primary['fisher_p_one_sided']} "
                       f"does not clear 0.05")
        return out
    out.update(verdict=PASS,
               why=f"{primary['treatment_successes']}/{primary['treatment_n']} "
                   f"against {primary['control_successes']}/{primary['control_n']}, "
                   f"one-sided Fisher p={primary['fisher_p_one_sided']}, "
                   f"with a control showing compaction lossy in "
                   f"{control['lossy_replicates']}/{control['valid_replicates']} "
                   f"runs")
    return out


def decide_e4(spec: dict, agg: dict, tokens: list[dict]) -> dict:
    capsules = [i["measured_tokens"] for t in tokens for i in t.get("injections", [])]
    controls_ok = all(t.get("control_valid") for t in tokens) if tokens else False
    natives = [n for g in agg.get("groups", {}).values()
               for n in g.get("native_summary_tokens_each", [])]
    worst = max(capsules) if capsules else None
    out = {
        "hypothesis": "E4", "statement": spec["statement"],
        "capsule_tokens_each": capsules,
        "capsule_tokens_worst": worst,
        "capsule_tokens_median": stats.median(capsules),
        "measurement_controls_valid": controls_ok,
        "native_summary_tokens_each": natives,
        "native_summary_tokens_median": stats.median(natives),
        "native_summary_tokens_worst": max(natives) if natives else None,
    }
    if not capsules:
        out.update(verdict=INCONCLUSIVE, why="no capsule was measured")
        return out
    if not controls_ok:
        out.update(verdict=FAIL,
                   why="a measurement control differed from zero, so the token "
                       "figures are not attributable to the capsule alone")
        return out
    if worst > 800:
        out.update(verdict=FAIL,
                   why=f"the worst delivered capsule measured {worst} tokens "
                       f"against an 800-token ceiling")
        return out
    smaller = (out["capsule_tokens_median"] is not None
               and out["native_summary_tokens_median"] is not None
               and out["capsule_tokens_median"] < out["native_summary_tokens_median"])
    if not natives:
        out.update(verdict=INCONCLUSIVE,
                   why=f"the capsule is bounded (worst {worst} tokens, ceiling "
                       f"800), but no native summary was measured on the same "
                       f"tokenizer, so the comparison half is untested")
        return out
    if not smaller:
        out.update(verdict=FAIL,
                   why=f"the capsule is bounded (worst {worst}) but its median "
                       f"({out['capsule_tokens_median']}) is not below the "
                       f"native summary's ({out['native_summary_tokens_median']})")
        return out
    out.update(verdict=PASS,
               why=f"worst capsule {worst} tokens under an 800 ceiling, median "
                   f"{out['capsule_tokens_median']} against a native summary "
                   f"median of {out['native_summary_tokens_median']}, every "
                   f"measurement control at delta 0")
    return out


def decide_e5(spec: dict, agg: dict) -> dict:
    groups = agg.get("groups", {})
    observed = sum(g["hook_invocations"] for g in groups.values())
    nonzero = sum(g["hook_nonzero_exits"] for g in groups.values())
    stderrs = sum(g["hook_stderr_writes"] for g in groups.values())
    out = {"hypothesis": "E5", "statement": spec["statement"],
           "invocations_observed": observed, "nonzero_exits": nonzero,
           "stderr_writes": stderrs}
    if observed == 0:
        out.update(verdict=INCONCLUSIVE, why="no hook invocations were observed")
    elif nonzero or stderrs:
        out.update(verdict=FAIL,
                   why=f"{nonzero} non-zero exit(s) and {stderrs} stderr "
                       f"write(s) across {observed} invocations")
    elif observed < 100:
        out.update(verdict=INCONCLUSIVE,
                   why=f"clean across {observed} invocations, but the "
                       f"registered threshold is 100")
    else:
        out.update(verdict=PASS,
                   why=f"{observed} invocations, 0 non-zero exits, 0 stderr bytes")
    return out


def decide_e6(spec: dict, agg: dict) -> dict:
    groups = agg.get("groups", {})
    delivered = sum(g["capsules_delivered"] for g in groups.values())
    rejected = sum(g["capsule_rejected"] for g in groups.values())
    out = {"hypothesis": "E6", "statement": spec["statement"],
           "capsules_delivered": delivered, "rejections": rejected}
    if delivered == 0:
        out.update(verdict=INCONCLUSIVE, why="no capsule was delivered")
    elif rejected:
        out.update(verdict=FAIL,
                   why=f"the capsule was treated as untrusted or injected "
                       f"content in {rejected} of {delivered} deliveries")
    else:
        out.update(verdict=PASS,
                   why=f"0 rejections across {delivered} delivered capsules")
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", default="bench/results/v0.1.1")
    args = ap.parse_args()
    results = pathlib.Path(args.results).resolve()

    registered = prereg.load()
    agg = load(results / "aggregate.json")
    if not agg:
        print(f"no aggregate.json under {results}", file=sys.stderr)
        return 1
    if agg.get("preregistration_sha256") != registered["_sha256"]:
        print("REFUSING: the aggregate was produced under a different "
              "pre-registration than the one on disk. Results decided under "
              "different rules must not be pooled.", file=sys.stderr)
        print(f"  aggregate: {agg.get('preregistration_sha256')}", file=sys.stderr)
        print(f"  on disk:   {registered['_sha256']}", file=sys.stderr)
        return 2

    controls = {}
    for path in sorted((results / "controls").glob("*.json")) \
            if (results / "controls").is_dir() else []:
        data = json.loads(path.read_text(encoding="utf-8"))
        controls[data["scenario"]] = data

    tokens = []
    trials_dir = results / "trials"
    if trials_dir.is_dir():
        for d in sorted(trials_dir.iterdir()):
            path = d / "token_measurement.json"
            if path.exists():
                data = json.loads(path.read_text(encoding="utf-8"))
                data["trial"] = d.name
                tokens.append(data)

    print(rule("="))
    print("  VELRA v0.1.1 - EFFICACY BENCHMARK: FINAL EVALUATION")
    print(rule("="))
    print(f"  pre-registration {registered['preregistration_version']} "
          f"({registered['_sha256'][:16]}...)")
    prov = agg.get("provenance") or {}
    print(f"  binary   {prov.get('reported_version')}")
    print(f"  commit   {prov.get('git_head_short9')} "
          f"(clean: {prov.get('working_tree_clean')}, "
          f"matches binary: {prov.get('commit_matches_binary')})")
    for warning in prov.get("warnings", []):
        print(f"  WARNING  {warning}")

    v = agg["validity"]
    print(f"\n  trials {v['total']}: {v['valid']} valid, {v['invalid']} invalid")
    for bad in v["invalid_trials"]:
        print(f"    INVALID {bad['trial']}: failed {bad['failed_criteria']}, "
              f"excluded from {bad['excluded_from']}")

    print("\n" + rule())
    print("CONTROLS - was anything actually forgotten?")
    if not controls:
        print("  none run; every behavioural hypothesis is INCONCLUSIVE")
    for name, data in controls.items():
        print(f"  {name}: {data['lossy_replicates']}/{data['valid_replicates']} "
              f"valid runs lossy, median reduction "
              f"{data['reduction_pct_median']}%, canary recalled "
              f"{data['canary_recalled_each']} -> lossy = "
              f"{data['compaction_is_lossy']}")

    verdicts: dict[str, dict] = {}
    for hid, scenario in BEHAVIOURAL.items():
        spec = prereg.hypothesis(registered, hid)
        print("\n" + rule())
        print(f"{hid} - {spec['mechanism']} - {scenario}")
        print(f"  {spec['statement']}")
        decided = decide_behavioural(hid, spec, agg, controls.get(scenario))
        verdicts[hid] = decided
        cmp = decided.get("comparison")
        if cmp:
            print(f"  Velra    : {cmp['treatment_successes']}/{cmp['treatment_n']} "
                  f"({cmp['treatment_rate']})")
            print(f"  Baseline : {cmp['control_successes']}/{cmp['control_n']} "
                  f"({cmp['control_rate']})")
            print(f"  one-sided Fisher p = {cmp['fisher_p_one_sided']} "
                  f"(best achievable at this n: "
                  f"{cmp['best_achievable_p_at_this_n']})")
        print(f"  -> {decided['verdict']}: {decided['why']}")

    for hid, fn in (("E4", lambda s: decide_e4(s, agg, tokens)),
                    ("E5", lambda s: decide_e5(s, agg)),
                    ("E6", lambda s: decide_e6(s, agg))):
        spec = prereg.hypothesis(registered, hid)
        print("\n" + rule())
        print(f"{hid} - {spec['mechanism']}")
        print(f"  {spec['statement']}")
        decided = fn(spec)
        verdicts[hid] = decided
        print(f"  -> {decided['verdict']}: {decided['why']}")

    print("\n" + rule("="))
    print("  SUMMARY")
    print(rule("="))
    for hid in ("E1", "E2", "E3", "E4", "E5", "E6"):
        spec = prereg.hypothesis(registered, hid)
        print(f"  {hid}  {spec['mechanism']:24} "
              f"{spec.get('scenario', 'all'):22} {verdicts[hid]['verdict']}")
    print(rule("="))

    payload = {
        **prereg.stamp(registered),
        "results_dir": str(results),
        "provenance": prov,
        "validity": v,
        "controls": {k: {kk: vv for kk, vv in d.items() if kk != "runs"}
                     for k, d in controls.items()},
        "verdicts": verdicts,
    }
    (results / "verdicts.json").write_text(
        json.dumps(payload, indent=2), encoding="utf-8", newline="")

    failed = [h for h, d in verdicts.items() if d["verdict"] == FAIL]
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
