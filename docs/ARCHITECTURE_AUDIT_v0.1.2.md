# Architecture audit — Velra as it stands before the v0.1.2 ledger work

Written from the code and the recorded artifacts, not from the README. Every
claim below names the file it was read out of. Nothing here is aspirational.

Traced execution path, confirmed end to end:

```
Claude Code hook event (JSON on stdin)
  -> crates/velra/src/hook.rs::dispatch          normalize + redact
  -> crates/velra-core/src/eventlog.rs           append to `events` (or spool)
  -> crates/velra-core/src/reducer.rs            cursor-based projection
  -> intents / edits / commands / dead_ends / file_stats / file_versions
  -> crates/velra-core/src/snapshot.rs           selection rules -> Snapshot
  -> crates/velra-core/src/render.rs             truncation ladder -> capsule
  -> crates/velra-core/src/checkpoint.rs         immutable `checkpoints` row
  -> crates/velra-core/src/continuation.rs       PENDING -> ATTACHED -> CONFIRMED
  -> hook.rs::Ctx::deliver                       hookSpecificOutput.additionalContext
  -> Claude Code transcript (post-compaction SessionStart)
```

## 1. Current data model

SQLite, `PRAGMA user_version = 2`, schema in
[db.rs](../crates/velra-core/src/db.rs). Two layers:

**Provenance layer (append-only).** `events` — one row per hook invocation,
`dedupe_key` unique, normalized+redacted `payload` JSON. `projects`,
`sessions`, `reducer_cursor`.

**Projection layer (derived, rebuildable from `events`).** `intents`,
`file_versions`, `edits`, `dead_ends`, `commands`, `file_stats`.

**Output layer.** `checkpoints` (immutable, enforced by a BEFORE UPDATE
trigger), `continuations` (one live row per session, enforced by a partial
unique index), `injections` (unique `delivery_key`), `compactions`.

Migration v2 added `file_stats.first_touch_ms`. Migrations are forward-only,
index `i` upgrades version `i` to `i+1`.

## 2. Current event sources

From [model.rs](../crates/velra-core/src/model.rs) `hook_event` and the
fixtures in `tests/fixtures/claude-code/2.1.268/`:

| Hook | Payload fields retained |
|---|---|
| `SessionStart` | `source` (startup/compact/resume/clear), `model`, `transcript_path` |
| `UserPromptSubmit` | **`prompt` (verbatim, capped 4 KiB, redacted)**, `prompt_id` |
| `PreToolUse` | `path`, `pre_hash`, `size`; for shell, `git` observations |
| `PostToolUse` | edits: `path`, `pre_hash`, `post_hash`, `lines_added/removed`, `excerpt`; reads: `path`, `offset`, `limit`, `pattern`; shell: `command`, `cwd`, `exit_code`, `stdout_tail`, `stderr_tail`, `git` |
| `PostToolUseFailure` | `tool_name`, `error`, `error_type`, `is_interrupt` |
| `Stop` | (triggers a turn-end rehash of actively-edited files) |
| `PreCompact` | `trigger`, `custom_instructions` |
| `PostCompact` | `trigger`, `summary` (the native compaction summary) |
| `SessionEnd` | `reason` |

**The critical Phase 0 finding.** `UserPromptSubmit` does expose the original
user prompt verbatim. This is not an inference: the fixture
`tests/fixtures/claude-code/2.1.268/user_prompt_submit.json` carries
`"prompt": "fix the flaky logout test and keep the session cookie behaviour
intact"`, and [hook.rs:624](../crates/velra/src/hook.rs) stores it into
`Payload.prompt` under `limits::PROMPT` (4 KiB) after redaction. Conversational
constraint capture is therefore **supported by an existing deterministic event
source** — Phase 0 option A. No new source is needed and no LLM is involved.

What is *not* available: assistant turns, tool-call reasoning, and anything the
user said that Claude Code did not deliver as a `UserPromptSubmit` (e.g. text
typed into a `/compact` custom instruction is signalled only as the boolean
`custom_instructions`).

## 3. Current state reconstruction

[reducer.rs](../crates/velra-core/src/reducer.rs), cursor-based, batches of 200
inside one `BEGIN IMMEDIATE` per batch.

- **Intents.** `intent.rs::classify_prompt` — `/`-prefixed prompts are ignored;
  `task:` starts a new epoch; `subtask:` replaces SUBTASK; the first prompt of
  an epoch at least 20 chars becomes ROOT; anything else at least 3 chars
  becomes LATEST. Whole-prompt only: no sub-prompt structure is extracted.
- **Edits and reverts.** `record_version` writes a `file_versions` row per
  observed hash, then `revert.rs::detect_revert` decides whether the file has
  returned to a hash that precedes live ACTIVE edits. A detected revert resolves
  those edits and inserts a `dead_ends` row with the mechanism
  (`git_command` / `inverse_edit` / `rewrite` / `external`) and, when the
  mechanism is a git command, the command text.
- **Commands.** `commands.rs::classify` buckets a shell command as
  test/build/lint/git/other and `outcome()` decides PASS/FAIL/INTERRUPTED/
  UNKNOWN from exit code, failure-event flag and output. A FAIL stores a
  `failure_excerpt` and resolves `mentioned_paths` against the real filesystem.
- **File stats.** `bump_stats` accumulates reads, edits, an `in_failure` flag,
  and first/last touch timestamps per `(session, epoch, path)`.
- **Ordering.** `versions()` reads history by `ts_ms` then `id` (D56), so a
  spooled event that lands late cannot make an old snapshot look current.

## 4. Current capsule sections

[render.rs](../crates/velra-core/src/render.rs), `render_version = 1`, fixed
order:

```
<VELRA_WORKSPACE_STATE v="1" checkpoint=… captured=… trigger=…>
[ABOUT_THIS_RECORD]      provenance paragraph (CONTEXT const)
[FIRST_MESSAGE]          ROOT intent, or "(not captured)"
[SUBTASK_MESSAGE]        SUBTASK intent          (optional)
[LATEST_MESSAGE]         LATEST intent           (optional, suppressed if == ROOT)
[WORKSPACE_STATE]        git branch@sha | N edits | last test run: PASS/FAIL
[TEST_RESULT]            open failure: command, exit code, excerpt tail
[REVERTED_EDITS]         dead ends: path, edit count, mechanism, time, ± excerpt
[RECENT_EDITS]           latest ACTIVE edit per path, +/- lines, afterward
[FILE_ACTIVITY]          ranked working files
[FAILURE_LOCATION]       inferred next target
[RECORD_DETAIL]          `velra inspect` pointer
</VELRA_WORKSPACE_STATE>
```

Framing is already neutral and provenance-tagged: `(OBSERVED | …)` vs
`(INFERRED | …)`, "Causal link: UNCONFIRMED." on the one place a causal
reading is tempting, and `render_traced` annotates every derived line with the
row ids it came from.

## 5. Current truncation mechanism

Two ladders plus a backstop, all in `render.rs`:

- `SPEC_STEPS` (7 rungs) run against `cfg.budget_tokens`
  (`DEFAULT_BUDGET_TOKENS = 730`).
- `CEILING_STEPS` (15 rungs) run **only if** the result is still above
  `HARD_CEILING_TOKENS = 1000` estimated or `ABSOLUTE_MAX_CHARS = 9500` chars.
- `enforce_ceiling` drops whole body lines from the end, keeping the head
  through `[WORKSPACE_STATE]` and the `[RECORD_DETAIL]` tail.

**Defect D-A (token budget has no hard stop at the target).** `run_steps`
returns as soon as the rungs are exhausted, whether or not the target was met.
So the *only* enforced bound is 1000 estimated tokens, not 730. A capsule may
legitimately be delivered at any size between 730 and 1000 estimated. This is
the structural reason the 800-real-token ceiling is an expectation rather than
a property.

**Defect D-B (the estimator under-reads the current capsule format).**
Measured today against the eight capsules Claude Code actually received in the
v0.1.1 run (`bench/results/v0.1.1/trials/*/delivered_capsule.txt` paired with
`token_measurement.json`):

| capsule | estimate | real | real/est |
|---|---:|---:|---:|
| s1-r1 | 728 | 777 | 1.067 |
| s1-r2 | 748 | 790 | 1.056 |
| s1-r3 | 746 | 791 | 1.060 |
| s1-r4 | 745 | **804** | **1.079** |
| s2-r1 | 728 | 753 | 1.034 |
| s2-r2 | 722 | 753 | 1.043 |
| s2-r3 | 729 | 751 | 1.030 |
| s2-r4 | 723 | 751 | 1.039 |

The docstring on `estimate_tokens` claims the walk "clears the worst observed
under-read thirty times over", citing the two v0.1-format fixtures in
`tests/fixtures/tokenizer/` where it over-reads by 3–6%. That claim is stale:
it was calibrated on the old, path-dense `<VELRA_CONTINUATION>` format. The
current format opens with a ~340-character prose paragraph, and the walk charges
prose at 4 letters per token where Claude's tokenizer charges nearer 3.

`estimator_is_above_real_tokenizer_counts` passes only because it checks the two
old fixtures and not the eight new ones.

## 6. Current delivery state machine

[continuation.rs](../crates/velra-core/src/continuation.rs).
`PENDING -> ATTACHED -> CONFIRMED`, terminal `SUPERSEDED` / `EXPIRED`.

- Exactly-one-live is a **database invariant**: `CREATE UNIQUE INDEX
  one_live_continuation ON continuations(session_id) WHERE state IN
  ('PENDING','ATTACHED')`.
- Exactly-once is enforced by `injections.delivery_key UNIQUE` plus a
  conditional `UPDATE … WHERE state = 'PENDING'` that must affect exactly one
  row. A repeat of a known `delivery_key` re-emits the identical bytes and
  records no new injection.
- `emit` runs inside the write transaction and returns whether the bytes
  reached stdout; a false return rolls the transaction back, so a failed write
  leaves the continuation deliverable.
- Channels: `session_start` (source `compact` or `resume`), `post_tool`,
  `user_prompt`. Re-emission is capped at `MAX_ATTACH = 3` and only on the
  prompt channel.
- Fail-open: every error path in `hook.rs` logs and exits 0; a watchdog and a
  panic silencer wrap the whole dispatch.

## 7. Current known defects

| id | Defect | Evidence |
|---|---|---|
| D-A | No hard stop at the render target; only the 1000-token ceiling is enforced | `render.rs::run_steps` returns after the last rung regardless of size |
| D-B | `estimate_tokens` under-reads the current format by up to 7.9% | table in §5; `bench/results/v0.1.1/verdicts.json` E4 = FAILED, worst 804 |
| D-C | `[FILE_ACTIVITY]` ladder steps 8 → 4 → 0 with no rung between | `SPEC_STEPS[0]` and `SPEC_STEPS[6]`; pinned by `the_working_files_ladder_still_steps_from_four_to_zero` |
| D-D | No constraint entity: a constraint survives only if it fits inside the ROOT prompt's 160–240 char cap | no `constraints` table; `[FIRST_MESSAGE]` truncates |
| D-E | `working_score` has no unit test of its own; only an integration test replays a sweep | `snapshot.rs::working_score` is private |
| D-F | E6 capsule acceptance failed once in eight deliveries | `verdicts.json` E6 = FAILED |
| D-G | s3 was never run: no paired trials exist, so E3 is INCONCLUSIVE for want of data | `bench/results/v0.1.1/trials/` holds s1 and s2 only |
| D-H | s3's target is partly recoverable from the tree: `legacy_tsv_v2.py` is the only tsv-named importer with `DELIMITER = None` | `s3_working_set.py::build_tree` mutates exactly one module |

Note on D-G: the *pairing logic* (`scenario_aggregate.py::pair_up`) is already
correct and already rejects unmatched pairs. What is missing is (a) explicit
pair identity in trial metadata rather than an implicit match on the
`replicate` integer, (b) a regression test proving symmetry, and (c) s3 ever
being scheduled.

## 8. Ledger field feasibility against the real event stream

| Requested field | Deterministically observable? | Source | Present today |
|---|---|---|---|
| ROOT_OBJECTIVE | **Yes** | `UserPromptSubmit.prompt`, first ≥20-char prompt of the epoch | Yes — `intents` (level ROOT), rendered `[FIRST_MESSAGE]` with event id and timestamp |
| CONSTRAINTS | **Yes, as verbatim quotation** | `UserPromptSubmit.prompt` | **No** — this is the gap (D-D) |
| REJECTED_APPROACHES | **Yes at file granularity** | edit/revert hash chain + command outcomes | Yes — `dead_ends`, rendered `[REVERTED_EDITS]` |
| WORKING_SET | **Yes** | `file_stats` + pinning from dead ends/attempts/failure mentions | Yes — `working_score`, rendered `[FILE_ACTIVITY]` |
| CURRENT_STATE | **Yes** | git head/branch, edit count, last test outcome, checkpoint partiality | Yes — `[WORKSPACE_STATE]` |
| ACTIVE_FAILURE | **Yes** | `commands` where the latest run of a signature is FAIL | Yes — `[TEST_RESULT]` |

**What CONSTRAINTS can and cannot be.** The event stream gives the user's
sentences, not their meaning. A deterministic extractor can therefore do
exactly one honest thing: select the sentences of a user prompt that contain an
explicit requirement or prohibition cue, and quote them verbatim with the cue
that selected them and the event they came from. It cannot decide whether a
constraint is still in force, cannot rank constraints, and cannot turn a
preference into a hard rule. Anything beyond verbatim selection would be
invention, and the capsule would be asserting something the log does not
contain.

**What REJECTED_APPROACHES cannot be.** Velra does not retain edit bodies — only
hashes, line counts and a one-line `excerpt`. It can say *which file* was
edited and then reverted, and *which command ran next and how it exited*. It
cannot say which idea was being tried. The existing capsule is already correct
about this: it says "Observed afterward: `pytest -q` FAIL. Causal link:
UNCONFIRMED." rather than claiming a reason. That framing is kept.

## 9. What the v0.1.1 evidence actually says

From `bench/results/v0.1.1/verdicts.json`, untouched:

- **E1 FAILED** — baseline 3/3, Velra 4/4 on `avoided_dead_ends`. Both arms
  avoided the burned ground every time. The metric could not discriminate.
- **E2 FAILED** — baseline 4/4, Velra 3/4 on `constraint_honoured`. The
  baseline honoured a turn-0 constraint after compaction *more often than
  Velra did*.
- **E3 INCONCLUSIVE** — no s3 trials exist.
- **E4 FAILED** — worst delivered capsule 804 real tokens against an 800 ceiling.
- **E5 PASSED** — 937 hook invocations, 0 non-zero exits, 0 stderr bytes.
- **E6 FAILED** — 1 of 8 deliveries was treated by the agent as injected content.

Two of those are ceiling effects (E1, E2): native compaction did not lose the
information in a way that changed what the agent did, so the scenarios were not
causally testable as built. The v0.1.1 s1 fixture was already rebuilt in
response to E1; s2 and s3 have not been. This is the single most important input
to the Phase 2/3 work: **the benchmark's job is to establish whether the target
information was lost at all before it can attribute anything to Velra.**
