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
  block (≤ 800 estimated tokens) into the post-compaction context, carrying the
  root objective, the active failure, dead ends already tried, recent edit
  attempts and the working file set.
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

### Guarantees

- Hooks always exit 0, never write to stderr, and print either nothing or one
  JSON object.
- An internal watchdog abandons work at 250 ms (synchronous hooks) so Claude
  Code is never delayed.
- No network access at runtime, no telemetry, no LLM calls, and nothing is ever
  written inside your repository.

[Unreleased]: https://github.com/{{GITHUB_ORG}}/velra/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/{{GITHUB_ORG}}/velra/releases/tag/v0.1.0
