# BUILD_PROMPT_v0.1.md — Velra v0.1 "Lossless Compaction for Claude Code"

## 0. Instructions to the implementing agent

You are implementing **Velra v0.1** end-to-end in a new repository. This document is the complete specification. It states *what* must be true, not *how* to write the code.

- Keywords **MUST / MUST NOT / SHOULD / MAY** follow RFC 2119.
- Implement **only** what is in scope (§2). Do not add features, commands, dependencies on network services, or files outside this spec.
- When this spec is ambiguous, choose the option that (1) never blocks or visibly disturbs Claude Code, then (2) preserves data, then (3) is simplest. Record every such choice in `DECISIONS.md` with a one-line rationale.
- Claude Code evolves. Where this spec names a Claude Code version-dependent feature, you MUST confirm it against the official hooks reference (`https://code.claude.com/docs/en/hooks`) and changelog at build time and encode the result as constants in a single `compat` module.
- Work in milestones (§22). Each milestone ends with its acceptance tests passing.

---

## 1. Product context

Velra is a local-first infrastructure layer that keeps long-running AI coding sessions productive across context transitions. It observes the agent's work through lifecycle hooks, maintains a small evidence-backed task state in a local database, freezes that state at compaction, and delivers a compact **Continuation Capsule** into the post-compaction context so the agent keeps the task, the current failure, and the approaches already tried and discarded.

Velra is not an agent, not a chat UI, not a memory platform, and never calls an LLM. Mental model: *Git stores code continuity; Velra stores task continuity.*

v0.1 targets exactly one pain: **compaction amnesia in Claude Code**.

---

## 2. Scope

### In scope (v0.1)
1. Single static binary `velra` for macOS (arm64, x86_64), Linux (x86_64, aarch64; static musl), Windows (x86_64; aarch64 SHOULD).
2. One-liner installers and `velra enable` / `velra disable` that idempotently and atomically merge hook registrations into the user's Claude Code settings file, preserving comments and formatting.
3. Hook subcommands for the Claude Code events listed in §6.3.
4. Append-only SQLite (WAL) event log with a spool fallback; an incremental, cursor-based reducer scheduled via async hooks; a bounded reducer pass at `PreCompact`.
5. Root/subtask/latest-request intent tracking with explicit-boundary rules.
6. File version tracking via content hashes; revert and discard detection (dead ends) using hash returns and `git` restore-family commands.
7. Test/build/lint command outcome tracking with a minimal, deterministic failure excerpt.
8. Immutable checkpoints at `PreCompact`; continuation delivery state machine with idempotent injections; delivery via `SessionStart(compact)` with `PostToolUse` and `UserPromptSubmit` fallbacks.
9. Deterministic, zero-LLM `<VELRA_CONTINUATION>` capsule ≤800 estimated tokens.
10. CLI: `enable`, `disable`, `status`, `inspect`, `doctor`, `--version`, and hidden `hook <event>` / `reduce`.

### Out of scope (v0.1) — MUST NOT implement
AST/tree-sitter parsing; failure fingerprinting beyond §14; loop/churn detection; health notices; MCP server; warm start pack; `resume`, `history`, `export`; other agents; autonomous rotation; any network access at runtime; telemetry; any file written inside the user's repository; any daemon or OS service; any LLM call.

---

## 3. Tech stack specification & justification

### 3.1 Prescribed stack
| Concern | Choice |
|---|---|
| Language | **Rust**, stable toolchain, edition 2021 or later; MSRV pinned in `rust-toolchain.toml` |
| Storage | **SQLite** via `rusqlite` with the `bundled` feature (SQLite compiled into the binary; no system libsqlite) |
| JSON | `serde` + `serde_json` with `raw_value` (so large `tool_response` bodies are not fully materialized) |
| Settings editing | `jsonc-parser` with its CST feature (comment- and format-preserving edits) |
| Hashing | `blake3` (content identity, IDs) |
| IDs | ULID (`ulid` crate) for checkpoints and injections |
| Redaction | `aho-corasick` literal prefilter + lazily compiled `regex` set (compiled only on prefilter hit) |
| CLI parsing | `clap` for non-hook commands only; `hook` path dispatches manually from `argv` before `clap` initializes |
| Config | optional `~/.velra/config.toml` parsed with `toml` only if the file exists |
| Testing | `insta` (golden snapshots), `assert_cmd`, `tempfile`, `proptest` (state machine), `hyperfine` (benchmarks, CI only) |
| Distribution | `dist` (cargo-dist) for release CI, shell + PowerShell installers, checksums, Homebrew formula, npm wrapper; `cargo-zigbuild` for Linux musl cross builds |

### 3.2 Justification against the non-negotiable criteria
1. **Zero runtime prerequisites:** a Rust binary links everything statically (musl on Linux, static CRT where feasible on Windows); SQLite is compiled in. The host needs nothing installed.
2. **Instant cold start:** no VM, no GC, no JIT, no interpreter. Process start is dominated by the OS loader (~1 ms on macOS/Linux). The hook path MUST NOT initialize an async runtime, a CLI parser, a logger that touches disk on success, or any global regex.
3. **Single static binary:** `dist` produces per-target archives from one CI workflow.
4. **Concurrent embedded storage:** SQLite WAL gives many readers + one writer without blocking readers; the design (§10) keeps write transactions tiny so hook processes rarely wait, with a spool file as a no-loss fallback.
5. **Future-proof for v0.2/v0.3:** tree-sitter (C library) has first-class Rust bindings; the official Rust MCP SDK exists; both compile statically.

Disqualified alternatives (document in README "Why Rust"): Node/Bun/Deno (runtime or large single-file binaries with 10–30 ms start), Python (runtime + environment), JVM (runtime + start), Go (tree-sitter and fast SQLite need cgo, which complicates cross-compilation), Zig (insufficient ecosystem for JSONC CST and MCP).

### 3.3 Build profile
Release profile MUST use: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "unwind"` (required so the hook wrapper can catch panics and exit 0). Target binary size ≤ 8 MB per platform.

### 3.4 Repository layout (required top-level)
`crates/velra` (binary), `crates/velra-core` (state, reducer, renderer; no I/O to Claude Code), `tests/fixtures/claude-code/<version>/` (recorded hook payloads), `tests/golden/`, `bench/`, `install/`, `DECISIONS.md`, `README.md`, `CHANGELOG.md`, `SECURITY.md`, `LICENSE` (MIT or Apache-2.0).

---

## 4. Performance budgets

Measured as full process wall time (spawn → exit) with `hyperfine --warmup 20 --runs 500`, DB pre-populated with 100,000 events, on reference machines (Apple Silicon M1 or later; GitHub-hosted `ubuntu-latest` x86_64). CI MUST fail on regression beyond budget.

| Invocation | p50 | p99 |
|---|---|---|
| `hook post-tool-use` (2 KiB payload, non-edit tool) | ≤ 2 ms | ≤ 5 ms |
| `hook post-tool-use` (Edit of a 50 KiB file, includes hashing) | ≤ 3 ms | ≤ 6 ms |
| `hook pre-tool-use` (edit tool, 50 KiB file) | ≤ 3 ms | ≤ 6 ms |
| `hook user-prompt-submit` (no pending continuation) | ≤ 2 ms | ≤ 4 ms |
| `hook user-prompt-submit` / `session-start` (delivering) | ≤ 3 ms | ≤ 6 ms |
| `hook pre-compact` (≤ 200 unreduced events) | ≤ 6 ms | ≤ 10 ms |
| `hook post-tool-use` with 8 MiB `tool_response` | — | ≤ 25 ms |

Windows: same measurements, target p99 ≤ 15 ms wall for all sync hooks; documented as OS spawn overhead.

Internal watchdog: every sync hook MUST abandon work and exit 0 with empty stdout if its own wall time reaches **250 ms**; async `reduce` at **1,000 ms**.

---

## 5. Installation & distribution

### 5.1 Channels
1. **Primary (macOS/Linux):** `curl -LsSf https://{{VELRA_DOMAIN}}/install.sh | sh`
2. **Primary (Windows):** `powershell -ExecutionPolicy Bypass -c "irm https://{{VELRA_DOMAIN}}/install.ps1 | iex"`
3. **Homebrew:** `brew install {{GITHUB_ORG}}/tap/velra`
4. **Rust users:** `cargo binstall velra` (prebuilt) and `cargo install velra` (source)
5. **npm (convenience only):** `npm i -g velra` — platform binaries via `optionalDependencies`; MUST NOT use `postinstall` scripts; runtime never needs Node.

### 5.2 Installer requirements
- Detect OS/arch; download the matching archive from GitHub Releases; verify SHA-256 against the published checksum file; abort on mismatch with a clear message.
- Install to `~/.velra/bin/velra` (`%USERPROFILE%\.velra\bin\velra.exe`), replacing any existing binary atomically (write temp + rename).
- Add `~/.velra/bin` to PATH idempotently (single marked line in the detected shell rc; Windows user PATH), unless `VELRA_NO_MODIFY_PATH=1`.
- MUST NOT require sudo/admin. MUST NOT run `velra enable` unless invoked with `--enable` (e.g. `… | sh -s -- --enable`).
- Final lines printed:
  ```
  ✓ velra {version} installed to ~/.velra/bin/velra
  Next: velra enable
  ```
- Releases MUST publish build provenance attestations (GitHub artifact attestations).

### 5.3 Stable hook path
Hooks MUST reference a path that survives upgrades. `velra enable` resolves the binary path as follows: if the running executable's canonical path contains a versioned package-manager directory (e.g. Homebrew `Cellar/`, `/nix/store/`, a cargo registry path), use the PATH entry that resolves to it *without* following symlinks (e.g. `/opt/homebrew/bin/velra`); otherwise use the canonical path. Store the chosen path in `~/.velra/state.json` for `doctor`.

---

## 6. Zero-touch configuration: `velra enable` / `velra disable`

### 6.1 Settings file resolution
Target = `$CLAUDE_CONFIG_DIR/settings.json` if `CLAUDE_CONFIG_DIR` is set, else `~/.claude/settings.json`. If the target is a symlink, edit the symlink's target file (dotfile managers). Velra MUST NOT touch project-level `.claude/settings*.json` or any file in a repository.

### 6.2 `velra enable` algorithm (behavioral requirements)
1. If `~/.claude` does not exist: create it, create `settings.json`, print `! Claude Code not detected; hooks registered and will activate when it is installed.`
2. Read original bytes. Parse as JSONC via CST. On parse failure: print `✗ Could not parse {path} at line {l}, column {c}: {msg}. No changes made.` and exit 1.
3. Write a backup of the original bytes to `~/.velra/backups/settings.json.{UTC yyyymmddTHHMMSSZ}.bak`, fsync it; keep the 10 most recent backups.
4. Detect the installed Claude Code version via `claude --version` (timeout 3 s; absent → assume latest known). Using `compat` constants, choose exec form (`command` + `args`) if supported, else shell form with the absolute path double-quoted. Omit the `if` field if unsupported (hooks then self-filter in-process).
5. For each registration in §6.3: find an existing **Velra handler** = a handler whose `command` basename (case-insensitive, `.exe` stripped) is `velra` and whose first argument (exec form) or command string (shell form) invokes `hook` or `reduce`. If an identical handler exists → no change. If a Velra handler exists for the same event+role but differs → update it in place. Otherwise append a new matcher-group object to the end of `hooks.<Event>` (creating `hooks` and the array if absent).
6. MUST NOT modify, reorder, or reformat any non-Velra key, handler, or group. New nodes MUST use the file's detected indentation (2 spaces, 4 spaces, or tab).
7. Re-parse the result; verify semantic equality of everything except Velra handlers. On failure: no write; exit 1.
8. Atomic write: temp file in the same directory, same permissions, fsync, rename; fsync the directory on POSIX. Immediately before rename, re-read the target; if its bytes changed since step 2 (concurrent writer), restart from step 2, max 3 attempts.
9. If the effective settings contain `"disableAllHooks": true`, print a warning that hooks are globally disabled.
10. Print:
    ```
    ✓ Velra enabled for Claude Code.
      Settings: {path}  (backup: {backup_path})

    Nothing else required. Keep coding normally.
    Tip: run `velra inspect` any time to see what would survive a /compact.
    ```
- `--dry-run`: print a unified diff of the would-be change; write nothing.
- Running `enable` twice MUST produce byte-identical files and print `✓ Velra already enabled.`

### 6.3 Hook registrations (v0.1)
`{BIN}` = stable path from §5.3. Timeouts are Claude Code's outer safety net (seconds); Velra's internal watchdog (§4) is the real bound.

| Event | Matcher | `if` | Args | async | timeout |
|---|---|---|---|---|---|
| `SessionStart` | *(omitted = all sources)* | — | `hook session-start` | no | 10 |
| `UserPromptSubmit` | — | — | `hook user-prompt-submit` | no | 10 |
| `PreToolUse` | `Write\|Edit\|MultiEdit\|NotebookEdit` | — | `hook pre-tool-use` | no | 10 |
| `PreToolUse` | `Bash` | `Bash(git *)` | `hook pre-tool-use` | no | 10 |
| `PreToolUse` | `PowerShell` | `PowerShell(git *)` | `hook pre-tool-use` | no | 10 |
| `PostToolUse` | `*` | — | `hook post-tool-use` | no | 10 |
| `PostToolUseFailure` | `*` | — | `hook post-tool-use-failure` | no | 10 |
| `PostToolBatch` | — | — | `reduce` | **yes** | 30 |
| `Stop` | — | — | `hook stop` | no | 10 |
| `Stop` | — | — | `reduce` | **yes** | 30 |
| `PreCompact` | *(all)* | — | `hook pre-compact` | no | 10 |
| `PostCompact` | *(all)* | — | `hook post-compact` | **yes** | 30 |
| `SessionEnd` | *(all)* | — | `hook session-end` | no | 1 |

Events not supported by the detected Claude Code version (per `compat`) MUST be skipped with a note in `enable` output; the system MUST function (degraded) without `PostToolBatch`, `PostCompact`, and `PostToolUseFailure`.

### 6.4 `velra disable`
Removes exactly the Velra handlers; removes a matcher group only if it contains no remaining handlers; removes a `hooks.<Event>` key only if its array becomes empty; removes `hooks` only if it becomes empty **and** it did not exist before `enable` (recorded in `~/.velra/state.json`). Same backup + atomic write rules. For a settings file that had no Velra entries before `enable`, `enable` then `disable` MUST yield a byte-identical file. `--purge` additionally deletes `~/.velra/` except `backups/` after confirmation (`--yes` skips the prompt).

---

## 7. CLI surface (v0.1)

| Command | Behavior |
|---|---|
| `velra enable [--dry-run]` | §6.2 |
| `velra disable [--purge] [--yes]` | §6.4 |
| `velra status` | Enabled? (hooks present and path executable), Claude Code version, DB path/size, sessions tracked, last event age, live continuations and their states. Exit 0 if healthy, 1 otherwise |
| `velra inspect [--session <id> \| --last] [--checkpoint <id>] [--section <name>] [--json]` | Without `--checkpoint`: runs a reducer pass and renders the capsule for the most recently active session of the current directory's project **without** creating a checkpoint. With `--checkpoint`: prints that frozen capsule. `--section dead-ends\|failure\|files\|attempts` prints full untruncated detail for that section |
| `velra doctor` | Checks: binary path in settings matches an executable; settings parse; hooks present for each event; DB opens, WAL mode, schema version; spool backlog; home on a network filesystem (warn: WAL unsupported); last hook event within the most recent Claude Code session (if none: "hooks may be disabled by `disableAllHooks` or managed policy"); recent entries in `errors.log`. Prints one line per check with ✓/!/✗ |
| `velra --version` | `velra {semver} ({git sha}, {target})` |
| `velra hook <event>` | Hidden. §8 |
| `velra reduce` | Hidden. §11 |

Color output only when stdout is a TTY and `NO_COLOR` is unset.

---

## 8. Hook IPC contracts

### 8.1 Universal rules for every `hook` / `reduce` invocation
1. **Exit code is always 0.** Velra MUST NEVER exit 2 (blocking on several events, e.g. `PreCompact` exit 2 blocks compaction and `UserPromptSubmit` exit 2 erases the prompt).
2. **stdout is either empty or exactly one JSON object** on a single line followed by `\n`. Never plain text (on `SessionStart` and `UserPromptSubmit`, plain text becomes model context). Nothing else may write to stdout; logging goes to files only.
3. **stderr is empty** on success. On internal error, write nothing to stderr (Claude Code may surface it); log to `~/.velra/logs/errors.log` instead.
4. **stdin:** read to EOF with a hard cap of 64 MiB; beyond the cap, keep draining and discarding (never leave the writer blocked). Parse tolerantly: unknown fields ignored; missing fields degrade the event, never fail it. `tool_response` MUST be kept as raw JSON and only the fields Velra needs extracted.
5. **Kill switch:** if env `VELRA_DISABLE=1` or file `~/.velra/disabled` exists → exit 0 immediately, empty stdout.
6. **Panics:** caught at the top level → exit 0, empty stdout, logged.
7. **Watchdog:** §4.
8. **Time:** capture `ts_ms` (Unix epoch ms) at process start; it is the event's timestamp.

### 8.2 Common input fields consumed
`session_id` (required; if absent → exit 0, no-op), `hook_event_name`, `cwd`, `transcript_path`, `prompt_id` (optional), `agent_id` / `agent_type` (optional; marks subagent events), `permission_mode` (ignored).
**Project root:** env `CLAUDE_PROJECT_DIR` if set, else walk up from `cwd` to the first directory containing `.git` (file or directory), else `cwd`. `project_id` = first 16 hex chars of blake3(canonical root path, case-folded on Windows).

### 8.3 Event-specific input consumed and output produced

| Subcommand | Input fields consumed (tolerant) | Work | stdout |
|---|---|---|---|
| `session-start` | `source` ∈ {startup, resume, clear, compact, fork}; `model` (opt) | Append event. If `source ∈ {compact, resume}`: attempt delivery (§15, channel `session_start`). If `source = clear`: expire live continuation for the session | Delivery JSON (§8.4) or empty |
| `user-prompt-submit` | `prompt`, `prompt_id` | Append event (prompt redacted, ≤4 KiB). Run reconcile (§15.4). Attempt delivery (channel `user_prompt`) | Delivery JSON or empty |
| `pre-tool-use` | `tool_name`, `tool_input.file_path \| path \| notebook_path`; for Bash/PowerShell `tool_input.command` | Edit tools: hash the target file (§13.1) → append `pre_edit` observation. Shell: if command contains a git restore-family subcommand (§13.3) → hash every file edited in this session's current epoch (≤64 files, read via one indexed query) → append `git_pre` observation; otherwise exit immediately | Always empty |
| `post-tool-use` | `tool_name`, `tool_input`, `tool_use_id`, `tool_response` (or `tool_output`) | Normalize per §9 → append. Edit tools: hash file post-edit. Shell git restore-family: rehash tracked files. Then reconcile and attempt delivery (channel `post_tool`) only if a PENDING continuation exists for the session | Delivery JSON or empty |
| `post-tool-use-failure` | `tool_name`, `tool_input`, `tool_use_id`, `error` \| `error_message`, `error_type`, `is_interrupt` | Append failure event | Always empty |
| `stop` | `stop_hook_active` | Append turn-end event (confirmation evidence) | Always empty |
| `pre-compact` | `trigger` ∈ {manual, auto}, `custom_instructions` (presence only) | Barrier: §15.2 | `{"systemMessage": "⚡ Velra checkpoint saved: {summary}"}` or empty if nothing worth saving |
| `post-compact` | `trigger`, `compact_summary` \| `summary` | Record native summary (redacted, ≤32 KiB) linked to the checkpoint | Always empty |
| `session-end` | `reason` | Append event. If `reason ∈ {clear, logout}` → expire live continuation | Always empty |
| `reduce` (async) | any common input | §11 reducer pass | Always empty |

### 8.4 Delivery output (exact shape)
```json
{"hookSpecificOutput":{"hookEventName":"<SessionStart|UserPromptSubmit|PostToolUse>","additionalContext":"<capsule text>"},"systemMessage":"⚡ Velra restored: {summary} ({n} tokens)"}
```
`{summary}` = comma-joined non-zero items from: `objective`, `{k} failing test(s)`, `{k} dead end(s)`, `{k} files`. The capsule string MUST be ≤ 9,500 characters (Claude Code spills hook output over 10,000 characters to a file).

---

## 9. Event normalization, retention & redaction

### 9.1 Retained payload per event (after redaction; total ≤ 16 KiB except PostCompact ≤ 48 KiB)
| Event / tool | Retained fields |
|---|---|
| UserPromptSubmit | `prompt` (≤4 KiB), `prompt_id` |
| Pre/Post edit tools (`Write`, `Edit`, `MultiEdit`, `NotebookEdit`) | `path` (project-relative, `/` separators; absolute if outside root), `pre_hash`/`post_hash`, `size`, `lines_added`, `lines_removed` (from `old_string`/`new_string` line counts; for `Write`: added = new line count, removed = null), `excerpt` (≤3 lines: first changed `-` line and first `+` line, each ≤160 chars; none for Write), `original_hash` (if `tool_response.originalFile` present) |
| Read | `path`, `offset`/`limit` if present |
| Grep / Glob | `pattern` (≤200 chars), `path` |
| Bash / PowerShell | `command` (≤2 KiB), `exit_code` (from `tool_response.exit_code \| exitCode \| returnCode` if present), `interrupted`, `stdout_tail`, `stderr_tail` (last 8 KiB each, ANSI stripped), git observations if any |
| PostToolUseFailure | `tool_name`, `error` (≤4 KiB), `error_type`, `is_interrupt`, plus the tool's retained input fields |
| Other tools (incl. `mcp__*`) | `tool_name` only |
| SessionStart / SessionEnd / Stop / PreCompact | the consumed fields listed in §8.3 |
| PostCompact | `summary` (≤32 KiB) |

### 9.2 Redaction (applied before any write, including spool)
- Replace matches with `[REDACTED:{kind}]`. Minimum detector set: AWS access key IDs and secret-key assignments; GitHub tokens (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`); `sk-` / `sk-ant-` style API keys; Slack tokens (`xox[abprs]-`); Google API keys (`AIza…`); Stripe keys (`sk_live_`, `rk_live_`); JWTs (`eyJ…\.eyJ…\.…`); PEM private key blocks; URLs with embedded credentials (`scheme://user:pass@`); generic assignments `(api[_-]?key|secret|token|passw(or)?d|auth)\s*[:=]\s*\S{8,}` (case-insensitive).
- **Sensitive paths** (glob, case-insensitive): `.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`, `id_ed25519*`, `.npmrc`, `.pypirc`, `.netrc`, `*credentials*`, `*secret*`, `.ssh/**`, `.aws/**`, `*.kdbx`. For these: store path + hash only; no excerpts; never displayed beyond the path.
- Redaction MUST cost ≤ 1 ms p99 on 16 KiB input; regexes compile only when the literal prefilter hits.

---

## 10. Storage: layout, concurrency & schema

### 10.1 Filesystem layout
`$VELRA_HOME` (default `~/.velra`, Windows `%USERPROFILE%\.velra`): `bin/`, `velra.db` (+ `-wal`, `-shm`), `spool/`, `logs/errors.log` (rotated at 1 MiB, 3 files), `backups/`, `state.json`, `config.toml` (optional), `disabled` (optional). Directory mode 0700, files 0600 on POSIX.

### 10.2 Connection configuration
- At DB creation only: `PRAGMA journal_mode = WAL;` (persistent — MUST NOT be re-issued per connection).
- Every connection: `PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = OFF;` and `busy_timeout` by role:

| Role | busy_timeout | On SQLITE_BUSY after timeout |
|---|---|---|
| Sync hook append | 100 ms | write event to spool; exit 0 |
| Sync hook delivery (read + conditional update) | 100 ms | skip delivery this time (continuation stays PENDING); exit 0 |
| `pre-compact` | 50 ms per statement | render from what is readable; checkpoint `partial = 1`; if insert impossible, spool a `checkpoint_request` event |
| `reduce` | 1,000 ms | exit 0 (next trigger will retry) |
| CLI | 5,000 ms | error message |

- `PRAGMA journal_size_limit = 67108864`; default auto-checkpoint.
- Write transactions in hooks MUST be `BEGIN IMMEDIATE`, contain ≤ 3 statements, and commit within 2 ms.

### 10.3 Spool (no-loss fallback)
One file per event: `spool/{ts_ms}-{pid}-{rand4}.jsonl`, created with create-new semantics, single write, fsync not required. Content = the exact normalized event row as JSON. The reducer ingests spool files in filename order (inserting with their original `ts_ms` and `dedupe_key`), then deletes them. `doctor` warns if > 1,000 spool files exist.

### 10.4 Schema (schema version 1; `PRAGMA user_version = 1`)
All tables `STRICT`. Timestamps are Unix ms `INTEGER`. Paths use `/` separators.

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;

CREATE TABLE events (
  id            INTEGER PRIMARY KEY,
  dedupe_key    TEXT NOT NULL UNIQUE,  -- blake3(hook_event|session_id|tool_use_id|prompt_id|ts_ms|agent_id)[0..32]
  session_id    TEXT NOT NULL,
  project_id    TEXT NOT NULL,
  agent_id      TEXT,
  hook_event    TEXT NOT NULL,
  tool_name     TEXT,
  tool_use_id   TEXT,
  ts_ms         INTEGER NOT NULL,
  payload       TEXT NOT NULL          -- normalized, redacted JSON (§9.1)
) STRICT;
CREATE INDEX events_session_ts ON events(session_id, ts_ms);

CREATE TABLE projects (project_id TEXT PRIMARY KEY, root_path TEXT NOT NULL, is_git INTEGER NOT NULL, created_ms INTEGER NOT NULL) STRICT;

CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, started_ms INTEGER NOT NULL,
  last_event_ms INTEGER NOT NULL, transcript_path TEXT, epoch INTEGER NOT NULL DEFAULT 1,
  ended_ms INTEGER, end_reason TEXT
) STRICT;

CREATE TABLE reducer_cursor (id INTEGER PRIMARY KEY CHECK (id = 1), last_event_id INTEGER NOT NULL) STRICT;

CREATE TABLE intents (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL,
  level TEXT NOT NULL CHECK (level IN ('ROOT','SUBTASK','LATEST')),
  text TEXT NOT NULL, source_event_id INTEGER NOT NULL, created_ms INTEGER NOT NULL,
  superseded_ms INTEGER
) STRICT;
CREATE INDEX intents_live ON intents(session_id, epoch, level) WHERE superseded_ms IS NULL;

CREATE TABLE file_versions (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, path TEXT NOT NULL,
  content_hash TEXT NOT NULL, size INTEGER NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('pre_edit','post_edit','original','git_pre','git_post','turn_scan')),
  event_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX file_versions_path ON file_versions(session_id, path, id);

CREATE TABLE edits (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL,
  event_id INTEGER NOT NULL UNIQUE, agent_id TEXT, path TEXT NOT NULL, tool_name TEXT NOT NULL,
  pre_hash TEXT, post_hash TEXT NOT NULL, lines_added INTEGER, lines_removed INTEGER,
  excerpt TEXT, ts_ms INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('ACTIVE','REVERTED','DISCARDED','COMMITTED','REAPPLIED')),
  resolved_event_id INTEGER, mechanism TEXT CHECK (mechanism IN ('git_command','inverse_edit','rewrite','external'))
) STRICT;

CREATE TABLE dead_ends (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL, path TEXT NOT NULL,
  edit_ids TEXT NOT NULL,              -- JSON array
  mechanism TEXT NOT NULL, command_text TEXT, resolved_ms INTEGER NOT NULL,
  observed_after_command_id INTEGER,   -- first test/build run between first edit and revert
  reapplied INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE commands (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, epoch INTEGER NOT NULL, event_id INTEGER NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('test','build','lint','git','other')),
  signature TEXT NOT NULL, command_text TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK (outcome IN ('PASS','FAIL','INTERRUPTED','UNKNOWN')),
  exit_code INTEGER, excerpt TEXT, mentioned_paths TEXT, ts_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE file_stats (
  session_id TEXT NOT NULL, epoch INTEGER NOT NULL, path TEXT NOT NULL,
  reads INTEGER NOT NULL DEFAULT 0, edits INTEGER NOT NULL DEFAULT 0,
  in_failure INTEGER NOT NULL DEFAULT 0, last_touch_ms INTEGER NOT NULL,
  PRIMARY KEY (session_id, epoch, path)
) STRICT;

CREATE TABLE checkpoints (
  checkpoint_id TEXT PRIMARY KEY,      -- "ckpt_" + ULID
  session_id TEXT NOT NULL, project_id TEXT NOT NULL, epoch INTEGER NOT NULL,
  created_ms INTEGER NOT NULL, trigger TEXT NOT NULL CHECK (trigger IN ('manual','auto','cli')),
  head_commit TEXT, branch TEXT, event_watermark INTEGER NOT NULL, partial INTEGER NOT NULL,
  render_version INTEGER NOT NULL, capsule TEXT NOT NULL, capsule_tokens_est INTEGER NOT NULL,
  summary_json TEXT NOT NULL
) STRICT;
CREATE TRIGGER checkpoints_immutable BEFORE UPDATE ON checkpoints
BEGIN SELECT RAISE(ABORT, 'checkpoints are immutable'); END;

CREATE TABLE continuations (
  checkpoint_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('PENDING','ATTACHED','CONFIRMED','SUPERSEDED','EXPIRED')),
  attach_count INTEGER NOT NULL DEFAULT 0, attached_ms INTEGER, attached_channel TEXT,
  confirmed_ms INTEGER, confirm_event_id INTEGER, updated_ms INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX one_live_continuation ON continuations(session_id)
  WHERE state IN ('PENDING','ATTACHED');

CREATE TABLE injections (
  injection_id TEXT PRIMARY KEY,       -- "{checkpoint_id}:inj:{n}"
  checkpoint_id TEXT NOT NULL, delivery_key TEXT NOT NULL UNIQUE,
  channel TEXT NOT NULL CHECK (channel IN ('session_start','post_tool','user_prompt')),
  ts_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE compactions (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, checkpoint_id TEXT,
  trigger TEXT, pre_ms INTEGER, post_ms INTEGER, native_summary TEXT
) STRICT;
```

### 10.5 Migrations
Migrations are forward-only, transactional, keyed by `user_version`. CLI commands run pending migrations. A hook that sees `user_version` lower than expected MAY migrate only if it obtains an exclusive lock within 200 ms; otherwise it spools. A hook that sees a **higher** `user_version` than it knows MUST no-op (exit 0) and log once per hour. On `SQLITE_CORRUPT`/`SQLITE_NOTADB`: rename the DB to `velra.db.corrupt-{ts}`, recreate, log; never block.

---

## 11. Reducer

- **Purpose:** turn raw `events` (and spool files) into the materialized tables (`sessions`, `intents`, `file_versions`, `edits`, `dead_ends`, `commands`, `file_stats`).
- **Triggers:** async `reduce` hook (`PostToolBatch`, `Stop`), the bounded pass inside `pre-compact`, `velra inspect`, `velra status`.
- **Correctness:** each batch is one `BEGIN IMMEDIATE` transaction that reads `reducer_cursor`, processes up to **200** events with `id > last_event_id` in id order, writes derived rows, advances the cursor, commits. Concurrent reducers are therefore safe by construction; no lease is required for correctness. A reducer MAY skip work if it observes another reducer committed within the last 50 ms.
- **Idempotency:** re-processing an event (e.g. after a crash mid-batch, which rolls back) MUST produce identical derived state. Derived tables use `event_id UNIQUE` where applicable.
- **Spool ingestion:** occurs at the start of every reducer pass, before processing events.
- **Turn-end scan:** on a `Stop` event, the reducer rehashes every file with an ACTIVE edit in the current epoch (≤64 files, ≤4 MiB each) and records `turn_scan` versions when hashes changed (catches edits by `sed`, formatters, or the user's editor).
- **Purity:** event → derived-state logic lives in `velra-core` and is unit-testable without SQLite.

---

## 12. Intent hierarchy

State per `(session_id, epoch)`: at most one live `ROOT`, one live `SUBTASK`, one live `LATEST`.

Rules (applied by the reducer to `UserPromptSubmit` events, in order):
1. Normalize: trim; collapse whitespace.
2. Prompts starting with `/` (slash commands) never set any intent.
3. Prefix `task:` (case-insensitive) → **new epoch**: increment `sessions.epoch`; the remainder becomes ROOT; SUBTASK and LATEST cleared.
4. Prefix `subtask:` → remainder replaces SUBTASK (old version superseded, retained).
5. If no live ROOT exists in the epoch and the prompt is ≥ 20 characters → it becomes ROOT.
6. Every prompt that sets no ROOT/SUBTASK and is ≥ 3 characters replaces LATEST.
7. `SessionStart(source = clear)` → new epoch (ROOT unset until rule 5 or 3 applies).
8. `SessionStart(source ∈ {compact, resume})` never changes intents.
9. Intents are never deleted; superseded rows keep `superseded_ms`.

Ordinary follow-up prompts ("why did that fail?") MUST NOT replace ROOT.

---

## 13. File tracking, reverts & dead ends

### 13.1 Hashing
`content_hash` = blake3 of file bytes, first 32 hex chars. Files > 4 MiB: `large:{size}:{mtime_ns}`. Missing file: `absent`. Unreadable: `unreadable`. Hashing happens in `pre-tool-use` (pre_edit), `post-tool-use` (post_edit, git_post), and reducer turn-end scans.

### 13.2 Revert detection (hash return)
For each `(session_id, path)`, let the ordered version sequence be V₀…Vₙ. When a new observation Vₖ equals some Vⱼ with j < k−1 and at least one ACTIVE edit's `post_hash` is among V₍ⱼ₊₁₎…V₍ₖ₋₁₎:
- those edits → `REVERTED`, `mechanism` from the observation that produced Vₖ: `git_post` → `git_command`; an Edit whose new_string/old_string are the inverse of an earlier edit, or any post_edit → `inverse_edit`; Write → `rewrite`; `turn_scan` → `external`.
- one `dead_ends` row groups those edits.

### 13.3 Discard detection (git restore family)
Shell commands are split into subcommands on `&&`, `||`, `;`, `|`, newlines. A subcommand is restore-family if it matches: `git restore …`, `git checkout -- …`, `git checkout <path>` (argument resolves to a tracked file path), `git checkout .`, `git reset --hard …`, `git stash` (push/save/no subcommand), `git clean …`. For each file with ACTIVE edits whose `git_post` hash differs from its last `post_edit` hash: edits since the last COMMITTED marker on that file → `DISCARDED`, `mechanism = git_command`, `command_text` recorded; one `dead_ends` row.
`git commit …` (successful) → ACTIVE edits whose `post_hash` equals the file's current hash → `COMMITTED` (never dead ends).

### 13.4 Reapplication
If after a dead end the file's hash returns to a hash produced by one of that dead end's edits, set `dead_ends.reapplied = 1` and the edits' status `REAPPLIED`; such dead ends are excluded from the capsule.

### 13.5 Observed-after linkage
`observed_after_command_id` = the first `commands` row of kind `test` or `build` with `ts_ms` between the dead end's first edit and its resolution. Rendered as an observation, never as a cause.

### 13.6 Subagents
Events with `agent_id` are tracked identically and rendered with the suffix `(subagent)`.

### 13.7 Git metadata without spawning git
HEAD commit and branch are resolved by reading `.git/HEAD`, loose refs, and `packed-refs` directly; `.git` files (`gitdir:` worktrees) MUST be followed. No `git` subprocess is ever spawned by hooks.

---

## 14. Command & test tracking

- **Kind classification** (per subcommand, first match wins):
  - `test`: `pytest`, `py.test`, `python -m pytest|unittest`, `jest`, `vitest`, `mocha`, `(npm|pnpm|yarn|bun) (run )?test`, `bun test`, `deno test`, `node --test`, `cargo (test|nextest)`, `go test`, `rspec`, `bundle exec rspec`, `rails test`, `phpunit`, `dotnet test`, `mvn … test`, `(./)?gradlew? … test`, `mix test`, `swift test`, `ctest`, `make test`, `tox`, `nox`.
  - `build`: `tsc`, `cargo (build|check)`, `go (build|vet)`, `(npm|pnpm|yarn|bun) (run )?build`, `mvn … (compile|package)`, `gradle … build`, `dotnet build`, `make` (no target or `all`/`build`), `swift build`.
  - `lint`: `eslint`, `ruff`, `flake8`, `mypy`, `pyright`, `cargo clippy`, `golangci-lint`, `rubocop`, `biome`, `prettier --check`.
  - `git`: any `git …`. Otherwise `other`.
- **Signature:** kind + normalized command (collapse whitespace, strip env assignments, strip `--color*`/`--reporter*` flags).
- **Outcome:** `INTERRUPTED` if `interrupted`/`is_interrupt`; `FAIL` if the event is `PostToolUseFailure`, or a present exit code ≠ 0, or (kind ∈ {test, build, lint} and the output tail matches a runner failure marker: `FAILED`, `FAIL `, `failed`, `--- FAIL`, `test result: FAILED`, `error[`, `error TS`, `Error:`, `Traceback`, `✗`, `✕`, `ERRORS`); `PASS` if kind ∈ {test, build, lint}, no failure marker, and a pass marker (`passed`, `PASS`, `ok`, `test result: ok`, `0 failed`, `Tests: .* passed`) or exit code 0; else `UNKNOWN`.
- **Excerpt (FAIL only):** strip ANSI; redact; select up to 8 lines matching `FAIL|Error|error:|Expected|Received|expected|assert|panicked|AssertionError|^E  |Traceback` preferring the last occurrences; if none, last 6 non-empty lines. Each line ≤ 200 chars. Deterministic order = original order.
- **Mentioned paths:** tokens matching `path(:line(:col)?)?` that resolve to existing files under the project root; stored as JSON; set `file_stats.in_failure = 1` for them.
- Commands of kind `other` are stored but never rendered as failures.

---

## 15. Checkpoints & continuation delivery (two-phase commit)

### 15.1 States
`PENDING` (waiting for delivery) → `ATTACHED` (emitted to Claude Code at least once) → `CONFIRMED` (evidence the session progressed after attachment). Terminal: `SUPERSEDED`, `EXPIRED`. The partial unique index guarantees at most one live (PENDING/ATTACHED) continuation per session.

### 15.2 `pre-compact` barrier (≤10 ms p99)
1. Append the PreCompact event.
2. Run a reducer pass with a **6 ms** deadline (batches of ≤200). If not caught up → `partial = 1`.
3. MUST NOT read the transcript. MUST NOT spawn processes. MUST NOT perform network I/O.
4. If the session's current epoch has no ROOT, no commands, and no edits → create nothing; empty stdout.
5. Otherwise, in one transaction: supersede any live continuation for the session; insert an immutable checkpoint (rendered capsule, §16; `event_watermark` = max reduced event id; git metadata per §13.7); insert a `PENDING` continuation; insert a `compactions` row with `pre_ms`.
6. Emit `systemMessage` with the summary.

### 15.3 Delivery channels (attempted in this order of occurrence)
1. `session_start` — `SessionStart` with `source = compact` (fires after every compaction) or `source = resume` (same `session_id`).
2. `post_tool` — first `PostToolUse` of the session with `ts_ms` > checkpoint `created_ms` while still PENDING (covers auto-compaction mid-turn if channel 1 did not fire).
3. `user_prompt` — `UserPromptSubmit` while PENDING, or while ATTACHED via `user_prompt` per T6.

### 15.4 Transition table
| # | Trigger | Guard | Effect |
|---|---|---|---|
| T1 | PreCompact (§15.2 step 5) | — | old live → SUPERSEDED; new → PENDING |
| T2 | Delivery opportunity on any channel | state = PENDING | Conditional update `state = 'ATTACHED' WHERE state = 'PENDING'`; only the process whose update changed a row emits; insert injection row; `attach_count += 1` |
| T3 | Any `PostToolUse`, `PostToolUseFailure`, or `Stop` event in the session | state = ATTACHED and event `ts_ms` > `attached_ms` | → CONFIRMED (`confirm_event_id` set) |
| T4 | `UserPromptSubmit` | state = ATTACHED, channel = `user_prompt`, no T3 evidence since `attached_ms` (aborted turn) | Re-emit capsule; new injection row (n+1); stays ATTACHED; if `attach_count` would exceed 3 → EXPIRED instead |
| T5 | `UserPromptSubmit` | state = ATTACHED, channel ∈ {`session_start`, `post_tool`} | No re-emission (the context sits in the conversation independent of the prompt) |
| T6 | `SessionStart(clear)` or `SessionEnd(reason ∈ {clear, logout})` | live continuation exists | → EXPIRED |
| T7 | Any hook for the session (reconcile) | state = ATTACHED and T3 evidence exists | → CONFIRMED (crash recovery for missed transitions) |
| T8 | Reconcile | state = PENDING and `created_ms` older than 7 days | → EXPIRED |
| T9 | `SessionStart(startup)` for a different session | — | never delivers another session's continuation in v0.1 |

### 15.5 Idempotency
`delivery_key` = blake3(hook_event | session_id | prompt_id or tool_use_id or `source`+`ts_ms`). If a delivery_key already exists, re-emit the same capsule (the previous output was presumably lost) without creating a new injection or incrementing `attach_count`.

### 15.6 Auto-compaction when the user ignores everything
No user action is ever required: auto-compaction fires PreCompact → checkpoint; SessionStart(compact) or the next tool call delivers. This path MUST be covered by an acceptance test.

---

## 16. The Continuation Capsule (render_version = 1)

### 16.1 Template (exact; ASCII literals; `\n` line endings; sections in this order; `?` = omitted when empty)
```
<VELRA_CONTINUATION v="1" checkpoint="{checkpoint_id}" captured="{created_ms as RFC3339 UTC, second precision}" trigger="{manual|auto|cli}">
[CONTEXT]
Velra is a local tool that recorded this task state from Claude Code tool events before the conversation was compacted. Entries are observations of tool activity. Links between an edit and a later test result are unconfirmed unless stated. Files on disk are the current source of truth for code.
[ROOT_TASK_OBJECTIVE] (OBSERVED | user prompt | {HH:MM})
{root text, <=240 chars, or "(not captured)"}
?[ACTIVE_SUBTASK] (OBSERVED | subtask: prompt | {HH:MM})
?{subtask text, <=200 chars}
?[LATEST_REQUEST] (OBSERVED | user prompt | {HH:MM})      -- omitted if identical to ROOT
?{latest text, <=200 chars}
[STATUS]
{branch or "no git"}{" @ " short_sha if git} | {n} edits this task | last test run: {PASS|FAIL|INTERRUPTED|none}{" (partial capture)" if partial}
?[ACTIVE_FAILURE] (OBSERVED | {test|build|lint} run | {HH:MM})
?Command: {command_text, <=160 chars}
?Result: FAIL{" (exit " n ")" if known}
?  {excerpt line}            -- up to 8 lines, each indented two spaces
?[DEAD_ENDS] (OBSERVED)
?- {path}{" (subagent)"?} | {k} edit(s) | {reverted via `{cmd}`|reverted by a later edit|file rewritten|changed outside the agent} at {HH:MM}
?    - {excerpt minus line}
?    + {excerpt plus line}
?  Observed afterward: `{command}` {outcome}. Causal link: UNCONFIRMED.
?[RECENT_ATTEMPTS] (OBSERVED)
?- {path} | +{a}/-{r} lines | {HH:MM} | afterward: {`command` outcome | no test run yet}
?[WORKING_FILES] (OBSERVED)
?- {path} | edited {e}x, read {r}x{", in failure output" if in_failure}
?[NEXT_KNOWN_TARGET] (INFERRED | {rule name})
?{path[:line]}
[RECOVERY]
Full detail for any section: `velra inspect --checkpoint {checkpoint_id} --section <dead-ends|failure|files|attempts>`
</VELRA_CONTINUATION>
```
(The `--` annotations and `?` markers are spec notation, not output.)

### 16.2 Content selection rules (deterministic)
- **ACTIVE_FAILURE:** most recent `commands` row of kind test/build/lint with outcome FAIL in the current epoch, unless a later row with the same `signature` has outcome PASS.
- **DEAD_ENDS:** non-reapplied dead ends in the epoch, most recent first, max 4; excerpt lines only if the path is not sensitive.
- **RECENT_ATTEMPTS:** ACTIVE edits in the epoch, grouped by path (latest per path), most recent first, max 4, excluding paths already listed under DEAD_ENDS.
- **WORKING_FILES:** score = 3·edits + min(reads, 5) + 4·in_failure; ties broken by `last_touch_ms` desc, then path asc; max 8; excludes paths not existing on disk at render time.
- **NEXT_KNOWN_TARGET:** rule `failure-location` = first mentioned `path:line` in the active failure excerpt resolving under the root; else rule `last-active-edit` = most recently edited ACTIVE path; else omitted.
- **Times** `HH:MM` in the machine's local time zone (tests fix `TZ=UTC`).
- Rendering is a pure function of (materialized snapshot, config); identical input MUST yield byte-identical output on every platform.

### 16.3 Budget
`est_tokens = ceil(char_count / 3.2)`. Target ≤ 800 (configurable `budget_tokens`); hard ceiling 1,000; absolute ceiling 9,500 characters. Truncation order until within target: WORKING_FILES → max 4 → RECENT_ATTEMPTS → max 2 → DEAD_ENDS excerpt lines removed (oldest first) → failure excerpt → max 3 lines → LATEST_REQUEST → 120 chars → ROOT → 160 chars → WORKING_FILES removed. CONTEXT, ROOT line, STATUS, ACTIVE_FAILURE header, DEAD_ENDS headers, and RECOVERY are never removed. If still above target but below the hard ceiling, emit as is.

---

## 17. User-visible messages (exact copy)

| When | Message |
|---|---|
| PreCompact checkpoint | `⚡ Velra checkpoint saved: {summary}` |
| Delivery | `⚡ Velra restored: {summary} ({n} tokens)` |
| Nothing to save | *(no output)* |

No other user-visible output is produced by hooks in v0.1.

---

## 18. Fail-open & edge-case matrix (all MUST hold)

| Situation | Required behavior |
|---|---|
| DB locked beyond budget | Spool (append) or skip (delivery); exit 0 |
| DB missing | Create (with migrations) if possible within 200 ms; else spool |
| DB corrupt | Rotate aside, recreate, log |
| Disk full / read-only home | Exit 0, empty stdout |
| stdin malformed / empty / >64 MiB | Exit 0; record a `malformed` event if possible |
| Missing `session_id` | No-op |
| Unknown hook event or tool | Record minimal event |
| Hook killed by timeout mid-transaction | Transaction rolls back; no partial state |
| Process crash after emitting capsule, before commit | Next hook reconciles; worst case the capsule is emitted once more (acceptable) |
| User presses Ctrl+C right after submitting the prompt | T4: capsule re-emitted on next prompt |
| Two compactions without any delivery | Older continuation SUPERSEDED; newest delivered |
| Parallel tool calls racing to deliver | Exactly one emits (conditional update) |
| Multiple concurrent sessions in one project | Fully isolated by `session_id` |
| Subagent tool events | Tracked, tagged `(subagent)` |
| Non-git directory | Git features off; hash-based reverts still work |
| Worktrees / `cd` during session | Project root from `CLAUDE_PROJECT_DIR`; paths normalized |
| Windows paths | Case-folded identity, `/` display separators, exec-form spawn handles spaces |
| Binary moved/deleted | Claude Code shows a non-blocking hook error; `velra doctor` detects and `velra enable` repairs |
| Newer schema than binary | No-op, logged |
| `VELRA_DISABLE=1` / `~/.velra/disabled` | Immediate no-op |

---

## 19. Security & privacy
- No network I/O at runtime (enforced by a test that runs all hooks under a network-denied sandbox where available, and by a dependency audit: no HTTP client crate linked).
- No telemetry. No accounts. No API keys.
- Redaction per §9.2 before any persistence. Sensitive paths never excerpted.
- Files 0600 / dirs 0700 (POSIX); Windows default user-profile ACLs.
- `SECURITY.md` documents what is stored, where, and how to purge.

---

## 20. Local observability
`~/.velra/logs/errors.log`: one line per error: `{RFC3339} {level} {subcommand} {session_id?} {message}`. `VELRA_LOG=debug` additionally logs per-invocation timing to `~/.velra/logs/debug.log` (off by default; MUST NOT affect budgets when off).

---

## 21. Acceptance test suite

All automated tests run on macOS, Linux, and Windows CI unless marked.

### A. Install & configuration
- A1 Installer on clean macOS/Linux/Windows images installs, verifies checksum, prints the exact final lines; a tampered archive aborts.
- A2 `enable` on: missing file; empty `{}`; file with comments + trailing commas; file with existing unrelated hooks on the same events; symlinked file; 4-space and tab indentation. Each: only Velra handlers added, formatting preserved (golden bytes).
- A3 `enable` twice → byte-identical; `enable` → `disable` → byte-identical to original (for all A2 fixtures).
- A4 Invalid JSONC → exit 1, file untouched, message with line/column.
- A5 Concurrent modification between read and rename → retried; no lost foreign edit.
- A6 `--dry-run` writes nothing and prints a diff.
- A7 Older Claude Code version fixture (no `args` support) → shell form with quoted path.

### B. IPC contract
- B1 For every subcommand, with every recorded fixture under `tests/fixtures/claude-code/*`: exit code 0; stdout empty or one valid JSON object + `\n`; stderr empty.
- B2 Fuzz (`proptest`/`cargo-fuzz`, ≥10 minutes per subcommand in CI nightly): random/truncated/huge stdin never yields non-zero exit, stderr output, or invalid stdout.
- B3 Delivery JSON matches §8.4 exactly for all three channels.
- B4 8 MiB `tool_response` → retained payload ≤ 16 KiB; budget per §4.

### C. Storage & concurrency
- C1 32 processes × 500 `post-tool-use` invocations in parallel → exactly 16,000 events present after one `reduce` (DB + ingested spool), zero duplicates, zero non-zero exits.
- C2 Hold an exclusive write lock for 2 s while hooks run → all hooks exit within budget + 100 ms, events land in spool, later ingested.
- C3 Kill `reduce` with SIGKILL mid-batch (repeat 100×) → derived state equals a clean replay.
- C4 Corrupt DB file → next hook recreates; `doctor` reports.
- C5 Newer `user_version` → hooks no-op.

### D. State machine (property-based + scenario)
- D1 Model-based `proptest` over random interleavings of {PreCompact, SessionStart(src), UserPromptSubmit, PostToolUse, Stop, SessionEnd(reason), crash} → invariants: ≤1 live continuation per session; CONFIRMED only after an injection; no delivery across sessions; `attach_count ≤ 3`; checkpoints never mutate.
- D2 Scenario: compact → SessionStart(compact) delivers → next prompt does not re-deliver.
- D3 Scenario: SessionStart hook absent → first PostToolUse after compaction delivers mid-turn.
- D4 Scenario: delivery via UserPromptSubmit → no evidence → next UserPromptSubmit re-delivers (aborted turn) → PostToolUse → CONFIRMED.
- D5 Parallel PostToolUse × 8 racing on PENDING → exactly one emits.
- D6 Duplicate delivery_key → identical capsule re-emitted, no new injection row.
- D7 `/clear` expires the continuation.

### E. Capsule
- E1 Golden snapshots (`insta`) for ≥12 fixture states (empty-ish, failure only, dead ends via each mechanism, subagent, non-git, partial, sensitive path, long prompts, many files, Windows paths) — byte-identical across OSes.
- E2 Budget: property test over random states → ≤ 1,000 est. tokens and ≤ 9,500 chars always; ≤ 800 whenever truncation can achieve it.
- E3 No causal language: the renderer output never contains "caused", "because", "due to", "led to".
- E4 Every rendered line (except CONTEXT and RECOVERY) derives from a DB row; test asserts traceability via a debug render mode that annotates source row ids.

### F. Reverts & dead ends
- F1 Edit A→B then Edit B→A → one dead end (`inverse_edit`).
- F2 Edits then `git restore <file>` → DISCARDED (`git_command`), command text recorded.
- F3 `git reset --hard` → all touched files' uncommitted edits DISCARDED.
- F4 Edits then `git commit` → COMMITTED, no dead end.
- F5 Revert then reapply → excluded from capsule.
- F6 File changed by `sed -i` then turn end → `turn_scan` version recorded.
- F7 Test run between attempt and revert → "Observed afterward" rendered with UNCONFIRMED.

### G. Fail-open & chaos
- G1 Read-only `$VELRA_HOME` → all hooks exit 0, empty stdout.
- G2 Panic injected (test feature flag) in each subcommand → exit 0, logged.
- G3 Watchdog: artificial 1 s stall → process exits by 260 ms with empty stdout.
- G4 `VELRA_DISABLE=1` → no DB access (verified via file access monitoring on Linux).

### H. Performance
- H1 Budgets in §4 enforced in CI on reference runners; results published as a CI artifact.

### I. End-to-end with real Claude Code (scripted manual checklist; required before release; record the Claude Code version)
1. Fresh install + `velra enable`; start `claude` in a sample repo with a failing test.
2. Give a ≥20-char task; let Claude edit a file; run tests (fail); have Claude revert via `git restore`; try another edit.
3. `velra inspect` shows ROOT, ACTIVE_FAILURE, one DEAD_END, WORKING_FILES.
4. Run `/compact` → user sees "checkpoint saved" and "restored" messages (record which surfaced).
5. Ask "what should we try next?" → Claude's answer does not re-propose the reverted change.
6. Repeat with an auto-compaction (long session or reduced context) without user interaction → capsule delivered mid-turn or on SessionStart(compact).
7. Ctrl+C immediately after the first post-compaction prompt (UserPromptSubmit channel only, SessionStart handler temporarily removed) → next prompt receives the capsule again.
8. `velra disable` → hooks gone, Claude Code unaffected.

### J. Security
- J1 Redaction corpus (≥50 secret samples, ≥50 benign look-alikes): 100% of secrets redacted in DB and capsule; ≤ 5% benign false positives.
- J2 `.env` edits: path + hash only.
- J3 Dependency audit: no network-capable crate in the dependency graph of the hook path.

---

## 22. Milestones & Definition of Done

1. **M1 Skeleton & distribution:** binary, build profile, CI matrix, `--version`, installers, release dry run. (A1)
2. **M2 Settings:** `enable`/`disable`/`--dry-run`, compat module. (A2–A7)
3. **M3 Event log:** hook subcommands append normalized, redacted events; spool; watchdog; kill switch. (B1–B4, C1, C2, G1–G4, J1–J3)
4. **M4 Reducer & tracking:** intents, file versions, reverts, discards, commands. (C3–C5, F1–F7)
5. **M5 Checkpoint, capsule, delivery:** barrier, renderer, state machine, messages. (D1–D7, E1–E4)
6. **M6 CLI & polish:** `status`, `inspect`, `doctor`, README with a 30-second demo GIF script and "Why Rust", SECURITY.md. (H1, I)

**Done** = every acceptance test passes on all CI platforms, the E2E checklist is recorded against a named Claude Code version, `DECISIONS.md` lists every ambiguity resolution, and a user can go from zero to "⚡ Velra restored" with two commands and one `/compact`.
