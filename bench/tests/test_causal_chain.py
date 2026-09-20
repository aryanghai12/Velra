"""Unit tests for the v0.1.2 causal-chain machinery.

Everything here runs offline against synthetic captures. The point is that each
stage of the chain can fail *on its own*, and that the classification names the
right one -- a benchmark that cannot tell a capture failure from a delivery
failure is the thing this release exists to replace.
"""

from __future__ import annotations

import json
import pathlib
import sqlite3
import sys

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "bench"))
sys.path.insert(0, str(REPO_ROOT / "bench" / "harness"))

import stages  # noqa: E402
from scenarios import leaks, registry  # noqa: E402
from scenarios.targets import TargetFact  # noqa: E402
from scenario_aggregate import PAIR_INVARIANTS, pair_key_mismatch, pair_up  # noqa: E402


# --------------------------------------------------------------------------
# Target facts
# --------------------------------------------------------------------------

FACT = TargetFact(
    id="f1",
    what="the burned twin",
    probe="Which module did we revert?",
    recalled_markers=("surcharges",),
    ledger_table="dead_ends",
    ledger_sql="SELECT 1 FROM dead_ends WHERE id <= ?1",
    capsule_markers=("surcharges.py",),
    necessary_because="x" * 100,
)


def test_a_probe_forbids_tools_and_offers_an_out():
    q = FACT.question()
    assert "Do not use any tools" in q
    assert "UNKNOWN" in q
    assert FACT.probe in q


def test_a_marker_in_the_answer_counts_as_recall():
    assert FACT.recalled("It was src/ledger/surcharges.py")["recalled"] is True


def test_unknown_is_never_recall_even_with_a_marker():
    # A hedged answer that names the file *and* disclaims it must not score:
    # the question is whether the fact survived, not whether the word did.
    got = FACT.recalled("UNKNOWN - possibly surcharges, but I cannot tell")
    assert got["said_unknown"] is True
    assert got["recalled"] is False


def test_an_answer_that_used_a_tool_is_discarded():
    got = FACT.recalled("<TOOL:Read> src/ledger/surcharges.py")
    assert got["used_tool"] is True
    assert got["recalled"] is False


def test_a_miss_is_reported_as_a_miss():
    got = FACT.recalled("I no longer have that detail.")
    assert got["recalled"] is False
    assert got["markers_found"] == []


def test_capsule_presence_is_case_and_separator_insensitive():
    assert FACT.in_capsule(r"- src\ledger\Surcharges.py | 1 edit")["present"] is True
    assert FACT.in_capsule("nothing relevant")["present"] is False
    assert FACT.in_capsule(None) == {"target_fact": "f1", "delivered": False,
                                     "present": False, "markers_found": []}


# --------------------------------------------------------------------------
# Leak scanning
# --------------------------------------------------------------------------


def _tree(tmp_path: pathlib.Path, files: dict) -> pathlib.Path:
    for rel, text in files.items():
        p = tmp_path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8", newline="")
    return tmp_path


def test_a_clean_tree_and_prompt_set_passes(tmp_path):
    repo = _tree(tmp_path, {"src/a.py": "def f():\n    return 1\n"})
    got = leaks.scan(repo, ["do the thing", "/compact", "fix it"], 2,
                     ["the answer is rounding"], env={})
    assert got["clean"] is True
    assert got["hits"] == []


def test_a_leak_in_a_file_is_fatal(tmp_path):
    repo = _tree(tmp_path, {"src/a.py": "# TODO: the answer is rounding\n"})
    got = leaks.scan(repo, ["go"], 0, ["the answer is rounding"], env={})
    assert got["clean"] is False
    assert got["fatal_hits"][0]["surface"] == "tree"


def test_a_leak_in_a_filename_is_fatal(tmp_path):
    # Paths are read as often as contents, and the original scan ignored them.
    repo = _tree(tmp_path, {"src/fix_the_rounding.py": "pass\n"})
    got = leaks.scan(repo, ["go"], 0, ["fix_the_rounding"], env={})
    assert got["clean"] is False
    assert {h["surface"] for h in got["fatal_hits"]} == {"filenames"}


def test_a_leak_in_the_measured_prompt_is_fatal(tmp_path):
    repo = _tree(tmp_path, {"src/a.py": "pass\n"})
    got = leaks.scan(repo, ["set up", "/compact", "now fix the rounding"], 2,
                     ["fix the rounding"], env={})
    assert got["clean"] is False
    assert got["fatal_hits"][0]["surface"] == "post_prompt"


def test_a_term_stated_once_before_the_boundary_is_reported_but_allowed(tmp_path):
    # The turn script is *supposed* to say the target aloud once. That is the
    # setup; destroying it is what the boundary is for.
    repo = _tree(tmp_path, {"src/a.py": "pass\n"})
    got = leaks.scan(repo, ["we will fix the rounding", "/compact", "now do it"],
                     2, ["fix the rounding"], env={})
    assert got["clean"] is True
    assert got["hits_by_surface"] == {"pre_prompts": 1}


def test_an_environment_leak_is_fatal(tmp_path):
    repo = _tree(tmp_path, {"src/a.py": "pass\n"})
    got = leaks.scan(repo, ["go"], 0, ["surcharges"],
                     env={"VELRA_ANSWER": "src/ledger/surcharges.py"})
    assert got["clean"] is False
    assert got["fatal_hits"][0]["surface"] == "environment"


def test_the_path_env_var_does_not_trip_the_scan(tmp_path):
    repo = _tree(tmp_path, {"src/a.py": "pass\n"})
    got = leaks.scan(repo, ["go"], 0, ["bin"], env={"PATH": "/usr/bin:/bin"})
    assert got["clean"] is True


# --------------------------------------------------------------------------
# Stage 1 -- compaction loss
# --------------------------------------------------------------------------


def _control(recalled_each, valid_each=None):
    valid_each = valid_each or [True] * len(recalled_each)
    return {"target_facts": {"f1": [
        {"recalled": r, "valid": v, "answer": "x"}
        for r, v in zip(recalled_each, valid_each)]},
        "reduction_pct_median": 60.0, "compaction_is_lossy": True}


def test_a_fact_forgotten_by_a_majority_counts_as_lost():
    got = stages.stage_compaction_loss(_control([False, False, True]))
    assert got["status"] == "pass"
    assert got["per_fact"]["f1"]["lost"] is True


def test_a_fact_the_baseline_still_recalls_makes_the_scenario_untestable():
    got = stages.stage_compaction_loss(_control([True, True, False]))
    assert got["status"] == "fail"
    assert "cannot attribute" in got["reason"]


def test_an_exact_split_is_not_a_loss():
    # Majority, strictly. Two out of four is not evidence that the information
    # went away, and this is the direction to be conservative in.
    got = stages.stage_compaction_loss(_control([False, False, True, True]))
    assert got["status"] == "fail"


def test_invalid_control_runs_do_not_count_toward_the_majority():
    got = stages.stage_compaction_loss(
        _control([False, True, True], valid_each=[True, False, False]))
    assert got["per_fact"]["f1"]["valid_replicates"] == 1
    assert got["status"] == "pass"


def test_a_missing_control_blocks_the_chain_rather_than_passing_it():
    got = stages.stage_compaction_loss(None)
    assert got["status"] == "n/a"
    assert got["blocks_chain"] is True


def test_a_control_that_probed_no_target_fact_blocks_the_chain():
    got = stages.stage_compaction_loss({"target_facts": {}})
    assert got["status"] == "n/a"
    assert got["blocks_chain"] is True


# --------------------------------------------------------------------------
# Stage 2 -- capture
# --------------------------------------------------------------------------

SCHEMA = """
CREATE TABLE events (id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL);
CREATE TABLE constraints (
  id INTEGER PRIMARY KEY, text TEXT, kind TEXT, cue TEXT,
  prompt_ordinal INTEGER, source_event_id INTEGER, superseded_ms INTEGER);
CREATE TABLE checkpoints (
  checkpoint_id TEXT, session_id TEXT, epoch INTEGER, created_ms INTEGER,
  "trigger" TEXT, event_watermark INTEGER, partial INTEGER,
  capsule_tokens_est INTEGER, capsule TEXT);
"""


def _ledger(trial: pathlib.Path, watermark: int, constraint_event_id: int | None):
    trial.mkdir(parents=True, exist_ok=True)
    con = sqlite3.connect(trial / "velra.db")
    con.executescript(SCHEMA)
    con.executemany("INSERT INTO events (id, ts_ms) VALUES (?, ?)",
                    [(i, 1000 + i) for i in range(1, 50)])
    con.execute(
        'INSERT INTO checkpoints VALUES (?,?,?,?,?,?,?,?,?)',
        ("ckpt_1", "s", 1, 5000, "auto", watermark, 0, 700, "<capsule/>"))
    if constraint_event_id is not None:
        con.execute(
            "INSERT INTO constraints (id, text, kind, cue, prompt_ordinal, "
            "source_event_id, superseded_ms) VALUES (1, ?, 'labelled', "
            "'constraint', 0, ?, NULL)",
            ("settle must keep iterating invoice.items", constraint_event_id))
    con.commit()
    con.close()


CONSTRAINT_FACT = {
    "id": "c1", "what": "the turn-0 constraint", "probe": "quote it",
    "recalled_markers": ["invoice.items"], "ledger_table": "constraints",
    "ledger_sql": ("SELECT id FROM constraints WHERE superseded_ms IS NULL "
                   "AND LOWER(text) LIKE '%invoice.items%' "
                   "AND source_event_id <= ?1"),
    "capsule_markers": ["invoice.items"], "necessary_because": "x" * 100,
}


def test_state_recorded_before_the_watermark_is_captured(tmp_path):
    _ledger(tmp_path, watermark=30, constraint_event_id=12)
    got = stages.stage_capture(tmp_path, "velra", [CONSTRAINT_FACT])
    assert got["status"] == "pass"
    assert got["per_fact"]["c1"]["captured"] is True
    assert got["per_fact"]["c1"]["event_watermark"] == 30


def test_state_recorded_after_the_watermark_is_not_captured(tmp_path):
    # The row exists, but it landed after the checkpoint was taken, so the
    # capsule could not have carried it. Counting it would turn a real miss
    # into a pass.
    _ledger(tmp_path, watermark=10, constraint_event_id=40)
    got = stages.stage_capture(tmp_path, "velra", [CONSTRAINT_FACT])
    assert got["status"] == "fail"
    assert got["per_fact"]["c1"]["captured"] is False


def test_a_missing_row_is_a_capture_failure_not_a_delivery_one(tmp_path):
    _ledger(tmp_path, watermark=30, constraint_event_id=None)
    got = stages.stage_capture(tmp_path, "velra", [CONSTRAINT_FACT])
    assert got["status"] == "fail"
    assert "CAPTURE failure" in got["reason"]


def test_the_baseline_arm_has_no_ledger_to_check(tmp_path):
    got = stages.stage_capture(tmp_path, "baseline", [CONSTRAINT_FACT])
    assert got["status"] == "n/a"
    assert got["blocks_chain"] is False


def test_a_velra_trial_with_no_database_fails_capture(tmp_path):
    got = stages.stage_capture(tmp_path, "velra", [CONSTRAINT_FACT])
    assert got["status"] == "fail"


# --------------------------------------------------------------------------
# Stage 3 -- delivery
# --------------------------------------------------------------------------


def _stream(trial: pathlib.Path, rows: list[dict]) -> pathlib.Path:
    trial.mkdir(parents=True, exist_ok=True)
    path = trial / "stream.jsonl"
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n",
                    encoding="utf-8", newline="")
    return path


def _hook_response(capsule: str, hook_name: str = "SessionStart:compact") -> dict:
    return {
        "type": "system", "subtype": "hook_response", "hook_name": hook_name,
        "hook_event": "SessionStart", "exit_code": 0,
        "stdout": json.dumps({"hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": capsule}}),
    }


CAPSULE = ('<VELRA_WORKSPACE_STATE v="1" checkpoint="ckpt_1" captured="x" '
           'trigger="auto">\n[FILE_ACTIVITY] (OBSERVED)\n'
           '- src/ledger/surcharges.py | edited 1x, read 2x\n'
           '[RECORD_DETAIL]\n</VELRA_WORKSPACE_STATE>')


def test_a_delivery_that_precedes_the_status_line_still_counts_as_post_boundary(tmp_path):
    # Measured on the real v0.1.1 captures: the SessionStart:compact hook
    # response lands one line *before* `compact_result: success`, because the
    # hook is part of how compaction finishes. Ordering alone would score every
    # real delivery as pre-boundary.
    _stream(tmp_path, [
        {"type": "user"},
        _hook_response(CAPSULE),
        {"type": "system", "subtype": "status", "compact_result": "success"},
    ])
    events, boundary = stages.delivery_events(tmp_path / "stream.jsonl")
    assert boundary == 2
    assert len(events) == 1
    assert events[0]["after_compaction"] is True


def test_a_delivery_before_any_compaction_is_not_post_boundary(tmp_path):
    _stream(tmp_path, [
        _hook_response(CAPSULE, hook_name="UserPromptSubmit"),
        {"type": "user"},
    ])
    events, boundary = stages.delivery_events(tmp_path / "stream.jsonl")
    assert boundary is None
    assert events[0]["after_compaction"] is False


def _delivery(tmp_path, capsule=CAPSULE, tokens=770, control_delta=0,
              deliveries=1):
    rows = [{"type": "user"}]
    rows += [_hook_response(capsule) for _ in range(deliveries)]
    rows += [{"type": "system", "subtype": "status", "compact_result": "success"}]
    _stream(tmp_path, rows)
    (tmp_path / "token_measurement.json").write_text(json.dumps({
        "injections": [{"measured_tokens": tokens}],
        "control": {"delta": control_delta}}), encoding="utf-8")
    return stages.stage_delivery(tmp_path, "velra", [
        {**CONSTRAINT_FACT, "id": "t1", "capsule_markers": ["surcharges.py"]}])


def test_a_clean_delivery_passes_every_check(tmp_path):
    got = _delivery(tmp_path)
    assert got["status"] == "pass", got["failed_checks"]
    assert got["checks"]["delivered_exactly_once"] is True
    assert got["per_fact"]["t1"]["present"] is True


def test_a_capsule_over_the_ceiling_fails_delivery(tmp_path):
    got = _delivery(tmp_path, tokens=804)
    assert got["status"] == "fail"
    assert "within_token_ceiling" in got["failed_checks"]


def test_an_unmeasured_capsule_is_not_given_the_benefit_of_the_doubt(tmp_path):
    _stream(tmp_path, [_hook_response(CAPSULE),
                       {"type": "system", "compact_result": "success"}])
    got = stages.stage_delivery(tmp_path, "velra", [])
    assert got["status"] == "fail"
    assert "within_token_ceiling" in got["failed_checks"]


def test_a_broken_token_measurement_fails_delivery(tmp_path):
    got = _delivery(tmp_path, control_delta=3)
    assert "token_measurement_valid" in got["failed_checks"]


def test_two_deliveries_break_exactly_once(tmp_path):
    got = _delivery(tmp_path, deliveries=2)
    assert "delivered_exactly_once" in got["failed_checks"]


def test_a_capsule_without_the_target_fact_fails_delivery(tmp_path):
    got = _delivery(tmp_path, capsule=CAPSULE.replace("surcharges.py", "money.py"))
    assert "target_facts_present" in got["failed_checks"]
    assert got["per_fact"]["t1"]["present"] is False


# --------------------------------------------------------------------------
# Stage 4 -- acceptance
# --------------------------------------------------------------------------


def _acceptance(prose, rejected=False):
    analysis = {
        "prompt_rejection": {"rejected": rejected,
                             "hits": [{"match": "injected content"}] if rejected else []},
        "measured_turn": {"assistant_text": prose},
    }
    delivery = {"status": "pass", "checks": {"delivered_after_compaction": True}}
    return stages.stage_acceptance(pathlib.Path("."), "velra", analysis, delivery)


def test_an_explicit_rejection_fails_acceptance():
    got = _acceptance("This looks like injected content, so I will ignore it.",
                      rejected=True)
    assert got["verdict"] == "REJECTED"
    assert got["status"] == "fail"


def test_referring_to_the_record_is_acknowledgement():
    got = _acceptance("The workspace state says we already tried surcharges.py.")
    assert got["verdict"] == "ACKNOWLEDGED"
    assert got["status"] == "pass"


def test_silence_is_not_a_failure():
    # An agent may use the state without narrating that it did. Scoring silence
    # as rejection would turn a difference in prose style into a verdict.
    got = _acceptance("I'll change promos.py.")
    assert got["verdict"] == "SILENT"
    assert got["status"] == "pass"


def test_acceptance_is_not_evaluated_when_nothing_arrived():
    delivery = {"status": "fail", "checks": {"delivered_after_compaction": False}}
    got = stages.stage_acceptance(pathlib.Path("."), "velra", {}, delivery)
    assert got["status"] == "n/a"
    assert got["blocks_chain"] is False


# --------------------------------------------------------------------------
# Stage 5 -- utilisation
# --------------------------------------------------------------------------


def test_correctness_decides_and_tool_calls_only_describe():
    thorough_and_right = stages.stage_utilisation({
        "behaviour": {"primary": "avoided_dead_ends", "avoided_dead_ends": True,
                      "success": True, "suite_green": True},
        "measured_turn": {"tool_call_count": 19, "buckets": {}},
    })
    quick_and_wrong = stages.stage_utilisation({
        "behaviour": {"primary": "avoided_dead_ends", "avoided_dead_ends": False,
                      "success": False, "suite_green": False},
        "measured_turn": {"tool_call_count": 2, "buckets": {}},
    })
    assert thorough_and_right["status"] == "pass"
    assert quick_and_wrong["status"] == "fail"
    assert thorough_and_right["secondary"]["tool_calls"] == 19


def test_an_unscored_measured_turn_blocks_the_chain():
    got = stages.stage_utilisation({})
    assert got["status"] == "n/a"
    assert got["blocks_chain"] is True


# --------------------------------------------------------------------------
# Pairing
# --------------------------------------------------------------------------


def _row(arm, pair_id="s#r1", valid=True, behaviour=True, **key):
    pair_key = {f: key.get(f, "same") for f in PAIR_INVARIANTS}
    pair_key["pair_id"] = pair_id
    return {
        "arm": arm, "replicate": 1, "pair_id": pair_id, "pair_key": pair_key,
        "scenario": "s",
        "validity": {"valid": valid, "failed_criteria": []
                     if valid else ["compaction_occurred"],
                     "excluded_from": [] if valid else ["behavioural"]},
        "behaviour": {"primary": "p", "p": True, "success": True}
        if behaviour else None,
    }


def test_a_matched_pair_is_kept():
    got = pair_up([_row("velra")], [_row("baseline")])
    assert got["report"]["n_pairs"] == 1
    assert got["report"]["symmetric"] is True


def test_an_unmatched_arm_drops_the_whole_pair_and_is_named():
    # The v0.1.1 defect: one invalid baseline, and the aggregate compared 4
    # Velra trials against 3 baselines as though the arms had been matched.
    got = pair_up([_row("velra")], [_row("baseline", valid=False)])
    assert got["report"]["n_pairs"] == 0
    assert got["report"]["dropped_pairs"][0]["pair_id"] == "s#r1"
    assert got["report"]["dropped_pairs"][0]["because"][0]["arm"] == "baseline"


def test_a_missing_arm_drops_the_pair():
    got = pair_up([_row("velra")], [])
    assert got["report"]["n_pairs"] == 0
    assert got["report"]["dropped_pairs"][0]["because"][0]["reason"] == \
        "no trial on disk"
    assert got["report"]["symmetric"] is False


@pytest.mark.parametrize("field", [f for f in PAIR_INVARIANTS if f != "pair_id"])
def test_any_disagreeing_invariant_rejects_the_pair(field):
    got = pair_up([_row("velra", **{field: "A"})],
                  [_row("baseline", **{field: "B"})])
    assert got["report"]["n_pairs"] == 0
    mismatch = got["report"]["dropped_pairs"][0]["because"][0]["fields"]
    assert [m["field"] for m in mismatch] == [field]


def test_a_trial_without_pair_identity_is_refused_rather_than_guessed():
    velra, base = _row("velra"), _row("baseline")
    base.pop("pair_key")
    got = pair_up([velra], [base])
    assert got["report"]["n_pairs"] == 0
    assert got["report"]["dropped_pairs"][0]["because"][0]["fields"][0][
        "reason"] == "absent"


def test_pairing_is_symmetric_across_many_replicates():
    velra = [_row("velra", pair_id=f"s#r{i}") for i in range(1, 5)]
    base = [_row("baseline", pair_id=f"s#r{i}") for i in range(1, 5)]
    got = pair_up(velra, base)
    assert got["report"]["n_pairs"] == 4
    assert got["report"]["paired_ids"] == [f"s#r{i}" for i in range(1, 5)]
    assert got["report"]["symmetric"] is True
    # And the reverse direction gives the same pairs: nothing depends on which
    # arm was listed first.
    assert pair_up(base, velra)["report"]["paired_ids"] == \
        got["report"]["paired_ids"]


def test_pair_key_mismatch_lists_every_disagreeing_field():
    got = pair_key_mismatch(
        _row("velra", model="sonnet", claude_version="1"),
        _row("baseline", model="opus", claude_version="2"))
    assert sorted(m["field"] for m in got) == ["claude_version", "model"]


# --------------------------------------------------------------------------
# Scenario declarations
# --------------------------------------------------------------------------


def test_every_scenario_declares_at_least_one_target_fact():
    for name, scenario in registry.SCENARIOS.items():
        assert scenario.target_facts, f"{name} makes no causal claim"


def test_every_target_fact_is_fully_specified():
    for name, scenario in registry.SCENARIOS.items():
        for fact in scenario.target_facts:
            assert fact.recalled_markers, f"{name}/{fact.id}: no recall markers"
            assert fact.capsule_markers, f"{name}/{fact.id}: no capsule markers"
            assert "?1" in fact.ledger_sql, \
                f"{name}/{fact.id}: ledger SQL must bind the watermark"
            assert len(fact.necessary_because) > 80, \
                f"{name}/{fact.id}: necessity is not argued"


def test_fixture_seeds_are_stable_and_distinct():
    seeds = {n: s.fixture_seed() for n, s in registry.SCENARIOS.items()}
    assert len(set(seeds.values())) == len(seeds), seeds
    assert seeds == {n: s.fixture_seed() for n, s in registry.SCENARIOS.items()}


def test_no_measured_prompt_mentions_its_own_target_fact():
    # The post-compaction turn must be identical across arms and must not hand
    # the answer to either of them.
    for name, scenario in registry.SCENARIOS.items():
        prompt = scenario.turns[scenario.measured_index].lower()
        for fact in scenario.target_facts:
            for marker in fact.recalled_markers:
                assert marker.lower() not in prompt, \
                    f"{name}: the measured turn names {marker!r}"


def test_s3_offers_three_indistinguishable_candidates():
    from scenarios import s3_working_set as s3
    assert len(s3.TARGET_SIBLINGS) == 2
    assert s3.TARGET not in s3.TARGET_SIBLINGS


def test_s3_scores_repairing_every_candidate_as_an_unresolved_reference():
    from scenarios import s3_working_set as s3
    manifest = {"sibling_files": list(s3.TARGET_SIBLINGS)}
    final = {"files": {s3.TARGET: s3.TAB_DELIMITER}, "pytest_exit": 0}
    shotgun = s3.score(
        final,
        {"edit_targets": [s3.TARGET, *s3.TARGET_SIBLINGS], "edit_count": 3,
         "first_edit_file": s3.TARGET, "first_edit_index": 0,
         "buckets": {"search": 1, "read": 3}},
        manifest)
    assert shotgun["target_fixed"] is True
    assert shotgun["repaired_every_candidate"] is True
    assert shotgun["resolved_the_reference"] is False
    assert shotgun["success"] is False

    precise = s3.score(
        final,
        {"edit_targets": [s3.TARGET], "edit_count": 1,
         "first_edit_file": s3.TARGET, "first_edit_index": 0,
         "buckets": {"search": 0, "read": 1}},
        manifest)
    assert precise["resolved_the_reference"] is True
    assert precise["success"] is True


def test_the_v1_preregistration_is_untouched():
    # Every artifact under bench/results/v0.1.1 carries this hash. Changing the
    # file would make those results uninterpretable.
    import prereg
    assert prereg.digest(prereg.PATH) == (
        "4658a1a538411e0f15bf9c5d57ee50ede85bcd2d7929b8c7045262732c2eb1ad")


def test_the_v2_preregistration_registers_every_stage_and_hypothesis():
    import prereg
    v2 = prereg.load_v2()
    assert [s["name"] for s in v2["causal_chain"]["stages"]] == [
        name.upper() for name in stages.STAGES]
    scenarios = {h["scenario"] for h in v2["hypotheses"]}
    assert set(registry.SCENARIOS) <= scenarios
    assert set(v2["possible_outcomes"]["values"]) >= {
        "BASELINE WINS", "VELRA WINS", "TIE", "INCONCLUSIVE"}
