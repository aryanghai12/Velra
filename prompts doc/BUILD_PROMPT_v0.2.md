# BUILD_PROMPT_v0.2.md — Velra v0.2 "Structural Intelligence & Failure Normalization"

## 0. Instructions to the implementing agent

You are extending an existing, working **Velra v0.1** codebase (Rust, single static binary, SQLite WAL, Claude Code hooks) to **v0.2**. This document is self-contained: §1 restates the v0.1 invariants you MUST preserve; §2 onward specify the new behavior.

- RFC 2119 keywords apply. Implement only what is in scope. Record ambiguity resolutions in `DECISIONS.md`.
- Anything computationally heavy in v0.2 (parsing, diffing, fingerprinting, scoring) MUST run in the **async reducer path**, never in a synchronous hook. Sync hooks keep their v0.1 budgets.
- Before starting, run the full v0.1 acceptance suite; it MUST remain green at every milestone.

---

## 1. Carried-forward invariants (from v0.1; MUST NOT regress)

1. Hooks always exit 0; stdout empty or exactly one JSON object; stderr empty; internal watchdog 250 ms (sync) / 1,000 ms (reduce); kill switch `VELRA_DISABLE` / `~/.velra/disabled`.
2. Sync hook budgets: p99 ≤ 5–6 ms (macOS/Linux), `pre-compact` ≤ 10 ms, Windows ≤ 15 ms wall.
3. `pre-compact` never reads the transcript, spawns processes, or performs network I/O.
4. Append-only `events`, spool fallback, cursor-based transactional reducer, immutable checkpoints, single live continuation per session, two-phase delivery (`session_start` → `post_tool` → `user_prompt`) with idempotent `delivery_key`.
5. Root intent changes only on explicit boundaries (`task:` prefix, `/clear`).
6. Deterministic, zero-LLM capsule; factual (non-imperative) wording; causal links always `UNCONFIRMED`; ≤800 est. tokens target, ≤9,500 chars absolute.
7. No network, no telemetry, no repository files, no daemon, redaction before persistence, sensitive paths never excerpted.

---

## 2. Scope

### In scope (v0.2)
1. **Structural layer:** tree-sitter-based symbol extraction; mapping of edits and failure locations to their complete enclosing symbol (function, method, class, test block); signatures and structural skeletons for large symbols.
2. **Content snapshots:** content-addressed, compressed storage of file versions (needed for diffs and symbol mapping).
3. **Polyglot failure fingerprinting:** normalization pipeline + line-oriented recognizers for common runners and compilers; per-test outcome history; flakiness scoring.
4. **Diff churn & loop detector:** region-level (symbol-level) oscillation, repeated identical failures, redundant rereads, context-pressure proxy, progress detection.
5. **Session health (advisory only):** `HEALTHY` / `DRAGGING` / `LOOPING` with evidence; rate-limited user-facing notice.
6. **Provenance & staleness:** evidence classes `VERIFIED`, `OBSERVED`, `INFERRED`, `UNCONFIRMED`, `STALE` on every capsule item; render-time freshness checks.
7. **Capsule render_version 2** with `[ACTIVE_SYMBOLS]`, symbol-level dead ends, fingerprinted failures.
8. CLI: `velra why` (explain current health and signals); `velra inspect --section symbols|health`.

### Out of scope (v0.2) — MUST NOT implement
Any automatic action (compaction, session rotation, blocking); MCP; warm start pack; `resume`/`history`/`export`; other agents; LLM calls; embeddings; network access; language servers; running the user's tests.

---

## 3. Structural layer

### 3.1 Parser technology
- Use **tree-sitter** via the official Rust bindings with grammars compiled into the binary (static, no runtime downloads).
- **Tier-1 languages (MUST):** TypeScript, TSX, JavaScript (incl. JSX), Python, Go, Rust, Java.
- **Tier-2 (SHOULD):** C#, Ruby, PHP, Kotlin, C, C++, Swift.
- Language detection by file extension (plus shebang for extensionless scripts). Unknown language → file-level fallback (v0.1 behavior).
- Binary size budget after grammars: ≤ 30 MB. Grammar initialization MUST be lazy (no cold-start cost for hooks that never parse).
- Parse limits: skip files > 1 MiB or with parse time > 50 ms (record `parse_skipped` with reason). Parsing happens only in the reducer.

### 3.2 Symbol extraction
- Use tree-sitter tag queries (vendored `tags.scm`-style queries per grammar, pinned by grammar version) plus Velra-owned **test-block queries**: Jest/Vitest/Mocha `describe|it|test(...)` call blocks (name = first string argument, nested names joined with ` > `); pytest `def test_*` and `class Test*`; Go `func Test*`/`Benchmark*`; Rust functions under `#[test]`/`#[tokio::test]`; JUnit methods annotated `@Test`.
- A **symbol record**: `kind` ∈ {function, method, class, interface, module, test, test_suite}; `name`; `qualified_name` (enclosing symbols joined by `.` or ` > ` for tests); `signature` (source text from symbol start to the start of its body node, whitespace-collapsed, ≤160 chars); `start_line`, `end_line`, `start_byte`, `end_byte`; `body_hash` (blake3 of the symbol's byte range, 32 hex); `parent_id`.
- Symbols are computed per `(path, content_hash)` and cached; identical content is never reparsed.

### 3.3 Edit → symbol mapping (boundary completeness)
- For each edit with known pre and post snapshots (§4), compute changed line ranges using a line diff (Myers or patience; deterministic).
- Map each changed range to the **innermost complete enclosing symbol** in the post-version (for deletions, in the pre-version). Never emit a bare line range when an enclosing symbol exists.
- Top-level changes outside any symbol map to a synthetic region `(path, "<module>")`.
- Store the mapping in `edit_regions`.

### 3.4 Large-symbol handling
A symbol exceeding **80 lines** is never rendered or stored for display in full; its **skeleton** is: signature; direct child symbols' signatures; called identifiers (up to 12, from call-expression nodes, deduplicated, in first-occurrence order); changed-region line span(s); and a recovery pointer. Symbols ≤ 80 lines MAY be stored in full for later (warm-tier) use but are not injected in v0.2.

### 3.5 Failure location → symbol
For each parsed failure record with `file:line` (§5), resolve the enclosing test or function symbol in the file's current snapshot. If the file changed since the failure was observed, resolve against the snapshot current at failure time and mark the item `STALE` if that symbol's `body_hash` no longer exists in the latest version.

---

## 4. Content snapshots

- New table `blobs`: `hash TEXT PRIMARY KEY` (blake3, 32 hex), `size INTEGER`, `zstd BLOB` (zstd level 3), `created_ms INTEGER`. Insert with `INSERT OR IGNORE` (dedup).
- Snapshot capture points: `pre-tool-use` (edit tools), `post-tool-use` (edit tools), turn-end scans. Only files ≤ **256 KiB**; sensitive paths (v0.1 §9.2) are **never** snapshotted.
- Hot-path cost control: sync hooks store the raw bytes into the spool as a `blob_pending` record when compression would push the hook past **3 ms** of its budget; the reducer compresses and inserts. Otherwise the hook inserts directly within its existing transaction.
- `file_versions` gains `blob_hash TEXT NULL`.

---

## 5. Failure normalization & fingerprinting

### 5.1 Normalization pipeline (applied in order; pure function; golden-tested)
1. Strip ANSI CSI/OSC sequences and carriage-return overwrite artifacts.
2. Normalize line endings to `\n`.
3. Replace the project root prefix with `<ROOT>`, the home directory with `~`, and temp directories (`/tmp/…`, `/var/folders/…`, `%TEMP%`-style Windows paths) with `<TMP>`.
4. Replace: hex addresses `0x[0-9a-fA-F]{6,}` → `<ADDR>`; UUIDs → `<UUID>`; ISO-8601 and `HH:MM:SS(.fff)` timestamps → `<TS>`; durations (`\d+(\.\d+)?\s?(ms|s|sec|seconds|m|min)\b`) → `<DUR>`; PIDs/ports after `pid`/`port` keywords → `<N>`; standalone integers with ≥ 4 digits → `<N>`.
5. For fingerprinting only: replace `:line(:col)?` suffixes in stack frames with `:<L>` (display keeps real numbers).
6. Collapse runs of whitespace.

### 5.2 Recognizers (line-oriented, no external parsers)
Each recognizer consumes normalized output and emits zero or more **FailureRecords** `{runner, test_id?, file?, line?, error_class, message_head}` where `message_head` is the first meaningful message line (≤200 chars).
- **MUST support:** pytest (incl. `FAILED path::test - Error` summary and `E   ` lines), Python unittest, Jest, Vitest, Mocha, node:test/TAP, Go test (`--- FAIL:`), `go build`/`go vet`, cargo test/nextest (`test x ... FAILED`, `panicked at`), rustc/cargo build (`error[E####]`), RSpec, Minitest, PHPUnit, JUnit/Maven Surefire/Gradle, `dotnet test`, `tsc` (`file(l,c): error TS####`), ESLint, Ruff, mypy, pyright.
- **Generic fallback:** `path:line(:col)?: (error|Error|FAIL)…` lines; else one record from the tail with `error_class = "unrecognized"`.
- Each recognizer MUST ship with ≥ 3 real-output fixtures (collected from actual runner versions, versions noted), including one passing run and one mixed run.

### 5.3 Fingerprint
`fingerprint = blake3(runner ␟ test_id ␟ error_class ␟ normalized message_head)[0..12]`, displayed as `F-{first 4 hex}` in the capsule and full in CLI. Two runs of the same failing assertion with different timestamps, temp paths, or durations MUST produce the same fingerprint (fixture-tested).

### 5.4 Per-test outcome history & flakiness
- Table `test_outcomes`: `(command_id, test_id, outcome, fingerprint?)`.
- For each `(signature, test_id)`, compute over the last 10 runs: `flips_without_edit` = number of consecutive run pairs whose outcomes differ **and** between which no ACTIVE/REVERTED edit occurred to any file in the working set; `variability = flips_without_edit / max(1, runs − 1)`.
- `flaky_suspect = true` iff `flips_without_edit ≥ 1` and `variability ≥ 0.25`.
- Flips *with* edits between runs are treated as progress or regression, never as flakiness.

---

## 6. Churn & loop detection

All signals are computed by the reducer per `(session_id, epoch)` and stored in `health_signals` with their evidence (row ids). Thresholds are config keys with the defaults shown.

| Signal | Definition | Default threshold |
|---|---|---|
| **S1 Repeated failure** | Same non-empty set of failure fingerprints observed in ≥ N consecutive runs of the same signature, with ≥ 1 edit between each pair | N = 3 |
| **S2 Region oscillation** | For a region `(path, qualified_name)`, the sequence of `body_hash` values after each edit contains a value that reappears after a different value (A→B→A), at least K times within the last 10 edits to that region | K = 2 |
| **S3 Redundant rereads** | Read of a file whose `(mtime, size)` is unchanged since a previous Read in the same epoch (captured by `stat` in the sync hook, no content read) | ≥ 4 in the last 30 tool events |
| **S4 Context-pressure proxy** | `transcript_bytes / median(transcript_bytes at this project's previous auto-compactions)`; fallback absolute 2.5 MB when fewer than 2 prior auto-compactions; transcript size via `stat` only | ≥ 0.7 |
| **P1 Progress (negative evidence)** | Failing fingerprint set strictly shrank, or a previously failing signature passed, within the last 3 runs | any |
| **P2 Flakiness (negative evidence)** | Any fingerprint in S1's set belongs to a `flaky_suspect` test | any |

### 6.1 Health state
- `LOOPING` ⇔ S1 ∧ (S2 ∨ S3) ∧ ¬P1 ∧ ¬P2
- `DRAGGING` ⇔ ¬LOOPING ∧ S4 ∧ ¬P1 ∧ (S2 ∨ S3)
- `HEALTHY` otherwise.
Large, long, or expensive sessions with P1 MUST remain HEALTHY. Session size alone (S4) MUST NEVER produce any state other than HEALTHY.

### 6.2 Advisory notice
- Emitted as `{"systemMessage": "…"}` by the **sync `stop` hook**, which only reads the precomputed health row (no computation; v0.1 budget).
- Conditions: state is `LOOPING`, or `DRAGGING` for the first time in the epoch; not emitted if a notice was emitted in the last 30 minutes for this session, nor twice for the same `(state, fingerprint set)` episode.
- Copy (exact):
  - LOOPING: `⚡ Velra: same failure {F-xxxx} {n}x · {qualified_name}() changed back and forth · checkpoint ready — /compact continues cleanly with task state preserved. Details: velra why`
  - DRAGGING: `⚡ Velra: session is carrying a lot of history (≈{pct}% of your usual compaction size) · checkpoint ready — /compact when convenient. Details: velra why`
- Notices are advisory. Velra MUST NOT block, compact, stop, or modify the session. Before emitting, the stop hook refreshes nothing; if the health row is older than 60 s, it emits nothing.
- Config `notices = true|false` (default true).

### 6.3 `velra why`
Prints the health state, each signal with its evidence (runs, timestamps, fingerprints, region hash sequence as short hashes), and the negative evidence considered. Exit 0.

---

## 7. Provenance & staleness

- Every capsule item carries exactly one class:
  - `VERIFIED` — re-checked at render time against the current disk (file exists and hash matches the referenced version).
  - `OBSERVED` — directly recorded from a tool event, not re-checked.
  - `INFERRED` — produced by a named deterministic rule (e.g. next-target selection).
  - `UNCONFIRMED` — a co-occurrence (edit followed by result) with no established causality.
  - `STALE` — the referenced artifact changed since observation (hash or symbol `body_hash` no longer current).
- **Render-time freshness check** (inside the `pre-compact` 10 ms budget): for up to 12 referenced files, compare `(mtime, size)` with the last observation; if unchanged → eligible for `VERIFIED` when the hash was recorded; if changed → hash the file only if the remaining budget ≥ 2 ms, else mark `STALE`.
- Git-aware invalidation: if HEAD moved since the observation and the referenced file differs between the observed version and the current one, mark `STALE`.
- Inference is never silently promoted: an `INFERRED` or `UNCONFIRMED` item can become `VERIFIED` only through a new direct observation.

---

## 8. Schema additions (migration to `user_version = 2`)

```sql
CREATE TABLE blobs (hash TEXT PRIMARY KEY, size INTEGER NOT NULL, zstd BLOB NOT NULL, created_ms INTEGER NOT NULL) STRICT;
ALTER TABLE file_versions ADD COLUMN blob_hash TEXT;

CREATE TABLE symbols (
  id INTEGER PRIMARY KEY, path TEXT NOT NULL, content_hash TEXT NOT NULL, lang TEXT NOT NULL,
  kind TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT NOT NULL, signature TEXT NOT NULL,
  start_line INTEGER NOT NULL, end_line INTEGER NOT NULL, start_byte INTEGER NOT NULL, end_byte INTEGER NOT NULL,
  body_hash TEXT NOT NULL, parent_id INTEGER, skeleton_json TEXT
) STRICT;
CREATE INDEX symbols_by_version ON symbols(path, content_hash);
CREATE TABLE parse_status (path TEXT NOT NULL, content_hash TEXT NOT NULL, status TEXT NOT NULL, reason TEXT, PRIMARY KEY (path, content_hash)) STRICT;

CREATE TABLE edit_regions (
  edit_id INTEGER NOT NULL, path TEXT NOT NULL, qualified_name TEXT NOT NULL, symbol_kind TEXT NOT NULL,
  pre_body_hash TEXT, post_body_hash TEXT, changed_lines TEXT NOT NULL, PRIMARY KEY (edit_id, qualified_name)
) STRICT;

CREATE TABLE failures (
  id INTEGER PRIMARY KEY, command_id INTEGER NOT NULL, runner TEXT NOT NULL, test_id TEXT,
  file TEXT, line INTEGER, symbol_qualified_name TEXT, error_class TEXT NOT NULL,
  message_head TEXT NOT NULL, fingerprint TEXT NOT NULL
) STRICT;
CREATE INDEX failures_fp ON failures(fingerprint);
CREATE TABLE test_outcomes (command_id INTEGER NOT NULL, test_id TEXT NOT NULL, outcome TEXT NOT NULL, fingerprint TEXT, PRIMARY KEY (command_id, test_id)) STRICT;

CREATE TABLE health_signals (
  session_id TEXT NOT NULL, epoch INTEGER NOT NULL, computed_ms INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('HEALTHY','DRAGGING','LOOPING')),
  signals_json TEXT NOT NULL, last_notice_ms INTEGER, last_notice_episode TEXT,
  PRIMARY KEY (session_id, epoch)
) STRICT;
```
Migration MUST be reversible in the sense that a v0.1 binary encountering `user_version = 2` no-ops safely (already guaranteed by v0.1 rules). Backfill of symbols for existing data is NOT required.

---

## 9. Hook changes

| Hook | Change | Budget impact |
|---|---|---|
| `pre-tool-use` / `post-tool-use` (edit tools) | Add snapshot capture (§4) | stays within v0.1 budgets (spool fallback) |
| `post-tool-use` (Read) | Record `(mtime_ns, size)` via `stat` | ≤ 0.1 ms |
| `stop` (sync) | Record transcript size via `stat`; emit advisory notice from the precomputed health row | ≤ 5 ms p99 |
| `reduce` (async) | Snapshots → parse → symbols → edit regions → failure recognition → fingerprints → outcomes → health signals | ≤ 1,000 ms per pass; resumable across passes |
| `pre-compact` | Uses precomputed symbol/fingerprint data only; freshness check §7 | ≤ 10 ms p99 unchanged |

If the reducer has not yet processed structural data for an item, the capsule falls back to v0.1 file-level rendering for that item (never blocks waiting).

---

## 10. Capsule render_version 2

Changes relative to v1 (all other v1 rules unchanged; sections keep v1 order with `[ACTIVE_SYMBOLS]` inserted after `[ACTIVE_FAILURE]`):

```
[ACTIVE_FAILURE] ({class} | {kind} run | {HH:MM})
Command: {command}
Result: FAIL (exit {n}) | {k} failing | fingerprint {F-xxxx} seen {r}x{" | flaky suspect" if flaky}
Target: {test qualified_name} ({path}:{line})            -- boundary-complete, not a bare line
  {up to 6 excerpt lines}
[ACTIVE_SYMBOLS] ({class})
- {signature}  [{path}] {edited {e}x | in failure | changed lines {a}-{b}}
                                                          -- max 6; skeletons referenced, never expanded
[DEAD_ENDS] ({class})
- {qualified_name}() in {path} | {k} edit(s) | {mechanism phrase} at {HH:MM}
    - {minus line}
    + {plus line}
  Observed afterward: {fingerprint or outcome}. Causal link: UNCONFIRMED.
[HEALTH] (INFERRED | loop detector)                       -- only when LOOPING or DRAGGING
{state}: {signal names with counts}
```
- `ACTIVE_SYMBOLS` selection: symbols touched by ACTIVE edits in the epoch, plus the symbol enclosing the active failure target; ranked by (in failure, edits, recency); max 6.
- Region-level dead ends (partial reverts inside a symbol, detected via `edit_regions` body hashes) are now reported; file-level dead ends remain for unparsed files.
- Budget and truncation rules from v1 apply; `ACTIVE_SYMBOLS` truncates to 3 before `RECENT_ATTEMPTS` is touched; `HEALTH` is dropped first.

---

## 11. Acceptance tests (in addition to the full v0.1 suite)

### S. Structural
- S1 For each tier-1 language, fixtures with nested classes/functions/tests: extracted symbols, qualified names, signatures, and line/byte ranges match golden files.
- S2 Edit inside a method body maps to that method, not its class; edit spanning two functions maps to both; top-level edit maps to `<module>`.
- S3 A 300-line function renders as a skeleton (signature, children, ≤12 calls, changed span) and never in full.
- S4 Files > 1 MiB or with a forced parse timeout record `parse_skipped` and fall back to file level.
- S5 Grammar init is lazy: `hook post-tool-use` for a non-edit tool shows no measurable regression vs v0.1 (H1 budgets).
- S6 Binary size ≤ 30 MB on every target.

### N. Failure normalization
- N1 For every recognizer: fixture outputs produce the expected FailureRecords.
- N2 Same failure with different timestamps, durations, temp paths, and addresses → identical fingerprint; different assertion messages → different fingerprints.
- N3 Mixed pass/fail run → correct per-test outcomes.

### L. Loop detector
- L1 Scenario: 4 runs, same fingerprint, same function edited A→B→A→B → `LOOPING`, notice emitted once, `velra why` lists evidence.
- L2 Scenario: long session (transcript 5 MB), 12 runs, failing set shrinks to zero → `HEALTHY` throughout.
- L3 Scenario: test alternates fail/pass with no edits between runs → `flaky_suspect`, never `LOOPING`.
- L4 Scenario: only S4 high → `HEALTHY`.
- L5 Rate limit: repeated LOOPING in the same episode within 30 minutes → one notice.
- L6 Notice emitted by `stop` within v0.1 budget; stale (>60 s) health row → no notice.

### P. Provenance
- P1 Referenced file unchanged → `VERIFIED`; changed after observation → `STALE`; freshness check never exceeds the `pre-compact` budget (budget-starved case marks `STALE`).
- P2 Renderer never emits an item without a class tag; causal words remain absent (v0.1 E3).
- P3 Golden capsules (render_version 2) for ≥ 12 states, byte-identical across OSes.

### R. Regression
- R1 All v0.1 tests green; all v0.1 performance budgets met.
- R2 Upgrade test: a v0.1 database with live continuations migrates to v2 and the pending continuation is still delivered.

---

## 12. Milestones & Definition of Done
1. **M1** Snapshots + schema v2 + migration (R2).
2. **M2** Tree-sitter tier-1 + symbols + edit mapping + skeletons (S1–S6).
3. **M3** Normalization + recognizers + fingerprints + outcomes + flakiness (N1–N3).
4. **M4** Signals + health + notices + `velra why` (L1–L6).
5. **M5** Provenance + staleness + render_version 2 (P1–P3).

**Done** = all v0.1 and v0.2 tests pass on all platforms, dogfooding across ≥ 3 real repositories in ≥ 3 tier-1 languages records zero false `LOOPING` notices on flaky tests, and `DECISIONS.md` is updated.
