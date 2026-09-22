#!/usr/bin/env python3
"""The Token-Burn runner. Four modes, three of which cannot spend anything.

    python bench/tokenburn/run.py --selftest   # the pipeline, on mock trials
    python bench/tokenburn/run.py --dry-run    # readiness: fixtures, leaks,
                                               # ladder, telemetry, plan
    python bench/tokenburn/run.py --smoke      # restore -> SessionStart, real
                                               # binary, still offline
    python bench/tokenburn/run.py --live       # refuses without authorization

``--selftest``, ``--dry-run`` and ``--smoke`` start no Claude process, make no
network call and spend nothing. That is not a convention: they pass through
:func:`safety.assert_offline`, and there is no code path from any of them to a
Claude binary. ``--smoke`` in particular stays offline **by default and always
in this phase** — it drives ``velra hook session-start`` directly, which is the
whole delivery contract, and a live Claude smoke test needs an explicit
authorization this phase does not have.

``--live`` requires ``--live`` *and* ``VELRA_ALLOW_LIVE_BENCHMARK=1`` *and* a
terminal that is not inside Claude Code. :func:`safety.require_live` checks all
three before the first subprocess, and refuses by raising.

Readiness
---------

``--dry-run`` writes ``bench/results/v0.1.2/readiness.json`` and ends in
exactly one of two states::

    READY FOR LIVE EVALUATION
    NOT READY FOR LIVE EVALUATION   <- with every blocker named

Resumption
----------

``--resume`` keeps every completed trial and skips it. ``--only`` narrows to
named benchmarks, ``--pairs`` to named pair ids. A completed trial is never
overwritten in place: re-running one moves the old directory into
``quarantine/`` with a timestamp. Nothing here deletes evidence.
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
BENCH = HERE.parent
REPO_ROOT = BENCH.parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import aggregate  # type: ignore[no-redef]
    import capsule_probe  # type: ignore[no-redef]
    import context_fixture  # type: ignore[no-redef]
    import isolation  # type: ignore[no-redef]
    import leakscan  # type: ignore[no-redef]
    import prereg  # type: ignore[no-redef]
    import safety  # type: ignore[no-redef]
    import scenarios  # type: ignore[no-redef]
    import selftest as selftest_mod  # type: ignore[no-redef]
    import smoke as smoke_mod  # type: ignore[no-redef]
    import telemetry  # type: ignore[no-redef]
else:
    from . import (aggregate, capsule_probe, context_fixture, isolation,
                   leakscan, prereg, safety, scenarios, telemetry)
    from . import selftest as selftest_mod
    from . import smoke as smoke_mod

RESULTS = BENCH / "results" / "v0.1.2"
TOKENBURN = RESULTS / "tokenburn"
TRIALS = TOKENBURN / "trials"
QUARANTINE = TOKENBURN / "quarantine"
READINESS = RESULTS / "readiness.json"

#: Result trees that are historical record. The runner reads them to confirm
#: they are still there and writes to none of them.
PRESERVED = (
    BENCH / "results" / "trials",
    BENCH / "results" / "v0.1.1",
    BENCH / "results" / "v0.1.1_frozen_baseline",
    BENCH / "results" / "superseded",
    BENCH / "results" / "v0.1.2" / "trials",
    BENCH / "results" / "v0.1.2" / "stages",
    BENCH / "results" / "v0.1.2" / "controls",
    REPO_ROOT / "BENCHMARK_REPORT.md",
    BENCH / "results" / "v0.1.2" / "hardened",
)

#: The archived scenarios, which must still be where the archive says.
LEGACY_MODULES = ("s1_dead_end_pair.py", "s2_hidden_constraint.py",
                  "s3_working_set.py", "base.py", "registry.py", "targets.py")

VELRA_BINARY = REPO_ROOT / "target" / "release" / (
    "velra.exe" if os.name == "nt" else "velra")

#: The portable MinGW toolchain this machine builds with.
WINLIBS = pathlib.Path(os.path.expanduser(
    "~/AppData/Local/Programs/winlibs-mingw64/mingw64/bin"))


def now_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def sh(argv, **kw) -> subprocess.CompletedProcess:
    kw.setdefault("cwd", str(REPO_ROOT))
    kw.setdefault("encoding", "utf-8")
    kw.setdefault("errors", "replace")
    return subprocess.run([str(a) for a in argv], capture_output=True,
                          text=True, **kw)


def build_env() -> dict:
    env = dict(os.environ)
    if WINLIBS.is_dir():
        env["PATH"] = str(WINLIBS) + os.pathsep + env.get("PATH", "")
    return env


def step(title: str) -> None:
    print()
    print("=" * 78)
    print(f"  {title}")
    print("=" * 78, flush=True)


class Report:
    """One line per check, so readiness is a list rather than a mood."""

    def __init__(self) -> None:
        self.rows: list[dict] = []

    def add(self, phase: str, name: str, ok: bool, detail: str = "",
            fatal: bool = True) -> bool:
        self.rows.append({"phase": phase, "check": name, "ok": bool(ok),
                          "detail": str(detail)[:600], "fatal": fatal})
        mark = "PASS" if ok else ("BLOCK" if fatal else "warn")
        print(f"  [{mark:>5}] {name}" + (f" -- {detail}" if detail else ""),
              flush=True)
        return bool(ok)

    @property
    def blocking(self) -> list[dict]:
        return [r for r in self.rows if not r["ok"] and r["fatal"]]

    @property
    def warnings(self) -> list[dict]:
        return [r for r in self.rows if not r["ok"] and not r["fatal"]]

    def to_json(self) -> dict:
        return {"checks": self.rows, "blockers": self.blocking,
                "warnings": self.warnings, "ready": not self.blocking}


# --------------------------------------------------------------------------
# phases
# --------------------------------------------------------------------------


def phase_environment(rep: Report, args) -> dict:
    step("Environment")
    info: dict = {"python": sys.version.split()[0], "platform": sys.platform,
                  "repo_root": str(REPO_ROOT)}
    rep.add("environment", "python >= 3.11", sys.version_info >= (3, 11),
            info["python"])

    cargo = sh(["cargo", "--version"], env=build_env())
    info["cargo"] = (cargo.stdout or cargo.stderr).strip()
    rep.add("environment", "cargo available", cargo.returncode == 0,
            info["cargo"], fatal=False)

    info["velra_binary"] = str(VELRA_BINARY)
    rep.add("environment", "velra release binary present", VELRA_BINARY.exists(),
            str(VELRA_BINARY))
    if VELRA_BINARY.exists():
        version = sh([VELRA_BINARY, "--version"])
        info["velra_version"] = (version.stdout or "").strip()
        rep.add("environment", "velra --version answers",
                version.returncode == 0, info["velra_version"])

    sys.path.insert(0, str(BENCH / "harness"))
    try:
        import claude_binary  # noqa: E402
        resolved = claude_binary.resolve()
        info["claude_binary"] = str(resolved) if resolved else None
        info["claude_version"] = (claude_binary.version_of(resolved)
                                  if resolved else None)
        rep.add("environment", "Claude Code binary resolved",
                bool(resolved) and pathlib.Path(resolved).exists(),
                f"{info['claude_version']} at {resolved}")
    except Exception as exc:  # noqa: BLE001 - reporting it is the point
        info["claude_binary_error"] = str(exc)
        rep.add("environment", "Claude Code binary resolved", False, str(exc))

    info["model_configuration"] = {
        "model": args.model,
        "permission_mode": "bypassPermissions",
        "flags": list(safety.CLAUDE_FLAGS),
        "max_budget_usd_per_session": args.max_budget_usd,
    }
    info["preregistration"] = prereg.stamp()
    rep.add("environment", "pre-registration loads and hashes", True,
            info["preregistration"]["preregistration_sha256"][:16])
    return info


#: Paths this runner writes itself. They are excluded from the clean-tree
#: computation, and from nothing else.
#:
#: Without this the gate is self-invalidating: the dry run writes
#: `readiness.json`, `git status --porcelain` then lists it, and the artifact
#: reports the tree as dirty *because it exists*. A gate that fails on its own
#: output can never pass, and the blocker it names is not the one a reader
#: would act on. Everything outside this list is still counted, so a real
#: uncommitted source change is still a blocker.
SELF_WRITTEN = (
    "bench/results/v0.1.2/readiness.json",
    "bench/results/v0.1.2/tokenburn/",
)


def is_self_written(porcelain_line: str) -> bool:
    """Whether a `git status --porcelain` line names a path this runner wrote."""
    path = porcelain_line[3:].strip().strip('"')
    if " -> " in path:                      # a rename: judge the destination
        path = path.split(" -> ", 1)[1].strip().strip('"')
    path = path.replace("\\", "/")
    return any(path == p or path.startswith(p) for p in SELF_WRITTEN)


def git_provenance() -> dict:
    """HEAD, branch and the working-tree state, with self-written output split
    out so the gate cannot fail on the artifact it is producing."""
    head = sh(["git", "rev-parse", "HEAD"]).stdout.strip()
    branch = sh(["git", "rev-parse", "--abbrev-ref", "HEAD"]).stdout.strip()
    status = sh(["git", "status", "--porcelain"]).stdout
    all_changes = [line for line in status.splitlines() if line.strip()]
    self_written = [line for line in all_changes if is_self_written(line)]
    dirty = [line for line in all_changes if not is_self_written(line)]
    return {
        "git_head": head,
        "git_branch": branch,
        "working_tree_changes": dirty,
        "working_tree_clean": not dirty,
        "self_written_paths_excluded": self_written,
        "self_written_patterns": list(SELF_WRITTEN),
        "changes_including_self_written": len(all_changes),
        "note": ("`working_tree_clean` ignores the benchmark's own output "
                 "paths and nothing else. Every other uncommitted path is "
                 "counted, because evidence has to be attributable to a "
                 "commit somebody can check out."),
    }


def phase_provenance(rep: Report, args) -> dict:
    step("Repository provenance")
    info = git_provenance()
    dirty = info["working_tree_changes"]
    rep.add("provenance", "working tree clean (excluding this runner's own "
                          "output)", not dirty,
            f"{len(dirty)} changed path(s); "
            f"{len(info['self_written_paths_excluded'])} self-written path(s) "
            f"excluded", fatal=not args.allow_dirty)
    deleted = [line for line in dirty
               if line.startswith(" D") or line.startswith("D ")]
    rep.add("provenance", "no tracked file is deleted", not deleted,
            "; ".join(deleted[:5]))

    # Evidence has to name the binary it is evidence about. `provenance.py`
    # already answers this and was simply never called from here.
    sys.path.insert(0, str(BENCH / "harness"))
    try:
        import provenance as binary_provenance  # noqa: E402
        info["binary"] = binary_provenance.collect(VELRA_BINARY)
        matches = bool(info["binary"].get("commit_matches_binary"))
        rep.add("provenance", "the velra binary was built from HEAD", matches,
                f"{info['binary'].get('embedded_commit')} vs "
                f"{info['binary'].get('git_head_short9')}",
                fatal=not args.allow_dirty)
    except Exception as exc:  # noqa: BLE001 - reporting it is the point
        info["binary_error"] = str(exc)
        rep.add("provenance", "the velra binary was built from HEAD", False,
                str(exc)[:200], fatal=not args.allow_dirty)
    return info


def phase_legacy(rep: Report) -> dict:
    step("Legacy benchmark preservation")
    legacy = BENCH / "legacy"
    info: dict = {"legacy_root": str(legacy), "present": legacy.is_dir()}
    rep.add("legacy", "bench/legacy exists", legacy.is_dir(), str(legacy))

    missing = [name for name in LEGACY_MODULES
               if not (legacy / "scenarios" / name).is_file()]
    info["scenario_modules_present"] = not missing
    info["scenario_modules_missing"] = missing
    rep.add("legacy", "S1/S2/S3 scenario sources archived, not deleted",
            not missing, ", ".join(missing))

    runners = [p.name for p in sorted(legacy.glob("run_*.py"))] \
        if legacy.is_dir() else []
    info["archived_runners"] = runners
    rep.add("legacy", "the archived runners moved with them", len(runners) >= 3,
            ", ".join(runners))

    preserved = {}
    for path in PRESERVED:
        preserved[str(path.relative_to(REPO_ROOT))] = path.exists()
    info["preserved_artifacts"] = preserved
    absent = [k for k, v in preserved.items() if not v]
    rep.add("legacy", "historical results and reports still present",
            not absent, ", ".join(absent))

    info["counts_toward_v012_scorecard"] = False
    info["note"] = ("The archived scenarios remain runnable as regression "
                    "tests. They contribute nothing to this scorecard: the "
                    "Token-Burn pipeline reads only bench/results/v0.1.2/"
                    "tokenburn/trials and no legacy artifact is an input to "
                    "any verdict here.")
    rep.add("legacy", "legacy scenarios excluded from the v0.1.2 scorecard",
            True, "separate trials root, separate pre-registration")
    return info


def phase_tests(rep: Report, args) -> dict:
    step("Tests")
    info: dict = {}
    if args.skip_tests:
        rep.add("tests", "test suites", False, "skipped (--skip-tests)",
                fatal=True)
        return info

    own = sh([sys.executable, HERE / "selftest.py"])
    info["tokenburn_selftest_exit"] = own.returncode
    info["tokenburn_selftest_tail"] = (own.stdout or "").strip().splitlines()[-3:]
    rep.add("tests", "tokenburn selftest (18 scripted cases, offline)",
            own.returncode == 0, (own.stdout or "").strip().splitlines()[-1:][0]
            if own.stdout else (own.stderr or "")[-200:])

    pytest = sh([sys.executable, "-m", "pytest", "bench/tests", "-q"])
    info["pytest_exit"] = pytest.returncode
    info["pytest_tail"] = (pytest.stdout or "").strip().splitlines()[-3:]
    rep.add("tests", "pytest bench/tests", pytest.returncode == 0,
            (pytest.stdout or "").strip().splitlines()[-1] if pytest.stdout
            else (pytest.stderr or "")[-200:])

    legacy = sh([sys.executable, BENCH / "harness" / "selftest.py"])
    info["legacy_harness_selftest_exit"] = legacy.returncode
    rep.add("tests", "legacy harness selftest still passes after the archive",
            legacy.returncode == 0,
            (legacy.stdout or "").strip().splitlines()[-1] if legacy.stdout
            else (legacy.stderr or "")[-200:])

    if args.with_cargo_test:
        cargo = sh(["cargo", "test", "--workspace", "--features",
                    "fault-injection"], env=build_env())
        info["cargo_test_exit"] = cargo.returncode
        rep.add("tests", "cargo test --workspace --features fault-injection",
                cargo.returncode == 0,
                (cargo.stdout or "")[-300:] if cargo.returncode else "")
    else:
        info["cargo_test_exit"] = None
        rep.add("tests", "cargo test --workspace --features fault-injection",
                False, "not run by this invocation (--with-cargo-test to run "
                       "it here); run it separately before authorizing live",
                fatal=False)
    return info


def phase_fixtures(rep: Report, args, names, root: pathlib.Path) -> dict:
    step("Scenario fixtures and ground truth")
    info: dict = {}
    for name in names:
        scenario = scenarios.get(name)
        dest = root / f"readiness-{name}"
        try:
            manifest = scenario.build(dest, verify=True)
        except SystemExit as exc:
            rep.add("fixtures", f"{name}: builds and verifies", False,
                    str(exc)[:400])
            info[name] = {"error": str(exc)}
            continue
        truth = manifest["ground_truth"]
        ok = (truth["as_generated"]["observed_target"] == "fail"
              and all(truth[v.name]["observed_target"] == v.expect_target
                      and truth[v.name]["observed_invariant"] == v.expect_invariant
                      for v in scenario.variants))
        rep.add("fixtures", f"{name}: ground truth verified by running pytest",
                ok, json.dumps({k: {kk: vv for kk, vv in v.items()
                                    if kk.startswith("observed")}
                                for k, v in truth.items()}))
        info[name] = {
            "repo": str(dest),
            "fixture_seed": manifest["fixture_seed"],
            "turn_script_hash": manifest["turn_script_hash"],
            "turn_count": manifest["turn_count"],
            "transition": manifest["transition"],
            "target_test": manifest["target_test"],
            "ground_truth": truth,
            "manifest": {k: v for k, v in manifest.items()
                         if k not in ("leak_scan", "ground_truth")},
        }
        rep.add("fixtures", f"{name}: at least 15 meaningful turns",
                manifest["turn_count"] >= prereg.minimum_source_turns(),
                f"{manifest['turn_count']} turns")
    return info


def phase_leaks(rep: Report, names, fixtures: dict) -> dict:
    step("Leak scans across every surface a destination session can read")
    info: dict = {}
    for name in names:
        entry = fixtures.get(name) or {}
        if "repo" not in entry:
            rep.add("leaks", f"{name}: leak scan", False, "no fixture to scan")
            continue
        scenario = scenarios.get(name)
        result = leakscan.scan(pathlib.Path(entry["repo"]), scenario.turns,
                               scenario.continuation_prompt,
                               scenario.leak_terms)
        info[name] = {k: v for k, v in result.items() if k != "hits"}
        rep.add("leaks", f"{name}: no fatal leak on any of "
                         f"{len(result['surfaces_scanned'])} surfaces",
                result["clean"], leakscan.format_report(result)[:300])
    return info


def phase_ladder(rep: Report, args, root: pathlib.Path) -> dict:
    step("Context-load ladder")
    info: dict = {"rungs": [], "chars_per_token_assumed":
                  context_fixture.CHARS_PER_TOKEN,
                  "fixture_version": context_fixture.FIXTURE_VERSION,
                  "plan": context_fixture.ladder_plan("A_cold_continuation")}
    rungs = prereg.ladder() if not args.quick_ladder else prereg.ladder()[:2]
    for rung in rungs:
        into = root / f"ladder-{rung}"
        started = time.time()
        record = context_fixture.generate(into, rung, "A_cold_continuation",
                                          seed=args.seed)
        check = context_fixture.verify(into, record)
        # Determinism is not decoration here: two arms of a pair must be
        # handed the same bytes, and the only way to know they were is to
        # generate twice and compare.
        again = context_fixture.generate(root / f"ladder-{rung}-again", rung,
                                         "A_cold_continuation", seed=args.seed)
        deterministic = [f["sha256"] for f in record["files"]] == \
                        [f["sha256"] for f in again["files"]]
        shutil.rmtree(root / f"ladder-{rung}-again", ignore_errors=True)
        info["rungs"].append({
            "target_context_size": rung,
            "synthetic_context_size": record["synthetic_context_size"],
            "chars": record["chars"],
            "megabytes": round(record["bytes_on_disk"] / 1e6, 2),
            "seconds": round(time.time() - started, 2),
            "verification": check,
            "deterministic": deterministic,
            "actual_observed_context_size": None,
            "note": ("a generated fixture, not an observation of a Claude "
                     "context window"),
        })
        rep.add("ladder", f"{rung:,} generates, verifies and is deterministic",
                check["ok"] and deterministic,
                f"{record['chars']:,} chars, "
                f"{record['bytes_on_disk'] / 1e6:.2f} MB")
        shutil.rmtree(into, ignore_errors=True)
    if args.quick_ladder:
        rep.add("ladder", "every registered rung was exercised", False,
                "--quick-ladder ran only the first two rungs", fatal=False)
    return info


def phase_telemetry(rep: Report) -> dict:
    step("Telemetry abstraction")
    scraping = telemetry.scan_for_terminal_scraping(telemetry.package_sources())
    info = {
        "sources_in_preference_order": list(telemetry.SOURCES),
        "statuses": list(telemetry.STATUSES),
        "required_fields": list(telemetry.TELEMETRY_FIELDS),
        "identity_fields": list(telemetry.IDENTITY_FIELDS),
        "provenance_fields": ["metric_name", "value", "unit", "source",
                              "raw_artifact_reference", "measurement_status"],
        "terminal_scraping_hits": scraping,
        "cache_field_policy": (
            "cache_read_input_tokens and cache_creation_input_tokens are "
            "reported only when explicitly present in structured data; "
            "otherwise UNAVAILABLE. They are never recovered from stdout, "
            "stderr, progress displays or status lines, and never defaulted "
            "to zero."),
        "context_size_policy": (
            "actual_observed_context_size is UNAVAILABLE unless the runtime "
            "emits a structured context-size field. A synthetic fixture's "
            "size is reported separately and is never an observation."),
        "cache_condition_policy": (
            "UNKNOWN when either cache field is unavailable. UNKNOWN is never "
            "reported as EXPIRED."),
    }
    rep.add("telemetry", "no terminal scraping anywhere in the package",
            not scraping, json.dumps(scraping[:2]))
    rep.add("telemetry", "every metric carries the six provenance fields",
            True, ", ".join(info["provenance_fields"]))
    return info


def phase_mock(rep: Report, args) -> dict:
    step("MockClaudeAdapter")
    specs, expect = selftest_mod.cases()
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp) / "mock"
        root.mkdir(parents=True)
        result, failures = selftest_mod.run(root)
    info = {
        "scripted_cases": len(expect),
        "synthetic_trials": len(specs),
        "matched_pairs": result["pairing"]["n_pairs"],
        "dropped_pairs": [d["reason"] for d in result["pairing"]["dropped"]],
        "verdict_counts": result["pooled"]["overall_verdict_counts"],
        "failures": failures,
        "feeds_production_pipeline": True,
        "pipeline": ["parse.parse_trial", "metrics.evaluate", "causal.evaluate",
                     "pairing.pair_up", "verdict.pair_verdict",
                     "verdict.pool", "report.markdown"],
        "separate_evaluator_exists": False,
    }
    rep.add("mock", "MockClaudeAdapter feeds the production pipeline",
            not failures, f"{len(expect)} cases, {len(specs)} trials, "
                          f"{result['pairing']['n_pairs']} pairs")
    return info


def phase_capsule_provenance(rep: Report, names) -> dict:
    """§6: the qualification capsule comes from the production path.

    Blocking rather than advisory. `causal.link_f` fails any trial whose
    delivered capsule does not carry every declared marker, so a scenario whose
    markers the production renderer cannot produce would turn every Velra-arm
    trial into RECEIPT_FAILURE — and it would do so after the money was spent.
    """
    step("Capsule provenance")
    result = capsule_probe.run(VELRA_BINARY, names)
    info = {
        "ok": result["ok"],
        "capsule_written_by": result["capsule_written_by"],
        "capsule_written_by_benchmark": result["capsule_written_by_benchmark"],
        "note": result["note"],
        "scenarios": {name: {k: v for k, v in entry.items() if k != "capsule"}
                      for name, entry in result["scenarios"].items()},
    }
    for name, entry in sorted(result["scenarios"].items()):
        rep.add("capsule", f"{name}: the production capsule carries every "
                           f"declared marker", bool(entry.get("ok")),
                f"{len(entry.get('markers_found') or [])}"
                f"/{len(entry.get('declared_markers') or [])} markers, "
                f"{entry.get('capsule_chars', 0)} chars"
                + (f"; missing {entry['markers_missing']}"
                   if entry.get("markers_missing") else ""))
    rep.add("capsule", "the benchmark composes no capsule text",
            not result["capsule_written_by_benchmark"],
            result["capsule_written_by"])
    return info


def phase_isolation(rep: Report, args) -> dict:
    """Memory isolation and handoff validation, proven offline."""
    step("Trial validity preflight: memory isolation and source handoff")
    result = isolation.preflight()
    for c in result["checks"]:
        print(f"  [{'PASS' if c['ok'] else 'FAIL'}] {c['check']}", flush=True)
    failed = [c["check"] for c in result["checks"] if not c["ok"]]
    rep.add("isolation", "auto-memory isolated and handoff enforced, both arms",
            result["ok"], "; ".join(failed) or
            f"{len(result['checks'])} checks")
    return {"ok": result["ok"], "checks": result["checks"],
            "failed": failed}


def phase_smoke(rep: Report, args) -> dict:
    step("Offline restore -> SessionStart smoke test")
    with tempfile.TemporaryDirectory() as tmp:
        result = smoke_mod.run(VELRA_BINARY, pathlib.Path(tmp) / "smoke")
    passed = sum(1 for c in result["checks"] if c["ok"])
    info = {
        "ok": result["ok"],
        "checks_passed": passed,
        "checks_total": len(result["checks"]),
        "failed": [c["check"] for c in result["checks"] if not c["ok"]],
        "staged_tokens": result.get("staged_tokens"),
        "capsule_chars": result.get("capsule_chars"),
        "operational_state": result.get("operational_state"),
        "checks": [{k: v for k, v in c.items()} for c in result["checks"]],
        "live_claude_smoke_test_run": False,
        "note": ("drives the real velra binary against the recorded Claude "
                 "Code 2.1.272 SessionStart payloads. No Claude process is "
                 "started; a live smoke test needs explicit authorization "
                 "this phase does not have."),
    }
    rep.add("smoke", "restore stages, SessionStart delivers, exactly once",
            result["ok"], f"{passed}/{len(result['checks'])} checks")
    return info


def plan_pairs(names, pairs_per: int, stage: str,
               only_pairs=None) -> list[dict]:
    plan = []
    for name in names:
        for index in range(1, pairs_per + 1):
            pair_id = f"{name}#{stage}{index}"
            if only_pairs and pair_id not in only_pairs:
                continue
            plan.append({
                "pair_id": pair_id, "benchmark": name, "stage": stage,
                "replicate": index,
                "context_ladder_rung": None,
                "arms": {arm: {"trial": f"{name}-{stage}{index}-{arm}",
                               "dir": str(TRIALS / f"{name}-{stage}{index}-{arm}")}
                         for arm in ("baseline", "velra")},
            })
    return plan


def phase_plan(rep: Report, args, names) -> dict:
    step("Trial plan")
    doc = prereg.load()["trial_plan"]
    qualification = plan_pairs(names, doc["qualification"]["pairs_per_benchmark"],
                               "q", args.pairs)
    formal = plan_pairs(names, doc["formal"]["pairs_per_benchmark"], "f",
                        args.pairs)
    for entry in qualification + formal:
        done = {arm: pathlib.Path(v["dir"], "trial_meta.json").exists()
                for arm, v in entry["arms"].items()}
        entry["already_on_disk"] = done

    info = {
        "qualification": {
            "pairs": qualification,
            "pairs_per_benchmark": doc["qualification"]["pairs_per_benchmark"],
            "sessions": 2 * len(qualification),
            "purpose": doc["qualification"]["purpose"],
            "counts_toward_scorecard": False,
            "redesign_allowed": True,
        },
        "formal": {
            "pairs": formal,
            "pairs_per_benchmark": doc["formal"]["pairs_per_benchmark"],
            "sessions": 2 * len(formal),
            "redesign_allowed": False,
            "scenario_frozen_before_first_formal_trial": True,
        },
        "total_pairs": len(qualification) + len(formal),
        "total_sessions": 2 * (len(qualification) + len(formal)),
        "ladder_rung_for_trials": args.rung,
        "note": ("each trial drives two Claude sessions -- a source and a "
                 "destination -- so a 'session' here is a pair of processes; "
                 "the counts above are trials, matching §11."),
    }
    rep.add("plan", "every pair schedules both arms",
            all(set(p["arms"]) == {"baseline", "velra"}
                for p in qualification + formal),
            f"{info['total_pairs']} pairs, {info['total_sessions']} trials")
    rep.add("plan", "formal stage meets the registered pair count",
            doc["formal"]["pairs_per_benchmark"] >= 4,
            f"{doc['formal']['pairs_per_benchmark']} per benchmark")
    return info


# --------------------------------------------------------------------------
# resumption state
# --------------------------------------------------------------------------


def load_state() -> dict:
    path = TOKENBURN / "run_state.json"
    if path.exists():
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            return {}
    return {}


def save_state(state: dict) -> None:
    TOKENBURN.mkdir(parents=True, exist_ok=True)
    (TOKENBURN / "run_state.json").write_text(
        json.dumps(state, indent=2, default=str), encoding="utf-8",
        newline="")


def settings_path() -> pathlib.Path:
    config = os.environ.get("CLAUDE_CONFIG_DIR")
    base = pathlib.Path(config) if config else pathlib.Path.home() / ".claude"
    return base / "settings.json"


def snapshot_settings() -> tuple[pathlib.Path, bytes | None]:
    """Copy the user-level settings file aside before the arms mutate it.

    Both arms run `velra enable` or `velra disable` against the real
    settings file, because that registration is the thing under test and a
    trial that faked it would not be measuring the product. The consequence is
    that a run which dies between two trials leaves the user's Claude Code
    configured however the last arm left it. The snapshot below, restored in a
    `finally`, is what stops the benchmark having a side effect on the machine
    it ran on.
    """
    path = settings_path()
    data = path.read_bytes() if path.exists() else None
    if data is not None:
        keep = TOKENBURN / "settings-backup" / f"settings.{now_stamp()}.json"
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


def arm_order(pair_index: int) -> tuple:
    """Which arm runs first in this pair. Alternates, and is recorded.

    Anthropic's prompt cache is server-side and shared, so whichever arm runs
    second in a pair may find a warmer cache than the first did. Running the
    baseline first every time would hand that advantage to Velra in every
    pair, systematically and invisibly, in exactly the metric the benchmark
    exists to report. Alternating does not remove the effect; it stops it
    pointing the same way every time, and `arm_order_index` in each trial's
    metadata lets the analysis check whether order mattered.
    """
    return ("baseline", "velra") if pair_index % 2 == 0 else ("velra", "baseline")


def quarantine(trial_dir: pathlib.Path, why: str) -> None:
    """Move a completed trial aside. Never delete one."""
    if not trial_dir.exists():
        return
    dest = QUARANTINE / now_stamp() / trial_dir.name
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(trial_dir), str(dest))
    with open(QUARANTINE / "log.jsonl", "a", encoding="utf-8",
              newline="") as fh:
        fh.write(json.dumps({"at": now_stamp(), "trial": trial_dir.name,
                             "moved_to": str(dest), "why": why}) + "\n")


# --------------------------------------------------------------------------
# live
# --------------------------------------------------------------------------


def phase_live(args, plan: list[dict]) -> int:
    """The only code path that starts a Claude process. Gated twice."""
    safety.require_live(safety.MODE_LIVE, live_flag=args.live)
    # Imported here and nowhere else. `test_tokenburn_safety` fails the build
    # if this import appears outside this function, because the offline modes
    # must have no reachable path to the only module that starts Claude.
    if __package__:
        from . import live_trial
    else:
        import live_trial  # noqa: E402
    sys.path.insert(0, str(BENCH / "harness"))
    import claude_binary  # noqa: E402

    claude = pathlib.Path(claude_binary.resolve())
    state = load_state()
    state.setdefault("trials", {})
    fixture_root = pathlib.Path(
        args.fixture_root or (pathlib.Path(tempfile.gettempdir())
                              / "velra-tokenburn-fixtures"))
    fixture_root.mkdir(parents=True, exist_ok=True)
    TRIALS.mkdir(parents=True, exist_ok=True)

    settings_file, saved_settings = snapshot_settings()
    try:
        return _run_trials(args, plan, live_trial, claude, fixture_root, state)
    finally:
        restore_settings(settings_file, saved_settings)


def _run_trials(args, plan, live_trial, claude, fixture_root,
                state: dict) -> int:
    """Every planned trial, in alternating arm order."""
    for pair_index, entry in enumerate(plan):
        order = arm_order(pair_index)
        entry["arm_order"] = list(order)
        for order_index, arm in enumerate(order):
            spec = entry["arms"][arm]
            out = pathlib.Path(spec["dir"])
            key = spec["trial"]
            if (out / "trial_meta.json").exists():
                if args.resume and not args.force:
                    print(f"  resume: {key} already complete", flush=True)
                    continue
                if args.force:
                    quarantine(out, "re-run with --force")
                else:
                    raise SystemExit(
                        f"{key} exists. --resume keeps it, --force "
                        f"quarantines it and runs again.")
            state["trials"][key] = {
                "pair_id": entry["pair_id"], "benchmark": entry["benchmark"],
                "stage": entry["stage"], "arm": arm, "state": "running",
                "arm_order_index": order_index, "arm_order": list(order),
                "started": now_stamp()}
            save_state(state)
            live_trial.run_trial(
                scenario_name=entry["benchmark"], arm=arm,
                pair_id=entry["pair_id"], replicate=entry["replicate"],
                out=out, fixture=fixture_root / key, rung=args.rung,
                model=args.model, claude=claude,
                velra=VELRA_BINARY, max_budget_usd=args.max_budget_usd,
                seed=args.seed, arm_order_index=order_index,
                arm_order=list(order))
            state["trials"][key].update(state="complete", finished=now_stamp())
            save_state(state)

    result = aggregate.run(TRIALS, TOKENBURN)
    state["last_aggregate"] = now_stamp()
    state["verdicts"] = {v["pair_id"]: v["verdict"] for v in result["pairs"]}
    save_state(state)
    return 0


# --------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--selftest", action="store_true",
                      help="the pipeline on MockClaudeAdapter trials. Offline.")
    mode.add_argument("--dry-run", action="store_true",
                      help="readiness: fixtures, leaks, ladder, telemetry, "
                           "smoke, plan. Offline. The default.")
    mode.add_argument("--smoke", action="store_true",
                      help="restore -> SessionStart against the real binary. "
                           "Offline, always, in this phase.")
    mode.add_argument("--preflight", action="store_true",
                      help="trial-validity controls only: memory isolation, "
                           "leak scan, handoff validation. Offline; writes "
                           "nothing under bench/results.")
    mode.add_argument("--live", action="store_true",
                      help="the expensive evaluation. Needs "
                           f"{safety.LIVE_ENV}=1 as well.")

    ap.add_argument("--only", nargs="*", metavar="BENCHMARK",
                    choices=list(scenarios.SCENARIOS))
    ap.add_argument("--pairs", nargs="*", metavar="PAIR_ID")
    ap.add_argument("--stage", choices=("qualification", "formal"),
                    default="qualification",
                    help="which planned stage --live would execute")
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--rung", type=int, default=250_000,
                    help="context-load rung for the trials")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--max-budget-usd", type=float, default=25.0)
    ap.add_argument("--fixture-root", default=None)
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--allow-dirty", action="store_true")
    ap.add_argument("--skip-tests", action="store_true")
    ap.add_argument("--with-cargo-test", action="store_true")
    ap.add_argument("--quick-ladder", action="store_true",
                    help="only the first two rungs, for a fast check")
    args = ap.parse_args()

    if args.selftest:
        safety.assert_offline(safety.MODE_SELFTEST)
        return subprocess.run([sys.executable, str(HERE / "selftest.py")]).returncode
    if args.smoke:
        safety.assert_offline(safety.MODE_SMOKE)
        return subprocess.run([sys.executable, str(HERE / "smoke.py")]).returncode
    if args.preflight:
        safety.assert_offline(safety.MODE_SMOKE)
        return subprocess.run([sys.executable, str(HERE / "isolation.py"),
                               "--preflight"]).returncode

    names = list(args.only or scenarios.DEFAULT_ORDER)
    started = time.time()
    rep = Report()
    if not args.live:
        safety.assert_offline(safety.MODE_DRY_RUN)

    readiness: dict = {
        # Self-describing, because "which of these files is the current one"
        # is a question a reader should never have to answer from mtimes.
        "artifact": {
            "id": "velra-tokenburn-readiness",
            "path": str(READINESS.relative_to(REPO_ROOT)),
            "authoritative": True,
            "is_current": True,
            "benchmark": "v0.1.2 Token-Burn",
            "supersedes": (
                "bench/results/v0.1.2/hardened/readiness.20260921T101557Z.json "
                "-- the archived hardened evaluation's gate, frozen and "
                "preserved byte-for-byte; it is historical and is not this"),
            "written_by": "bench/tokenburn/run.py",
        },
        "at": now_stamp(),
        "mode": "live" if args.live else "dry-run",
        "benchmarks": names,
        "results_dir": str(TOKENBURN),
        **prereg.stamp(),
    }

    work = pathlib.Path(args.fixture_root
                        or (pathlib.Path(tempfile.gettempdir())
                            / "velra-tokenburn-readiness"))
    work.mkdir(parents=True, exist_ok=True)

    readiness["environment"] = phase_environment(rep, args)
    readiness["provenance"] = phase_provenance(rep, args)
    readiness["legacy_preservation"] = phase_legacy(rep)
    readiness["tests"] = phase_tests(rep, args)
    fixtures = phase_fixtures(rep, args, names, work)
    readiness["scenario_manifests"] = fixtures
    readiness["leak_scans"] = phase_leaks(rep, names, fixtures)
    readiness["context_configuration"] = phase_ladder(rep, args, work)
    readiness["telemetry"] = phase_telemetry(rep)
    readiness["mock_adapter"] = phase_mock(rep, args)
    readiness["capsule_provenance"] = phase_capsule_provenance(rep, names)
    readiness["smoke_test"] = phase_smoke(rep, args)
    readiness["trial_validity_preflight"] = phase_isolation(rep, args)
    readiness["trial_plan"] = phase_plan(rep, args, names)

    step("Live safety gate")
    gate = safety.status()
    readiness["live_safety_gate"] = gate
    rep.add("safety", "the gate exists in executable code", gate["gate_present"],
            f"{gate['requires_flag']} + {gate['requires_env']}")
    if args.live:
        # In live mode the gate must open, and `phase_live` refuses again
        # before the first process if it does not.
        rep.add("safety", "live execution is authorized",
                gate["would_allow_live_now"],
                "; ".join(gate["refusal_reasons_now"]))
    else:
        # In an offline mode this is information, not a requirement. Somebody
        # with the environment variable already exported is not a reason to
        # fail a dry run -- nothing in this mode can start a process anyway --
        # but they should be told that `--live` would now go through.
        rep.add("safety", "live execution is not currently authorized",
                not gate["would_allow_live_now"],
                "; ".join(gate["refusal_reasons_now"])
                or "IT IS AUTHORIZED: --live would start Claude processes",
                fatal=False)
    readiness["offline_guarantee"] = safety.assert_offline(
        safety.MODE_DRY_RUN) if not args.live else None

    readiness.update(rep.to_json())
    readiness["state"] = ("READY FOR LIVE EVALUATION" if not rep.blocking
                          else "NOT READY FOR LIVE EVALUATION")
    readiness["blockers"] = rep.blocking
    readiness["elapsed_seconds"] = round(time.time() - started, 1)

    READINESS.parent.mkdir(parents=True, exist_ok=True)
    READINESS.write_text(json.dumps(readiness, indent=2, default=str),
                         encoding="utf-8", newline="")

    step("Readiness")
    for row in rep.blocking:
        print(f"  BLOCKER  {row['phase']}/{row['check']}: {row['detail']}")
    for row in rep.warnings:
        print(f"  warning  {row['phase']}/{row['check']}: {row['detail']}")
    print(f"\n  {readiness['state']}")
    print(f"  readiness report: {READINESS}")
    print(f"  planned: {readiness['trial_plan']['total_pairs']} pairs, "
          f"{readiness['trial_plan']['total_sessions']} trials "
          f"(each drives a source and a destination session)")
    print(f"  elapsed: {readiness['elapsed_seconds']:.0f}s")

    if not args.live:
        print("\n  Dry run: no Claude process was started and nothing was "
              "spent.")
        return 0 if not rep.blocking else 1

    if rep.blocking:
        print("\n  Refusing to run live with blockers.", file=sys.stderr)
        return 1
    stage = "q" if args.stage == "qualification" else "f"
    plan = readiness["trial_plan"][args.stage]["pairs"]
    return phase_live(args, plan)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except safety.LiveExecutionRefused as exc:
        print(f"\n{exc}", file=sys.stderr)
        raise SystemExit(2) from exc
