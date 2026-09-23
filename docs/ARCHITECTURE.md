# Architecture

Velra is a hook-driven, local-first recorder. It watches what a Claude Code
session *does*: prompts, tool calls, edits, commands, test results, reverts.
It derives a small operational state from those events, and hands a bounded
rendering of that state to a later context, either after `/compact` in the
same session or, through `velra restore`, to a brand-new session.

It never reads or replays the conversation. It never calls a model. It never
touches the network at runtime.

- [Lifecycle](#lifecycle)
- [Components](#components)
- [Storage](#storage)
- [Workspaces and sessions](#workspaces-and-sessions)
- [From events to a capsule](#from-events-to-a-capsule)
- [Two delivery paths](#two-delivery-paths)
- [Design properties](#design-properties)
- [Fail-open hooks](#fail-open-hooks)
- [Where to read the code](#where-to-read-the-code)

---

## Lifecycle

```
Claude Code SOURCE session
   │  hook events: SessionStart, UserPromptSubmit, Pre/PostToolUse, Stop, PreCompact, …
   ▼
velra hook <event>          normalise → redact → append           (crates/velra/src/hook.rs)
   │
   ▼
events  (SQLite, append-only; spool/ when the DB is busy)          (velra-core: eventlog, spool)
   │
   ▼
reducer  → projections: intents, constraints, commands, edits,     (velra-core: reducer)
           file_versions, dead_ends, file_stats, …   = WORKSPACE / SESSION STATE
   │
   ├───────────── in-session path ───────────────┐
   │                                              ▼
   │                        PreCompact → checkpoint (frozen capsule) → continuation
   │                        PENDING → ATTACHED → CONFIRMED on SessionStart(compact|resume)
   │                        / UserPromptSubmit / PostToolUse, same session only
   │
   └───────────── cross-session path (explicit) ─┐
                                                  ▼
                 velra restore  → snapshot::build → render (bounded) → redact
                                → STAGED CAPSULE  ~/.velra/staged/<workspace_id>/staged_capsule
                                                  │
                                                  ▼
                 new Claude Code session → SessionStart(startup)
                                → staging::claim_with (once) → additionalContext
                                                  │
                                                  ▼
                                   DESTINATION session continues the work
```

## Components

| Crate / module | Responsibility |
|---|---|
| `crates/velra` (binary) | |
| `main.rs` | dispatches `velra hook <event>` and `velra reduce` straight from `argv`, without clap or a logger, before anything else. Everything else goes to the CLI |
| `hook.rs` | the hook runtime: read stdin, normalise, redact, append, deliver; watchdog; panic containment; one-JSON-object stdout |
| `normalize.rs` | Claude Code hook payload → `NewEvent`; per-field size caps |
| `cli.rs` | `enable`, `disable`, `status`, `inspect`, `doctor`, `restore` |
| `restore.rs` | session discovery (ledger ∪ transcripts) and the picker |
| `inspect.rs` | previews, section detail, `--trace` presentation |
| `settings.rs` | JSONC-preserving edits to Claude Code's `settings.json`; the hook registration table |
| `compat.rs` | Claude Code version detection and feature gates |
| `home.rs`, `log.rs`, `atomic.rs` | `$VELRA_HOME` layout, `config.toml`, logs, atomic writes |
| `crates/velra-core` (library, no Claude Code I/O) | |
| `db`, `eventlog`, `spool` | SQLite (WAL) schema and migrations; append; the spool for a busy database |
| `reducer` | cursor-based projection of `events` into task state; session epochs |
| `intent`, `constraint`, `commands`, `testids`, `revert`, `shell`, `git` | the derivations: objective and later directives, constraints stated in prompts, command outcomes, per-test status, revert detection by content hash |
| `snapshot` | selects what a capsule may contain from the projections |
| `render` | the pure renderer and the truncation ladder (`DEFAULT_BUDGET_TOKENS = 740`, `HARD_CEILING_TOKENS = 1000`, `ABSOLUTE_MAX_CHARS = 9500`) |
| `checkpoint`, `continuation` | the `/compact` path: frozen capsules and the exactly-once delivery state machine |
| `workspace` | the single definition of workspace identity |
| `restore`, `staging` | explicit cross-session restore: build, stage, claim |
| `transcript` | tolerant reading of Claude Code transcript headers, for picker labels |
| `provenance` | `inspect --trace`: presence per layer and the first loss |
| `redact`, `paths`, `hash`, `text`, `time` | redaction, path identity, blake3 hashing, token estimation, time |

## Storage

Everything is under `$VELRA_HOME` (default `~/.velra`) and stays on the
machine. The database (`velra.db`, schema v2) holds:

| Table(s) | Contents |
|---|---|
| `events` | the append-only log of redacted hook payloads |
| `projects`, `sessions` | workspaces and their sessions (with the current epoch) |
| `intents`, `constraints` | the objective and directives from prompts; constraints quoted verbatim |
| `commands`, `edits`, `file_versions`, `file_stats`, `dead_ends` | what ran and with what result; what changed; content hashes; reverts |
| `checkpoints`, `compactions`, `continuations`, `injections` | the `/compact` path |
| `reducer_cursor`, `meta` | reducer position and schema metadata |

A staged capsule is **not** in the database. It is one JSON file per
workspace, so delivery works even when the database is locked. See
[Configuration → Files](CONFIGURATION.md#files-and-directories) for the full
layout.

## Workspaces and sessions

```
WORKSPACE  (durable ownership boundary; id = blake3(canonical root)[0..16])
├── SESSION A  → events, projections, checkpoints     ← a source session, first-class
├── SESSION B  → …
└── staged capsule  (at most one; only by an explicit `velra restore`)
```

**Workspace resolution** is one function, `velra_core::workspace::resolve`,
called by both the hook and the CLI:

```
root := $CLAUDE_PROJECT_DIR, else the first ancestor of cwd holding .git, else cwd
id   := blake3(identity(normalize_abs(canonical(root))))[0..16 hex]
```

Canonicalisation makes different spellings of one directory (symlinks, case
on Windows, separators) the same workspace. The two callers once disagreed
about `CLAUDE_PROJECT_DIR`. Restore would then silently never deliver, which
is why the definition now lives in one place and a test drives the real
binary to prove the hook and the CLI agree.

**Sessions** are Claude Code's session ids. A session's state is scoped to
its current *epoch*: `SessionStart(clear)` starts a new epoch, so `/clear`
empties what a later restore or compaction would render, without deleting
the recorded events.

## From events to a capsule

1. **Capture.** Each hook normalises its payload, redacts secrets **before**
   anything is persisted (spool included), caps each field, and appends one
   event. Edits are hashed before (`PreToolUse`) and after (`PostToolUse`), so
   a later `git restore` or inverse edit is detected by comparing content
   hashes, not by interpreting text.
2. **Reduce.** The reducer walks new events from its cursor and updates the
   projections. It runs asynchronously on `Stop`/`PostToolBatch`, in a
   bounded pass at `PreCompact`, and in full before `inspect`/`restore`.
3. **Snapshot.** `snapshot::build` selects the state a capsule may contain:
   first and latest prompts (and the latest earlier one that names code),
   quoted constraints, per-test status, the latest test result and failure
   location, reverted edits, recent edits, file activity.
4. **Render.** `render::render` is a pure function of the snapshot and the
   budget. Every line is marked `OBSERVED` or `INFERRED`. If the text exceeds
   the target, a fixed, named ladder of reductions runs in order, shedding
   regenerable prose before exact identifiers, until it fits. It never
   exceeds the hard ceiling.
5. **Deliver** by one of the two paths below.

## Two delivery paths

| | In-session continuation | Cross-session restore |
|---|---|---|
| Trigger | `/compact` (manual or auto) | you run `velra restore` |
| Source | the same session | a session **you name** |
| Built from | a checkpoint frozen at `PreCompact` | the source session's current epoch, rendered when you run the command |
| Stored in | database (`checkpoints`, `continuations`) | `staged/<workspace_id>/staged_capsule` |
| Delivered on | `SessionStart(compact\|resume)`, else the first `UserPromptSubmit` or `PostToolUse` after compaction | `SessionStart(startup)` only |
| Exactly once by | `PENDING → ATTACHED → CONFIRMED` with a partial unique index and conditional updates | exclusive creation of a claim marker; deleted only after successful output |
| Expires | 7 days `PENDING` | 7 days after staging |
| Crosses sessions | **never** | only this way |

Automatic delivery is strictly session-scoped. A test pins
(`continuations_never_cross_sessions_without_an_explicit_restore`) that one
session's checkpoint is never handed to another. `velra restore` is the only
path across, and it opens only because a person selected the source by hand.

A staged record carries its own eligibility (`intent`, `deliver_on`), so the
claim logic contains no source names. A future `/clear`-handoff or
resume-handoff workflow would be a new intent value, not a new code path. A
source this build does not know is refused by default.

## Design properties

- **Durable workspace ownership.** The workspace is the key for sessions and
  for the staged slot; a capsule cannot be delivered into another workspace.
- **Source sessions are first-class.** Sessions are never merged. The source
  session id is recorded in the capsule and shown when it is delivered.
- **Deterministic and bounded.** The same ledger at the same clock renders
  byte-identical text. The token budget is a hard property of the renderer.
- **Explicit provenance.** Each capsule line is `OBSERVED` or `INFERRED`, and
  `velra inspect --section` / `--trace` go from a capsule line back to its
  rows.
- **Startup-only delivery for restores.** A restore capsule is never consumed
  by the session you are leaving.
- **Fail-open hooks.** See below.
- **Local-first.** No network code in the hook path. CI fails the build if a
  network-capable crate enters that dependency graph
  (`ci/check-no-network-deps.sh`).
- **No transcript replay.** Transcripts are read only for picker labels (the
  first 256 KiB), and `--trace`'s first layer. The capsule is built from
  Velra's own event log.

## Fail-open hooks

The hook path is built so that Velra can never be the reason a Claude Code
session fails:

- `velra hook` and `velra reduce` are dispatched before clap, logging or any
  global initialisation;
- the process **always exits 0**; panics are caught (the release profile keeps
  `panic = "unwind"` for this reason);
- stderr is never written; stdout is empty or exactly one JSON object, guarded
  by a once-lock so a watchdog can never interleave a partial write;
- a watchdog abandons work after 250 ms (sync hooks) or 1 s (reducer), and an
  in-flight event goes to the spool, so nothing recorded is lost;
- a locked database spools the event; a corrupt one is moved aside
  (`velra.db.corrupt-<ts>`) and recreated;
- `VELRA_DISABLE=1` or `~/.velra/disabled` makes every hook return
  immediately.

`crates/velra/tests/fail_open.rs` and the fault-injection feature
(`--features fault-injection`, `VELRA_TEST_*`) exercise panics, stalls and
broken databases against the real binary.

## Where to read the code

| To understand | Start at |
|---|---|
| a hook invocation end to end | `crates/velra/src/hook.rs` → `run` |
| what the capsule contains and why | `crates/velra-core/src/snapshot.rs`, `render.rs` |
| restore | `crates/velra/src/cli.rs` → `cmd_restore`, `crates/velra-core/src/restore.rs`, `staging.rs` |
| delivery at session start | `crates/velra/src/hook.rs` → `session_start`, `deliver_staged` |
| workspace identity | `crates/velra-core/src/workspace.rs` |
| `--trace` | `crates/velra-core/src/provenance.rs` |
| every design decision and deviation | [`DECISIONS.md`](../DECISIONS.md) (D1–D69) |

---

[← README](../README.md) · [Restore](RESTORE.md) · [Configuration](CONFIGURATION.md) · [Benchmark](BENCHMARK.md)
