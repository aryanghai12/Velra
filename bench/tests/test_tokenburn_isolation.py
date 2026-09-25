"""Trial validity: auto-memory isolation and the source handoff.

The v0.1.2 qualification (tag ``tokenburn-qualification-v0.1.2``) was confounded
twice: Claude Code auto-memory carried the scenario's hidden state into both
arms, and source sessions solved the target before the transition. These tests
pin the controls that make both detectable, and that an invalid trial can never
be scored. Everything here is offline; nothing starts Claude.
"""

from __future__ import annotations

import ast
import inspect
import json
import pathlib
import subprocess
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
TOKENBURN = BENCH / "tokenburn"
sys.path.insert(0, str(BENCH))

from tokenburn import aggregate  # noqa: E402
from tokenburn import context_fixture  # noqa: E402
from tokenburn import isolation  # noqa: E402
from tokenburn import leakscan  # noqa: E402
from tokenburn import prereg  # noqa: E402
from tokenburn import scenarios  # noqa: E402
from tokenburn.mock_adapter import MockSpec, write_trial  # noqa: E402

A = "A_cold_continuation"
B = "B_clear_survival"


@pytest.fixture
def config(tmp_path, monkeypatch):
    """A private Claude config dir, so no test touches the real one."""
    cfg = tmp_path / "claude-config"
    cfg.mkdir()
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(cfg))
    return cfg


def _run_trial_source() -> str:
    tree = ast.parse((TOKENBURN / "live_trial.py").read_text(encoding="utf-8"))
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == "run_trial":
            return ast.unparse(node)
    raise AssertionError("run_trial not found")


# --------------------------------------------------------------------------
# 1. auto-memory disabled for source and destination
# --------------------------------------------------------------------------


def test_every_trial_process_gets_the_env_control(tmp_path):
    env = isolation.trial_env(tmp_path / "home", tmp_path / "fixture")
    assert env[isolation.MEMORY_ENV] == "1"


def test_the_live_driver_uses_the_same_environment_function():
    # Text, not import: the offline suite must not import the live driver.
    text = (TOKENBURN / "live_trial.py").read_text(encoding="utf-8")
    assert "trial_env = isolation.trial_env" in text


def test_both_sessions_run_with_the_trial_env_and_are_checked(config):
    body = _run_trial_source()
    calls = [line for line in body.splitlines() if "drive_session(" in line]
    assert len(calls) == 2, calls           # source and destination
    assert all(", env," in c for c in calls), calls
    assert "controls['before_source'] = isolation.controls_state" in body
    assert "controls['before_destination'] = isolation.controls_state" in body


def test_controls_state_requires_both_controls(config, tmp_path):
    settings = isolation.settings_path()
    env = isolation.trial_env(tmp_path / "h", tmp_path / "f")
    assert not isolation.controls_state(env, settings)["auto_memory_disabled"]
    isolation.apply_memory_setting(settings)
    assert isolation.controls_state(env, settings)["auto_memory_disabled"]
    no_env = {k: v for k, v in env.items() if k != isolation.MEMORY_ENV}
    assert not isolation.controls_state(no_env, settings)["auto_memory_disabled"]
    data = json.loads(settings.read_text(encoding="utf-8"))
    data[isolation.MEMORY_DIR_SETTING] = "~/elsewhere"
    settings.write_text(json.dumps(data), encoding="utf-8")
    assert not isolation.controls_state(env, settings)["auto_memory_disabled"], \
        "a relocated memory directory cannot be proven isolated"


def test_settings_round_trip_is_byte_exact(config):
    settings = isolation.settings_path()
    original = b'{\n  "model": "opus",\n  "effortLevel": "high"\n}\n'
    settings.write_bytes(original)
    got = isolation.apply_memory_setting(settings)
    assert got["applied"] is True
    after = json.loads(settings.read_text(encoding="utf-8"))
    assert after == {"model": "opus", "effortLevel": "high",
                     "autoMemoryEnabled": False}
    body = _run_trial_source()
    assert "settings.write_bytes(settings_bytes)" in body
    assert "finally:" in body


def test_an_unparseable_settings_file_is_refused_not_rewritten(config):
    settings = isolation.settings_path()
    settings.write_text('{ "model": "opus", // comment\n}', encoding="utf-8")
    before = settings.read_bytes()
    assert isolation.apply_memory_setting(settings)["applied"] is False
    assert settings.read_bytes() == before


# --------------------------------------------------------------------------
# 2. the memory directory is scanned
# --------------------------------------------------------------------------


def test_memory_dir_follows_the_observed_project_slug(config):
    repo = pathlib.Path(r"C:\Users\aryan\AppData\Local\Temp"
                        r"\velra-tokenburn-fixtures\A_cold_continuation-q1-velra") \
        if sys.platform == "win32" else pathlib.Path("/tmp/x/A_cold-q1")
    slug = isolation.project_slug(repo)
    if sys.platform == "win32":
        assert slug == ("C--Users-aryan-AppData-Local-Temp-velra-tokenburn-"
                        "fixtures-A-cold-continuation-q1-velra")
    assert isolation.memory_dir(repo) == config / "projects" / slug / "memory"


def test_the_leak_scanner_reads_auto_memory_and_treats_it_as_fatal(tmp_path):
    memory = tmp_path / "memory"
    memory.mkdir()
    (memory / "note.md").write_text("windows close on the booking date, never "
                                    "the value date\n", encoding="utf-8")
    repo = tmp_path / "repo"
    repo.mkdir()
    result = leakscan.scan(repo, [], "continue", ["never the value date"],
                           env={}, memory_dir=memory)
    assert "auto_memory" in leakscan.FATAL_SURFACES
    assert result["clean"] is False
    assert result["fatal_hits"][0]["surface"] == "auto_memory"
    assert result["auto_memory_dir"] == str(memory)


def test_the_build_records_its_handoff_baseline_and_scans_automem(
        config, tmp_path):
    # (Named without the substring "_memo": it is one of A's leak terms, and
    # pytest exports the test name in PYTEST_CURRENT_TEST, which the
    # environment surface rightly scans.)
    repo = tmp_path / "fx"
    manifest = scenarios.get(A).build(repo, verify=True)
    assert manifest["leak_scan"]["auto_memory_dir"] == \
        str(isolation.memory_dir(repo))
    assert manifest["fixture_head"]
    assert "observed_invariant" in manifest["ground_truth"]["as_generated"]


# --------------------------------------------------------------------------
# 3 / 4. hidden memory invalidates; clean memory passes
# --------------------------------------------------------------------------


VALID_HANDOFF = {"source_handoff_valid": True, "source_handoff_reason": "ok"}


def test_a_hidden_memory_note_invalidates_even_when_it_paraphrases(tmp_path):
    memory = tmp_path / "memory"
    memory.mkdir()
    # No registered leak phrase matches this wording -- emptiness is the test.
    (memory / "MEMORY.md").write_text("- windows use `booking_date`\n",
                                      encoding="utf-8")
    scan = isolation.scan_memory(memory, ["never the value date"])
    assert scan["clean"] is False and scan["files"] == ["MEMORY.md"]
    assert scan["term_hits"] == []
    validity = isolation.trial_validity(auto_memory_disabled=True,
                                        memory_scan_clean=scan["clean"],
                                        handoff=VALID_HANDOFF)
    assert validity["valid"] is False
    assert "auto-memory directory was not empty" in \
        validity["invalidation_reason"]


def test_clean_memory_passes(tmp_path):
    assert isolation.scan_memory(tmp_path / "absent", ["x"])["clean"] is True
    empty = tmp_path / "empty"
    empty.mkdir()
    scan = isolation.scan_memory(empty, ["x"])
    assert scan["clean"] is True
    assert isolation.trial_validity(auto_memory_disabled=True,
                                    memory_scan_clean=True,
                                    handoff=VALID_HANDOFF) == \
        {"valid": True, "invalidation_reason": None}


def test_prior_memory_is_quarantined_not_deleted(tmp_path):
    memory = tmp_path / "memory"
    memory.mkdir()
    (memory / "MEMORY.md").write_text("old\n", encoding="utf-8")
    moved = isolation.quarantine_prior_memory(memory, tmp_path / "trial")
    assert moved and (pathlib.Path(moved) / "MEMORY.md").read_text() == "old\n"
    assert isolation.scan_memory(memory, [])["clean"]
    assert isolation.quarantine_prior_memory(memory, tmp_path / "t2") is None


# --------------------------------------------------------------------------
# 5 / 6. the source handoff, on real generated fixtures
# --------------------------------------------------------------------------


@pytest.fixture(scope="module", params=[A, B])
def fixture_repo(request, tmp_path_factory):
    name = request.param
    repo = tmp_path_factory.mktemp(f"handoff-{name}") / "repo"
    manifest = scenarios.get(name).build(repo, verify=True)
    context_fixture.generate(repo, 20_000, name, 0)
    return name, repo, manifest


def _restore(repo: pathlib.Path) -> None:
    subprocess.run(["git", "checkout", "--", "."], cwd=repo, check=True,
                   capture_output=True)
    subprocess.run(["git", "clean", "-fdq", "-e", "logs/"], cwd=repo,
                   check=True, capture_output=True)


def test_target_fail_with_the_generated_invariant_is_a_valid_handoff(
        fixture_repo):
    _, repo, manifest = fixture_repo
    got = isolation.validate_handoff(repo, manifest)
    assert got["source_handoff_valid"], got["source_handoff_reason"]
    assert got["target_status_at_handoff"] == "FAIL"
    assert got["invariant_status_at_handoff"]["matches_expected"] is True
    assert got["worktree_violations"] == []


def test_target_pass_at_handoff_invalidates(fixture_repo):
    name, repo, manifest = fixture_repo
    fix = next(v for v in scenarios.get(name).variants if v.name == "true_fix")
    try:
        for rel, text in fix.files.items():
            scenarios.write(repo / rel, text)
        got = isolation.validate_handoff(repo, manifest)
        assert got["source_handoff_valid"] is False
        assert got["target_status_at_handoff"] == "PASS"
        assert "is PASS at handoff" in got["source_handoff_reason"]
        assert any("tracked file changed" in v for v in got["worktree_violations"])
    finally:
        _restore(repo)


def test_a_committed_fix_or_a_stash_invalidates(fixture_repo):
    name, repo, manifest = fixture_repo
    fix = next(v for v in scenarios.get(name).variants if v.name == "true_fix")
    try:
        for rel, text in fix.files.items():
            scenarios.write(repo / rel, text)
        subprocess.run(["git", "stash"], cwd=repo, check=True,
                       capture_output=True)
        got = isolation.validate_handoff(repo, manifest)
        assert got["target_status_at_handoff"] == "FAIL"
        assert any("stash" in v for v in got["worktree_violations"])
        assert got["source_handoff_valid"] is False
    finally:
        subprocess.run(["git", "stash", "drop"], cwd=repo, capture_output=True)
        _restore(repo)


def test_harness_files_are_allowed_other_new_files_are_not(fixture_repo):
    _, repo, manifest = fixture_repo
    try:
        assert not isolation.worktree_violations(repo, manifest)  # logs/ exist
        (repo / "src" / "payments" / "__pycache__").mkdir(exist_ok=True)
        (repo / "src" / "payments" / "__pycache__" / "x.pyc").write_bytes(b"0")
        assert not isolation.worktree_violations(repo, manifest)
        (repo / "tests" / "test_extra.py").write_text("x = 1\n")
        assert isolation.worktree_violations(repo, manifest) == \
            ["untracked file: tests/test_extra.py"]
    finally:
        _restore(repo)


def test_a_handoff_is_judged_by_its_exit_code_not_its_prose(tmp_path):
    manifest = {"target_test": "t::x", "invariant": {},
                "ground_truth": {"as_generated": {"observed_invariant": True}}}
    for code, status in ((0, "PASS"), (1, "FAIL"), (2, "ERROR"), (5, "ERROR")):
        got = isolation.validate_handoff(
            tmp_path, dict(manifest, fixture_head=None),
            run_target=lambda repo, node, c=code: (c, "FAILED t::x"))
        assert got["target_status_at_handoff"] == status


# --------------------------------------------------------------------------
# 7. both arms are validated identically
# --------------------------------------------------------------------------


def test_no_validity_function_can_tell_the_arms_apart():
    for fn in (isolation.trial_env, isolation.session_env,
               isolation.validate_handoff, isolation.trial_validity,
               isolation.scan_memory, isolation.controls_state,
               isolation.apply_memory_setting):
        assert "arm" not in inspect.signature(fn).parameters, fn.__name__


def test_the_transition_hook_validates_before_it_stages_for_either_arm():
    body = _run_trial_source()
    assert "on_pause=at_transition" in body
    # Validation first, and not behind an arm check; staging only after it.
    hook = body[body.index("def at_transition"):body.index("if isolated:")]
    assert hook.index("validate_handoff") < hook.index("stage_now(")
    before = hook.split("validate_handoff")[0]
    for guard in ("arm ==", "arm !=", "if arm", "arm in"):
        assert guard not in before, guard


@pytest.mark.parametrize("bad_arm", ["baseline", "velra"])
def test_an_invalid_trial_on_either_arm_drops_the_pair(tmp_path, bad_arm):
    for arm in ("baseline", "velra"):
        write_trial(tmp_path, MockSpec(
            trial=f"p-{arm}", scenario=A, arm=arm, pair_id="p#1",
            handoff_target="PASS" if arm == bad_arm else "FAIL"))
    result = aggregate.run(tmp_path, tmp_path / "out", write=False)
    [dropped] = result["pairing"]["dropped"]
    assert dropped["reason"] == "INVALID_TRIAL"
    assert list(dropped["invalid_arms"]) == [bad_arm]


# --------------------------------------------------------------------------
# 8. invalid trials never reach an aggregate or a verdict
# --------------------------------------------------------------------------


def test_invalid_trials_are_excluded_from_every_scored_result(tmp_path):
    specs = []
    for arm in ("baseline", "velra"):
        specs.append(MockSpec(trial=f"ok-{arm}", scenario=A, arm=arm,
                              pair_id="ok#1"))
        specs.append(MockSpec(trial=f"mem-{arm}", scenario=B, arm=arm,
                              pair_id="mem#1", transition="clear",
                              memory_leak=True))
    for spec in specs:
        write_trial(tmp_path, spec)
    # An invalid trial has no destination to parse, and nothing tries to.
    assert not (tmp_path / "mem-velra" / "stream.jsonl").exists()
    result = aggregate.run(tmp_path, tmp_path / "out", write=True)
    assert [p["pair_id"] for p in result["pairs"]] == ["ok#1"]
    assert {d["pair_id"]: d["reason"] for d in result["pairing"]["dropped"]} \
        == {"mem#1": "INVALID_TRIAL"}
    assert {r["pair_id"] for r in result["trial_rows"]} == {"ok#1"}
    assert {e["trial"] for e in result["invalid_trials"]} == \
        {"mem-baseline", "mem-velra"}
    verdicts = json.loads((tmp_path / "out" / "verdicts.json").read_text())
    assert [v["pair_id"] for v in verdicts["pairs"]] == ["ok#1"]
    analysis = json.loads((tmp_path / "mem-velra" / "analysis.json").read_text())
    assert analysis["scored"] is False
    assert "auto-memory directory" in analysis["invalidation_reason"]
    report = (tmp_path / "out" / "report.md").read_text(encoding="utf-8")
    assert "INVALID_TRIAL" in report


def test_a_trial_recorded_before_the_checks_is_scored_as_before():
    # Frozen v0.1.2 trials carry no trial_validity; they must not be
    # reinterpreted as invalid (or as validated).
    assert isolation.is_invalid({}) is False
    assert isolation.is_invalid({"trial_validity": {"valid": True}}) is False
    assert isolation.is_invalid({"trial_validity": {"valid": False}}) is True


def test_every_validity_field_is_recorded_machine_readably():
    body = _run_trial_source()
    for key in ("auto_memory_disabled", "memory_scan_clean",
                "source_handoff_valid", "source_handoff_reason",
                "target_status_at_handoff", "invariant_status_at_handoff",
                "invalidation_reason", "trial_validity"):
        assert f"'{key}':" in body, key


# --------------------------------------------------------------------------
# the preregistration amendment
# --------------------------------------------------------------------------


def test_the_amendment_changes_classification_only():
    doc = prereg.load()
    assert doc["version"] == "1.1.0"
    assert "trial_validity" in doc and doc["amendments"][0]["version"] == "1.1.0"
    assert doc["verdict_rules"]["order"][0].startswith(
        "a pair containing an INVALID trial")
    try:
        old_raw = subprocess.run(
            ["git", "show", "7a09e65:bench/tokenburn/preregistration_tokenburn.json"],
            cwd=BENCH.parent, capture_output=True, text=True, check=True,
            encoding="utf-8").stdout
    except (OSError, subprocess.CalledProcessError):
        pytest.skip("the frozen 1.0.0 preregistration is not reachable in git")
    old = json.loads(old_raw)
    for key in ("thesis", "benchmarks", "trial_plan", "pair_identity_invariants",
                "fairness", "telemetry_rules", "context_ladder",
                "capsule_token_ceiling"):
        assert doc.get(key) == old.get(key), key
    new_rules = dict(doc["verdict_rules"])
    old_rules = dict(old["verdict_rules"])
    assert new_rules.pop("order")[1:] == old_rules.pop("order")
    assert new_rules == old_rules      # values, thresholds, failure classes


def test_the_offline_preflight_passes():
    result = isolation.preflight()
    assert result["ok"], [c for c in result["checks"] if not c["ok"]]
