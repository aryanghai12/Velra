# Contributing to Velra

Thanks for helping. Velra runs inside other people's Claude Code sessions,
so the bar is less about features than about never getting in the way and
never overstating what it does.

## Ground rules

1. **Never block or disturb Claude Code.** Hook code always exits 0, never
   writes to stderr, and writes to stdout either nothing or exactly one
   JSON object. A change that can violate this is a top-severity bug,
   however useful it is.
2. **Never lose recorded data.** An event reaches the database or the
   spool, never neither.
3. **Nothing leaves the machine.** No network-capable crate in the hook
   path (`ci/check-no-network-deps.sh` enforces it), no telemetry, no model
   calls.
4. **Record decisions.** Every resolved ambiguity or behavioural change gets
   a numbered row in [`DECISIONS.md`](DECISIONS.md) (currently D1–D150),
   with what was reproduced or measured to justify it.
5. **Claims follow evidence.** Documentation says what the code does and
   what was measured, and nothing more. [docs/GUARANTEES.md](docs/GUARANTEES.md)
   sorts every claim by strength; keep it true.

## Getting set up

Toolchain, repository layout and build details are in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). In short:

```bash
git clone https://github.com/aryanghai12/Velra.git
cd Velra
cargo build --release -p velra
```

Rust 1.98 or newer, a C compiler for the bundled SQLite, and Python 3.10+
with `pytest`.

## Before you open a pull request

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --no-fail-fast
cargo test --workspace --no-fail-fast            # default features: CI does not run this one
python -m pytest bench/tests -q
python bench/tokenburn/run.py --selftest
python bench/tokenburn/run.py --preflight
python scripts/smoke.py                          # needs the release binary
```

All of these are offline and spend nothing. What each suite covers, the
tests that are platform-specific or ignored, and the known intermittent
failures: [docs/TESTING.md](docs/TESTING.md).

CI also runs an MSRV job, an IPC fuzz job, the no-network dependency check
and the hook latency budgets (reported, not gating).

## Making a change

- **Hook path.** `main.rs` dispatches hooks before clap or any
  initialisation; keep work there minimal. Test through the real binary
  (`crates/velra/tests/common`), not only through the library.
- **Capsule.** The renderer is a pure function with a hard ceiling. Add a
  property or golden test, keep the ladder tests describing the behaviour,
  and check the effect on a real session with `velra inspect --trace`.
- **Settings.** `velra disable` must restore the settings file byte for
  byte; `crates/velra/tests/settings_edit.rs` guards it.
- **Workspace identity** lives in one function,
  `velra_core::workspace::resolve`. Do not re-derive it.
- **Documentation.** Update the page in `docs/` that describes the
  behaviour, and add a `CHANGELOG.md` entry under the unreleased version.
  If a claim in [GUARANTEES.md](docs/GUARANTEES.md) changes strength, move
  it.

## Benchmark changes

The benchmark is only worth anything if nobody can quietly tune it.

- `bench/tokenburn/preregistration_tokenburn.json` is hashed into every
  artifact. Changing it after a run invalidates that run instead of
  reinterpreting it. Amendments bump its version and say why.
- Frozen result trees are protected: the runner refuses to write into them
  (`bench/tokenburn/runroot.py`). Put a new run under a new
  `--results-root`. Never edit a file under `bench/results/` by hand.
- A pipeline change that would alter the committed v0.1.2 verdicts fails
  `bench/tests/test_tokenburn_requal_evidence.py`. If the change is a
  genuine fix, re-score into a copy, record the correction in
  `DECISIONS.md`, and keep the original evidence.
- Live runs cost money, refuse to start without
  `VELRA_ALLOW_LIVE_BENCHMARK=1` and `--live`, and refuse from inside a
  Claude Code session. See [`bench/README.md`](bench/README.md).

## Reporting bugs and security issues

Open an issue with `velra --version`, `velra doctor --json`, your OS and
Claude Code version, and, for a capsule content problem,
`velra inspect --session <id> --trace "<fact>" --json`. Capsules and logs
quote your prompts and file paths: review what you paste.

Security vulnerabilities: use a private advisory, as described in
[SECURITY.md](SECURITY.md#reporting-a-vulnerability).

## License

By contributing you agree that your contributions are licensed under the
[MIT License](LICENSE).
