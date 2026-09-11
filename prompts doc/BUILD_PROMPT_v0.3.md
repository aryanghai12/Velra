# BUILD_PROMPT_v0.3.md — Velra v0.3 "On-Demand Recovery & Local MCP Layer"

## 0. Instructions to the implementing agent

You are extending a working **Velra v0.2** codebase to **v0.3**. This document is self-contained: §1 restates invariants you MUST preserve; the rest specifies new behavior.

- RFC 2119 keywords apply. Implement only what is in scope. Record ambiguity resolutions in `DECISIONS.md`.
- The MCP server is a **long-lived, read-only** process. It MUST NEVER write to the database, and it MUST NEVER make the hook path slower.
- Confirm current Claude Code MCP registration commands and MCP protocol version against official docs at build time; encode results in the `compat` module.

---

## 1. Carried-forward invariants (v0.1 + v0.2; MUST NOT regress)

1. Hooks: always exit 0; stdout empty or one JSON object; stderr empty; watchdogs; kill switch; no async runtime initialized on the hook path.
2. Budgets: sync hooks p99 ≤ 5–6 ms (macOS/Linux), `pre-compact` ≤ 10 ms, Windows ≤ 15 ms wall.
3. Append-only events + spool + transactional cursor reducer; heavy work only in async `reduce`.
4. Immutable checkpoints; one live continuation per session; two-phase delivery with idempotent `delivery_key`; channels `session_start` → `post_tool` → `user_prompt`.
5. Root intent only on explicit boundaries. Deterministic, factual, zero-LLM capsule with provenance classes and `UNCONFIRMED` causality; ≤800 est. tokens target; any single hook output ≤ 9,500 characters.
6. Health notices advisory only; nothing automatic.
7. No network, no telemetry, no repository files, no daemon, redaction before persistence, sensitive paths never excerpted or snapshotted.

---

## 2. Scope

### In scope (v0.3)
1. **Local MCP server** `velra mcp` (stdio transport) exposing read-only recovery tools over the archive.
2. **Automatic MCP registration** in `velra enable` (user scope), with clean removal in `velra disable`.
3. **Hot / Warm / Cold tiering** with explicit tier assignment rules, budgets, and transitions.
4. **Warm Start Pack** assembly (boundary-complete source units, failing tests, recent diff hunks, referenced config snippets).
5. **Cold archive:** full command outputs and diffs (compressed), native compaction summaries, resolved items; SQLite FTS5 index.
6. **Fresh-session continuation:** `velra resume` launches a new Claude Code session that receives Capsule + Warm Start Pack via `SessionStart`.
7. **Task graph:** tasks spanning multiple sessions (compactions, resumes).
8. CLI: `velra resume`, `velra history`, `velra export`, `velra gc`.
9. Capsule render_version 3 `[RECOVERY]` section pointing to MCP tools.

### Out of scope (v0.3) — MUST NOT implement
Network transports for MCP (HTTP/SSE); write-capable MCP tools; embeddings/vector search; cloud sync; other agents; automatic rotation; LLM calls; MCP `resources`/`prompts` primitives (tools only).

---

## 3. MCP server

### 3.1 Process model
- Command: `velra mcp`. Transport: stdio JSON-RPC per the MCP specification version pinned in `compat`. Implementation SHOULD use the official Rust MCP SDK (`rmcp`); the async runtime is initialized **only** inside the `mcp` subcommand.
- Opens the database read-only (`mode=ro` URI or `PRAGMA query_only = ON`); WAL readers never block hook writers.
- **Project scope:** `CLAUDE_PROJECT_DIR` from the environment (Claude Code sets it for stdio MCP servers); fallback: current directory → git root.
- **Session scope:** tools accept an optional `session_id`; default = the most recently active session of the project.
- Startup ≤ 150 ms; idle memory ≤ 30 MB; each tool call p95 ≤ 50 ms on a DB with 1,000,000 events.
- Crash or DB unavailability MUST surface as an MCP tool error (`isError: true`) with a one-line reason; never hang.

### 3.2 Registration
- `velra enable` (unless `--no-mcp`): if a `claude` executable is on PATH, run the documented user-scope add command (verify syntax at build time; expected form `claude mcp add --scope user velra -- {BIN} mcp`), idempotently (detect existing `velra` server; update if the path differs). If the CLI is unavailable, print the exact manual command and continue (exit 0).
- `velra disable` removes the registration via the documented remove command.
- Velra MUST NOT hand-edit Claude Code's internal state files for MCP registration.

### 3.3 Output discipline (all tools)
- Plain-text responses with a fixed header line per item: `[{kind} {id}] ({provenance class}) {timestamp}`.
- Default response cap **2,000 est. tokens** (`ceil(chars/3.2)`); hard cap 8,000 characters; when truncated, append `truncated: true | next_cursor: {opaque}`.
- Redaction re-applied at output time. Sensitive paths: path + hash only.
- Tool descriptions are factual statements of what the tool returns (no imperative instructions to the model).
- Every item states freshness (`VERIFIED`/`OBSERVED`/`STALE`, per v0.2 rules evaluated at query time).

### 3.4 Tools (names are exact)

| Tool | Input (JSON Schema summary) | Returns |
|---|---|---|
| `velra_checkpoint` | `checkpoint_id?: string` | The frozen capsule of that checkpoint (default: latest in session) + metadata (trigger, partial, commit, delivery state) |
| `velra_search` | `query: string (1..200)`, `kinds?: [prompt, command, failure, dead_end, attempt, compaction_summary, diff]`, `limit?: 1..20 (default 8)`, `cursor?` | FTS5 matches with ≤300-char snippets, ids, timestamps, provenance |
| `velra_attempts` | `path?`, `symbol?`, `status?: [ACTIVE, REVERTED, DISCARDED, COMMITTED, REAPPLIED]`, `limit?`, `cursor?` | Edits and dead ends with region, excerpt, mechanism, observed-afterward results (always `UNCONFIRMED` causality) |
| `velra_failure` | `fingerprint?`, `test_id?`, `limit?` | Timeline of runs for the fingerprint/test: outcomes, first/last seen, flakiness, related edits between runs |
| `velra_diff` | `edit_id: integer`, `context_lines?: 0..10 (default 3)` | Unified diff from snapshots, capped at 200 lines; refuses sensitive paths |
| `velra_output` | `command_id: integer`, `stream?: stdout\|stderr\|both`, `tail_lines?: 1..400 (default 120)` | Archived full command output (redacted), tail-first |
| `velra_working_set` | `budget_tokens?: 500..4000 (default 1800)` | The Warm Start Pack (§5) at the requested budget |
| `velra_evidence` | `item_ref: string` (e.g. `dead_end:42`) | Provenance record: source events, hashes, commit, timestamps, class |
| `velra_history` | `limit?: 1..50` | Sessions and checkpoints of the current task graph with one-line summaries |

---

## 4. Hot / Warm / Cold tiering

### 4.1 Definitions and budgets
| Tier | Contents | Delivery | Budget |
|---|---|---|---|
| **HOT** | Capsule: root objective, active subtask/latest request, status, active failure, active symbols (signatures), live dead ends, next target, recovery pointer | Injected automatically after compaction / at resume | ≤ 800 est. tokens (target), ≤ 1,000 hard |
| **WARM** | Warm Start Pack: boundary-complete source of active symbols (≤80 lines each, else skeleton), bodies of failing tests, recent diff hunks, referenced config snippets | Injected **only** on `velra resume` fresh sessions; otherwise via `velra_working_set` | ≤ 6,000 characters when injected (so capsule + pack ≤ 9,500 characters) |
| **COLD** | Full command outputs, all diffs, resolved failures, superseded intents, older checkpoints, native compaction summaries, reapplied/old dead ends | Only via MCP tools and CLI | Unbounded (disk), never injected |

### 4.2 Tier assignment rules (deterministic; evaluated by the reducer; stored as `tier` column on relevant rows)
- Failure fingerprint → WARM when its signature passes in 2 consecutive runs; → COLD after 1 further checkpoint without recurrence.
- Dead end → COLD when `reapplied = 1`, when its region's symbol no longer exists, or after 3 checkpoints in which the region was untouched.
- Active symbol → WARM when not edited, read, or referenced by a failure in the last 40 tool events; → COLD when the epoch changes.
- Intent rows superseded → COLD.
- Any item marked `STALE` for 2 consecutive checkpoints → COLD.
- Promotion COLD → HOT happens only through new observations (e.g. the fingerprint recurs), never through retrieval alone.
- Nothing is deleted by tiering. Deletion happens only via `velra gc` (§7).

---

## 5. Warm Start Pack

### 5.1 Composition order (fill until budget; each unit is boundary-complete)
1. Failing test bodies for the active failure (test symbol source; skeleton if > 80 lines).
2. Active symbols in capsule rank order (full source if ≤ 80 lines and the current disk hash matches the snapshot; otherwise skeleton + `STALE`).
3. Most recent diff hunks of ACTIVE edits (≤ 40 lines each, max 3).
4. Config snippets: files matching common config names (`*.config.*`, `*.toml`, `*.yaml`, `*.yml`, `*.json`, `.env.example`) that appear in failure output or were edited in the epoch; only the enclosing key/section (≤ 30 lines); never sensitive paths.

### 5.2 Envelope
```
<VELRA_WORKING_SET v="1" checkpoint="{checkpoint_id}">
[NOTE]
These are copies of source units as recorded by Velra at {captured}. Files on disk are authoritative; re-read a file before editing it.
[UNIT {n}] {kind} {qualified_name} | {path}:{start}-{end} | {VERIFIED|STALE}
{source text or skeleton}
...
</VELRA_WORKING_SET>
```
The pack MUST NOT imitate the format of any native tool result (no fake `Read` output, no line-number gutters mimicking Claude Code tools).

---

## 6. Fresh-session continuation (`velra resume`)

- `velra resume [--checkpoint <id>] [--no-warm] [-- <args passed to claude>]`
  1. Resolve the checkpoint (default: most recent in the current project; if no checkpoint exists, create one now with `trigger = 'cli'` from current state, same rules as the PreCompact barrier).
  2. Create a one-time `resume_tokens` row (`token` random 128-bit hex, `checkpoint_id`, `created_ms`, `consumed_ms NULL`, expiry 10 minutes).
  3. Launch `claude` with inherited stdio and the environment variable `VELRA_RESUME_TOKEN={token}` (Unix: `exec`; Windows: spawn, wait, propagate exit code).
- `session-start` hook with `source = startup`: if `VELRA_RESUME_TOKEN` is present and the token is unconsumed and unexpired → consume it atomically, link the new session to the checkpoint's task (§6.1), deliver Capsule + Warm Start Pack in a single `additionalContext` (≤ 9,500 chars), `systemMessage`: `⚡ Velra resumed task: {summary} + working set ({k} units)`. Subsequent `SessionStart` events in the same process (compact/clear) ignore the consumed token.
- The old session is never modified or terminated.

### 6.1 Task graph
- New table `tasks (task_id TEXT PK, project_id, root_intent_id, created_ms, status)`; `sessions` gains `task_id`, `parent_session_id`, `parent_checkpoint_id`.
- A task spans: its original session epoch, all compactions within it, and all sessions started via `velra resume` from its checkpoints. `task:` prefix or `/clear` creates a new task.

---

## 7. Cold archive & retention

- Full command outputs: `post-tool-use` stores outputs ≤ 64 KiB directly as blobs (zstd); larger outputs (≤ 1 MiB) are spooled raw and compressed by the reducer; > 1 MiB → head 64 KiB + tail 256 KiB. Linked via `command_outputs (command_id, stream, blob_hash, truncated)`.
- Native compaction summaries (from `post-compact`) are indexed.
- **FTS5** virtual table `archive_fts(kind, ref_id, session_id, ts_ms, text)` populated by the reducer from redacted text. Verify FTS5 is compiled into the bundled SQLite; enable via build flags if not.
- **Retention:** no automatic deletion. `velra gc [--older-than <days>] [--project <path>] [--dry-run]` deletes COLD rows and unreferenced blobs, never the latest checkpoint per task; `velra status` warns above 1 GiB total.

---

## 8. CLI additions

| Command | Behavior |
|---|---|
| `velra resume` | §6 |
| `velra history [--project] [--limit N] [--json]` | Tasks → sessions → checkpoints tree with summaries and delivery states |
| `velra export <checkpoint_id> [--format md\|json] [--include-warm]` | Writes to stdout; redaction applied; sensitive content omitted |
| `velra gc` | §7 |

---

## 9. Capsule render_version 3

Only `[RECOVERY]` changes:
```
[RECOVERY]
Velra MCP tools available in this session: velra_attempts, velra_failure, velra_diff, velra_search, velra_working_set, velra_evidence. CLI equivalent: `velra inspect --checkpoint {checkpoint_id} --section <name>`.
```
If `velra enable --no-mcp` was used, render the v2 CLI-only line instead.

---

## 10. Schema additions (migration to `user_version = 3`)
`tasks`, `resume_tokens`, `command_outputs`, `archive_fts` (FTS5), `tier TEXT NOT NULL DEFAULT 'HOT' CHECK (tier IN ('HOT','WARM','COLD'))` columns on `failures`, `dead_ends`, `edits`, `intents`, `symbols`; `sessions.task_id`, `sessions.parent_session_id`, `sessions.parent_checkpoint_id`. Backfill: existing sessions get one task each; tiers computed on the first reducer pass.

---

## 11. Acceptance tests (in addition to v0.1 + v0.2 suites)

### M. MCP
- M1 Protocol conformance: initialize, tools/list, tools/call for every tool, using the MCP reference inspector or an SDK client in CI; schemas validate.
- M2 Read-only: the server process never acquires a write lock (verified by running C1-style write load concurrently: hook budgets unchanged, zero `SQLITE_BUSY` spools caused by the server).
- M3 Caps: every tool respects 2,000-token default and 8,000-char hard cap; cursors paginate to completion without duplicates.
- M4 Sensitive path requests (`velra_diff` on `.env`) refused with a clear message.
- M5 Performance: startup ≤ 150 ms; p95 tool latency ≤ 50 ms at 1,000,000 events.
- M6 `enable`/`disable` register and remove the server idempotently; missing `claude` CLI prints manual instructions and exits 0.

### T. Tiering & warm pack
- T1 Tier transition scenarios for each rule in §4.2 (golden DB states).
- T2 Warm pack never exceeds 6,000 characters; units are boundary-complete; STALE units rendered as skeleton.
- T3 Capsule + warm pack in one `additionalContext` ≤ 9,500 characters for 1,000 random states (property test).
- T4 Warm pack contains no text resembling native tool output formats (regression corpus).

### Q. Resume & task graph
- Q1 `velra resume` → new session's first `SessionStart(startup)` delivers capsule + pack exactly once; later SessionStart events in that process do not.
- Q2 Expired or reused token → no delivery, logged.
- Q3 Old session untouched; task graph links both sessions; `velra history` shows the tree.
- Q4 Windows: exit code of `claude` propagated.

### K. Archive
- K1 FTS search finds prompts, dead ends, failures, and compaction summaries; redacted content never matches secret literals.
- K2 `gc --dry-run` lists without deleting; `gc` never deletes the latest checkpoint per task; DB integrity check passes afterward.

### E2E (manual, recorded against a named Claude Code version)
1. After a compaction, ask Claude "why did we abandon the previous approach?" → Claude calls `velra_attempts` or `velra_search` and answers with evidence instead of re-running the experiment.
2. `velra resume` → the first turn of the new session starts from the working set (count Read calls on turn 1 vs a plain `claude` + manual note baseline; record both).

---

## 12. Milestones & Definition of Done
1. **M1** Schema v3, task graph, archive + FTS (K1, K2).
2. **M2** Tiering rules + warm pack (T1–T4).
3. **M3** MCP server + tools + registration (M1–M6).
4. **M4** `resume` / `history` / `export` / `gc` + render_version 3 (Q1–Q4).

**Done** = all v0.1–v0.3 tests pass on all platforms; hook budgets unchanged; E2E recorded; `DECISIONS.md` updated.
