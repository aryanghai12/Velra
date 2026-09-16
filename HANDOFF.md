# Velra v0.1 — handoff

Everything a new session needs to continue this work without re-deriving it.

---

## 1. What this is

Velra makes a Claude Code task survive `/compact`. It registers hooks, watches
tool activity, and at `PreCompact` freezes a checkpoint and renders a
**Continuation Capsule** (rendered to 745 estimated tokens, hard ceiling 1,000;
the 745 is the spec's 800 less a margin for the estimator's ~6% under-read)
that is injected back into context exactly once after compaction.

Built to `prompts doc/BUILD_PROMPT_v0.1.md` (695 lines). Scope is §2 of that
spec and nothing more. v0.2–v0.4 specs sit in the same folder and are **out of
scope** until asked.

Three standing rules from the spec that govern every change:

1. **Never block or disturb Claude Code.** Hooks exit 0 always, stderr always
   empty, stdout empty or exactly one JSON object plus a newline.
2. **Preserve data.** An event reaches the database or the spool; never neither.
3. **Simplest thing that satisfies the spec** — and every ambiguity resolved
   gets a numbered row in [DECISIONS.md](DECISIONS.md) (now D1–D63).

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

**Test suite: 157 passing, 0 failing, 1 ignored.**

```
velra-core unit      49      crates/velra-core/src/*.rs
velra unit           13      crates/velra/src/*.rs
tracking.rs          18      F1–F7 revert / discard / command tracking
capsule.rs           22      E1 goldens (15), E2 budget + proptest, E4 traceability
state_machine.rs     11      D1–D7 delivery states
settings_edit.rs     12      A2–A7 JSONC editing
ipc_contract.rs      13      B1–B4 hook contract (1 ignored: needs a real claude binary)
storage.rs            7      C1–C6 concurrency, corruption, schema, migration
fail_open.rs          7      G1–G4 chaos
security.rs           6      J1–J3 redaction, sensitive paths, dependency audit
```

Two of those are `#[cfg(unix)]` (`g1_read_only_home_never_blocks_a_hook`,
`state_files_are_private_on_posix`) and do not compile on Windows, so the same
tree reports 159 on Linux. Nothing is missing when the local count is lower.

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
tests/fixtures/tokenizer/             2 delivered capsules + measured token counts
crates/velra/tests/                   acceptance suite + common/mod.rs harness
scripts/smoke.py                      full product pass against the release binary
bench/run.sh                          §4 budgets (hyperfine optional)
bench/run_full_benchmark.py           the v0.1 benchmark (≈ $2.80/replicate)
bench/run_efficacy_benchmark.py       the v0.1.1 efficacy benchmark (≈ $40-60)
bench/README.md                       what each benchmark answers, and why
bench/scenarios/                      the three v0.1.1 scenarios + ground truth
bench/harness/preregistration.json    hypotheses and criteria, fixed before the run
bench/harness/regression_gate.py      everything free that must pass first
bench/harness/selftest.py             the v0.1.1 pipeline, offline, ~15 s
bench/tests/                          the harness's own unit tests
bench/harness/claude_binary.py        resolves the active Claude Code binary
install/ npm/ .github/workflows/      distribution

crates/velra/examples/   diagnostics, never shipped in the binary
  seed.rs         synthesises a benchmark database (reduces fully; no tail)
  budget_probe.rs sweeps render budgets against a real trial database and
                  reports which sections survive at each — this is what showed
                  the `[WORKING_FILES]` ladder cliff
```

---

## 4. Building on this machine (the non-obvious part)

MSVC build tools are **not** installed and installing them needs admin. Rust is
therefore set up with host `x86_64-pc-windows-gnu`, and a portable WinLibs
MinGW-w64 GCC provides the C toolchain that `rusqlite` (bundled SQLite) needs.

The repository carries **no `rust-toolchain.toml`** — it used to, and pinning a
bare channel there resolved against this machine's default host (GNU) while
forcing that choice on every other contributor too. The GNU toolchain is now
selected by a machine-local rustup directory override instead:

```bash
rustup override set 1.98.1-x86_64-pc-windows-gnu    # already set here
```

That lives in `~/.rustup/settings.toml`, not in the repo, so a fresh clone
elsewhere just uses whatever host toolchain that machine already has.

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

Claude Code here is **2.1.270, VS Code extension only** — `claude` is not on
PATH, so compat detection scans `~/.vscode/extensions/anthropic.claude-code-*`
and otherwise assumes the latest known version. `VELRA_CLAUDE_VERSION` forces a
specific one. The extension auto-updates, so the bench harness resolves the
binary through `bench/harness/claude_binary.py` rather than pinning a version;
`VELRA_BENCH_CLAUDE` overrides it.

### Commands

```bash
cargo test --workspace --all-features                                  # 157 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings   # clean
cargo fmt --all
INSTA_UPDATE=always cargo test --workspace --all-features              # refresh goldens
python scripts/smoke.py                                                # product pass
bash bench/run.sh                                                      # §4 budgets
VELRA_STRESS=1 cargo test -p velra --all-features --test storage       # full C1 shape
python -m pytest bench/tests -q                                        # benchmark harness tests
python bench/harness/selftest.py                                       # benchmark pipeline, offline
python bench/harness/regression_gate.py --binary target/release/velra.exe
```

`rustup`'s default toolchain on this box is `stable-x86_64-pc-windows-msvc`
and there is no MSVC linker installed, so a bare `cargo test` fails at link
time with `link: extra operand` — that is a GNU `link.exe` on PATH being
handed MSVC arguments, not a code failure. Use
`cargo +stable-x86_64-pc-windows-gnu`. The regression gate detects this and
selects the toolchain matching the release binary's target triple on its own;
`$VELRA_BENCH_CARGO_TOOLCHAIN` overrides.

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

Profiled with `VELRA_LOG=debug` (which logs a phase breakdown), release build,
100k-event database, on this Windows box:

```
stdin=30us parse=27us context=160us db-open=1340us db-append=1050us handler=1170us
```

`db-open` at ~1.3 ms is the floor, not an inefficiency: Python's own SQLite
opens the same file in 1.6 ms. The §4 p50 budgets (2 ms for post-tool-use)
therefore describe Linux, which is where CI measures them with hyperfine.

**On Windows, only the marginal number means anything.** The benchmark timed a
spawn control — the same binary with `VELRA_DISABLE=1`, which exits before it
opens the database — at a p99 of 7.2 ms against a small database and 25.0 ms
against a 38 MiB one. Bare process creation there already costs more than §4's
whole 15 ms allowance, so that allowance was never a statement about Velra.
Against the control, Velra's own cost is +3.4 to +11.0 ms at p50 and barely
moves between the two database sizes. README and D54 now state the Windows
budget that way. Re-measure with `python bench/harness/hook_overhead.py`.

### The benchmark, and what it found

`python bench/run_full_benchmark.py` runs the real experiment: two arms of a
16-turn Claude Code session over a generated 88-file repository with a planted
one-cent rounding defect, `/compact` at turn 14, differing only in
`velra enable` vs `velra disable`. Roughly $2.80 per replicate.
[BENCHMARK_REPORT.md](BENCHMARK_REPORT.md) is the write-up; §15 is the addendum
recording the fixes made afterwards.

It found four real defects — the token estimator under-reading by a third, a
`git_pre` row silently deleting `[DEAD_ENDS]` from a delivered capsule, a revert
chained with a failing command being lost entirely, and an audit sweep evicting
the files the task was about. All are fixed and all are covered by tests that
run offline.

**The benchmark has since been re-run against the fixed binary** (`e9f40151c`,
Claude Code 2.1.270). §17 of the report carries the current verdicts —
**H1, H2, H3 PASSED; H4 FAILED** on `PreCompact` at 18.45 ms marginal p99
against a 15 ms allowance. H3 is settled by measurement, not inference: the
delivered block is **703 real tokens** on Anthropic's tokenizer, control
delta 0. Recommendation 7 still stands — H1 needs a harder defect (several
coordinated edits, or two competing dead ends) before any claim of advantage
over vanilla compaction is honest, because both arms still solve this one in
two tool calls.

Read §18 before trusting H1 or H2: in 1 of 3 replicates the agent rejected the
capsule as a prompt injection and passed every criterion anyway.

The finding worth remembering: **Claude Code 2.1.269's own compaction summary is
better than expected at exactly the things Velra carries.** It named the
objective, the failing assertion and the reverted dead end unprompted, and the
baseline arm solved the task in the same two tool calls as the Velra arm. Velra's
measured advantage is that its record is *bounded* (the native summary ranged
670–6,937 tokens across four runs of an identical script) and *deterministic*
(twice, the model treated the compaction template as a prompt injection and
declined to summarise at all). Do not claim more than that without new data.

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
  needing a literal backslash should build it with `chr(92)`, use a Python raw
  string, or edit by line index.
- **Python on Windows defaults stdout to cp1252**, which raises
  `UnicodeEncodeError` on Velra's own `✓` and `⚡`. `scripts/smoke.py`
  reconfigures its streams; the bench harness pins `encoding="utf-8"` on every
  `subprocess.run`, because the locale codec was quietly writing mojibake into
  the evidence files.
- **Spooled events arrive with an old timestamp and a new row id.** Anything
  that reasons about "what happened after what" must order by `ts_ms`, not by
  row id — reading `file_versions` in insertion order is what deleted
  `[DEAD_ENDS]` from a delivered capsule (D56).

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

Nothing in the v0.1 spec is unimplemented, and the defects the benchmark found
are fixed. The open items are measurement and release logistics:

1. **The capsule gets rejected as a prompt injection, intermittently.** In one
   of three Velra replicates the measured turn opened by calling the
   `VELRA_CONTINUATION` block "fabricated tool/system content" and ignoring it.
   The turn still passed every H1 and H2 criterion — which is the problem: the
   harness cannot tell "the capsule helped" from "the capsule was discarded and
   the task was easy". §18 of `BENCHMARK_REPORT.md` has the quote and the v0.2
   plan (reframe the block as passive workspace state, drop the imperative
   register from `[CONTEXT]` and `[RECOVERY]`, and measure rejection rate as a
   first-class metric). **This is the single most valuable thing left to do.**
2. **The truncation ladder has a cliff at `[WORKING_FILES]`.** `SPEC_STEPS` in
   `render.rs` goes `working_max = 4` → `working_max = 0` with nothing between,
   so the section vanishes entirely on one step. It was present at budget 800,
   absent at 720 *and* at 745 — the last of those 75 tokens *under* target.
   Planned v0.2 fix: a `working_max = 2` step between the two, keeping the two
   top-ranked files for ~35 estimated tokens. `crates/velra/examples/budget_probe.rs`
   sweeps the budget against a real trial database and shows the flip.
3. **A harder defect for H1.** Both arms solved the planted one-cent bug in two
   tool calls, so the task cannot separate them. Recommendation 7 of the report
   asks for a defect needing several coordinated edits, or two competing
   plausible dead ends, before any claim of advantage over vanilla compaction.
4. **H4's latency half is genuinely over budget.** `PreCompact` costs 18.45 ms
   marginal p99 against a 15 ms allowance — Velra's own work, spawn subtracted.
   Two false leads are recorded in §17 so they are not re-chased: the seeder
   does *not* leave events unreduced (`reduce` drains; `batch` sizes a batch),
   and the 15 ms allowance is if anything too generous (§4 implies ≈10 ms).
   The work is to find the ~9 ms p50 `PreCompact` spends above process spawn.
5. **Placeholders — resolved at v0.1.** The distribution org and domain
   placeholders that used to sit in `install/install.sh`, `install/install.ps1`,
   `install/homebrew/velra.rb`, `npm/velra/package.json`, `CHANGELOG.md` and
   `Cargo.toml` are now substituted to `aryanghai12` and
   `https://github.com/aryanghai12/Velra`. The curl/irm one-liners still point
   at repo-root paths that GitHub does not serve as raw scripts; they need a
   raw.githubusercontent.com or release-asset URL before the install docs are
   accurate.
6. **The §21 I checklist** (`docs/E2E_CHECKLIST.md`) needs one pass against a
   live Claude Code session — specifically the auto-compaction case and the
   Ctrl+C replay case, which no script can fake. `python scripts/smoke.py`
   covers everything else.
7. **CI has never run.** There is no remote; the workflows are untested against
   GitHub's runners.
8. **No GIF recorded** — the README carries the shot list for it.
9. **hyperfine is not installed locally**, so local budget numbers come from the
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
