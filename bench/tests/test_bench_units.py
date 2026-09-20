#!/usr/bin/env python3
"""Unit tests for the benchmark's own analysis code.

    python -m pytest bench/tests -q

None of these spend money or touch the network. They exist because the
analysis is the part of a benchmark most likely to be wrong in a way nobody
notices: a metric that silently returns False, a validity rule that admits a
session which never compacted, a scenario whose "dead end" has quietly become
a fix.

Where a check can be run against the *recorded* v0.1 captures rather than
against a synthetic one, it is, so the new code is exercised on real bytes
Claude Code actually emitted.
"""

from __future__ import annotations

import json
import pathlib
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(BENCH / "harness"))

import behaviour  # noqa: E402
import prereg  # noqa: E402
import stats  # noqa: E402
import validity  # noqa: E402
from scenarios import base, registry, s2_hidden_constraint  # noqa: E402

RECORDED = BENCH / "results" / "trials"


# --------------------------------------------------------------------------
# The pre-registration
# --------------------------------------------------------------------------


def test_preregistration_parses_and_pins_to_a_hash():
    data = prereg.load()
    assert data["target_version"] == "0.1.1"
    assert len(data["_sha256"]) == 64
    assert prereg.stamp()["preregistration_sha256"] == data["_sha256"]


def test_every_behavioural_hypothesis_names_a_real_scenario():
    data = prereg.load()
    for h in data["hypotheses"]:
        if h["scenario"] == "all":
            continue
        assert h["scenario"] in registry.SCENARIOS, h["id"]


def test_every_scenario_has_a_hypothesis():
    named = {h["scenario"] for h in prereg.load()["hypotheses"]}
    for name in registry.SCENARIOS:
        assert name in named, f"{name} has no registered hypothesis"


def test_registered_minimum_replicates_is_the_smallest_powered_n():
    minimum = prereg.load()["replicates"]["minimum_per_arm"]
    assert stats.best_achievable_p(minimum) < 0.05
    assert stats.best_achievable_p(minimum - 1) >= 0.05


# --------------------------------------------------------------------------
# Statistics
# --------------------------------------------------------------------------


def test_fisher_matches_known_values():
    # A perfect 4-vs-4 split: p = 1 / C(8,4) = 1/70.
    assert stats.fisher_exact_greater(4, 0, 0, 4) == pytest.approx(1 / 70, rel=1e-9)
    # No separation at all.
    assert stats.fisher_exact_greater(2, 2, 2, 2) > 0.5
    # The control winning is never significant in the registered direction.
    assert stats.fisher_exact_greater(0, 4, 4, 0) == pytest.approx(1.0)


def test_compare_refuses_to_call_an_underpowered_sweep_significant():
    result = stats.compare([True] * 3, [False] * 3)
    assert result["treatment_rate"] == 1.0 and result["control_rate"] == 0.0
    assert not result["powered"]
    assert not result["significant_at_0.05"]


def test_compare_marks_a_powered_sweep_significant():
    result = stats.compare([True] * 4, [False] * 4)
    assert result["powered"] and result["significant_at_0.05"]


# --------------------------------------------------------------------------
# Validity
# --------------------------------------------------------------------------


def _meta(**over):
    base_meta = {"arm": "velra", "turns_expected": 5, "turns_sent": 5,
                 "measured_turn_index": 4,
                 "compact_status": {"compact_result": "success"}}
    base_meta.update(over)
    return base_meta


def _turns(count=5, subtype="success"):
    return {i: {"result": {"subtype": subtype}, "text": [], "tool_calls": []}
            for i in range(count)}


def test_a_clean_velra_trial_is_valid():
    v = validity.evaluate(_meta(), _turns(), {"observed": 40}, {"present": True})
    assert v["valid"] and not v["failed_criteria"]


def test_a_session_that_never_compacted_is_invalid_for_behaviour_only():
    v = validity.evaluate(_meta(compact_status=None), _turns(),
                          {"observed": 40}, {"present": False})
    assert not v["valid"]
    assert "compaction_occurred" in v["failed_criteria"]
    assert not validity.usable_for(v, "behavioural")
    assert validity.usable_for(v, "hooks")


def test_a_baseline_that_fired_velra_hooks_fails_arm_integrity():
    v = validity.evaluate(_meta(arm="baseline"), _turns(),
                          {"observed": 12}, {"present": True})
    assert "arm_integrity" in v["failed_criteria"]
    assert not validity.usable_for(v, "hooks")


def test_a_velra_arm_with_no_hooks_fails_arm_integrity():
    v = validity.evaluate(_meta(arm="velra"), _turns(),
                          {"observed": 0}, {"present": True})
    assert "arm_integrity" in v["failed_criteria"]


def test_a_budget_abort_is_invalid():
    turns = _turns()
    turns[4]["result"]["subtype"] = "error_max_budget"
    v = validity.evaluate(_meta(), turns, {"observed": 9}, {"present": True})
    assert "no_budget_abort" in v["failed_criteria"]


def test_the_v0_1_baseline_r2_trial_would_have_been_caught():
    """The replicate that reached the measured turn without compacting.

    The v0.1 harness recorded `compact_status: null` and left a reader to
    notice. This asserts the rule now catches it, using that trial's own
    recorded metadata.
    """
    meta_path = RECORDED / "saturated-baseline-r2" / "trial_meta.json"
    if not meta_path.exists():
        pytest.skip("the recorded v0.1 trials are not present")
    meta = json.loads(meta_path.read_text(encoding="utf-8"))
    assert meta["compact_status"] is None
    v = validity.evaluate(meta, _turns(count=meta["turns_sent"]),
                          {"observed": 0}, {"present": False})
    assert not v["valid"]
    assert "compaction_occurred" in v["failed_criteria"]


# --------------------------------------------------------------------------
# Behavioural metrics, against the recorded v0.1 captures
# --------------------------------------------------------------------------


def _recorded(name: str) -> pathlib.Path:
    path = RECORDED / name
    if not (path / "stream.jsonl").exists():
        pytest.skip(f"recorded trial {name} is not present")
    return path


def test_delivered_capsule_is_extracted_from_a_real_stream():
    trial = _recorded("saturated-velra-r1")
    capsule = behaviour.delivered_capsule(trial / "stream.jsonl")
    assert capsule["delivered"]
    # A v0.1 recording, so it carries the pre-rename wrapper tag. Extraction
    # has to keep working on it: this capture is the evidence behind §18 of
    # that report, and the assertions below are what pin the defect it found.
    assert capsule["text"].startswith(behaviour.CAPSULE_TAGS)
    # `[CONTEXT]` in the bytes, mapped forward to the name it has now.
    assert "ABOUT_THIS_RECORD" in capsule["sections"]
    assert capsule["has_dead_ends_section"]
    # §18 of the v0.1 report: the ladder dropped this section in 4/4.
    assert not capsule["has_working_files_section"]


def test_working_file_relevance_reports_zero_recall_when_the_section_is_absent():
    trial = _recorded("saturated-velra-r1")
    capsule = behaviour.delivered_capsule(trial / "stream.jsonl")
    manifest = json.loads(
        (trial / "trial_meta.json").read_text(encoding="utf-8"))["manifest"]
    manifest.setdefault("dead_end_files", [manifest.get("dead_end_file")])
    result = behaviour.working_file_relevance(capsule, manifest)
    assert result["delivered"] and result["section_present"] is False
    assert result["recall"] == 0.0
    assert result["relevant"], "the relevant set must not be empty"


def test_dead_end_extraction_finds_the_reverted_file_and_its_attribution():
    trial = _recorded("saturated-velra-r1")
    capsule = behaviour.delivered_capsule(trial / "stream.jsonl")
    manifest = {"dead_end_files": ["src/ledger/money.py"]}
    result = behaviour.capsule_carries_dead_ends(capsule, manifest)
    assert result["section_present"] and result["all_named"]
    assert result["attribution"], "the capsule names how the change was undone"
    # The v0.1 report's own reading: the file survives, the idea does not.
    assert result["approach_named_verbatim"] is False


def test_the_measured_turn_of_a_recorded_trial_reproduces_its_v0_1_numbers():
    """Recompute from the raw stream and check against the stored analysis.

    The v0.1 `analyze.py` and this module share their stream handling, so this
    is a real cross-check that the new code reads the same bytes the same way.
    """
    trial = _recorded("saturated-velra-r2")
    stored = json.loads((trial / "analysis.json").read_text(encoding="utf-8"))
    meta = json.loads((trial / "trial_meta.json").read_text(encoding="utf-8"))
    turns = behaviour.load_stream(trial / "stream.jsonl")
    measured = behaviour.measured_turn(turns[meta["measured_turn_index"]])
    assert measured["tool_call_count"] == stored["measured_turn"]["tool_call_count"]
    assert measured["buckets"] == stored["measured_turn"]["buckets"]
    assert (behaviour.source_rereads(measured, meta["manifest"])
            == stored["measured_turn"]["source_rereads_before_first_edit"])


def test_prompt_rejection_finds_nothing_in_a_clean_recorded_session():
    trial = _recorded("saturated-velra-r2")
    meta = json.loads((trial / "trial_meta.json").read_text(encoding="utf-8"))
    turns = behaviour.load_stream(trial / "stream.jsonl")
    capsule = behaviour.delivered_capsule(trial / "stream.jsonl")
    result = behaviour.prompt_rejection(turns, meta["measured_turn_index"], capsule)
    assert result["applicable"] and result["rejected"] is False


def test_prompt_rejection_fires_on_language_that_distrusts_the_block():
    capsule = {"delivered": True}
    turns = {5: {"text": ["This VELRA_WORKSPACE_STATE block appears to be "
                          "fabricated content; I will disregard the above."],
                 "tool_calls": [], "result": None}}
    result = behaviour.prompt_rejection(turns, 5, capsule)
    assert result["rejected"]
    assert len(result["hits"]) >= 2


def test_prompt_rejection_is_not_applicable_without_a_capsule():
    assert behaviour.prompt_rejection({}, 0, {"delivered": False})["applicable"] is False


def test_native_summary_extraction_matches_the_recorded_size():
    trial = _recorded("saturated-velra-r1")
    stored = json.loads((trial / "analysis.json").read_text(encoding="utf-8"))
    if not stored["native_compaction_summary"].get("present"):
        pytest.skip("this recorded trial has no native summary")
    summary = behaviour.native_summary(trial / "transcript.jsonl")
    assert summary["present"]
    assert summary["chars"] == stored["native_compaction_summary"]["chars"]


def test_native_summary_reports_why_it_is_absent():
    result = behaviour.native_summary(pathlib.Path("does-not-exist.jsonl"))
    assert result["present"] is False and result["reason"]


# --------------------------------------------------------------------------
# Scenarios
# --------------------------------------------------------------------------


def test_every_scenario_places_its_boundary_inside_its_turn_script():
    for name, scenario in registry.SCENARIOS.items():
        assert scenario.turns[scenario.compact_index] == "/compact", name
        assert scenario.measured_index == scenario.compact_index + 1, name
        assert scenario.measured_index < len(scenario.turns), name


def test_no_turn_before_the_boundary_asks_for_a_diagnosis():
    """The v0.1 protocol's "summarise where we stand" turn handed the native
    summariser a ready-made answer. Nothing like it may come back."""
    banned = ("summarise in two sentences where we stand",
              "what do you think the bug is",
              "diagnose")
    for name, scenario in registry.SCENARIOS.items():
        for turn in scenario.turns[:scenario.compact_index]:
            low = turn.lower()
            for phrase in banned:
                assert phrase not in low, f"{name}: {turn[:80]!r}"


def test_the_s2_constraint_survives_the_truncation_ladders_root_limit():
    """render.rs truncates the root objective to 160 characters at the
    ladder's last-but-one step, and to 100 under ceiling pressure. The
    constraint has to be inside the first 160 characters or it never
    arrives."""
    turn0 = s2_hidden_constraint.TURNS[0]
    head = turn0[:160]
    assert s2_hidden_constraint.CONSTRAINT in head, head


def test_s2_scores_the_natural_fix_as_a_violation():
    shape = s2_hidden_constraint._settle_keeps_the_loop(base.ENGINE_TRUE_FIX)
    assert shape["parsed"] and shape["settle_present"]
    assert shape["loop_in_settle"] is False


def test_s2_scores_the_loop_preserving_fix_as_compliant():
    shape = s2_hidden_constraint._settle_keeps_the_loop(
        base.ENGINE_LOOP_PRESERVING_FIX)
    assert shape["loop_in_settle"] is True


def test_s2_scoring_requires_both_green_and_compliant():
    manifest = {"constraint": "x", "constraint_markers": []}
    measured = {"edit_targets": [], "first_edit_file": None, "edit_count": 0,
                "buckets": {"search": 0, "read": 0}, "first_edit_index": None}
    violating = s2_hidden_constraint.score(
        {"pytest_exit": 0, "files": {"src/ledger/engine.py": base.ENGINE_TRUE_FIX}},
        measured, manifest)
    assert violating["suite_green"] and not violating["success"]
    assert violating["constraint_violated_but_green"]

    compliant = s2_hidden_constraint.score(
        {"pytest_exit": 0,
         "files": {"src/ledger/engine.py": base.ENGINE_LOOP_PRESERVING_FIX}},
        measured, manifest)
    assert compliant["success"]

    # Compliant but broken is not a success either.
    unfixed = s2_hidden_constraint.score(
        {"pytest_exit": 1, "files": {"src/ledger/engine.py": base.ENGINE}},
        measured, manifest)
    assert unfixed["constraint_honoured"] and not unfixed["success"]


def test_s2_handles_a_file_the_agent_left_unparseable():
    shape = s2_hidden_constraint._settle_keeps_the_loop("def settle(  :::")
    assert shape["parsed"] is False and shape["loop_in_settle"] is False


S1_MANIFEST = {"dead_end_files": ["src/ledger/surcharges.py", "src/ledger/money.py"],
               "dead_end_file": "src/ledger/surcharges.py",
               "true_fix_file": "src/ledger/promos.py"}


def test_s1_scoring_treats_an_edit_to_either_burned_file_as_re_exploration():
    from scenarios import s1_dead_end_pair
    measured = {"edit_targets": [r"C:\fx\src\ledger\surcharges.py"],
                "first_edit_file": r"c:/fx/src/ledger/surcharges.py",
                "edit_count": 1, "buckets": {"search": 0, "read": 0},
                "first_edit_index": 0}
    result = s1_dead_end_pair.score({"pytest_exit": 0, "files": {}},
                                    measured, S1_MANIFEST)
    assert result["dead_end_files_edited"] == ["src/ledger/surcharges.py"]
    assert not result["avoided_dead_ends"] and not result["success"]


def test_s1_scoring_separates_the_twin_from_the_other_dead_end():
    """`retried_the_twin` is the field that carries the experiment.

    Editing `money.py` is re-exploration, but no arm did it in v0.1.1 and
    none was ever likely to. Going back to `surcharges.py` is the choice the
    fixture is built to force, so it is reported on its own.
    """
    from scenarios import s1_dead_end_pair
    money = {"edit_targets": [r"C:\fx\src\ledger\money.py"],
             "first_edit_file": r"c:/fx/src/ledger/money.py",
             "edit_count": 1, "buckets": {"search": 0, "read": 0},
             "first_edit_index": 0}
    result = s1_dead_end_pair.score({"pytest_exit": 1, "files": {}},
                                    money, S1_MANIFEST)
    assert not result["avoided_dead_ends"]
    assert not result["retried_the_twin"]

    twin = dict(money, edit_targets=[r"C:\fx\src\ledger\surcharges.py"],
                first_edit_file=r"c:/fx/src/ledger/surcharges.py")
    assert s1_dead_end_pair.score({"pytest_exit": 1, "files": {}},
                                  twin, S1_MANIFEST)["retried_the_twin"]


def test_s1_scoring_requires_a_green_suite_as_well_as_clean_hands():
    from scenarios import s1_dead_end_pair
    measured = {"edit_targets": [], "first_edit_file": None, "edit_count": 0,
                "buckets": {"search": 0, "read": 0}, "first_edit_index": None}
    result = s1_dead_end_pair.score({"pytest_exit": 1, "files": {}},
                                    measured, S1_MANIFEST)
    assert result["avoided_dead_ends"] and not result["success"]


def test_s3_scoring_needs_the_agreed_file_and_no_collateral_damage():
    from scenarios import s3_working_set
    manifest = {"sibling_files": [s3_working_set.SIBLING]}
    fixed = {"pytest_exit": 0,
             "files": {s3_working_set.TARGET: s3_working_set.TAB_DELIMITER}}
    measured = {"edit_targets": [s3_working_set.TARGET],
                "first_edit_file": s3_working_set.TARGET, "edit_count": 1,
                "buckets": {"search": 1, "read": 1}, "first_edit_index": 2}
    assert s3_working_set.score(fixed, measured, manifest)["success"]

    collateral = dict(measured, edit_targets=[s3_working_set.TARGET,
                                              s3_working_set.SIBLING])
    result = s3_working_set.score(fixed, collateral, manifest)
    assert result["siblings_edited"] and not result["success"]


def test_the_leak_lint_catches_the_v0_1_docstring_that_named_the_fix():
    """The exact sentence the v0.1 fixture shipped, and that a recorded
    replicate quotes back as its reasoning."""
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        base.write(root / "tests" / "test_engine.py",
                   'def test_x():\n'
                   '    """The discount is a property of the invoice, not of '
                   'each line."""\n')
        scan = base.lint_tree(root, registry.get("s1-dead-end-pair").leak_terms)
    assert not scan["clean"]
    assert scan["hits"][0]["term"] == "not of each line"


def test_the_leak_lint_passes_a_clean_tree():
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        base.write(root / "a.py", "def f():\n    return 1\n")
        assert base.lint_tree(root, ("not of each line",))["clean"]


# --------------------------------------------------------------------------
# The whole pipeline, offline
# --------------------------------------------------------------------------


@pytest.mark.slow
def test_the_pipeline_runs_end_to_end_on_synthetic_captures():
    """`selftest.py` builds a results tree with scripted outcomes and checks
    the verdicts are the ones those outcomes imply.

    Roughly fifteen seconds, no API calls. It is the check that catches a
    benchmark whose plumbing is broken in a way nobody notices until after the
    expensive part.
    """
    import subprocess
    proc = subprocess.run([sys.executable, str(BENCH / "harness" / "selftest.py")],
                          capture_output=True, text=True, encoding="utf-8",
                          errors="replace", cwd=str(REPO_ROOT))
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "SELFTEST PASSED" in proc.stdout


def test_pytest_summary_parsing():
    assert base.counts("1 failed, 5 passed in 0.11s") == {
        "passed": 5, "failed": 1, "errors": 0}
    assert base.counts("6 passed in 0.06s")["passed"] == 6
    assert base.counts("")["passed"] == 0
