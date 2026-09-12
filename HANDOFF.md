# Velra v0.1 — handoff

Everything a new session needs to continue this work without re-deriving it.

---

## 1. What this is

Velra makes a Claude Code task survive `/compact`. It registers hooks, watches
tool activity, and at `PreCompact` freezes a checkpoint and renders a
**Continuation Capsule** (≤ 800 estimated tokens) that is injected back into
context exactly once after compaction.

Built to `prompts doc/BUILD_PROMPT_v0.1.md` (695 lines). Scope is §2 of that
spec and nothing more. v0.2–v0.4 specs sit in the same folder and are **out of
scope** until asked.

Three standing rules from the spec that govern every change:

1. **Never block or disturb Claude Code.** Hooks exit 0 always, stderr always
   empty, stdout empty or exactly one JSON object plus a newline.
2. **Preserve data.** An event reaches the database or the spool; never neither.
3. **Simplest thing that satisfies the spec** — and every ambiguity resolved
   gets a numbered row in [DECISIONS.md](DECISIONS.md) (now D1–D55).

---

## 2. Status: complete and green

| Milestone | Scope | State |
|---|---|---|
| M1 | Binary, build profile, CI matrix, `--version`, installers, release workflow | done |
| M2 | `enable`/`disable`/`--dry-run`, compat module | done (A2–A7) |
| M3 | Hook subcommands, normalization, redaction, spool, watchdog, kill switch | done (B1–B4, C1–C2, G1–G4, J1–J3) |
| M4 | Reducer: intents, file versions, reverts, discards, commands | done (C3–C5, F1–F7) |
| M5 | Checkpoint barrier, renderer, delivery state machine, messages | done (D1–D7, E1–E4) |
| M6 | `status`, `inspect`, `doctor`, README + demo script, SECURITY.md | done (H1, I) |

**Test suite: 139 passing, 0 failing, 1 ignored.**

```
velra-core unit      42      crates/velra-core/src/*.rs
velra unit           11      crates/velra/src/*.rs
tracking.rs          14      F1–F7 revert / discard / command tracking
capsule.rs           19      E1 goldens (15), E2 budget + proptest, E4 traceability
state_machine.rs     11      D1–D7 delivery states
settings_edit.rs     13      A2–A7 JSONC editing
ipc_contract.rs      12      B1–B4 hook contract (1 ignored: needs a real claude binary)
storage.rs            6      C1–C5 concurrency, corruption, schema
fail_open.rs          8      G1–G4 chaos
security.rs           6      J1–J3 redaction, sensitive paths, dependency audit
```

Release binary: **5.12 MB** (budget 8 MB), `velra 0.1.0 (x86_64-pc-windows-gnu)`.

A full product pass (`python scripts/smoke.py`) was run against that binary:
enable → task with a failing test and an abandoned edit → `PreCompact` barrier
→ capsule returned at `SessionStart(compact)` → second channel correctly
injected nothing → `status`/`doctor` clean → `disable` restored the settings
file byte for byte.

---

## 3. Layout

```
crates/velra-core/src/   pure logic + storage, no Claude Code specifics
  db.rs          schema (§10.4), Role busy-timeouts, corruption rotation
  eventlog.rs    append + dedupe          spool.rs     no-loss file fallback
  reducer.rs     cursor-based derivation of all task state
  revert.rs      content-hash return, run collapsing, git discard
  render.rs      capsule renderer + the truncation ladder
  snapshot.rs    selects what the capsule shows
  checkpoint.rs  frozen capsules
  continuation.rs  PENDING -> ATTACHED -> CONFIRMED state machine
  redact.rs      aho-corasick prefilter + lazily compiled regexes
  intent.rs commands.rs git.rs hash.rs paths.rs text.rs shell.rs time.rs
  model.rs event.rs lib.rs

crates/velra/src/        Claude Code integration
  main.rs        argv dispatch that keeps clap off the hook path
  hook.rs        every hook subcommand, watchdog, phase timing
  normalize.rs   lenient payload parsing + redaction + budgets
  settings.rs    JSONC editing by AST byte-range splices (13 registrations)
  compat.rs      version gates as constants; VELRA_CLAUDE_VERSION override
  cli.rs         enable/disable/status/inspect/doctor
  inspect.rs     section output
  home.rs atomic.rs log.rs

tests/fixtures/claude-code/2.1.268/   17 recorded hook payloads
tests/golden/capsule/                 15 capsule goldens
tests/golden/settings/                 5 settings goldens
crates/velra/tests/                   acceptance suite + common/mod.rs harness
scripts/smoke.py                      full product pass against the release binary
bench/run.sh                          §4 budgets (hyperfine optional)
install/ npm/ .github/workflows/      distribution
```

---

## 4. Building on this machine (the non-obvious part)

MSVC build tools are **not** installed and installing them needs admin. Rust is
therefore set up with host `x86_64-pc-windows-gnu`, and a portable WinLibs
MinGW-w64 GCC provides the C toolchain that `rusqlite` (bundled SQLite) needs.

**Tool shells do not inherit that PATH.** Before `cargo build --release` (debug
usually works from cache; release needs `dlltool.exe`):

```bash
export PATH="/c/Users/aryan/AppData/Local/Programs/winlibs-mingw64/mingw64/bin:$PATH"
```

PowerShell equivalent:

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:LOCALAPPDATA\Programs\winlibs-mingw64\mingw64\bin;" + $env:Path
```

Without it: `error calling dlltool 'dlltool.exe': program not found`.

CI and releases use MSVC on GitHub runners, so this is a local-only concern.

Claude Code here is **2.1.268, VS Code extension only** — `claude` is not on
PATH, so compat detection scans `~/.vscode/extensions/anthropic.claude-code-*`
and otherwise assumes the latest known version. `VELRA_CLAUDE_VERSION=2.1.268`
forces it.

### Commands

```bash
cargo test --workspace --all-features                                  # 139 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings   # clean
cargo fmt --all
INSTA_UPDATE=always cargo test --workspace --all-features              # refresh goldens
python scripts/smoke.py                                                # product pass
bash bench/run.sh                                                      # §4 budgets
VELRA_STRESS=1 cargo test -p velra --all-features --test storage       # full C1 shape
```

---

## 5. Design points worth not rediscovering

- **The hook path never parses argv with clap.** `main.rs` dispatches on raw
  argv; clap is only reached by the CLI subcommands.
- **stdout is guarded by a mutex** (`EMITTED` in `hook.rs`). The watchdog takes
  that lock before `exit(0)`, so a process can never die mid-write and emit half
  a JSON object.
- **The watchdog spools the in-flight event before exiting** (D48). Without it,
  a 250 ms deadline under load dropped events from both the database and the
  spool. Re-spooling a row that did commit is harmless — ingestion deduplicates
  on `dedupe_key`.
- **Settings are edited as byte-range splices over the JSONC AST**, not via the
  CST mutation API (D6). That is what makes `enable` then `disable` restore the
  file byte for byte, comments and key order intact.
- **`deliver()` emits inside the transaction, before commit** — the capsule is
  written to stdout by a closure the transaction calls, so a crash between
  "printed" and "recorded" is impossible.
- **The reducer splits its batch at `Stop`** and hashes outside the write
  transaction, re-checking the cursor afterwards (D14), so turn-end scans never
  hold the write lock.
- **Redaction runs before anything is persisted**, including the spool.
  Sensitive paths (`.env`, `*.pem`, `id_rsa*`, `.ssh/**`) are stored as path and
  hash with no excerpt.
- **Velra never touches project-level `.claude/settings*.json`** — only the
  user-level file — and writes nothing inside the user's repository.

### Performance, measured

Profiled with `VELRA_LOG=debug` (which now logs a phase breakdown), release
build, 100k-event database, on this Windows box:

```
stdin=30us parse=27us context=160us db-open=1340us db-append=1050us handler=1170us
```

`db-open` at ~1.3 ms is the floor, not an inefficiency: Python's own SQLite
opens the same file in 1.6 ms. The §4 p50 budgets (2 ms for post-tool-use)
therefore describe Linux, which is where CI measures them with hyperfine; §4's
Windows allowance (p99 ≤ 15 ms) covers the wall-clock case here. Recorded as
D54.

---

## 6. Gotchas that cost time

- **CRLF.** Writing fixtures through Python text mode turns `\n` into `\r\n`, so
  `originalFile` in the payload stops matching the bytes on disk and revert
  detection silently finds nothing. `scripts/smoke.py` writes with `newline=''`
  for exactly this reason.
- **Windows first-run cost.** The first execution of a freshly linked `.exe`
  takes ~3 s (loader plus anti-virus); every run after is ~300 ms. Timing tests
  warm the binary first (`warm_binary` in `fail_open.rs`).
- **Test-suite self-saturation.** `cargo test` runs a binary's tests in
  parallel; C1's 200 spawned processes plus the neighbouring storage tests trip
  the real 250 ms watchdog. C1 sets `VELRA_TEST_WATCHDOG_MS=60000`
  (fault-injection builds only) so it measures storage, not scheduling (D49).
- **insta snapshot paths are relative to the test file's directory.** The
  settings goldens were written one level above the repository root before this
  was caught.
- **Heredocs through the Bash tool collapse doubled backslashes.** Patch scripts
  needing a literal backslash should build it with `chr(92)` or edit by line
  index.

---

## 7. Environment variables

| Variable | Effect |
|---|---|
| `VELRA_HOME` | State directory (default `~/.velra`). |
| `VELRA_DISABLE=1` | Kill switch: every hook exits immediately, no database access. |
| `VELRA_LOG=debug` | Per-invocation timing and phase breakdown to `~/.velra/logs/debug.log`. |
| `VELRA_CLAUDE_VERSION` | Assume this Claude Code version instead of detecting it. |
| `CLAUDE_CONFIG_DIR`, `CLAUDE_PROJECT_DIR` | Respected for settings and project root. |
| `VELRA_STRESS=1` | C1 runs the full 32 × 500 shape. |
| `VELRA_BENCH_STRICT=1` | Gate on budgets even with the built-in timer. |
| `VELRA_TEST_PANIC`, `VELRA_TEST_STALL_MS`, `VELRA_TEST_WATCHDOG_MS` | Fault injection; `fault-injection` feature only, never in release. |

---

## 8. What is left

Nothing in the v0.1 spec is unimplemented. The open items are all external:

1. **Placeholders to fill before publishing.** `{{VELRA_DOMAIN}}` and
   `{{GITHUB_ORG}}` appear in `README.md`, `install/install.sh`,
   `install/install.ps1`, `install/homebrew/velra.rb`, `npm/velra/package.json`
   and `Cargo.toml`. They need real values at first release.
2. **The §21 I checklist** (`docs/E2E_CHECKLIST.md`) needs one pass against a
   live Claude Code session — specifically the auto-compaction case and the
   Ctrl+C replay case, which no script can fake. `python scripts/smoke.py`
   covers everything else.
3. **CI has never run.** There is no remote; the workflows are untested against
   GitHub's runners.
4. **No GIF recorded** — the README carries the shot list for it.
5. **hyperfine is not installed locally**, so local budget numbers come from the
   built-in timer (reported, not gated). CI installs it and gates.

---

## 9. Git

Local repository on `main`, no remote. Commits are plain: **no Claude
co-author trailer and no AI attribution**, a standing instruction.

```
Add HANDOFF.md
README: the 30-second demo shot list (M6)
Add scripts/smoke.py: a full product pass against the release binary
Phase timing in the debug log, and a bench script that runs without hyperfine
Acceptance suite: 139 tests across the §21 matrix, plus three robustness fixes
Velra v0.1: hook runtime, reducer, capsule renderer, delivery and CLI
```
