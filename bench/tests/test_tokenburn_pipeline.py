"""The Token-Burn pipeline's own tests.

Everything here runs offline in under a few seconds. The properties checked are
the ones §16 of the Phase 3 specification names, and each test is written so
that its failure says which guarantee broke rather than which assertion did.
"""

from __future__ import annotations

import dataclasses
import json
import pathlib
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
# Imported as a package, not by putting its directory on the path. The archived
# harness has its own `prereg`, `verdict`, `selftest` and `scenarios`, and a
# flat import would hand whichever test ran first the wrong one.
sys.path.insert(0, str(BENCH))

from tokenburn import aggregate  # noqa: E402
from tokenburn import causal  # noqa: E402
from tokenburn import metrics  # noqa: E402
from tokenburn import pairing  # noqa: E402
from tokenburn import parse  # noqa: E402
from tokenburn import prereg  # noqa: E402
from tokenburn import report as report_mod  # noqa: E402
from tokenburn import scenarios  # noqa: E402
from tokenburn import selftest as selftest_mod  # noqa: E402
from tokenburn import telemetry  # noqa: E402
from tokenburn import verdict as verdict_mod  # noqa: E402
from tokenburn.mock_adapter import MockSpec, write_trial  # noqa: E402

A = "A_cold_continuation"
B = "B_clear_survival"


def spec(trial: str, arm: str, pair_id: str = "t#1", **kw) -> MockSpec:
    return MockSpec(trial=trial, scenario=kw.pop("scenario", A),
                    pair_id=pair_id, arm=arm, **kw)


def run_pair(tmp_path, baseline: MockSpec, velra: MockSpec) -> dict:
    trials = tmp_path / "trials"
    write_trial(trials, baseline)
    write_trial(trials, velra)
    return aggregate.run(trials, tmp_path, write=True)


# --------------------------------------------------------------------------
# the adapter feeds the real evaluator
# --------------------------------------------------------------------------


def test_mock_adapter_feeds_the_production_pipeline(tmp_path):
    """One evaluator, not two. The mock writes artifacts; production reads."""
    result = run_pair(tmp_path, spec("b", "baseline"), spec("v", "velra"))
    assert result["n_trials"] == 2
    assert result["pairing"]["n_pairs"] == 1
    # The artifacts the production stages write, next to the mock's own.
    for name in ("analysis.json", "causal_chain.json"):
        assert (tmp_path / "trials" / "v" / name).exists()
    assert (tmp_path / "aggregate.json").exists()
    assert (tmp_path / "verdicts.json").exists()


def test_there_is_no_second_evaluator():
    """No module in the package may define its own verdict vocabulary."""
    verdict_words = {"VELRA_WIN", "BASELINE_WIN", "TIE"}
    defining = []
    for path in telemetry.package_sources():
        if path.name in ("verdict.py", "selftest.py", "report.py"):
            continue
        text = path.read_text(encoding="utf-8")
        if any(f'{word} = "' in text for word in verdict_words):
            defining.append(path.name)
    assert not defining, f"a second verdict vocabulary lives in {defining}"


def test_mock_and_live_drivers_agree_on_the_artifact_set():
    live = (BENCH / "tokenburn" / "live_trial.py").read_text(encoding="utf-8")
    from tokenburn import mock_adapter
    for name in mock_adapter.TRIAL_ARTIFACTS + mock_adapter.VELRA_ONLY_ARTIFACTS:
        assert name in live, (
            f"the live driver never writes {name}, which the mock adapter "
            f"writes and the parser reads")


# --------------------------------------------------------------------------
# telemetry
# --------------------------------------------------------------------------


def test_synthetic_token_math_is_correct(tmp_path):
    turns, per_turn, read, creation = 5, 3_000, 40_000, 1_200
    trial = write_trial(tmp_path, spec("v", "velra", turns=turns,
                                       input_tokens_per_turn=per_turn,
                                       cache_read_per_turn=read,
                                       cache_creation_per_turn=creation))
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    m = evaluation["metrics"]
    assert m["input_tokens"]["value"] == turns * per_turn
    assert m["cache_read_input_tokens"]["value"] == turns * read
    assert m["cache_creation_input_tokens"]["value"] == turns * creation
    assert m["total_input_tokens"]["value"] == turns * (per_turn + read + creation)


def test_telemetry_provenance_survives_aggregation(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline"), spec("v", "velra"))
    cell = result["pairs"][0]["burden"]["input_tokens"]["velra"]
    assert cell["source"] == telemetry.SOURCE_SYNTHETIC_FIXTURE
    assert cell["measurement_status"] == telemetry.STATUS_MEASURED
    assert cell["raw_artifact_reference"].endswith("stream.jsonl")
    assert cell["unit"] == "tokens"
    # And it is still attached in the per-trial rows the report prints.
    row = next(r for r in result["trial_rows"] if r["arm"] == "velra")
    assert row["telemetry_source"] == telemetry.SOURCE_SYNTHETIC_FIXTURE


def test_unavailable_telemetry_becomes_inconclusive(tmp_path):
    result = run_pair(tmp_path,
                      spec("b", "baseline", emit_usage=False),
                      spec("v", "velra", emit_usage=False))
    pair = result["pairs"][0]
    assert pair["verdict"] == verdict_mod.INCONCLUSIVE
    assert pair["failure_class"] == "TELEMETRY_UNAVAILABLE"


def test_missing_cache_telemetry_stays_unavailable_and_never_zero(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra", emit_cache_fields=False))
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    cache = evaluation["metrics"]["cache_read_input_tokens"]
    assert cache["value"] is None
    assert cache["measurement_status"] == telemetry.STATUS_UNAVAILABLE
    assert cache["source"] == telemetry.SOURCE_UNAVAILABLE
    # And the derived total refuses rather than treating the gap as a zero.
    assert evaluation["metrics"]["total_input_tokens"]["value"] is None


def test_a_partially_present_field_is_not_partially_summed(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra", turns=5,
                                       cache_fields_missing_from=2))
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    cache = evaluation["metrics"]["cache_read_input_tokens"]
    assert cache["value"] is None
    assert "only 3 of 5" in cache["note"]


def test_cache_hit_and_cache_miss_are_distinct(tmp_path):
    hit = metrics.evaluate(parse.parse_trial(
        write_trial(tmp_path / "hit", spec("v", "velra", cache_hit=True))))
    miss = metrics.evaluate(parse.parse_trial(
        write_trial(tmp_path / "miss", spec("v", "velra", cache_hit=False))))
    assert hit["cache_condition"]["condition"] == telemetry.CACHE_HIT
    assert miss["cache_condition"]["condition"] == telemetry.CACHE_MISS


def test_unknown_cache_condition_is_never_expired(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra", emit_cache_fields=False))
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    cache = evaluation["cache_condition"]
    assert cache["condition"] == telemetry.CACHE_UNKNOWN
    assert "not EXPIRED" in cache["why"]
    assert "EXPIRED" not in {telemetry.CACHE_HIT, telemetry.CACHE_MISS,
                             telemetry.CACHE_UNKNOWN}


def test_a_metric_cannot_hold_a_value_while_unavailable():
    with pytest.raises(ValueError):
        telemetry.Metric("x", 5, "tokens", telemetry.SOURCE_UNAVAILABLE, None,
                         telemetry.STATUS_UNAVAILABLE)
    with pytest.raises(ValueError):
        telemetry.Metric("x", None, "tokens",
                         telemetry.SOURCE_STRUCTURED_USAGE, None,
                         telemetry.STATUS_MEASURED)


def test_arithmetic_over_an_absent_value_is_inconclusive():
    known = telemetry.Metric.measured("a", 100, "tokens",
                                      telemetry.SOURCE_STRUCTURED_USAGE, "x")
    absent = telemetry.Metric.unavailable("b", "tokens", "x")
    assert telemetry.difference("d", known, absent).measurement_status == \
        telemetry.STATUS_INCONCLUSIVE
    assert telemetry.percent_change("p", known, absent).value is None
    assert telemetry.total("t", [known, absent], "tokens", "x").value is None


def test_no_terminal_scraping_is_performed():
    hits = telemetry.scan_for_terminal_scraping(telemetry.package_sources())
    assert hits == [], f"presentation output is being pattern-matched: {hits}"


def test_the_scraping_guard_actually_catches_scraping(tmp_path):
    """A guard nobody has seen fire is a guard nobody knows works."""
    offender = tmp_path / "offender.py"
    offender.write_text(
        "import re\n"
        "def recover(stdout):\n"
        "    return re.search(r'cache_read_input_tokens: (\\d+)', stdout)\n",
        encoding="utf-8")
    hits = telemetry.scan_for_terminal_scraping([offender])
    assert hits and hits[0]["why"].startswith("regular expression")


# --------------------------------------------------------------------------
# capture health
# --------------------------------------------------------------------------


def test_malformed_jsonl_does_not_crash_analysis(tmp_path):
    result = run_pair(tmp_path,
                      spec("b", "baseline", malformed_lines=4),
                      spec("v", "velra", malformed_lines=4))
    assert result["pairs"][0]["verdict"] in (verdict_mod.TIE,
                                             verdict_mod.VELRA_WIN,
                                             verdict_mod.BASELINE_WIN)
    analysis = json.loads(
        (tmp_path / "trials" / "v" / "analysis.json").read_text(encoding="utf-8"))
    assert analysis["capture"]["malformed_lines"] >= 4
    assert analysis["capture"]["usable"] is True


def test_malformed_telemetry_does_not_crash_analysis(tmp_path):
    """A usage object whose fields are the wrong type is not a crash."""
    trial = write_trial(tmp_path, spec("v", "velra"))
    path = trial / "stream.jsonl"
    lines = path.read_text(encoding="utf-8").splitlines()
    lines.append(json.dumps({"type": "result", "uuid": "bad",
                             "usage": {"input_tokens": "lots",
                                       "output_tokens": None}}))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="")
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    assert evaluation["metrics"]["input_tokens"]["value"] is None
    assert evaluation["metrics"]["input_tokens"]["measurement_status"] == \
        telemetry.STATUS_UNAVAILABLE


def test_a_mostly_broken_capture_is_marked_unusable(tmp_path):
    result = run_pair(
        tmp_path, spec("b", "baseline"),
        spec("v", "velra", malformed_fraction_override=0.6))
    pair = result["pairs"][0]
    assert pair["verdict"] == verdict_mod.INCONCLUSIVE
    assert pair["failure_class"] == "UNUSABLE_CAPTURE"


def test_duplicate_events_are_deduplicated(tmp_path):
    clean = metrics.evaluate(parse.parse_trial(
        write_trial(tmp_path / "clean", spec("v", "velra"))))
    dupes = metrics.evaluate(parse.parse_trial(
        write_trial(tmp_path / "dupes", spec("v", "velra",
                                             duplicate_events=3))))
    assert dupes["capture"]["duplicates_removed"] == 3
    assert clean["metrics"]["total_input_tokens"]["value"] == \
        dupes["metrics"]["total_input_tokens"]["value"], (
        "a duplicated result event inflated the token total")


# --------------------------------------------------------------------------
# cross-session restore and delivery
# --------------------------------------------------------------------------


def test_cross_session_restore_is_represented_correctly(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra"))
    parsed = parse.parse_trial(trial)
    assert parsed.source_session_id
    assert parsed.destination_session_id
    assert parsed.source_session_id != parsed.destination_session_id
    chain = causal.evaluate(parsed, metrics.evaluate(parsed))
    assert chain["links"]["C_retained"]["status"] == "pass"
    assert chain["links"]["D_staged"]["status"] == "pass"
    staged = parsed.restore["staged"]
    assert staged["source_session_id"] == parsed.source_session_id
    assert staged["deliver_on"] == ["startup"]


def test_a_destination_that_is_the_source_fails_the_chain(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra", same_session_id=True))
    parsed = parse.parse_trial(trial)
    chain = causal.evaluate(parsed, metrics.evaluate(parsed))
    assert chain["links"]["F_received"]["status"] == "fail"
    assert "not a new session" in chain["links"]["F_received"]["why"]


def test_capsule_delivery_is_represented_correctly(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra"))
    parsed = parse.parse_trial(trial)
    assert len(parsed.deliveries) == 1
    delivery = parsed.deliveries[0]
    assert delivery.on_startup
    assert delivery.hook_event == "SessionStart"
    assert parse.CAPSULE_OPEN in delivery.text


def test_a_replayed_transcript_voids_the_comparison(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra",
                                       old_transcript_replayed=True))
    parsed = parse.parse_trial(trial)
    chain = causal.evaluate(parsed, metrics.evaluate(parsed))
    assert chain["links"]["F_received"]["status"] == "fail"


def test_the_baseline_arm_has_no_velra_links(tmp_path):
    trial = write_trial(tmp_path, spec("b", "baseline"))
    parsed = parse.parse_trial(trial)
    chain = causal.evaluate(parsed, metrics.evaluate(parsed))
    for link in causal.VELRA_ONLY:
        assert chain["links"][link]["status"] == "n/a"
    assert chain["first_broken_link"] is None


# --------------------------------------------------------------------------
# verdicts
# --------------------------------------------------------------------------


HEAVY = dict(turns=7, input_tokens_per_turn=11_000, cache_read_per_turn=310_000)
LIGHT = dict(turns=3, input_tokens_per_turn=3_200, cache_read_per_turn=14_000)


def test_velra_wins_are_reported_correctly(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline", **HEAVY),
                      spec("v", "velra", **LIGHT))
    pair = result["pairs"][0]
    assert pair["verdict"] == verdict_mod.VELRA_WIN
    assert pair["burden_change_pct"] < -prereg.material_burden_pct()
    assert pair["arms"]["velra"]["final_correctness"] is True


def test_baseline_wins_are_reported_correctly(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline", **LIGHT),
                      spec("v", "velra", **HEAVY))
    assert result["pairs"][0]["verdict"] == verdict_mod.BASELINE_WIN


def test_ties_are_reported_correctly(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline"), spec("v", "velra"))
    assert result["pairs"][0]["verdict"] == verdict_mod.TIE


def test_token_reduction_without_correctness_is_not_a_win(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline", **HEAVY),
                      spec("v", "velra", correct=False, **LIGHT))
    pair = result["pairs"][0]
    assert pair["verdict"] == verdict_mod.BASELINE_WIN
    assert pair["failure_class"] == "TASK_FAILURE"
    # The saving is still reported: it is the most informative row here.
    assert pair["burden_change_pct"] < -prereg.material_burden_pct()


def test_a_baseline_that_reconstructs_the_state_is_a_baseline_success(tmp_path):
    result = run_pair(tmp_path,
                      spec("b", "baseline", scenario=B, uses_state=True,
                           correct=True, extra_searches=5),
                      spec("v", "velra", scenario=B, uses_state=True,
                           correct=True))
    pair = result["pairs"][0]
    assert pair["arms"]["baseline"]["final_correctness"] is True
    assert pair["verdict"] == verdict_mod.TIE


@pytest.mark.parametrize("kwargs,expected_class,expected_link", [
    ({"ledger_missing_markers": ("retry_backoff",)}, "CAPTURE_FAILURE",
     "velra/C_retained"),
    ({"wrong_workspace": True}, "STAGING_FAILURE", "velra/D_staged"),
    ({"stale_capsule": True}, "STAGING_FAILURE", "velra/D_staged"),
    ({"injections": 2, "claim_attempts": 2, "claim_successes": 2},
     "DELIVERY_FAILURE", "velra/E_delivered"),
    ({"capsule_markers_present": False}, "RECEIPT_FAILURE", "velra/F_received"),
    ({"staged": False}, "STAGING_FAILURE", "velra/D_staged"),
])
def test_causal_chain_failures_become_inconclusive(tmp_path, kwargs,
                                                   expected_class,
                                                   expected_link):
    result = run_pair(tmp_path, spec("b", "baseline"),
                      spec("v", "velra", **kwargs))
    pair = result["pairs"][0]
    assert pair["verdict"] == verdict_mod.INCONCLUSIVE
    assert pair["failure_class"] == expected_class
    assert pair["broken_link"] == expected_link


def test_a_leaky_scenario_is_inconclusive_on_both_arms(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline", leak_clean=False),
                      spec("v", "velra", leak_clean=False))
    pair = result["pairs"][0]
    assert pair["verdict"] == verdict_mod.INCONCLUSIVE
    assert pair["failure_class"] == "SCENARIO_LEAK"


def test_a_short_source_session_is_not_a_cold_continuation(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline", source_turns=4),
                      spec("v", "velra", source_turns=4))
    pair = result["pairs"][0]
    assert pair["verdict"] == verdict_mod.INCONCLUSIVE
    assert pair["failure_class"] == "NO_LARGE_STATE"


# --------------------------------------------------------------------------
# pairing
# --------------------------------------------------------------------------


def test_a_half_pair_is_dropped_whole(tmp_path):
    trials = tmp_path / "trials"
    write_trial(trials, spec("v", "velra", pair_id="lonely#1"))
    result = aggregate.run(trials, tmp_path, write=True)
    assert result["pairing"]["n_pairs"] == 0
    assert result["pairing"]["dropped"][0]["reason"] == "INCOMPLETE_PAIR"


def test_mismatched_identity_drops_the_pair(tmp_path):
    trials = tmp_path / "trials"
    write_trial(trials, spec("b", "baseline"))
    write_trial(trials, dataclasses.replace(spec("v", "velra"),
                                            model="another-model"))
    result = aggregate.run(trials, tmp_path, write=True)
    assert result["pairing"]["n_pairs"] == 0
    dropped = result["pairing"]["dropped"][0]
    assert dropped["reason"] == "IDENTITY_MISMATCH"
    assert dropped["differences"][0]["field"] == "model"


def test_every_registered_invariant_is_actually_compared():
    key = {name: index for index, name in
           enumerate(pairing.prereg.pair_invariants())}
    for name in key:
        other = dict(key, **{name: "changed"})
        assert any(d["field"] == name for d in pairing.mismatch(key, other)), (
            f"{name} is registered as a pair invariant but is not compared")


# --------------------------------------------------------------------------
# qualification and formal stages
# --------------------------------------------------------------------------


def test_qualification_and_formal_metrics_both_work(tmp_path):
    """Two stages, one pipeline. The stage is a label, not a code path."""
    trials = tmp_path / "trials"
    for stage, count in (("q", 2), ("f", 4)):
        for index in range(1, count + 1):
            pair_id = f"{A}#{stage}{index}"
            write_trial(trials, spec(f"{A}-{stage}{index}-baseline", "baseline",
                                     pair_id=pair_id, **HEAVY))
            write_trial(trials, spec(f"{A}-{stage}{index}-velra", "velra",
                                     pair_id=pair_id, **LIGHT))
    result = aggregate.run(trials, tmp_path, write=True)
    assert result["pairing"]["n_pairs"] == 6
    group = result["pooled"]["groups"][A]
    assert group["verdict_counts"][verdict_mod.VELRA_WIN] == 6
    assert group["total_input_tokens_pct_change"]["n_measured_pairs"] == 6
    assert group["total_input_tokens_pct_change"]["median"] < -50


def test_the_registered_plan_is_two_then_four_pairs():
    plan = prereg.load()["trial_plan"]
    assert plan["qualification"]["pairs_per_benchmark"] == 2
    assert plan["formal"]["pairs_per_benchmark"] == 4
    assert plan["formal"]["total_pairs"] == 8
    assert plan["formal"]["total_sessions"] == 16
    assert plan["qualification"]["counts_toward_scorecard"] is False


# --------------------------------------------------------------------------
# reporting
# --------------------------------------------------------------------------


def test_the_report_never_prints_an_absent_value_as_zero(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline", emit_usage=False),
                      spec("v", "velra", emit_usage=False))
    row = next(r for r in result["trial_rows"] if r["arm"] == "velra")
    assert row["input_tokens"] == telemetry.STATUS_UNAVAILABLE
    assert row["cache_read_input_tokens"] == telemetry.STATUS_UNAVAILABLE
    # Every token column of an untelemetered trial reads as a status word.
    # `| 0 |` does appear in the table and legitimately so: the first correct
    # action was taken at step zero, which is a measurement, not a gap.
    for column in ("input_tokens", "output_tokens", "cache_read_input_tokens",
                   "cache_creation_input_tokens", "total_input_tokens"):
        assert row[column] in (telemetry.STATUS_UNAVAILABLE,
                               telemetry.STATUS_INCONCLUSIVE), column
    markdown = report_mod.markdown(result)
    assert "unavailable" in markdown


def test_every_required_report_column_is_produced(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline"), spec("v", "velra"))
    row = result["trial_rows"][0]
    for column in report_mod.TRIAL_COLUMNS:
        assert column in row, f"§17 requires the column {column!r}"


def test_a_proxy_is_never_labelled_as_a_measurement(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra"))
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    capsule = evaluation["metrics"]["capsule_tokens"]
    assert capsule["measurement_status"] == telemetry.STATUS_PROXY
    assert "not a tokenizer measurement" in capsule["note"]
    row = report_mod.trial_row(evaluation, {"causal_validity": "DEFERRED"})
    assert str(row["capsule_tokens"]).startswith("~")


def test_a_synthetic_context_size_is_never_an_observation(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra",
                                       synthetic_context_size=900_000))
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    m = evaluation["metrics"]
    assert m["synthetic_context_size"]["value"] == 900_000
    assert m["synthetic_context_size"]["source"] == \
        telemetry.SOURCE_SYNTHETIC_FIXTURE
    # An estimated fixture size is a proxy, never a measurement: it is a
    # character count divided by a declared ratio.
    assert m["synthetic_context_size"]["measurement_status"] == \
        telemetry.STATUS_PROXY
    assert "NOT a Claude observation" in m["synthetic_context_size"]["note"]
    assert m["achieved_context_size"]["value"] is None
    assert m["achieved_context_size"]["measurement_status"] == \
        telemetry.STATUS_UNAVAILABLE


def test_an_observed_context_size_is_reported_when_the_runtime_emits_one(tmp_path):
    trial = write_trial(tmp_path, spec("v", "velra",
                                       emit_context_observation=True,
                                       observed_context_size=187_000))
    evaluation = metrics.evaluate(parse.parse_trial(trial))
    assert evaluation["metrics"]["achieved_context_size"]["value"] == 187_000


# --------------------------------------------------------------------------
# the selftest itself
# --------------------------------------------------------------------------


def test_the_selftest_covers_every_required_case():
    _, expect = selftest_mod.cases()
    required = {
        "case01-velra-win", "case02-baseline-win", "case03-tie",
        "case04-orphan", "case04-mismatch", "case05-partial-telemetry",
        "case06-malformed", "case07-duplicates", "case08-cache-hit",
        "case09-cache-miss", "case10-restore-ok", "case11-injection-ok",
        "case12-double-injection", "case13-wrong-workspace",
        "case14-stale-capsule", "case15-cheap-and-wrong",
        "case16-no-telemetry", "case17-capture-failure",
        "case18-baseline-reconstructs",
    }
    assert required <= set(expect)


@pytest.mark.slow
def test_the_selftest_passes(tmp_path):
    _, failures = selftest_mod.run(tmp_path / "selftest")
    assert failures == []


# --------------------------------------------------------------------------
# telemetry coverage: the expected set of usage-bearing records
# --------------------------------------------------------------------------


def _stream(tmp_path, *events, name="t") -> pathlib.Path:
    trial = tmp_path / name
    trial.mkdir(parents=True, exist_ok=True)
    lines = [json.dumps({"_velra_bench": "turn_start", "turn": 0, "text": "go"})]
    lines += [json.dumps(e) for e in events]
    (trial / "stream.jsonl").write_text("\n".join(lines) + "\n",
                                        encoding="utf-8", newline="")
    (trial / "trial_meta.json").write_text(
        json.dumps({"scenario": A, "arm": "velra"}), encoding="utf-8",
        newline="")
    return trial


def _usage(**kw):
    base = {"input_tokens": 1000, "output_tokens": 50,
            "cache_read_input_tokens": 9000,
            "cache_creation_input_tokens": 0}
    base.update(kw)
    return base


def test_assistant_and_result_usage_are_not_added_together(tmp_path):
    """The defect this guards is a silent 2x on a realistic capture.

    Claude Code reports one turn's spend twice -- on the assistant event that
    carries the message and again on the result event that closes the turn.
    Summing both reported exactly double the true input, and not by the same
    factor on both arms, because the number of assistant messages per turn
    varies with how much tool use a session did.
    """
    trial = _stream(
        tmp_path,
        {"type": "assistant", "uuid": "a1", "session_id": "s",
         "message": {"id": "m1", "role": "assistant",
                     "content": [{"type": "text", "text": "hi"}],
                     "usage": _usage()}},
        {"type": "result", "subtype": "success", "uuid": "r1",
         "session_id": "s", "usage": _usage()})
    parsed = parse.parse_trial(trial)
    assert parsed.usage_selection["selected_class"] == parse.USAGE_CLASS_RESULT
    assert parsed.usage_selection["available_classes"] == {
        "assistant": 1, "result": 1}
    m = metrics.evaluate(parsed)["metrics"]
    assert m["input_tokens"]["value"] == 1000, "one turn used 1000, not 2000"
    assert m["total_input_tokens"]["value"] == 10_000


def test_assistant_usage_is_used_when_no_result_event_carries_any(tmp_path):
    trial = _stream(
        tmp_path,
        {"type": "assistant", "uuid": "a1", "session_id": "s",
         "message": {"id": "m1", "role": "assistant", "content": [],
                     "usage": _usage()}},
        {"type": "result", "subtype": "success", "uuid": "r1",
         "session_id": "s"})
    parsed = parse.parse_trial(trial)
    assert parsed.usage_selection["selected_class"] == parse.USAGE_CLASS_ASSISTANT
    assert metrics.evaluate(parsed)["metrics"]["input_tokens"]["value"] == 1000


def test_records_without_a_usage_object_are_not_expected_telemetry(tmp_path):
    """A hook response, a tool result and a turn marker are not missing
    telemetry. They are simply not usage-bearing records."""
    trial = _stream(
        tmp_path,
        {"type": "system", "subtype": "hook_response", "uuid": "h1",
         "hook_name": "SessionStart:startup", "exit_code": 0, "stdout": "{}"},
        {"type": "user", "uuid": "u1", "session_id": "s",
         "message": {"role": "user",
                     "content": [{"type": "tool_result",
                                  "tool_use_id": "x", "content": "ok"}]}},
        {"type": "result", "subtype": "success", "uuid": "r1",
         "session_id": "s", "usage": _usage()})
    parsed = parse.parse_trial(trial)
    assert parsed.usage_selection["expected_record_count"] == 1
    m = metrics.evaluate(parsed)["metrics"]
    assert m["input_tokens"]["measurement_status"] == telemetry.STATUS_MEASURED
    assert m["input_tokens"]["value"] == 1000


def test_completeness_is_judged_within_the_selected_class_only(tmp_path):
    """An assistant event missing the cache fields must not make the result
    class incomplete -- it is not in the expected set."""
    trial = _stream(
        tmp_path,
        {"type": "assistant", "uuid": "a1", "session_id": "s",
         "message": {"id": "m1", "role": "assistant", "content": [],
                     "usage": {"input_tokens": 1000, "output_tokens": 50}}},
        {"type": "result", "subtype": "success", "uuid": "r1",
         "session_id": "s", "usage": _usage()})
    m = metrics.evaluate(parse.parse_trial(trial))["metrics"]
    assert m["cache_read_input_tokens"]["value"] == 9000
    assert m["total_input_tokens"]["value"] == 10_000


def test_a_missing_field_in_the_selected_class_stays_unavailable(tmp_path):
    trial = _stream(
        tmp_path,
        {"type": "result", "subtype": "success", "uuid": "r1",
         "session_id": "s", "usage": _usage()},
        {"type": "result", "subtype": "success", "uuid": "r2",
         "session_id": "s",
         "usage": {"input_tokens": 1000, "output_tokens": 50}})
    m = metrics.evaluate(parse.parse_trial(trial))["metrics"]
    cache = m["cache_read_input_tokens"]
    assert cache["value"] is None
    assert cache["measurement_status"] == telemetry.STATUS_UNAVAILABLE
    assert "only 1 of 2" in cache["note"]


def test_a_trial_with_no_usage_of_any_class_is_unavailable(tmp_path):
    trial = _stream(tmp_path,
                    {"type": "result", "subtype": "success", "uuid": "r1",
                     "session_id": "s"})
    parsed = parse.parse_trial(trial)
    assert parsed.usage_selection["selected_class"] is None
    m = metrics.evaluate(parsed)["metrics"]
    assert m["input_tokens"]["value"] is None
    assert m["input_tokens"]["measurement_status"] == telemetry.STATUS_UNAVAILABLE


def test_the_usage_selection_is_recorded_in_the_analysis(tmp_path):
    result = run_pair(tmp_path, spec("b", "baseline"), spec("v", "velra"))
    analysis = json.loads(
        (tmp_path / "trials" / "v" / "analysis.json").read_text(encoding="utf-8"))
    selection = analysis["usage_selection"]
    assert selection["selected_class"]
    assert "why" in selection and selection["expected_record_count"] > 0
