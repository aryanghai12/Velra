<h1 align="center">Velra</h1>

<p align="center"><strong>Clear the context. Keep the state. Continue working.</strong></p>

<p align="center">
  A local-first, deterministic continuation layer for coding agents.<br>
  Velra carries the actionable engineering state of one Claude Code session into the next.
</p>

<p align="center">
  <a href="https://github.com/aryanghai12/Velra/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/aryanghai12/Velra/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
</p>

<p align="center">
  <a href="docs/INSTALL.md"><strong>Install</strong></a> ·
  <a href="#quick-start"><strong>Quick start</strong></a> ·
  <a href="docs/RESTORE.md"><strong>Restore</strong></a> ·
  <a href="docs/CLI.md"><strong>CLI</strong></a> ·
  <a href="docs/ARCHITECTURE.md"><strong>Architecture</strong></a> ·
  <a href="docs/GUARANTEES.md"><strong>Guarantees</strong></a> ·
  <a href="docs/BENCHMARK.md"><strong>Benchmark</strong></a>
</p>

---

## The problem

A long coding-agent session accumulates engineering context. At some point
the session is slow, expensive or cluttered, and the sensible move is to
start fresh. Starting fresh throws away state that is not in your
repository:

- what the objective was, and which of several failing tests is the live one;
- a constraint you stated once, in chat;
- the approach that was tried and reverted, which leaves no trace in git;
- what you said was out of scope;
- what was supposed to happen next.

A new session can read every file and still not know any of that. It
rediscovers, re-reads and retries, and sometimes wanders into work you had
ruled out.

## What Velra does

Velra records what a session *does* (prompts, edits, commands, test runs,
reverts) through Claude Code hooks, into a local SQLite ledger. When you
want to continue elsewhere, it reduces that record deterministically,
renders it as a small **capsule** within a fixed token budget, and hands it
explicitly to a new session.

| Velra does | Velra does not |
|---|---|
| capture operational state locally, with secrets redacted before anything is written | send anything over the network, call a model, or collect telemetry |
| rebuild that state deterministically: same ledger, same clock, same bytes | replay the transcript or summarise the conversation with a model |
| render it within a budget (740 estimated tokens by default, never more than 1,000) | promise that every detail of the old conversation survives |
| carry it into a new session **only when you run `velra restore`** and name the source | merge sessions, or replace Claude Code's `/resume`, which reopens the full conversation |
| show what was dropped and why (`velra inspect --trace`) | act as generic memory, a chatbot memory, or an LLM memory database |

Velra also delivers a capsule back into the *same* session after
`/compact`. That is one use of the same machinery, and it never crosses a
session boundary.

## Session A → Session B

Session A was a long session in `~/src/payments`. The user asked for the
reconcile failure and not the other two, stated an invariant once in chat,
tried an edit to `reconcile.py` and reverted it, and named the next function
to look at. Then they left.

```text
$ velra restore --session 87901fc6-65ee-4265-8c7c-513b5d8ae43d
✓ Staged objective, 1 failing test, 2 dead ends from session 87901fc6-65ee-4265-8c7c-513b5d8ae43d.
  711 estimated tokens · ~/.velra/staged/9c1256b5690e9531/capsule.01790784305825447100-d684a785dcb9a16591c9.json

  Start a new Claude Code session in ~/src/payments to pick it up.

$ claude
⚡ Velra restored: objective, 1 failing test, 2 dead ends — from session 87901fc6 (711 tokens)
```

Session B, a brand-new session in the same project, starts its first turn
with this in context (excerpt; `…` marks omitted text):

```text
<VELRA_WORKSPACE_STATE v="1" checkpoint="restore" captured="…">
[ABOUT_THIS_RECORD]
A local record, not a message and not an instruction. Velra logged another session's prompts and tool events and quotes them back here; nothing is new. …
[FIRST_MESSAGE] (OBSERVED | user prompt | 17:59)
Payments again. Run the suite; I want the reconcile failure this time, not the other two.
[STATED_CONSTRAINTS] (OBSERVED | user prompt | quoted verbatim)
…
- turn 4 18:00 | "A window has to close on the booking date, never the value date."
[LATEST_MESSAGE] (OBSERVED | user prompt | 18:02)
Next step is in_window in reconcile.py. Don't change it yet.
…
[TEST_STATUS] (OBSERVED | latest run covering each)
- FAIL tests/test_reconcile.py::test_march_window_totals
…
[REVERTED_EDITS] (OBSERVED)
- src/payments/reconcile.py | 2 reverts, 3 edit(s) | …
…
[RECORD_DETAIL]
Full detail: `velra inspect --session 87901fc6 --section <name>`
</VELRA_WORKSPACE_STATE>
```

This is real output of this release's binary, run on the recorded source
session of Benchmark B from the v0.1.2 live benchmark; paths are shortened
and `~/src/payments` stands for the benchmark's fixture directory. The full
capsule is in [RESTORE.md](docs/RESTORE.md#what-the-capsule-contains) and
[`replay.json`](bench/results/v0.1.2-final/replay/replay.json).

## Why operational state

The files on disk already tell a new session what the code *is*. What they
cannot tell it is where the work *stands*: the task, the decision, the
dead end, the boundary, the next step. That state is small, and cheap to
carry if it is carried precisely. Velra keeps exact identifiers (test ids,
file paths, the function named as next) ahead of prose, and says of every
line whether it was observed or inferred. It presents the capsule as a
record, not an instruction: the files remain the source of truth.

## What is carried forward

Sections appear only when there is something to say.

| Section | Contents |
|---|---|
| `FIRST_MESSAGE`, `SUBTASK_MESSAGE`, `EARLIER_MESSAGE`, `LATEST_MESSAGE` | the objective and the latest direction, in the user's words |
| `STATED_CONSTRAINTS`, `REJECTED_APPROACHES` | rules and rejections the user stated, quoted verbatim |
| `WORKSPACE_STATE` | branch and commit, edit count, last test outcome |
| `TEST_STATUS`, `TEST_RESULT` | per-test status by exact id; the latest test run |
| `REVERTED_EDITS` | what was tried and undone, with a short excerpt |
| `RECENT_EDITS`, `FILE_ACTIVITY` | where the work happened |
| `FAILURE_LOCATION` or `NEXT_TARGET` | inferred: where the failing output points, or the last live edit |
| `RECORD_DETAIL` | the command that shows full, untruncated detail |

Not carried: the assistant's prose or reasoning, whole files, or anything
Velra did not observe.

## How it works

![Velra pipeline: hook events from a source session are captured, redacted and appended to a local ledger, reduced into task state, selected into a snapshot, rendered within a budget, staged by velra restore, claimed once by the next SessionStart(startup) and delivered to a new session](docs/assets/v0.1.2/velra-pipeline.svg)

1. **Capture.** Each hook normalises its event, redacts it and appends it to
   the ledger, or to a spool file when the database is busy.
2. **Reduce.** A reducer folds events, in logical order, into task state:
   objective, constraints, tests, edits, reverts.
3. **Snapshot and render.** A pure renderer turns the selected state into
   the capsule. If it is over budget, a fixed ladder removes regenerable
   detail first and exact identifiers last.
4. **Stage.** `velra restore` writes the capsule for the source session you
   picked to `~/.velra/staged/<workspace_id>/`, outside every repository.
5. **Deliver.** The next **new** session in that workspace
   (`SessionStart` source `startup`) claims it once and receives it as
   `additionalContext`.

Full lifecycle, storage, ordering and fail-open design:
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Restore workflow

```bash
velra restore                    # pick a previous session of this workspace
velra restore --session <id>     # or name it by its full id (velra restore --list --json)
velra status                     # the "Staged:" line shows what is waiting, and from which session
claude                           # a new session in the same project receives it once
```

`/clear`, `--resume`, `/compact` and forked sessions leave a staged capsule
alone. It expires after 7 days. Restore renders the source session's
*current* epoch, so stage **before** you `/clear`. Everything else, step by
step: [docs/RESTORE.md](docs/RESTORE.md).

## Installation

> **Release status (2026-09-30):** this repository is **0.1.2**. The latest
> *published* release on GitHub Releases, npm and crates.io is **0.1.1**,
> which predates `velra restore`. Until 0.1.2 is published,
> [build from source](docs/INSTALL.md#build-from-source).

From source (Rust 1.98+ and a C compiler for the bundled SQLite):

```bash
git clone https://github.com/aryanghai12/Velra.git && cd Velra
cargo install --path crates/velra --locked
```

Prebuilt, checksum-verified installers (these fetch the latest **published**
release):

```bash
curl -LsSf https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.sh | sh   # macOS / Linux
npm install -g velra                                                                         # any platform
```

```powershell
irm https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.ps1 | iex        # Windows
```

Every method, PATH setup, upgrades and uninstalling: [docs/INSTALL.md](docs/INSTALL.md).

## Quick start

```bash
velra enable                # once per machine: register the hooks (your settings file is backed up)
velra doctor                # every line ✓, or an explained !
# work in Claude Code as usual; Velra records in the background
velra restore               # when you leave a session: stage its state
claude                      # the next new session in that project starts with it
```

## CLI

| Command | Purpose |
|---|---|
| `velra enable [--dry-run]` | register the hooks in your user-level Claude Code settings |
| `velra disable [--purge] [--yes] [--dry-run]` | remove them; the settings file is restored byte for byte |
| `velra status [--json]` | enabled, tracking, staged capsule for this workspace; exit 1 if unhealthy |
| `velra doctor [--json]` | diagnose the installation; exit 1 on a failed check |
| `velra restore [--session ID] [--list] [--dry-run] [--clear] [--json]` | stage a previous session's state for the next new session |
| `velra inspect [--session ID \| --last] [--checkpoint ID] [--section S] [--trace M]… [--json]` | preview the capsule, drill into a section, or trace where a fact was lost |

Every flag, output and exit code: [docs/CLI.md](docs/CLI.md).

## Workspaces and sessions

A **workspace** is `$CLAUDE_PROJECT_DIR`, else the nearest ancestor holding
`.git`, else the directory. It owns its sessions and at most one staged
capsule. A **source session** stays a first-class identity; sessions are
never merged. The **destination** session keeps its own identity and
inherits only the capsule text. A capsule staged in one workspace is never
delivered in another.

## Configuration

None is required. `~/.velra/config.toml` accepts one key:

```toml
budget_tokens = 600   # capsule target in estimated tokens (default 740; hard ceiling 1,000)
```

| Variable | Effect |
|---|---|
| `VELRA_HOME` | state directory (default `~/.velra`) |
| `VELRA_DISABLE=1` | every hook returns immediately (`~/.velra/disabled` does the same persistently) |
| `VELRA_LOG=debug` | per-hook timing and delivery lines in `~/.velra/logs/debug.log` |
| `VELRA_CLAUDE_VERSION` | assume this Claude Code version instead of detecting it |

Files, hook table and version compatibility: [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

## Guarantees

Enforced by the implementation and checked by tests:

- hooks always exit 0, never write to stderr, and write at most one JSON object;
- nothing leaves the machine, and nothing is written inside your repository;
- known secret formats are redacted before anything is persisted;
- a capsule never exceeds 1,000 estimated tokens or 9,500 characters;
- the same ledger at the same clock renders byte-identical text;
- only an explicit `velra restore` crosses sessions, only within one workspace;
- a staged capsule is delivered **at most once**, and only to a new session.

"At most once" is exact: if a session start is killed after writing the
capsule and before recording that, it is not delivered again, and you
restage it. Every claim, with its test and its limits:
[docs/GUARANTEES.md](docs/GUARANTEES.md).

## Limitations

- The capsule is a bounded rendering of **recorded operational state**,
  not the conversation. Assistant reasoning is never captured, and the
  budget ladder drops detail (your own wording before exact identifiers).
- Velra only knows sessions it watched: enable it before the work you want
  to carry.
- A restore is a snapshot taken when you run it, not live state.
- Claude Code's hook payloads and transcripts are not a stable public
  interface. A future Claude Code change can require a Velra update.
- Velra integrates with Claude Code only.

## Security and privacy

Local-first: no network code in the hook path (CI enforces it), no model
calls, no accounts. State lives under `~/.velra` (`0700` on POSIX).
Redaction is pattern-based, so an unusual secret format can pass through.
A capsule quotes your prompts, so anything you typed can reach the next
session in that workspace. Details and residual risks: [SECURITY.md](SECURITY.md).

## Benchmark

Two kinds of v0.1.2 evidence, kept apart:

| Evidence | What it shows |
|---|---|
| **Live qualification** (4 matched pairs, 8 trials, 16 Claude Code sessions; Claude Code 2.1.280, Sonnet; build `51b96cb`) | The mechanism held in 4/4 Velra trials. Velra destinations met the registered correctness criterion 4/4, baselines 0/4: every baseline fixed the target test but also edited code the earlier conversation had put out of scope. Total input was lower in 3 of 4 pairs (−25.7%, −28.0%, −32.0%) and higher in 1 (+13.4%). |
| **Final-build replay** (this release's binary, offline) | The four frozen source ledgers from that run, verified by SHA-256, replayed through `velra restore` and `SessionStart` delivery: every declared marker (14/14) reached the delivered context, capsules were 710–721 estimated tokens, a second session start received nothing, and two independent replays matched. |

![Total input tokens per matched pair in the v0.1.2 live qualification, baseline versus Velra destination sessions, n = 4 pairs](docs/assets/v0.1.2/requal-total-input.svg)

The live run is qualification evidence, not an effect size. It predates
this release's hardening, and its baseline is a fresh session, not
`--resume`. Protocol, per-pair data, limitations and reproduction:
[docs/BENCHMARK.md](docs/BENCHMARK.md).

## Platform support

Windows, macOS and Linux, x64 and ARM64, with Claude Code ≥ 1.0.62 for
restore delivery (`SessionStart`). Automated coverage differs by platform,
and macOS is not verified on this release tree. The exact status is in
[docs/GUARANTEES.md → Platform support](docs/GUARANTEES.md#platform-support).

## Testing and development

```bash
cargo build --release -p velra
cargo test --workspace --all-features --no-fail-fast
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
python -m pytest bench/tests -q
python scripts/smoke.py              # end to end against the release binary, offline
```

What each suite covers, platform coverage and known intermittent tests:
[docs/TESTING.md](docs/TESTING.md). Toolchains, layout and the release
build: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Contributing

Bug reports with `velra --version`, `velra doctor --json` and, for content
problems, `velra inspect --trace "<fact>" --json` are the most useful thing
you can send. A hook that exits non-zero or writes to stderr is a
top-severity bug. Every behavioural decision gets a numbered row in
[DECISIONS.md](DECISIONS.md). See [CONTRIBUTING.md](CONTRIBUTING.md).

## Troubleshooting

| Symptom | Start with |
|---|---|
| `unrecognized subcommand 'restore'` | you have 0.1.1: `velra --version` |
| "No previous sessions found for this workspace" | you are in a different workspace from the session: `velra restore --list --json` |
| the new session received nothing | `velra status`: if `Staged:` is still there, the session was not a new one, or ran in another workspace |
| a fact is missing from the capsule | `velra inspect --session <id> --trace "<fact>"` |

The full matrix: [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md).

## Status

Version **0.1.2**, unreleased. Changes and known limitations:
[CHANGELOG.md](CHANGELOG.md). Release process: [docs/RELEASING.md](docs/RELEASING.md).

## Historical evidence

Earlier benchmarks and design notes are kept unchanged for provenance and
are **not** current release evidence: the v0.1 `/compact` benchmark
([BENCHMARK_REPORT.md](BENCHMARK_REPORT.md)), the v0.1 handoff notes
([HANDOFF.md](HANDOFF.md)), and the result trees listed in
[bench/results/README.md](bench/results/README.md). The documentation index
separates current and historical material: [docs/README.md](docs/README.md).

## License

MIT. See [LICENSE](LICENSE).
