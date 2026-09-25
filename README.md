<h1 align="center">Velra</h1>

<p align="center"><strong>Clear the context. Keep the state. Continue working.</strong></p>

<p align="center">
  Local-first session continuity for Claude Code. Velra records what a session <em>does</em>,
  and hands a small, bounded record of where the work stands to the next context,
  whether that is after <code>/compact</code> or in a brand-new session, without replaying the conversation.
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
  <a href="docs/TROUBLESHOOTING.md"><strong>Troubleshooting</strong></a> ·
  <a href="docs/ARCHITECTURE.md"><strong>Architecture</strong></a> ·
  <a href="docs/BENCHMARK.md"><strong>Benchmark</strong></a>
</p>

---

## The problem

Long Claude Code sessions get slow, expensive and cluttered. The sensible
move is to leave: run `/clear`, or start a new session. But leaving throws
away the small amount of state you still need, and none of it is in your
repository:

- which of several failing tests you were actually working on;
- a constraint you stated once, in chat ("the key must stay a pure function
  of the request");
- the approach you tried and reverted, so it leaves no trace in git;
- what you said was out of scope ("leave the ledger failures alone");
- what you were about to do next.

A fresh session can read every file and still not know any of that. So it
re-discovers, re-reads, re-tries, and sometimes wanders into work you had
ruled out.

## What Velra is

Velra is a single native binary that registers
[Claude Code hooks](docs/CONFIGURATION.md#hook-registration). While you work,
it records what the session does (prompts, edits, commands, test results,
reverts) into a local SQLite ledger. From that ledger it renders a
**capsule**: a bounded (~700 token), deterministic, provenance-tagged record
of the task's operational state. It delivers the capsule where it is needed:

- **across a new session**: `velra restore` stages a previous session's
  capsule, and the next new session in that workspace receives it at
  `SessionStart`;
- **across `/compact`**: a capsule is frozen before compaction and delivered
  once after it, in the same session.

| Velra is | Velra is not |
|---|---|
| local-first: no network at runtime, no accounts, no telemetry | a cloud memory service |
| a bounded operational-state handoff between Claude Code sessions | generic conversational memory |
| built from recorded tool events and your prompts | transcript replay, or a summary written by a model |
| deterministic: same ledger, same bytes | an LLM that "remembers everything" |
| explicit: you choose which session to carry forward | a replacement for `/resume`, which reopens the full conversation |
| honest about loss: `velra inspect --trace` shows what was dropped and why | a guarantee that every detail of the old conversation survives |

## How it works

```
Claude Code session A ──hooks──▶ velra ──▶ local ledger (~/.velra/velra.db)
                                              │   reducer: objective, constraints, tests,
                                              │   edits, reverts, next step
                                              ▼
                     velra restore ──▶ bounded capsule, staged for this workspace
                                              │
Claude Code session B ── SessionStart(startup) ◀┘  delivered once, never twice
        └─▶ starts with the capsule in context and continues the work
```

The capsule is a record, not an instruction. It says what was observed and
tells the agent that files on disk are the source of truth. See
[Architecture](docs/ARCHITECTURE.md) for the full lifecycle.

## Requirements

- **Claude Code** with hooks. Restore delivery uses `SessionStart` (Claude
  Code ≥ 1.0.62). The benchmark ran on 2.1.280.
- **Windows** (x64, ARM64), **macOS** (Apple silicon, Intel) or **Linux**
  (x64, aarch64). CI tests all three.
- **Velra 0.1.2 or later for `velra restore`.**

> **Release status (2026-09-23):** this repository is **0.1.2**. The latest
> *published* release (GitHub Releases, npm, crates.io) is **0.1.1**, which
> predates `velra restore`. Until 0.1.2 is published,
> [build from source](docs/INSTALL.md#build-from-source) to use restore.

## Installation

Prebuilt, checksum-verified, no compiler needed. These install the latest
**published** release:

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.ps1 | iex
```

```bash
# macOS / Linux
curl -LsSf https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.sh | sh

# any platform
npm install -g velra        # or: cargo binstall velra
```

From source (Rust 1.98+ and a C compiler for the bundled SQLite):

```bash
git clone https://github.com/aryanghai12/Velra.git && cd Velra
cargo install --path crates/velra --locked
```

Then register the hooks once per machine, and check:

```bash
velra enable
velra --version && velra doctor
```

The full guide covers PATH setup, every option, upgrades and uninstalling:
**[docs/INSTALL.md](docs/INSTALL.md)**.

## Quick start

```bash
# 1. Once: register the hooks.
velra enable

# 2. Work in Claude Code as usual. Velra records in the background.
cd ~/src/payments && claude
#    … investigate, try something, revert it, decide the next step …
#    then leave: exit, or /clear. The session has grown too big.

# 3. Stage that session's state for your next one.
velra restore
#    Velra — Restore previous session
#    1. Payments again. Run the suite; I want the reconcile failure this time...
#       8790…e43d · Last activity: 2026-09-23T12:32:23Z · state: yes · ~544 KB
#    Select [1-1] (q to cancel): 1
#    ✓ Staged objective, 1 failing test, 2 dead ends, 8 files from session 87901fc6-….
#      680 estimated tokens · ~/.velra/staged/9c1256b5690e9531/capsule.<gen>.json

# 4. Start a brand-new session in the same project.
claude
#    ⚡ Velra restored: objective, 1 failing test, 2 dead ends, 8 files — from session 87901fc6 (680 tokens)
> Continue the task.
```

After `/compact`, you don't have to do anything: the capsule is delivered in
the same session automatically.

## CLI

| Command | Purpose |
|---|---|
| `velra enable [--dry-run]` | register the hooks in your user-level Claude Code settings (backed up first) |
| `velra disable [--purge] [--yes] [--dry-run]` | remove them; the settings file is restored byte for byte |
| `velra status [--json]` | enabled? tracking? is a capsule staged for this workspace? Exit 1 if unhealthy |
| `velra doctor [--json]` | diagnose the installation; exit 1 on a failed check |
| `velra restore [--session ID] [--list] [--dry-run] [--clear] [--json]` | stage a previous session's state for the next new session |
| `velra inspect [--session ID \| --last] [--checkpoint ID] [--section S] [--trace M]… [--json]` | preview the capsule, drill into a section, or trace where a fact was lost |

Every flag, output and exit code: **[docs/CLI.md](docs/CLI.md)**.

## `velra restore`

`velra restore` resolves the workspace from your current directory and lists
that workspace's sessions (newest first; `state: yes` marks restorable ones).
It builds a bounded capsule from the session you pick and stages it atomically
at `~/.velra/staged/<workspace_id>/capsule.<gen>.json`. The next **new** session
there (`SessionStart` with source `startup`) receives it, once at most. `/clear`,
`--resume` and `/compact` leave it staged, and it expires after 7 days.

To restore across `/clear`, stage **before** clearing: a restore renders the
session's current epoch, and `/clear` starts a new, empty one.

The full walkthrough covers workspaces, the capsule format, stale handling,
one-shot delivery and debugging: **[docs/RESTORE.md](docs/RESTORE.md)**.

## SessionStart delivery

| `SessionStart` source | Staged restore capsule | In-session `/compact` continuation |
|---|---|---|
| `startup` (new session) | **delivered once** | — |
| `compact`, `resume` | left staged | delivered |
| `clear` | left staged | expired (the recorded state is kept) |
| `fork`, unknown | left staged | — |

Delivery is a single JSON object (`hookSpecificOutput.additionalContext` plus
a one-line `systemMessage` naming the source session).

## Workspaces and sessions

A **workspace** is `$CLAUDE_PROJECT_DIR`, else the nearest ancestor with
`.git`, else the directory. It owns its sessions and at most one staged
capsule. A **source session** stays a separate, first-class identity; sessions
are never merged. The **destination** session inherits only the capsule text.
[More](docs/ARCHITECTURE.md#workspaces-and-sessions).

## Configuration

None is required. Optional `~/.velra/config.toml`:

```toml
budget_tokens = 600   # capsule target, estimated tokens (default 740; hard ceiling 1000)
```

| Variable | Effect |
|---|---|
| `VELRA_HOME` | state directory (default `~/.velra`) |
| `VELRA_DISABLE=1` | kill switch for every hook (`~/.velra/disabled` does the same persistently) |
| `VELRA_LOG=debug` | per-hook timing and delivery lines in `~/.velra/logs/debug.log` |
| `VELRA_CLAUDE_VERSION` | assume this Claude Code version instead of detecting it |
| `CLAUDE_CONFIG_DIR`, `CLAUDE_PROJECT_DIR` | honoured as Claude Code honours them |

Files, hook table, version compatibility and failure behaviour:
**[docs/CONFIGURATION.md](docs/CONFIGURATION.md)**.

## Debugging

```bash
velra doctor                                      # installation health
velra status                                      # is a capsule staged here? from which session?
velra restore --dry-run --session <id>            # exactly what would be staged
velra inspect --session <id> --trace "in_window"  # where a fact was lost, and why
```

`--trace` walks a string through each layer (transcript → stored events →
ledger → snapshot → renderer → restore → staged capsule). It reports the
first layer where it disappeared, with a computed reason such as "not
captured: assistant prose is not hooked" or "budgeted out by ladder rung
`working_files 8->4`".

## Troubleshooting

The common cases:

| Symptom | Where to look |
|---|---|
| `velra: command not found` | PATH; open a new terminal ([details](docs/TROUBLESHOOTING.md#installation)) |
| `unrecognized subcommand 'restore'` | you have 0.1.1; see [Which version you get](docs/INSTALL.md#which-version-you-get) |
| "No previous sessions found for this workspace" | you are in a different workspace from the one the session ran in |
| Restore succeeded but the new session got nothing | `velra status` still shows `Staged:` → the next session was `--resume`/`/clear`, another workspace, or hooks were off |
| A fact is missing from the capsule | `velra inspect --trace "<fact>"` |

The full diagnostic matrix covers symptom, cause, diagnostic command,
expected output and fix: **[docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md)**.

## Safety and failure behaviour

- **Hooks fail open.** They always exit 0, never write to stderr, and write at
  most one JSON object. A 250 ms watchdog bounds how long a synchronous hook
  can hold Claude Code up. Typical hooks take a few milliseconds; the
  `PreCompact` latency budget is a known open item.
  A locked, corrupt, read-only or missing database never surfaces as a
  Claude Code error.
- **Nothing leaves your machine.** No network code in the hook path (CI
  enforces it), no LLM calls, no telemetry.
- **Redaction before persistence.** API keys, tokens, private keys and
  credential URLs are replaced before anything is written. Sensitive files
  (`.env`, `*.pem`, `id_rsa*`, `.ssh/**`, …) are stored as path and hash only.
- **Nothing is written inside your repository.** The only file Velra edits is
  your user-level Claude Code settings file, backed up first and restored byte
  for byte by `velra disable`.

Details: [SECURITY.md](SECURITY.md).

## Benchmark

Velra v0.1.2 was requalified against a preregistered protocol
(`velra-tokenburn` v1.1.0) on Claude Code 2.1.280 with Sonnet: **4 matched
pairs, 8 trials, 16 sessions**, all valid. Claude Code auto-memory was
disabled and verified clean for both arms. Each trial is a large source
session (≈250K tokens of synthetic context, a proxy, not an observed context
size), then a transition (new session or `/clear`), then a fresh destination
session given one identical prompt. The baseline destination has the
repository. The Velra destination also has the restored capsule.

| | Result |
|---|---|
| Velra mechanism (retain → stage → deliver → receive → use → correct) | held in **4/4** Velra trials |
| Met the registered correctness criterion | Velra **4/4**, baseline **0/4** |
| Total input burden | lower in **3 of 4** pairs (−25.7%, −28.0%, −32.0%), higher in 1 (+13.4%) |
| Steps to first correct action | Velra 2, 2, 2, 4 vs baseline 6, 7, 4, 5 |
| Capsule size | 680–737 estimated tokens |
| Pair verdicts | 3 VELRA_WIN, 1 INCONCLUSIVE |

What the correctness gap means: every baseline *did* make the target test
pass without reintroducing the reverted approach. It failed because it also
edited the modules of the other failing tests, which the earlier conversation
had explicitly put out of scope. Velra's sessions stayed in scope. This is
**qualification evidence** (the preregistration excludes it from the
scorecard). The baseline is a fresh session, not `--resume`, and the savings
are workload-dependent, not guaranteed.

Full research-style report with methodology, per-pair data, causal chain,
limitations and threats to validity: **[docs/BENCHMARK.md](docs/BENCHMARK.md)**.
Frozen evidence: [`bench/results/v0.1.2-requal/`](bench/results/v0.1.2-requal/).
Reproduce: [`bench/README.md`](bench/README.md).

## Limitations

- The capsule is a **bounded summary of recorded operational state**. The
  assistant's reasoning is never recorded, the truncation ladder drops detail
  to stay in budget, and a reverted edit is recorded as *what* changed, not
  *why*.
- Velra only knows sessions it watched: enable it before the work you want to
  carry.
- `velra restore` carries a snapshot taken when you run it. It is not live,
  and it expires after 7 days.
- The benchmark is four qualification pairs on one machine, one model and
  one scenario family. It supports "the mechanism works and helped in these
  scenarios", not an effect size.
- Claude Code's transcript and hook payloads are not a stable public
  interface. Velra parses them tolerantly and detects the Claude Code version,
  but a future Claude Code change can require a Velra update.

## Architecture

Hooks → redacted append-only event log (SQLite, WAL) → reducer projections →
snapshot → pure, budgeted renderer → delivery, either the in-session
`/compact` continuation (exactly-once state machine) or an explicit
cross-session restore (staged file, one-shot claim, `startup` only). Two
crates: `velra` (binary: hooks and CLI) and `velra-core` (the logic, with no
Claude Code I/O). **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**.

## Development

```bash
cargo build --release -p velra
cargo test --workspace --all-features                                # Rust suite
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
python -m pytest bench/tests -q                                      # benchmark pipeline + evidence checks
python bench/tokenburn/run.py --selftest                             # the scoring pipeline on scripted trials
python scripts/smoke.py                                              # end-to-end product pass, offline
```

## Testing

The Rust suite covers the event log, reducer, renderer and truncation
properties, restore, staged delivery and claim races, settings round-trips,
fault injection and fail-open behaviour. It runs on Linux, macOS and Windows
in CI, plus an MSRV job, an IPC fuzz job and a no-network-dependency check.
The Python suite tests the benchmark harness and re-scores the committed
v0.1.2 evidence on every run. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Contributing

Bug reports with `velra --version`, `velra doctor --json` and, for content
problems, `velra inspect --trace … --json` are the most useful thing you can
send. Any hook that exits non-zero or writes to stderr is a top-severity bug.
Every behavioural decision gets a numbered row in [DECISIONS.md](DECISIONS.md).
See **[CONTRIBUTING.md](CONTRIBUTING.md)**.

## Release notes

[CHANGELOG.md](CHANGELOG.md). v0.1.2 adds cross-session restore,
`SessionStart` delivery of staged state, exact-identifier retention,
`velra inspect --trace`, and the requalified benchmark.

## License

MIT. See [LICENSE](LICENSE).

## Links

- [Install](docs/INSTALL.md) · [CLI reference](docs/CLI.md) · [Restore](docs/RESTORE.md) · [Configuration](docs/CONFIGURATION.md) · [Troubleshooting](docs/TROUBLESHOOTING.md) · [Architecture](docs/ARCHITECTURE.md)
- [Benchmark report](docs/BENCHMARK.md) · [Benchmark evidence](bench/results/v0.1.2-requal/) · [Benchmark harness](bench/README.md) · [Result trees, current and historical](bench/results/README.md)
- [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md) · [Design decisions](DECISIONS.md) · [Release process](docs/RELEASING.md)
