# CLI reference

Every user-facing command in Velra 0.1.2. The help text below is the
release binary's own `--help` output (`velra.exe` on Windows prints its file
name in `Usage:`). `velra <command> --help` on your machine is
authoritative. Example outputs are real output of the release binary;
long local paths are shortened to `~/…`.

```
$ velra --help
Local-first, deterministic continuation for Claude Code: clear the context, keep the state, continue working.

Usage: velra <COMMAND>

Commands:
  enable   Register Velra's hooks in your Claude Code settings
  disable  Remove Velra's hooks from your Claude Code settings
  status   Show whether Velra is enabled and what it is tracking
  inspect  Preview the capsule Velra would render for a session right now
  doctor   Diagnose the installation
  restore  Carry a previous session's task state into your next one
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

| Command | Use it to |
|---|---|
| [`velra enable`](#velra-enable) | register the hooks, once per machine |
| [`velra disable`](#velra-disable) | remove them; optionally purge Velra's data |
| [`velra status`](#velra-status) | check that Velra is on, what it tracks, and whether a capsule is staged here |
| [`velra doctor`](#velra-doctor) | diagnose a broken installation |
| [`velra restore`](#velra-restore) | carry a previous session's state into your next session |
| [`velra inspect`](#velra-inspect) | preview a capsule, drill into one section, or trace where a fact was lost |
| [`velra --version`](#velra---version) | print the version, commit and target |

**Conventions.** Human output goes to stdout, with `✓` for OK, `!` for a
warning and `✗` for a failure. Colour is off when stdout is not a terminal
or `NO_COLOR` is set. `--json` output is pretty-printed JSON on stdout in
ASCII (other characters as `\u` escapes, so a shell that decodes output
with its console code page reads the same values), and stdout carries
nothing else: a failure is `{"error": "<message>"}` with exit **1**, and a
warning that does not stop the command (`Could not catch up the reducer: …`)
goes to stderr. A usage error (an unknown flag) exits **2** with clap's
message on stderr. A reader that closes stdout early (`velra doctor | head -1`)
ends the output quietly; the command keeps its own exit status.

Commands that act on "this workspace" resolve it from the current
directory: `$CLAUDE_PROJECT_DIR` if set, otherwise the nearest ancestor
containing `.git`, otherwise the directory itself
([details](ARCHITECTURE.md#workspaces-and-sessions)). State lives in
`$VELRA_HOME` (default `~/.velra`).

---

## `velra enable`

```
Register Velra's hooks in your Claude Code settings

Usage: velra enable [OPTIONS]

Options:
      --dry-run  Print the change as a diff without writing anything
  -h, --help     Print help
```

Writes Velra's 13 hook handlers into the user-level Claude Code settings
file (`~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR/settings.json`; a
symlinked file is followed). It detects the Claude Code version first and
skips hooks that version does not support. It backs the file up to
`~/.velra/backups/settings.json.<timestamp>.bak` first (byte for byte; no
backup when there was no file), and preserves comments, key order,
formatting and a UTF-8 byte order mark. Velra handlers registered under
several binary paths are reduced to one. `--dry-run` writes nothing at all,
`$VELRA_HOME` included.

The file is replaced in one step (a temp file renamed over it), never
rewritten in place. Whether another program changed it since it was read is
checked immediately before the rename; if so, the edit is recomputed from
what is there, up to three times. When Windows refuses the replacement
because another program holds the file open, it is retried for up to a
second; if it still fails, or the file is read-only, the message names the
file and says it is unchanged.

```
$ velra enable
✓ Velra enabled for Claude Code.
  Settings: ~/.claude/settings.json  (backup: ~/.velra/backups/settings.json.<timestamp>.bak)

Nothing else required. Keep coding normally.
Tip: `velra restore` carries a session's state into your next one; `velra inspect` previews it.
```

| Situation | Output | Exit |
|---|---|---|
| already enabled | `✓ Velra already enabled.` | 0 |
| `--dry-run` | a unified diff, then `(dry run: nothing was written)` | 0 |
| settings file did not exist | creates it and says `Created …` | 0 |
| Claude Code settings directory missing | creates it; `! Claude Code not detected; hooks registered and will activate when it is installed.` | 0 |
| hooks unsupported by the detected version | `Skipped (needs a newer Claude Code, <version>): …` | 0 |
| `"disableAllHooks": true` in settings | `! "disableAllHooks": true is set, so no hooks run until you remove it.` | 0 |
| settings file unparseable, or not writable | `✗ <reason>`; nothing written | 1 |

Run it **with the binary you intend to keep**: the registered command is
that binary's absolute path, resolved to a stable `PATH` entry when the
binary lives in a versioned package-manager directory. Running it again
repairs a moved binary.

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

Removes only Velra's handlers. If the settings file had no `hooks` key
before `velra enable`, the key is removed too, so the file is byte-identical
to its pre-enable state. `--purge` deletes everything in `$VELRA_HOME`
except `backups/`, after a `[y/N]` prompt; on a non-terminal stdin the
prompt answers no unless `--yes` is given.

```
$ velra disable
✓ Velra disabled. Claude Code is otherwise untouched.
  Settings: ~/.claude/settings.json  (backup: ~/.velra/backups/settings.json.<timestamp>.bak)
```

Exit 0 on success, including "was not enabled; nothing to remove"; 1 if the
settings file cannot be read or written, or a purge partly fails.

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
  Binary:      ~/Videos/Velra/target/release/velra.exe
  Database:    ~/.velra/velra.db (232 KiB)
  Tracking:    3 session(s), 78 event(s)
  Last event:  7d ago
  Continuations: none live
  Staged:      objective, 1 failing test, 2 dead ends (711 tokens) from session 87901fc6
               delivered on SessionStart(startup) — start a new session in ~/src/payments to pick it up
```

- `Staged:` appears only when a `velra restore` capsule is waiting for the
  **current directory's** workspace. It is the one place a staged capsule
  is visible without opening the file.
- `Continuations:` lists in-session capsules (after `/compact`) that are
  `PENDING` or `ATTACHED`. `Last continuation:` shows the most recent one
  in any state, with what that state establishes: `ATTACHED` means the
  capsule was written to Claude Code; `CONFIRMED` means the session also
  went on afterwards. Neither proves a model read it (D113).
- `Claude Code:` shows the detected version and how it was found
  ([detection order](CONFIGURATION.md#claude-code-version-compatibility)).

`--json` fields: `enabled`, `handlers`, `expected_handlers`, `binary`,
`binary_exists`, `claude_code`, `db_path`, `db_bytes`, `sessions`, `events`,
`last_event_age`, `live_continuations`, `latest_continuation` (`null`, or
`session`, `checkpoint`, `state`, `channel`, `attach_count`, `meaning`),
`database_error` (`null`, or why the database will not open),
`workspace_root`, `staged`, `legacy_staged`, `healthy`. `staged` has a
`state` (`empty`, `staged`, `claimed`, `claimed_interrupted` or
`unreadable`), the `capsule`'s `source_session_id`, `tokens`, `summary` and
`deliver_on` where there is one, and a `meaning`. A claim is not a
delivery: `claimed` means a starting session took the capsule and it is not
yet known to have been received; `claimed_interrupted` means that session
start did not finish.

**Exit:** 0 when healthy (hooks registered, the recorded binary exists, the
database opens at this schema), 1 otherwise.

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
| database | it will not open | journal mode is not WAL; more than 5,000 unreduced events; no hook event ever recorded |
| spool | — | more than 1,000 spooled events waiting |
| corrupt databases | — | rotated `*.corrupt-*` files exist in `$VELRA_HOME` |
| filesystem | — | `$VELRA_HOME` looks like a network file system |
| errors log | — | recent lines in `logs/errors.log` (the last three are shown) |

```
$ velra doctor
✓ settings parse: ~/.claude/settings.json
✓ binary: ~/Videos/Velra/target/release/velra.exe
✓ 13 hook handlers registered across 10 events
✓ database: WAL, schema v3
✓ last hook event 7d ago
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
      --session <SESSION>
          Restore this session id instead of showing the picker

      --list
          List the sessions this workspace can restore from, and stop

      --dry-run
          Print the capsule that would be staged without staging it

      --clear
          Discard whatever is currently staged for this workspace

      --json


  -h, --help
          Print help (see a summary with '-h')
```

The full workflow and every guarantee: **[Restore](RESTORE.md)**. This
section covers flags and outputs.

### Forms

| Invocation | What it does |
|---|---|
| `velra restore` | lists up to 20 sessions of this workspace, newest first, and prompts `Select [1-N] (q to cancel):` |
| `velra restore --session <id>` | stages that session without a prompt; `<id>` is the full session id |
| `velra restore --list` | prints the list and stops; stages nothing |
| `velra restore --dry-run [--session <id>]` | prints the capsule that would be staged; stages nothing |
| `velra restore --clear` | discards this workspace's staged capsule |
| `--json` with `--list`, or without `--session` | the list as JSON |
| `--json` with `--session` | the staging result as JSON |
| `--json` with `--clear` | `{"cleared", "workspace_id", "workspace_root"}` |

### List and picker

```
$ velra restore --list
1. Continue the task and fix the bug.
   7fa9…e4ad · Last activity: 2026-09-23T12:33:01Z · state: yes

2. 5772…400d
   5772…400d · Last activity: 2026-09-23T12:32:23Z · state: none

3. Payments again. Run the suite; I want the reconcile failure this time...
   8790…e43d · Last activity: 2026-09-23T12:32:23Z · state: yes
```

Each row's label is the session's first prompt, or its id when there is
none. `state: yes` means Velra's ledger holds restorable task state for that
session; `state: none` rows are shown so you can recognise the session, but
cannot be staged. The shortened ids (`8790…e43d`) are for display: pass the
full id from `--list --json`. At the picker, an empty answer, `q` or
end-of-input cancels. An out-of-range or non-numeric answer never selects
anything: it re-prompts up to three times at a terminal, once otherwise.

### Staging

```
$ velra restore --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d
✓ Staged objective, 1 failing test, 2 dead ends from session 87901fc6-65ee-4265-8c7c-513b5d8ae43d.
  711 estimated tokens · ~/.velra/staged/9c1256b5690e9531/capsule.01790784305825447100-d684a785dcb9a16591c9.json

  Start a new Claude Code session in ~/src/payments to pick it up.
```

### JSON

```
$ velra restore --list --json
{
  "workspace_id": "9c1256b5690e9531",
  "workspace_root": "~/src/payments",
  "sessions": [
    {
      "session_id": "7fa9ba41-0b8a-4375-89dd-3990625ae4ad",
      "title": "Continue the task and fix the bug.",
      "last_activity_ms": 1790166781973,
      "has_state": true
    },
    …
  ]
}

$ velra restore --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d --json
{
  "workspace_id": "9c1256b5690e9531",
  "source_session_id": "87901fc6-65ee-4265-8c7c-513b5d8ae43d",
  "source_checkpoint_id": null,
  "tokens": 711,
  "content_hash": "d4810d1839162fb84c1cf61035066137",
  "summary": "objective, 1 failing test, 2 dead ends",
  "staged": true,
  "staged_path": "~/.velra/staged/9c1256b5690e9531/capsule.01790784349021899300-efbe19256c4ac0f41db3.json",
  "workspace_root": "~/src/payments"
}
```

With `--dry-run --json` the same object has `"staged": false` and no
`staged_path` or `workspace_root`.

### Failure behaviour

| Output | Meaning | Exit |
|---|---|---|
| `! No Velra database yet at …` | Velra has never recorded a hook event on this machine | 1 |
| `! No previous sessions found for this workspace (<root>).` | no transcript and no ledger session for this workspace | 1 (0 with `--list`) |
| `! Velra has no task state for any of this workspace's N session(s) yet.` | sessions exist, none with state | 1 |
| `✗ no session <id> recorded for this workspace` | unknown id, or a session owned by another workspace; followed by `Run \`velra restore --list\` to see this workspace's sessions.` | 1 |
| `✗ session <id> has no task state to restore` | the session exists but its current epoch recorded nothing restorable | 1 |
| `! Cancelled. Nothing was staged.` | the picker was declined | 1 |
| `✗ Could not stage the capsule: …` | the staging directory is not writable | 1 |
| `✓ Discarded the staged capsule for <root>.` / `✓ Nothing was staged for <root>.` | `--clear` | 0 |

With `--json`, each failure is `{"error": "<message>"}` on stdout, with the
same exit code. Staging again replaces the workspace's staged capsule; there
is only ever one live record.

## `velra inspect`

```
Preview the capsule Velra would render for a session right now

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

Catches the reducer up first, then renders a **live preview** of a
session's capsule. `inspect` never creates a checkpoint and never stages
anything.

| Invocation | Output |
|---|---|
| `velra inspect` | preview for this workspace's most recent session (falls back to the machine's most recent) |
| `velra inspect --last` | preview for the most recently active session anywhere |
| `velra inspect --session <id>` | preview for that session. `<id>` is a full id or a **unique prefix of 8 or more characters**, which is how a restored capsule names its source. Exit 1 if no recorded session matches, or the prefix matches more than one |
| `velra inspect --section dead-ends\|failure\|files\|attempts` | the untruncated detail behind one capsule section |
| `velra inspect --checkpoint <id> [--section …]` | a frozen `/compact` checkpoint, byte for byte |
| `velra inspect --trace <marker> [--trace …]` | where each marker was lost ([Troubleshooting](TROUBLESHOOTING.md#tracing-a-lost-fact-velra-inspect---trace)) |
| `--json` | the preview (`snapshot_json`), the checkpoint, the traces (a list, one object per marker), or `{"section", "detail"}` for `--section` |

Example, the session a restored capsule names, by its prefix:

```
$ velra inspect --session 87901fc6 --section dead-ends
DEAD ENDS
---------
[2] src/payments/reconcile.py | external at 18:01
  edit 1: Edit src/payments/reconcile.py +1/-1 REVERTED at 17:59
      - stamp = row["value_date"]
      + stamp = row["booking_date"]
  edit 3: Edit src/payments/reconcile.py +7/-9 REVERTED at 18:00
      - import datetime
  observed afterward: `cd "$(pwd)" && python -m pytest tests/test_reconcile.py -v 2>&1 | tail -30` PASS. Causal link: UNCONFIRMED.

[1] src/payments/reconcile.py | inverse_edit at 18:00
  edit 2: Edit src/payments/reconcile.py +9/-7 REVERTED at 18:00
      + import datetime
  observed afterward: `cd "$(pwd)" && python -m pytest tests/test_reconcile.py -v 2>&1 | tail -30` FAIL. Causal link: UNCONFIRMED.
```

**Exit:** 1 if there is no database, no matching session, an unknown
section or checkpoint, or the trace fails; otherwise 0. A marker being
`ABSENT` is a result, not an error.

## `velra --version`

```
$ velra --version          # a build of 77328b0 on a Windows GNU host
velra 0.1.2 (77328b098, x86_64-pc-windows-gnu)
```

The version, the commit the binary was built from (9 characters of
`HEAD`), and the target triple. A build from a tree with uncommitted changes
still reports `HEAD`. Quote this line in bug reports.

## Internal entry points

Two more commands exist for Claude Code to invoke, never for you:
`velra hook <event>` (one per registered hook, reading the hook JSON on
stdin) and `velra reduce` (the async reducer). They bypass the CLI parser,
**always exit 0**, never write to stderr, and write at most one JSON object
to stdout. They appear in your settings file after `velra enable`. See
[Architecture → Fail-open hooks](ARCHITECTURE.md#fail-open-hooks).

---

[← README](../README.md) · [Restore](RESTORE.md) · [Configuration](CONFIGURATION.md) · [Troubleshooting](TROUBLESHOOTING.md)
