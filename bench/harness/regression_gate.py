#!/usr/bin/env python3
"""Offline checks that must pass before a live session is worth paying for.

Every check here is free, fast and deterministic. They exist because each one
corresponds to a way a previous run produced evidence about something other
than what it claimed:

  RC1  fixture leak scan       the v0.1 fixture shipped a TODO naming the fix
                               (caught mid-run, four trials discarded) and a
                               docstring on the failing test naming it again
                               (not caught; a recorded replicate quotes it)
  RC2  fixture ground truth    a "dead end" that quietly becomes a fix, or a
                               "true fix" that does not fix, makes the whole
                               scenario meaningless
  RC3  cargo test              the Rust suite carries a named regression test
                               for each of the five capsule defects the v0.1
                               benchmark found
  RC4  provenance              the v0.1 report attributes its dataset to a
                               commit the binary was not built from

A failure here aborts the run. Spending money to measure a binary whose known
defects have come back is not an experiment.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO_ROOT / "bench"))
# The S1/S2/S3 scenarios are archived under bench/legacy (v0.1.2 Phase 3);
# `from scenarios import ...` still resolves because their home is on the path.
sys.path.insert(0, str(REPO_ROOT / "bench" / "legacy"))

import provenance  # noqa: E402
import prereg  # noqa: E402
from scenarios import base, registry  # noqa: E402

# Each historical defect, and the test that would catch it coming back. The
# names are asserted to exist, so deleting or renaming a regression test fails
# this gate rather than silently removing the guard.
HISTORICAL_DEFECTS = [
    {"id": "D1", "found": "v0.1 benchmark §9",
     "defect": "estimate_tokens assumed 3.2 chars/token; the capsule's real "
               "density is 2.07-2.16, so the ladder never ran and blocks of "
               "951 and 1,147 tokens shipped against an 800 ceiling",
     "test": "estimator_is_above_real_tokenizer_counts",
     "file": "crates/velra/tests/capsule.rs"},
    {"id": "D2", "found": "v0.1 benchmark §8",
     "defect": "a late git_pre row was read as proof a discarded change had "
               "come back, so [REVERTED_EDITS] was filtered out of a delivered "
               "capsule entirely",
     "test": "f5c_a_late_git_pre_row_does_not_resurrect_a_dead_end",
     "file": "crates/velra/tests/tracking.rs"},
    {"id": "D2b", "found": "v0.1 benchmark §15",
     "defect": "the fix for D2 must not stop a genuine re-application from "
               "closing a dead end",
     "test": "f5d_a_later_edit_that_restores_the_content_still_reapplies",
     "file": "crates/velra/tests/tracking.rs"},
    {"id": "D3", "found": "v0.1 benchmark §8",
     "defect": "`Bash(git *)` is a prefix match, so the PreToolUse "
               "registration never fired for `cd \"...\" && git restore ...`",
     "test": "b1_a_chained_git_restore_is_observed_on_both_sides_and_on_failure",
     "file": "crates/velra/tests/ipc_contract.rs"},
    {"id": "D4", "found": "v0.1 benchmark §15",
     "defect": "git effects were read only from calls that succeeded, so "
               "`git restore x && pytest` with a still-failing suite lost the "
               "revert on both sides",
     "test": "f2b_a_restore_chained_with_a_failing_command_is_still_attributed",
     "file": "crates/velra/tests/tracking.rs"},
    {"id": "D5", "found": "v0.1 benchmark §11",
     "defect": "[FILE_ACTIVITY] ranked ties by most-recent touch, so an audit "
               "sweep across 84 modules evicted the files the task ran through",
     "test": "an_unrelated_read_sweep_does_not_evict_the_files_the_task_is_about",
     "file": "crates/velra/tests/capsule.rs"},
    {"id": "D6", "found": "v0.1 benchmark §15",
     "defect": "a `git commit` in a call that failed marked its edits "
               "COMMITTED on the strength of a hash that is identical either way",
     "test": "f4b_a_failed_commit_leaves_edits_active",
     "file": "crates/velra/tests/tracking.rs"},
    {"id": "D7", "found": "v0.1 benchmark §15",
     "defect": "schema v2 had no test that migrated a populated v1 database",
     "test": "c6_a_v1_database_migrates_forward_with_its_rows",
     "file": "crates/velra/tests/storage.rs"},
    {"id": "D8", "found": "v0.1 benchmark §15",
     "defect": "the discarded approach must reach the rendered capsule at all",
     "test": "a_discarded_approach_always_reaches_the_capsule",
     "file": "crates/velra/tests/capsule.rs"},
]


def rc1_leak_scan() -> dict:
    """No generated fixture may contain a phrase naming its own fix."""
    findings = []
    with tempfile.TemporaryDirectory() as tmp:
        for name, scenario in registry.SCENARIOS.items():
            dest = pathlib.Path(tmp) / name
            try:
                manifest = scenario.build(dest, verify=False)
            except SystemExit as exc:
                findings.append({"scenario": name, "error": str(exc)})
                continue
            scan = base.lint_tree(dest, scenario.leak_terms)
            findings.append({"scenario": name, "clean": scan["clean"],
                             "hits": scan["hits"],
                             "terms_checked": len(scan["terms"]),
                             "files": manifest.get("noise_module_count")})
    ok = all(f.get("clean") for f in findings)
    return {"id": "RC1", "name": "fixture leak scan", "passed": ok,
            "findings": findings}


def rc2_ground_truth() -> dict:
    """Every declared dead end stays red; every declared fix turns it green."""
    findings = []
    ok = True
    with tempfile.TemporaryDirectory() as tmp:
        for name, scenario in registry.SCENARIOS.items():
            dest = pathlib.Path(tmp) / name
            try:
                manifest = scenario.build(dest, verify=True)
            except SystemExit as exc:
                ok = False
                findings.append({"scenario": name, "passed": False, "error": str(exc)})
                continue
            findings.append({"scenario": name, "passed": True,
                             "ground_truth": manifest["ground_truth"]})
    return {"id": "RC2", "name": "fixture ground truth", "passed": ok,
            "findings": findings}


def cargo_toolchain(binary: pathlib.Path) -> str | None:
    """Which rustup toolchain can actually link on this host.

    `rustup`'s default here is `stable-x86_64-pc-windows-msvc` while the
    release binary is `x86_64-pc-windows-gnu`, and a bare `cargo test` picks
    the default and fails at link time on a machine with no MSVC linker. That
    is a host-configuration failure wearing a test failure's clothes, so it is
    resolved rather than reported.

    ``$VELRA_BENCH_CARGO_TOOLCHAIN`` overrides. Otherwise the binary's own
    target triple is matched against the installed toolchains, which keeps the
    tests running against the same triple the trials measure.
    """
    pinned = os.environ.get("VELRA_BENCH_CARGO_TOOLCHAIN")
    if pinned:
        return pinned
    version = subprocess.run([str(binary), "--version"], capture_output=True,
                             text=True, encoding="utf-8", errors="replace")
    triple = None
    if "(" in version.stdout and "," in version.stdout:
        triple = version.stdout.split("(", 1)[1].rstrip(")\n ").split(",")[-1].strip()
    if not triple:
        return None
    listed = subprocess.run(["rustup", "toolchain", "list"], capture_output=True,
                            text=True, encoding="utf-8", errors="replace")
    if listed.returncode != 0:
        return None
    installed = [line.split()[0] for line in listed.stdout.splitlines() if line.strip()]
    if any(t.endswith(triple) and "(default)" in t for t in listed.stdout.splitlines()):
        return None  # the default already matches; no override needed
    return next((t for t in installed if t.endswith(triple)), None)


def rc3_rust_suite(run_tests: bool, binary: pathlib.Path) -> dict:
    """The named regression test for every historical defect still exists."""
    missing = []
    for defect in HISTORICAL_DEFECTS:
        path = REPO_ROOT / defect["file"]
        text = path.read_text(encoding="utf-8") if path.exists() else ""
        if f"fn {defect['test']}(" not in text:
            missing.append(defect)

    result = {"id": "RC3", "name": "historical-defect regression tests",
              "defects_tracked": len(HISTORICAL_DEFECTS),
              "missing_tests": missing,
              "passed": not missing, "cargo_test": None}
    if missing or not run_tests:
        if not run_tests:
            result["cargo_test"] = {"skipped": True,
                                    "why": "--skip-cargo-test was passed"}
        return result

    env = dict(os.environ)
    winlibs = pathlib.Path(os.path.expanduser(
        "~/AppData/Local/Programs/winlibs-mingw64/mingw64/bin"))
    if winlibs.is_dir():
        env["PATH"] = str(winlibs) + os.pathsep + env.get("PATH", "")
    toolchain = cargo_toolchain(binary)
    cmd = ["cargo"] + ([f"+{toolchain}"] if toolchain else []) + ["test", "--workspace"]
    proc = subprocess.run(cmd, cwd=str(REPO_ROOT), capture_output=True, text=True,
                          encoding="utf-8", errors="replace", env=env)
    result["cargo_test"] = {"command": " ".join(cmd),
                            "toolchain": toolchain,
                            "exit": proc.returncode,
                            "tail": (proc.stdout or "").strip().splitlines()[-25:],
                            "stderr_tail": (proc.stderr or "").strip().splitlines()[-15:]}
    result["passed"] = proc.returncode == 0
    return result


def rc4_provenance(binary: pathlib.Path) -> dict:
    record = provenance.collect(binary)
    return {"id": "RC4", "name": "provenance",
            "passed": bool(record["working_tree_clean"]
                           and record["commit_matches_binary"]),
            "record": record}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--out", default="bench/results/v0.1.1/regression_gate.json")
    ap.add_argument("--skip-cargo-test", action="store_true")
    ap.add_argument("--allow-dirty", action="store_true",
                    help="record a provenance failure but do not abort on it")
    args = ap.parse_args()

    binary = pathlib.Path(args.binary).resolve()
    checks = [
        rc1_leak_scan(),
        rc2_ground_truth(),
        rc3_rust_suite(not args.skip_cargo_test, binary),
        rc4_provenance(binary),
    ]
    blocking = [c for c in checks
                if not c["passed"] and not (c["id"] == "RC4" and args.allow_dirty)]
    payload = {**prereg.stamp(), "checks": checks,
               "historical_defects": HISTORICAL_DEFECTS,
               "passed": not blocking,
               "allow_dirty": args.allow_dirty}
    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=2), encoding="utf-8", newline="")

    for check in checks:
        mark = "ok  " if check["passed"] else "FAIL"
        print(f"  [{mark}] {check['id']} {check['name']}")
        if check["id"] == "RC3" and check.get("missing_tests"):
            for defect in check["missing_tests"]:
                print(f"           missing {defect['test']} in {defect['file']} "
                      f"(guards {defect['id']})")
        if check["id"] == "RC4" and not check["passed"]:
            for warning in check["record"]["warnings"]:
                print(f"           {warning}")
        if check["id"] in ("RC1", "RC2") and not check["passed"]:
            for finding in check["findings"]:
                if finding.get("error"):
                    print(f"           {finding['scenario']}: {finding['error']}")
                for hit in finding.get("hits", []):
                    print(f"           {finding['scenario']} leak: "
                          f"{hit['file']}:{hit['line']} {hit['term']!r}")
    print(f"  regression gate: {'PASSED' if not blocking else 'FAILED'}")
    return 0 if not blocking else 1


if __name__ == "__main__":
    raise SystemExit(main())
