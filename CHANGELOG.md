# Changelog

All notable changes to Velra are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Velra adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] — Unreleased

Not yet published: the latest release on GitHub Releases, npm and crates.io
is 0.1.1. See [docs/RELEASING.md](docs/RELEASING.md).

### Release notes

**Clear the context. Keep the state. Continue working.** Velra 0.1.2 carries
bounded operational state across a Claude Code session boundary, not only
across `/compact`.

- **`velra restore`.** Pick a previous session of this workspace (or name
  it with `--session`). Velra renders that session's operational state (the
  objective, stated constraints, test status, reverted edits, recent work and
  the next step) into a bounded capsule, ~700 estimated tokens by default
  with a hard ceiling of 1,000, and stages it. No transcript is replayed and
  no model is called. `--list`, `--dry-run`, `--clear` and `--json` make it
  scriptable. [docs/RESTORE.md](docs/RESTORE.md).
- **SessionStart delivery.** The next brand-new session in that workspace
  (`SessionStart` source `startup`) receives the capsule exactly once. `/clear`,
  `resume`, `compact` and `fork` leave it staged, and it expires after 7 days.
  Delivery fails open and never blocks a session.
- **Workspace/session model.** The workspace (git root or
  `CLAUDE_PROJECT_DIR`) is the durable ownership boundary. Source sessions
  remain first-class and are named in the capsule. One definition of
  workspace identity is shared by the CLI and the hooks.
- **Retention specificity.** Exact identifiers now survive into the capsule:
  per-test status by exact test id, the latest message that names code, a
  truncation ladder that sheds prose before identifiers (D64–D66).
- **`velra inspect --trace <MARKER>`.** It reports where a fact was lost
  between the recorded events and the capsule, with a computed reason (D67).
  `velra status` shows a staged capsule.
- **Benchmark requalification.** Under preregistration `velra-tokenburn`
  v1.1.0, on Claude Code 2.1.280 with Sonnet: 4 matched pairs, 8 trials, all
  valid, auto-memory verifiably disabled. The Velra mechanism held end to end
  in 4/4 Velra trials. Velra arms met the registered correctness criterion
  4/4 and baselines 0/4. Baselines passed the target test but edited code the
  earlier conversation had put out of scope. Total input burden was lower in
  3 of 4 pairs and higher in 1. This is qualification evidence, not an effect
  size. [docs/BENCHMARK.md](docs/BENCHMARK.md).

**Known limitations.** The capsule is a bounded summary of recorded
operational state. Assistant reasoning is not captured, and detail is dropped
to stay in budget. A restore is a snapshot, not live. The benchmark is four
qualification pairs on one machine, model and scenario family, with a
fresh-session (not `--resume`) baseline and a synthetic ~250K-token context
proxy.

**Upgrading.** Run `velra enable` once after installing 0.1.2. The database
schema is unchanged (v2).

### Added

- `velra inspect --trace <MARKER>` (repeatable, `--json`): where a string was
  lost between the recorded events and the capsule, and why (D67).

- **Staged capsules are delivered at `SessionStart`.** `velra restore` stages
  state; a brand-new Claude Code session in that workspace now picks it up.
  That closes the loop the product is named for: leave the conversation, keep
  the state, carry on.

  Which session start may consume a capsule is decided by the capsule, not by
  the hook. Each staged record carries an `intent` and a `deliver_on` list, and
  `claim_with` tests the incoming `SessionStart` source for membership in that
  list — no source name appears anywhere in the staging logic. A `velra
  restore` capsule declares `new_session` / `["startup"]`, so it is delivered
  to a genuinely new session and is left completely untouched by `clear`,
  `resume`, `compact` and `fork`: not consumed, not discarded, not counted
  against anything. Those four are all continuations of a conversation that is
  still running — `resume` and `compact` already belong to the in-session
  continuation path, and consuming a capsule on `clear` or `fork` would spend
  state staged for the *next* session on the one the user is still sitting in.
  A source this build has never heard of is refused on the same rule, so the
  default is always "do not deliver".

  A future clear- or resume-handoff workflow is therefore a new constant and a
  different value in one field; `deliver_on_is_data_not_code` stages a capsule
  for a source no shipped intent uses and watches it deliver, so the seam stays
  open.

  `claim_with` calls its `emit` closure while the capsule is still on disk and
  deletes it only once emit reports success, mirroring `continuation::deliver`.
  Without that ordering a capsule would be consumed by a delivery that never
  happened — the hook refuses a second JSON object per process, so it is a real
  case rather than a hypothetical one.
- `SessionStart` fixtures for Claude Code 2.1.272 (`startup`, `clear`,
  `resume`, `compact`, `fork`, and a minimal one), replayed by the existing
  fixture-contract test and by the delivery matrix. The minimal fixture carries
  only `session_id`, `hook_event_name` and `source`, because that is what the
  installed runtime actually sends: Velra's own event log for 2.1.272 holds
  `SessionStart` payloads consisting of nothing but `{"source":"startup"}`, so
  `model` and `transcript_path` are genuinely absent and must not be required.
- `velra status` now reports a staged capsule for the current workspace: what
  it is, how large, which session it came from and which `SessionStart` source
  will consume it. It is one file outside the repository and was otherwise
  invisible.

- **`velra restore` — explicit cross-session restore.** The problem this
  answers: a Claude Code conversation grows until it is worth leaving, and
  leaving it costs the small amount of operational state needed to carry on —
  what the task was, which constraint was stated once in turn 0, which approach
  was already tried and reverted, which test is currently failing. `velra
  restore` lets you pick a previous session of this workspace and stages that
  session's state so a brand-new session can pick it up. Clear the context,
  keep the state.

  The ownership model this required is a change of shape rather than of
  schema. A workspace is now the durable boundary and holds many sessions, each
  with its own state; the source session stays a first-class identity and is
  recorded in the staged artifact; the destination session inherits nothing but
  the capsule text. Automatic delivery is unchanged and still never crosses a
  session — `continuations_never_cross_sessions_without_an_explicit_restore`
  now states both halves of that as one claim. `/clear` still expires the live
  continuation and now demonstrably does *not* expire the state behind it
  (`d7_clear_expires_the_continuation_but_never_the_state`).

  No new dependency. No LLM anywhere in the path: the capsule is a query over
  the existing ledger, rendered by the existing renderer under the existing
  token budget, and the picker's labels are strings already on disk.
- `velra-core::transcript` — discovery of historical sessions from
  `~/.claude/projects/<project>/<session>.jsonl`. The transcript is Claude
  Code's own journal and not a published interface, so the parser assumes
  nothing: every line is parsed as a free-form value, unknown record types and
  unknown fields cost nothing, an unparseable line is skipped rather than
  fatal, and the trailing partial line of a bounded read is always discarded so
  a session being written to right now cannot contribute a truncated string.
  Only the first 256 KiB of a transcript is ever read — the files on the
  development machine run to 4 MB and the picker needs a label, not a
  conversation. A session whose transcript yields no usable label is still
  selectable, shown by id and last activity.
- `velra-core::staging` — the staged capsule, at
  `$VELRA_HOME/staged/<workspace_id>/staged_capsule`. Written atomically
  (temp file, fsync, rename), so an interrupted stage leaves the previous
  capsule intact rather than a half-written replacement. Claimed by the
  exclusive creation of a marker file (`create_new`), not a read-then-delete
  and not a rename: exactly one of several racing claimants wins, on Windows
  as well as POSIX (a rename-based claim was tried first and produced four
  winners out of eight threads on Windows). A capsule past seven days, or one
  that is unreadable, is discarded; one failing its content hash or carrying
  another workspace's id is refused and left on disk as evidence.
  `SessionStart` consumes it (see "Staged capsules are delivered at
  `SessionStart`" above).
- `velra restore --list`, `--session`, `--dry-run`, `--clear` and `--json`, so
  the command is usable from a script and testable without a terminal.

- **A v0.1.1 efficacy benchmark** (`python bench/run_efficacy_benchmark.py`),
  built to answer the question the v0.1 benchmark could not: does the
  continuation capsule change what the agent *does* after compaction? Three
  scenarios, each targeting one capsule section and each built so the answer is
  **not recoverable from the repository** — two eliminated approaches
  (`[DEAD_ENDS]`), a constraint stated once in the first turn where both
  possible fixes make the suite green (`[ROOT_TASK_OBJECTIVE]`), and an
  anaphoric reference to a file read before a sweep across 84 unrelated modules
  (`[WORKING_FILES]`). Success criteria, valid-trial rules and the replicate
  minimum are pre-registered in `bench/harness/preregistration.json` and hashed
  into every artifact; the verdict script refuses to evaluate results produced
  under a different hash. Four replicates per arm is a floor rather than a
  preference: with a 2×N table, a perfect split reaches one-sided Fisher
  p = 0.050 at n=3 and 0.014 at n=4. `bench/README.md` records what each design
  decision is a response to.
- **An offline regression gate** (`bench/harness/regression_gate.py`) that must
  pass before any live session is paid for: a leak scan over every generated
  fixture, ground truth verified by running pytest against each declared dead
  end and fix, a named regression test asserted present for each of the nine
  defects the v0.1 benchmark found, and a provenance check.
- **An offline self-test** (`bench/harness/selftest.py`) that runs the entire
  analysis pipeline on synthetic captures with scripted outcomes and checks
  every branch of the verdict logic, in about fifteen seconds and for nothing.
- `bench/tests/` — the benchmark harness's own unit tests, including checks
  that recompute v0.1's published numbers from its recorded raw captures.
- `capsule.rs::the_working_files_ladder_still_steps_from_four_to_zero` pins the
  `[WORKING_FILES]` cliff as measured, so the planned `working_max = 2` rung has
  to flip it deliberately. The capsule property test now also asserts exactly
  one opening and one closing tag, which `ends_with` alone did not catch.

### Changed

- **Product description.** The CLI, crate, npm and Homebrew descriptions said
  "Lossless compaction for Claude Code". The capsule is bounded, and lossy by
  design, and 0.1.2 carries state across sessions. They now read "Local-first
  session continuity for Claude Code".
- **Documentation rewritten around the current product and evidence.** New
  `docs/INSTALL.md`, `CLI.md`, `RESTORE.md`, `CONFIGURATION.md`,
  `TROUBLESHOOTING.md`, `ARCHITECTURE.md`, `BENCHMARK.md`, `RELEASING.md` and
  `CONTRIBUTING.md`. The v0.1 benchmark report, the v0.1 handoff notes and the
  pre-run architecture audits are kept and labelled historical.
- **Benchmark evidence.** `bench/results/v0.1.2-requal/` is the current
  evidence and is protected from being overwritten. `bench/results/README.md`
  names every other result tree as historical. The 1.0.0 qualification (tag
  `tokenburn-qualification-v0.1.2`) is marked invalidated.

- **The efficacy benchmark now measures the workflow v0.1.2 actually ships.**
  S1, S2 and S3 are within-session experiments about compaction; nothing in
  them crosses a session boundary, which is the whole of what cross-session
  restore does. They are archived under `bench/legacy/`, still runnable as
  regression tests, and they contribute nothing to the v0.1.2 scorecard. Every
  historical result tree, `BENCHMARK_REPORT.md` and both earlier
  pre-registrations are untouched; the one rename is
  `bench/results/v0.1.2/readiness.json` to `readiness_hardened.json`, because
  the new benchmark's readiness report takes that path.

  The replacement, `bench/tokenburn/`, asks whether leaving a large
  conversation behind and restoring a bounded capsule into a fresh session
  costs less input and still does the work correctly. Two benchmarks — a cold
  continuation and a `/clear` — over one realistic payments fixture with three
  genuinely failing tests, where nothing in the tree says which one is the live
  task, what was decided about it, or which approach was already tried and
  reverted. The abandoned approach is never committed, so `git log`, `git
  reflog` and `git stash list` carry no trace of it, and all three are fatal
  leak-scan surfaces alongside `CLAUDE.md`, `.claude/`, the environment and the
  post-transition prompt.

  Three properties the old system did not have. Every metric carries its
  source, the artifact it was read from and whether it was measured, proxied or
  unavailable — and a metric that was not observed stays `null` through every
  arithmetic operation instead of becoming a zero that would flatter Velra. A
  `MockClaudeAdapter` writes synthetic trials in the live artifact shape and
  feeds the production parser, metrics, pairing, causal chain and verdict, so
  eighteen scripted cases exercise the real evaluator offline. Of the seventeen
  matched pairs they produce, three are Velra wins and fourteen are not — two
  baseline wins, six ties and six inconclusive — including a baseline that
  reconstructs the state for itself and is scored as a success. And live execution is gated in executable code: `--live`
  plus `VELRA_ALLOW_LIVE_BENCHMARK=1` plus a terminal that is not inside Claude
  Code, checked twice, with structural tests that fail the build if the runner
  acquires any other path to the live driver.

  A pre-qualification audit then found three defects that would have made the
  first paid run meaningless, all fixed before anything was authorised. Claude
  Code reports a turn's usage twice — once on the assistant event, once on the
  result event — and the parser was summing both, reporting exactly double the
  true input and not by the same factor on both arms; usage records now carry a
  class and exactly one class is believed. `SessionStart(clear)` bumps the
  session epoch and `restore::build` reads the current one, so the `/clear`
  benchmark's restore, issued after the clear, returned `NoState` — measured
  against the release binary, not inferred — and the restore now runs before
  the clear turn with the source session still live, which is also what a
  developer would do. And the readiness gate counted its own output as an
  uncommitted change, so producing the artifact was what made the tree dirty.

  (At the time of this entry no live trial had been run; see the benchmark
  entries above for the two runs since.) `bench/results/v0.1.2/readiness.json` said so,
  and `docs/ARCHITECTURE_AUDIT_TOKEN_STATE.md` records the measurement
  limitations — including that Claude Code may not expose the cache token
  fields at all, in which case the benchmark's primary question reports
  INCONCLUSIVE rather than an estimate.

- The development version is now `0.1.2`. The post-release bump after the
  `v0.1.1` tag had never been made, so the workspace manifest, the
  `velra-core` path dependency, the npm package and `velra --version` all still
  reported `0.1.1` while the source and the benchmark pre-registration had
  moved on. Historical `v0.1.1` references in reports and benchmark artifacts
  are untouched — they describe a released version and a collected dataset.

- **Quickstart commands no longer carry shell traps.** The primary Windows line
  is now the native `irm ... | iex`, with the `powershell -ExecutionPolicy
  Bypass -Command "..."` wrapper given separately for people pasting from CMD —
  wrapping it by default spawns a second PowerShell and fails with
  `ResourceUnavailable` inside an existing session. The `cargo-binstall` section
  now bootstraps that tool from its own prebuilt release instead of suggesting
  `cargo install cargo-binstall`, which compiles 370+ crates and needs the C++
  linker the whole tier exists to avoid. npm's global and zero-install paths are
  spelled out separately, and a table says which install methods register the
  Claude Code hooks for you and which need `velra enable` once.

### Fixed

- **A benchmark pair was scored TIE with only one correct arm.** The
  requalification's first aggregate printed `A_cold_continuation#q2` as a TIE
  "because both arms were correct" beside a baseline recorded incorrect. The
  registered TIE needs both arms correct. The pair is now INCONCLUSIVE with
  link I named. The pooled burden median now covers every pair whose
  mechanism held, so a pair no longer leaves the median because its burden
  went against Velra (D69). The frozen evidence was re-scored before it was
  committed. Per-trial analyses were byte-identical.
- **The CI latency-budget job measured nothing.** It called `bench/run.sh`
  after the script moved to `bench/legacy/`, and `continue-on-error` hid the
  missing file. The script also resolved its root one level short.

- **Exact continuation state survives restore.** The v0.1.2 Token-Burn
  qualification restored four sessions and every capsule lost an identifier
  the next session needed, although the ledger held all of them (D64-D66):
  - a new `[TEST_STATUS]` section names each tracked test by its exact id
    (`tests/x.py::test_y`, `mod::tests::name`, `TestName`, ...) with its latest
    covered status, including tests the session got passing;
  - when the latest message names no code, `[EARLIER_MESSAGE]` carries the
    most recent one that does, so "next, look at `parse_header`" survives a
    closing "that's all for today";
  - the truncation ladder now drops regenerable prose (observed-afterward
    lines, excerpt context, the inferred failure location) before exact
    identifiers, and shortens a message before dropping it;
  - repeated reverts of one file share a `[REVERTED_EDITS]` header, and files
    outside the workspace rank below workspace files.

  On the four frozen qualification ledgers the declared markers carried go
  from 8/14 to 14/14, every capsule still at or under the 740-token target.

- **The hook and the CLI could disagree about which workspace they were in.**
  `hook.rs::project_root` honoured `CLAUDE_PROJECT_DIR`; `cli.rs::workspace_for_cwd`
  did not. Their tails were identical character for character, but wherever
  that variable pointed somewhere other than the repository root the two
  produced different `workspace_id`s for the same directory — so `velra
  restore` would stage under one key and `SessionStart` would look under
  another. The failure mode is the worst available: a missing staged capsule is
  indistinguishable from nothing having been staged, so restore would simply
  never deliver, without an error anywhere. Both now call
  `velra_core::workspace::resolve`, and the CLI honours `CLAUDE_PROJECT_DIR`
  as the hook always has. Agreement is now a property of there being one
  implementation; `the_cli_and_the_hook_agree_even_from_a_subdirectory` drives
  the real binary with the project dir, the CLI's cwd and the hook's reported
  cwd all set to three different strings.

- **`velra --version` could report a stale commit.** `build.rs` declared
  `rerun-if-changed` on `.git/HEAD`, which on a branch holds `ref:
  refs/heads/<name>` and does not change when you commit — only the ref file
  does — so cargo never re-ran the build script and the binary kept reporting
  whatever sha it was first built at. The v0.1 benchmark's four-replicate
  dataset is attributed to `e9f40151c` while the tree it ran from was several
  commits further on. The ref `HEAD` points at and `packed-refs` are now watched
  too, and `.git` is resolved through the `gitdir:` indirection so worktrees and
  submodules work.
- **Two defects in the v0.1 benchmark fixture**, both of which made its measured
  turn easier than it claimed to be. `test_exact_payment_settles_invoice`
  carried the docstring *"The discount is a property of the invoice, not of each
  line"* — the fix, in one sentence, in the first file every trial reads; the
  recorded `saturated-velra-r4` transcript quotes it back as its reasoning. And
  the generator's "before the promotion" commit used a string replacement that
  matched nothing, so `discount_for` was present from the first commit and the
  commit whose message says it applies the promotion never touched `rules.py`.
  Both are fixed in `bench/fixture/make_fixture.py`; the recorded v0.1 results
  are left exactly as they were measured.

## [0.1.1] — 2026-09-15

### Changed

- **The npm package installs itself.** `npm install -g velra` and `npx velra`
  no longer depend on the unpublished `@velra/cli-*` platform packages. The
  launcher resolves the host target, downloads the matching release archive
  from GitHub Releases, verifies its published SHA-256, extracts it (gzip/tar
  and zip are decoded in-process — no external `tar`, no npm dependencies) and
  caches it at a stable path under `$VELRA_HOME/cache/v<version>/<target>/`.
  Resolution order: `$VELRA_BINARY`, a vendored binary beside the launcher, the
  cache, then the network. `VELRA_NO_DOWNLOAD=1` keeps it offline.
- **Removed `rust-toolchain.toml`.** Pinning a bare channel there resolved
  against each machine's *default host* triple, which silently forced
  windows-gnu builds — and `dlltool.exe` failures — on Windows contributors who
  had MSVC available. Builds now use whatever host toolchain is installed. CI
  runs `stable` on Linux, macOS and Windows, and a separate job compiles
  against the declared MSRV of 1.98.

### Added

- **`cargo binstall velra`.** `package.metadata.binstall` maps every released
  target to its archive, so Rust users get a prebuilt binary without a C
  compiler, Visual Studio Build Tools or a linker. Windows-GNU hosts are mapped
  to the statically linked MSVC binary.
- **`.gitattributes`.** Pins LF on the shell scripts and the npm launcher. The
  committed blobs were already LF, but a Windows checkout could materialise
  them with CRLF, and running such a copy under WSL or Linux fails with
  `set: Illegal option -`.

### Fixed

- `.gitignore` excluded `/npm/**/bin/`, so the npm launcher - the package's
  only entry point - was never tracked. The rule now ignores binaries dropped
  into that directory without swallowing its source.
- Unrendered `{{VELRA_DOMAIN}}` placeholders in the npm launcher's error paths.
- Installer self-documented URLs pointed at `github.com/.../install.sh`, which
  404s; they now point at the raw content URL that actually serves the script.
- `install.sh` ran `velra enable` a second time while building its own failure
  message — backticks inside a double-quoted string are command substitution.
- `build.rs` no longer declares a `rerun-if-changed` on `../../.git/HEAD` when
  that path does not exist, which forced a rebuild on every run of an installed
  crate.

## [0.1.0] — 2026-09-14

First release. One job: your task survives `/compact` in Claude Code.

### Added

- **Continuation Capsule.** At `PreCompact`, Velra freezes an immutable
  checkpoint and delivers a deterministic, zero-LLM `<VELRA_CONTINUATION>`
  block (bounded at 800 estimated tokens, hard ceiling 1,000) into the
  post-compaction context, carrying the root objective, the active failure,
  dead ends already tried, recent edit attempts and the working file set.
- **Two-command setup.** `velra enable` merges hook registrations into your
  user-level Claude Code settings, preserving comments, trailing commas and
  indentation, with a timestamped backup and an atomic write. `velra disable`
  restores the file byte for byte.
- **Delivery state machine.** PENDING → ATTACHED → CONFIRMED with three
  channels (`SessionStart(compact)`, the next `PostToolUse`, the next
  `UserPromptSubmit`), idempotent injections, and at most one live continuation
  per session. Auto-compaction needs no user action.
- **Evidence tracking.** Intent hierarchy (root/subtask/latest request), file
  version history by content hash, revert and `git restore`-family discard
  detection, dead-end grouping, and test/build/lint outcome tracking with a
  deterministic failure excerpt.
- **Local-first storage.** Append-only SQLite (WAL) event log with a no-loss
  spool fallback and an incremental, cursor-based reducer driven by async hooks.
- **CLI.** `enable`, `disable`, `status`, `inspect`, `doctor`, `--version`.
- **Redaction.** Secrets are replaced before anything is written, including the
  spool; sensitive paths are recorded as path and hash only.
- **Distribution.** Single static binary for macOS (arm64, x86_64), Linux
  (x86_64, aarch64, static musl) and Windows (x86_64, aarch64), with shell and
  PowerShell installers, SHA-256 verification and build provenance attestations.

### Fixed

Five defects found by the empirical benchmark in
[BENCHMARK_REPORT.md](BENCHMARK_REPORT.md), all of them in what reaches the
capsule rather than in how it is delivered. The benchmark's §15 has the detail.

- **The capsule now fits its budget.** `estimate_tokens` assumed 3.2 characters
  per token; the capsule's real density is 2.07–2.16, so it under-read by about
  a third and the truncation ladder never ran. Two blocks that Claude Code
  actually received measured 951 and 1,147 tokens against a declared ceiling of
  800. The estimator now walks the text by run, and both measured capsules are
  kept as regression fixtures.
- **A dead end could silently disappear from the capsule.** A `git_pre`
  snapshot, which by construction holds the content that was discarded, could
  arrive after the turn scan that recorded the revert — spooled events keep
  their original timestamp but get a fresh row id — and be read as proof the
  change had come back. `[DEAD_ENDS]` was then filtered out of a delivered
  capsule entirely. Re-application now requires a settled observation that is
  not older than the dead end, and version history is read in the order things
  happened rather than the order they were stored.
- **A revert chained with a failing command was lost.** `git restore x &&
  pytest` is reported as one failed tool call whenever the suite still fails,
  which is the ordinary shape of discarding an attempt; git effects were read
  only from calls that succeeded.
- **`git` was only matched as the first word of a command.** The `PreToolUse`
  registration used an `if` rule of `Bash(git *)`, a prefix match that never saw
  `cd "…" && git restore …`. The filter moved into the binary, which parses the
  whole command line.
- **An unrelated read sweep could evict the files the task is about.** Working
  files were ranked with recency as the tie-break, so an audit across dozens of
  modules read once each pushed out the files named in the failing traceback.

Also fixed while tracing those: a failed `git commit` marked its edits
COMMITTED; `absent` and `unreadable` were accepted as matching content hashes;
the redaction prefilter indexed a hand-sized array; and quoted commands spent
their character allowance on a leading `cd` into an absolute path.

### Changed

- The Windows performance budget is stated as marginal cost over an empty run
  rather than as total wall time. On a machine where starting a process costs a
  p99 of 7–25 ms, a 15 ms wall-time allowance is not a statement about Velra.
  Velra's own marginal cost is +3.4 to +11.0 ms at p50 and barely moves between
  a 0.5 MiB and a 38 MiB database.
- Database schema v2 adds `file_stats.first_touch_ms`. Existing databases
  migrate in place on first open.

### Guarantees

- Hooks always exit 0, never write to stderr, and print either nothing or one
  JSON object.
- An internal watchdog abandons work at 250 ms (synchronous hooks) so Claude
  Code is never delayed.
- No network access at runtime, no telemetry, no LLM calls, and nothing is ever
  written inside your repository.

[Unreleased]: https://github.com/aryanghai12/velra/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aryanghai12/velra/releases/tag/v0.1.0
