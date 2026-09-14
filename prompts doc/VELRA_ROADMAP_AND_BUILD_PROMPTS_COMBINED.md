# Strategic Roadmap & Developer Psychology Audit — Velra

> **Velra** — *Fresh context. Same work.*
> Adaptive context-lifecycle infrastructure for AI coding agents.
> Product name: **Velra** · binary `velra` · data directory `~/.velra/`.

**Pre-launch check (not yet verified):** confirm availability of `velra` on crates.io, npm, Homebrew core/tap naming, GitHub org, and a domain (e.g. `velra.dev`) before the first public release. Every build prompt treated the domain as a substitutable placeholder, now resolved to `https://github.com/aryanghai12/Velra`.

---

## 1. The wedge in one sentence

> After `/compact`, Claude still knows **what you were doing, what is failing, and what you already tried and threw away.**

Native compaction summarises the conversation. It is weakest at **negative knowledge**: approaches that were attempted, observed to fail, and reverted. Once those disappear, the agent re-proposes the same dead end, and the developer has to say "we already tried that". That moment is the trauma. Velra v0.1 exists to remove it, and nothing else.

---

## 2. Sequencing logic: the trust ladder

Every release asks the developer for exactly one new unit of trust. Releases that ask for two at once die in the "cool, but not on my machine" phase.

| Version | New trust asked | Trauma addressed | Viral trigger | Exit criterion to advance |
|---|---|---|---|---|
| **v0.1** | "Let a binary add hooks to my `~/.claude/settings.json`" | Compaction amnesia: forgotten goal, failing test, dead ends | `⚡ Velra restored: objective · 1 failing test · 2 dead ends` appears seconds after `/compact`, and Claude's next message avoids the dead end | Acceptance suite green; ≥20 real-world compactions dogfooded with zero hook errors shown to the user |
| **v0.2** | "Let it parse my code (locally) and judge my session's health" | "Claude is looping and I can't tell"; line numbers that point into the middle of nothing | Symbol-accurate capsule plus a *correct* loop notice ("same failure 4×, same function edited back and forth") | Loop notices have ≤10% "that was wrong" rate in dogfooding; flaky tests never trigger a notice |
| **v0.3** | "Let it run a local MCP server the agent can query" | "I need the thing from two hours ago" / cold-start fresh sessions | `velra resume` opens a fresh session that starts working on turn 1 without rereading the repo | Fresh-session first-turn productivity measurably better than native `/clear` + manual note |
| **v0.4+** | "Let it run my unattended pipelines and my other agents" | Tool fragmentation (Cursor + Codex + Claude); overnight agent runs that rot | Published Continuation Fidelity benchmark numbers, including where Velra loses | Benchmark shows Velra ≥ native compaction on task success with lower rediscovery; results are public |

Why this order and not another:

1. **v0.1 before intelligence.** Structural parsing (v0.2) is impressive but invisible. A capsule that is *mostly right with file-level precision* delivers the "Aha" immediately. Precision upgrades land better once people already depend on the feature.
2. **Health signals before rotation.** Automatic session management (v0.4) is only safe once the loop detector (v0.2) has been proven honest on real humans in advisory mode. You earn the right to act by first being right while only speaking.
3. **Recovery after capture.** An MCP recovery layer (v0.3) is only useful once there is a rich, trustworthy archive to recover from, which v0.1–v0.2 build.
4. **Portability last.** Multi-agent adapters multiply the test matrix. Doing it before the core is stable multiplies bugs, not users.

---

## 3. Per-version psychology audit

### v0.1 — Lossless Compaction for Claude Code
- **Trauma:** You spend 40 minutes debugging, compaction fires (often automatically, mid-task), and the next message is a confident suggestion you already rejected. You feel robbed of your time and lose trust in the agent.
- **Why the wedge is sharp:** It is felt by every heavy Claude Code user, it recurs daily, and it is binary: either Claude remembers the dead end or it does not. Binary outcomes make great demos.
- **Aha choreography (the 6-second moment):**
  1. `curl -LsSf https://github.com/aryanghai12/Velra/install.sh | sh && velra enable` → `✓ Velra enabled for Claude Code. Nothing else required.`
  2. Work normally for 5–10 minutes (edit, run tests, revert something).
  3. `velra inspect` → see exactly what would survive, rendered locally. (This builds trust *before* the magic.)
  4. `/compact` → `⚡ Velra checkpoint saved` → `⚡ Velra restored: objective · 1 failing test · 2 dead ends · 5 files (612 tokens)`.
  5. Claude's next message references the failing test and steers away from the reverted approach.
- **What we deliberately cut:** AST parsing, loop detection, MCP, dashboards, config UIs, any LLM usage, any network I/O, any files in the repo.
- **Anti-virality risks:** a single visible "hook error" line in the Claude Code transcript reads as "this tool breaks Claude". Fail-open is therefore a *growth* requirement, not only an engineering one.

### v0.2 — Structural Intelligence & Failure Normalization
- **Trauma:** "Claude has edited `rotateToken` back and forth for 20 minutes and the same test fails the same way." The developer senses a loop but has no evidence, so they either interrupt too early (lose progress) or too late (burn quota).
- **Aha:** A notice that is *specific and correct*: `⚡ Velra: same failure (F-3a9c) 4× · rotateToken() edited A→B→A · checkpoint ready — /compact to continue cleanly.` Correctness is the product; one false alarm on a flaky test costs more trust than ten correct alarms earn.
- **What we cut:** automatic action of any kind. v0.2 only speaks.

### v0.3 — On-Demand Recovery & Local MCP
- **Trauma:** "Why did we reject TTL=300?" is lost forever once it leaves the context. Starting a *fresh* session costs 10 minutes of rereading.
- **Aha:** The agent calls `velra_attempts` on its own and answers with evidence (edit, test run, timestamp) instead of re-running the experiment. `velra resume` produces a first turn that already knows the working set.
- **What we cut:** cloud sync, semantic embeddings, vector databases. Full-text search over a local archive is enough and is explainable.

### v0.4+ — Portability, Autonomous Rotation, Fidelity Benchmark
- **Trauma:** Teams mix agents; unattended overnight runs degrade silently as context fills with debris.
- **Aha:** The same capsule works in Cursor and Codex; `velra run` keeps a 6-hour headless job productive; and a public benchmark shows where it helps *and where it does not*.
- **Credibility trigger:** publishing losses. Developer-tool audiences reward honest benchmarks and punish marketing numbers.

---

## 4. Architectural corrections to the source specification

As the reviewing architect, these are deliberate deviations from the original product specification, each backed by the current Claude Code hook documentation. They are encoded in the build prompts.

| # | Source spec said | Velra build prompts say | Why |
|---|---|---|---|
| 1 | Inject the continuation only via the next `UserPromptSubmit` | **Primary:** `SessionStart` with source `compact` (fires right after every compaction). **Fallbacks:** first `PostToolUse` after compaction (mid-turn), then `UserPromptSubmit` with full two-phase commit | Auto-compaction frequently happens *mid-turn*. Waiting for the next user prompt means the agent keeps working amnesiac for the rest of that turn, which is exactly when dead ends get repeated |
| 2 | A global npm install as the ideal install | `curl … \| sh` / `irm … \| iex` / Homebrew as primary; npm only as a convenience channel | npm requires Node.js, which violates the zero-runtime-prerequisite rule |
| 3 | Background state reducer (implied daemon) | No daemon. Reduction is scheduled by Claude Code's own **async hooks** (`PostToolBatch`, `Stop`) and bounded at `PreCompact` | A daemon adds install friction, OS service registration, and a new failure mode. Async hooks give background execution for free |
| 4 | Event logging via `PostToolUse`/`PostToolUseFailure` only | Adds a filtered `PreToolUse` for edit tools and `git` commands | Revert detection needs the file's *pre-edit* content hash. Without it, "A→B→A" cannot be proven |
| 5 | A protocol block of "fixed continuation rules" wrapping the state | Capsule header is **factual, not imperative** | Claude Code's docs warn that injected text framed as out-of-band system commands can trigger prompt-injection defenses, causing Claude to surface it to the user instead of using it |
| 6 | "<800 tokens" as the only size rule | Also a hard **10,000-character** ceiling for any single hook output (capsule + warm pack) | Claude Code caps hook output strings at 10,000 characters and spills larger output to a file, which defeats the purpose |
| 7 | 3–5 ms cold start as a universal target | 3–5 ms p99 on macOS/Linux; Windows target is ≤15 ms wall-clock | Windows process creation alone costs several milliseconds regardless of language; promising otherwise is dishonest |

---

## 5. Tech stack verdict (summary; full defense in BUILD_PROMPT_v0.1 §3)

**Rust (stable, 2021+ edition) + statically bundled SQLite (`rusqlite`, `bundled`) + `serde_json` + `jsonc-parser` (CST) + `blake3`; distributed with `dist` (cargo-dist) as single static binaries.**

- Rust: no runtime, no GC, ~1 ms process start, deterministic latency, first-class tree-sitter bindings (needed in v0.2), official Rust MCP SDK (`rmcp`, needed in v0.3).
- Disqualified: Node/Bun/Deno (runtime or 50 MB+ binaries with 10–30 ms start), Python (runtime + venv), JVM (runtime + start time), Go (viable for v0.1, but tree-sitter and fast SQLite both require cgo, which breaks trivial cross-compilation), Zig (ecosystem too thin for JSONC CST editing and MCP).

---

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Claude Code changes a hook payload field | Tolerant parsing (unknown fields ignored, missing fields → degraded event), recorded-fixture contract tests pinned per Claude Code version, `velra doctor` reports schema drift |
| A Velra bug surfaces as a visible "hook error" | Hook subcommands always exit 0, never print invalid JSON, internal watchdog at 250 ms, panic catcher |
| Users distrust a tool that edits `settings.json` | Timestamped backup, dry-run diff (`velra enable --dry-run`), byte-identical `disable`, open source |
| The capsule is wrong and misleads the agent | Every line has a provenance tag; causal links are always `UNCONFIRMED`; the live repo is stated as authoritative |
| Scope creep into a "memory platform" | Each build prompt has an explicit out-of-scope list; memory (what happened) ≠ lifecycle (what must survive the boundary) |
| Privacy: prompts and outputs stored on disk | Local-only, 0600 permissions, secret redaction before write, sensitive paths stored as path+hash only, `velra disable --purge` |

---

## 7. Measuring success without telemetry

Velra ships with **zero telemetry**. Evidence of success comes from: GitHub stars and issues, release download counts, the public benchmark (v0.4), and structured dogfooding logs that stay on the dogfooder's machine (`velra status --report` produces a redacted, human-reviewable summary a user can choose to paste into an issue).

---

## 8. What Velra must never become

A context dashboard, a chat interface, a coding agent, a generic memory platform, a mandatory cloud service, a paid LLM dependency, a source-control replacement, an automatic destroyer of human sessions, a repository metadata generator, or a tool that blocks development when it fails.
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
1. **Primary (macOS/Linux):** `curl -LsSf https://github.com/aryanghai12/Velra/install.sh | sh`
2. **Primary (Windows):** `powershell -ExecutionPolicy Bypass -c "irm https://github.com/aryanghai12/Velra/install.ps1 | iex"`
3. **Homebrew:** `brew install aryanghai12/tap/velra`
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
# BUILD_PROMPT_v0.4.md — Velra v0.4+ "Cross-Agent Portability & Adaptive Policies"

## 0. Instructions to the implementing agent

You are extending a working **Velra v0.3** codebase to **v0.4**. This document is self-contained: §1 restates invariants you MUST preserve; the rest specifies new behavior.

- RFC 2119 keywords apply. Implement only what is in scope. Record ambiguity resolutions in `DECISIONS.md`.
- **Verification gate (mandatory):** third-party agent hook systems change frequently. For every adapter, before writing code you MUST: (a) read the vendor's current official hooks/plugin documentation, (b) record the doc URL, the date, and the agent version in `adapters/<agent>/VERIFIED.md`, (c) capture real payload fixtures from that version. Any capability you cannot verify MUST be declared `false`. Never infer a capability from another agent's behavior.
- Autonomous behavior is **opt-in, supervised, and shadow-first**. Nothing in v0.4 changes the default human experience.

---

## 1. Carried-forward invariants (v0.1–v0.3; MUST NOT regress)

1. Hooks always exit 0 (or the target agent's documented non-blocking success code); output is empty or one valid document in the agent's expected format; nothing on stderr; watchdogs; kill switch.
2. Sync hook budgets p99 ≤ 5–6 ms (macOS/Linux), barrier ≤ 10 ms, Windows ≤ 15 ms wall — for **every** adapter.
3. Append-only events, spool, cursor reducer, immutable checkpoints, one live continuation per session, idempotent two-phase delivery.
4. Deterministic, factual, zero-LLM capsule with provenance classes; causality `UNCONFIRMED`; HOT ≤ 800 est. tokens; any single injected string within the target agent's documented limit (Claude Code: ≤ 9,500 chars).
5. Health notices advisory by default; read-only MCP; no network except what a benchmark run explicitly performs through the agent under test; no telemetry; no repository files; no daemon.

---

## 2. Scope

### In scope (v0.4)
1. **Canonical Event Model (CEM)** and **adapter layer**; refactor the Claude Code integration into the first adapter without behavior change.
2. Adapters: **Cursor**, **OpenAI Codex CLI**, **Cline**, **OpenCode** (each behind the verification gate; ship only those that pass it).
3. **Capability-driven delivery planner** that picks the best available channel per agent and degrades gracefully.
4. **Rotation Engine** with human mode (existing advisory) and **autonomous mode** for supervised headless pipelines (`velra run`).
5. **Adaptive policies:** transparent, bounded, local threshold tuning from recorded outcomes.
6. **Continuation Fidelity benchmark suite** (`velra bench`).

### Out of scope (v0.4) — MUST NOT implement
Cloud sync, team state, enterprise policy servers, learned ML models or ML dependencies, LLM-based summarization, IDE UI extensions, automatic rotation of interactive human sessions, writing any file into user repositories (adapters use user-level config locations only, unless an agent offers no user-level location — then the adapter is marked unsupported rather than writing into the repo).

---

## 3. Canonical Event Model

### 3.1 Event kinds
`session.start{source: startup|resume|clear|compact|fork|unknown}`, `session.end{reason}`, `prompt.submit{text}`, `tool.pre{class: edit|shell|read|search|mcp|other, path?, command?}`, `tool.post{class, …, output_tail?, exit_code?}`, `tool.fail{class, error}`, `file.edit{path, edits?}` (for agents that report edits directly instead of as tool calls), `turn.end`, `compact.pre{trigger}`, `compact.post{summary?}`.
Every CEM event carries: `agent` (`claude_code|cursor|codex|cline|opencode`), `agent_version?`, `session_id` (agent-native id, namespaced as `{agent}:{id}`), `project_root`, `ts_ms`, `subagent_id?`, `raw_ref` (dedupe key).

### 3.2 Capability flags (per adapter, per detected agent version)
`observe_prompts`, `observe_tool_results`, `observe_shell_output`, `observe_edits`, `pre_edit_hook`, `compact_pre`, `compact_post`, `inject_session_start`, `inject_after_compact`, `inject_prompt`, `inject_post_tool`, `user_message`, `async_hooks`, `headless_mode`, `session_resume_by_id`, `max_injection_chars`.

### 3.3 Storage
`events` gains `agent TEXT NOT NULL DEFAULT 'claude_code'` and stores CEM-normalized payloads; the reducer consumes only CEM. Migration `user_version = 4`.

---

## 4. Adapter contract

Each adapter is a module providing, declaratively:
1. **Detection:** how to find the agent and its version (binary on PATH, config dir), without network access.
2. **Config target:** exact user-level config file path(s) per OS and the file format.
3. **Installer:** idempotent, comment/format-preserving (where the format allows comments), backed-up, atomic merge of Velra hook entries; exact inverse on disable. Reuses v0.1 settings-merge guarantees (byte-identical enable→disable round trip on fixtures).
4. **Input normalizer:** agent payload → CEM (tolerant parsing, unknown fields ignored).
5. **Output encoder:** delivery/notice → the agent's documented output shape; empty output when the channel is not supported.
6. **Capability table** (verified).
7. **Contract tests** over recorded fixtures for every hook event used.

`velra enable [--agent <name>|--all]` enables detected agents (default: Claude Code only, preserving v0.1 behavior; `--all` enables every detected, verified adapter). `velra status` lists each adapter with version and capability summary.

### 4.1 Adapter targets (starting points; each MUST pass the verification gate)
| Agent | Expected integration surface to verify | Expected notes |
|---|---|---|
| **Claude Code** | `~/.claude/settings.json` hooks (existing) | Reference adapter; all capabilities true per v0.1–v0.3 |
| **Cursor** | User-level `hooks.json` (`version: 1`) with events such as `sessionStart`, `beforeSubmitPrompt`, `preToolUse`/`postToolUse`/`postToolUseFailure`, `afterFileEdit`, `afterShellExecution`, `preCompact`, `stop`, `sessionEnd` | Verify which events can return context to the model; `preCompact` may be observe-only; there may be no post-compaction event |
| **Codex CLI** | User-level `hooks.json` with Claude-style nested schema (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`) | Verify compaction events and per-event `additionalContext` support (known to differ by event); timeouts in seconds |
| **Cline** | Verify current hook or extension mechanism | If no stable hook surface exists, ship as unsupported with a documented reason |
| **OpenCode** | Plugin system (plugin runs inside OpenCode's own runtime; the plugin shim only forwards events to the `velra` binary via stdin/stdout) | The shim adds no new host prerequisite because it runs in the agent's existing runtime |

---

## 5. Capability-driven delivery planner

Given a live continuation and an adapter's capabilities, the planner chooses channels in order:
1. `inject_after_compact` (or `inject_session_start` with a compact source) — immediate.
2. `inject_post_tool` — mid-turn fallback.
3. `inject_prompt` — next-turn fallback with two-phase re-delivery on aborted turns.
4. If none exist: **fresh-session handoff only** — the user is told (via `user_message` if available, else via `velra status`) that the checkpoint is available through `velra resume --agent <name>` (if `session_resume_by_id` or headless launch supported) or `velra export`.
The two-phase state machine (PENDING/ATTACHED/CONFIRMED) is agent-agnostic; `CONFIRMED` evidence = any subsequent `tool.post`, `tool.fail`, or `turn.end` in the same session. Capsule wording: identical sections; the `[CONTEXT]` sentence names the agent (`…from {Agent Name} tool events…`). Injection length respects `max_injection_chars` (truncation rules from v0.1 §16.3, then warm pack omitted).

Agents without `compact_pre`: checkpoints are created at every `turn.end` whose reducer state changed since the last checkpoint (rolling checkpoint, keep last 3 per session), so a continuation is always available.

---

## 6. Rotation Engine

### 6.1 Separation of concerns
- **Session health** (v0.2 signals S1–S4, P1–P2) = evidence.
- **Rotation eligibility** = a policy decision over evidence plus a safe boundary.
- **Rotation action** = checkpoint + transition to a fresh context.

### 6.2 Human mode (default; unchanged)
Advisory notices only. Velra never compacts, stops, or restarts an interactive session.

### 6.3 Autonomous mode (opt-in, supervised headless only)
- Entry point: `velra run [--agent claude_code|codex|…] [--policy <file>] [--shadow] --prompt <text> [--until "<shell check>"] [--max-iterations N] [--max-rotations-per-hour R] [-- <agent args>]`.
- `velra run` supervises a loop of headless agent invocations (e.g. Claude Code print mode). Each iteration it decides **CONTINUE** (resume the same agent session by id) or **ROTATE** (start a fresh session receiving Capsule + Warm Start Pack via the v0.3 resume-token mechanism).
- **Rotation eligibility (all MUST hold):**
  1. Safe boundary: the previous iteration ended with `turn.end`, no in-flight tools, no background tasks reported.
  2. Health = `LOOPING`, or (`DRAGGING` and S4 ≥ policy `pressure_min`, default 0.85).
  3. No P1 progress within the last `progress_window` runs (default 3).
  4. No P2 flakiness in the S1 fingerprint set.
  5. At least `min_iterations_between` iterations since the last rotation (default 3) and `max_rotations_per_hour` not exceeded (default 4).
- `--until` check passing ends the run successfully; `--max-iterations` bounds cost.
- **Shadow mode** (`--shadow`, and the default for the first run with a new policy): decisions are computed and logged, but the loop always CONTINUEs.
- Every decision is logged to `rotation_decisions (run_id, iteration, decision, eligible, evidence_json, policy_version, ts_ms)`; `velra run --explain <run_id>` prints them.
- Autonomous mode MUST refuse to start when stdin is an interactive TTY without `--yes`, and MUST NOT be enabled through configuration alone.

---

## 7. Adaptive policies

- Policy file (TOML), versioned, with bounded parameters: signal thresholds (N, K, S3 count, S4 ratio), `pressure_min`, `progress_window`, warm pack budget (1,000–6,000 chars), capsule budget (600–1,000 tokens).
- **Outcome records** per transition (compaction delivery, resume, rotation): post-transition duplicate reads (Read of a file whose hash was already read before the transition and is unchanged), dead-end repetitions (an edit producing a region `body_hash` equal to a dead end's version), turns-to-first-new-test-outcome, loop recurrence within 10 turns.
- **Adaptation rules** (transparent, deterministic, per project, applied at most once per 20 recorded transitions, each step within ±10% of the parameter's range and clamped to bounds):
  - Duplicate reads after transitions above a threshold → raise warm pack budget one step.
  - Dead-end repetitions after transitions → raise DEAD_ENDS priority in truncation order (move above RECENT_ATTEMPTS removal).
  - Loops recurring after autonomous rotations → raise rotation eligibility threshold (N + 1) — rotation that doesn't help is stopped.
  - Zero loops and zero duplicate reads for 3 consecutive windows → lower budgets one step (smaller context).
- Every adaptation is logged with before/after values; `velra policy explain` prints current parameters and the history; `velra policy reset` restores defaults. No hidden state, no ML.

---

## 8. Continuation Fidelity benchmark (`velra bench`)

### 8.1 Purpose
Answer honestly: *can Velra approach the quality of a disciplined manual micro-session workflow while removing the manual burden?* Publish results, including losses.

### 8.2 Task manifest (YAML; schema versioned)
`id`, `repo` (git URL or local path), `commit`, `setup` (shell commands), `prompt`, `subtasks` (ordered list with optional oracle handoff notes), `success_check` (shell command; exit 0 = success), `transition_points` (after subtask k, or at token/turn counts), `max_turns`, `timeout_minutes`, `language`, `tags`.

### 8.3 Conditions
| Id | Condition | Definition |
|---|---|---|
| A | Monolithic | One long session; no forced transitions (native auto-compaction allowed and recorded) |
| B | Manual micro-session (oracle) | At each transition point, a fresh session receives the manifest's human-written handoff note |
| C | Native compaction | At each transition point, native compaction is forced; Velra disabled |
| D | Velra | Same forced transitions as C with Velra enabled (capsule delivery) |
| E | Velra fresh-session | At each transition point, `velra resume`-style fresh session (capsule + warm pack) |

Forcing a transition in headless mode MUST use a documented agent mechanism (verify at build time); if none exists for an agent, that condition is marked unavailable for that agent rather than approximated.

### 8.4 Execution
- Each run executes in an isolated temporary clone (or git worktree) at the pinned commit; separate `VELRA_HOME` per run; agent config isolated via documented config-dir environment variables where available.
- Default n = 5 runs per task × condition; randomized condition order; wall-clock and turn limits enforced.
- The benchmark uses the user's own agent account; `velra bench` MUST print an estimated run count and require `--yes` before starting.

### 8.5 Metrics (computed from Velra's event log; definitions exact)
Task success (success_check), time to completion, total tool calls, duplicate reads (as §7), dead-end repetitions (as §7), repeated failure fingerprints after a transition, human explanation count (condition B note length as a reference cost), first-post-transition-turn productivity (tool calls until the first non-reverted edit or new test outcome), transition latency (barrier + delivery hook durations), injected context size (est. tokens), total transcript bytes processed.

### 8.6 Reporting
- Output: raw JSONL per run + a Markdown report with medians, IQR, and bootstrap 95% CIs per condition; per-task breakdowns; a "where Velra lost" section listing every task where D or E underperformed C or B.
- A starter corpus of ≥ 12 tasks across ≥ 4 languages with seeded, reproducible bugs MUST be included (`bench/tasks/`), each with an oracle handoff note for condition B.

---

## 9. Schema additions (migration to `user_version = 4`)
`events.agent`; `adapters (agent PK, version, capabilities_json, verified_doc_url, verified_ms)`; `rolling_checkpoints` flag on `checkpoints` (new column `rolling INTEGER NOT NULL DEFAULT 0`); `runs`, `run_iterations`, `rotation_decisions`; `transition_outcomes`; `policy_versions (project_id, version, params_json, created_ms, reason)`; `bench_runs`, `bench_metrics`.

---

## 10. Acceptance tests (in addition to v0.1–v0.3 suites)

### X. Adapters
- X1 Claude Code refactor: every v0.1–v0.3 test passes unchanged through the CEM path; budgets unchanged.
- X2 For each shipped adapter: `VERIFIED.md` present with URL, date, version; fixtures for every used event; normalizer produces expected CEM; encoder produces exact documented output; enable→disable byte-identical round trip on config fixtures.
- X3 Capability degradation: adapters lacking post-compaction injection fall back per §5; lacking all injection → no output, handoff message path works.
- X4 Rolling checkpoints for agents without `compact_pre`: at most 3 per session, immutable, superseded correctly.
- X5 Budgets met for every adapter hook on all platforms.

### Y. Rotation & policies
- Y1 Eligibility truth table: each of the five conditions individually false → CONTINUE.
- Y2 Shadow mode never rotates; decisions logged identically to live mode.
- Y3 Rate limits enforced (`min_iterations_between`, `max_rotations_per_hour`).
- Y4 `velra run` refuses interactive TTY without `--yes`; cannot be enabled by config alone.
- Y5 Policy adaptation: synthetic outcome streams produce the specified parameter steps, clamped to bounds; `policy explain`/`reset` correct.

### Z. Benchmark
- Z1 Manifest schema validation with helpful errors.
- Z2 Isolation: concurrent runs never share clones, `VELRA_HOME`, or agent config.
- Z3 Metrics computed from fixture event logs match hand-computed values.
- Z4 Report includes CIs and the "where Velra lost" section even when empty (stating "none observed").
- Z5 Starter corpus: every task's success_check fails at the pinned commit and passes with the reference fix.

---

## 11. Milestones & Definition of Done
1. **M1** CEM + Claude Code adapter refactor (X1).
2. **M2** Delivery planner + rolling checkpoints (X3, X4).
3. **M3** Adapters one at a time, each gated (X2, X5).
4. **M4** Rotation engine + `velra run` + shadow mode (Y1–Y4).
5. **M5** Adaptive policies (Y5).
6. **M6** Benchmark harness + starter corpus + first published report (Z1–Z5).

**Done** = all suites green on all platforms; every shipped adapter verified against current vendor docs; a first benchmark report is published with raw data, including every condition where Velra did not win.
