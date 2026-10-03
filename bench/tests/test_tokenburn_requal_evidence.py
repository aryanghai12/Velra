"""The committed v0.1.2 requalification evidence agrees with itself.

`bench/results/v0.1.2-requal/` is the benchmark evidence the documentation
cites. These tests read only its tracked, derived files -- the raw captures are
not in git -- and check that the aggregate, the verdicts, the per-trial
analyses, the causal chains and the trial metadata tell one story, and that the
current pipeline, re-applied to the committed analyses, still reaches the
committed verdicts. A change that would silently re-score the evidence fails
here first.
"""

from __future__ import annotations

import json
import pathlib
import statistics
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
sys.path.insert(0, str(BENCH))

from tokenburn import pairing  # noqa: E402
from tokenburn import prereg  # noqa: E402
from tokenburn import report as report_mod  # noqa: E402
from tokenburn import verdict as verdict_mod  # noqa: E402

ROOT = BENCH / "results" / "v0.1.2-requal"
PREREG_SHA = "c13150b6da58a0861a6319986b7a18c0e0d3659c043adba8ac44c6af326ffcd2"
RUN_HEAD = "51b96cb5fd03e1bcba9e4c5a5140727ddbe8055c"


def load(path: pathlib.Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def evidence() -> dict:
    agg = load(ROOT / "aggregate.json")
    trials = {}
    for row in agg["trial_rows"]:
        name = f"{row['scenario']}-{row['pair_id'].split('#')[1]}-{row['arm']}"
        d = ROOT / "trials" / name
        trials[name] = {"analysis": load(d / "analysis.json"),
                        "chain": load(d / "causal_chain.json"),
                        "meta": load(d / "trial_meta.json"),
                        "dir": d}
    return {"aggregate": agg, "verdicts": load(ROOT / "verdicts.json"),
            "trials": trials}


def test_the_evidence_is_stamped_with_the_registered_rules(evidence):
    assert prereg.digest() == PREREG_SHA
    for doc in (evidence["aggregate"], evidence["verdicts"],
                evidence["aggregate"]["pooled"]):
        assert doc["preregistration_id"] == "velra-tokenburn"
        assert doc["preregistration_version"] == "1.1.0"
        assert doc["preregistration_sha256"] == PREREG_SHA
    assert "v1.1.0" in (ROOT / "report.md").read_text(encoding="utf-8")


def test_the_run_is_complete_and_nothing_was_dropped(evidence):
    agg = evidence["aggregate"]
    assert agg["n_trials"] == 8 and len(evidence["trials"]) == 8
    assert agg["pairing"] == {"n_pairs": 4, "dropped": [], "unassigned": []}
    assert agg["invalid_trials"] == []
    state = load(ROOT / "run_state.json")["trials"]
    assert len(state) == 8
    assert {t["state"] for t in state.values()} == {"complete"}


def test_verdicts_json_and_the_aggregate_are_the_same_result(evidence):
    assert evidence["verdicts"]["pairs"] == evidence["aggregate"]["pairs"]
    assert evidence["verdicts"]["pooled"] == evidence["aggregate"]["pooled"]
    assert evidence["verdicts"]["scored_by"] == evidence["aggregate"]["scored_by"]


def test_every_trial_ran_on_the_recorded_identity(evidence):
    for trial in evidence["trials"].values():
        key = trial["meta"]["pair_key"]
        assert key["git_head"] == RUN_HEAD
        assert key["claude_version"].startswith("2.1.280")
        assert key["model"] == "sonnet"
        assert key["context_ladder_rung"] == 250_000


def test_every_trial_was_isolated_and_handed_off_validly(evidence):
    for name, trial in evidence["trials"].items():
        meta = trial["meta"]
        assert meta["trial_validity"] == {"valid": True,
                                          "invalidation_reason": None}, name
        isolation = meta["memory_isolation"]
        for when in ("before_source", "before_destination"):
            controls = isolation["effective"][when]
            assert controls["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] == "1"
            assert controls["settings"]["autoMemoryEnabled"] is False
            assert controls["settings"]["autoMemoryDirectory"] is None
            assert controls["auto_memory_disabled"] is True
        for when in ("before_source", "before_destination", "after_destination"):
            assert isolation["scans"][when]["clean"] is True, (name, when)
        assert meta["source_handoff_valid"] is True
        assert meta["target_status_at_handoff"] == "FAIL"
        assert meta["old_transcript_replayed"] is False
        assert meta["source_session_id"] != meta["destination_session_id"]


def test_the_pairs_agree_with_the_per_trial_artifacts(evidence):
    trials = evidence["trials"]
    for pair in evidence["aggregate"]["pairs"]:
        for arm in ("baseline", "velra"):
            cell = pair["arms"][arm]
            trial = trials[cell["trial"]]
            analysis, chain = trial["analysis"], trial["chain"]
            assert cell["final_correctness"] == analysis["final_correctness"]["correct"]
            assert cell["causal_validity"] == chain["causal_validity"]
            m = analysis["metrics"]
            total = (m["input_tokens"]["value"]
                     + m["cache_read_input_tokens"]["value"]
                     + m["cache_creation_input_tokens"]["value"])
            assert m["total_input_tokens"]["value"] == total
            assert pair["burden"]["total_input_tokens"][arm]["value"] == total
        b = pair["burden"]["total_input_tokens"]["baseline"]["value"]
        v = pair["burden"]["total_input_tokens"]["velra"]["value"]
        assert pair["burden_change_pct"] == pytest.approx((v - b) / b * 100,
                                                          abs=1e-3)


def test_every_verdict_follows_the_registered_order(evidence):
    """Re-derived here from correctness and burden alone, independently."""
    threshold = prereg.material_burden_pct()
    for pair in evidence["aggregate"]["pairs"]:
        b_ok = pair["arms"]["baseline"]["final_correctness"]
        v_ok = pair["arms"]["velra"]["final_correctness"]
        change = pair["burden_change_pct"]
        if b_ok and not v_ok:
            expected = verdict_mod.BASELINE_WIN
        elif v_ok and change <= -threshold:
            expected = verdict_mod.VELRA_WIN
        elif b_ok and change >= threshold:
            expected = verdict_mod.BASELINE_WIN
        elif b_ok and v_ok:
            expected = verdict_mod.TIE
        else:
            expected = verdict_mod.INCONCLUSIVE
        assert pair["verdict"] == expected, pair["pair_id"]
        if pair["verdict"] == verdict_mod.TIE:
            assert b_ok and v_ok


def test_the_current_pipeline_reaches_the_committed_verdicts(evidence):
    trials = evidence["trials"]
    evaluations = [t["analysis"] for t in trials.values()]
    chains = {name: t["chain"] for name, t in trials.items()}
    paired = pairing.pair_up(evaluations)
    rescored = [verdict_mod.pair_verdict(pair, chains) for pair in paired["pairs"]]
    assert rescored == evidence["aggregate"]["pairs"]
    assert verdict_mod.pool(rescored) == evidence["aggregate"]["pooled"]
    rows = [report_mod.trial_row(t["analysis"], t["chain"])
            for t in trials.values()]
    key = lambda r: (r["pair_id"], r["arm"])  # noqa: E731
    assert sorted(rows, key=key) == sorted(evidence["aggregate"]["trial_rows"],
                                           key=key)


def test_the_pooled_summary_recomputes(evidence):
    pairs = evidence["aggregate"]["pairs"]
    for name, group in evidence["aggregate"]["pooled"]["groups"].items():
        mine = [p for p in pairs if p["benchmark"] == name]
        pct = group["total_input_tokens_pct_change"]
        assert pct["n_measured_pairs"] == len(mine)
        assert pct["median"] == round(statistics.median(
            p["burden_change_pct"] for p in mine), 3)
        assert group["correctness"]["of"] == len(mine)


def test_velra_arms_hold_the_mechanism_and_baselines_break_only_at_h(evidence):
    for name, trial in evidence["trials"].items():
        links = trial["chain"]["links"]
        assert links["A_large_state"]["status"] == "pass"
        assert links["B_absent_natively"]["status"] == "pass"
        if name.endswith("-velra"):
            for link in ("C_retained", "D_staged", "E_delivered",
                         "F_received", "G_used", "H_correct"):
                assert links[link]["status"] == "pass", (name, link)
            restore = load(trial["dir"] / "velra_restore.json")
            staged = restore["staged"]
            assert staged["intent"] == "new_session"
            assert "startup" in staged["deliver_on"]
            assert staged["tokens"] <= prereg.capsule_token_ceiling()
            assert restore["ledger_evidence"]["markers_missing"] == []
        else:
            assert trial["chain"]["first_broken_link"] == "H_correct"
            assert not (trial["dir"] / "velra_restore.json").exists()


def test_raw_captures_are_listed_by_hash_and_not_committed():
    manifest = load(ROOT / "raw_captures.sha256.json")
    assert manifest["n_files"] == len(manifest["files"]) > 0
    names = {f["path"].rsplit("/", 1)[-1] for f in manifest["files"]}
    assert {"stream.jsonl", "source_stream.jsonl", "transcript.jsonl"} <= names
    assert all(len(f["sha256"]) == 64 for f in manifest["files"])
