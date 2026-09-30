#!/usr/bin/env python3
"""Replay the frozen v0.1.2 source ledgers through a Velra binary, offline.

    python bench/tokenburn/replay.py --binary target/release/velra.exe \\
        --out bench/results/<new directory>

The live requalification (``bench/results/v0.1.2-requal/``) ran against the
build at 51b96cb. Every Velra-arm trial kept its source session's ledger
(``trials/*-velra/velra_home/velra.db``). Those files are not committed, but
``raw_captures.sha256.json`` fixes each one by size and SHA-256, so anyone who
holds them can check they are the ones the run wrote.

This script asks one question of a *later* binary: given exactly the state
those source sessions recorded, does this build still carry the declared
markers from the ledger, through ``velra restore``, into the bytes a new
session's ``SessionStart(startup)`` receives?

For each Velra trial it:

1. verifies ``velra.db`` against the frozen manifest and refuses on mismatch;
2. copies it into a temporary ``VELRA_HOME`` (the evidence tree is only read);
3. recreates the source workspace directory at its recorded path if it no
   longer exists, so workspace identity resolves exactly as it did live, and
   checks the resolved ``workspace_id`` against the frozen one;
4. runs the real ``velra restore --session <source> --json``;
5. traces every declared marker with ``velra inspect --trace``;
6. delivers with the real ``velra hook session-start`` (source ``startup``,
   a new session id), then starts a second session to check nothing is
   delivered twice;
7. repeats 2-4 in a second, independent copy and compares the capsules.

What it does not do: start Claude, call a model, or measure anything a live
session would (use of the state, correctness, token burden). Those remain the
live run's results, for the build it ran.

Nothing here writes capsule text or markers. Markers are read from each
trial's frozen ``trial_meta.json`` (``manifest.capsule_markers``).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import runroot  # type: ignore[no-redef]
else:
    from . import runroot

EVIDENCE = runroot.REQUAL
CAPTURED = re.compile(r' captured="[^"]*"')

# Velra's output carries U+26A1 and arrows; a cp1252 console cannot print them.
for stream in (sys.stdout, sys.stderr):
    if hasattr(stream, "reconfigure"):
        stream.reconfigure(encoding="utf-8", errors="replace")


def sha256(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(binary, args, env, cwd, stdin=None):
    r = subprocess.run([str(binary), *args], input=(stdin or "").encode(),
                       capture_output=True, env=env, cwd=cwd, timeout=120)
    return r.returncode, r.stdout.decode("utf-8", "replace"), \
        r.stderr.decode("utf-8", "replace")


def environment(home: pathlib.Path, config: pathlib.Path, root: str) -> dict:
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("VELRA_", "CLAUDE"))}
    env.update(VELRA_HOME=str(home), CLAUDE_CONFIG_DIR=str(config),
               CLAUDE_PROJECT_DIR=root, VELRA_CLAUDE_VERSION="2.1.280")
    return env


def restore_once(binary, db: pathlib.Path, root: str, session: str,
                 scratch: pathlib.Path, tag: str):
    """Stage from a fresh copy of ``db``. Returns (result, staged, env)."""
    home, config = scratch / f"home-{tag}", scratch / f"claude-{tag}"
    home.mkdir(parents=True)
    config.mkdir(parents=True)
    shutil.copyfile(db, home / "velra.db")
    env = environment(home, config, root)
    code, out, err = run(binary, ["restore", "--session", session, "--json"],
                         env, root)
    result = {"exit": code, "stderr": err.strip()}
    staged = None
    if code == 0:
        body = json.loads(out)
        result.update({k: body.get(k) for k in
                       ("workspace_id", "source_session_id", "tokens",
                        "content_hash", "summary", "staged")})
        staged_path = pathlib.Path(body["staged_path"])
        staged = json.loads(staged_path.read_text(encoding="utf-8"))
    else:
        result["stdout"] = out.strip()
    return result, staged, env


def replay_trial(binary, trial: pathlib.Path, manifest: dict,
                 scratch: pathlib.Path) -> dict:
    meta = json.loads((trial / "trial_meta.json").read_text(encoding="utf-8"))
    frozen = json.loads((trial / "velra_restore.json").read_text(encoding="utf-8"))
    markers = list(meta["manifest"]["capsule_markers"])
    session = frozen["source_session_id"]
    root = frozen["workspace_root"]
    rel = f"trials/{trial.name}/velra_home/velra.db"
    db = trial / "velra_home" / "velra.db"

    record: dict = {"trial": trial.name, "scenario": meta["scenario"],
                    "source_session_id": session, "markers": markers}
    expected = manifest.get(rel)
    if expected is None or not db.exists():
        record["status"] = "INPUT_MISSING"
        return record
    actual = sha256(db)
    record["input"] = {"path": rel, "sha256": actual,
                       "manifest_sha256": expected,
                       "verified": actual == expected}
    if actual != expected:
        record["status"] = "INPUT_MISMATCH"
        return record

    created_root = False
    if not os.path.isdir(root):
        os.makedirs(root)
        created_root = True
    # Git metadata ([WORKSPACE_STATE]) is read from the workspace on disk when
    # the capsule is rendered. A recreated directory holds no repository, so
    # that line reads "no git" where the live capsule named a branch and commit.
    record["workspace_recreated_without_repository"] = created_root
    try:
        first, staged, env = restore_once(binary, db, root, session, scratch,
                                          f"{trial.name}-1")
        record["restore"] = first
        record["workspace_id_frozen"] = frozen["workspace_id"]
        record["workspace_id_matches"] = first.get("workspace_id") == frozen["workspace_id"]
        if staged is None:
            record["status"] = "RESTORE_FAILED"
            return record

        capsule = staged["capsule"]
        frozen_capsule = frozen["staged"]["capsule"]
        record["staged"] = {
            "intent": staged.get("intent"),
            "deliver_on": staged.get("deliver_on"),
            "tokens": staged.get("tokens"),
            "chars": len(capsule),
            "markers_present": [m for m in markers if m in capsule],
            "markers_missing": [m for m in markers if m not in capsule],
            "names_another_session": "another session's" in capsule,
            "detail_command_names_source": f"velra inspect --session {session[:8]}" in capsule,
            "capsule": capsule,
        }
        record["frozen"] = {
            "tokens": frozen["staged"].get("tokens"),
            "chars": len(frozen_capsule),
            "markers_present": [m for m in markers if m in frozen_capsule],
            "markers_missing": [m for m in markers if m not in frozen_capsule],
        }

        trace_args = ["inspect", "--session", session, "--json"]
        for m in markers:
            trace_args += ["--trace", m]
        code, out, err = run(binary, trace_args, env, root)
        traces = {}
        if code == 0:
            body = json.loads(out)
            for t in body if isinstance(body, list) else body.get("traces", []):
                traces[t.get("marker")] = t.get("first_loss") or "none"
        record["trace"] = {"exit": code, "first_loss": traces,
                           "stderr": err.strip()}

        destination = f"replay-destination-{trial.name}"
        payload = json.dumps({"session_id": destination,
                              "hook_event_name": "SessionStart",
                              "source": "startup", "cwd": root})
        code, out, err = run(binary, ["hook", "session-start"], env, root, payload)
        delivered = {"exit": code, "stderr_empty": err == "",
                     "stdout_objects": 0, "additional_context": None}
        if out.strip():
            obj = json.loads(out)
            delivered["stdout_objects"] = 1
            ctx = obj.get("hookSpecificOutput", {}).get("additionalContext", "")
            delivered["additional_context_equals_staged"] = ctx == capsule
            delivered["markers_present"] = [m for m in markers if m in ctx]
            delivered["markers_missing"] = [m for m in markers if m not in ctx]
            delivered["system_message"] = obj.get("systemMessage")
            delivered["system_message_names_source"] = session[:8] in (obj.get("systemMessage") or "")
        del delivered["additional_context"]
        record["delivery"] = delivered

        payload2 = json.dumps({"session_id": destination + "-second",
                               "hook_event_name": "SessionStart",
                               "source": "startup", "cwd": root})
        code, out, err = run(binary, ["hook", "session-start"], env, root, payload2)
        record["second_startup"] = {"exit": code, "stderr_empty": err == "",
                                    "delivered": bool(out.strip())}

        second, staged2, _ = restore_once(binary, db, root, session, scratch,
                                          f"{trial.name}-2")
        same = (staged2 is not None
                and CAPTURED.sub("", staged2["capsule"]) == CAPTURED.sub("", capsule))
        record["determinism"] = {
            "second_restore_exit": second["exit"],
            "identical_apart_from_captured_timestamp": same,
            "tokens": [staged.get("tokens"), None if staged2 is None else staged2.get("tokens")],
        }

        ok = (record["workspace_id_matches"]
              and not record["staged"]["markers_missing"]
              and delivered["exit"] == 0 and delivered["stderr_empty"]
              and delivered["stdout_objects"] == 1
              and delivered.get("additional_context_equals_staged") is True
              and not delivered.get("markers_missing")
              and record["second_startup"]["exit"] == 0
              and not record["second_startup"]["delivered"]
              and same)
        record["status"] = "PASS" if ok else "FAIL"
        return record
    finally:
        if created_root:
            shutil.rmtree(root, ignore_errors=True)
            parent = os.path.dirname(root)
            try:
                os.rmdir(parent)
            except OSError:
                pass


def report(result: dict) -> str:
    lines = [
        "# Final-build replay of the frozen v0.1.2 source ledgers",
        "",
        "Written by `bench/tokenburn/replay.py`. Every value below is copied from",
        "`replay.json` in this directory; nothing here is written by hand.",
        "",
        f"- Binary: `{result['binary']['version']}` (sha256 `{result['binary']['sha256'][:16]}…`)",
        f"- Source commit of the replaying tree: `{result['git_head']}`"
        + ("" if not result["working_tree"]["uncommitted_product_paths"] else
           f" plus uncommitted changes to {', '.join('`' + p + '`' for p in result['working_tree']['uncommitted_product_paths'])}"),
        f"- Inputs: the Velra-arm source ledgers of `{result['evidence']}`, verified against `raw_captures.sha256.json`",
        f"- Platform: {result['platform']}",
        f"- Replayed: {result['replayed_at']}",
        "",
        "| Trial | Input verified | Workspace id matches | Tokens then → now | Chars then → now | Markers in staged capsule | Markers in delivered context | Second startup delivered | Deterministic | Status |",
        "|---|:-:|:-:|---:|---:|---|---|:-:|:-:|---|",
    ]
    for t in result["trials"]:
        if "staged" not in t:
            lines.append(f"| {t['trial']} | | | | | | | | | {t['status']} |")
            continue
        n = len(t["markers"])
        lines.append(
            f"| {t['trial']} | {'yes' if t['input']['verified'] else 'NO'} | "
            f"{'yes' if t['workspace_id_matches'] else 'NO'} | "
            f"{t['frozen']['tokens']} → {t['staged']['tokens']} | "
            f"{t['frozen']['chars']} → {t['staged']['chars']} | "
            f"{len(t['staged']['markers_present'])}/{n} | "
            f"{len(t['delivery'].get('markers_present', []))}/{n} | "
            f"{'yes' if t['second_startup']['delivered'] else 'no'} | "
            f"{'yes' if t['determinism']['identical_apart_from_captured_timestamp'] else 'NO'} | "
            f"{t['status']} |")
    if any(t.get("workspace_recreated_without_repository") for t in result["trials"]):
        lines += ["", "The source workspaces no longer exist, so each was recreated as an empty",
                  "directory at its recorded path. Git metadata is read from disk at render",
                  "time, so `[WORKSPACE_STATE]` reads `no git` where the live capsule named a",
                  "branch and commit; token and character counts include that difference."]
    lines += ["", f"Overall: **{result['overall']}**", ""]
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--binary", required=True, type=pathlib.Path)
    ap.add_argument("--out", required=True, type=pathlib.Path,
                    help="a new or empty directory; never a protected evidence tree")
    args = ap.parse_args(argv)

    out = args.out.resolve()
    runroot.assert_writable(out, "the replay results")
    if out.exists() and any(out.iterdir()):
        print(f"refusing to write into non-empty {out}", file=sys.stderr)
        return 3
    binary = args.binary.resolve()
    if not binary.exists():
        print(f"no binary at {binary}", file=sys.stderr)
        return 3

    manifest_doc = json.loads((EVIDENCE / "raw_captures.sha256.json").read_text(encoding="utf-8"))
    manifest = {f["path"]: f["sha256"] for f in manifest_doc["files"]}
    version = subprocess.run([str(binary), "--version"], capture_output=True,
                             text=True).stdout.strip()
    head = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True,
                          text=True, cwd=runroot.REPO_ROOT).stdout.strip()
    # `velra --version` names HEAD and nothing else, so say whether the tree
    # the binary was built from had product changes on top of it.
    porcelain = subprocess.run(["git", "status", "--porcelain"], capture_output=True,
                               text=True, cwd=runroot.REPO_ROOT).stdout.splitlines()
    product = sorted(line[3:] for line in porcelain
                     if line[3:].startswith(("crates/", "Cargo.")))

    trials = sorted(p for p in (EVIDENCE / "trials").iterdir()
                    if p.is_dir() and p.name.endswith("-velra"))
    with tempfile.TemporaryDirectory(prefix="velra-replay-") as tmp:
        records = [replay_trial(binary, t, manifest, pathlib.Path(tmp)) for t in trials]

    import datetime
    result = {
        "what": "final-build replay of the frozen v0.1.2 requalification source ledgers",
        "evidence": EVIDENCE.relative_to(runroot.REPO_ROOT).as_posix(),
        "binary": {"version": version, "sha256": sha256(binary)},
        "git_head": head,
        "working_tree": {"clean": not porcelain,
                         "uncommitted_product_paths": product},
        "platform": f"{platform.system()} {platform.release()} {platform.machine()}",
        "python": platform.python_version(),
        "replayed_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "trials": records,
        "overall": "PASS" if records and all(r["status"] == "PASS" for r in records) else "FAIL",
    }
    out.mkdir(parents=True, exist_ok=True)
    with open(out / "replay.json", "w", encoding="utf-8", newline="\n") as f:
        json.dump(result, f, indent=2, ensure_ascii=False)
        f.write("\n")
    with open(out / "replay.md", "w", encoding="utf-8", newline="\n") as f:
        f.write(report(result))
    print(report(result))
    return 0 if result["overall"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
