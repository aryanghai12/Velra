#!/usr/bin/env python3
"""The whole pipeline, offline, on eighteen scripted cases with known answers.

    python bench/tokenburn/selftest.py
    python bench/tokenburn/selftest.py --keep /tmp/tb-selftest --verbose

No Claude process. No network. No API spend. :mod:`mock_adapter` writes trial
directories, :mod:`aggregate` runs the production pipeline over them, and every
expectation below is checked against what came out.

The failure mode this exists to catch is the one that only shows up after the
money has been spent: a metric that never fires, an aggregate that drops an
arm, a verdict that says INCONCLUSIVE because a key was missing rather than
because the evidence was. Each case below is a value of :class:`MockSpec` and a
statement about what the pipeline must conclude from it.

The eighteen cases §2 of the Phase 3 specification requires
-----------------------------------------------------------

===  ===========================================  ==========================
 1   strong Velra win                             VELRA_WIN
 2   strong baseline win                          BASELINE_WIN
 3   tie                                          TIE
 4   invalid trial                                dropped, named
 5   missing telemetry (partial cache fields)     INCONCLUSIVE
 6   malformed JSONL                              survives; counted
 7   duplicate event                              deduplicated; totals intact
 8   cache-hit                                    cache_condition HIT
 9   cache-miss                                   cache_condition MISS
10   successful ``velra restore``                 links C and D pass
11   successful SessionStart injection            links E and F pass
12   double-injection attempt                     INCONCLUSIVE DELIVERY_FAILURE
13   wrong workspace                              INCONCLUSIVE STAGING_FAILURE
14   stale capsule                                INCONCLUSIVE STAGING_FAILURE
15   correctness lost despite token reduction     BASELINE_WIN TASK_FAILURE
16   telemetry unavailable (none at all)          INCONCLUSIVE
17   incomplete causal chain (capture failure)    INCONCLUSIVE CAPTURE_FAILURE
18   baseline legitimately reconstructing state   TIE, baseline correct
===  ===========================================  ==========================

Case 18 is the one that keeps the benchmark honest. A baseline that works the
state out for itself is a *baseline success*, and the pipeline has to report it
as one. If this case ever started coming out as a Velra win, the verdict logic
would have acquired a thumb on the scale.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import pathlib
import shutil
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import aggregate  # type: ignore[no-redef]
    import safety  # type: ignore[no-redef]
    import telemetry  # type: ignore[no-redef]
    import verdict as verdict_mod  # type: ignore[no-redef]
    from mock_adapter import MockSpec, write_trial  # type: ignore[no-redef]
else:
    from . import aggregate, safety, telemetry
    from . import verdict as verdict_mod
    from .mock_adapter import MockSpec, write_trial

A = "A_cold_continuation"
B = "B_clear_survival"


def pair(pair_id: str, scenario: str, *, baseline: dict,
         velra: dict) -> list[MockSpec]:
    """Two specs that agree on every pairing invariant by construction."""
    common = dict(scenario=scenario, pair_id=pair_id)
    return [
        MockSpec(trial=f"{pair_id.replace('#', '-')}-baseline", arm="baseline",
                 **common, **baseline),
        MockSpec(trial=f"{pair_id.replace('#', '-')}-velra", arm="velra",
                 **common, **velra),
    ]


# A baseline that continues a huge conversation: every turn re-reads the whole
# thing out of cache, and it takes a while to find its feet.
HEAVY_BASELINE = dict(turns=7, input_tokens_per_turn=11_000,
                      cache_read_per_turn=310_000, cache_creation_per_turn=0,
                      reads_before_target=4, extra_searches=3,
                      uses_state=True, correct=True, ladder_rung=500_000,
                      synthetic_context_size=500_000, source_turns=18)

# A fresh session carrying a 600-token capsule.
LIGHT_VELRA = dict(turns=3, input_tokens_per_turn=3_200,
                   cache_read_per_turn=14_000, cache_creation_per_turn=1_800,
                   reads_before_target=0, extra_searches=0,
                   uses_state=True, correct=True, ladder_rung=500_000,
                   synthetic_context_size=500_000, source_turns=18)

EVEN = dict(turns=4, input_tokens_per_turn=5_000, cache_read_per_turn=60_000,
            cache_creation_per_turn=0, uses_state=True, correct=True,
            ladder_rung=250_000, synthetic_context_size=250_000,
            source_turns=18)


def cases() -> tuple[list[MockSpec], dict]:
    """Every scripted trial, and what the pipeline must say about each pair."""
    specs: list[MockSpec] = []
    expect: dict[str, dict] = {}

    # 1 -- a strong Velra win.
    specs += pair("case01-velra-win", A, baseline=HEAVY_BASELINE,
                  velra=LIGHT_VELRA)
    expect["case01-velra-win"] = {"verdict": verdict_mod.VELRA_WIN,
                                  "burden_below": -50.0}

    # 2 -- a strong baseline win: both correct, Velra somehow costs far more.
    specs += pair("case02-baseline-win", A,
                  baseline=dict(EVEN, input_tokens_per_turn=2_000,
                                cache_read_per_turn=10_000),
                  velra=dict(EVEN, input_tokens_per_turn=9_000,
                             cache_read_per_turn=120_000))
    expect["case02-baseline-win"] = {"verdict": verdict_mod.BASELINE_WIN}

    # 3 -- a tie: both correct, the burden within the materiality threshold.
    specs += pair("case03-tie", A, baseline=EVEN, velra=EVEN)
    expect["case03-tie"] = {"verdict": verdict_mod.TIE}

    # 4 -- invalid trials: one pair with an arm missing, one whose recorded
    #      identities disagree. Both must be dropped whole and named.
    specs.append(MockSpec(trial="case04-orphan-velra", scenario=A,
                          pair_id="case04-orphan", arm="velra", **EVEN))
    mismatched = pair("case04-mismatch", A, baseline=EVEN, velra=EVEN)
    mismatched[1] = dataclasses.replace(mismatched[1], model="a-different-model")
    specs += mismatched
    expect["case04-orphan"] = {"dropped": "INCOMPLETE_PAIR"}
    expect["case04-mismatch"] = {"dropped": "IDENTITY_MISMATCH"}

    # 5 -- missing telemetry: the cache fields are on some events and not
    #      others. A partial sum is not a total, so the pair is INCONCLUSIVE.
    specs += pair("case05-partial-telemetry", A,
                  baseline=dict(EVEN, cache_fields_missing_from=2),
                  velra=dict(EVEN, cache_fields_missing_from=2))
    expect["case05-partial-telemetry"] = {
        "verdict": verdict_mod.INCONCLUSIVE,
        "failure_class": "TELEMETRY_UNAVAILABLE",
        "metric_unavailable": ("velra", "cache_read_input_tokens")}

    # 6 -- malformed JSONL on both arms. The analysis survives and counts it.
    specs += pair("case06-malformed", A,
                  baseline=dict(EVEN, malformed_lines=3),
                  velra=dict(EVEN, malformed_lines=3))
    expect["case06-malformed"] = {"verdict": verdict_mod.TIE,
                                  "malformed_at_least": 3}

    # 7 -- duplicated events. Deduplicated, and the totals must match case 3's
    #      exactly: a duplicate that survived would inflate them.
    specs += pair("case07-duplicates", A,
                  baseline=dict(EVEN, duplicate_events=2),
                  velra=dict(EVEN, duplicate_events=2))
    expect["case07-duplicates"] = {"verdict": verdict_mod.TIE,
                                   "duplicates_removed": 2,
                                   "totals_match": "case03-tie"}

    # 8 and 9 -- the two cache conditions, which must not collapse into one.
    specs += pair("case08-cache-hit", A,
                  baseline=dict(EVEN, cache_hit=True),
                  velra=dict(EVEN, cache_hit=True))
    expect["case08-cache-hit"] = {"verdict": verdict_mod.TIE,
                                  "cache_condition": telemetry.CACHE_HIT}
    specs += pair("case09-cache-miss", A,
                  baseline=dict(EVEN, cache_hit=False),
                  velra=dict(EVEN, cache_hit=False))
    expect["case09-cache-miss"] = {"verdict": verdict_mod.TIE,
                                   "cache_condition": telemetry.CACHE_MISS}

    # 10 and 11 -- restore and injection, working. Same shape as case 1; the
    #      assertions are on the causal links rather than on the verdict.
    specs += pair("case10-restore-ok", B, baseline=HEAVY_BASELINE,
                  velra=dict(LIGHT_VELRA, transition="clear"))
    expect["case10-restore-ok"] = {"verdict": verdict_mod.VELRA_WIN,
                                   "links_pass": ("C_retained", "D_staged")}
    specs += pair("case11-injection-ok", B, baseline=HEAVY_BASELINE,
                  velra=dict(LIGHT_VELRA, transition="clear"))
    expect["case11-injection-ok"] = {"verdict": verdict_mod.VELRA_WIN,
                                     "links_pass": ("E_delivered", "F_received")}

    # 12 -- the capsule claimed twice. Exactly-once is the contract.
    specs += pair("case12-double-injection", A, baseline=EVEN,
                  velra=dict(EVEN, injections=2, claim_attempts=2,
                             claim_successes=2))
    expect["case12-double-injection"] = {
        "verdict": verdict_mod.INCONCLUSIVE,
        "failure_class": "DELIVERY_FAILURE",
        "broken_link": "velra/E_delivered"}

    # 13 -- staged for a different workspace.
    specs += pair("case13-wrong-workspace", A, baseline=EVEN,
                  velra=dict(EVEN, wrong_workspace=True))
    expect["case13-wrong-workspace"] = {
        "verdict": verdict_mod.INCONCLUSIVE,
        "failure_class": "STAGING_FAILURE",
        "broken_link": "velra/D_staged"}

    # 14 -- staged eight days ago, past the seven-day TTL.
    specs += pair("case14-stale-capsule", A, baseline=EVEN,
                  velra=dict(EVEN, stale_capsule=True))
    expect["case14-stale-capsule"] = {
        "verdict": verdict_mod.INCONCLUSIVE,
        "failure_class": "STAGING_FAILURE",
        "broken_link": "velra/D_staged"}

    # 15 -- the case the whole design turns on: a large token reduction and
    #       the wrong answer. Correctness outranks the saving.
    specs += pair("case15-cheap-and-wrong", A, baseline=HEAVY_BASELINE,
                  velra=dict(LIGHT_VELRA, correct=False))
    expect["case15-cheap-and-wrong"] = {"verdict": verdict_mod.BASELINE_WIN,
                                        "failure_class": "TASK_FAILURE",
                                        "burden_below": -50.0}

    # 16 -- no usage telemetry at all, on either arm.
    specs += pair("case16-no-telemetry", A,
                  baseline=dict(EVEN, emit_usage=False),
                  velra=dict(EVEN, emit_usage=False))
    expect["case16-no-telemetry"] = {
        "verdict": verdict_mod.INCONCLUSIVE,
        "failure_class": "TELEMETRY_UNAVAILABLE",
        "metric_unavailable": ("baseline", "input_tokens")}

    # 17 -- the ledger never held part of the state. A capture failure, which
    #       is a different defect from a delivery one and must say so.
    specs += pair("case17-capture-failure", A, baseline=EVEN,
                  velra=dict(EVEN,
                             ledger_missing_markers=("retry_backoff",)))
    expect["case17-capture-failure"] = {
        "verdict": verdict_mod.INCONCLUSIVE,
        "failure_class": "CAPTURE_FAILURE",
        "broken_link": "velra/C_retained"}

    # 18 -- the baseline works it out for itself. Not a scenario defect, not a
    #       Velra loss: a baseline success, reported as one.
    specs += pair("case18-baseline-reconstructs", B,
                  baseline=dict(EVEN, uses_state=True, correct=True,
                                extra_searches=4, extra_reads=3),
                  velra=dict(EVEN, uses_state=True, correct=True))
    expect["case18-baseline-reconstructs"] = {
        "verdict": verdict_mod.TIE,
        "baseline_correct": True,
        "velra_correct": True}

    return specs, expect


# --------------------------------------------------------------------------
# checking
# --------------------------------------------------------------------------


def _metric(evaluation: dict, name: str) -> dict:
    return (evaluation.get("metrics") or {}).get(name) or {}


def check(result: dict, expect: dict) -> list[str]:
    """Every expectation, against what the pipeline actually produced."""
    failures: list[str] = []
    verdicts = {v["pair_id"]: v for v in result["pairs"]}
    dropped = {d["pair_id"]: d for d in result["pairing"]["dropped"]}
    by_trial = {row["pair_id"]: row for row in result["trial_rows"]}

    for pair_id, want in sorted(expect.items()):
        if "dropped" in want:
            if pair_id not in dropped:
                failures.append(f"{pair_id}: expected to be dropped as "
                                f"{want['dropped']}, but it was not")
            elif dropped[pair_id]["reason"] != want["dropped"]:
                failures.append(f"{pair_id}: dropped as "
                                f"{dropped[pair_id]['reason']}, expected "
                                f"{want['dropped']}")
            continue

        got = verdicts.get(pair_id)
        if got is None:
            failures.append(f"{pair_id}: no verdict was produced")
            continue
        if got["verdict"] != want["verdict"]:
            failures.append(f"{pair_id}: verdict {got['verdict']}, expected "
                            f"{want['verdict']} ({got.get('why')})")
        if "failure_class" in want and got.get("failure_class") != want["failure_class"]:
            failures.append(f"{pair_id}: failure class "
                            f"{got.get('failure_class')}, expected "
                            f"{want['failure_class']}")
        if "broken_link" in want and got.get("broken_link") != want["broken_link"]:
            failures.append(f"{pair_id}: broken link {got.get('broken_link')}, "
                            f"expected {want['broken_link']}")
        if "burden_below" in want:
            change = got.get("burden_change_pct")
            if change is None or change > want["burden_below"]:
                failures.append(f"{pair_id}: burden change {change}, expected "
                                f"below {want['burden_below']}")
        for arm_key, flag in (("baseline_correct", "baseline"),
                              ("velra_correct", "velra")):
            if arm_key in want:
                actual = got["arms"][flag]["final_correctness"]
                if actual != want[arm_key]:
                    failures.append(f"{pair_id}: {arm_key}={actual}, expected "
                                    f"{want[arm_key]}")
        if "cache_condition" in want:
            for arm in ("baseline", "velra"):
                actual = got["arms"][arm]["cache_condition"]
                if actual != want["cache_condition"]:
                    failures.append(f"{pair_id}/{arm}: cache condition "
                                    f"{actual}, expected "
                                    f"{want['cache_condition']}")
        if "metric_unavailable" in want:
            arm, name = want["metric_unavailable"]
            cell = (got["burden"].get(name) or {}).get(arm) or {}
            if cell.get("measurement_status") != telemetry.STATUS_UNAVAILABLE:
                failures.append(
                    f"{pair_id}/{arm}: {name} is "
                    f"{cell.get('measurement_status')}, expected unavailable")
            if cell.get("value") is not None:
                failures.append(f"{pair_id}/{arm}: {name} has a value "
                                f"{cell.get('value')} while unavailable")
        if "links_pass" in want:
            chain = result["_chains"].get(
                verdicts[pair_id]["arms"]["velra"]["trial"], {})
            for link in want["links_pass"]:
                status = (chain.get("links") or {}).get(link, {}).get("status")
                if status != "pass":
                    failures.append(f"{pair_id}: link {link} is {status}, "
                                    f"expected pass")
        if "malformed_at_least" in want:
            for arm in ("baseline", "velra"):
                trial = got["arms"][arm]["trial"]
                capture = result["_captures"].get(trial, {})
                if capture.get("malformed_lines", 0) < want["malformed_at_least"]:
                    failures.append(
                        f"{pair_id}/{arm}: {capture.get('malformed_lines')} "
                        f"malformed lines counted, expected at least "
                        f"{want['malformed_at_least']}")
        if "duplicates_removed" in want:
            for arm in ("baseline", "velra"):
                trial = got["arms"][arm]["trial"]
                capture = result["_captures"].get(trial, {})
                if capture.get("duplicates_removed") != want["duplicates_removed"]:
                    failures.append(
                        f"{pair_id}/{arm}: removed "
                        f"{capture.get('duplicates_removed')} duplicates, "
                        f"expected {want['duplicates_removed']}")
        if "totals_match" in want:
            other = verdicts.get(want["totals_match"])
            if other is None:
                failures.append(f"{pair_id}: cannot compare totals with "
                                f"{want['totals_match']}")
            else:
                for arm in ("baseline", "velra"):
                    mine = (got["burden"]["total_input_tokens"][arm] or {}).get("value")
                    theirs = (other["burden"]["total_input_tokens"][arm] or {}).get("value")
                    if mine != theirs:
                        failures.append(
                            f"{pair_id}/{arm}: total {mine} differs from "
                            f"{want['totals_match']}'s {theirs}; a duplicate "
                            f"event survived deduplication")
    return failures


def structural_checks(result: dict) -> list[str]:
    """Properties that must hold whatever the scripted outcomes are."""
    failures = []
    for row in result["trial_rows"]:
        source = row["telemetry_source"]
        if source == telemetry.SOURCE_STRUCTURED_USAGE:
            failures.append(
                f"{row['pair_id']}/{row['arm']}: a synthetic trial reported "
                f"provenance {source!r}; the mock adapter must never produce "
                f"telemetry that looks like Claude's own")
        if row["achieved_context_size"] == row["synthetic_context_size"] \
                and row["synthetic_context_size"] != telemetry.STATUS_UNAVAILABLE:
            failures.append(
                f"{row['pair_id']}/{row['arm']}: achieved context size equals "
                f"the synthetic fixture size; the two must never be the same "
                f"reading")
    scraping = telemetry.scan_for_terminal_scraping(telemetry.package_sources())
    if scraping:
        failures.append(f"terminal scraping detected in this package: "
                        f"{scraping[:3]}")
    return failures


def run(root: pathlib.Path, verbose: bool = False) -> tuple[dict, list[str]]:
    specs, expect = cases()
    for spec in specs:
        write_trial(root / "trials", spec)

    result = aggregate.run(root / "trials", root, write=True)

    # Re-read what the pipeline wrote, rather than trusting in-memory state:
    # the artifacts are what a reader would see.
    result["_chains"] = {}
    result["_captures"] = {}
    for trial in sorted((root / "trials").iterdir()):
        chain = trial / "causal_chain.json"
        analysis = trial / "analysis.json"
        if chain.exists():
            result["_chains"][trial.name] = json.loads(
                chain.read_text(encoding="utf-8"))
        if analysis.exists():
            result["_captures"][trial.name] = json.loads(
                analysis.read_text(encoding="utf-8")).get("capture") or {}

    failures = check(result, expect) + structural_checks(result)
    if verbose:
        print(json.dumps(
            {v["pair_id"]: {"verdict": v["verdict"],
                            "class": v.get("failure_class"),
                            "pct": v.get("burden_change_pct")}
             for v in result["pairs"]}, indent=2))
    return result, failures


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--keep", default=None,
                    help="write the synthetic tree here and leave it behind")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    offline = safety.assert_offline(safety.MODE_SELFTEST)
    print(f"selftest: offline={offline['offline']}, "
          f"claude processes permitted={offline['claude_processes_permitted']}")

    tmp = None
    if args.keep:
        root = pathlib.Path(args.keep).resolve()
        shutil.rmtree(root, ignore_errors=True)
    else:
        tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(tmp.name) / "tokenburn-selftest"
    root.mkdir(parents=True, exist_ok=True)

    specs, expect = cases()
    print(f"building {len(specs)} synthetic trials across "
          f"{len(expect)} scripted cases ...", flush=True)
    result, failures = run(root, verbose=args.verbose)

    print()
    for pair_verdict in result["pairs"]:
        print(f"  {pair_verdict['pair_id']:<32} {pair_verdict['verdict']:<14} "
              f"{pair_verdict.get('failure_class') or ''}")
    for dropped in result["pairing"]["dropped"]:
        print(f"  {dropped['pair_id']:<32} DROPPED        "
              f"{dropped['reason']}")

    if failures:
        print("\nSELFTEST FAILED")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print(f"\nSELFTEST PASSED - {len(expect)} cases, "
          f"{result['n_trials']} trials, {result['pairing']['n_pairs']} pairs, "
          f"no Claude process and no network call.")
    if args.keep:
        print(f"synthetic tree left at {root}")
    if tmp:
        tmp.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
