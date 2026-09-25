# Configuration

Velra needs no configuration. Everything on this page is optional.

- [Files and directories](#files-and-directories)
- [`config.toml`](#configtoml)
- [Environment variables](#environment-variables)
- [Hook registration](#hook-registration)
- [SessionStart behaviour](#sessionstart-behaviour)
- [Claude Code version compatibility](#claude-code-version-compatibility)
- [Failure behaviour](#failure-behaviour)
- [Debug and trace controls](#debug-and-trace-controls)
- [Inspecting the active configuration](#inspecting-the-active-configuration)

---

## Files and directories

Velra's state lives in **`$VELRA_HOME`**: by default `~/.velra`, or
`%USERPROFILE%\.velra` on Windows. On POSIX it is created `0700` and files are
`0600`. Velra never writes inside your repository.

| Path | Contents | Safe to delete? |
|---|---|---|
| `velra.db` (+ `-wal`, `-shm`) | SQLite event log and derived task state, schema v2, WAL mode | only with `velra disable --purge`; all history is lost |
| `spool/` | events written while the database was busy; ingested by the next reducer pass | no: it is recorded state waiting to land |
| `staged/<workspace_id>/capsule.<gen>.json` (and `.claimed`) | a capsule staged by `velra restore`, and its claim while a session start delivers it | use `velra restore --clear` |
| `logs/errors.log` | one line per internal error or refusal; rotated at 1 MiB, 3 files kept | yes |
| `logs/debug.log` | per-invocation timing and delivery events, only with `VELRA_LOG=debug` | yes |
| `backups/settings.json.<timestamp>.bak` | your Claude Code settings before each `enable`/`disable` | kept even by `--purge` |
| `state.json` | what `velra enable` did: binary path, settings path, versions | written by `enable` |
| `config.toml` | your optional settings | yours |
| `disabled` | if this file exists, every hook exits immediately | yours: the kill switch |
| `bin/`, `cache/` | installer binary, npm launcher download cache | by uninstall |
| `velra.db.corrupt-*` | a database Velra could not open, moved aside before starting fresh | after you have looked |

The one file outside `$VELRA_HOME` that Velra edits is your user-level
Claude Code settings file ([Hook registration](#hook-registration)).

## `config.toml`

`$VELRA_HOME/config.toml` is read if it exists. One key is supported:

```toml
# Capsule target size, in Velra's estimated tokens.
# Default 740. Values below 64 are raised to 64. The renderer never exceeds
# its hard ceiling of 1,000 tokens / 9,500 characters, whatever you set.
budget_tokens = 600
```

`budget_tokens` applies to every capsule Velra renders: `/compact`
continuations, `velra restore`, and `velra inspect` previews. A smaller
budget drops more detail via the truncation ladder (`velra inspect --trace`
names the rung). A larger one keeps more, up to the ceiling.

An unreadable or invalid file is ignored, and the defaults apply. To confirm
the value in effect, check the token count in `velra restore --dry-run --json`.

## Environment variables

Runtime, read by the binary:

| Variable | Effect | Default |
|---|---|---|
| `VELRA_HOME` | state directory | `~/.velra` |
| `VELRA_DISABLE=1` | kill switch: every hook exits immediately without touching the database | unset |
| `VELRA_LOG=debug` | write per-invocation timing and delivery lines to `logs/debug.log` | unset |
| `VELRA_CLAUDE_VERSION` | assume this Claude Code version instead of detecting it, for example `2.1.280` | detected |
| `CLAUDE_CONFIG_DIR` | where Claude Code's `settings.json` lives (honoured as Claude Code does) | `~/.claude` |
| `CLAUDE_PROJECT_DIR` | the workspace root, overriding the git-root search (Claude Code sets it for hooks) | unset |
| `NO_COLOR` | plain CLI output | unset |

The hooks read the environment of the **Claude Code process**, so hook
variables such as `VELRA_LOG` and `VELRA_DISABLE` must be set where you start
`claude`, not in the terminal where you run `velra`.

Install time only, read by the installers and the npm launcher:
`VELRA_VERSION`, `VELRA_NO_MODIFY_PATH`, `VELRA_DOWNLOAD_BASE`,
`VELRA_BINARY`, `VELRA_NO_DOWNLOAD` ([Install](INSTALL.md)).

## Hook registration

`velra enable` edits `~/.claude/settings.json` (or
`$CLAUDE_CONFIG_DIR/settings.json`). It adds these handlers, each running
`<absolute path to velra> <args>`:

| Event | Matcher | Command | Mode | Timeout | Registered when |
|---|---|---|---|---|---|
| `SessionStart` | — | `hook session-start` | sync | 10 s | Claude Code ≥ 1.0.62 |
| `UserPromptSubmit` | — | `hook user-prompt-submit` | sync | 10 s | always |
| `PreToolUse` | `Write\|Edit\|MultiEdit\|NotebookEdit` | `hook pre-tool-use` | sync | 10 s | always |
| `PreToolUse` | `Bash` | `hook pre-tool-use` | sync | 10 s | always |
| `PreToolUse` | `PowerShell` | `hook pre-tool-use` | sync | 10 s | ≥ 2.1.84 |
| `PostToolUse` | `*` | `hook post-tool-use` | sync | 10 s | always |
| `PostToolUseFailure` | `*` | `hook post-tool-use-failure` | sync | 10 s | ≥ 2.1.119 |
| `PostToolBatch` | — | `reduce` | async | 30 s | ≥ 2.1.268 |
| `Stop` | — | `hook stop` | sync | 10 s | always |
| `Stop` | — | `reduce` | async | 30 s | ≥ 2.1.23 |
| `PreCompact` | — | `hook pre-compact` | sync | 10 s | ≥ 1.0.48 |
| `PostCompact` | — | `hook post-compact` | async | 30 s | ≥ 2.1.76 |
| `SessionEnd` | — | `hook session-end` | sync | 1 s | ≥ 1.0.85 |

Only Velra's own handlers are ever added or removed. Your other hooks, other
settings, comments and key order are left alone. Project-level
`.claude/settings*.json` is never touched. If `"disableAllHooks": true` is
set, Velra's hooks do not run, and `enable` and `doctor` warn about it.

## SessionStart behaviour

`SessionStart` is where both kinds of capsule are delivered:

| Source | In-session continuation (after `/compact`) | Staged capsule (`velra restore`) |
|---|---|---|
| `startup` | — | **delivered once** |
| `compact` | delivered | left staged |
| `resume` | delivered, if one is live | left staged |
| `clear` | the live continuation is expired; the recorded state is kept | left staged |
| `fork` / unknown | — | left staged |

The in-session continuation is also delivered on the first
`UserPromptSubmit` or `PostToolUse` after compaction, whichever fires first.
Its `PENDING → ATTACHED → CONFIRMED` state machine makes delivery exactly
once ([Architecture](ARCHITECTURE.md#two-delivery-paths)).

Nothing about this is configurable. The eligible sources are data inside
each staged record (`deliver_on`), not a setting.

## Claude Code version compatibility

`velra enable`, `status` and `doctor` detect the Claude Code version in this
order:

1. `VELRA_CLAUDE_VERSION`, if set;
2. `claude --version` (on Windows also `cmd /C claude --version`, for npm's
   `claude.cmd`), with a 3-second timeout;
3. the installed VS Code extension's version;
4. otherwise, assume the newest version this build knows (2.1.269), and say
   so.

The version decides which hooks are registered (table above). Other gates:
the exec form of hook commands (≥ 2.1.139), the `if` field (≥ 2.1.85), and
`prompt_id` in hook input (≥ 2.1.196; older versions fall back to timestamps
for delivery keys, and `doctor` warns). After upgrading Claude Code across
one of those versions, run `velra enable` again to pick up the new hooks.

The `SessionStart` payloads Velra depends on are replayed from recorded
fixtures in the test suite for Claude Code 2.1.272. The live benchmark ran on
2.1.280.

## Failure behaviour

Every hook invocation:

- exits **0**, always, including on panic, timeout, a locked or corrupt
  database, or a read-only `$VELRA_HOME`;
- writes **nothing** to stderr;
- writes to stdout either nothing or exactly one JSON object;
- gives up rather than hold Claude Code up indefinitely: a 250 ms watchdog on synchronous
  hooks and 1 s on the async reducer. An event in flight when the watchdog
  fires is written to the spool, not lost.

Errors are recorded in `logs/errors.log` and surfaced by `velra doctor`. If
Velra is ever the reason a Claude Code session misbehaves, that is a bug;
[report it](../SECURITY.md#reporting-a-vulnerability) or open an issue.

To switch Velra off:

| Scope | How |
|---|---|
| one Claude Code session | `VELRA_DISABLE=1 claude` |
| every session, until undone | create `~/.velra/disabled` |
| remove the hooks | `velra disable` |

## Debug and trace controls

| Control | What it gives you |
|---|---|
| `VELRA_LOG=debug` (Claude Code's environment) | `logs/debug.log`: per-hook phase timing, and `restored staged capsule from session <id> (<n> tokens, <hash>)` on every staged delivery |
| `logs/errors.log` (always on) | internal errors and refusals, such as a stale or corrupt staged capsule |
| `velra inspect --trace <marker>` | where a string was lost between the recorded events and the capsule, and why |
| `velra inspect --section <name>` | untruncated detail behind a capsule section |
| `velra restore --dry-run` | the exact capsule that would be staged |

See [Troubleshooting](TROUBLESHOOTING.md) for how to use them.

## Inspecting the active configuration

```bash
velra status --json     # binary, detected Claude Code version, database, handler counts
velra doctor            # settings file, registrations, database, logs
velra enable --dry-run  # the diff between your settings and what Velra would register
cat ~/.velra/state.json # what `velra enable` recorded
cat ~/.velra/config.toml
```

---

[← README](../README.md) · [Install](INSTALL.md) · [CLI reference](CLI.md) · [Troubleshooting](TROUBLESHOOTING.md)
