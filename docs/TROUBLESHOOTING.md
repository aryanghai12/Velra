# Troubleshooting and debugging

Start with these three. They are read-only and change nothing:

```bash
velra --version     # restore needs 0.1.2+
velra doctor        # installation health; exits 1 on a failed check
velra status        # enabled? what is tracked? is a capsule staged for this workspace?
```

Velra's hooks **fail open**: they always exit 0, never write to stderr, and
never block Claude Code. The cost of that design is that a problem shows up
as Velra *not doing something*, never as an error in Claude Code. The
evidence is in `~/.velra/logs/errors.log` (always on), and in
`~/.velra/logs/debug.log` when Claude Code runs with `VELRA_LOG=debug`.

Prefer inspection to deletion. Nothing on this page asks you to delete the
database. To throw away a staged capsule, use `velra restore --clear`.

- [Installation](#installation)
- [Restore and delivery](#restore-and-delivery)
- [Capsule content](#capsule-content)
- [Hooks](#hooks)
- [Tracing a lost fact: `velra inspect --trace`](#tracing-a-lost-fact-velra-inspect---trace)
- [Capture vs staging vs delivery: a decision guide](#capture-vs-staging-vs-delivery-a-decision-guide)
- [Reporting a bug](#reporting-a-bug)

---

## Installation

| Symptom | Likely cause | Diagnose | Expected | Fix |
|---|---|---|---|---|
| `velra: command not found` / `'velra' is not recognized` | install directory not on `PATH` yet, or the terminal predates the install | `ls ~/.velra/bin` (Windows: `dir $env:USERPROFILE\.velra\bin`) | `velra` / `velra.exe` present | open a new terminal. On Windows the installer edits the **user** `PATH`; elsewhere check the `# added by velra` line in your rc file, or add `~/.velra/bin` yourself |
| `error: unrecognized subcommand 'restore'` | Velra 0.1.1 or older | `velra --version` | `velra 0.1.2 (…)` or later | install 0.1.2 ([Install → Which version you get](INSTALL.md#which-version-you-get)); until it is published, build from source |
| Hooks run an old or missing binary after an upgrade or move | the settings file holds the path `velra enable` was run with | `velra doctor` | `✓ binary: <path>` | run `velra enable` again with the binary you want to keep |
| `✗ settings parse failed at line L, column C` | `~/.claude/settings.json` is not valid JSON/JSONC | `velra doctor` | `✓ settings parse: …` | fix the file (Claude Code will not read it either). Velra refuses to edit a file it cannot parse |
| `Could not create …` / permission denied during `enable` or `restore` | `$VELRA_HOME` or the settings directory is not writable | `ls -ld ~/.velra ~/.claude` | owned by you, writable | fix ownership/permissions, or point `VELRA_HOME` at a writable directory |
| `! … looks like a network filesystem` | `$VELRA_HOME` on NFS/SMB | `velra doctor` | no warning | set `VELRA_HOME` to a local disk; SQLite WAL is unreliable on network filesystems |
| Build from source fails with `gcc.exe: program not found` / `dlltool.exe` (Windows GNU) or `link.exe not found` (MSVC) | no C toolchain for the bundled SQLite | `cargo build --release -p velra` | builds | install the toolchain listed in [Install → Build from source](INSTALL.md#build-from-source) |

## Restore and delivery

| Symptom | Likely cause | Diagnose | Expected | Fix |
|---|---|---|---|---|
| `! No Velra database yet at …` | hooks have never run on this machine | `velra doctor` | `✓ N hook handlers registered…`, `✓ last hook event …` | `velra enable`, then use Claude Code once. Velra only restores sessions it watched |
| `! No previous sessions found for this workspace.` | you are in a different workspace from the one the session ran in (another directory, a different git root, or a `CLAUDE_PROJECT_DIR` that differs) | `velra restore --list --json` and compare `workspace_root` with where the session ran | the project root you expect | `cd` into that project. If Claude Code ran with `CLAUDE_PROJECT_DIR` set, set the same value before `velra restore` |
| `! Velra has no task state for any of this workspace's N session(s) yet.` | the sessions predate `velra enable`, or recorded nothing | `velra restore --list` | some rows `state: yes` | work one session with Velra enabled first |
| `✗ session <id> has no task state to restore` | the session was `/clear`ed after its work: a restore renders the *current* epoch, which is empty | `velra inspect --session <id>` | a non-empty capsule | stage **before** `/clear` ([Restoring across `/clear`](RESTORE.md#restoring-across-clear)); or pick another session |
| `✗ no session <id> recorded for this workspace` | typo, an abbreviated id (`8790…e43d` is display only), or a session from another workspace | `velra restore --list --json` | the full id in `session_id` | pass the full id, from the right directory |
| **Restore succeeded but the new session did not receive the state** | (1) the next session was not a new one: `--resume`, `--continue` and `/clear` never consume a restore capsule; (2) it started in a different workspace; (3) hooks did not run in that session; (4) the capsule expired (7 days) | `velra status` in the project | if the `Staged:` line is **still there**, nothing consumed it: causes 1–3. If it is gone and nothing arrived: check `errors.log` for `staged capsule not delivered: …` | start a plain new session (`claude`) from inside the project; for (3) see [Hooks](#hooks) |
| The picked session is the wrong one / an old one got restored | the picker lists newest first by last activity, and labels are first prompts, which can repeat | `velra restore --list` (full ids via `--json`); `velra restore --dry-run --session <id>` to preview | the preview shows the work you expect | `velra restore --session <correct id>`. It replaces the staged capsule |
| Several sessions look identical | the same opening prompt used more than once | `velra restore --list --json` (`last_activity_ms`), then `--dry-run --session <id>` for each | — | choose by preview, not label |
| The restored state is stale | the capsule is a snapshot from when you ran `velra restore`, and you worked on afterwards | `velra status` shows its session; `velra restore --dry-run --session <id>` shows the current render | — | run `velra restore` again, or `velra restore --clear` |
| A capsule arrived in a session where you did not want it | it was staged for this workspace and this was the next new session | `debug.log` line `restored staged capsule from session …` | — | `velra restore --clear` before starting sessions you want clean |
| `staged capsule failed its content hash` / `belongs to workspace …` in `errors.log` | the staged file was edited, or copied between machines or workspaces | `cat ~/.velra/staged/<workspace_id>/capsule.<gen>.json` | — | `velra restore --clear` (the file was kept as evidence), then restage |
| `staged capsule is N days old` in `errors.log` | older than 7 days; discarded | — | — | restage from the session you want |
| `velra status` says the staged capsule `was claimed by a session start that did not finish` | a session start was ended (by its watchdog, or killed) after claiming the capsule; it may already have delivered it, so it is not delivered again | `velra status` | — | `velra restore` to stage it again |
| `velra restore` finds no sessions, or the new session receives nothing, in a subdirectory | Claude Code's workspace is the directory it was started in; the terminal resolved another | `velra restore --list --json` (`workspace_root`) | the root printed is where Claude Code was started | run `velra restore` from that directory, and start the new session there |
| State appears to be delivered twice | two different mechanisms: a restore capsule (new sessions) and an in-session continuation (after `/compact` in the *same* session). Each delivers once at most | `debug.log` around the session start | at most one `restored staged capsule` per staging | if one staging really delivered twice, that is a bug; [report it](#reporting-a-bug) with `debug.log` |

## Capsule content

| Symptom | Likely cause | Diagnose | Fix |
|---|---|---|---|
| A fact you expected (test name, file, symbol, constraint) is missing | not captured (assistant prose is never hooked), superseded by a later prompt, or cut by the token budget | `velra inspect --session <id> --trace "<fact>"` | depends on `first_loss`; see [below](#tracing-a-lost-fact-velra-inspect---trace) |
| A section is shorter than you want | the truncation ladder trimmed it to stay within the budget | `velra inspect --session <id> --section dead-ends\|failure\|files\|attempts` | read the full detail there; or raise `budget_tokens` ([Configuration](CONFIGURATION.md#configtoml)) |
| The capsule seems too large | a raised `budget_tokens` | `velra restore --dry-run --json` (`tokens`) | lower `budget_tokens`. The renderer's hard ceiling (1,000 tokens / 9,500 characters) holds whatever you set |
| `staged capsule is N chars, above the 9500 char margin…` in `errors.log` | a staged file not produced by this build (hand-edited or foreign) | `velra restore --dry-run` | restage with `velra restore` |

## Hooks

| Symptom | Likely cause | Diagnose | Expected | Fix |
|---|---|---|---|---|
| `velra status` says `Not enabled` | hooks not registered in the settings file Claude Code reads | `velra doctor`; check `CLAUDE_CONFIG_DIR` | handlers registered | `velra enable` (with the same `CLAUDE_CONFIG_DIR` Claude Code uses) |
| `SessionStart` (or any hook) never fires | `"disableAllHooks": true`, a managed policy disabling user hooks, `VELRA_DISABLE=1`, or `~/.velra/disabled` | `velra doctor` warns about `disableAllHooks` and `no hook events recorded yet`; `ls ~/.velra/disabled` | `✓ last hook event …` recent | remove the blocker |
| `Skipped (needs a newer Claude Code…)` on `enable` | Claude Code older than the hook's minimum version | `velra status` (`Claude Code:` line) | ≥ 1.0.62 for `SessionStart` | upgrade Claude Code, then `velra enable` again |
| Wrong Claude Code version detected (`claude` not on `PATH`, extension only) | detection falls back to the VS Code extension, then to the newest version this build knows | `velra status` | your actual version | set `VELRA_CLAUDE_VERSION=<x.y.z>` and re-run `velra enable` |
| "Did the hook fail?" | hooks always exit 0 by design; failure shows only in logs | `velra doctor` (last three errors), `tail ~/.velra/logs/errors.log` | `✓ no recent errors` | act on the logged error, or report it |
| Velra is silent: no `⚡ Velra …` message | normal when nothing is staged, or when there is no `/compact` continuation. Whether `systemMessage` text is shown also depends on the Claude Code surface | `VELRA_LOG=debug claude`, then `~/.velra/logs/debug.log` | per-hook lines | none needed if the debug log shows the hooks running |
| `N events not reduced yet` / large spool backlog | the async reducer is behind (it is capped per run) | `velra doctor` | no warning | any `velra inspect`/`restore` catches the reducer up |

### Checking `SessionStart` delivery without Claude Code

You can drive the hook directly to see whether a staged capsule is eligible
and delivers. **This consumes the capsule**, so restage afterwards.

```bash
cd ~/src/payments                     # the workspace
velra status                          # confirm a "Staged:" line
printf '{"session_id":"test","hook_event_name":"SessionStart","source":"startup","cwd":"%s"}' "$PWD" \
  | velra hook session-start; echo " exit=$?"
# → {"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<VELRA_WORKSPACE_STATE …"}, "systemMessage":"⚡ Velra restored: …"} exit=0
```

With `"source":"clear"` or `"resume"` it prints nothing, exits 0, and leaves
the capsule staged. That is the designed behaviour.

---

## Tracing a lost fact: `velra inspect --trace`

`--trace <MARKER>` follows any string, such as a test id, symbol, path or
phrase you typed, through every layer a capsule is built from. It reports
`PRESENT`, `ABSENT` or `N/A` at each:

| Layer | What it checks | If the marker is first lost here, it means |
|---|---|---|
| `source_event` | the session's Claude Code transcript on disk | it never appeared in the session at all |
| `normalized_state` | the hook payloads Velra stored (`events`) | **not captured**: it was only in assistant prose or reasoning, which Velra does not hook |
| `ledger` | the reducer's projections (intents, commands, files, tests, constraints) | captured, but no projection kept it |
| `snapshot` | what `snapshot::build` selected for the capsule | **selected out**, for example a prompt superseded by a later one |
| `renderer` | `render::render` under the configured budget | **budgeted out**: the report names the ladder rung that removed it |
| `restore` | the rendered text after redaction, as `velra restore` stages it | redacted (it matched a secret pattern) |
| `capsule` | the capsule currently staged for this workspace from this session | the staged file is older or different: **restage** |

```
$ velra inspect --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d --trace in_window --trace retry_backoff --trace feed.py
MARKER: in_window
source_event:     PRESENT  [18 transcript line(s) in …/87901fc6-….jsonl]
normalized_state: PRESENT  [2 event(s): events:39,events:64]
ledger:           PRESENT  [intents:16 (LATEST, epoch 1)]
snapshot:         PRESENT  [latest (intents:16)]
renderer:         PRESENT  [22 ladder rung(s) applied; at full detail: PRESENT]
restore:          PRESENT  [the renderer's text after redaction, as `velra restore` stages it]
capsule:          N/A  [nothing staged for this workspace from this session]
first_loss:       none

MARKER: retry_backoff
source_event:     PRESENT  [1 transcript line(s) in …/87901fc6-….jsonl]
normalized_state: ABSENT  [0 event(s)]
…
first_loss:       normalized_state
reason:           not captured: no hook payload Velra stored contains it (assistant prose and reasoning are not hooked)

MARKER: feed.py
…
snapshot:         PRESENT  [working_files]
renderer:         ABSENT  [22 ladder rung(s) applied; at full detail: PRESENT]
first_loss:       renderer
reason:           budgeted out: removed by ladder rung `working_files 8->4` (target 740 estimated tokens)
```

How to read that:

- `in_window` made it all the way. `capsule: N/A` only means nothing is
  staged right now from this session.
- `retry_backoff` appears in the transcript but only in the assistant's
  words, so Velra never captured it. The remedy is for it to appear in a
  prompt or a tool call. No setting changes this.
- `feed.py` was selected but cut at render time by a named rung. A larger
  `budget_tokens` would keep it, and `--section files` shows it now.

Reasons are computed from the rows and the ladder, never guessed. When no
specific reason applies, the report says `other`. Use `--json` for a
machine-readable form (one object per marker, with the evidence per layer).

## Capture vs staging vs delivery: a decision guide

```
Is the fact in the capsule you expected?
│
├─ velra restore --dry-run --session <id>  shows it?
│    ├─ no  → CAPTURE / SELECTION / BUDGET problem → velra inspect --trace "<fact>"
│    └─ yes → was it staged?
│          ├─ velra status shows "Staged: … from session <id>"?
│          │    ├─ yes, still there after starting a session → DELIVERY never happened
│          │    │       (not a startup session / wrong workspace / hooks off)
│          │    └─ gone → it was claimed; check debug.log / errors.log for which
│          └─ never staged → re-run `velra restore`; read its exit code and message
└─ the new session received it but ignored it → the capsule is context, not an
   instruction; files on disk are authoritative (see RESTORE.md → Guarantees)
```

And for source-session selection specifically:

```bash
velra restore --list --json          # every candidate with full id, time, has_state
velra restore --dry-run --session <id>   # exactly what that one would stage
velra inspect --session <id> --section attempts   # what that session actually did
```

---

## Reporting a bug

Include:

- `velra --version`
- `velra doctor --json`
- `velra status --json`
- the relevant lines of `~/.velra/logs/errors.log`, and of `debug.log` if you
  can reproduce with `VELRA_LOG=debug`
- for a content problem: `velra inspect --session <id> --trace "<fact>" --json`

Review what you paste. Capsules and logs quote your prompts and file paths
(secrets are redacted by pattern, not by guarantee). Any Velra hook that exits
non-zero or writes to stderr is a top-severity bug. Security issues go
through a private advisory ([SECURITY.md](../SECURITY.md)).

---

[← README](../README.md) · [Restore](RESTORE.md) · [Configuration](CONFIGURATION.md) · [Architecture](ARCHITECTURE.md)
