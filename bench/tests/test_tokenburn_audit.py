"""Pre-qualification audit checks: provenance, readiness authority, arm
isolation, Benchmark B transition semantics and capsule provenance.

Each test here pins a finding from the audit that preceded the qualification
run. They are separate from `test_tokenburn_safety.py` because they are not
about whether the benchmark can spend money — they are about whether what it
would measure would mean anything.
"""

from __future__ import annotations

import ast
import json
import pathlib
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
TOKENBURN = BENCH / "tokenburn"
sys.path.insert(0, str(BENCH))

from tokenburn import capsule_probe  # noqa: E402
from tokenburn import live_trial  # noqa: E402
from tokenburn import prereg  # noqa: E402
from tokenburn import run as run_mod  # noqa: E402
from tokenburn import safety  # noqa: E402
from tokenburn import scenarios  # noqa: E402


def _function(tree: ast.Module, name: str) -> ast.FunctionDef:
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == name:
            return node
    raise AssertionError(f"{name} not found")


def _source(name: str) -> str:
    return (TOKENBURN / name).read_text(encoding="utf-8")


# --------------------------------------------------------------------------
# 1. provenance
# --------------------------------------------------------------------------


def test_the_gate_does_not_fail_on_its_own_output():
    """Producing readiness.json must not be what makes the tree dirty.

    Before this, the dry run wrote the artifact, `git status --porcelain` then
    listed it, and the artifact reported the tree as dirty *because it
    existed*. A gate that fails on its own output can never pass, and the
    blocker it named was not one a reader could act on.
    """
    assert run_mod.is_self_written("?? bench/results/v0.1.2/readiness.json")
    assert run_mod.is_self_written(
        "?? bench/results/v0.1.2/tokenburn/trials/x/analysis.json")
    for line in (" M crates/velra/src/hook.rs",
                 "?? bench/tokenburn/run.py",
                 " M bench/results/v0.1.2/aggregate.json",
                 "?? bench/results/v0.1.2/hardened/readiness.json"):
        assert not run_mod.is_self_written(line), (
            f"{line!r} is a real change and must still count")


def test_a_rename_is_judged_on_its_destination():
    assert run_mod.is_self_written(
        "R  bench/old.json -> bench/results/v0.1.2/tokenburn/new.json")
    assert not run_mod.is_self_written(
        "R  bench/results/v0.1.2/tokenburn/old.json -> bench/kept.json")


def test_provenance_reports_both_counts():
    info = run_mod.git_provenance()
    assert len(info["git_head"]) == 40
    assert info["changes_including_self_written"] >= len(
        info["working_tree_changes"])
    assert "self_written_paths_excluded" in info
    assert info["self_written_patterns"]


def test_every_trial_records_the_commit_it_is_evidence_about():
    text = _source("live_trial.py")
    # Takes the run's own output paths to exclude (--results-root inside the
    # repository); called with no argument it behaves as before.
    assert "def repo_provenance(exclude: tuple = ())" in text
    assert '"repo_provenance": provenance' in text
    assert '"git_head": provenance["git_head"]' in text


def test_the_pair_key_includes_the_commit():
    """Two arms run against different source must not be able to pair."""
    assert "git_head" in prereg.pair_invariants()


def test_the_readiness_gate_checks_the_binary_against_head():
    tree = ast.parse(_source("run.py"))
    phase = _function(tree, "phase_provenance")
    body = ast.unparse(phase)
    assert "binary_provenance.collect" in body
    assert "was built from HEAD" in body


# --------------------------------------------------------------------------
# 2. readiness artifact authority
# --------------------------------------------------------------------------


def test_exactly_one_current_readiness_artifact_exists():
    results = BENCH / "results" / "v0.1.2"
    current = sorted(p.name for p in results.glob("readiness*.json"))
    assert current == ["readiness.json"], (
        f"more than one readiness artifact sits at the top of {results}: "
        f"{current}")


def test_the_current_artifact_declares_itself_authoritative():
    path = BENCH / "results" / "v0.1.2" / "readiness.json"
    if not path.exists():
        pytest.skip("no readiness artifact on disk yet; --dry-run writes it")
    artifact = json.loads(path.read_text(encoding="utf-8")).get("artifact") or {}
    assert artifact.get("authoritative") is True
    assert artifact.get("is_current") is True
    assert artifact.get("supersedes")


def test_the_historical_snapshot_is_frozen_where_nothing_writes():
    frozen = BENCH / "results" / "v0.1.2" / "hardened"
    snapshots = sorted(frozen.glob("readiness.2*.json"))
    assert snapshots, "the hardened evaluation's readiness snapshot is gone"
    runner = (BENCH / "legacy" / "run_hardened_eval.py").read_text(
        encoding="utf-8")
    assert 'RESULTS / "hardened" / "readiness.json"' in runner
    for snapshot in snapshots:
        assert snapshot.name not in runner, (
            f"the archived runner would overwrite the frozen {snapshot.name}")


def test_the_hardened_evidence_is_still_where_it_was():
    results = BENCH / "results" / "v0.1.2"
    for name in ("trials", "stages", "controls", "aggregate.json",
                 "verdicts_v2.json", "provenance.json"):
        assert (results / name).exists(), f"{name} is missing"


# --------------------------------------------------------------------------
# 3. arm isolation
# --------------------------------------------------------------------------


def test_each_arm_gets_its_own_output_and_fixture_directory():
    plan = run_mod.plan_pairs(["A_cold_continuation"], 2, "q")
    dirs = {spec["dir"] for entry in plan for spec in entry["arms"].values()}
    trials = {spec["trial"] for entry in plan for spec in entry["arms"].values()}
    assert len(dirs) == 4 and len(trials) == 4, (
        "two arms sharing an output or fixture directory would let one "
        "inherit the other's tree, ledger and generated files")


def test_the_fixture_is_rebuilt_from_scratch_for_every_trial():
    """`Scenario.build` removes the tree before writing it, so no file, no git
    object and no generated log survives from a previous arm."""
    source = _source("scenarios.py")
    build = source[source.index("    def build(self, dest"):]
    body = build[:build.index("    def verify(")]
    assert "rmtree_force(repo)" in body
    assert body.index("rmtree_force(repo)") < body.index('git(repo, "init"')


def test_the_velra_home_is_wiped_per_trial():
    text = _source("live_trial.py")
    assert 'velra_home = out / "velra_home"' in text
    assert "shutil.rmtree(velra_home, ignore_errors=True)" in text


def test_the_trial_environment_isolates_state_from_the_outer_session():
    env = live_trial.trial_env(pathlib.Path("/home"), pathlib.Path("/fx"))
    assert env["VELRA_HOME"] == str(pathlib.Path("/home"))
    assert env["CLAUDE_PROJECT_DIR"] == str(pathlib.Path("/fx"))
    for marker in safety.NESTED_MARKERS:
        assert marker not in env, (
            f"{marker} would leak the outer Claude Code session into a trial")


def test_both_arms_explicitly_set_the_hook_registration():
    """The baseline removes the hooks rather than assuming they are absent, so
    whatever the previous arm left behind is never a confound."""
    assert '"enable" if arm == "velra" else "disable"' in _source("live_trial.py")


def test_the_live_phase_snapshots_and_restores_the_settings_file():
    text = _source("run.py")
    assert "def snapshot_settings()" in text and "def restore_settings(" in text
    phase = _function(ast.parse(text), "phase_live")
    tries = [n for n in ast.walk(phase) if isinstance(n, ast.Try)]
    assert tries, "phase_live must restore the settings file in a finally"
    assert any("restore_settings" in ast.unparse(n)
               for block in tries for n in block.finalbody), (
        "a run that dies mid-trial must not leave the user's Claude Code "
        "reconfigured")


def test_arm_order_alternates_and_is_recorded():
    """A shared server-side prompt cache means order matters. It cannot be
    controlled, so it is balanced and recorded instead of ignored."""
    assert run_mod.arm_order(0) == ("baseline", "velra")
    assert run_mod.arm_order(1) == ("velra", "baseline")
    assert run_mod.arm_order(2) == ("baseline", "velra")
    assert '"arm_order_index": arm_order_index' in _source("live_trial.py")


# --------------------------------------------------------------------------
# 4. Benchmark B transition semantics
# --------------------------------------------------------------------------


def test_b_stages_before_the_clear_turn():
    """Measured against the release binary: `SessionStart(clear)` bumps the
    session epoch and `restore::build` reads the current one, so a restore
    issued after the clear returns NoState and the trial measures nothing."""
    text = _source("live_trial.py")
    assert 'CLEAR_TURN = "/clear"' in text
    assert "pause_before_turn=restore_at" in text
    assert "source_turns.index(CLEAR_TURN)" in text


def test_the_clear_turn_is_the_last_turn_of_scenario_b():
    scenario = scenarios.get("B_clear_survival")
    assert scenario.turns[-1] == live_trial.CLEAR_TURN
    assert scenario.transition == "clear"
    assert scenario.turns.index(live_trial.CLEAR_TURN) == \
        scenario.transition_index


def test_scenario_a_has_no_clear_turn_and_stages_after_the_session():
    scenario = scenarios.get("A_cold_continuation")
    assert live_trial.CLEAR_TURN not in scenario.turns
    assert scenario.transition == "new_session"


def test_the_transition_lifecycle_is_recorded_in_every_trial():
    text = _source("live_trial.py")
    for field in ("restore_before_turn_index",
                  "restore_while_source_session_live",
                  "destination_is_a_new_process",
                  "continuation_prompt_identical_on_both_arms",
                  "clear_turn_index"):
        assert field in text, f"the lifecycle record omits {field}"


def test_the_destination_is_always_a_new_process():
    """A staged capsule is delivered on SessionStart(startup) and on nothing
    else, so both arms of B start a fresh destination session."""
    text = _source("live_trial.py")
    destination = text[text.index("# -- the destination session"):]
    assert "drive_session(" in destination[:600]


def test_both_arms_send_the_same_continuation_prompt():
    for name in scenarios.DEFAULT_ORDER:
        manifest = scenarios.manifest_for(name)
        assert manifest["continuation_prompt"] == \
            scenarios.get(name).continuation_prompt
    text = _source("live_trial.py")
    assert "[scenario.continuation_prompt]" in text, (
        "the destination prompt must come from the scenario, not the arm")


def test_phase_two_clear_behaviour_is_not_changed_by_the_benchmark():
    """The fix for B is in the benchmark's ordering, not in the product."""
    reducer = (REPO_ROOT / "crates" / "velra-core" / "src"
               / "reducer.rs").read_text(encoding="utf-8")
    assert 'if p.source.as_deref() == Some("clear") {' in reducer
    assert "bump_epoch(&ctx)?;" in reducer


# --------------------------------------------------------------------------
# 6. capsule provenance
# --------------------------------------------------------------------------


def test_the_live_driver_never_composes_capsule_text():
    """The capsule must come out of `velra restore`, not out of the scenario."""
    text = _source("live_trial.py")
    assert "VELRA_WORKSPACE_STATE" not in text, (
        "the live driver appears to build capsule text; it may only read what "
        "velra staged")
    assert 'capsule = ((record.get("staged") or {}).get("capsule") or "")' in text


def test_the_scenario_declares_markers_but_never_supplies_them():
    """`capsule_markers` are checked against the production capsule. Nothing
    in the scenario module writes capsule text for a live trial."""
    text = _source("scenarios.py")
    assert "VELRA_WORKSPACE_STATE" not in text


#: Modules allowed to mention the capsule tag, and why each one may.
#:
#: Only the first *composes* capsule text. The other three recognise it:
#: `parse` locates a delivery by it, `smoke` counts occurrences to prove one
#: bounded record arrived rather than a replayed conversation, and
#: `capsule_probe` checks that what `velra restore` produced is production
#: output. Composition anywhere else would mean the benchmark was supplying
#: its own answer.
CAPSULE_TEXT_MODULES = {
    "mock_adapter.py": "composes synthetic capsules, marked as synthetic",
    "parse.py": "recognises a delivery",
    "smoke.py": "asserts exactly one record arrived",
    "capsule_probe.py": "checks the restore output is production text",
}


def test_only_the_mock_adapter_composes_capsules_and_marks_them_synthetic():
    mock = _source("mock_adapter.py")
    assert "VELRA_WORKSPACE_STATE" in mock
    assert 'SYNTHETIC_MARK = "MockClaudeAdapter"' in mock
    mentions = {p.name for p in TOKENBURN.glob("*.py")
                if "VELRA_WORKSPACE_STATE" in p.read_text(encoding="utf-8")}
    unexpected = mentions - set(CAPSULE_TEXT_MODULES)
    assert not unexpected, (
        f"{sorted(unexpected)} mention capsule text; if they compose it, the "
        f"benchmark is supplying its own ground truth")
    # The live driver is the one that must never mention it at all.
    assert "live_trial.py" not in mentions


def _shares_a_phrase(text: str, turns, words: int = 4) -> bool:
    """Whether some turn shares a run of `words` consecutive words with text."""
    def runs(s: str) -> set:
        tokens = [w.strip(".,;:—-").lower() for w in s.split() if w.strip()]
        return {" ".join(tokens[i:i + words])
                for i in range(max(0, len(tokens) - words + 1))}

    mine = runs(text)
    return any(mine & runs(turn) for turn in turns)


def test_the_probe_replays_the_scenario_rather_than_inventing_activity():
    """The probe may only put into the ledger what the turn script puts there.

    An earlier version of this test forbade any declared marker from appearing
    in the probe's own prompts, and it was wrong. Benchmark B's invariant —
    that a window closes on the booking date — *is* a sentence a user says out
    loud once, and Velra's capsule carries user prompts verbatim; that is the
    mechanism under test, not a cheat. What actually has to hold is narrower
    and checkable: every prompt the probe replays is drawn from the scenario's
    real turn script, so the probe cannot manufacture a marker the scenario
    would not itself have produced.
    """
    for name, activity in capsule_probe.ACTIVITY.items():
        turns = scenarios.get(name).turns
        for kind, value in activity:
            if kind != "prompt":
                continue
            assert _shares_a_phrase(value, turns), (
                f"{name}: the probe replays a prompt that appears nowhere in "
                f"the turn script: {value[:80]!r}")


def test_the_probe_reaches_the_ledger_only_through_real_hook_channels():
    """Every activity entry is a hook event a real session emits."""
    legal = {"prompt", "read", "edit", "test_fail"}
    for name, activity in capsule_probe.ACTIVITY.items():
        kinds = {kind for kind, _ in activity}
        assert kinds <= legal, f"{name}: {kinds - legal}"
    probe = _source("capsule_probe.py")
    assert "velra_core" not in probe and "sqlite3" not in probe, (
        "the probe must not write the ledger directly; it drives the hooks")


@pytest.mark.slow
def test_the_production_capsule_carries_every_declared_marker(tmp_path):
    """Area 6, run for real against the release binary, offline."""
    binary = capsule_probe.binary_path(None)
    if not binary.exists():
        pytest.skip("no release binary built")
    result = capsule_probe.run(binary, root=tmp_path / "probe")
    assert result["capsule_written_by_benchmark"] is False
    for name, entry in result["scenarios"].items():
        assert entry["capsule_is_production_output"], name
        assert not entry["markers_missing"], (
            f"{name}: the production renderer did not carry "
            f"{entry['markers_missing']}; every Velra-arm trial would be a "
            f"RECEIPT_FAILURE")
