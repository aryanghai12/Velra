"""Safety, dependencies, fixtures and leak scanning for the Token-Burn bench.

The tests here are the ones whose failure would mean the benchmark could spend
money, pull in a dependency it promised not to, or measure a scenario whose
answer is readable from the repository. None of them starts a process that
could reach Anthropic, and two of them prove that the code paths that could
are shut.
"""

from __future__ import annotations

import ast
import json
import pathlib
import subprocess
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
TOKENBURN = BENCH / "tokenburn"
# See the note in test_tokenburn_pipeline.py: package import, not path insert.
sys.path.insert(0, str(BENCH))

from tokenburn import context_fixture  # noqa: E402
from tokenburn import leakscan  # noqa: E402
from tokenburn import prereg  # noqa: E402
from tokenburn import safety  # noqa: E402
from tokenburn import scenarios  # noqa: E402
from tokenburn import selftest as selftest_mod  # noqa: E402
from tokenburn import telemetry  # noqa: E402


def sources(root: pathlib.Path) -> list[pathlib.Path]:
    return [p for p in sorted(root.rglob("*.py"))
            if "__pycache__" not in p.parts]


# --------------------------------------------------------------------------
# the live gate
# --------------------------------------------------------------------------


def test_live_mode_needs_both_the_flag_and_the_environment_variable():
    env = {}
    assert not safety.check(safety.MODE_LIVE, live_flag=True, env=env).allowed
    assert not safety.check(safety.MODE_LIVE, live_flag=False,
                            env={safety.LIVE_ENV: "1"}).allowed
    allowed = safety.check(safety.MODE_LIVE, live_flag=True,
                           env={safety.LIVE_ENV: "1"})
    assert allowed.allowed, allowed.reasons


def test_live_mode_refuses_inside_a_claude_code_session():
    for marker in safety.NESTED_MARKERS:
        result = safety.check(safety.MODE_LIVE, live_flag=True,
                              env={safety.LIVE_ENV: "1", marker: "1"})
        assert not result.allowed
        assert any("inside a Claude Code session" in r for r in result.reasons)


def test_require_live_raises_rather_than_returning_false():
    with pytest.raises(safety.LiveExecutionRefused):
        safety.require_live(safety.MODE_LIVE, live_flag=True, env={})
    with pytest.raises(safety.LiveExecutionRefused):
        safety.require_live(safety.MODE_LIVE, live_flag=False,
                            env={safety.LIVE_ENV: "1"})


def test_the_environment_variable_must_be_exactly_one():
    for value in ("0", "true", "yes", "", "11"):
        assert not safety.check(safety.MODE_LIVE, live_flag=True,
                                env={safety.LIVE_ENV: value}).allowed


@pytest.mark.parametrize("mode", safety.OFFLINE_MODES)
def test_offline_modes_permit_nothing(mode):
    record = safety.assert_offline(mode, env={})
    assert record["offline"] is True
    assert record["claude_processes_permitted"] == 0
    assert record["network_calls_permitted"] == 0
    assert record["api_spend_permitted_usd"] == 0.0


def test_assert_offline_refuses_to_bless_live_mode():
    with pytest.raises(safety.LiveExecutionRefused):
        safety.assert_offline(safety.MODE_LIVE, env={})


# --------------------------------------------------------------------------
# no live process in the offline modes -- structurally, not by convention
# --------------------------------------------------------------------------


def _function(tree: ast.Module, name: str) -> ast.FunctionDef:
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == name:
            return node
    raise AssertionError(f"{name} not found")


def test_the_runner_imports_the_live_driver_only_inside_the_gated_function():
    tree = ast.parse((TOKENBURN / "run.py").read_text(encoding="utf-8"))
    live_phase = _function(tree, "phase_live")
    inside = {node.lineno for node in ast.walk(live_phase)
              if hasattr(node, "lineno")}

    offenders = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            names = [a.name for a in node.names] + [getattr(node, "module", "")]
            if any(n and "live_trial" in n for n in names):
                if node.lineno not in inside:
                    offenders.append(node.lineno)
    # A dynamic import is still an import. `__import__("live_trial")` and
    # `importlib.import_module("live_trial")` are not ast.Import nodes, so the
    # walk above would miss them; the text check below does not.
    text = (TOKENBURN / "run.py").read_text(encoding="utf-8")
    for dynamic in ('__import__("live_trial")', "import_module('live_trial')",
                    'import_module("live_trial")'):
        assert dynamic not in text, (
            f"run.py reaches the live driver dynamically via {dynamic}")
    assert not offenders, (
        f"run.py imports live_trial outside phase_live at lines {offenders}; "
        f"the offline modes must have no reachable path to it")


def test_the_live_phase_checks_the_gate_before_anything_else():
    tree = ast.parse((TOKENBURN / "run.py").read_text(encoding="utf-8"))
    body = [n for n in _function(tree, "phase_live").body
            if not isinstance(n, ast.Expr) or not isinstance(n.value, ast.Constant)]
    first = body[0]
    assert isinstance(first, ast.Expr) and isinstance(first.value, ast.Call), \
        "phase_live must begin with the gate call"
    assert ast.unparse(first.value).startswith("safety.require_live"), \
        f"phase_live begins with {ast.unparse(first.value)!r}, not the gate"


def test_the_live_driver_gates_itself_too():
    text = (TOKENBURN / "live_trial.py").read_text(encoding="utf-8")
    assert "safety.require_live" in text, (
        "live_trial.py must not depend on its caller having been careful")


def test_selftest_starts_no_process_at_all(tmp_path, monkeypatch):
    """Not 'starts no Claude process' -- starts no process."""
    def refuse(*args, **kwargs):
        raise AssertionError(f"the selftest started a subprocess: {args[:1]}")

    monkeypatch.setattr(subprocess, "run", refuse)
    monkeypatch.setattr(subprocess, "Popen", refuse)
    monkeypatch.setattr(subprocess, "check_output", refuse)
    _, failures = selftest_mod.run(tmp_path / "selftest")
    assert failures == []


def test_no_module_in_the_analysis_path_can_launch_claude():
    """The parser, the metrics and the verdict never shell out."""
    for name in ("parse.py", "metrics.py", "causal.py", "pairing.py",
                 "verdict.py", "aggregate.py", "report.py", "telemetry.py",
                 "mock_adapter.py"):
        text = (TOKENBURN / name).read_text(encoding="utf-8")
        assert "subprocess" not in text, (
            f"{name} imports subprocess; nothing in the analysis path should "
            f"be able to start anything")


# --------------------------------------------------------------------------
# dependencies
# --------------------------------------------------------------------------

#: Named in the Phase 3 specification as forbidden, plus the obvious relatives.
FORBIDDEN = {
    "pandas", "numpy", "scipy", "polars", "matplotlib", "seaborn", "sklearn",
    "torch", "tensorflow", "plotly", "statsmodels", "pyarrow", "dask",
    "xarray", "altair", "bokeh", "numba",
}

#: Third-party modules the benchmark is allowed to import at all.
ALLOWED_THIRD_PARTY = {"pytest"}


def imported_top_level(path: pathlib.Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(a.name.split(".")[0] for a in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            names.add(node.module.split(".")[0])
    return names


def test_the_benchmark_imports_no_heavy_data_science_package():
    offenders = {}
    for path in sources(BENCH):
        hits = imported_top_level(path) & FORBIDDEN
        if hits:
            offenders[str(path.relative_to(REPO_ROOT))] = sorted(hits)
    assert not offenders, f"forbidden dependencies: {offenders}"


def test_the_tokenburn_package_is_standard_library_only():
    local = {p.stem for p in sources(TOKENBURN)}
    local |= {p.stem for p in sources(BENCH / "harness")}
    stdlib = set(sys.stdlib_module_names)
    offenders = {}
    for path in sources(TOKENBURN):
        outside = (imported_top_level(path) - stdlib - local
                   - ALLOWED_THIRD_PARTY)
        if outside:
            offenders[path.name] = sorted(outside)
    assert not offenders, (
        f"the harness must stay standard-library only: {offenders}")


def test_the_repository_declares_no_new_python_dependency():
    """There is no requirements file to grow, and that is the point."""
    for name in ("requirements.txt", "requirements-dev.txt", "pyproject.toml",
                 "setup.py", "Pipfile", "poetry.lock"):
        assert not (REPO_ROOT / name).exists(), (
            f"{name} appeared; the benchmark is meant to need no install step")


# --------------------------------------------------------------------------
# the context-load ladder
# --------------------------------------------------------------------------


def test_the_ladder_is_the_registered_one():
    assert prereg.ladder() == (100_000, 250_000, 500_000, 700_000, 800_000,
                               900_000)


def test_a_rung_generates_verifies_and_is_deterministic(tmp_path):
    first = context_fixture.generate(tmp_path / "a", 100_000,
                                     "A_cold_continuation", seed=3)
    second = context_fixture.generate(tmp_path / "b", 100_000,
                                      "A_cold_continuation", seed=3)
    assert [f["sha256"] for f in first["files"]] == \
           [f["sha256"] for f in second["files"]]
    check = context_fixture.verify(tmp_path / "a", first)
    assert check["ok"], check["problems"]
    assert check["shortfall_fraction"] <= 0.02


def test_a_different_seed_produces_a_different_fixture(tmp_path):
    a = context_fixture.generate(tmp_path / "a", 100_000, "A_cold_continuation",
                                 seed=1)
    b = context_fixture.generate(tmp_path / "b", 100_000, "A_cold_continuation",
                                 seed=2)
    assert [f["sha256"] for f in a["files"]] != [f["sha256"] for f in b["files"]]


def test_the_generator_records_that_its_size_is_an_estimate(tmp_path):
    record = context_fixture.generate(tmp_path, 100_000, "A_cold_continuation")
    assert record["token_basis"] == "estimated"
    assert record["chars_per_token_assumed"] == context_fixture.CHARS_PER_TOKEN
    assert record["actual_observed_context_size"] is None
    assert "NOT an observation" in record["warning"]
    assert record["kind"] == "synthetic_load_fixture"


def test_the_generated_text_is_engineering_output_not_filler(tmp_path):
    context_fixture.generate(tmp_path, 100_000, "A_cold_continuation")
    text = (tmp_path / "logs" / "build-01.log").read_text(encoding="utf-8")
    for marker in ("Compiling", "warning:", "--> src/payments/"):
        assert marker in text
    # No single line may make up a large share of the file: that would be
    # repeated filler wearing a log's clothes.
    lines = [line for line in text.splitlines() if line.strip()]
    most_common = max(lines.count(line) for line in set(lines))
    assert most_common < len(lines) * 0.25


@pytest.mark.slow
def test_the_largest_rung_generates_within_budget(tmp_path):
    record = context_fixture.generate(tmp_path, 900_000, "A_cold_continuation")
    check = context_fixture.verify(tmp_path, record)
    assert check["ok"], check["problems"]
    assert record["bytes_on_disk"] > 3_000_000


# --------------------------------------------------------------------------
# scenarios and leak scanning
# --------------------------------------------------------------------------


def test_both_scenarios_have_at_least_fifteen_turns():
    for name in scenarios.DEFAULT_ORDER:
        scenario = scenarios.get(name)
        assert len(scenario.turns) >= prereg.minimum_source_turns()


def test_both_arms_get_the_same_continuation_prompt():
    for name in scenarios.DEFAULT_ORDER:
        manifest = scenarios.manifest_for(name)
        assert manifest["continuation_prompt"] == \
            scenarios.get(name).continuation_prompt
        # The prompt is a property of the scenario, not of the arm: there is
        # nowhere in the manifest for an arm-specific one to live.
        assert "baseline" not in json.dumps(manifest).lower()


def test_the_leak_scanner_covers_the_nine_required_surfaces():
    # The nine of the Phase 3 specification, plus Claude Code's per-project
    # auto-memory (prereg 1.1.0): the v0.1.2 qualification showed it carrying
    # scenario state into both arms from outside the repository.
    required = {"tree", "filenames", "git_history", "git_refs", "claude_md",
                "project_memory", "auto_memory", "environment", "post_prompt",
                "pre_prompts"}
    assert set(leakscan.ALL_SURFACES) == required
    assert "pre_prompts" not in leakscan.FATAL_SURFACES
    assert "auto_memory" in leakscan.FATAL_SURFACES


def test_git_history_is_a_fatal_surface(tmp_path):
    """The archived scenarios planted dead ends as commits. This one may not."""
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.email", "t@t.t"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "t"], cwd=repo, check=True)
    (repo / "a.txt").write_text("harmless\n", encoding="utf-8")
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "try widening the window"],
                   cwd=repo, check=True)
    result = leakscan.scan(repo, ["turn"], "continue",
                           ["widening the window"])
    assert not result["clean"]
    assert result["fatal_hits"][0]["surface"] == "git_history"


def test_claude_md_is_a_fatal_surface(tmp_path):
    repo = tmp_path / "repo"
    (repo / "docs").mkdir(parents=True)
    (repo / "docs" / "CLAUDE.md").write_text(
        "The window closes on the booking date.\n", encoding="utf-8")
    result = leakscan.scan(repo, ["turn"], "continue",
                           ["closes on the booking date"])
    assert not result["clean"]
    assert any(h["surface"] == "claude_md" for h in result["fatal_hits"])


def test_project_memory_is_a_fatal_surface(tmp_path):
    repo = tmp_path / "repo"
    (repo / ".claude").mkdir(parents=True)
    (repo / ".claude" / "notes.md").write_text(
        "we already tried the module-level dict\n", encoding="utf-8")
    result = leakscan.scan(repo, ["turn"], "continue", ["module-level dict"])
    assert not result["clean"]
    assert any(h["surface"] == "project_memory" for h in result["fatal_hits"])


def test_a_leak_in_the_turn_script_is_not_fatal(tmp_path):
    """The setup states the constraint aloud once. That is the mechanism."""
    empty = tmp_path / "repo"
    empty.mkdir()
    result = leakscan.scan(empty, ["the key must be pure"], "continue",
                           ["must be pure"])
    assert result["clean"]
    assert result["hits_by_surface"].get("pre_prompts") == 1


@pytest.mark.slow
@pytest.mark.parametrize("name", list(scenarios.DEFAULT_ORDER))
def test_each_scenario_builds_verifies_and_does_not_leak(tmp_path, name):
    scenario = scenarios.get(name)
    manifest = scenario.build(tmp_path / name, verify=True)
    truth = manifest["ground_truth"]
    assert truth["as_generated"]["observed_target"] == "fail"
    assert truth["true_fix"]["observed_invariant"] is True
    assert truth["dead_end"]["observed_invariant"] is False
    assert manifest["leak_scan"]["clean"]


# --------------------------------------------------------------------------
# the pre-registration
# --------------------------------------------------------------------------


def test_the_preregistration_is_hashed_into_every_artifact():
    stamp = prereg.stamp()
    assert stamp["preregistration_sha256"] == prereg.digest()
    assert len(stamp["preregistration_sha256"]) == 64


def test_the_archived_preregistrations_are_untouched():
    """The v0.1.1 and hardened artifacts carry those hashes."""
    v1 = BENCH / "harness" / "preregistration.json"
    assert v1.exists()
    import hashlib
    assert hashlib.sha256(v1.read_bytes()).hexdigest() == (
        "4658a1a538411e0f15bf9c5d57ee50ede85bcd2d7929b8c7045262732c2eb1ad")


def test_the_scorecard_does_not_read_any_legacy_artifact():
    text = "\n".join(p.read_text(encoding="utf-8") for p in sources(TOKENBURN))
    for forbidden in ("results/v0.1.1", "results/trials", "scenario_verdict",
                      "hardened_verdict", "s1-dead-end", "s2-hidden",
                      "s3-working"):
        assert forbidden not in text, (
            f"the Token-Burn scorecard references {forbidden!r}; legacy "
            f"artifacts must not be inputs to it")


# --------------------------------------------------------------------------
# the readiness gate's own arithmetic
# --------------------------------------------------------------------------


def test_the_readiness_gate_does_not_block_an_authorized_live_run():
    """The dry run reports "not authorized" as information, not as a failure.

    The first version of this check was fatal in every mode, which meant that
    the moment somebody legitimately exported VELRA_ALLOW_LIVE_BENCHMARK=1 the
    readiness gate failed and `--live` became impossible. The two modes want
    opposite answers from the same fact, so they ask two different questions.
    """
    text = (TOKENBURN / "run.py").read_text(encoding="utf-8")
    assert 'rep.add("safety", "live execution is authorized",' in text, (
        "live mode must assert that the gate is open")
    assert 'rep.add("safety", "live execution is not currently authorized",' \
        in text
    # The offline form must be non-fatal; the live form must not be.
    offline = text.index('"live execution is not currently authorized"')
    assert "fatal=False" in text[offline:offline + 500], (
        "an exported environment variable must not fail a dry run")
