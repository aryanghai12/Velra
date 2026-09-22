#!/usr/bin/env python3
"""Trial validity: memory isolation and the source handoff. All offline.

    python bench/tokenburn/isolation.py --preflight

Two things invalidated the v0.1.2 qualification without any code noticing
(evidence: tag ``tokenburn-qualification-v0.1.2``), and this module is where
both are now checked, identically for the baseline and the Velra arm.

1. **A hidden memory channel.** Claude Code 2.1.272 keeps an *auto-memory*
   directory per project, ``<config>/projects/<slug>/memory/``, outside the
   repository and therefore outside every surface the leak scan read. In six
   of eight source sessions the agent wrote the scenario's unwritten
   constraint there, and every B destination session -- on both arms -- was
   handed it as instructions. The benchmark then compared two sessions that
   both had the answer.

   Isolation is enforced with the two controls the binary itself defines,
   and nothing else:

   * ``CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`` in the environment of every Claude
     process of every trial. The binary checks it before any setting and a
     truthy value disables auto-memory outright.
   * ``"autoMemoryEnabled": false`` in the user settings file for the
     duration of the trial ("When false, Claude will not read from or write
     to the auto-memory directory"), restored byte for byte afterwards.

   It is *verified* rather than trusted: the fixture's memory directory must
   be empty before the source session starts (anything left by an earlier run
   is moved aside, never deleted) and still empty before the destination
   starts. A leak-term scan runs as well, but emptiness is the criterion,
   because a memory note paraphrases -- the note the last run wrote said
   ``booking_date``, never ``value_date``, which no registered leak phrase
   matches.

2. **A source session that did not leave the intended state.** Twice the
   source agent edited product code despite "don't change anything yet", and
   the target test was passing at the handoff. The destination was then asked
   to continue a task that was already done. :func:`validate_handoff` checks,
   immediately before the transition, that the target test fails, that the
   scenario's invariant is in the state the generated tree has, and that the
   worktree is the generated commit plus nothing but the harness's own
   files. A trial that fails any of those is INVALID: it is recorded with the
   reason and never continued, repaired or scored.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import time
from typing import Callable, Sequence

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import context_fixture  # type: ignore[no-redef]
    import leakscan  # type: ignore[no-redef]
    import safety  # type: ignore[no-redef]
else:
    from . import context_fixture, leakscan, safety

#: The environment control, and the value that disables auto-memory. The
#: binary tests it with its truthiness helper before consulting any setting.
MEMORY_ENV = "CLAUDE_CODE_DISABLE_AUTO_MEMORY"
MEMORY_ENV_VALUE = "1"
#: The settings control. ``false`` stops both reads and writes.
MEMORY_SETTING = "autoMemoryEnabled"
#: A setting that relocates the memory directory. Isolation cannot be proven
#: per fixture while it is set, so the preflight refuses.
MEMORY_DIR_SETTING = "autoMemoryDirectory"


# --------------------------------------------------------------------------
# where Claude Code keeps a project's auto-memory
# --------------------------------------------------------------------------


def claude_config_dir(env: dict | None = None) -> pathlib.Path:
    env = os.environ if env is None else env
    config = env.get("CLAUDE_CONFIG_DIR")
    return pathlib.Path(config) if config else pathlib.Path.home() / ".claude"


def settings_path(env: dict | None = None) -> pathlib.Path:
    return claude_config_dir(env) / "settings.json"


def project_slug(repo: pathlib.Path) -> str:
    """Claude Code's per-project directory name: every character of the
    absolute path that is not a letter or digit becomes ``-``.

    Observed, not assumed: ``C:\\Users\\aryan\\AppData\\Local\\Temp\\
    velra-tokenburn-fixtures\\A_cold_continuation-q1-velra`` was stored as
    ``C--Users-aryan-AppData-Local-Temp-velra-tokenburn-fixtures-A-cold-
    continuation-q1-velra`` by 2.1.272 during the qualification run.
    """
    return re.sub(r"[^A-Za-z0-9]", "-", str(pathlib.Path(repo).resolve()))


def memory_dir(repo: pathlib.Path, env: dict | None = None) -> pathlib.Path:
    """The auto-memory directory Claude Code would use for ``repo``."""
    projects = claude_config_dir(env) / "projects"
    slug = project_slug(repo)
    # Windows paths are case-insensitive; match an existing directory whose
    # name differs only in case rather than inventing a second one.
    if projects.is_dir():
        for candidate in projects.iterdir():
            if candidate.name.lower() == slug.lower():
                return candidate / "memory"
    return projects / slug / "memory"


def memory_files(directory: pathlib.Path) -> list[pathlib.Path]:
    if not directory.is_dir():
        return []
    return sorted(p for p in directory.rglob("*") if p.is_file())


# --------------------------------------------------------------------------
# the two controls
# --------------------------------------------------------------------------


def session_env(base: dict) -> dict:
    """The environment every Claude process of every trial runs with.

    One function, called for both arms: there is no argument through which
    the arms could differ.
    """
    env = dict(base)
    env[MEMORY_ENV] = MEMORY_ENV_VALUE
    return env


def trial_env(velra_home: pathlib.Path, fixture: pathlib.Path) -> dict:
    """The environment of every Claude process in every trial, both arms.

    `live_trial` binds this very function as its `trial_env`, so the preflight
    exercises the code the live run uses rather than a copy of it.
    """
    env = dict(os.environ)
    for key in safety.NESTED_MARKERS:
        env.pop(key, None)
    env["VELRA_HOME"] = str(velra_home)
    env["VELRA_LOG"] = "debug"
    env["CLAUDE_PROJECT_DIR"] = str(fixture)
    return session_env(env)


def read_settings(path: pathlib.Path) -> dict:
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError, UnicodeDecodeError):
        return {"__unparseable__": True}
    return data if isinstance(data, dict) else {"__unparseable__": True}


def apply_memory_setting(path: pathlib.Path) -> dict:
    """Set ``autoMemoryEnabled: false``, keeping every other key, and read it
    back. Returns what was verified on disk.

    The file is rewritten as JSON. `velra enable`/`disable` rewrite it too,
    which is why this runs *after* the arm is set, and why the caller keeps
    the original bytes and restores them when the trial ends.
    """
    current = read_settings(path)
    if current.get("__unparseable__"):
        return {"path": str(path), "applied": False,
                "why": "settings.json is not a JSON object; refusing to "
                       "rewrite it"}
    current[MEMORY_SETTING] = False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(current, indent=2) + "\n", encoding="utf-8",
                    newline="")
    back = read_settings(path)
    return {"path": str(path), "applied": back.get(MEMORY_SETTING) is False,
            MEMORY_SETTING: back.get(MEMORY_SETTING),
            MEMORY_DIR_SETTING: back.get(MEMORY_DIR_SETTING)}


def controls_state(env: dict, settings: pathlib.Path) -> dict:
    """The effective state of both controls, read at the moment it matters."""
    on_disk = read_settings(settings)
    env_ok = env.get(MEMORY_ENV) == MEMORY_ENV_VALUE
    setting_ok = on_disk.get(MEMORY_SETTING) is False
    relocated = on_disk.get(MEMORY_DIR_SETTING)
    return {
        "env": {MEMORY_ENV: env.get(MEMORY_ENV), "disables": env_ok},
        "settings": {"path": str(settings), MEMORY_SETTING:
                     on_disk.get(MEMORY_SETTING), "disables": setting_ok,
                     MEMORY_DIR_SETTING: relocated},
        "auto_memory_disabled": env_ok and setting_ok and relocated is None,
    }


# --------------------------------------------------------------------------
# verification
# --------------------------------------------------------------------------


def quarantine_prior_memory(directory: pathlib.Path,
                            into: pathlib.Path) -> str | None:
    """Move whatever an earlier run left in ``directory`` aside. Never delete.

    The qualification's own fixtures live at fixed paths, so a re-run in the
    same place would otherwise reopen the memory the last run wrote.
    """
    if not memory_files(directory):
        return None
    dest = into / f"memory_quarantine-{time.strftime('%Y%m%dT%H%M%S')}"
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(directory), str(dest))
    return str(dest)


def scan_memory(directory: pathlib.Path, terms: Sequence[str],
                markers: Sequence[str] = ()) -> dict:
    """Clean means *empty*. Hits are recorded as evidence, not as the test."""
    files = memory_files(directory)
    hits = leakscan.scan_auto_memory(directory, list(terms) + list(markers))
    return {
        "path": str(directory),
        "exists": directory.exists(),
        "files": [str(p.relative_to(directory)).replace("\\", "/")
                  for p in files],
        "term_hits": hits[:40],
        "clean": not files,
    }


# --------------------------------------------------------------------------
# the source handoff
# --------------------------------------------------------------------------


#: Untracked paths the harness itself produces and a handoff may contain.
HARNESS_UNTRACKED_DIRS = ("__pycache__", ".pytest_cache")


def harness_untracked_files() -> set[str]:
    """The context-load files and their metadata record, as `logs/...`."""
    return ({rel.replace("\\", "/") for rel, _, _ in context_fixture.LOG_FILES}
            | {"logs/fixture.meta.json"})


def _git(repo: pathlib.Path, *args: str) -> tuple[int, str]:
    proc = subprocess.run(["git", *args], cwd=str(repo), capture_output=True,
                          text=True, encoding="utf-8", errors="replace")
    return proc.returncode, (proc.stdout or "")


def _run_target(repo: pathlib.Path, node_id: str) -> tuple[int, str]:
    proc = subprocess.run([sys.executable, "-m", "pytest", "-q", "-p",
                           "no:cacheprovider", node_id], cwd=str(repo),
                          capture_output=True, text=True, encoding="utf-8",
                          errors="replace")
    return proc.returncode, (proc.stdout or "") + (proc.stderr or "")


def invariant_holds(repo: pathlib.Path, invariant: dict) -> bool:
    """The manifest's invariant against the tree as it stands (the same rule
    `scenarios.Scenario._invariant_holds` applies at build time)."""
    rel = invariant.get("must_hold_in_file")
    if not rel:
        return True
    path = pathlib.Path(repo) / rel
    if not path.is_file():
        return False
    text = path.read_text(encoding="utf-8")
    if any(s not in text for s in (invariant.get("must_contain") or [])):
        return False
    return not any(s in text for s in (invariant.get("must_not_contain") or []))


def worktree_violations(repo: pathlib.Path, manifest: dict) -> list[str]:
    """Everything that distinguishes the tree from the generated handoff state."""
    out: list[str] = []
    expected_head = manifest.get("fixture_head")
    _, head = _git(repo, "rev-parse", "HEAD")
    head = head.strip()
    if not expected_head:
        out.append("the manifest records no fixture_head to compare against")
    elif head != expected_head:
        out.append(f"HEAD moved: {head[:12]} is not the generated commit "
                   f"{expected_head[:12]}")
    _, heads = _git(repo, "for-each-ref", "--format=%(refname)", "refs/heads")
    extra = [r for r in heads.split() if r != "refs/heads/main"]
    if extra:
        out.append(f"branches created: {extra}")
    _, stash = _git(repo, "stash", "list")
    if stash.strip():
        out.append(f"stash entries: {len(stash.strip().splitlines())}")
    _, status = _git(repo, "status", "--porcelain=v1", "--untracked-files=all")
    allowed = harness_untracked_files()
    for line in status.splitlines():
        if not line.strip():
            continue
        code, rel = line[:2], line[3:].strip().strip('"').replace("\\", "/")
        if code == "??":
            parts = rel.split("/")
            if rel in allowed or any(p in HARNESS_UNTRACKED_DIRS for p in parts):
                continue
            out.append(f"untracked file: {rel}")
        else:
            out.append(f"tracked file changed ({code.strip()}): {rel}")
    return out


def validate_handoff(repo: pathlib.Path, manifest: dict, *,
                     run_target: Callable[[pathlib.Path, str],
                                          tuple[int, str]] = _run_target) -> dict:
    """Did the intended source state exist at the transition?

    Three conditions, all required, none repaired:

    * the target test **fails** (pytest exit 1; a pass, a collection error or
      a missing test are all violations);
    * the invariant is in the state the *generated* tree had
      (``ground_truth.as_generated.observed_invariant``);
    * the worktree is the generated commit, with no tracked change, no new
      branch, no stash, and no untracked file but the harness's own.

    The same function runs for both arms.
    """
    repo = pathlib.Path(repo)
    reasons: list[str] = []
    target = manifest.get("target_test")
    code, output = run_target(repo, target) if target else (None, "")
    status = {0: "PASS", 1: "FAIL"}.get(code, "ERROR" if target else "UNKNOWN")
    if status != "FAIL":
        reasons.append(f"target test {target} is {status} at handoff "
                       f"(pytest exit {code}); the scenario requires FAIL")

    invariant = manifest.get("invariant") or {}
    expected = ((manifest.get("ground_truth") or {}).get("as_generated")
                or {}).get("observed_invariant")
    held = invariant_holds(repo, invariant)
    if expected is None:
        reasons.append("the manifest records no as-generated invariant state")
    elif held != expected:
        reasons.append(f"invariant on {invariant.get('must_hold_in_file')} "
                       f"{'holds' if held else 'is broken'} at handoff; the "
                       f"generated tree has it "
                       f"{'holding' if expected else 'broken'}")

    tree = worktree_violations(repo, manifest)
    reasons.extend(f"worktree: {v}" for v in tree)
    return {
        "source_handoff_valid": not reasons,
        "source_handoff_reason": "; ".join(reasons) if reasons
        else "target FAIL, invariant as generated, worktree as generated",
        "target_status_at_handoff": status,
        "target_test": target,
        "target_output_tail": output.strip().splitlines()[-5:],
        "invariant_status_at_handoff": {
            "holds": held, "expected": expected,
            "matches_expected": expected is not None and held == expected,
        },
        "worktree_violations": tree,
    }


def trial_validity(*, auto_memory_disabled: bool, memory_scan_clean: bool,
                   handoff: dict | None) -> dict:
    """The single verdict the aggregate reads. Reasons are concatenated in the
    order the checks ran so the first failure is the first reason."""
    reasons: list[str] = []
    if not auto_memory_disabled:
        reasons.append("auto-memory was not verifiably disabled")
    if not memory_scan_clean:
        reasons.append("the fixture's auto-memory directory was not empty "
                       "before the destination session")
    if handoff is None:
        reasons.append("the source handoff was not validated")
    elif not handoff.get("source_handoff_valid"):
        reasons.append("source handoff invalid: "
                       + str(handoff.get("source_handoff_reason")))
    return {"valid": not reasons,
            "invalidation_reason": "; ".join(reasons) if reasons else None}


def is_invalid(meta: dict) -> bool:
    """A trial is excluded when it *says* it is invalid. Trials recorded before
    these checks existed carry no ``trial_validity`` and are left as they
    were scored."""
    validity = meta.get("trial_validity")
    return isinstance(validity, dict) and validity.get("valid") is False


# --------------------------------------------------------------------------
# preflight
# --------------------------------------------------------------------------


def preflight(work: pathlib.Path | None = None, *,
              build_fixtures: bool = True) -> dict:
    """Prove the controls without starting Claude. Every check is concrete:
    temp settings files, temp memory directories, real generated fixtures."""
    import tempfile
    if __package__ in (None, ""):
        import aggregate  # type: ignore[no-redef]
        import mock_adapter  # type: ignore[no-redef]
        import scenarios  # type: ignore[no-redef]
    else:
        from . import aggregate, mock_adapter, scenarios

    owned = work is None
    root = pathlib.Path(work or tempfile.mkdtemp(prefix="velra-isolation-"))
    root.mkdir(parents=True, exist_ok=True)
    try:
        return _preflight(root, build_fixtures, aggregate, mock_adapter,
                          scenarios)
    finally:
        if owned:
            scenarios.rmtree_force(root)


def _preflight(root: pathlib.Path, build_fixtures: bool, aggregate,
               mock_adapter, scenarios) -> dict:
    checks: list[dict] = []

    def check(name: str, ok: bool, detail: str = "") -> None:
        checks.append({"check": name, "ok": bool(ok), "detail": detail})

    # 1. memory isolation: both controls, for both arms, from one function.
    envs = {}
    for arm in ("baseline", "velra"):
        home = root / f"home-{arm}"
        envs[arm] = trial_env(home, root / "fixture")
    check("CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 in every trial environment",
          all(e.get(MEMORY_ENV) == MEMORY_ENV_VALUE for e in envs.values()))
    memory_keys = {arm: {k: v for k, v in e.items()
                         if "MEMORY" in k.upper() or k == "CLAUDE_CONFIG_DIR"}
                   for arm, e in envs.items()}
    check("baseline and Velra get identical memory controls",
          memory_keys["baseline"] == memory_keys["velra"],
          json.dumps(memory_keys))

    settings = root / "config" / "settings.json"
    settings.parent.mkdir(parents=True, exist_ok=True)
    original = b'{\n  "model": "opus",\n  "hooks": {}\n}\n'
    settings.write_bytes(original)
    applied = apply_memory_setting(settings)
    state = controls_state(envs["velra"], settings)
    check("autoMemoryEnabled=false is written and read back",
          applied["applied"] and state["auto_memory_disabled"],
          json.dumps(state))
    check("other settings keys survive",
          read_settings(settings).get("model") == "opus")
    settings.write_bytes(original)
    check("the settings round-trip restores the original bytes",
          settings.read_bytes() == original)

    # 2. the memory directory: located, quarantined, scanned.
    fake_env = {"CLAUDE_CONFIG_DIR": str(root / "config")}
    repo = root / "fixture"
    repo.mkdir(exist_ok=True)
    mem = memory_dir(repo, fake_env)
    check("memory directory follows Claude Code's project slug",
          mem.parent.name == project_slug(repo) and mem.name == "memory",
          str(mem))
    mem.mkdir(parents=True, exist_ok=True)
    (mem / "MEMORY.md").write_text("- windows close on booking_date, never "
                                   "value_date\n", encoding="utf-8")
    dirty = scan_memory(mem, ["never the value date"], ["booking_date"])
    check("a non-empty memory directory is not clean", not dirty["clean"])
    check("memory contents reach the leak scanner",
          any(h["surface"] == "auto_memory" for h in dirty["term_hits"]))
    moved = quarantine_prior_memory(mem, root / "trial")
    check("prior memory is moved aside, not deleted",
          moved is not None and pathlib.Path(moved, "MEMORY.md").exists())
    check("after quarantine the directory is clean",
          scan_memory(mem, [])["clean"])
    check("the leak scanner registers auto_memory as fatal",
          "auto_memory" in leakscan.FATAL_SURFACES
          and "auto_memory" in leakscan.ALL_SURFACES)

    # 3. the handoff, on real generated fixtures.
    if build_fixtures:
        for name, scenario in scenarios.SCENARIOS.items():
            fx = root / f"handoff-{name}"
            manifest = scenario.build(fx, verify=True)
            context_fixture.generate(fx, 20_000, name, 0)
            as_generated = validate_handoff(fx, manifest)
            check(f"{name}: the generated handoff state is valid",
                  as_generated["source_handoff_valid"],
                  as_generated["source_handoff_reason"])
            for variant in scenario.variants:
                saved = {rel: (fx / rel).read_text(encoding="utf-8")
                         for rel in variant.files}
                for rel, text in variant.files.items():
                    scenarios.write(fx / rel, text)
                solved = validate_handoff(fx, manifest)
                check(f"{name}: a source that applied '{variant.name}' is "
                      f"invalid", not solved["source_handoff_valid"]
                      and solved["target_status_at_handoff"] == "PASS",
                      solved["source_handoff_reason"])
                for rel, text in saved.items():
                    scenarios.write(fx / rel, text)
            (fx / "scratch_notes.md").write_text("x\n", encoding="utf-8")
            stray = validate_handoff(fx, manifest)
            check(f"{name}: an unexpected new file invalidates the handoff",
                  not stray["source_handoff_valid"],
                  stray["source_handoff_reason"])

    # 4. an invalid trial never reaches a verdict.
    trials = root / "trials"
    for arm in ("baseline", "velra"):
        mock_adapter.write_trial(trials, mock_adapter.MockSpec(
            trial=f"pf-{arm}", scenario="A_cold_continuation", arm=arm,
            pair_id="pf#1", handoff_target="PASS" if arm == "velra"
            else "FAIL"))
        mock_adapter.write_trial(trials, mock_adapter.MockSpec(
            trial=f"pf-mem-{arm}", scenario="B_clear_survival", arm=arm,
            pair_id="pf#2", transition="clear", memory_leak=arm == "baseline"))
    result = aggregate.run(trials, root / "out", write=False)
    dropped = {d["pair_id"]: d["reason"] for d in result["pairing"]["dropped"]}
    check("a solved-source pair is dropped as INVALID_TRIAL",
          dropped.get("pf#1") == "INVALID_TRIAL", json.dumps(dropped))
    check("a memory-leak pair is dropped as INVALID_TRIAL",
          dropped.get("pf#2") == "INVALID_TRIAL", json.dumps(dropped))
    invalid_keys = {(e["pair_id"], e["arm"]) for e in result["invalid_trials"]}
    row_keys = {(r["pair_id"], r["arm"]) for r in result["trial_rows"]}
    check("invalid trials reach no verdict, row or pool",
          not result["pairs"] and len(invalid_keys) == 2
          and not invalid_keys & row_keys
          and not result["pooled"].get("pairs"),
          f"invalid={sorted(invalid_keys)} rows={sorted(row_keys)}")

    # 5. the machine this would run on: the real settings file can carry the
    #    control, and nothing relocates the memory directory. Read-only.
    real = settings_path()
    on_disk = read_settings(real)
    check("the user settings file is a JSON object the harness can round-trip",
          not on_disk.get("__unparseable__"), str(real))
    check("autoMemoryDirectory is not set (memory stays per project)",
          on_disk.get(MEMORY_DIR_SETTING) is None, str(real))

    return {"ok": all(c["ok"] for c in checks), "checks": checks}


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--preflight", action="store_true")
    ap.add_argument("--no-fixtures", action="store_true",
                    help="skip building the real fixtures (no git/pytest)")
    args = ap.parse_args()
    if not args.preflight:
        ap.print_help()
        return 0
    result = preflight(build_fixtures=not args.no_fixtures)
    for c in result["checks"]:
        print(f"  [{'PASS' if c['ok'] else 'FAIL'}] {c['check']}"
              + (f" -- {c['detail']}" if not c["ok"] and c["detail"] else ""))
    print("\nISOLATION PREFLIGHT " + ("PASSED" if result["ok"] else "FAILED")
          + " -- no Claude process was started.")
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
