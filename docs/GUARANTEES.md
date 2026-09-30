# Guarantees and limits

This page says what Velra 0.1.2 promises, how strongly, and on what
evidence. It sorts every claim into one of four tiers:

| Tier | Meaning |
|---|---|
| **Guaranteed** | Enforced by the implementation and checked by automated tests that pass on the release tree. |
| **Supported** | Implemented and tested, but it depends on something Velra does not control, usually Claude Code's hook behaviour. |
| **Known limitation** | A boundary of the design, stated so nobody has to find it by accident. |
| **Unverified** | Not established by any evidence in this repository. Treat it as unknown. |

Test names below are real and can be run by name, for example
`cargo test -p velra --test staging_claims`. Numbered decisions (`D…`) are
rows in [DECISIONS.md](../DECISIONS.md).

- [Guaranteed](#guaranteed)
- [Delivery semantics, precisely](#delivery-semantics-precisely)
- [Supported](#supported)
- [Known limitations](#known-limitations)
- [Unverified](#unverified)
- [Platform support](#platform-support)

---

## Guaranteed

| Property | What holds | Checked by |
|---|---|---|
| **Local-first** | No network-capable crate is linked into the hook path. No model is called. No telemetry. | `ci/check-no-network-deps.sh`; `security::j3_no_network_capable_crate_is_linked` |
| **Nothing written inside your repository** | State lives under `$VELRA_HOME`. The one file outside it that Velra edits is your user-level Claude Code settings file. | staging location is fixed to `$VELRA_HOME/staged/` (`crates/velra-core/src/staging.rs`); `settings_edit.rs` |
| **Hooks fail open** | Every `velra hook …` and `velra reduce` exits 0, writes nothing to stderr, and writes at most one JSON object to stdout, including on panic, timeout, a locked, corrupt or read-only database, and malformed input. | `fail_open.rs`; `ipc_contract::b1_*`, `b2_fuzz_*`; `staged_delivery::stdout_contains_only_structured_json` |
| **Redaction before persistence** | Strings matching Velra's secret detectors are replaced with `[REDACTED:<kind>]` before anything is written, the spool included. Sensitive paths are stored as path and hash only. | `security::j1_secrets_never_reach_the_database_or_the_capsule`, `a_planted_secret_reaches_no_persisted_or_printed_surface`, `j2_sensitive_paths_store_only_the_path_and_hash` |
| **Bounded capsule** | A rendered capsule never exceeds 1,000 estimated tokens or 9,500 characters, whatever `budget_tokens` is set to. A staged file larger than that is refused at delivery. | `capsule::e2_budget_always_holds` (property test); `render_fidelity::rendered_sizes_are_within_budget`; `staged_delivery::oversized_capsule_rejected` |
| **Deterministic rendering** | The same ledger rendered at the same clock gives byte-identical text. Replaying the recorded events rebuilds the same capsule. | `render_fidelity::identical_input_renders_identically`; `restore::deterministic_capsule_generation`; `lifecycle::replaying_the_recorded_events_rebuilds_the_same_capsule` |
| **Only an explicit restore crosses sessions** | The in-session continuation is delivered only to the session that produced it. The only way one session's state reaches another is `velra restore`, for a session you name. | `continuation_lifecycle::a_continuation_is_never_delivered_to_another_session`; `lifecycle::restores_never_cross_sessions_or_workspaces` |
| **Workspace isolation** | A staged capsule is delivered only in the workspace it was staged for. The hook and the CLI resolve workspaces with one function. | `workspace_identity.rs`; `staged_delivery::wrong_workspace_rejected`, `hook_and_cli_agree_on_workspace_identity` |
| **Source identity** | The staged record names its source session. The capsule says it is another session's record and names that session's first 8 characters (D147). The destination keeps its own session id. | `restore::destination_session_does_not_inherit_source_session_identity`; `staged_delivery::the_destination_session_does_not_inherit_the_source_identity` |
| **Startup-only restore delivery** | A restore capsule is consumed only by `SessionStart` with source `startup`. `clear`, `resume`, `compact`, `fork` and unknown sources leave it staged. | `staged_delivery::clear_rejects_startup_only_capsule` and siblings; `an_unknown_session_start_source_is_refused` |
| **At-most-once restore delivery** | One staged record is delivered at most once, including under concurrent session starts and a restage during delivery. See [below](#delivery-semantics-precisely) for what happens when a process dies. | `staging_claims.rs`; `staged_delivery::concurrent_session_start_single_winner`, `duplicate_session_start_delivers_once` |
| **Integrity at delivery** | A record that fails its content hash, names another workspace, is malformed, is an unknown format version, or is older than 7 days is not delivered, and an older record is never delivered in its place. | `staged_delivery::a_capsule_failing_its_content_hash_is_rejected`, `stale_capsule_rejected`, `malformed_capsule_rejected`; `staging_claims::every_invalid_record_has_one_stated_outcome_and_no_fallback` |
| **Settings restored byte for byte** | `velra disable` returns the settings file to its pre-`enable` bytes when Velra's hooks were the only ones added. Comments, key order and a UTF-8 BOM are kept. | `settings_edit.rs`; `platform::a_settings_file_with_a_byte_order_mark_keeps_it_through_enable_and_disable` |
| **No event loss under tested contention** | Concurrent hook processes, a held write lock, an interrupted reduce and a corrupt database lose no recorded event and store none twice. | `storage::c1_*`–`c6_*`; `storage_faults.rs` |

"Estimated tokens" are Velra's own estimate (`crates/velra-core/src/text.rs`),
not a tokenizer count. `capsule::estimator_is_above_real_tokenizer_counts`
checks that the estimate stays above real tokenizer counts on recorded
fixtures, so the ceiling is conservative for those fixtures.

## Delivery semantics, precisely

Velra does not claim exactly-once delivery anywhere. What it does claim:

**Restore capsule (`velra restore` → next new session).**

- The claim is the exclusive creation of `capsule.<gen>.claimed`. Nothing
  ever takes a claim over, so two processes never both hold one.
- The capsule is written to stdout while the claim is held. The record is
  deleted only after that write succeeded.
- If the write fails, the claim is released and the capsule stays staged
  for the next session start.
- If the process dies **after** writing and **before** deleting (for
  example, killed by its watchdog), Velra cannot tell whether Claude Code
  received the capsule. It is **not** delivered again. After a minute,
  `velra status` reports it as `claimed_interrupted`; run `velra restore`
  to stage it again.

So across a process kill the guarantee is **at most once**, and the outcome
can be zero deliveries (`staging_claims::the_hook_killed_by_its_watchdog_after_emitting_does_not_deliver_twice`).

**In-session continuation (after `/compact`).**

- A continuation moves `PENDING → ATTACHED → CONFIRMED`. It is written on
  the first of `SessionStart(compact|resume)`, `UserPromptSubmit` or
  `PostToolUse` after the compaction, and no other channel writes it after
  that.
- The prompt channel may write it again, up to three writes in all
  (`continuation::MAX_ATTACH`), only when the previous turn ended before any
  tool call or turn end, which is how an aborted turn looks from a hook.
- `ATTACHED` means the capsule was written to Claude Code. `CONFIRMED` means
  the session produced another tool call or turn end afterwards. Neither
  proves that a model read it: Claude Code returns nothing to a hook (D113).
- A continuation older than 7 days is not written.

## Supported

These work and are tested against recorded Claude Code payloads, but depend
on Claude Code:

- **Hook delivery.** Velra only runs when Claude Code runs its hooks. A
  managed policy, `"disableAllHooks": true`, `VELRA_DISABLE=1` or
  `~/.velra/disabled` stop it. `velra doctor` reports the first two.
- **Payload shapes.** Hook input and transcripts are not a stable public
  interface. Velra parses them tolerantly. Its fixtures are recorded from
  Claude Code 2.1.272 (`tests/fixtures/claude-code/2.1.272/`), and the live
  benchmark ran on 2.1.280.
- **Version gates.** Which hooks are registered depends on the detected
  Claude Code version ([Configuration](CONFIGURATION.md#hook-registration)).
  After a Claude Code upgrade, run `velra enable` again.
- **Receipt.** Claude Code places `additionalContext` into the session. The
  live benchmark observed the delivered bytes in the destination sessions;
  Velra itself cannot observe that from a hook.

## Known limitations

**What the capsule is.** A bounded rendering of recorded operational state:
prompts, tool events, test runs, edits and reverts. It is not the
conversation. The assistant's prose and reasoning are never captured.
A reverted edit is recorded as *what* changed, with a short excerpt, not
*why*. Velra only knows sessions it watched.

**The budget trade-off (D64, D71, D72).** The default target is 740
estimated tokens. When a capsule does not fit, a fixed, named ladder removes
detail: regenerable output and list tails first, then prose, with exact
identifiers (test ids, file paths, the next function named) kept longest.
The consequence is deliberate: under pressure your own wording is shortened
or dropped before an identifier is. A larger `budget_tokens` keeps more
(up to the 1,000-token ceiling) at the cost of context in every session
that receives it. Margins can be small: the retention fixture renders 4
tokens under its budget, so the restored capsule's framing was held to no
more than a continuation's (D147). `velra inspect --trace <fact>` names the
rung that removed a line.

**Snapshots, not live state.** A restore renders the source session at the
moment you run the command. Work done afterwards is not included; restage
to refresh. A staged capsule expires after 7 days.

**`/clear`.** A restore renders the session's current epoch, and `/clear`
starts a new, empty one. Stage **before** you clear.

**Duplicate events (D148).** A tool event is deduplicated by its
`tool_use_id`, so the same tool call delivered to two registered copies of
the hook is stored once. Three cases are not covered:

- events without a `tool_use_id` (prompts, session starts, stops) keep the
  clock in their key, so two copies of the same prompt observed at different
  milliseconds are stored twice;
- `prompt_id` duplicates are not eliminated globally: nothing establishes
  that Claude Code never repeats one, so it is not treated as unique;
- if an older Velra binary and this one are both registered, the older one
  still writes its old key, and a tool call can be stored twice.
  `velra enable` reduces Velra's registrations in your user-level settings
  file to one, whatever binary path they name. It never touches
  project-level `.claude/settings*.json`, so a copy registered there stays
  registered (D110).

**Redaction is pattern matching.** It covers the formats listed in
[SECURITY.md](../SECURITY.md#redaction). An unusual secret format can pass
through. Anything you type into a prompt can reach the next session in that
workspace via a restore.

**Settings edits race other writers.** `enable`/`disable` check for a
concurrent change immediately before the atomic rename and retry, but the
window from that check to the rename remains. Under a stress test saving the
file every 12 ms, 5 of 956 saves landed in it (D143). No file system offers
a compare-and-swap to close it.

**Watchdog edges (D145).** A synchronous hook gives up after 250 ms. Its
event is spooled if the deadline falls after the event was built. A deadline
while stdin is still being read or parsed loses that event and logs it to
`logs/errors.log`.

**Spool backlog.** Before deciding whether to re-write a continuation, the
prompt hook ingests at most 32 spool files (D120). A larger backlog is
ingested by the next reducer pass or CLI command.

## Unverified

Nothing in this repository establishes the following. Treat them as
unknown:

- **macOS on the release tree.** The last CI run on macOS (`108c70d`) failed.
  The fix for the known cause (D146) passes locally and under simulated CPU
  starvation on Linux, but has not run on macOS.
- **The final build with a live model.** The live benchmark ran on build
  `51b96cb`, before the retention fix (`c7deb12`) and twelve hardening
  phases. The final build has been checked offline against the same source
  ledgers ([Benchmark](BENCHMARK.md#2-final-build-replay-of-the-frozen-source-ledgers)),
  not in a live session.
- **Claude Code versions** other than the recorded 2.1.272 fixtures and the
  2.1.280 live run.
- **ARM64 on Linux and Windows, and x64 on macOS.** Release builds are
  produced for them; no test runs on them.
- **Power loss, network file systems, and long-running databases** far
  larger than the 100,000-event test seed.
- **Other coding agents.** Velra integrates with Claude Code only.

## Platform support

| Platform | Release target | Automated tests | Status of this release tree |
|---|---|---|---|
| Windows x64 | `x86_64-pc-windows-msvc` | CI `windows-latest` | see [Testing → Platform coverage](TESTING.md#platform-coverage) |
| Linux x64 | `x86_64-unknown-linux-musl` | CI `ubuntu-latest` (glibc) | see [Testing → Platform coverage](TESTING.md#platform-coverage) |
| macOS arm64 | `aarch64-apple-darwin` | CI `macos-latest` | last CI run failed (`108c70d`); fix unverified on macOS |
| macOS x64 | `x86_64-apple-darwin` | none | built, untested |
| Linux arm64 | `aarch64-unknown-linux-musl` | none | built, untested |
| Windows arm64 | `aarch64-pc-windows-msvc` | none | built, untested |

---

[← README](../README.md) · [Architecture](ARCHITECTURE.md) · [Restore](RESTORE.md) · [Security](../SECURITY.md) · [Testing](TESTING.md)
