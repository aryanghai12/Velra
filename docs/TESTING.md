# Testing

What Velra's tests cover, how to run them, what ran on this release tree,
and what is known to be flaky. For building and tooling see
[DEVELOPMENT.md](DEVELOPMENT.md).

- [The validation matrix](#the-validation-matrix)
- [Rust suites](#rust-suites)
- [Python suites and offline checks](#python-suites-and-offline-checks)
- [Ignored tests and measurements](#ignored-tests-and-measurements)
- [Platform-specific tests](#platform-specific-tests)
- [Platform coverage](#platform-coverage)
- [Known intermittent failures](#known-intermittent-failures)
- [CI](#ci)

---

## The validation matrix

Run all of it before a release, on the release tree. Everything is offline.

```bash
cargo test --workspace --features fault-injection --no-fail-fast
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
python -m pytest bench/tests -q
python bench/harness/selftest.py
python bench/tokenburn/run.py --selftest
python bench/tokenburn/run.py --preflight
cargo build --release -p velra && python scripts/smoke.py
python bench/tokenburn/run.py --dry-run --results-root <new dir>   # needs a clean tree and a release build of HEAD
```

Run both feature sets. `--features fault-injection` (the only feature, so
the same as `--all-features`) compiles test seams into the binary and
enables tests that need them; a default build has neither, and CI runs only
the first. Always pass `--no-fail-fast`: `cargo test` otherwise stops at the
first failing test binary and hides the rest. `cargo test -q` prints dots,
so read the `test result:` lines or the exit code, not `... FAILED` lines.

## Rust suites

Unit tests live beside the code (208 in `velra-core`, 28 in the `velra`
binary). Integration tests in `crates/velra/tests/` drive the real binary
through `crates/velra/tests/common/`.

| Suite | Covers | Tests (fault-injection / default) |
|---|---|---:|
| `adversarial.rs` | property and fault tests: kills at every write point, corrupt databases, racing processes | 22 / 16 |
| `capsule.rs` | capsule contents per state, the budget property, the ladder order, the token estimator | 23 / 23 |
| `cli_contract.rs` | `--json` documents, exit codes, closed stdout | 6 / 6 |
| `continuation_lifecycle.rs` | the `/compact` path end to end: checkpoint, delivery once per point, confirmation, expiry | 32 / 30 |
| `fail_open.rs` | exit 0 and silence under an unusable home, panics, the watchdog, the kill switch | 8 / 6 |
| `ipc_contract.rs` | every recorded hook fixture, fuzzed and malformed stdin, output shape | 12 / 11 |
| `ledger.rs` | constraints and the working-set ranking | 21 / 21 |
| `lifecycle.rs` | the whole restore lifecycle through the real binary; replay rebuilds the same capsule; duplicate hook registration | 5 / 5 |
| `ordering.rs` | logical event order, spooled events, rebuilds | 19 / 19 |
| `platform.rs` | Windows and POSIX specifics: `PATH` search, path spellings, byte order marks, held-open files | 8 / 8 |
| `prompt_classification.rs`, `prompt_semantics.rs` | what counts as the user's words and what a prompt means | 22 / 22, 19 / 19 |
| `render_fidelity.rs` | what the rendered capsule claims against the snapshot | 15 / 15 |
| `restore.rs` | `velra restore`: selection, isolation, determinism, redaction, atomic staging | 24 / 24 |
| `retention.rs`, `retention_priority.rs` | exact identifiers and priority under the budget | 14 / 14, 20 / 20 |
| `revert_provenance.rs` | revert, dead-end and file-version provenance | 25 / 25 |
| `security.rs` | redaction everywhere, sensitive paths, the dependency audit, file modes | 7 / 7 |
| `settings_edit.rs` | `enable`/`disable` against real settings files, byte-for-byte round trips | 17 / 17 |
| `snapshot_selection.rs` | snapshot selection under pressure | 19 / 19 |
| `staged_delivery.rs` | `SessionStart` delivery: sources, integrity, workspace, one-shot, fail-open | 42 / 42 |
| `staging_claims.rs` | claim safety on the real file system: races, restages, killed holders | 10 / 8 |
| `state_machine.rs` | the continuation state machine | 11 / 11 |
| `storage.rs`, `storage_faults.rs` | concurrency, spool, corruption, migrations, interrupted writes | 7 / 7, 19 / 17 |
| `tracking.rs` | reverts, discards, commits, reapplication, turn-end scans | 18 / 18 |
| `workspace_identity.rs` | the hook and the CLI agree on the workspace | 8 / 8 |
| `storage_bounds.rs` | measurements only (ignored, below) | 0 / 0 |

## Python suites and offline checks

| Command | Checks |
|---|---|
| `python -m pytest bench/tests -q` | the benchmark harness, isolation, safety rules (standard library only), result-root protection, and a re-score of the frozen v0.1.2 evidence that fails if any verdict would change |
| `python bench/harness/selftest.py` | the archived harness's pipeline on scripted outcomes |
| `python bench/tokenburn/run.py --selftest` | the Token-Burn scoring pipeline on 23 scripted cases |
| `python bench/tokenburn/run.py --preflight` | memory-isolation and source-handoff validation, without a Claude process |
| `python bench/tokenburn/run.py --dry-run --results-root <dir>` | the benchmark's readiness gate: fixtures, ground truth, leak scans, the ladder, telemetry rules, the restore/delivery smoke (`bench/tokenburn/smoke.py`), the preflight and provenance (a clean tree, a release binary built at `HEAD`). Ends `READY FOR LIVE EVALUATION` or names each blocker |
| `python scripts/smoke.py` | enable, a task with a failing test and a reverted edit, `PreCompact`, delivery at `SessionStart(compact)`, `status`, `doctor`, `disable`, and a byte-identical settings file, against the release binary in a temporary directory |

## Ignored tests and measurements

Eight tests are `#[ignore]`d. They are measurements or long fuzzing, not
checks, and are run by hand:

| Test | Run with |
|---|---|
| `ipc_contract::b2_extended_fuzz` | `cargo test -p velra --test ipc_contract -- --ignored` (CI: the IPC fuzz job) |
| six `storage_bounds::measure_*` | `cargo test --release -p velra --test storage_bounds -- --ignored --nocapture` |
| `db::tests::measure_busy_waits_against_the_clock` | `cargo test -p velra-core measure_busy_waits -- --ignored --nocapture` |

## Platform-specific tests

- **POSIX only** (`#[cfg(unix)]`): `settings_edit::a2_symlinked_settings_file_is_edited_through_the_link`,
  `fail_open::g1_read_only_home_never_blocks_a_hook`. The file-mode
  assertions in `security::state_files_are_private_on_posix` are compiled
  only on POSIX; on Windows that test runs without them.
- **Windows only** (`#[cfg(windows)]`): `platform::a_read_only_settings_file_is_left_as_it_was_and_named`,
  `platform::a_settings_file_held_open_by_another_program`,
  `adversarial::a_hanging_shim_neither_holds_velras_output_nor_outlives_it`,
  and Windows cases inside other `platform.rs` tests (8.3 short names, the
  `claude.cmd` search).

So the same suite runs a slightly different set of tests per platform.

## Platform coverage

Results for this release tree:

| Platform | How | Result |
|---|---|---|
| Windows 11 x64, `x86_64-pc-windows-gnu` (local) | full matrix above | fault-injection 689 passed, 0 failed, 8 ignored; default 674 passed, 0 failed, 8 ignored; clippy and fmt clean; pytest 295 passed; both selftests, preflight and smoke passed |
| Linux x64, Ubuntu 24.04 under WSL 2 (kernel 6.6), Rust 1.98.1, gcc 13.3 (local) | both cargo suites, on a copy of the working tree | all features 689 passed, 0 failed, 8 ignored; default 674 passed, 0 failed, 8 ignored. The Python suites were **not** run on Linux (no `pytest` in that environment) |
| macOS | not run on this tree | **unverified**. The last CI run on macOS (`108c70d`, run 36422183210) failed; the Phase 12 fix (D146) has not run on macOS |
| Windows MSVC, Linux glibc (CI) | not run on this tree | last CI run (`108c70d`): passed. Historical for this tree |

Timing- and race-sensitive suites were also repeated three times each on
Windows with fault injection (`staging_claims`, `staged_delivery`,
`lifecycle`, `storage_faults`, `continuation_lifecycle`, `adversarial`,
`storage`): 21 runs, 0 failures.

## Known intermittent failures

Seen before, not reproduced reliably, and not seen in the runs above.
Record the full log if one appears:

| Test | Platform | What was seen |
|---|---|---|
| `adversarial::hook_processes_meeting_a_corrupt_database_lose_nothing` | Windows, full runs under load | `BrokenPipe` writing a child's stdin |
| `adversarial::a_hanging_shim_neither_holds_velras_output_nor_outlives_it` | Windows, under load | a grandchild outlived the status command |
| `storage_faults::spooled_confirmation_evidence_prevents_a_re_emission` | Linux, one full default-feature run | failed once; 0 of 60 in isolation |

## CI

`.github/workflows/ci.yml` runs on pushes to `main`, on pull requests, and
on manual dispatch (`workflow_dispatch`). A push to another branch does not
trigger it by itself.

| Job | Runs |
|---|---|
| `test (ubuntu-latest)`, `test (windows-latest)`, `test (macos-latest)` | fmt, clippy (`-D warnings`), `cargo test --workspace --all-features`, and an 8 MB binary-size budget |
| MSRV (1.98) | `cargo check` on the declared `rust-version` |
| IPC fuzz | the ignored extended fuzz, 2 minutes per subcommand; only on manual dispatch or a schedule (none is configured) |
| no network-capable dependency | `ci/check-no-network-deps.sh` |
| performance budgets (H1) | `bench/legacy/run.sh` with `hyperfine`; **`continue-on-error`**, so a green job does not mean the budgets were met |

CI does not run the default-feature suite, the Python suites, or the smoke
test; run those locally.

---

[← README](../README.md) · [Development](DEVELOPMENT.md) · [Guarantees](GUARANTEES.md) · [Contributing](../CONTRIBUTING.md)
