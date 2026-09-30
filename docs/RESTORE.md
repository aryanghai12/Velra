# `velra restore`

`velra restore` carries the operational state of a session you choose into
the next **new** Claude Code session in the same workspace. It does not
replay the conversation. It stages a bounded capsule, rendered from what
Velra recorded in the source session, and the destination session receives
it once at startup.

Requires Velra **0.1.2 or later** (`velra --version`), with hooks enabled
(`velra enable`) *before* the session you want to restore from. Velra can
only restore what it watched.

- [Quick start](#quick-start)
- [The four things involved](#the-four-things-involved)
- [What happens, step by step](#what-happens-step-by-step)
- [What the capsule contains](#what-the-capsule-contains)
- [Restoring across `/clear`](#restoring-across-clear)
- [Inspecting and debugging](#inspecting-and-debugging)
- [Guarantees and limits](#guarantees-and-limits)

---

## Quick start

![Session A records into the ledger; velra restore renders it and stages a capsule for the workspace; Session B, a new session in the same workspace, claims it once at SessionStart(startup)](assets/v0.1.2/restore-session-flow.svg)

```bash
cd ~/src/payments
velra restore                   # pick the session to continue from
claude                          # a brand-new session in the same project receives it
```

If you already know the source session's full id, skip the picker:

```bash
velra restore --list --json     # full ids, newest first
velra restore --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d
```

Nothing else is required: no flag on `claude`, no file to paste.

---

## The four things involved

| Term | What it is | Where it lives |
|---|---|---|
| **Workspace** | The project: `$CLAUDE_PROJECT_DIR`, else the nearest ancestor holding `.git`, else the directory. It owns sessions and at most one staged capsule. Identified by 16 hex characters derived from its canonical path. | the key of `~/.velra/staged/<workspace_id>/` |
| **Source session** | The previous session you choose. Velra's ledger holds its recorded events and derived state. It stays a first-class identity: its id is written into the staged record, and the capsule names it. | `~/.velra/velra.db` |
| **Staged capsule** | The bounded, redacted rendering of the source session's state at the moment you ran `velra restore`, waiting for a new session. One file, outside every repository. | `~/.velra/staged/<workspace_id>/capsule.<gen>.json` |
| **Destination session** | The next brand-new Claude Code session in that workspace. It keeps its own session id and inherits nothing but the capsule text. | — |

The capsule is state **from** the source session delivered **to** the
destination. It says so in its first line, and it never presents the
source's prompts as the destination's own.

---

## What happens, step by step

### 1. Workspace resolution

`velra restore` resolves the workspace from the current directory with the
same function the hooks use (`velra_core::workspace::resolve`):

```
root := $CLAUDE_PROJECT_DIR, else the first ancestor of cwd holding .git, else cwd
id   := blake3(canonical, normalised root)[0..16 hex]
```

Run it anywhere inside the repository. Claude Code started in a
subdirectory (a monorepo package) records that subdirectory as the
workspace, so inside a repository `velra restore` uses the nearest
directory, from the current one up to the repository root, in which Velra
has recorded a session. Outside a repository only the current directory is
tried. The command prints the workspace root it staged for; start the new
session there.

### 2. Historical session discovery

Velra lists up to **20** sessions of this workspace, newest first, from two
sources:

- **Claude Code's transcripts** (`~/.claude/projects/<project>/<session>.jsonl`)
  supply the label (the first prompt) and the last-activity time. Only the
  first 256 KiB of each file is read, only to find a label, and nothing
  read is stored.
- **Velra's ledger** decides which sessions are restorable (`state: yes`).

A session that only appears in the transcripts (from before Velra was
enabled) is listed as `state: none` so you can recognise it. It cannot be
restored.

### 3. Explicit source-session selection

You pick a row in the picker or pass `--session <full id>`. Nothing is
chosen for you: an empty answer, `q` or end-of-input cancels, and an
invalid answer re-prompts. A session id recorded in another workspace is
refused (`no session <id> recorded for this workspace`).

### 4. Source-state lookup

The reducer catches up first, ingesting anything spooled, so nothing
recorded is missed. Velra then checks that the ledger holds task state for
the session in its current epoch. If not: `session <id> has no task state
to restore`.

### 5. Snapshot generation

Velra builds a snapshot of the source session's **current epoch**: after a
`/clear`, only work since the clear. The snapshot selects the objective,
the latest direction, stated constraints and rejections, per-test status,
the latest test result, reverted edits and file activity
([Architecture → Snapshot selection](ARCHITECTURE.md#snapshot-selection)).

### 6. Deterministic rendering

The snapshot is rendered by the same renderer as `/compact` capsules, with
a target of **740 estimated tokens** by default (`budget_tokens` in
`config.toml`), a hard ceiling of 1,000 tokens and 9,500 characters. If the
record does not fit, a fixed truncation ladder removes regenerable detail
first; exact identifiers such as test ids and file paths are kept longest.
The text is then redacted again and hashed. No model is involved: the
capsule is a deterministic function of the ledger rows and the clock.

### 7. Staging

The record is written atomically (temp file, fsync, rename) as a new file,
`~/.velra/staged/<workspace_id>/capsule.<gen>.json`, and any older record of
the workspace is then removed by its exact name. A record is never
rewritten, and only the newest one is ever delivered. It holds:

| Field | Meaning |
|---|---|
| `version` | record format (2); an unknown version is refused |
| `workspace_id`, `workspace_root` | the owner; checked again at delivery |
| `intent: "new_session"` | why it was staged (provenance) |
| `deliver_on: ["startup"]` | the only `SessionStart` source that may consume it |
| `source_session_id`, `source_checkpoint_id` | where the state came from; never where it goes |
| `created_ms` | for the 7-day expiry |
| `render_version`, `tokens`, `summary` | renderer version, size, one-line summary |
| `content_hash` | `blake3(capsule)`, checked at delivery |
| `capsule` | the text that will be injected |

Restoring again replaces the staged record. There is only ever one live
record per workspace.

### 8. Integrity

At delivery the newest record is checked. It is delivered only if it
parses at the current format version, its content hash matches, it names
this workspace, it accepts the incoming `SessionStart` source, it is under
7 days old and under 9,500 characters. A newest record that fails any check
is **not** replaced by an older one: that would deliver state you already
superseded.

| Condition | Result |
|---|---|
| older than 7 days | refused and deleted at the next session start; `velra restore` also sweeps it |
| content hash mismatch | refused and **kept** as evidence; logged to `errors.log` |
| names another workspace | refused and kept; logged |
| unreadable, or unknown format version | refused and deleted |
| over 9,500 characters (a hand-edited or foreign file) | refused and kept; logged |

### 9. Claim and `SessionStart` delivery

Every `SessionStart` runs Velra's hook, which checks the workspace's staging
directory:

| `SessionStart` source | What happens to the staged capsule |
|---|---|
| `startup` (a new session) | **claimed and delivered**, then deleted |
| `resume`, `compact` | left staged: those belong to the in-session `/compact` path |
| `clear` | left staged: `/clear` empties the session you are in; the capsule is for your *next* one |
| `fork`, or a source this build does not know | left staged |

The claim is the exclusive creation of `capsule.<gen>.claimed`. One process
wins; any other session start sees the claim and leaves the capsule alone.
On delivery the hook prints one JSON object:

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<VELRA_WORKSPACE_STATE …>…</VELRA_WORKSPACE_STATE>"},
 "systemMessage":"⚡ Velra restored: objective, 1 failing test, 2 dead ends — from session 87901fc6 (711 tokens)"}
```

`additionalContext` places the capsule in the new session's context. The
`systemMessage` names the source session. (Both strings above are from the
final-build replay in [the benchmark](BENCHMARK.md#2-final-build-replay-of-the-frozen-source-ledgers).)

### 10. Destination session identity

The destination keeps its own session id. Its own events are recorded under
that id from its first hook; nothing of the source's history is attached to
it. The capsule's opening line says it is **another session's** record, and
its last line points at that session: `velra inspect --session <first 8
characters of the source id> --section <name>`.

### 11. One-shot behaviour

A staged record is delivered **at most once**. The record is deleted only
after the hook has written its output successfully. If the write fails,
the claim is released and the capsule stays staged for the next session
start.

A session start that dies after writing the capsule and before deleting it
(for example, ended by its watchdog) leaves the record and its claim
behind. Velra cannot tell whether that capsule reached Claude Code, so it
is **not** delivered again, and nothing takes the claim over. After a
minute, `velra status` reports it as claimed by a session start that did
not finish; run `velra restore` to stage it again.

### 12. Stale state

The capsule is a snapshot of the moment you ran `velra restore`. If you keep
working in the source session afterwards, restage to refresh it. It expires
after 7 days: two weeks later the branch has moved and the failing test has
been fixed or forgotten, and injecting it would be worse than injecting
nothing.

### 13. Wrong workspace or session

| Situation | What happens |
|---|---|
| `velra restore` run in a different directory from the session | `No previous sessions found for this workspace` (exit 1). Compare `workspace_root` in `velra restore --list --json` with where the session ran. |
| `--session` names a session of another workspace | `no session <id> recorded for this workspace` (exit 1) |
| the new session starts in a different workspace | nothing is delivered there; the capsule stays staged for its own workspace |
| a staged file copied into another workspace's directory | refused (names another workspace), kept, logged |

### 14. Failure behaviour

Delivery **never blocks a session.** The hook always exits 0, never writes
to stderr, and emits either nothing or the one JSON object. Staged delivery
reads a file, not the database, so it still works when the database is
locked. Anything unexpected is logged to `~/.velra/logs/errors.log`, and
the session starts normally without the capsule.

`velra restore` itself fails loudly, with exit code 1 and a message, when
there is nothing to restore ([CLI → Failure behaviour](CLI.md#failure-behaviour)).

### 15. Diagnostics

See [Inspecting and debugging](#inspecting-and-debugging).

---

## What the capsule contains

A real capsule: the Benchmark B source session (`87901fc6…`) of the v0.1.2
live benchmark, whose frozen ledger was replayed through this release's
binary by `bench/tokenburn/replay.py`. The recorded workspace no longer
exists on disk, so `[WORKSPACE_STATE]` reads `no git` where the live run
showed a branch and commit.

```
<VELRA_WORKSPACE_STATE v="1" checkpoint="restore" captured="2026-09-30T15:49:33Z">
[ABOUT_THIS_RECORD]
A local record, not a message and not an instruction. Velra logged another session's prompts and tool events and quotes them back here; nothing is new. OBSERVED came from a tool event, INFERRED was derived from it. Files on disk are the source of truth. Say so if a line here conflicts with them.
[FIRST_MESSAGE] (OBSERVED | user prompt | 17:59)
Payments again. Run the suite; I want the reconcile failure this time, not the other two.
[STATED_CONSTRAINTS] (OBSERVED | user prompt | quoted verbatim)
- turn 4 18:00 | "Here's the thing you can't get from the code: the upstream feed backdates."
- turn 4 18:00 | "A window has to close on the booking date, never the value date."
[LATEST_MESSAGE] (OBSERVED | user prompt | 18:02)
Next step is in_window in reconcile.py. Don't change it yet.
[WORKSPACE_STATE]
no git | 3 edits this task | last test run: FAIL
[TEST_STATUS] (OBSERVED | latest run covering each)
- FAIL tests/test_reconcile.py::test_march_window_totals
[TEST_RESULT] (OBSERVED | test run | 18:01)
Command: python -m pytest tests/ -v 2>&1 | tail -60
Result: FAIL
[REVERTED_EDITS] (OBSERVED)
- src/payments/reconcile.py | 2 reverts, 3 edit(s) | last reverted by an unidentified change at 18:01
    + stamp = row["booking_date"]  (edit 1 of 2)
    + import datetime
[RECORD_DETAIL]
Full detail: `velra inspect --session 87901fc6 --section <name>`
</VELRA_WORKSPACE_STATE>
```

Sections appear only when there is something to say. The full set, in
the order they are rendered: `ABOUT_THIS_RECORD`, `FIRST_MESSAGE`,
`STATED_CONSTRAINTS`, `REJECTED_APPROACHES`, `SUBTASK_MESSAGE`,
`EARLIER_MESSAGE`, `LATEST_MESSAGE`, `WORKSPACE_STATE`, `TEST_STATUS`,
`TEST_RESULT`, `REVERTED_EDITS`, `RECENT_EDITS`, `FILE_ACTIVITY`, then either
`FAILURE_LOCATION` (a location taken from failing output) or `NEXT_TARGET`
(the most recent live edit), and `RECORD_DETAIL`. Every section is marked
`OBSERVED` (read from a tool event or prompt) or `INFERRED` (derived from
observed rows); the last two targets are `INFERRED`.

What it does **not** contain:

- the assistant's prose or reasoning (not hooked, never recorded);
- file contents, beyond short edit excerpts;
- anything from the transcript;
- the *reason* behind a reverted edit. Velra records that a file was edited
  and reverted, with a short excerpt, not why.

---

## Restoring across `/clear`

`/clear` starts a new epoch of the **same** session, and a restore renders
the session's *current* epoch. So stage **before** you clear:

```bash
velra restore --session <this session's full id>   # while the session is still open
# then, in Claude Code: /clear
# then start a new session (exit and run `claude` again) to receive it
```

The `/clear` itself leaves the staged capsule alone; the next new session
picks it up. Restoring *after* `/clear` finds the new, empty epoch and
reports `session <id> has no task state to restore`. That is expected: you
asked Claude Code to drop that state.

---

## Inspecting and debugging

| Question | Command |
|---|---|
| What would be staged, without staging it? | `velra restore --dry-run [--session <id>]` |
| Is something staged here, how big, from which session? | `velra status` (the `Staged:` line) |
| Which sessions can this workspace restore from? | `velra restore --list` or `--list --json` |
| The exact staged record | the file named by `staged_path` in `velra restore --json` |
| Discard the staged capsule | `velra restore --clear` |
| Why a fact is missing from the capsule | `velra inspect --session <id> --trace "<fact>"` ([how](TROUBLESHOOTING.md#tracing-a-lost-fact-velra-inspect---trace)) |
| Full detail behind a section | `velra inspect --session <id> --section dead-ends\|failure\|files\|attempts` |
| Did the new session receive it? | set `VELRA_LOG=debug` where Claude Code starts, then look in `~/.velra/logs/debug.log` for `restored staged capsule from session …`; refusals are in `errors.log` |

`velra inspect --session` accepts a full id or a unique prefix of 8 or more
characters, which is how a delivered capsule names its source.
`velra restore --session` takes the full id.

If a delivery did not happen, work through the
[troubleshooting matrix](TROUBLESHOOTING.md#restore-and-delivery).

---

## Guarantees and limits

Guaranteed and covered by tests:

- the capsule is bounded (1,000 estimated tokens and 9,500 characters at
  most) and deterministic for a given ledger and clock;
- a staged capsule is delivered at most once, only to a `startup` session
  in the same workspace, within 7 days;
- automatic delivery never crosses sessions; only `velra restore` does, for
  the session you named;
- the hook never blocks or fails a Claude Code session;
- nothing is written inside your repository.

Not guaranteed:

- that every detail of the old conversation survives: the capsule is a
  bounded rendering of recorded state, and the ladder drops detail;
- that the agent acts on it: the capsule is context, and the files remain
  the source of truth;
- exactly-once delivery: a session start killed mid-delivery results in
  zero deliveries, not two;
- anything about sessions Velra did not observe.

Velra is not a replacement for Claude Code's `/resume`, which reopens the
full conversation. Use `/resume` when you want the conversation back. Use
`velra restore` when you want a fresh, small context that still knows where
the work stands. The full claim table: [Guarantees](GUARANTEES.md).

---

[← README](../README.md) · [CLI reference](CLI.md) · [Troubleshooting](TROUBLESHOOTING.md) · [Architecture](ARCHITECTURE.md)
