# End-to-end checklist (§21 I)

A scripted manual pass against a real Claude Code install. Required before
every release. Record the Claude Code version and paste the result table into
the release PR.

| Field | Value |
|---|---|
| Velra version | `velra --version` output |
| Claude Code version | `claude --version` output |
| Platform | macOS / Linux / Windows + arch |
| Date | |
| Performed by | |

## Setup

1. Fresh install from the published artifact (not a local build):

   ```sh
   curl -LsSf https://{{VELRA_DOMAIN}}/install.sh | sh
   velra enable
   velra doctor          # every line ✓ or an explained !
   ```

2. Create a sample repository with a failing test:

   ```sh
   mkdir -p /tmp/velra-e2e && cd /tmp/velra-e2e && git init
   printf 'def add(a, b):\n    return a - b\n' > calc.py
   printf 'from calc import add\n\ndef test_add():\n    assert add(2, 2) == 4\n' > test_calc.py
   git add -A && git commit -m "initial"
   ```

## Steps

| # | Step | Expected | Result |
|---|---|---|---|
| 1 | Start `claude` in the sample repo. | Session starts normally; no hook errors in the transcript. | |
| 2 | Prompt (≥ 20 chars): "fix the failing add test without changing the test file". | — | |
| 3 | Let Claude edit `calc.py` and run `pytest` (it fails at least once). | — | |
| 4 | Ask Claude to revert with `git restore calc.py`, then try a different edit. | — | |
| 5 | In another terminal: `velra inspect`. | Capsule shows ROOT_TASK_OBJECTIVE, ACTIVE_FAILURE, one DEAD_ENDS entry (`reverted via \`git restore calc.py\``), and WORKING_FILES. | |
| 6 | Run `/compact` in Claude Code. | Compaction completes normally. Note which messages surfaced: "⚡ Velra checkpoint saved" (PreCompact `systemMessage` is discarded by Claude Code on current versions) and "⚡ Velra restored: … (N tokens)" on the next SessionStart. | |
| 7 | Ask: "what should we try next?" | The answer does **not** re-propose the reverted change, and refers to the recorded failure. | |
| 8 | `velra inspect --checkpoint <id from step 6> --section dead-ends`. | Full, untruncated dead-end detail. | |
| 9 | Repeat with **auto-compaction**: long session (or reduce the auto-compact window) and no user interaction. | Capsule delivered mid-turn on the first `PostToolUse` after compaction, or on `SessionStart(compact)`. | |
| 10 | Remove the `SessionStart` handler temporarily, compact, then press Ctrl+C immediately after the first post-compaction prompt. | The next prompt receives the capsule again (T4 re-emission). Restore the handler afterwards. | |
| 11 | `velra status`. | Enabled, session count ≥ 1, continuation CONFIRMED. Exit code 0. | |
| 12 | `velra disable`. | Hooks gone; `git diff` of the settings file (if version-controlled) is empty, or the file is byte-identical to the pre-enable backup in `~/.velra/backups/`. | |
| 13 | Continue using Claude Code for one more turn. | Unaffected: no hook errors, no messages from Velra. | |

## Verification commands

```sh
# The settings file is byte-identical to the backup taken before `enable`:
diff <(cat ~/.claude/settings.json) ~/.velra/backups/settings.json.*.bak && echo "byte-identical"

# No stderr and exit 0 from every hook, with a real payload:
cat tests/fixtures/claude-code/*/post_tool_use_edit.json | velra hook post-tool-use; echo "exit=$?"

# Nothing was written inside the repository:
git -C /tmp/velra-e2e status --porcelain
```

## Sign-off

- [ ] Every step above passed on the recorded Claude Code version.
- [ ] `velra doctor` is clean on a fresh install.
- [ ] Zero-to-"⚡ Velra restored" took two commands and one `/compact`.
