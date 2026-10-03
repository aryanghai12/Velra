# Changelog

All notable changes to Velra are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Velra adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] — Unreleased

Not yet published: the latest release on GitHub Releases, npm and
crates.io is 0.1.1. See [docs/RELEASING.md](docs/RELEASING.md).

### Release notes

**Clear the context. Keep the state. Continue working.**

0.1.2 turns Velra from a `/compact` companion into a cross-session
continuation layer. You can leave a long Claude Code session, run
`velra restore`, and start a brand-new session that begins with a bounded,
deterministic record of where the work stands: the objective, the
constraints you stated, per-test status, what was tried and reverted, and
the next step. Nothing is replayed from the old conversation and no model
is involved. The in-session `/compact` continuation from 0.1.x remains, as
one use of the same machinery.

Most of the work in this release went into making that path trustworthy:
twelve hardening phases (DECISIONS D70–D148) on prompt handling, event
order, revert provenance, snapshot selection, rendering, staging and
delivery, storage and concurrency, security, Windows process handling, and
adversarial and lifecycle testing.

What is and is not guaranteed is written down in one place:
[docs/GUARANTEES.md](docs/GUARANTEES.md).

**Upgrading.** Run `velra enable` once after installing 0.1.2. The database
migrates from schema v2 to v3 on first use (it adds the `constraints`
table). Migrations are forward-only: 0.1.1 cannot use the database
afterwards.

### Added

- **`velra restore`: explicit cross-session restore.** Pick a previous
  session of this workspace (or name it with `--session <id>`); Velra
  renders its operational state into a capsule of 740 estimated tokens by
  default, never more than 1,000, and stages it under
  `~/.velra/staged/<workspace_id>/`. `--list`, `--dry-run`, `--clear` and
  `--json` make it scriptable. [docs/RESTORE.md](docs/RESTORE.md).
- **`SessionStart` delivery of a staged capsule.** The next new session in
  that workspace (source `startup`) claims it at most once and receives it
  as `additionalContext`, with a `systemMessage` naming the source session.
  `clear`, `resume`, `compact`, `fork` and unknown sources leave it staged.
  It expires after 7 days. Delivery fails open.
- **Workspace and session model.** The workspace (`CLAUDE_PROJECT_DIR`,
  else the git root, else the directory) owns sessions and at most one
  staged capsule, resolved by one function shared by the hooks and the CLI.
  Source sessions stay first-class; the destination keeps its own identity.
- **Constraint and rejection ledger.** Constraints and rejected approaches
  the user states are extracted deterministically from the whole prompt,
  quoted verbatim, and printed under `[STATED_CONSTRAINTS]` and
  `[REJECTED_APPROACHES]` (D77–D79, D94, D98–D100).
- **Exact-identifier retention.** Per-test status by exact test id
  (`[TEST_STATUS]`), the most recent earlier message that names code
  (`[EARLIER_MESSAGE]`), and a named truncation ladder that sheds
  regenerable prose before identifiers (D64–D66, D71–D73).
- **`velra inspect --trace <MARKER>`** reports, layer by layer from the
  transcript to the staged capsule, where a fact was lost and why (D67,
  D74, D88). `inspect --session` accepts a unique prefix of 8 or more
  characters (D147).
- **`velra status`** shows a staged capsule for this workspace, its source
  and its claim state, and the latest continuation with what its state
  establishes (D113).
- **Recorded `SessionStart` fixtures** for Claude Code 2.1.272.
- **Benchmark evidence.** A preregistered live requalification
  (`velra-tokenburn` v1.1.0), an offline replay of its source ledgers
  through the release binary, and hook latency on the release binary
  ([docs/BENCHMARK.md](docs/BENCHMARK.md)); tools `bench/tokenburn/replay.py`,
  `bench/legacy/spawn_floor.py`, `scripts/doc_assets.py`.
- **Documentation**: new [GUARANTEES](docs/GUARANTEES.md),
  [DEVELOPMENT](docs/DEVELOPMENT.md) and [TESTING](docs/TESTING.md) pages;
  every other active page rebuilt against the implementation.

### Changed

- **Positioning.** The CLI, package metadata and documentation describe
  Velra as continuation across sessions, not as a `/compact` helper (D149).
- **Restored capsules name their source.** A capsule rendered by
  `velra restore` says it is another session's record and points at
  `velra inspect --session <source prefix>` (D147). Earlier builds told the
  new session they were its own prompts.
- **Prompt handling.** Context a client injects around a prompt (for
  example `<ide_opened_file>`) is not treated as the user's words (D70,
  D75). Slash commands, pasted material and multi-paragraph prompts are
  classified by explicit rules (D76, D80). The prompt hook stays inside its
  watchdog on very large prompts (D81).
- **Event order.** Projections fold events in logical order; a late
  spooled event triggers a bounded rebuild, and projection times are
  logical time (D82–D87, D119).
- **Revert provenance.** Reverts are credited only to commands that can
  reach the file, detected only by content digest, and a reapplied dead end
  is closed by the file's content, not its cause (D89–D92, D121).
- **Rendering.** Dead ends show the edit that was actually rejected; long
  messages keep their closing request; rule lists say how many they omit;
  `[FAILURE_LOCATION]` is used only for a location taken from failing output
  (D93–D104).
- **Staging.** One immutable record per stage, claimed by exclusive file
  creation that nothing takes over; delivery is at most once, and a
  killed delivery is reported, not repeated (D105–D109).
- **In-session continuation.** A delivery key already recorded writes
  nothing; continuations older than 7 days are not written; the watchdog
  cannot end a hook between writing a capsule and committing it; the path
  is described as once per delivery point, not exactly once (D110–D115).
- **Event dedupe.** A tool event's dedupe key uses its `tool_use_id` and not
  the clock, so one tool call seen by two registered copies of the hook is
  stored once (D148).
- **CLI contract.** `--json` output is always one ASCII JSON document on
  stdout; a closed stdout ends output quietly; `inspect --session` for an
  unknown session fails (D125–D127, D136).

### Fixed

- Storage: malformed spool files and payloads no longer stop the reducer;
  `velra status` reports a database hooks cannot use; migrations roll back
  whole (D116–D118).
- A corrupt database is rotated by one process at a time, and on POSIX an
  open verifies it still holds the file it named (D137).
- Hooks no longer lose an event silently when the watchdog fires while the
  database is opening (D145).
- The lock budget is enforced against the clock by Velra's own busy handler
  (D133, D146).
- Settings edits survive a concurrent writer except in a narrow, measured
  window; backups are written atomically; stale temp files are cleaned up
  (D128, D134, D143, D144).
- Windows: `claude --version` is never run from the current directory and
  is killed with its child processes at the deadline; `cmd` and PowerShell
  line continuations, a byte order mark in settings or hook input, and
  non-ASCII paths are handled (D129–D132, D135, D138).

- The Token-Burn readiness gate's own restore/delivery smoke
  (`bench/tokenburn/smoke.py`) still read the staged file name used before
  D105, so the gate blocked and its "gone once claimed" check passed without
  testing anything. It now reads the record `velra restore` reports and the
  workspace's `capsule.<gen>.json` records.

### Security

- Redaction covers armored PGP private keys, orphaned private-key tails,
  `Authorization: Basic` headers and credentials passed as flag arguments,
  and matches prefixed tokens after escapes (D122, D140).
- Output tails and edit excerpts are redacted before they are cut, so a cut
  cannot split a secret past its detector (D123, D139, D141).
- Every log line is redacted before it is written (D124).

### Known limitations

- The capsule is a bounded rendering of recorded operational state, not the
  conversation. Assistant reasoning is never captured; the ladder drops
  detail, the user's wording before exact identifiers.
- Restore delivery is at most once: a session start killed mid-delivery
  delivers nothing, and the capsule must be restaged.
- Events without a `tool_use_id` (prompts, session starts, stops) can still
  be stored twice under duplicate hook registration, and an older binary
  registered alongside writes its old dedupe key (D148).
- Hooks exceeded their latency budgets on the measured Windows machine;
  `PreCompact` remains the furthest over (open since v0.1).
- The live benchmark is four qualification pairs on one machine, model and
  scenario family, run on build `51b96cb` before this release's hardening;
  its baseline is a fresh session, not `--resume`.
- macOS is not verified on the release tree: the last macOS CI run
  (`108c70d`) failed, and the fix (D146) has not run there.
- Full list: [docs/GUARANTEES.md](docs/GUARANTEES.md).

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
