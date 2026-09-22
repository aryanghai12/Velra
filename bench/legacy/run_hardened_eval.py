#!/usr/bin/env python3
"""The v0.1.2 hardened efficacy evaluation.

    python bench/legacy/run_hardened_eval.py --dry-run      # free: prove readiness
    python bench/legacy/run_hardened_eval.py --live         # spends money

Nothing in this script spends an API token unless ``--live`` is given. That is
the whole shape of it: every check that can be made offline is made offline,
first, and the expensive part is a single explicit flag at the end.

The design it runs is registered in ``bench/harness/preregistration_v2.json``,
which is hashed into every artifact. ``preregistration.json`` (1.0.0) stays on
disk untouched because the v0.1.1 results carry its hash and have to stay
interpretable under the rules they were collected beneath.

Phases
------

    0  environment            tool versions, Claude Code binary, Python, git
    1  repository provenance  clean tree, binary built from HEAD
    2  build                  cargo build --release -p velra
    3  unit and self tests    cargo test (with fault-injection) + bench pytest
                              + harness selftest
    4  fixtures               generate each scenario, verify ground truth by
                              running its real pytest suite
    5  leak scans             five surfaces per scenario; a fatal hit aborts
    6  manifests              every scenario declares target facts, a compaction
                              boundary inside its turn script, and a measured
                              turn after it
    7  plan                   the matched-pair schedule, written out
    ---------------------------------------------------------------- free ----
    8  controls               per-scenario information-loss probes  [--live]
    9  trials                 both arms of every pair               [--live]
    10 analysis               telemetry, tokens, stages, aggregate, verdicts

Resumption
----------

``--resume`` skips any trial whose directory already holds a ``trial_meta.json``
and any control whose JSON already exists. ``--only`` narrows to named
scenarios, ``--pairs`` to named pair ids. Nothing is ever overwritten in place:
a re-run of an existing trial requires ``--force``, which moves the old one into
``quarantine/`` with a timestamp rather than deleting it.

Settings safety
---------------

The trials register and unregister Velra's hooks in the real user-level Claude
Code settings file, because that is the thing under test. This script snapshots
that file before phase 8 and restores it in a ``finally``, and the trial driver
writes its own timestamped backup into ``~/.velra/backups`` regardless. Run it
from an ordinary terminal, not from inside a Claude Code session: a nested
session shares the settings file being mutated.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent
# This runner was archived into bench/legacy in v0.1.2 Phase 3. Its results,
# harness and binary paths still resolve against the original bench/ tree so
# that every historical artifact keeps the path it was recorded under.
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
HARNESS = BENCH / "harness"
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HARNESS))

import prereg  # noqa: E402
from scenarios import leaks, registry  # noqa: E402

RESULTS = BENCH / "results" / "v0.1.2"
TRIALS = RESULTS / "trials"
CONTROLS = RESULTS / "controls"
STAGES = RESULTS / "stages"
QUARANTINE = RESULTS / "quarantine"

#: Results directories that are historical record and must never be written to.
IMMUTABLE = (BENCH / "results" / "v0.1.1", BENCH / "results" / "trials",
             BENCH / "results" / "superseded")

#: The portable MinGW toolchain this machine uses for the release build. Absent
#: elsewhere, in which case the normal toolchain is already fine.
WINLIBS = pathlib.Path(os.path.expanduser(
    "~/AppData/Local/Programs/winlibs-mingw64/mingw64/bin"))

BINARY = REPO_ROOT / "target" / "release" / (
    "velra.exe" if os.name == "nt" else "velra")


# ---------------------------------------------------------------------------
# plumbing
# ---------------------------------------------------------------------------


class Abort(SystemExit):
    """A readiness gate that did not pass."""


def step(title: str) -> None:
    print()
    print("=" * 78)
    print(f"  {title}")
    print("=" * 78, flush=True)


def sh(cmd, check: bool = False, **kw) -> subprocess.CompletedProcess:
    printable = " ".join(str(c) for c in cmd)
    print(f"$ {printable}", flush=True)
    kw.setdefault("cwd", str(REPO_ROOT))
    result = subprocess.run([str(c) for c in cmd], **kw)
    if check and result.returncode != 0:
        raise Abort(f"failed: {printable}")
    return result


def capture(cmd, **kw) -> subprocess.CompletedProcess:
    kw.setdefault("cwd", str(REPO_ROOT))
    kw.setdefault("encoding", "utf-8")
    kw.setdefault("errors", "replace")
    return subprocess.run([str(c) for c in cmd], capture_output=True, text=True, **kw)


def build_env() -> dict:
    env = dict(os.environ)
    if WINLIBS.is_dir():
        env["PATH"] = str(WINLIBS) + os.pathsep + env.get("PATH", "")
    return env


def now_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


class Report:
    """Accumulates one line per check so the readiness gate is a list, not a mood."""

    def __init__(self) -> None:
        self.rows: list[dict] = []

    def add(self, phase: str, name: str, ok: bool, detail: str = "",
            fatal: bool = True) -> bool:
        self.rows.append({"phase": phase, "check": name, "ok": bool(ok),
                          "detail": detail, "fatal": fatal})
        mark = "PASS" if ok else ("FAIL" if fatal else "WARN")
        print(f"  [{mark}] {name}" + (f" -- {detail}" if detail else ""), flush=True)
        return bool(ok)

    @property
    def blocking(self) -> list[dict]:
        return [r for r in self.rows if not r["ok"] and r["fatal"]]

    @property
    def warnings(self) -> list[dict]:
        return [r for r in self.rows if not r["ok"] and not r["fatal"]]

    def to_json(self) -> dict:
        return {"checks": self.rows,
                "blocking": self.blocking,
                "warnings": self.warnings,
                "ready": not self.blocking}


# ---------------------------------------------------------------------------
# phases 0-7: free
# ---------------------------------------------------------------------------


def phase_environment(rep: Report, args) -> dict:
    step("Phase 0 - environment")
    info: dict = {"python": sys.version.split()[0], "platform": sys.platform,
                  "cwd": str(REPO_ROOT)}

    rep.add("environment", "python >= 3.11", sys.version_info >= (3, 11),
            info["python"])

    cargo = capture(["cargo", "--version"], env=build_env())
    info["cargo"] = (cargo.stdout or cargo.stderr).strip()
    rep.add("environment", "cargo available", cargo.returncode == 0, info["cargo"])

    if WINLIBS.is_dir():
        rep.add("environment", "MinGW toolchain on PATH", True, str(WINLIBS),
                fatal=False)

    try:
        import claude_binary  # noqa: E402
        claude = claude_binary.resolve()
        info["claude_binary"] = str(claude)
        info["claude_version"] = claude_binary.version_of(claude)
        rep.add("environment", "Claude Code binary resolved",
                bool(claude) and pathlib.Path(claude).exists(),
                f"{info['claude_version']} at {claude}")
    except Exception as exc:  # noqa: BLE001 - the whole point is to report it
        info["claude_binary_error"] = str(exc)
        rep.add("environment", "Claude Code binary resolved", False, str(exc))

    nested = [k for k in ("CLAUDECODE", "CLAUDE_CODE_SSE_PORT",
                          "CLAUDE_CODE_ENTRYPOINT") if os.environ.get(k)]
    info["nested_session_vars"] = nested
    rep.add("environment", "not running inside a Claude Code session",
            not nested,
            ("found " + ", ".join(nested) + "; the trials mutate the settings "
             "file this session is reading") if nested else "",
            fatal=args.live)

    info["preregistration"] = prereg.stamp_v2()
    rep.add("environment", "preregistration v2 loads and hashes", True,
            info["preregistration"]["preregistration_sha256"][:16])
    # v1 must be byte-identical to what the v0.1.1 artifacts were scored under.
    v1_hash = prereg.digest(prereg.PATH)
    info["preregistration_v1_sha256"] = v1_hash
    rep.add("environment", "preregistration v1 unchanged",
            v1_hash == "4658a1a538411e0f15bf9c5d57ee50ede85bcd2d7929b8c7045262732c2eb1ad",
            v1_hash[:16])
    return info


def phase_provenance(rep: Report, args) -> dict:
    step("Phase 1 - repository and binary provenance")
    head = capture(["git", "rev-parse", "HEAD"]).stdout.strip()
    status = capture(["git", "status", "--porcelain"]).stdout.strip()
    branch = capture(["git", "rev-parse", "--abbrev-ref", "HEAD"]).stdout.strip()
    dirty = [line for line in status.splitlines() if line.strip()]
    info = {"git_head": head, "git_branch": branch,
            "working_tree_changes": dirty,
            "working_tree_clean": not dirty}

    rep.add("provenance", "working tree clean", not dirty,
            f"{len(dirty)} changed path(s)" if dirty else "",
            fatal=not args.allow_dirty)

    for path in IMMUTABLE:
        if not path.exists():
            continue
        touched = [d for d in dirty if path.relative_to(REPO_ROOT).as_posix()
                   in d.replace("\\", "/")]
        rep.add("provenance", f"historical results untouched: {path.name}",
                not touched, "; ".join(touched[:3]))
    return info


def phase_build(rep: Report, args) -> dict:
    step("Phase 2 - build the release binary")
    if args.skip_build:
        rep.add("build", "release binary present", BINARY.exists(),
                "skipped build (--skip-build)")
        return {"built": False, "binary": str(BINARY)}
    ok = sh(["cargo", "build", "--release", "-p", "velra"],
            env=build_env()).returncode == 0
    rep.add("build", "cargo build --release -p velra", ok)
    rep.add("build", "binary exists", BINARY.exists(), str(BINARY))

    version = capture([BINARY, "--version"]).stdout.strip() if BINARY.exists() else ""
    info = {"built": ok, "binary": str(BINARY), "reported_version": version,
            "bytes": BINARY.stat().st_size if BINARY.exists() else None}
    if BINARY.exists():
        result = capture([sys.executable, HARNESS / "provenance.py",
                          "--binary", BINARY, "--out", RESULTS / "provenance.json"])
        if (RESULTS / "provenance.json").exists():
            prov = json.loads((RESULTS / "provenance.json").read_text(encoding="utf-8"))
            info["provenance"] = prov
            rep.add("build", "binary was built from HEAD",
                    bool(prov.get("commit_matches_binary")),
                    f"{prov.get('embedded_commit')} vs {prov.get('git_head_short9')}",
                    fatal=not args.allow_dirty)
        else:
            rep.add("build", "provenance recorded", False,
                    (result.stderr or "")[:200])
    return info


def phase_tests(rep: Report, args) -> dict:
    step("Phase 3 - unit and self tests")
    info: dict = {}
    if args.skip_tests:
        rep.add("tests", "test suites", False, "skipped (--skip-tests)",
                fatal=True)
        return info

    # `--features fault-injection` is not optional here. Without it the storage
    # stress test measures the 250 ms hook watchdog instead of the storage
    # layer and loses events on a loaded machine; see the note on
    # `c1_parallel_processes_lose_no_events_and_create_no_duplicates`.
    rust = sh(["cargo", "test", "--workspace", "--features", "fault-injection"],
              env=build_env())
    info["cargo_test_exit"] = rust.returncode
    rep.add("tests", "cargo test --workspace --features fault-injection",
            rust.returncode == 0)

    bench = sh([sys.executable, "-m", "pytest", "bench/tests", "-q"])
    info["bench_pytest_exit"] = bench.returncode
    rep.add("tests", "pytest bench/tests", bench.returncode == 0)

    self_test = sh([sys.executable, HARNESS / "selftest.py"],
                   stdout=subprocess.DEVNULL if args.quiet else None)
    info["selftest_exit"] = self_test.returncode
    rep.add("tests", "harness selftest (synthetic end-to-end pipeline)",
            self_test.returncode == 0)
    return info


def phase_fixtures(rep: Report, args, scenarios: list[str],
                   fixture_root: pathlib.Path) -> dict:
    step("Phase 4 - generate and verify fixtures")
    info: dict = {}
    for name in scenarios:
        scenario = registry.get(name)
        dest = fixture_root / f"readiness-{name}"
        try:
            manifest = scenario.build(dest, verify=True)
        except SystemExit as exc:
            rep.add("fixtures", f"{name}: fixture builds and verifies", False,
                    str(exc)[:400])
            info[name] = {"error": str(exc)}
            continue
        truths = {k: v["observed"] for k, v in manifest["ground_truth"].items()}
        expected = {k: v["expected"] for k, v in manifest["ground_truth"].items()}
        ok = truths == expected
        rep.add("fixtures", f"{name}: ground truth verified by running pytest",
                ok, json.dumps(truths))
        info[name] = {"manifest": manifest, "ground_truth": truths,
                      "fixture_seed": manifest.get("fixture_seed"),
                      "repo": str(dest)}
    return info


def phase_leaks(rep: Report, args, scenarios: list[str], fixtures: dict) -> dict:
    step("Phase 5 - leak scans across every surface an agent can read")
    info: dict = {}
    for name in scenarios:
        entry = fixtures.get(name) or {}
        if "manifest" not in entry:
            rep.add("leaks", f"{name}: leak scan", False, "no fixture to scan")
            continue
        scenario = registry.get(name)
        result = leaks.scan(pathlib.Path(entry["repo"]), scenario.turns,
                            scenario.measured_index, scenario.leak_terms)
        info[name] = result
        rep.add("leaks", f"{name}: no fatal leak on any surface",
                result["clean"], leaks.format_report(result)[:400])
    return info


def phase_manifests(rep: Report, args, scenarios: list[str],
                    fixtures: dict) -> dict:
    step("Phase 6 - scenario manifests and causal claims")
    info: dict = {}
    for name in scenarios:
        scenario = registry.get(name)
        turns = list(scenario.turns)
        checks = {
            "declares_target_facts": bool(scenario.target_facts),
            "boundary_inside_turn_script": 0 <= scenario.compact_index < len(turns),
            "measured_turn_after_boundary":
                scenario.compact_index < scenario.measured_index < len(turns),
            "compact_turn_is_a_compact_command":
                turns[scenario.compact_index].strip().startswith("/compact"),
            "measured_prompt_is_identical_for_both_arms": True,
            "target_facts_carry_probes": all(
                f.probe and f.recalled_markers for f in scenario.target_facts),
            "target_facts_carry_ledger_queries": all(
                "?1" in f.ledger_sql for f in scenario.target_facts),
            "target_facts_carry_capsule_markers": all(
                f.capsule_markers for f in scenario.target_facts),
            "target_facts_justify_necessity": all(
                len(f.necessary_because) > 80 for f in scenario.target_facts),
        }
        failed = sorted(k for k, ok in checks.items() if not ok)
        info[name] = {
            "checks": checks,
            "failed": failed,
            "measured_prompt": turns[scenario.measured_index],
            "target_facts": [f.id for f in scenario.target_facts],
            "fixture_seed": (fixtures.get(name) or {}).get("fixture_seed"),
        }
        rep.add("manifests", f"{name}: manifest complete", not failed,
                ", ".join(failed))
    return info


def plan_pairs(scenarios: list[str], replicates: int,
               only_pairs: list[str] | None) -> list[dict]:
    plan = []
    for name in scenarios:
        for replicate in range(1, replicates + 1):
            pair_id = f"{name}#r{replicate}"
            if only_pairs and pair_id not in only_pairs:
                continue
            plan.append({
                "pair_id": pair_id,
                "scenario": name,
                "replicate": replicate,
                "arms": {
                    arm: {"trial": f"{name}-{arm}-r{replicate}",
                          "dir": str(TRIALS / f"{name}-{arm}-r{replicate}")}
                    for arm in ("baseline", "velra")
                },
            })
    return plan


def phase_plan(rep: Report, args, scenarios: list[str],
               only_pairs: list[str] | None) -> dict:
    step("Phase 7 - matched-pair schedule")
    plan = plan_pairs(scenarios, args.replicates, only_pairs)
    symmetric = all(set(p["arms"]) == {"baseline", "velra"} for p in plan)
    rep.add("plan", "every pair schedules both arms", symmetric,
            f"{len(plan)} pairs, {2 * len(plan)} sessions")
    rep.add("plan", "replicates meet the registered minimum",
            args.replicates >= prereg.stamp_v2()["minimum_replicates_per_arm"],
            f"--replicates {args.replicates}", fatal=False)
    for p in plan:
        done = {arm: pathlib.Path(v["dir"], "trial_meta.json").exists()
                for arm, v in p["arms"].items()}
        p["already_on_disk"] = done
    return {"pairs": plan, "sessions": 2 * len(plan),
            "controls": len(scenarios) * args.control_replicates}


# ---------------------------------------------------------------------------
# phases 8-10: live
# ---------------------------------------------------------------------------


def settings_path() -> pathlib.Path:
    config = os.environ.get("CLAUDE_CONFIG_DIR")
    base = pathlib.Path(config) if config else pathlib.Path.home() / ".claude"
    return base / "settings.json"


def snapshot_settings() -> tuple[pathlib.Path, bytes | None]:
    path = settings_path()
    data = path.read_bytes() if path.exists() else None
    if data is not None:
        keep = RESULTS / "settings-backup" / f"settings.{now_stamp()}.json"
        keep.parent.mkdir(parents=True, exist_ok=True)
        keep.write_bytes(data)
        print(f"  settings snapshot -> {keep}", flush=True)
    return path, data


def restore_settings(path: pathlib.Path, data: bytes | None) -> None:
    try:
        if data is None:
            path.unlink(missing_ok=True)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        print(f"  settings restored: {path}", flush=True)
    except OSError as exc:
        print(f"  WARNING: could not restore {path}: {exc}", file=sys.stderr)


def quarantine(trial_dir: pathlib.Path, why: str) -> None:
    if not trial_dir.exists():
        return
    dest = QUARANTINE / now_stamp() / trial_dir.name
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(trial_dir), str(dest))
    log = QUARANTINE / "log.jsonl"
    with open(log, "a", encoding="utf-8", newline="") as fh:
        fh.write(json.dumps({"at": now_stamp(), "trial": trial_dir.name,
                             "moved_to": str(dest), "why": why}) + "\n")
    print(f"  quarantined {trial_dir.name} -> {dest}", flush=True)


def phase_controls(args, scenarios: list[str],
                   fixture_root: pathlib.Path) -> dict:
    step("Phase 8 - per-scenario information-loss controls (Velra disabled)")
    CONTROLS.mkdir(parents=True, exist_ok=True)
    sh([BINARY, "disable"])
    out = {}
    for name in scenarios:
        target = CONTROLS / f"{name}.json"
        if args.resume and target.exists():
            print(f"  resume: {target.name} already on disk", flush=True)
            out[name] = json.loads(target.read_text(encoding="utf-8"))
            continue
        code = sh([sys.executable, HARNESS / "loss_probe.py",
                   "--scenario", name,
                   "--fixture", fixture_root / f"control-{name}",
                   "--replicates", args.control_replicates,
                   "--model", args.model,
                   "--max-budget-usd", args.max_budget_usd,
                   "--out", target]).returncode
        if code != 0:
            raise Abort(f"control for {name} failed")
        out[name] = json.loads(target.read_text(encoding="utf-8"))
        for fact_id, rows in (out[name].get("target_facts") or {}).items():
            usable = [r for r in rows if r["valid"]]
            lost = [r for r in usable if not r["recalled"]]
            if usable and len(lost) * 2 <= len(usable):
                print(f"\n  NOTE: {name}/{fact_id} SURVIVED compaction in "
                      f"{len(usable) - len(lost)}/{len(usable)} control runs. "
                      f"This scenario is NOT CAUSALLY TESTABLE and its "
                      f"hypothesis will say so however the arms score.\n",
                      flush=True)
    return out


def phase_trials(args, plan: list[dict], fixture_root: pathlib.Path) -> list[pathlib.Path]:
    step("Phase 9 - matched-pair trials")
    TRIALS.mkdir(parents=True, exist_ok=True)
    done: list[pathlib.Path] = []
    for pair in plan:
        for arm in ("baseline", "velra"):
            spec = pair["arms"][arm]
            out = pathlib.Path(spec["dir"])
            done.append(out)
            if (out / "trial_meta.json").exists():
                if args.resume and not args.force:
                    print(f"  resume: {spec['trial']} already on disk", flush=True)
                    continue
                if args.force:
                    quarantine(out, "re-run with --force")
                else:
                    raise Abort(
                        f"{spec['trial']} already exists. Use --resume to keep "
                        f"it or --force to quarantine it and run again.")
            step(f"  trial {spec['trial']}")
            code = sh([sys.executable, HARNESS / "scenario_trial.py",
                       "--scenario", pair["scenario"], "--arm", arm,
                       "--replicate", pair["replicate"],
                       "--pair-id", pair["pair_id"],
                       "--model", args.model,
                       "--out", out,
                       "--fixture", fixture_root / spec["trial"],
                       "--max-budget-usd", args.max_budget_usd]).returncode
            if code != 0:
                raise Abort(f"trial {spec['trial']} failed")
    return done


def phase_analysis(args, trials: list[pathlib.Path], controls: dict) -> int:
    step("Phase 10 - analysis")
    present = [d for d in trials if (d / "trial_meta.json").exists()]
    if not present:
        raise Abort("no trials on disk to analyse")

    for d in present:
        sh([sys.executable, HARNESS / "scenario_analyze.py", "--trial", d])

    if not args.skip_tokens:
        for d in present:
            if (d / "velra.db").exists():
                sh([sys.executable, HARNESS / "measure_tokens.py", "--trial", d,
                    "--model", args.model])
    for d in present:
        if (d / "native_summary.txt").exists() and not args.skip_native_tokens:
            sh([sys.executable, HARNESS / "native_tokens.py", "--trial", d,
                "--model", args.model])

    STAGES.mkdir(parents=True, exist_ok=True)
    for d in present:
        meta = json.loads((d / "trial_meta.json").read_text(encoding="utf-8"))
        control = CONTROLS / f"{meta['scenario']}.json"
        sh([sys.executable, HARNESS / "stages.py", "--trial", d,
            *(["--control", control] if control.exists() else []),
            "--out", STAGES / f"{d.name}.json"])

    sh([sys.executable, HARNESS / "scenario_aggregate.py",
        *[str(d) for d in present], "--out", RESULTS / "aggregate.json"])
    # The v0.1.2 verdicts are decided from the staged evidence against
    # preregistration_v2.json. `scenario_verdict.py` stays where it is and
    # still decides E1-E6 for the v0.1.1 artifacts; it is not run here.
    return sh([sys.executable, HARNESS / "hardened_verdict.py",
               "--results", RESULTS]).returncode


# ---------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--dry-run", action="store_true", default=True,
                      help="default. Phases 0-7 only: no API calls, no money.")
    mode.add_argument("--live", action="store_true",
                      help="run the expensive evaluation after the readiness "
                           "gate passes. Roughly $40-80 at the registered "
                           "minimum of 4 replicates across 3 scenarios.")

    ap.add_argument("--only", nargs="*", metavar="SCENARIO",
                    choices=list(registry.SCENARIOS),
                    help="restrict to these scenarios")
    ap.add_argument("--pairs", nargs="*", metavar="PAIR_ID",
                    help="restrict to these pair ids, e.g. s1-dead-end-pair#r2")
    ap.add_argument("--replicates", type=int, default=4)
    ap.add_argument("--control-replicates", type=int, default=2)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--max-budget-usd", type=float, default=25.0)
    ap.add_argument("--fixture-root", default=None)

    ap.add_argument("--resume", action="store_true",
                    help="keep trials and controls already on disk")
    ap.add_argument("--force", action="store_true",
                    help="re-run trials that exist, quarantining the old ones")
    ap.add_argument("--allow-dirty", action="store_true",
                    help="proceed with a dirty tree or a stale binary, "
                         "recording the provenance failure in every artifact")
    ap.add_argument("--skip-build", action="store_true")
    ap.add_argument("--skip-tests", action="store_true",
                    help="not recommended: the gate exists to stop a run "
                         "producing evidence about a binary whose known "
                         "defects have come back")
    ap.add_argument("--skip-tokens", action="store_true")
    ap.add_argument("--skip-native-tokens", action="store_true")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    scenarios = list(args.only or registry.DEFAULT_ORDER)
    fixture_root = pathlib.Path(
        args.fixture_root
        or (pathlib.Path(tempfile.gettempdir()) / "velra-hardened-fixtures"))
    fixture_root.mkdir(parents=True, exist_ok=True)
    RESULTS.mkdir(parents=True, exist_ok=True)

    started = time.time()
    rep = Report()
    readiness = {
        "at": now_stamp(),
        "mode": "live" if args.live else "dry-run",
        "scenarios": scenarios,
        "replicates": args.replicates,
        "results_dir": str(RESULTS),
        **prereg.stamp_v2(),
    }

    readiness["environment"] = phase_environment(rep, args)
    readiness["provenance"] = phase_provenance(rep, args)
    readiness["build"] = phase_build(rep, args)
    readiness["tests"] = phase_tests(rep, args)
    fixtures = phase_fixtures(rep, args, scenarios, fixture_root)
    readiness["fixtures"] = {k: {kk: vv for kk, vv in v.items() if kk != "manifest"}
                             for k, v in fixtures.items()}
    readiness["leak_scans"] = phase_leaks(rep, args, scenarios, fixtures)
    readiness["manifests"] = phase_manifests(rep, args, scenarios, fixtures)
    readiness["plan"] = phase_plan(rep, args, scenarios, args.pairs)
    readiness.update(rep.to_json())

    # v0.1.2 Phase 3 gave `readiness.json` to the Token-Burn benchmark, which
    # is the v0.1.2 efficacy scorecard and the single authoritative current
    # readiness artifact. This archived runner writes into its own directory so
    # that it can overwrite neither that artifact nor the frozen snapshot of
    # its own last run, which sits beside this path under a timestamped name.
    out = RESULTS / "hardened" / "readiness.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(readiness, indent=2, default=str),
                   encoding="utf-8", newline="")

    step("Readiness")
    for row in rep.blocking:
        print(f"  BLOCKING  {row['phase']}/{row['check']}: {row['detail']}")
    for row in rep.warnings:
        print(f"  warning   {row['phase']}/{row['check']}: {row['detail']}")
    verdict = "READY FOR LIVE EVALUATION" if not rep.blocking \
        else "NOT READY FOR LIVE EVALUATION"
    print(f"\n  {verdict}")
    print(f"  readiness report: {out}")
    print(f"  {readiness['plan']['sessions']} trial sessions + "
          f"{readiness['plan']['controls']} control sessions planned")
    print(f"  elapsed: {time.time() - started:.0f}s")

    if not args.live:
        print("\n  Dry run: nothing was sent to the API. Re-run with --live to "
              "execute the evaluation.")
        return 0 if not rep.blocking else 1

    if rep.blocking:
        print("\n  Refusing to run live with blocking failures.", file=sys.stderr)
        return 1

    path, saved = snapshot_settings()
    try:
        controls = phase_controls(args, scenarios, fixture_root)
        trials = phase_trials(args, readiness["plan"]["pairs"], fixture_root)
        code = phase_analysis(args, trials, controls)
    finally:
        restore_settings(path, saved)

    print(f"\ntotal wall time: {time.time() - started:.0f}s")
    print(f"results:   {RESULTS}")
    print(f"stages:    {STAGES}")
    print(f"verdicts:  {RESULTS / 'verdicts_v2.json'}")
    print("\nNothing here writes the report. Read the raw artifacts and "
          "recompute the headline numbers before believing any summary.")
    return code


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Abort as exc:
        print(f"\nABORTED: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
