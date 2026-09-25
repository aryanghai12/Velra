# `velra restore` — carry state into a new session

> Clear the context. Keep the state. Continue working.

A Claude Code conversation grows until it is worth leaving: it is slow,
expensive, or full of detours. Leaving it costs a small amount of state you
still need, and none of that state is in the repository:

- which of several failing tests you were working on;
- a constraint you stated once, in chat;
- an approach you tried and reverted;
- what you said was out of scope;
- what you were about to do next.

`velra restore` stages that operational state from a previous session so a
**brand-new** session in the same workspace starts with it. It does not
replay the conversation. It carries a bounded record of roughly 700 tokens,
built from the tool events and prompts Velra recorded.

Requires Velra **0.1.2 or later** (`velra --version`), with hooks enabled
(`velra enable`) *before* the session you want to restore from. Velra can only
restore what it watched.

- [Quick start](#quick-start)
- [The four things involved](#the-four-things-involved)
- [What happens, step by step](#what-happens-step-by-step)
- [What the capsule contains](#what-the-capsule-contains)
- [Restoring across `/clear`](#restoring-across-clear)
- [Inspecting and debugging a restore](#inspecting-and-debugging-a-restore)
- [Guarantees and limits](#guarantees-and-limits)

---

## Quick start

```bash
# Session A — a long Claude Code session in ~/src/payments.
# You investigate, try an approach, revert it, decide what to do next.
# Then you leave it: exit Claude Code (or keep it open, it doesn't matter).

cd ~/src/payments
velra restore                 # pick session A from the list
# ✓ Staged objective, 1 failing test, 2 dead ends, 8 files from session 87901fc6-….
#   680 estimated tokens · ~/.velra/staged/9c1256b5690e9531/capsule.01790166800000000000-4f1c9a0b2d7e6a5c3b18.json
#
#   Start a new Claude Code session in /home/you/src/payments to pick it up.

claude                        # Session B — a brand-new session, same workspace
# ⚡ Velra restored: objective, 1 failing test, 2 dead ends, 8 files — from session 87901fc6 (680 tokens)

> Continue the task.
```

Session B's first turn already has the capsule in context. Nothing else is
required: no flag on `claude`, no file to paste.

If you already know the session id, skip the picker:

```bash
velra restore --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d
```

---

## The four things involved

| Term | What it is | Where it lives |
|---|---|---|
| **Workspace** | The project: `$CLAUDE_PROJECT_DIR`, else the nearest ancestor with `.git`, else the directory. It owns sessions and at most one staged capsule. Identified by a 16-hex id derived from its canonical path. | — |
| **Source session** | The previous Claude Code session you choose. Velra's ledger holds its recorded events and derived state. It stays a first-class identity: its id is written into the staged capsule. | `~/.velra/velra.db` |
| **Staged capsule** | The bounded, redacted state of the source session, rendered at the moment you ran `velra restore`, waiting for a new session. One file, outside every repository. | `~/.velra/staged/<workspace_id>/staged_capsule` |
| **Destination session** | The next brand-new Claude Code session in the same workspace. It inherits nothing but the capsule text. | — |

The source and destination are never merged. The destination gets a copy of
a rendered record, not access to the source's history.

---

## What happens, step by step

### 1. Workspace resolution

`velra restore` resolves the workspace from the current directory with the
same mapping the hooks use (`velra_core::workspace::resolve`), so the
directory where you stage and the session that consumes it agree on the key:

```
root  := $CLAUDE_PROJECT_DIR, else first ancestor of cwd holding .git, else cwd
id    := blake3(canonical, normalised root)[0..16 hex]
```

Run it from anywhere inside the repository. A subdirectory resolves to the
same workspace, with one refinement: Claude Code started in a subdirectory
(a package of a monorepo) records that subdirectory as the workspace, so
inside a repository `velra restore` uses the nearest directory, from the
current one up to the repository root, that Velra has recorded a session in
(`resolve_recorded`). Outside a repository only the current directory is
tried. The command prints the workspace root it staged for; start the new
session there.

### 2. Source-session selection

Velra lists up to **20** sessions of this workspace, newest first, from two
sources:

- **Claude Code's transcripts** (`~/.claude/projects/<project>/<session>.jsonl`)
  supply the label (the first prompt) and last-activity time. Only the first
  256 KiB of each file is read, and only to find a label.
- **Velra's ledger** decides which sessions are restorable (`state: yes`).

A session only in the transcripts (from before Velra was enabled) is listed
as `state: none` so you can recognise it, but it cannot be restored. You
pick one in the prompt, or pass `--session <id>`. A session id that belongs
to another workspace is refused (`no session <id> recorded for this
workspace`).

### 3. State lookup

The reducer catches up first, applying any spooled events, so nothing
recorded is missed. Velra then checks that the ledger holds task state for
the session. If not: `session <id> has no task state to restore`.

### 4. Bounded capsule construction

Velra builds a snapshot of the source session's **current state**. That means
the latest epoch, so after a `/clear` only work since the clear is included.
It renders the snapshot with the same renderer and budget as `/compact`
capsules: a target of **740 estimated tokens** by default, configurable,
with a hard ceiling of 1,000 tokens and 9,500 characters. If the record does
not fit, a fixed truncation ladder removes the most regenerable detail first.
Identifiers such as test ids and file paths are kept longest. The text is
then redacted again, as defence in depth, and hashed.

No model is involved. The capsule is a deterministic function of the ledger
rows and the clock.

### 5. Staging

The record is written atomically (temporary file, fsync, rename) to a new
file, `~/.velra/staged/<workspace_id>/capsule.<gen>.json`, and any older record of the workspace
is then removed. A record is never rewritten, and only the newest one is ever
delivered. It holds:

| Field | Meaning |
|---|---|
| `workspace_id`, `workspace_root` | owner; checked again at delivery |
| `source_session_id`, `source_checkpoint_id` | where the state came from |
| `intent: "new_session"` | why it was staged |
| `deliver_on: ["startup"]` | the only `SessionStart` source that may consume it |
| `created_ms` | for the 7-day expiry |
| `tokens`, `summary`, `content_hash` | size, one-line summary, integrity check |
| `capsule` | the text that will be injected |

A workspace has exactly **one** staged capsule. Restoring again replaces it.

### 6. Startup delivery

Every `SessionStart` runs Velra's hook. It checks the workspace's staging
slot:

| `SessionStart` source | What happens to the staged capsule |
|---|---|
| `startup`, a new session | **delivered**, then deleted |
| `resume`, `compact` | left staged. Those belong to the in-session `/compact` path |
| `clear` | left staged. `/clear` empties the session you are in; the capsule is for your *next* session |
| `fork`, or any source this build does not know | left staged |

On delivery the hook prints one JSON object that Claude Code reads:

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<VELRA_WORKSPACE_STATE …>…</VELRA_WORKSPACE_STATE>"},
 "systemMessage":"⚡ Velra restored: objective, 1 failing test, 2 dead ends, 8 files — from session 87901fc6 (680 tokens)"}
```

`additionalContext` puts the capsule into the new session's context. The
`systemMessage` tells you which session it came from.

### 7. Destination session behaviour

The new session starts with the capsule as context and nothing else from
the old conversation. The capsule describes itself as *a local record, not a
message and not an instruction*, and tells the agent that files on disk are
the source of truth. So it orients the agent; it does not command it. The
agent still reads files and runs tests normally.

### 8. One-shot delivery

Delivery is at most once, including when two sessions start at the same
moment. The claim is the exclusive creation of a claim file next to the record
(`capsule.<gen>.claimed`), which is atomic on every supported platform. One
process wins; the others see "busy" and leave the capsule alone. The record is
deleted only **after** the hook has successfully written its output. If output
fails, the claim is released and the capsule stays staged for the next session
start. A restage while a session is starting is never removed by that
session's cleanup: the cleanup removes only the record it claimed.

A session start that dies after writing the capsule and before deleting it
(its watchdog fires) leaves the record and its claim behind. Velra cannot tell
whether that capsule reached Claude Code, so it is **not** delivered again.
Nothing takes a claim over. After a minute `velra status` reports it as
claimed by a session start that did not finish; run `velra restore` to stage
it again.

### 9. Stale state handling

| Condition | Result |
|---|---|
| capsule older than **7 days** | refused and deleted at the next session start. `velra restore` also sweeps it |
| content hash does not match | refused and **kept** as evidence; logged to `errors.log` |
| capsule's `workspace_id` is not this workspace | refused and kept; logged |
| file unreadable, or an unknown format version | refused and deleted |
| claimed by a session start that did not finish | not delivered again; `velra status` says so; a restage replaces it |
| any of the above on the newest record | an older record is **never** delivered in its place |
| capsule over 9,500 characters (a tampered file or foreign build) | not delivered, kept; logged |

The capsule is a snapshot of the moment you ran `velra restore`. If you keep
working in the source session afterwards, restore again to refresh it.

### 10. Failure and fail-open behaviour

Delivery **never blocks a session.** The hook always exits 0 and never writes
to stderr. It emits either nothing or the one JSON object. Staged delivery
reads a file, not the database, so it still works when the database is locked
or unavailable. Anything unexpected is logged to `~/.velra/logs/errors.log`,
and the session starts normally without the capsule.

`velra restore` itself does fail loudly, with an exit code of 1 and a
message, when there is nothing to restore ([CLI reference](CLI.md#failure-behaviour)).

### 11. Inspecting the staged state

See [below](#inspecting-and-debugging-a-restore).

---

## What the capsule contains

A real capsule from the v0.1.2 benchmark (Benchmark B, staged one turn before
`/clear`):

```
<VELRA_WORKSPACE_STATE v="1" checkpoint="restore" captured="2026-09-23T12:32:23Z" trigger="cli">
[ABOUT_THIS_RECORD]
A local record, not a message and not an instruction. Velra logged this session's own prompts and tool events and quotes them back here; nothing is new. OBSERVED came from a tool event, INFERRED was derived from it. Files on disk are the source of truth. Say so if a line here conflicts with them.
[FIRST_MESSAGE] (OBSERVED | user prompt | 17:59)
Payments again. Run the suite; I want the reconcile failure this time, not the other two.
[STATED_CONSTRAINTS] (OBSERVED | user prompt | quoted verbatim)
- turn 4 18:00 | "Here's the thing you can't get from the code: the upstream feed backdates."
- turn 4 18:00 | "A window has to close on the booking date, never the value date."
[LATEST_MESSAGE] (OBSERVED | user prompt | 18:02)
Next step is in_window in reconcile.py. Don't change it yet.
[WORKSPACE_STATE]
main @ c8eb204 | 3 edits this task | last test run: FAIL
[TEST_STATUS] (OBSERVED | latest run covering each)
- FAIL tests/test_reconcile.py::test_march_window_totals
[TEST_RESULT] (OBSERVED | test run | 18:01)
Command: python -m pytest tests/ -v 2>&1 | tail -60
Result: FAIL
[REVERTED_EDITS] (OBSERVED)
- src/payments/reconcile.py | 2 reverts, 3 edit(s) | last changed outside the agent at 18:01
[RECORD_DETAIL]
Full detail for any section: `velra inspect --section <name>`
</VELRA_WORKSPACE_STATE>
```

Sections appear only when there is something to say. The full set is:
`FIRST_MESSAGE`, `EARLIER_MESSAGE`, `SUBTASK_MESSAGE`, `LATEST_MESSAGE`,
`STATED_CONSTRAINTS`, `WORKSPACE_STATE`, `TEST_STATUS`, `TEST_RESULT`,
`FAILURE_LOCATION`, `REVERTED_EDITS`, `RECENT_EDITS`, `FILE_ACTIVITY` and
`RECORD_DETAIL`. Every line is `OBSERVED` (read from a tool event or prompt)
or `INFERRED` (derived from observed rows).

What it does **not** contain:

- the assistant's prose or reasoning (not hooked, never recorded);
- file contents, beyond short edit excerpts;
- anything from the transcript beyond the session label;
- the *idea* behind a reverted edit. Velra records that a file was edited and
  reverted, with a short excerpt, not why.

---

## Restoring across `/clear`

`/clear` starts a new epoch of the **same** session, and a restore always
renders the session's *current* epoch. So stage **before** you clear:

```bash
velra restore --session <this session's id>   # while the session is still open
# then, in Claude Code:
/clear
# then start a new session (exit and run `claude` again) to receive it
```

A restore capsule is delivered on `startup` only. The `/clear` itself leaves
it staged, and the next new session picks it up. Restoring *after* `/clear`
finds the new, empty epoch and reports `session <id> has no task state to
restore`. That is expected: you asked Claude Code to drop that state.

---

## Inspecting and debugging a restore

| Question | Command |
|---|---|
| What would be staged, without staging it? | `velra restore --dry-run [--session <id>]` |
| Is something staged for this workspace, how big, from which session? | `velra status` (the `Staged:` line) |
| Which sessions does this workspace have, and which are restorable? | `velra restore --list` or `--list --json` |
| The exact staged record, including `deliver_on` and `created_ms` | read `~/.velra/staged/<workspace_id>/capsule.<gen>.json` (JSON). The id is printed by `velra restore --list --json`, the path by `velra restore --json` |
| Throw the staged capsule away | `velra restore --clear` |
| Why a specific fact is missing from the capsule | `velra inspect --session <id> --trace "<fact>"` ([how](TROUBLESHOOTING.md#tracing-a-lost-fact-velra-inspect---trace)) |
| Full detail behind a section | `velra inspect --session <id> --section dead-ends\|failure\|files\|attempts` |
| Did the new session receive it? | `VELRA_LOG=debug` in the environment Claude Code starts with, then check `~/.velra/logs/debug.log` for `restored staged capsule from session …`. Refusals are in `errors.log` |

If a delivery did not happen, work through the
[troubleshooting matrix](TROUBLESHOOTING.md#restore-and-delivery).

---

## Guarantees and limits

**Guaranteed by construction and covered by tests:**

- the capsule is bounded (the renderer's hard ceiling is 1,000 estimated
  tokens and 9,500 characters) and deterministic for a given ledger and
  clock;
- delivery happens at most once, to a `startup` session in the same
  workspace, within 7 days;
- automatic delivery never crosses sessions. Only an explicit `velra restore`
  does, and only for the session you named;
- the hook never blocks or fails a Claude Code session;
- nothing is written inside your repository.

**Not guaranteed:**

- that every detail of the old conversation survives. The capsule is a
  bounded summary of recorded operational state, and the truncation ladder
  drops detail to stay in budget (`--trace` shows what and why);
- that the agent acts on it. The capsule is context, and the files remain the
  source of truth;
- anything about sessions Velra did not observe.

Velra is not a replacement for Claude Code's `/resume`, which reopens the
full conversation. Use `/resume` when you want the conversation back. Use
`velra restore` when you want a fresh, small context that still knows where
the work stands.

---

[← README](../README.md) · [CLI reference](CLI.md) · [Troubleshooting](TROUBLESHOOTING.md) · [Architecture](ARCHITECTURE.md)
