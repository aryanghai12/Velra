# Velra v0.1.2 benchmark report — Token-Burn requalification

**Status: qualification evidence.** Four matched pairs, eight trials, sixteen
Claude Code sessions. The preregistration says qualification pairs test
whether the benchmark works and do **not** count toward the scorecard. The
formal stage (four pairs per benchmark) has not been run. Read every result
below as "what four pairs showed", not as an effect size.

| | |
|---|---|
| Product | Velra 0.1.2, commit [`51b96cb`](https://github.com/aryanghai12/Velra/commit/51b96cb5fd03e1bcba9e4c5a5140727ddbe8055c) (working tree clean at every trial start) |
| Scored by | pipeline at `5fb1fe7` (see [§15](#15-how-the-aggregate-was-produced)) |
| Evidence | [`bench/results/v0.1.2-requal/`](../bench/results/v0.1.2-requal/), frozen in commit `54a7b8f` |
| Preregistration | `velra-tokenburn` **v1.1.0**, sha256 `c13150b6da58a0861a6319986b7a18c0e0d3659c043adba8ac44c6af326ffcd2` ([file](../bench/tokenburn/preregistration_tokenburn.json)) |
| Claude Code | 2.1.280 |
| Model | Sonnet (`--model sonnet`) |
| Run window | 2026-09-23 12:04–12:40 UTC, Windows 11 x64 |
| Live gate | `VELRA_ALLOW_LIVE_BENCHMARK=1` and `--live`, from a terminal outside Claude Code |

---

## 1. Executive summary

- **The mechanism held in every Velra trial.** In all four Velra arms, causal
  links A–H passed. The source session had a large state, the state was absent
  from the repository, Velra retained it, `velra restore` staged it, the new
  session's `SessionStart(startup)` delivered it exactly once, the destination
  received every declared marker, acted on it, and finished the task
  correctly.
- **Every Velra arm met the registered correctness criterion. No baseline arm
  did.** This does **not** mean "baselines could not fix the bug". All four
  baselines made the target test pass and kept the scenario invariant. They
  failed because they also edited the modules of the two *other* failing tests,
  which the source conversation had explicitly placed out of scope. The
  correctness criterion counts that. See [§11](#11-correctness-definition).
- **Total input burden was lower in 3 of 4 pairs** (−25.7%, −28.0%, −32.0%)
  and higher in one (+13.4%). Pair verdicts: **3 × VELRA_WIN,
  1 × INCONCLUSIVE**, 0 × BASELINE_WIN, 0 × TIE.
- **Velra reached the first correct action sooner in every pair**: 2, 2, 2
  and 4 tool steps, against 6, 7, 4 and 5 for the baselines.
- **Capsules stayed small and bounded:** 737, 737, 680 and 693 tokens by
  Velra's own render-budget estimate (1,423–1,535 characters delivered). These
  are estimates, not tokenizer measurements.
- **The ~250K context is a synthetic proxy.** The source sessions read a
  generated fixture of ≈250K estimated tokens. Claude Code does not report the
  context size it actually held, so no observed context size exists anywhere
  in this evidence.

The defensible finding is narrow. In these scenarios, a fresh session that
received Velra's bounded capsule continued the interrupted task inside the
scope the previous conversation had set. It did so in fewer steps, and usually
with less input, than a fresh session that had only the repository. The
reduction depends on the workload, and a qualification-sized sample cannot
estimate it.

---

## 2. Product hypothesis

> A developer can leave a large Claude Code conversation behind, start a
> brand-new session, restore a small bounded operational state, and continue
> the work without rehydrating the old conversation.

The operational state in question is what exists only in the conversation:

- which of several failing tests is the live task;
- a constraint stated once in chat;
- an approach that was tried and reverted without a commit;
- what was explicitly declared out of scope;
- the next step.

None of that is in the repository, and the benchmark checks this (link B).

---

## 3. Benchmark protocol

Each trial is one arm of one matched pair and has two Claude Code sessions.

```
fixture repo (3 genuinely failing tests) + ~250K-token synthetic log fixture
        │
        ▼
SOURCE SESSION  — 17 scripted turns: orientation, a constraint stated once,
                  a dead end tried and reverted with `git restore`, reading
                  the load fixture, the next step named, nothing fixed
        │
        ├── validity gate (both arms): auto-memory off and memory dir empty,
        │   target test FAILS, invariant as generated, worktree as generated
        │
        ├── Velra arm only: `velra restore --session <source> --json`
        │     A: after the source session ends
        │     B: one turn before `/clear`, source process still alive
        │
        ├── A: the source ends            B: `/clear` sent to the source (both arms)
        ▼
DESTINATION SESSION — a brand-new process, both arms, one identical prompt:
        A: "Continue where we left off and finish the task. Do not ask me what it was."
        B: "Continue the task and fix the bug."
        │
        ▼
end state captured → target test, full suite, invariant, edits → analysis
```

The Velra arm runs `velra enable` and the baseline arm runs `velra disable`.
Both then get identical memory-isolation settings. The **only** asymmetry
between arms is `velra restore` and the capsule it stages. The baseline keeps
full repository access, every native feature, the same flags and the same
permission mode (`bypassPermissions`).

**Important, and easy to misread:** in both benchmarks the baseline's
destination is a *new session*. It is not a `--continue` or `--resume` of the
source. The preregistration's prose calls Baseline A "a native continuation";
the harness has never implemented that ([DECISIONS D68](../DECISIONS.md)). So
this report compares **a fresh native session** with **a fresh session plus
Velra's capsule**. It does **not** measure what continuing the ~250K
conversation itself would have cost.

---

## 4. Version and provenance

Every trial records its identity in `trial_meta.json → pair_key`, and pairing
refuses to match two arms whose keys differ:

| Field | Value |
|---|---|
| `git_head` | `51b96cb5fd03e1bcba9e4c5a5140727ddbe8055c` (all 8) |
| `claude_version` | `2.1.280 (Claude Code)` (all 8) |
| `model` | `sonnet` (all 8) |
| `permission_mode` | `bypassPermissions` (all 8) |
| `context_ladder_rung` | `250000` (all 8) |
| `turn_script_hash` | A: `e84715336b0762fc`, B: `8656331d0e9dd957` |
| `fixture_seed` | A: `f6233e55efb918ca`, B: `a070849ef9ae109d` |

`repo_provenance` in each `trial_meta.json` records a clean working tree at
trial start. The only untracked path was the run's own output root.

## 5. Environment

A single Windows 11 x64 machine. The release binary was built at `51b96cb`,
and the readiness gate passed at 11:57:55 UTC (`readiness.json`: `READY FOR
LIVE EVALUATION`). Claude Code ran headless with `-p --input-format
stream-json --output-format stream-json --include-hook-events
--strict-mcp-config`.

## 6. Isolation controls

Preregistration 1.1.0 exists because the 1.0.0 run was confounded: Claude Code
auto-memory carried scenario state into **both** arms. Under 1.1.0 a trial is
invalid, and never continued to a destination session, unless all of the
following hold. They are checked by the same code for both arms
(`bench/tokenburn/isolation.py`) and recorded in each `trial_meta.json`:

| Control | Result, all 8 trials |
|---|---|
| `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` in the session environment | ✓ before source, ✓ before destination |
| `autoMemoryEnabled: false` in user settings, read back | ✓ before source, ✓ before destination |
| `autoMemoryDirectory` unset | ✓ (`null`) |
| Fixture's memory directory empty and free of scenario terms | ✓ before source, ✓ before destination, ✓ after destination |
| Earlier memory for the fixture moved aside (never deleted) | 6 trials had leftover memory from the invalidated 1.0.0 run; quarantined before the trial |
| User settings restored byte for byte after the trial | ✓ |
| Isolation identical for both arms | ✓ |
| Target test **FAIL** at the handoff | ✓ |
| Invariant as generated at the handoff | ✓ |
| Worktree as generated at the handoff (no commit, stash or stray file) | ✓ |
| Old transcript replayed into the destination | never |

Result: **8 of 8 trials valid**, no trial invalid, dropped or unassigned.

## 7. Qualification structure

| Stage | Pairs | Status |
|---|---|---|
| Qualification | 2 per benchmark, 4 total, 8 trials | **this report** |
| Formal | 4 per benchmark, 8 total, 16 trials | not run |

Arm order alternated within each benchmark: in `q1` the baseline ran first,
in `q2` Velra ran first (`run_state.json`).

## 8. Scenarios

The fixture is a small payments service with **three genuinely failing
tests** (`test_retry`, `test_ledger`, `test_reconcile`).

**A — cold continuation** (`A_cold_continuation`, transition: new session).
The live task is `test_retry_preserves_idempotency_key`. In chat the user says
the ledger and reconcile failures are "someone else's" and must be left alone.
They state a constraint (the key must stay a pure function of the request, no
process-wide state), try a module-level memo, and revert it. The session ends
with the next step named (`retry_backoff`).

**B — `/clear` survival** (`B_clear_survival`, transition: `/clear`). The live
task is `test_march_window_totals`. The user states an invariant that exists
nowhere in the code (windows close on the booking date, never the value
date), tries widening the window by a day, reverts it, says the other two
failures are not to be touched, and names `in_window` as the next step. Then
`/clear`.

A leak scan checks every surface the destination session can read for the
scenario's terms before any session runs: the tree, file names, git history
and refs, `CLAUDE.md`, project and auto memory, the environment, and the
prompts. It was clean for all trials (link B).

The synthetic context fixture is generated engineering output (build logs,
pytest output, diffs), not filler. The source session reads it as part of its
script. Its size is **250,728** (A) and **250,571** (B) estimated tokens at
3.6 characters per token. It is labelled `synthetic_load_fixture` everywhere,
and `actual_observed_context_size` is `unavailable` in every trial.

## 9. Metrics

| Metric | Source | Status |
|---|---|---|
| `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `output_tokens` | the destination's structured `result` usage record | measured |
| `total_input_tokens` | input + cache reads + cache creation | measured (derived) |
| cache condition | the two cache fields | `HIT` in all 8 |
| turns, tool calls, file reads, searches | the destination's tool-use events | measured |
| `steps_to_first_correct_action` | tool steps before the first Read/Edit/Grep of the target file | measured |
| `capsule_tokens` | Velra's own render-budget estimate (`velra_restore.json`) | **proxy** |
| `capsule_chars` | the delivered hook output | measured |
| `synthetic_context_size` | the generated fixture | **proxy**, never a Claude observation |
| `actual_observed_context_size` | none | **unavailable** |

## 10. Measurement precedence

Only structured usage counts. The pipeline never recovers a number from
terminal output, stdout or status lines; `telemetry.scan_for_terminal_scraping`
fails the build if code tries. A missing field is `unavailable`, not zero. A
field present on only some records is not summed. `UNKNOWN` cache is not
`EXPIRED`. Each destination had one `result` usage record. The 13–25
per-message `assistant` usage records are alternative reports of the same
spend and are not added to it (`analysis.json → usage_selection`).

## 11. Correctness definition

A destination is **correct** only if *all five* declared checks pass
(`bench/tokenburn/metrics.py`, manifests in `bench/tokenburn/scenarios.py`):

| Check | A | B |
|---|---|---|
| `target_test` | `tests/test_retry.py::test_retry_preserves_idempotency_key` passes | `tests/test_reconcile.py::test_march_window_totals` passes |
| `required_final_state` | `retry.py` has no `_memo`, `setdefault`, `global ` | `reconcile.py` uses `booking_date`, no `_SLACK`, no `timedelta(days=1)` |
| `invariant` | same as above: the reverted approach is not back | same as above: the reverted approach is not back |
| `no_regression` | ≤ 2 suite failures | ≤ 2 suite failures |
| `dead_end_avoided` | no edit to `ledger.py` or `reconcile.py` | no edit to `retry.py` or `tests/test_reconcile.py` |

Read the last row carefully. Its name says "dead end", but its file list is
the **out-of-scope** code: the modules of the failing tests the user said to
leave alone (and, in B, the target's own test file). The reverted approach
itself is checked by `invariant` and `required_final_state`.

What the eight destinations actually did:

| Trial | target test | invariant | suite failures | out-of-scope files edited | correct |
|---|---|---|---|---|---|
| A q1 baseline | pass | ✓ | 0 | `ledger.py`, `reconcile.py` | **no** |
| A q1 Velra | pass | ✓ | 2 | — | yes |
| A q2 baseline | pass | ✓ | 0 | `ledger.py`, `reconcile.py` | **no** |
| A q2 Velra | pass | ✓ | 2 | — | yes |
| B q1 baseline | pass | ✓ | 1 | `retry.py` | **no** |
| B q1 Velra | pass | ✓ | 2 | — | yes |
| B q2 baseline | pass | ✓ | 0 | `retry.py` (and `ledger.py`) | **no** |
| B q2 Velra | pass | ✓ | 2 | — | yes |

So:

- **No arm, baseline or Velra, reintroduced the reverted approach.** The
  baselines did not "fall back into the dead end".
- **Every baseline went beyond the task.** It fixed the other failing tests
  too, editing code the source conversation had assigned to someone else.
  Every Velra arm fixed only its target and left the other two tests failing,
  as instructed.
- In every baseline, an out-of-scope file was also touched *before* the first
  correct action (`first_correct_action.dead_end_touched_first`).

You can argue that fixing extra tests is helpful. The registered criterion
says otherwise, and so did the user in the source conversation. The scope
instruction was stated only in chat, which is exactly the kind of state the
benchmark is about. The table above lets you judge it yourself.

## 12. Causal-chain methodology

Each trial is evaluated link by link (`bench/tokenburn/causal.py`). The first
link that cannot be demonstrated names the failure, and an undemonstrated link
never rounds up to a pass.

| Link | Meaning | Evidence |
|---|---|---|
| A large state | ≥ 15 source turns and a recorded context load | `trial_meta.json`, `context_fixture.json` |
| B absent natively | no readable surface carries the state | leak scan |
| C retained | every declared marker is in the ledger before the capsule is built | `velra_restore.json → ledger_scan` |
| D staged | a capsule for *this* workspace, not stale, eligible for `startup` | `velra_restore.json → staged` |
| E delivered | `SessionStart(startup)` emitted it, claimed exactly once, hook exit 0 | destination hook events |
| F received | exactly one capsule, every marker in the delivered bytes, new session id, no transcript replay | destination capture |
| G used | the destination identified the state or took the declared first correct action | `analysis.json` |
| H correct | §11 | `final_state.json` |
| I burden reduced | needs both arms, decided per pair | `verdicts.json` |

C–G describe Velra's machinery and are `n/a` for the baseline arm.

## 13. Pairing methodology

Two arms pair only if all ten identity fields in [§4](#4-version-and-provenance)
match. A pair with a missing or invalid arm is dropped and named. Result: 4
pairs, 0 dropped, 0 unassigned.

Verdicts follow the registered order: capture usable → preconditions → Velra
wrong and baseline right is `BASELINE_WIN` → burden must be measured on both
arms → `VELRA_WIN` needs Velra correct **and** ≥ 25% less total input →
`BASELINE_WIN` needs baseline correct and ≥ 25% more → `TIE` needs **both
arms correct** and a change under 25%. A combination no rule covers is
`INCONCLUSIVE`.

---

## 14. Results

### Per-pair breakdown

| Pair | Baseline total input | Velra total input | Change | Baseline correct | Velra correct | Verdict |
|---|---:|---:|---:|:-:|:-:|---|
| A q1 | 369,854 | 274,900 | **−25.67%** | ✗ | ✓ | VELRA_WIN |
| A q2 | 250,203 | 283,635 | **+13.36%** | ✗ | ✓ | INCONCLUSIVE (no registered rule) |
| B q1 | 328,371 | 236,454 | **−27.99%** | ✗ | ✓ | VELRA_WIN |
| B q2 | 481,550 | 327,286 | **−32.03%** | ✗ | ✓ | VELRA_WIN |

`A q2` is not a TIE. Only the Velra arm was correct, and it spent 13.4%
*more*. `VELRA_WIN` needs the reduction and `TIE` needs both arms correct, so
no registered rule decides the pair. The run's own first aggregate printed it
as a TIE "because both arms were correct", beside a baseline recorded
incorrect. That was a scoring bug, fixed before this evidence was frozen
([DECISIONS D69](../DECISIONS.md)).

### Aggregate results

| Benchmark | Pairs | Verdicts | Correct (baseline / Velra) | Median total-input change (values) |
|---|---|---|---|---|
| A cold continuation | 2 | 1 VELRA_WIN, 1 INCONCLUSIVE | 0/2 / 2/2 | −6.16% (−25.67, +13.36) |
| B `/clear` survival | 2 | 2 VELRA_WIN | 0/2 / 2/2 | −30.01% (−27.99, −32.03) |

The medians include every pair whose mechanism held, whatever its verdict. A
pair does not leave the median because its burden went against Velra. With
two pairs per benchmark, a median is a description, not an estimate.
Benchmarks are not pooled with each other.

### Causal results

| Trial | A | B | C | D | E | F | G | H | Chain |
|---|---|---|---|---|---|---|---|---|---|
| Velra × 4 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | complete through H; I decided per pair |
| Baseline × 4 | ✓ | ✓ | n/a | n/a | n/a | n/a | n/a | ✗ | breaks at H (scope, §11) |

Link I, per pair: reduced in A q1, B q1 and B q2; not reduced in A q2.

The delivered bytes carried every declared marker. A: `src/payments/retry.py`,
`test_retry_preserves_idempotency_key`, `retry_backoff`. B:
`src/payments/reconcile.py`, `test_march_window_totals`, `booking`,
`in_window`. The ledger held all of them before staging (`ledger_scan`).

One honest detail about link G: it passed through the "took the declared first
correct action" branch. The stricter prose check, which names all four of
current task, active failure, relevant files and next action, was complete in
**none** of the eight destinations (Velra 2/4 items in each, baselines 0–2/4).

### Token and input burden

| Pair | Arm | Uncached input | Cache read | Cache creation | **Total input** | Output |
|---|---|---:|---:|---:|---:|---:|
| A q1 | baseline | 18 | 348,840 | 20,996 | 369,854 | 4,305 |
| | Velra | 14 | 256,302 | 18,584 | 274,900 | 3,709 |
| A q2 | baseline | 12 | 226,649 | 23,542 | 250,203 | 5,326 |
| | Velra | 14 | 262,512 | 21,109 | 283,635 | 4,825 |
| B q1 | baseline | 16 | 305,882 | 22,473 | 328,371 | 4,945 |
| | Velra | 12 | 217,190 | 19,252 | 236,454 | 2,979 |
| B q2 | baseline | 16 | 432,426 | 49,108 | 481,550 | 5,699 |
| | Velra | 16 | 305,807 | 21,463 | 327,286 | 3,683 |

Almost all input is cache reads: each destination re-reads its growing
context once per tool step. Quoting "uncached input" alone (12–18 tokens)
would be the most misleading number this benchmark could print, so the
headline is the total.

### Effort

| Pair | Arm | Tool calls | File reads | Searches | Steps to first correct action |
|---|---|---:|---:|---:|---:|
| A q1 | baseline | 9 | 1 | 0 | 6 |
| | Velra | 9 | 3 | 2 | **2** |
| A q2 | baseline | 18 | 12 | 1 | 7 |
| | Velra | 9 | 2 | 1 | **2** |
| B q1 | baseline | 16 | 9 | 1 | 4 |
| | Velra | 6 | 2 | 0 | **2** |
| B q2 | baseline | 17 | 11 | 0 | 5 |
| | Velra | 9 | 3 | 1 | **4** |

Steps to first correct action is Benchmark B's registered primary effort
metric. The registered materiality threshold is 2 steps, so B q1 (−2) is
material and B q2 (−1) is not. Each destination was driven by a single prompt,
so every trial has `turns = 1`.

### Capsule size

| Trial | Estimated tokens (Velra render budget) | Delivered characters |
|---|---:|---:|
| A q1 | ~737 | 1,535 |
| A q2 | ~737 | 1,525 |
| B q1 | ~680 | 1,423 |
| B q2 | ~693 | 1,443 |

The renderer's target is 740 estimated tokens (`render::DEFAULT_BUDGET_TOKENS`),
its hard ceiling is 1,000, and the preregistered ceiling is 800. The token
figures are Velra's own estimator's output, not tokenizer counts.

---

## 15. How the aggregate was produced

```bash
python bench/tokenburn/aggregate.py \
    --trials bench/results/v0.1.2-requal/trials --out bench/results/v0.1.2-requal
```

That command runs parse → metrics → causal → pairing → verdict → pool →
report. It was re-run over the finished trials at `5fb1fe7`. Every
per-trial `analysis.json` and `causal_chain.json` came out **byte-identical**
to what the live run wrote. Only the pair verdicts and pooled summary
changed (D69). The aggregate records the scorer as `scored_by`.
`bench/tests/test_tokenburn_requal_evidence.py` re-scores the committed
analyses with the current pipeline on every test run and fails if anything
disagrees.

The runner now refuses to write into this tree (`runroot.PROTECTED`). To
re-score it, copy it to a new root.

## 16. Limitations

- **Qualification only.** Four pairs, two per benchmark; the preregistration
  excludes them from the scorecard. The formal stage has not been run.
- **Baseline is a fresh session, not a resumed one** (D68). The comparison
  says nothing about the cost of `--resume`/`--continue` on the large
  conversation.
- **Synthetic context.** The ~250K figure describes a generated fixture the
  source read. Claude Code's actual context size is unavailable.
- **Capsule tokens are estimates.**
- **One model, one Claude Code build, one machine, one operating system.**
- **One scenario family,** written by the project, with a single fixture.
- **Correctness depends on a scope check** whose manifest key is named
  `dead_end` (§11).

## 17. Threats to validity

- **Arm order and server-side caching.** Arms alternated. Velra ran second
  in A q1 and B q1 (−25.7%, −28.0%) and first in A q2 and B q2 (+13.4%,
  −32.0%). Four pairs cannot separate order effects from scenario variance.
- **Destination variance is large.** The two baseline arms of A spent 369,854
  and 250,203 tokens on the same prompt, a 48% spread. The burden change
  between arms is of the same order as that noise.
- **Scenario authorship.** The fixture and scripts were designed by the
  project that makes the product. The leak scan guards against state being
  readable. It does not guard against scenarios that happen to suit the
  capsule format.
- **Instruction-following as the discriminator.** The correctness difference
  rests on the baselines' scope violations. A model that is more conservative
  about touching unrelated failing tests could close that gap without any
  restored state.

## 18. Interpretation

What the evidence supports:

- Velra's capture → retain → stage → deliver → receive path works end to end
  on Claude Code 2.1.280, across a new session and across `/clear`, with
  auto-memory ruled out as a carrier.
- The capsule carried operational state that exists only in conversation:
  which task was live, what was out of scope, what the next step was. In
  these scenarios the destination used it to stay in scope and to reach the
  target file sooner.
- Input burden fell in three of four pairs.

What it does not support:

- "Velra always saves tokens." (A q2: +13.4%.)
- Any claim about continuing a real 250K-token context, or about Claude Code's
  context limit.
- Any claim of an effect size, or of superiority over `--resume`.
- "Baselines re-entered the rejected approach." None did.

## 19. Release implications

The v0.1.2 release can state:

- Velra preserves bounded operational state across Claude Code sessions, so a
  new session can continue work without replaying the previous transcript.
- Fresh qualification trials demonstrated bounded state capture, staging,
  startup delivery, receipt, use and task correctness in the tested
  scenarios.
- Fresh qualification results showed lower total input burden in 3 of 4
  matched pairs.

It should not state an effect size, universal savings, or production scale.

## 20. Raw artifacts

| Artifact | Path |
|---|---|
| Pipeline report | [`bench/results/v0.1.2-requal/report.md`](../bench/results/v0.1.2-requal/report.md) |
| Aggregate (every metric with provenance) | [`aggregate.json`](../bench/results/v0.1.2-requal/aggregate.json) |
| Pair verdicts | [`verdicts.json`](../bench/results/v0.1.2-requal/verdicts.json) |
| Readiness gate | [`readiness.json`](../bench/results/v0.1.2-requal/readiness.json) |
| Run state and arm order | [`run_state.json`](../bench/results/v0.1.2-requal/run_state.json) |
| Per-trial directories | [`trials/`](../bench/results/v0.1.2-requal/trials/) — `trial_meta.json`, `source_handoff.json`, `context_fixture.json`, `velra_restore.json`, `final_state.json`, `analysis.json`, `causal_chain.json`, `arm_setup.txt` |
| Raw captures, by hash | [`raw_captures.sha256.json`](../bench/results/v0.1.2-requal/raw_captures.sha256.json) |
| What each result tree is | [`bench/results/README.md`](../bench/results/README.md) |
| Runner and pipeline | [`bench/tokenburn/`](../bench/tokenburn/), design notes in [`bench/README.md`](../bench/README.md) |

Raw session captures are not committed. The transcripts carry account context
Claude Code injects into every session, and all of them embed local paths.
The manifest lists all 52 files by size and SHA-256.

**Historical benchmarks** (superseded, not part of this release's evidence):
the invalidated 1.0.0 Token-Burn qualification (tag
`tokenburn-qualification-v0.1.2`, `bench/results/v0.1.2/tokenburn/`), the
v0.1.2-hardened and v0.1.1 efficacy runs, and the v0.1 `/compact` benchmark
([`BENCHMARK_REPORT.md`](../BENCHMARK_REPORT.md)).

---

[← README](../README.md) · [Architecture](ARCHITECTURE.md) · [Reproduce the benchmark](../bench/README.md)
