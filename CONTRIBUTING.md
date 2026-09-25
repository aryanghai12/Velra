# Contributing to Velra

Thanks for helping. Velra sits inside other people's Claude Code sessions, so
the bar is less about features and more about never getting in the way and
never overstating what it does.

## Ground rules

1. **Never block or disturb Claude Code.** Hook code always exits 0, never
   writes to stderr, and writes to stdout either nothing or exactly one JSON
   object. A change that can violate this is a top-severity bug, however
   useful it is.
2. **Never lose recorded data.** An event reaches the database or the spool,
   never neither.
3. **Nothing leaves the machine.** No network-capable crate in the hook path
   (`ci/check-no-network-deps.sh` enforces this), no telemetry, no LLM calls.
4. **Record decisions.** Every resolved ambiguity or behavioural change gets a
   numbered row in [`DECISIONS.md`](DECISIONS.md) (currently D1–D69) with its
   rationale.
5. **Claims follow evidence.** Documentation and benchmark text say what was
   measured and nothing more. See [the benchmark rules](#benchmark-changes).

## Setting up

```bash
git clone https://github.com/aryanghai12/Velra.git
cd Velra
cargo build --release -p velra
```

You need Rust ≥ 1.98 and a C compiler for the bundled SQLite
([Install → Build from source](docs/INSTALL.md#build-from-source)). The
repository pins no toolchain. On a Windows GNU host, make sure MinGW-w64's
`bin` directory (with `gcc.exe` and `dlltool.exe`) is on `PATH` **in the shell
that runs cargo**. Existing `target/` artifacts can mask a missing compiler.

Python 3.10+ with `pytest` runs the benchmark harness tests.

## The checks CI runs

Run these before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python -m pytest bench/tests -q
python bench/tokenburn/run.py --selftest     # scoring pipeline on 23 scripted cases; offline, spends nothing
python bench/tokenburn/run.py --preflight    # memory-isolation and handoff validation; offline
python scripts/smoke.py                      # the product end to end against the release binary, offline
```

CI also runs an MSRV job (`cargo check` on 1.98.1), an IPC fuzz job
(`cargo test --test ipc_contract -- --ignored`), the no-network dependency
check, and the hook latency budgets (`bench/legacy/run.sh`, reported, not
gating).

Two Rust tests are `#[cfg(unix)]`, so Windows runs slightly fewer tests than
Linux.

## Where things are

| Path | What |
|---|---|
| `crates/velra/` | the binary: hook runtime (`hook.rs`), CLI (`cli.rs`), restore picker, settings editing |
| `crates/velra-core/` | the logic, with no Claude Code I/O: event log, reducer, snapshot, renderer, staging, continuation |
| `crates/velra/tests/` | integration tests against the real binary: fail-open, restore, staged delivery, settings round-trips, security |
| `tests/fixtures/`, `tests/golden/` | recorded Claude Code hook payloads and golden capsules |
| `bench/tokenburn/` | the v0.1.2 benchmark harness and scoring pipeline |
| `bench/results/` | frozen evidence. **Never edit by hand** ([README](bench/results/README.md)) |
| `bench/legacy/`, `bench/harness/` | archived v0.1–v0.1.2-hardened benchmarks, still runnable |
| `docs/` | user and contributor documentation |
| `prompts doc/` | the original build specifications that `DECISIONS.md` sections refer to |

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) explains how the pieces fit.
[`HANDOFF.md`](HANDOFF.md) holds historical v0.1 working notes, including
toolchain gotchas.

## Making a change

- Keep the hook path cheap. `main.rs` dispatches hooks before clap or any
  initialisation; do not add work there that the CLI could do instead.
- Capsule changes: the renderer is a pure function with a hard ceiling. Add a
  property or golden test, and check that the truncation-ladder tests still
  describe the behaviour. Use `velra inspect --trace` to see the effect on
  real sessions.
- Settings changes: `velra disable` must restore the settings file byte for
  byte. The round-trip tests in `crates/velra/tests/settings_edit.rs` guard
  this.
- Workspace identity lives in exactly one function
  (`velra_core::workspace::resolve`). Do not re-derive it elsewhere.
- Update the relevant page in `docs/` and add a `CHANGELOG.md` entry under the
  unreleased version.

## Benchmark changes

The benchmark is only worth anything if nobody can quietly tune it:

- `bench/tokenburn/preregistration_tokenburn.json` is hashed into every
  artifact. Changing it after a run invalidates that run instead of
  reinterpreting it. Amendments bump its version and say why.
- Frozen result trees are protected: the runner refuses to write into them
  (`bench/tokenburn/runroot.py`). Put a new run under a new `--results-root`.
- A pipeline change that would alter the committed v0.1.2 verdicts fails
  `bench/tests/test_tokenburn_requal_evidence.py`. If the change is a genuine
  fix, re-score into a copy, record the correction in `DECISIONS.md`, and
  keep the original evidence.
- Live runs cost money and refuse to start without `VELRA_ALLOW_LIVE_BENCHMARK=1`
  and `--live`, and from inside a Claude Code session. See
  [`bench/README.md`](bench/README.md).

## Reporting bugs and security issues

Open an issue with `velra --version`, `velra doctor --json`, your OS and
Claude Code version, and, for capsule content problems,
`velra inspect --session <id> --trace "<fact>" --json`. Review what you paste
for private content first.

Security vulnerabilities: use a private advisory, as described in
[SECURITY.md](SECURITY.md).

## License

By contributing you agree that your contributions are licensed under the
[MIT License](LICENSE).
