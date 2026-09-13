#!/usr/bin/env python3
"""Phase 1: verify the binary and prove the settings edit is non-destructive.

Two things are checked, and both produce evidence rather than a claim:

1. ``velra --version`` / ``status`` / ``doctor`` run clean.
2. ``enable`` followed by ``disable`` restores the settings file *byte for
   byte*. This is checked twice: once against the user's real Claude Code
   settings file, and once against a synthetic JSONC file that carries
   comments, an unusual key order and a foreign hook belonging to somebody
   else, because that is the case a naive JSON round-trip would destroy.

The synthetic check runs against a throwaway CLAUDE_CONFIG_DIR, so it cannot
touch anything the user cares about.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent

JSONC_SAMPLE = """{
  // my personal settings - do not reformat
  "model": "opus",
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "/usr/local/bin/my-own-audit.sh" }
        ]
      }
    ]
  },
  /* trailing comment block */
  "effortLevel": "high"
}
"""


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(binary, env, *args):
    # Explicit UTF-8: Velra prints U+2713, and the locale codec would put
    # mojibake into phase1_environment.json on Windows.
    return subprocess.run([str(binary), *args], capture_output=True, text=True,
                          encoding="utf-8", errors="replace", env=env)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary",
                    default=str(REPO_ROOT / "target" / "release" / "velra.exe"))
    ap.add_argument("--out", default="bench/results/phase1_environment.json")
    ap.add_argument("--work", default=None)
    args = ap.parse_args()

    binary = pathlib.Path(args.binary).resolve()
    env = dict(os.environ)
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)

    evidence: dict = {"binary": str(binary), "binary_bytes": binary.stat().st_size}

    version = run(binary, env, "--version")
    evidence["version"] = {"exit": version.returncode,
                           "stdout": version.stdout.strip(),
                           "stderr": version.stderr.strip()}
    print(f"velra --version -> {version.stdout.strip()} (exit {version.returncode})")

    # ---- 1. the real settings file, enable -> disable ---------------------
    settings = pathlib.Path(
        env.get("CLAUDE_CONFIG_DIR", os.path.expanduser("~/.claude"))) / "settings.json"
    real: dict = {"path": str(settings), "exists": settings.exists()}
    if settings.exists():
        before_hash = sha256(settings)
        before_bytes = settings.stat().st_size

        dry = run(binary, env, "enable", "--dry-run")
        real["dry_run_exit"] = dry.returncode
        real["dry_run_changed_file"] = sha256(settings) != before_hash
        real["dry_run_diff_lines"] = len(dry.stdout.splitlines())

        enabled = run(binary, env, "enable")
        after_enable_hash = sha256(settings)
        # Capture the size *now*, while the hooks are still registered. Reading
        # it later in the result dict would stat the file after `disable` has
        # already restored it, and silently report the wrong number.
        after_enable_bytes = settings.stat().st_size
        status = run(binary, env, "status")
        doctor = run(binary, env, "doctor")
        handlers = json.loads(settings.read_text(encoding="utf-8-sig")).get("hooks", {})
        disabled = run(binary, env, "disable")
        after_disable_hash = sha256(settings)

        real.update({
            "before_sha256": before_hash,
            "before_bytes": before_bytes,
            "enable_exit": enabled.returncode,
            "after_enable_sha256": after_enable_hash,
            "after_enable_bytes": after_enable_bytes,
            "hook_events_registered": sorted(handlers),
            "hook_handler_count": sum(
                len(h.get("hooks", []))
                for entries in handlers.values() for h in entries),
            "status_exit_when_enabled": status.returncode,
            "status_stdout": status.stdout,
            "doctor_exit_when_enabled": doctor.returncode,
            "doctor_stdout": doctor.stdout,
            "disable_exit": disabled.returncode,
            "after_disable_sha256": after_disable_hash,
            "after_disable_bytes": settings.stat().st_size,
            "restored_byte_for_byte": after_disable_hash == before_hash,
        })
        print(f"real settings: enable -> {real['hook_handler_count']} handlers across "
              f"{len(real['hook_events_registered'])} events")
        print(f"real settings: disable -> restored byte for byte = "
              f"{real['restored_byte_for_byte']}")
    evidence["real_settings"] = real

    # ---- 2. a JSONC file with comments and somebody else's hook -----------
    work = pathlib.Path(args.work or (
        pathlib.Path(os.environ.get("TEMP", "/tmp")) / "velra-phase1"))
    if work.exists():
        shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    sample = work / "settings.json"
    sample.write_text(JSONC_SAMPLE, encoding="utf-8", newline="")
    original = sample.read_bytes()

    jsonc_env = dict(env)
    jsonc_env["CLAUDE_CONFIG_DIR"] = str(work)
    run(binary, jsonc_env, "enable")
    enabled_text = sample.read_text(encoding="utf-8")
    run(binary, jsonc_env, "disable")
    restored = sample.read_bytes()

    evidence["jsonc_settings"] = {
        "path": str(sample),
        "original_sha256": hashlib.sha256(original).hexdigest(),
        "restored_sha256": hashlib.sha256(restored).hexdigest(),
        "restored_byte_for_byte": original == restored,
        "foreign_hook_survived_enable": "my-own-audit.sh" in enabled_text,
        "line_comment_survived_enable": "do not reformat" in enabled_text,
        "block_comment_survived_enable": "trailing comment block" in enabled_text,
        "velra_handlers_while_enabled": enabled_text.count("velra"),
    }
    j = evidence["jsonc_settings"]
    print(f"jsonc settings: foreign hook kept = {j['foreign_hook_survived_enable']}, "
          f"comments kept = {j['line_comment_survived_enable'] and j['block_comment_survived_enable']}, "
          f"restored byte for byte = {j['restored_byte_for_byte']}")

    ok = (version.returncode == 0
          and real.get("restored_byte_for_byte", False)
          and j["restored_byte_for_byte"]
          and j["foreign_hook_survived_enable"])
    evidence["all_checks_passed"] = ok

    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(evidence, indent=2), encoding="utf-8", newline="")
    print(f"\nphase 1: {'PASS' if ok else 'FAIL'}  ->  {out}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
