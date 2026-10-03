# Development

How to build Velra, where things live, and how the release binary is
produced. For the test suites see [TESTING.md](TESTING.md); for the rules a
change has to follow see [CONTRIBUTING.md](../CONTRIBUTING.md).

- [Toolchain](#toolchain)
- [Repository layout](#repository-layout)
- [Building](#building)
- [Working on the hook path](#working-on-the-hook-path)
- [Working on the capsule](#working-on-the-capsule)
- [Benchmark tooling](#benchmark-tooling)
- [Documentation assets](#documentation-assets)
- [Release build and version metadata](#release-build-and-version-metadata)

---

## Toolchain

| Need | Version | Why |
|---|---|---|
| Rust | 1.98 or newer (`rust-version` in `Cargo.toml`; CI checks it) | the workspace |
| C compiler | any that `cc` finds | `rusqlite` builds SQLite from source (`bundled`) |
| Python | 3.10 or newer, with `pytest` | benchmark harness tests, smoke test, replay |

The repository pins no toolchain file. C toolchains per platform:

| Platform | C toolchain |
|---|---|
| Windows, MSVC Rust host (the default) | Visual Studio Build Tools, "Desktop development with C++" |
| Windows, GNU Rust host (`x86_64-pc-windows-gnu`) | MinGW-w64 GCC; `gcc.exe` and `dlltool.exe` must be on `PATH` **in the shell that runs cargo** |
| macOS | Xcode Command Line Tools |
| Linux | `cc` (for example `build-essential`) |

On a Windows GNU host, existing `target/` artifacts can hide a missing
compiler: a build that only relinks succeeds, and a clean one fails with
`gcc.exe: program not found`.

## Repository layout

| Path | What |
|---|---|
| `crates/velra/` | the binary: hook runtime, CLI, restore picker, settings editing |
| `crates/velra/tests/` | integration tests that drive the real binary ([TESTING.md](TESTING.md)) |
| `crates/velra/examples/` | `seed` (bulk events for measurements), `budget_probe`, `token_calibration` |
| `crates/velra-core/` | the logic, with no Claude Code I/O: ledger, reducer, snapshot, renderer, staging, continuation |
| `tests/fixtures/claude-code/` | hook payloads recorded from Claude Code |
| `tests/fixtures/tokenizer/`, `tests/golden/` | tokenizer calibration data; golden capsules and settings files |
| `bench/tokenburn/` | the v0.1.2 benchmark harness, scoring pipeline and replay |
| `bench/tests/` | Python tests for the harness, and re-scoring of the committed evidence |
| `bench/results/` | benchmark evidence. Frozen trees are never edited ([README](../bench/results/README.md)) |
| `bench/legacy/`, `bench/harness/` | archived v0.1 to v0.1.2-hardened benchmarks, still runnable; `bench/legacy/run.sh` also runs the hook latency budgets |
| `scripts/` | `smoke.py` (end-to-end product pass), `diagnose_event_loss.py`, `doc_assets.py` |
| `ci/` | `check-no-network-deps.sh` |
| `install/`, `npm/velra/` | installers, Homebrew formula template, npm launcher |
| `docs/` | user and contributor documentation ([index](README.md)) |
| `prompts doc/` | the original build specifications that `DECISIONS.md` section numbers (`§…`) refer to; historical |

## Building

```bash
cargo build -p velra                 # debug
cargo build --release -p velra       # release: target/release/velra(.exe)
cargo install --path crates/velra --locked
```

The release profile uses fat LTO and one codegen unit, so a release build
takes several minutes. It keeps `panic = "unwind"`: the hook runtime catches
panics to guarantee exit 0.

`velra enable` registers the absolute path of the binary you run it with.
If you run it from `target/release`, every Claude Code session on the
machine runs your build. Use a scratch `CLAUDE_CONFIG_DIR` and
`VELRA_HOME` when experimenting:

```bash
export VELRA_HOME=/tmp/velra-dev CLAUDE_CONFIG_DIR=/tmp/claude-dev
./target/release/velra enable
```

## Working on the hook path

- `crates/velra/src/main.rs` dispatches `hook` and `reduce` before clap or
  any initialisation. Keep work out of that path that the CLI could do
  instead.
- A hook must exit 0, write nothing to stderr, and write at most one JSON
  object to stdout. `crates/velra/tests/ipc_contract.rs` checks this for
  every recorded fixture and under fuzzed input.
- To drive a hook by hand:

  ```bash
  printf '{"session_id":"dev","hook_event_name":"SessionStart","source":"startup","cwd":"%s"}' "$PWD" \
    | ./target/release/velra hook session-start; echo " exit=$?"
  ```

- Fault seams (`VELRA_TEST_*` stalls, panics, watchdog overrides) exist only
  with `--features fault-injection`.

## Working on the capsule

- `render::render` is a pure function of a snapshot and a budget. A change
  to it needs a property or golden test, and the ladder tests in
  `capsule.rs`, `render_fidelity.rs` and `retention_priority.rs` must still
  describe the behaviour.
- `velra inspect --trace <marker>` shows the effect of a change on a real
  session, layer by layer.
- `cargo run --release -p velra --example budget_probe -- <velra_home> [session]`
  sweeps the render budget against a real database and shows which
  sections survive at each target.
- `cargo run -p velra --example token_calibration -- tests/fixtures/tokenizer`
  compares Velra's token estimate with recorded tokenizer counts; the
  estimate must not read below the real cost on any fixture.

## Benchmark tooling

All of these are offline and spend nothing:

```bash
python bench/tokenburn/run.py --selftest       # the scoring pipeline on 23 scripted cases
python bench/tokenburn/run.py --preflight      # memory isolation and source-handoff checks
python bench/tokenburn/run.py --dry-run --results-root <new dir>   # the full readiness gate
python bench/tokenburn/replay.py --binary target/release/velra.exe --out <new dir>
bash bench/legacy/run.sh                       # hook latency against a 100,000-event database
```

`replay.py` needs the raw captures of the live run, which are not
committed ([Benchmark → Reproducibility](BENCHMARK.md#9-reproducibility)).
Live runs cost money and refuse to start without `VELRA_ALLOW_LIVE_BENCHMARK=1`
and `--live`, and from inside a Claude Code session. See
[bench/README.md](../bench/README.md).

## Documentation assets

The benchmark charts in `docs/assets/v0.1.2/` are generated from committed
evidence by one script:

```bash
python scripts/doc_assets.py
```

It reads only `bench/results/v0.1.2-requal/` and `bench/results/v0.1.2-final/`
and rewrites only the chart files it owns. The two diagrams
(`velra-pipeline.svg`, `restore-session-flow.svg`) are drawn by hand and
describe `docs/ARCHITECTURE.md`; update them with it.

## Release build and version metadata

```bash
cargo build --release -p velra
./target/release/velra --version
# velra 0.1.2 (<9-character commit>, <target triple>)
```

`crates/velra/build.rs` embeds `git rev-parse --short=9 HEAD` and the
target triple, and re-runs whenever `HEAD` or the branch ref moves. It does
not mark a dirty tree: a binary built with uncommitted changes reports the
commit it was built on top of. Build release artifacts from a clean
checkout of the release commit.

Published archives are built by `.github/workflows/release.yml` for six
targets (Linux musl x64/arm64, macOS x64/arm64, Windows MSVC x64/arm64). The
maintainer runbook is [RELEASING.md](RELEASING.md).

---

[← README](../README.md) · [Testing](TESTING.md) · [Architecture](ARCHITECTURE.md) · [Contributing](../CONTRIBUTING.md)
