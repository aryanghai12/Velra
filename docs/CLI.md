# CLI reference

This page covers every user-facing command in Velra 0.1.2. The help text
below is the binary's own `--help` output. `velra <command> --help` on your
machine is authoritative.

```
$ velra --help
Local-first session continuity for Claude Code: clear the context, keep the state.

Usage: velra <COMMAND>

Commands:
  enable   Register Velra's hooks in your Claude Code settings
  disable  Remove Velra's hooks from your Claude Code settings
  status   Show whether Velra is enabled and what it is tracking
  inspect  Show what would survive a /compact right now
  doctor   Diagnose the installation
  restore  Carry a previous session's task state into your next one
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

| Command | Use it to |
|---|---|
| [`velra enable`](#velra-enable) | register the hooks (once per machine) |
| [`velra disable`](#velra-disable) | remove them; optionally purge Velra's data |
| [`velra status`](#velra-status) | check that Velra is on, see what it tracks and whether a capsule is staged |
| [`velra doctor`](#velra-doctor) | diagnose a broken installation |
| [`velra restore`](#velra-restore) | carry a previous session's state into your next session |
| [`velra inspect`](#velra-inspect) | see the capsule, drill into one section, or trace where a fact was lost |
| [`velra --version`](#velra---version) | print the version, commit and target |

**Conventions.** Human output goes to stdout, with `✓` for OK, `!` for a
warning and `✗` for a failure. Colour is off when stdout is not a terminal
or `NO_COLOR` is set. `--json` output is pretty-printed JSON on stdout. A
usage error (unknown flag) exits **2**, with clap's message on stderr.

State lives in `$VELRA_HOME` (default `~/.velra`). Commands that act on "this
workspace" resolve it from the current directory: `$CLAUDE_PROJECT_DIR` if
set, otherwise the nearest ancestor containing `.git`, otherwise the directory
itself ([details](ARCHITECTURE.md#workspaces-and-sessions)).

---

## `velra enable`

```
Register Velra's hooks in your Claude Code settings

Usage: velra enable [OPTIONS]

Options:
      --dry-run  Print the change as a diff without writing anything
  -h, --help     Print help
```

Writes Velra's hook handlers into the user-level Claude Code settings file
(`~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR/settings.json`; a symlinked
file is followed). It detects the Claude Code version first and skips hooks
that version does not support. It backs the file up to
`~/.velra/backups/settings.json.<timestamp>.bak` before writing, and preserves
comments, key order and formatting.

```
$ velra enable
✓ Velra enabled for Claude Code.
  Settings: /home/you/.claude/settings.json  (backup: /home/you/.velra/backups/settings.json.20260923T120909Z.bak)

Nothing else required. Keep coding normally.
Tip: run `velra inspect` any time to see what would survive a /compact.
```

| Situation | Output | Exit |
|---|---|---|
| already enabled | `✓ Velra already enabled.` | 0 |
| `--dry-run` | a unified diff, then `(dry run: nothing was written)` | 0 |
| Claude Code settings directory missing | creates it; `! Claude Code not detected; hooks registered and will activate when it is installed.` | 0 |
| hooks unsupported by the detected version | `Skipped (needs a newer Claude Code, <version>): …` | 0 |
| `"disableAllHooks": true` in settings | `! "disableAllHooks": true is set, so no hooks run until you remove it.` | 0 |
| settings file unparseable, or not writable | `✗ <reason>`; nothing written | 1 |

Run it **with the binary you intend to keep**: the registered command is that
binary's absolute path, resolved to a stable `PATH` entry when the binary
lives in a versioned package-manager directory. Re-running it repairs a moved
binary.

## `velra disable`

```
Remove Velra's hooks from your Claude Code settings

Usage: velra disable [OPTIONS]

Options:
      --purge    Also delete ~/.velra (backups are kept)
      --yes      Skip the confirmation prompt for --purge
      --dry-run  Print the change as a diff without writing anything
  -h, --help     Print help
```

Removes only Velra's handlers. If the settings file had no `hooks` key before
`velra enable`, the key is removed too, so the file is byte-identical to its
pre-enable state. `--purge` deletes everything in `$VELRA_HOME` except
`backups/`, after a `[y/N]` prompt. On a non-terminal stdin the prompt answers
no unless `--yes` is given.

```
$ velra disable
✓ Velra disabled. Claude Code is otherwise untouched.
  Settings: /home/you/.claude/settings.json  (backup: /home/you/.velra/backups/settings.json.<timestamp>.bak)
```

Exit 0 on success, including "was not enabled; nothing to remove"; 1 if the
settings file cannot be read or written, or a purge partially fails.

## `velra status`

```
Show whether Velra is enabled and what it is tracking

Usage: velra status [OPTIONS]

Options:
      --json
  -h, --help  Print help
```

```
$ velra status
✓ Enabled (13 hook handlers registered)
  Claude Code: 2.1.280
  Binary:      /home/you/.velra/bin/velra
  Database:    /home/you/.velra/velra.db (244 KiB)
  Tracking:    3 session(s), 78 event(s)
  Last event:  1h ago
  Continuations: none live
  Last continuation: 87901fc6 CONFIRMED on session_start — written to Claude Code, and the session went on afterwards (not proof that a model read it)
  Staged:      objective, 1 failing test, 2 dead ends, 8 files (680 tokens) from session 87901fc6
               delivered on SessionStart(startup) — start a new session to pick it up
```

The `Staged:` lines appear only when a `velra restore` capsule is waiting for
the **current directory's** workspace. It is the one place a staged capsule
is visible without `inspect`. `Continuation:` lines list in-session capsules
(after `/compact`) that are `PENDING` or `ATTACHED`. `Last continuation:`
is the most recent one in any state, with what that state establishes:
`ATTACHED` means the capsule was written to Claude Code; `CONFIRMED` means
the session also went on afterwards (a tool call or a turn end). Neither is
proof that a model read it; Claude Code does not acknowledge hook output
(DECISIONS D113).

`Claude Code:` shows the detected version and how it was found: the `claude`
CLI, the VS Code extension, or `VELRA_CLAUDE_VERSION`. When none of those
works, Velra assumes the latest version it knows about.

`--json` fields: `enabled`, `handlers`, `expected_handlers`, `binary`,
`binary_exists`, `claude_code`, `db_path`, `db_bytes`, `sessions`, `events`,
`last_event_age`, `live_continuations`, `latest_continuation` (`session`,
`checkpoint`, `state`, `channel`, `attach_count`, `meaning`; `null` when there
is none), `database_error` (`null`, or why the database will not open),
`healthy`.

**Exit:** 0 when healthy (hooks registered, the recorded binary exists, and
the database opens at this schema), 1 otherwise. This makes it usable in scripts.

## `velra doctor`

```
Diagnose the installation

Usage: velra doctor [OPTIONS]

Options:
      --json
  -h, --help  Print help
```

Runs every check and prints one line each:

| Check | Fails (✗) when | Warns (!) when |
|---|---|---|
| settings parse | the file exists but does not parse, or its path cannot be determined | the file does not exist yet |
| binary | the recorded binary is missing | no binary recorded (`velra enable` never ran) |
| hook registrations | no Velra hooks registered | some expected registrations missing |
| `disableAllHooks` | — | it is `true` |
| Claude Code features | — | the version does not send `prompt_id` (delivery keys fall back to timestamps) |
| database | it will not open | journal mode is not WAL; > 5,000 unreduced events; no hook event ever recorded |
| spool | — | > 1,000 spooled events waiting |
| corrupt databases | — | rotated `*.corrupt-*` files exist in `$VELRA_HOME` |
| filesystem | — | `$VELRA_HOME` looks like a network filesystem (SQLite WAL is unreliable there) |
| errors log | — | recent lines in `logs/errors.log` (the last three are shown) |

```
$ velra doctor
✓ settings parse: /home/you/.claude/settings.json
✓ binary: /home/you/.velra/bin/velra
✓ 13 hook handlers registered across 10 events
✓ database: WAL, schema v2
✓ last hook event 1h ago
✓ no recent errors
```

`--json` prints `{"checks": [{"level": "ok|warn|fail", "message": …}], "healthy": bool}`.
**Exit:** 1 if any check failed, else 0. Warnings do not fail.

## `velra restore`

```
Carry a previous session's task state into your next one.

Pick a session this workspace has seen before; Velra stages its operational state so a brand-new
Claude Code session can pick up where it left off. Nothing is read from the old conversation.

Usage: velra restore [OPTIONS]

Options:
      --session <SESSION>  Restore this session id instead of showing the picker
      --list               List the sessions this workspace can restore from, and stop
      --dry-run            Print the capsule that would be staged without staging it
      --clear              Discard whatever is currently staged for this workspace
      --json
  -h, --help               Print help (see a summary with '-h')
```

Requires Velra **0.1.2 or later**. For the full workflow, what happens at each
step and how delivery works, see **[Restore](RESTORE.md)**. The reference
below covers flags and outputs.

### Forms

| Invocation | What it does |
|---|---|
| `velra restore` | lists up to 20 sessions of this workspace, newest first, and prompts `Select [1-N] (q to cancel):` |
| `velra restore --session <id>` | stages that session without a prompt |
| `velra restore --list` | prints the list and stops; stages nothing |
| `velra restore --dry-run [--session <id>]` | prints the capsule that would be staged; stages nothing |
| `velra restore --clear` | discards this workspace's staged capsule |
| `--json` with `--list`, or without `--session` | the list as JSON |
| `--json` with `--session` | the staging result as JSON |

### Picker

```
$ velra restore
Velra — Restore previous session

1. Continue the task and fix the bug.
   7fa9…e4ad · Last activity: 2026-09-23T12:33:02Z · state: yes · ~257 KB

2. Payments again. Run the suite; I want the reconcile failure this time...
   8790…e43d · Last activity: 2026-09-23T12:32:23Z · state: yes · ~544 KB

3. c974…819e
   c974…819e · Last activity: 2026-09-22T17:38:45Z · state: none · ~1 KB

Select [1-3] (q to cancel): 2
✓ Staged objective, 1 failing test, 2 dead ends, 8 files from session 87901fc6-65ee-4265-8c7c-513b5d8ae43d.
  680 estimated tokens · /home/you/.velra/staged/9c1256b5690e9531/capsule.01790166800000000000-4f1c9a0b2d7e6a5c3b18.json

  Start a new Claude Code session in /home/you/src/payments to pick it up.
```

Each row's label is the session's first prompt, taken from Claude Code's
transcript, or its id when there is none. `state: yes` means Velra's ledger
holds restorable task state for that session. `state: none` rows are shown
(so you can recognise the session) but cannot be staged. An empty answer,
`q`, or end-of-input cancels. An out-of-range or non-numeric answer never
selects anything: it re-prompts up to three times at a terminal, once
otherwise.

### JSON

```
$ velra restore --list --json
{
  "workspace_id": "9c1256b5690e9531",
  "workspace_root": "/home/you/src/payments",
  "sessions": [
    { "session_id": "7fa9ba41-0b8a-4375-89dd-3990625ae4ad", "title": "Continue the task and fix the bug.",
      "last_activity_ms": 1790166782016, "has_state": true },
    …
  ]
}

$ velra restore --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d --json
{
  "workspace_id": "9c1256b5690e9531",
  "source_session_id": "87901fc6-65ee-4265-8c7c-513b5d8ae43d",
  "source_checkpoint_id": null,
  "tokens": 680,
  "content_hash": "dfe66398088e6689f085bef1ef88b38f",
  "summary": "objective, 1 failing test, 2 dead ends, 8 files",
  "staged": true,
  "staged_path": "/home/you/.velra/staged/9c1256b5690e9531/capsule.01790166800000000000-4f1c9a0b2d7e6a5c3b18.json",
  "workspace_root": "/home/you/src/payments"
}
```

With `--dry-run --json` the same object has `"staged": false` and no
`staged_path` or `workspace_root`.

### Failure behaviour

| Output | Meaning | Exit |
|---|---|---|
| `! No Velra database yet at …` | Velra has never recorded a hook event on this machine | 1 |
| `! No previous sessions found for this workspace.` | no transcript and no ledger session for this workspace | 1 (0 with `--list`) |
| `! Velra has no task state for any of this workspace's N session(s) yet.` | sessions exist, none with state | 1 |
| `✗ no session <id> recorded for this workspace` | unknown id, or a session owned by another workspace; followed by a hint to use `--list` | 1 |
| `✗ session <id> has no task state to restore` | the session exists but recorded nothing restorable | 1 |
| `! Cancelled. Nothing was staged.` | picker declined | 1 |
| `✗ Could not stage the capsule: …` | the staging directory is not writable | 1 |
| `✓ Discarded the staged capsule.` / `✓ Nothing was staged.` | `--clear` | 0 |

Staging again replaces the previous capsule for the workspace atomically.
There is only ever one.

## `velra inspect`

```
Show what would survive a /compact right now

Usage: velra inspect [OPTIONS]

Options:
      --session <SESSION>        Session to inspect (defaults to this project's most recent)
      --last                     Use the most recently active session on this machine
      --checkpoint <CHECKPOINT>  Print a frozen capsule instead of a live preview
      --section <SECTION>        Full detail for one section: dead-ends, failure, files, attempts
      --trace <MARKER>           Trace a string (a test name, symbol, path) through every layer from
                                 the recorded events to the capsule, and report where it was lost.
                                 Repeatable
      --json
  -h, --help                     Print help
```

Catches the reducer up first, then renders a **live preview** of the capsule
for a session. That is the same text `velra restore --dry-run` would stage,
apart from redaction. `inspect` never creates a checkpoint and never stages
anything.

| Invocation | Output |
|---|---|
| `velra inspect` | preview for this workspace's most recent session (falls back to the machine's most recent) |
| `velra inspect --last` | preview for the most recently active session anywhere |
| `velra inspect --session <id>` | preview for that session |
| `velra inspect --section dead-ends\|failure\|files\|attempts` | untruncated detail behind one capsule section |
| `velra inspect --checkpoint <id> [--section …]` | a frozen `/compact` checkpoint, byte for byte |
| `velra inspect --trace <marker> [--trace …]` | where each marker was lost ([Debugging](TROUBLESHOOTING.md#tracing-a-lost-fact-velra-inspect---trace)) |
| `--json` | the preview (`snapshot_json`), the checkpoint, or the traces as JSON |

Example, `--section dead-ends`:

```
DEAD ENDS
---------
[1] src/payments/reconcile.py | inverse_edit at 18:00
  edit 2: Edit src/payments/reconcile.py +9/-7 REVERTED at 18:00
      + import datetime
  observed afterward: `cd "$(pwd)" && python -m pytest tests/test_reconcile.py -v 2>&1 | tail -30` FAIL. Causal link: UNCONFIRMED.
```

Example, `--trace`:

```
$ velra inspect --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d --trace feed.py
MARKER: feed.py
source_event:     PRESENT  [7 transcript line(s) in …/87901fc6-65ee-4265-8c7c-513b5d8ae43d.jsonl]
normalized_state: PRESENT  [3 event(s): events:3,events:15,events:16]
ledger:           PRESENT  [intents:3 (LATEST, epoch 1, superseded); file_stats:src/payments/feed.py]
snapshot:         PRESENT  [working_files]
renderer:         ABSENT  [22 ladder rung(s) applied; at full detail: PRESENT]
restore:          ABSENT  [the renderer's text after redaction, as `velra restore` stages it]
capsule:          N/A  [nothing staged for this workspace from this session]
first_loss:       renderer
reason:           budgeted out: removed by ladder rung `working_files 8->4` (target 740 estimated tokens)
```

**Exit:** 1 if there is no database, no session, an unknown section or
checkpoint, or the trace fails; otherwise 0. A marker being ABSENT is a
result, not an error.

## `velra --version`

```
$ velra --version
velra 0.1.2 (9c765610b, x86_64-pc-windows-gnu)
```

Version, the commit the binary was built from, and the target triple. Quote
it in bug reports.

## Internal entry points

Two more commands exist for Claude Code to invoke, never for you:
`velra hook <event>` (one per registered hook, reading the hook JSON on
stdin) and `velra reduce` (the async reducer). They bypass the CLI parser
entirely, **always exit 0**, never write to stderr, and write at most one
JSON object to stdout. They appear in your settings file after `velra enable`.
See [Architecture → fail-open hooks](ARCHITECTURE.md#fail-open-hooks).

---

[← README](../README.md) · [Restore](RESTORE.md) · [Configuration](CONFIGURATION.md) · [Troubleshooting](TROUBLESHOOTING.md)
