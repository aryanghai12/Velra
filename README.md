<h1 align="center">Velra</h1>

<p align="center"><strong>Your task survives <code>/compact</code>.</strong></p>

<p align="center">
  Velra is a local-first tool that remembers what Claude Code was doing — the objective, the failing test,
  and the fixes already tried and rejected — and hands it back the moment the conversation is compacted.
</p>

---

## The problem

Long Claude Code sessions hit compaction. The conversation is summarized, and
the details that matter most are exactly the ones summaries drop:

- **the objective** you gave two hours ago;
- **the failure** you are currently chasing;
- **the dead ends** — the three approaches already tried and reverted.

So the agent re-proposes the change you just rejected, and you re-explain the
task. Velra fixes that one thing.

> Git stores code continuity. Velra stores task continuity.

## How it works

Velra registers Claude Code [hooks](https://code.claude.com/docs/en/hooks) and
watches tool activity — nothing else. It never reads your transcript, never
calls an LLM, and never touches the network.

```
  tool events ──▶ append-only SQLite log ──▶ reducer ──▶ task state
                                                            │
                                    /compact ──▶ PreCompact ─┤ freeze checkpoint
                                                            │
       SessionStart(compact) / next tool call / next prompt ─▶ <VELRA_CONTINUATION>
```

At `PreCompact` it freezes an immutable checkpoint and renders a compact
capsule, bounded at 800 estimated tokens with a hard ceiling of 1,000. After
compaction it injects that capsule back into context, exactly once, through
whichever channel fires first.

Bounded is the point. Across four runs of an identical 16-turn script, Claude
Code's own compaction summary measured 670, 802, 4,410 and 6,937 tokens — a 10×
spread with no ceiling, and twice, in earlier trials, the model declined to
produce one at all. Velra's capsule is rendered from tool events by a pure
function, so it has a size you can predict and cannot be talked out of.

Here is what Claude receives:

```
<VELRA_CONTINUATION v="1" checkpoint="ckpt_01JD2..." captured="2026-09-12T10:04:05Z" trigger="auto">
[CONTEXT]
Velra is a local tool that recorded this task state from Claude Code tool events before the conversation
was compacted. Entries are observations of tool activity. Links between an edit and a later test result are
unconfirmed unless stated. Files on disk are the current source of truth for code.
[ROOT_TASK_OBJECTIVE] (OBSERVED | user prompt | 08:12)
fix the flaky logout test and keep the session cookie behaviour intact
[STATUS]
feat/auth-refactor @ 4f2a1c9 | 7 edits this task | last test run: FAIL
[ACTIVE_FAILURE] (OBSERVED | test run | 10:01)
Command: pytest tests/test_auth.py -x
Result: FAIL (exit 1)
  E   assert response.cookies["session"] is None
  tests/test_auth.py:88: AssertionError
[DEAD_ENDS] (OBSERVED)
- src/auth/session.py | 2 edit(s) | reverted via `git restore src/auth/session.py` at 09:41
    - max_age=None
    + max_age=0
  Observed afterward: `pytest tests/test_auth.py -x` FAIL. Causal link: UNCONFIRMED.
[RECENT_ATTEMPTS] (OBSERVED)
- src/auth/cookies.py | +12/-4 lines | 09:58 | afterward: `pytest tests/test_auth.py -x` FAIL
[WORKING_FILES] (OBSERVED)
- src/auth/cookies.py | edited 4x, read 2x, in failure output
- tests/test_auth.py | edited 0x, read 5x, in failure output
[NEXT_KNOWN_TARGET] (INFERRED | failure-location)
tests/test_auth.py:88
[RECOVERY]
Full detail for any section: `velra inspect --checkpoint ckpt_01JD2... --section <dead-ends|failure|files|attempts>`
</VELRA_CONTINUATION>
```

Every line is an observation of something that actually happened. Velra never
claims an edit *caused* a failure — it says what it saw, and when.

## Install

**macOS / Linux**

```sh
curl -LsSf https://{{VELRA_DOMAIN}}/install.sh | sh
velra enable
```

**Windows**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://{{VELRA_DOMAIN}}/install.ps1 | iex"
velra enable
```

**Other channels**

```sh
brew install {{GITHUB_ORG}}/tap/velra     # Homebrew
cargo binstall velra                      # prebuilt binary
cargo install velra                       # from source
npm i -g velra                            # convenience wrapper; the runtime never needs Node
```

Add `--enable` to the installer (`… | sh -s -- --enable`) to register the hooks
in the same step. Nothing needs sudo, and no daemon is installed.

That is the whole setup. Keep coding; the next `/compact` is handled.

## Commands

| Command | What it does |
|---|---|
| `velra enable [--dry-run]` | Registers the hooks in your user-level Claude Code settings (backup + atomic write). `--dry-run` prints a diff and writes nothing. |
| `velra disable [--purge] [--yes]` | Removes exactly Velra's handlers, restoring the file byte for byte. `--purge` also deletes `~/.velra` (backups kept). |
| `velra status` | Enabled? Claude Code version, database size, sessions tracked, live continuations. Exit 0 when healthy. |
| `velra inspect` | Renders the capsule for the current project's most recent session, without creating a checkpoint. |
| `velra inspect --section dead-ends` | Full, untruncated detail (`dead-ends`, `failure`, `files`, `attempts`). |
| `velra inspect --checkpoint <id>` | Prints a frozen capsule exactly as delivered. |
| `velra doctor` | One line per check: settings parse, hook registrations, binary path, WAL mode, schema version, spool backlog, recent errors. |

Run `velra inspect` right now to see what would survive a compaction.

## The 30-second demo

The shot list for the demo GIF. Terminal at 100x30, one take, no cuts.

| t | Shot | What the viewer reads |
|---|---|---|
| 0:00 | `curl -LsSf https://{{VELRA_DOMAIN}}/install.sh \| sh && velra enable` | Two commands, no prompts, no sudo. |
| 0:05 | `claude` in a repo with a failing test; type *"fix the flaky logout test"* | A real task, not a toy. |
| 0:09 | Claude edits `session.py`, runs `pytest` -> FAIL, reverts with `git restore`, edits `cookies.py` | The dead end happens on camera. |
| 0:17 | `/compact` -> the transcript collapses, and `⚡ Velra checkpoint saved` appears | The moment the context would normally be lost. |
| 0:21 | Type *"what should we try next?"* | The question that exposes amnesia. |
| 0:24 | Claude answers with the objective and the failing assertion, and does **not** re-propose `max_age=0` | The whole product, in one answer. |
| 0:28 | `velra inspect` scrolls the capsule that did it | Nothing magic: observed tool activity. |

Record with `asciinema rec --cols 100 --rows 30`, render with `agg --theme
monokai`. Keep it under 2 MB so it plays inline on GitHub.

## What it costs you

Velra runs inside Claude Code's hook path, so its budget is measured in
milliseconds of wall time per tool call:

| Hook | p50 | p99 |
|---|---|---|
| `post-tool-use` (2 KiB payload) | ≤ 2 ms | ≤ 5 ms |
| `post-tool-use` (edit of a 50 KiB file, includes hashing) | ≤ 3 ms | ≤ 6 ms |
| `user-prompt-submit` | ≤ 2 ms | ≤ 4 ms |
| `pre-compact` (the checkpoint barrier) | ≤ 6 ms | ≤ 10 ms |

These are enforced in CI with `hyperfine` on Linux against a database holding
100,000 events; the build fails on regression. An internal watchdog abandons
work at 250 ms, so a pathological case degrades to "Velra recorded nothing this
time" rather than "Claude Code is waiting".

**On Windows the number that means anything is the marginal one.** Starting a
process there is expensive enough to swamp everything Velra does: the same
binary with `VELRA_DISABLE=1`, which exits before it opens the database,
measures a p99 of 7.2 ms against a small database and 25.0 ms against a 38 MiB
one. Against that control Velra's own cost is **+3.4 to +11.0 ms at p50**, and
it barely moves between those two database sizes. The budget on Windows is
therefore stated as marginal cost over an empty run, not as total wall time;
`bench/harness/hook_overhead.py` measures both columns, and
[DECISIONS.md](DECISIONS.md) D54 has the numbers.

Velra also **cannot break your session**: hooks always exit 0, never write to
stderr, and print either nothing or a single JSON object. Across the 4,338 hook
invocations observed during the v0.1 benchmark, zero exited non-zero and zero
wrote a byte to stderr.

## Privacy

Local-first, and that is enforced rather than promised:

- **No network at runtime.** CI fails if an HTTP client, TLS stack or DNS
  resolver appears anywhere in the dependency graph.
- **No telemetry, no accounts, no LLM calls.**
- **Nothing is written inside your repository.** All state lives in `~/.velra`
  (`0700`), and the only file Velra edits is your user-level Claude Code
  settings.
- **Secrets are redacted before anything is written**, including the crash
  spool. Sensitive paths (`.env`, `*.pem`, `id_rsa*`, `.ssh/**`, …) are stored
  as path and hash only — never an excerpt.

See [SECURITY.md](SECURITY.md) for the full list of what is captured.

Kill switches, in order of scope:

```sh
VELRA_DISABLE=1 claude     # this session only; no database access at all
touch ~/.velra/disabled    # every session, until you remove the file
velra disable              # unregister the hooks
```

## Configuration

Velra needs no configuration. If you want a smaller or larger capsule, create
`~/.velra/config.toml`:

```toml
# Target capsule size in estimated tokens (default 800, hard ceiling 1000).
budget_tokens = 600
```

Environment variables:

| Variable | Effect |
|---|---|
| `VELRA_HOME` | State directory (default `~/.velra`). |
| `VELRA_DISABLE=1` | Kill switch: every hook exits immediately, touching nothing. |
| `VELRA_LOG=debug` | Per-invocation timing to `~/.velra/logs/debug.log`, broken down by phase (`stdin`, `parse`, `context`, `db-open`, `db-append`, `handler`). |
| `VELRA_CLAUDE_VERSION` | Assume this Claude Code version when registering hooks, instead of detecting it. Useful when `claude` is not on your PATH. |
| `CLAUDE_CONFIG_DIR` | Respected when locating `settings.json`. |
| `CLAUDE_PROJECT_DIR` | Respected when resolving the project root. |

## Why Rust

The hook path runs on **every tool call**, so the cost of starting the process
is the design constraint:

- **Zero runtime prerequisites.** One statically linked binary (musl on Linux,
  static CRT on Windows) with SQLite compiled in. Nothing to install, no
  version of anything to match.
- **Instant cold start.** No VM, no GC, no JIT, no interpreter — process start
  is dominated by the OS loader (~1 ms). The hook path initializes no async
  runtime, no CLI parser, no logger and no global regex.
- **Concurrent embedded storage.** SQLite in WAL mode gives many readers plus
  one writer; write transactions are tiny, and a spool file is the no-loss
  fallback when the database is locked.

Alternatives were disqualified on the first two points: Node/Bun/Deno (runtime,
or single-file binaries with 10–30 ms start), Python (runtime plus environment),
JVM (runtime plus start cost), Go (tree-sitter and fast SQLite need cgo, which
complicates static cross-compilation), Zig (ecosystem gaps for JSONC CST
editing and MCP, both needed after v0.1).

## How `velra enable` treats your settings

It edits `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR/settings.json`) and
nothing else — never a project's `.claude/settings*.json`:

1. Parses it as JSONC, so comments and trailing commas are fine.
2. Writes a timestamped backup to `~/.velra/backups/` (10 kept).
3. Adds only its own handlers, as minimal text splices — your formatting,
   comments and key order are untouched.
4. Re-parses the result and refuses to write if anything except Velra handlers
   would change.
5. Writes atomically (temp file + fsync + rename), retrying if another process
   modified the file meanwhile.

`velra enable` twice is a no-op. `velra enable` then `velra disable` gives you
back a byte-identical file. `--dry-run` shows the diff without writing.

## Development

```sh
cargo test --workspace --all-features   # unit + acceptance tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash bench/run.sh                        # §4 performance budgets (hyperfine optional)
python scripts/smoke.py                  # full product pass against the release binary
```

The workspace is two crates: `velra-core` (event log, reducer, state machine,
renderer — no Claude Code specifics) and `velra` (hooks, settings editing, CLI).
[DECISIONS.md](DECISIONS.md) records every ambiguity in the specification and
how it was resolved.

## License

MIT. See [LICENSE](LICENSE).
