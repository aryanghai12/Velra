# Changelog

All notable changes to Velra are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Velra adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — unreleased

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
