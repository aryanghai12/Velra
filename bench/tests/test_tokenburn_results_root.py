"""`--results-root`: a fresh run never touches the frozen evidence.

``bench/results/v0.1.2`` is the qualification evidence frozen at 7a09e65. The
runner used to write readiness.json, run_state.json, trials/ and the aggregate
into that very tree, so a fresh run would have collided with it, needed
--force to move it, or aggregated old and new trials together. These tests run
the real CLI offline (``--dry-run``; no Claude, no network) and drive the trial
loop with a stub in place of the live driver.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import subprocess
import sys
from types import SimpleNamespace

import pytest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
REPO = BENCH.parent
RUNNER = BENCH / "tokenburn" / "run.py"
FROZEN = BENCH / "results" / "v0.1.2"
sys.path.insert(0, str(BENCH))

from tokenburn import aggregate  # noqa: E402
from tokenburn import run as run_mod  # noqa: E402
from tokenburn import runroot  # noqa: E402
from tokenburn.mock_adapter import MockSpec, write_trial  # noqa: E402


def tree_hashes(root: pathlib.Path) -> dict[str, str]:
    return {str(p.relative_to(root)).replace("\\", "/"):
            hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(root.rglob("*")) if p.is_file()}


def git_status() -> str:
    return subprocess.run(["git", "status", "--porcelain"],
                          cwd=REPO, capture_output=True, text=True,
                          encoding="utf-8").stdout


def dry_run(root: str, cwd: pathlib.Path, work: pathlib.Path):
    return subprocess.run(
        [sys.executable, str(RUNNER), "--dry-run", "--skip-tests",
         "--quick-ladder", "--results-root", root,
         "--fixture-root", str(work)],
        cwd=str(cwd), capture_output=True, text=True, encoding="utf-8",
        errors="replace", timeout=900)


@pytest.fixture(scope="module")
def runs(tmp_path_factory):
    """Two dry runs into one absolute root, one into a relative root, with
    the frozen tree hashed before and after all three."""
    base = tmp_path_factory.mktemp("results-root")
    frozen_before = tree_hashes(FROZEN)
    status_before = git_status()
    absolute = base / "fresh-run"
    first = dry_run(str(absolute), REPO, base / "work1")
    readiness_first = json.loads((absolute / "readiness.json").read_text(
        encoding="utf-8"))
    second = dry_run(str(absolute), REPO, base / "work2")
    readiness_second = json.loads((absolute / "readiness.json").read_text(
        encoding="utf-8"))
    relative_cwd = base / "cwd"
    relative_cwd.mkdir()
    relative = dry_run("rel/run", relative_cwd, base / "work3")
    return SimpleNamespace(
        base=base, absolute=absolute, first=first, second=second,
        readiness_first=readiness_first, readiness_second=readiness_second,
        relative=relative, relative_root=relative_cwd / "rel" / "run",
        frozen_before=frozen_before, frozen_after=tree_hashes(FROZEN),
        status_before=status_before, status_after=git_status())


# A --------------------------------------------------------------------------


def test_a_fresh_root_receives_the_runs_output(runs):
    assert runs.first.returncode in (0, 1), runs.first.stderr[-2000:]
    assert (runs.absolute / "readiness.json").is_file()
    assert (runs.absolute / ".gitignore").is_file()
    rr = runs.readiness_first["results_root"]
    assert rr["explicit"] is True
    assert pathlib.Path(rr["root"]) == runs.absolute.resolve()
    assert pathlib.Path(rr["readiness"]) == runs.absolute / "readiness.json"
    assert pathlib.Path(rr["trials"]) == runs.absolute / "trials"
    assert pathlib.Path(rr["run_state"]) == runs.absolute / "run_state.json"
    assert pathlib.Path(rr["aggregate_dir"]) == runs.absolute
    assert pathlib.Path(rr["settings_backup"]) == runs.absolute / "settings-backup"
    assert pathlib.Path(rr["quarantine"]) == runs.absolute / "quarantine"


# B --------------------------------------------------------------------------


def test_the_frozen_tree_is_byte_identical_after_offline_runs(runs):
    assert runs.frozen_after == runs.frozen_before
    assert len(runs.frozen_before) > 500
    # Nothing anywhere in the repository changed either.
    assert runs.status_after == runs.status_before


# C / D ----------------------------------------------------------------------


def test_the_plan_points_only_into_the_root_and_finds_nothing_old(runs):
    trials = runs.readiness_first["results_root"]["trials"]
    for stage in ("qualification", "formal"):                       # G too
        pairs = runs.readiness_first["trial_plan"][stage]["pairs"]
        assert pairs
        for pair in pairs:
            for arm in pair["arms"].values():
                assert arm["dir"].startswith(trials), arm["dir"]
            # The frozen tree holds trials of the same names; none is seen.
            assert not any(pair["already_on_disk"].values()), pair["pair_id"]


def test_aggregation_sees_only_the_roots_trials(tmp_path):
    trials = tmp_path / "root" / "trials"
    for arm in ("baseline", "velra"):
        write_trial(trials, MockSpec(trial=f"new-{arm}",
                                     scenario="A_cold_continuation",
                                     arm=arm, pair_id="new#1"))
    result = aggregate.run(trials, tmp_path / "root", write=True)
    assert result["n_trials"] == 2
    assert {p["pair_id"] for p in result["pairs"]} == {"new#1"}
    assert pathlib.Path(result["trials_root"]) == trials


def test_historical_trials_outside_the_root_are_not_discovered(tmp_path):
    frozen_trials = FROZEN / "tokenburn" / "trials"
    assert any(frozen_trials.iterdir()), "the frozen trials are expected"
    root = runroot.select(tmp_path / "fresh")
    plan = run_mod.plan_pairs(["A_cold_continuation"], 2, "q",
                              trials_dir=root.trials)
    # Same trial names as the frozen run, different directories.
    names = {a["trial"] for p in plan for a in p["arms"].values()}
    assert "A_cold_continuation-q1-velra" in names
    assert (frozen_trials / "A_cold_continuation-q1-velra").is_dir()
    for pair in plan:
        for arm in pair["arms"].values():
            assert pathlib.Path(arm["dir"]).parent == root.trials
    assert aggregate.run(root.trials, root.root, write=False)["n_trials"] == 0


# E --------------------------------------------------------------------------


def test_a_second_dry_run_into_the_same_root_is_the_same_run(runs):
    assert runs.second.returncode == runs.first.returncode
    for key in ("results_root", "trial_plan", "benchmarks",
                "preregistration_sha256", "blockers"):
        first, second = runs.readiness_first[key], runs.readiness_second[key]
        if key == "blockers":
            first = [(b["phase"], b["check"]) for b in first]
            second = [(b["phase"], b["check"]) for b in second]
        assert first == second, key
    assert sorted(p.name for p in runs.absolute.iterdir()) == \
        [".gitignore", "readiness.json"]


class StubLive:
    """Stands in for live_trial: writes a synthetic trial, starts nothing."""

    def __init__(self):
        self.calls: list[str] = []

    def run_trial(self, *, scenario_name, arm, pair_id, out, **kwargs):
        self.calls.append(pathlib.Path(out).name)
        write_trial(pathlib.Path(out).parent, MockSpec(
            trial=pathlib.Path(out).name, scenario=scenario_name, arm=arm,
            pair_id=pair_id))
        return {}


def _args(**kw):
    base = dict(resume=False, force=False, rung=250_000, model="mock",
                max_budget_usd=0.0, seed=0)
    base.update(kw)
    return SimpleNamespace(**base)


@pytest.fixture
def selected(tmp_path, monkeypatch):
    root = runroot.select(tmp_path / "live-root")
    monkeypatch.setattr(run_mod, "RUN", root)
    return root


@pytest.mark.parametrize("stage", ["q", "f"])                     # G
def test_the_trial_loop_writes_only_under_the_root(selected, tmp_path, stage):
    before = tree_hashes(FROZEN)
    plan = run_mod.plan_pairs(["B_clear_survival"], 2 if stage == "q" else 4,
                              stage)
    stub = StubLive()
    run_mod._run_trials(_args(), plan, stub, pathlib.Path("claude"),
                        tmp_path / "fixtures", {"trials": {}})
    assert len(stub.calls) == 2 * len(plan)
    assert selected.run_state.is_file()
    for name in ("aggregate.json", "verdicts.json", "report.md"):
        assert (selected.root / name).is_file(), name
    state = json.loads(selected.run_state.read_text(encoding="utf-8"))
    assert set(state["verdicts"]) == {p["pair_id"] for p in plan}
    assert all(k.startswith(f"B_clear_survival-{stage}") for k in state["trials"])
    assert tree_hashes(FROZEN) == before


def test_rerunning_the_loop_follows_the_existing_resume_semantics(
        selected, tmp_path):
    plan = run_mod.plan_pairs(["A_cold_continuation"], 1, "q")
    run_mod._run_trials(_args(), plan, StubLive(), pathlib.Path("claude"),
                        tmp_path / "fx", {"trials": {}})
    # Again, without --resume: the existing trial stops the run.
    with pytest.raises(SystemExit, match="exists"):
        run_mod._run_trials(_args(), plan, StubLive(), pathlib.Path("claude"),
                            tmp_path / "fx", {"trials": {}})
    # With --resume: nothing is re-run and the aggregate is the same.
    stub = StubLive()
    first = json.loads((selected.root / "verdicts.json").read_text())
    run_mod._run_trials(_args(resume=True), plan, stub, pathlib.Path("claude"),
                        tmp_path / "fx", {"trials": {}})
    assert stub.calls == []
    assert json.loads((selected.root / "verdicts.json").read_text())["pairs"] \
        == first["pairs"]
    # With --force: the old trial is quarantined *inside the root*.
    run_mod._run_trials(_args(force=True), plan, StubLive(),
                        pathlib.Path("claude"), tmp_path / "fx", {"trials": {}})
    assert (selected.quarantine / "log.jsonl").is_file()


# F --------------------------------------------------------------------------


@pytest.mark.parametrize("value", [
    None,                                   # the default layout itself
    str(FROZEN),
    str(FROZEN / "tokenburn"),
    str(FROZEN / "tokenburn" / "trials" / "new"),
    str(BENCH / "results"),                 # contains the evidence
    str(REPO),
])
def test_the_frozen_evidence_is_never_a_writable_root(value):
    with pytest.raises(runroot.ProtectedEvidenceError):
        runroot.select(value).assert_writable()


def test_the_cli_refuses_the_frozen_root_before_writing(tmp_path):
    before = tree_hashes(FROZEN)
    for extra in ([], ["--results-root", str(FROZEN)],
                  ["--results-root", "bench/results/v0.1.2/tokenburn"]):
        proc = subprocess.run(
            [sys.executable, str(RUNNER), "--dry-run", "--skip-tests", *extra],
            cwd=str(REPO), capture_output=True, text=True, encoding="utf-8",
            errors="replace", timeout=300)
        assert proc.returncode == 3, (extra, proc.stdout[-500:], proc.stderr)
        assert "REFUSED" in proc.stderr and "--results-root" in proc.stderr
    assert tree_hashes(FROZEN) == before


def test_aggregation_refuses_to_rewrite_frozen_trials():
    with pytest.raises(runroot.ProtectedEvidenceError):
        aggregate.run(FROZEN / "tokenburn" / "trials", FROZEN / "tokenburn",
                      write=True)
    # Reading them, without writing, is still allowed.
    assert aggregate.run(FROZEN / "tokenburn" / "trials", pathlib.Path("unused"),
                         write=False)["n_trials"] == 8


def test_a_sibling_whose_name_extends_the_frozen_one_is_allowed(tmp_path):
    root = runroot.select("bench/results/v0.1.2-requal", cwd=REPO)
    assert root.writable()
    assert root.repo_relative() == "bench/results/v0.1.2-requal/"


def test_a_tracked_change_inside_the_frozen_tree_is_never_excused():
    line = " M bench/results/v0.1.2/tokenburn/trials/x/analysis.json"
    assert run_mod.is_self_written(
        line, patterns=("bench/results/v0.1.2/tokenburn/",)) is False
    assert run_mod.is_self_written(
        "?? bench/results/v0.1.2/tokenburn/trials/x/new.json",
        patterns=("bench/results/v0.1.2/tokenburn/",)) is True


def test_an_explicit_root_inside_the_repo_is_its_own_self_written_output(
        monkeypatch):
    monkeypatch.setattr(run_mod, "RUN",
                        runroot.select("bench/results/v0.1.2-requal", cwd=REPO))
    assert run_mod.is_self_written("?? bench/results/v0.1.2-requal/")
    assert not run_mod.is_self_written("?? bench/results/v0.1.2/readiness.json")


# H --------------------------------------------------------------------------


def test_a_relative_root_resolves_against_the_working_directory(runs):
    assert runs.relative.returncode in (0, 1), runs.relative.stderr[-2000:]
    readiness = runs.relative_root / "readiness.json"
    assert readiness.is_file()
    rr = json.loads(readiness.read_text(encoding="utf-8"))["results_root"]
    assert rr["given"] == "rel/run"
    assert pathlib.Path(rr["root"]) == runs.relative_root.resolve()


def test_relative_and_absolute_selection_agree(tmp_path):
    a = runroot.select(tmp_path / "x")
    b = runroot.select("x", cwd=tmp_path)
    c = runroot.select(str(tmp_path / "y" / ".." / "x"))
    assert a.root == b.root == c.root
    assert a.trials == b.trials == tmp_path.resolve() / "x" / "trials"
