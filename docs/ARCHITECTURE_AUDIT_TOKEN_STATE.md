# Architecture audit — token burden and cross-session state

Written from the code as it stands after Phase 3. Every claim names the file it
came out of, and nothing here is a measurement of Claude: **no live trial has
been run.** Where this document talks about tokens, it talks about the
machinery for measuring them and about what that machinery is and is not
allowed to say.

The product thesis under test, stated once:

> A developer can intentionally leave a huge Claude Code conversation behind,
> start a brand-new session, restore only a small bounded operational state,
> and continue the work without rehydrating the entire old conversation.

```
LARGE / EXPENSIVE CONVERSATIONAL STATE
            ↓  intentional exit
  NEW FRESH CLAUDE SESSION
            ↓  velra restore  →  SessionStart(startup)
  SMALL VELRA OPERATIONAL STATE
            ↓
       CONTINUE WORK
```

Velra is not assumed to win. Of the seventeen matched pairs the selftest
scripts, **three** come out as a Velra win; the rest are two baseline wins, six
ties and six inconclusive. One of the ties — a baseline that works the state
out for itself — is the case the verdict logic is most carefully written to
report honestly.

---

## 1. Benchmark architecture

```
bench/
  tokenburn/                 the v0.1.2 Token-Burn benchmark
    scenarios.py             two benchmarks, one realistic repository
    context_fixture.py       the context-load ladder, generated to disk
    leakscan.py              nine surfaces a destination session can read
    live_trial.py            the live driver          (built; never executed)
    mock_adapter.py          synthetic trials in the live artifact shape
    parse.py                 raw artifacts  -> ParsedTrial
    telemetry.py             metrics that carry their own provenance
    metrics.py               ParsedTrial    -> A1..A4
    causal.py                the A..I chain
    pairing.py               matched pairs on recorded identity
    verdict.py               one of four values per pair
    aggregate.py             the pipeline, end to end
    report.py                the §17 tables
    safety.py                the live-execution gate
    smoke.py                 restore → SessionStart, offline, real binary
    selftest.py              eighteen scripted cases
    run.py                   --selftest / --dry-run / --smoke / --live
    preregistration_tokenburn.json
  legacy/                    S1/S2/S3 and their runners, archived
  harness/                   shared with the archive; unchanged
  tests/                     pytest for both systems
```

The pipeline is one path and there is no second evaluator:

```
raw artifacts
   → parse.parse_trial          dedupe, malformed accounting, deliveries
   → metrics.evaluate           A1 burden, A2 recovery, A3 effort, A4 correctness
   → causal.evaluate            links A..I
   → pairing.pair_up            matched pairs, dropped pairs named
   → verdict.pair_verdict       VELRA_WIN | BASELINE_WIN | TIE | INCONCLUSIVE
   → verdict.pool               counts, and only comparable summaries
   → report.markdown            the tables
```

`mock_adapter.py` writes artifacts into a trial directory. `live_trial.py`
writes artifacts into a trial directory. Both are read by `parse.py` and
nothing downstream can tell which produced them except through the
`telemetry_source` recorded in them. `test_tokenburn_pipeline.py` asserts that
no module outside `verdict.py` defines a verdict vocabulary, and that the two
writers agree on the artifact set.

### Trial directory

```
<trial>/
  trial_meta.json        identity, pair key, declared manifest, leak scan
  stream.jsonl           destination session, Claude Code stream-json
  source_stream.jsonl    source session, same format
  transcript.jsonl       destination session artifact
  usage.json             structured usage rollup, if the interface exposes one
  velra_restore.json     restore + staging + claim lifecycle
  final_state.json       end state, target-test exit, suite failures
  context_fixture.json   which ladder rung this trial ran under
  analysis.json          written by metrics.evaluate
  causal_chain.json      written by causal.evaluate
```

Every artifact but `trial_meta.json` is optional. An absent one produces
unavailable metrics and a broken causal link, never an exception: a benchmark
that crashes on a half-captured trial loses the trials either side of it.

---

## 2. MockClaudeAdapter

`bench/tokenburn/mock_adapter.py`.

One dataclass, `MockSpec`, with a field for every condition the benchmark needs
to be able to model: session creation, a source and a distinct destination
session, user turns, assistant turns, tool calls, tool results, context growth,
compaction, `/clear`, resume, `SessionStart` events, `velra restore`, staged
capsule creation, capsule injection, cache state, input and output token usage,
and the final task outcome.

Determinism comes from `random.Random(spec.seed_string())`, which hashes the
trial name, the scenario and the seed. The same spec produces byte-identical
artifacts anywhere, so a failing selftest is reproducible rather than a story
about a flaky run.

**The adapter marks its own work.** Every synthetic `usage` object carries
`_telemetry_source: "synthetic_fixture"`, so the provenance that reaches the
report says `synthetic_fixture` and not `structured_usage`. A number the mock
invented cannot be mistaken downstream for a number Claude reported, and the
selftest fails if any synthetic trial ever reports `structured_usage`.

The eighteen scripted cases live in `selftest.py`:

| # | case | expected |
|---|---|---|
| 1 | strong Velra win | `VELRA_WIN` |
| 2 | strong baseline win | `BASELINE_WIN` |
| 3 | tie | `TIE` |
| 4 | invalid trial (orphan arm; identity mismatch) | dropped, named |
| 5 | missing telemetry — cache fields on some events only | `INCONCLUSIVE` |
| 6 | malformed JSONL | survives, counted |
| 7 | duplicate event | deduplicated, totals unchanged |
| 8 | cache-hit | `cache_condition = HIT` |
| 9 | cache-miss | `cache_condition = MISS` |
| 10 | successful `velra restore` | links C, D pass |
| 11 | successful SessionStart injection | links E, F pass |
| 12 | double-injection attempt | `INCONCLUSIVE DELIVERY_FAILURE` |
| 13 | wrong workspace | `INCONCLUSIVE STAGING_FAILURE` |
| 14 | stale capsule | `INCONCLUSIVE STAGING_FAILURE` |
| 15 | correctness lost despite token reduction | `BASELINE_WIN TASK_FAILURE` |
| 16 | telemetry unavailable entirely | `INCONCLUSIVE` |
| 17 | incomplete causal chain | `INCONCLUSIVE CAPTURE_FAILURE` |
| 18 | baseline legitimately reconstructing state | `TIE`, baseline correct |

Case 7 asserts that the totals equal case 3's exactly. Case 15 asserts that the
token saving is still *reported* while the verdict goes against Velra. Case 18
is the one that keeps the benchmark honest: if it ever started coming out as a
Velra win, the verdict logic would have acquired a thumb on the scale.

---

## 3. Telemetry architecture and provenance

`bench/tokenburn/telemetry.py`.

### Source order

| source | what it is |
|---|---|
| `structured_usage` | the `usage` object on a `result`/`assistant` event of Claude Code's `stream-json` output. Authoritative. |
| `session_artifact` | a Claude Code session artifact in which the field is **explicitly present**. |
| `synthetic_fixture` | generated by `MockClaudeAdapter`. Real for testing the pipeline, never real for a claim about Claude. |
| `derived` | computed from other metrics; inherits the weakest status of its inputs. |
| `unavailable` | nothing structured carried it. The value is `None` and stays `None`. |

There is no fourth observational source. The `Metric` dataclass refuses to
exist without one, and refuses to hold a value while its status says the value
is absent.

### Provenance, per metric

```
metric_name  value  unit  source  raw_artifact_reference  measurement_status
```

for example

```
input_tokens             81742  tokens  structured_usage  trial-003/stream.jsonl  measured
cache_read_input_tokens   null  tokens  unavailable       trial-003/stream.jsonl  unavailable
```

`measurement_status` is one of `measured`, `proxy`, `unavailable`,
`inconclusive`. The report prints a proxy with a leading `~` and prints an
absent value as its status word, never as `0`, `-` or a blank cell.

### The anti-scraping rule, in executable form

If `cache_read_input_tokens` or `cache_creation_input_tokens` is not explicitly
present in structured data, the metric is `unavailable`. It is not recovered by
matching a regular expression against stdout, stderr, a progress display or a
status line, and it is not guessed from context length.

`telemetry.scan_for_terminal_scraping` enforces that over this package's own
source: it rejects a regular-expression call on a line that also mentions a
presentation surface, and a token field name on such a line. It runs in the
selftest, in the dry-run readiness gate and in
`test_tokenburn_pipeline.py::test_no_terminal_scraping_is_performed`, and a
companion test feeds it a deliberate offender to prove the guard fires.

Reading `event["stdout"]` out of a structured `hook_response` is not scraping
and is not caught: that is a JSON field of a structured event, and it is how
the delivered capsule is located. What is caught is pattern-matching over it
for numbers.

### The expected set of usage-bearing records

Claude Code reports one turn's spend **twice**: on the `assistant` event that
carries the message, and again on the `result` event that closes the turn.
Summing both reported exactly double the true input on a realistic capture, and
not by the same factor on both arms — the number of assistant messages per turn
varies with how much tool use a session did, so the corruption lands unevenly
on the one quantity the benchmark exists to report.

Every usage-bearing record therefore carries a `usage_class` — `result`,
`assistant`, `rollup` or `transcript` — and exactly one class is believed, in
that preference order. The chosen class *is* the expected set: a field has to
be present on every record of that class, and records of any other class are
not summed alongside it. `parse.select_usage_records` records which class was
chosen, what else was available and why, and `analysis.json` carries that
decision so it is inspectable rather than implicit.

Lines of the capture that carry no usage object at all — turn markers, hook
responses, tool results — are not expected telemetry records and are never
counted as missing. A trial with no usage-bearing record of any class reports
`unavailable`, not zero.

### Missing telemetry never becomes zero

`difference`, `percent_change` and `total` return `inconclusive` when any input
is absent. The partial-field rule is stricter still: if
`cache_read_input_tokens` is present on eleven of twelve usage records, the sum
of the eleven is **not** the trial's cache reads, and the metric reports
`unavailable` with the count. Reporting the partial sum would understate a
baseline's cached input, which is the direction that flatters Velra.

### Cache condition

`HIT`, `MISS` or `UNKNOWN`. `UNKNOWN` is returned whenever either cache field
is unavailable, and it is never rewritten as `EXPIRED` anywhere. A run whose
cache state was not reported did not demonstrate a cache miss; it demonstrated
that the telemetry did not say.

### What is currently known to be unavailable

Claude Code's `stream-json` `result` events carry a `usage` object. Whether
that object carries the two cache fields on this build is **not asserted
anywhere in this repository**, because no live trial has been run. If it does
not, those metrics come out `unavailable`, the pair-level burden comparison
becomes `INCONCLUSIVE(TELEMETRY_UNAVAILABLE)`, and that is the honest result —
not a fallback, not an estimate.

---

## 4. Smoke-test flow

`bench/tokenburn/smoke.py`. Offline, against the real binary, no Claude
process.

```
source session exists          seeded through `velra hook ...`
        ↓
`velra restore --session <id> --json`
        ↓
capsule staged                 $VELRA_HOME/staged/<workspace>/staged_capsule
        ↓
new destination session        a different session id
        ↓
SessionStart startup           `velra hook session-start`, source=startup
        ↓
structured additionalContext
        ↓
destination receives the capsule
```

The payloads are the recorded ones in
`tests/fixtures/claude-code/2.1.272/session_start_*.json`, so the hook is
driven with the shapes a real client sends. Claude Code's side of this contract
is a documented interface — JSON on stdin, one JSON object on stdout — which is
why no Claude process is needed to exercise it.

The tiny operational state is the specification's own example, and it is the
only thing the verifier looks for:

```
active_file = src/payments/retry.rs
next_action = inspect retry_backoff()
```

Twenty-two checks, all passing as of this writing, covering: the source session
is restorable; restore stages a capsule; the capsule is within the registered
800-token ceiling; the staged record declares startup-only delivery; it names
the source session and not a destination; the capsule arrives through
`SessionStart`; the session ids differ; the active file and next action both
arrive; the capsule is bounded; the old transcript is not replayed; a second
and a third `SessionStart` do not re-inject it; the staged file is gone once
claimed; it can be staged again; `clear`, `resume`, `compact` and `fork` each
leave it untouched and still staged; and `startup` still gets it afterwards.

The run is also written out as a trial-shaped directory so that
`parse.parse_trial` reads the real hook responses — the delivery the smoke test
finds is found by the same code that will find the live one.

**No live Claude smoke test has been run, and `--smoke` cannot start one.** It
needs an explicit authorization this phase does not have.

---

## 5. Benchmark A — cold-continuation methodology

`scenarios.A_SCENARIO`.

A seventeen-turn session on a small payments service with three genuinely
failing tests. The turns do real work: run the suite, read `retry.py` and its
test, read `idempotency.py`, search for callers, read `git log`, try an
approach, revert it with `git restore`, re-run, read the other two failures,
read four CI logs, and stop with the next step named and nothing changed.

Three things exist only in that conversation:

* which of the three reds is the live task;
* the constraint stated once in turn 4 — the key must stay a pure function of
  the request, no process-wide state;
* the approach tried in turn 8 and reverted in turn 9, **without a commit**, so
  it leaves no trace in `git log`, `git reflog` or any branch.

**Baseline A** is a legitimate native continuation of that session. Full
repository access, every native feature, the same permission mode, the same
flags, the same continuation prompt. Nothing is hidden and nothing is removed.

**Velra A** reaches the equivalent state, then:

```
velra restore --session <source> → staged capsule
                                 → brand-new Claude session
                                 → SessionStart(startup)
                                 → the same continuation prompt
```

The destination receives no transcript. The only additional state is the
capsule, and `causal.link_f` fails the trial if the old conversation appears in
the destination capture.

### Metrics

* **A1 input burden** — `input_tokens` (uncached), `cache_read_input_tokens`,
  `cache_creation_input_tokens`, `total_input_tokens`, `output_tokens`, and the
  context size *actually observed*. Absolute and percentage differences.
  Unlike categories are never combined: `input_tokens` alone is reported beside
  the total and labelled as uncached input, because quoting it as "input" when
  one arm read 300k tokens from cache would be the most misleading number this
  benchmark could print.
* **A2 state recovery** — does the destination identify the current task, the
  active failure, the relevant files and the next action? Scored over the
  assistant prose *and* the targets of its tool calls: an agent that opens the
  right file without narrating it has identified the working file.
* **A3 recovery effort** — turns, tool calls, file reads, searches, and steps to
  the first correct action.
* **A4 final correctness** — the target test passes, the suite does not
  regress, the invariant holds, the dead end was not re-entered.

A4 outranks everything. A Velra arm that spent a tenth of the input and wrote
the wrong fix loses to a baseline that spent everything and got it right, and
that case is decided before the token comparison is looked at.

---

## 6. Benchmark B — `/clear` methodology

`scenarios.B_SCENARIO`.

Seventeen turns on the reconciliation bug, ending in `/clear`. The
post-`/clear` prompt is `Continue the task and fix the bug.` — identical on
both arms.

The invariant that decides the fix is stated once, in turn 4, and nowhere else:
the upstream feed backdates, so a window closes on the booking date and never
on the value date. The dead end — widening the window by a day on each side —
is tried in turn 7 and reverted in turn 8 without a commit.

Both are checked against nine surfaces before a trial is paid for
(`leakscan.py`): file contents, filenames, git history (`log --all -p`,
`reflog --all`, `stash list`), git refs, `CLAUDE.md` at any depth, `.claude/`
project memory, the environment, and the post-transition prompt. A hit on any
of those is fatal. The turn script itself is scanned and reported but is not
fatal: stating the constraint aloud once *is* the setup, and those turns never
reach the destination session on either arm.

### The lifecycle, in order, and why the order is not free

```
   source session, turns 0..15          real work; the invariant stated once
        |
   [velra arm only]  velra restore      <- BEFORE the clear, source still live
        |
   turn 16: /clear                      both arms, same turn, same point
        |
   fresh destination session            a new process on both arms
        |
   SessionStart(startup)                the only source a staged capsule is
        |                                delivered on
   "Continue the task and fix the bug."  identical on both arms
```

`velra restore` **must** run before the `/clear` turn, and that is measured
rather than assumed. `SessionStart(source="clear")` bumps the session epoch
(`reducer.rs`, the `SESSION_START` arm) and `restore::build` reads the
session's *current* epoch through `snapshot::build`. Driven against the release
binary, the same session restores a 566-token capsule before the clear hook
fires and answers `session … has no task state to restore` after it. Restoring
after the clear would have made every Velra-arm B trial a capture failure, and
the scenario would have measured nothing.

It is also the only honest reading of the workflow: a developer about to clear
stages what they mean to carry first. Asking the tool to recover state after
telling it to drop that state is not a thing anyone does. **Phase 2's
clear/resume behaviour is correct and is unchanged** — the fix is the
benchmark's ordering, in `live_trial.drive_session`'s `pause_before_turn` hook,
which runs the restore with the source process still alive.

The destination is a new process on **both** arms. A staged capsule is
delivered on `SessionStart(startup)` and on nothing else, so the Velra arm
could not continue in the cleared process even in principle; making both arms
start fresh is what keeps the comparison symmetric, with the capsule as the
only difference.

**Baseline B** is a genuine native post-`/clear` session with normal repository
inspection allowed. Nothing is copied into it — no transcript, no summary, no
invariant, no capsule. If it reconstructs the state by reasoning, that is a
baseline success and is scored as one. Nothing anywhere forces it to fail.

### Metrics

* **B1** turns and steps to the first correct action;
* **B2** turns to resolution;
* **B3** recovery cost — tool calls, file reads, searches;
* **B4** state correctness — active failure identified, correct working file,
  invariant respected, dead end avoided, task correct.

`baseline = failure` and `velra = one turn` are hypotheses. Neither is
hard-coded anywhere, and `test_a_baseline_that_reconstructs_the_state_is_a_baseline_success`
exists to keep it that way.

---

## 7. Context-load ladder and synthetic fixture generation

`bench/tokenburn/context_fixture.py`.

Rungs: **100K, 250K, 500K, 700K, 800K, 900K**. These are *targets*. Nothing in
this repository asserts that the runtime can hold 900K, and the record for
every observation carries `target_context_size`, `achieved_context_size`,
`context_generation_method` and `reason_if_target_not_reached`.

### Generation

Programmatic, seeded, streamed to disk. Six kinds of realistic engineering
output interleaved — compiler/build logs, pytest output, unified diffs, shell
transcripts, structured tool results and source excerpts — all of it about the
payments fixture, so a session reading it is doing something a developer would
recognise. The budget is split across the four log files the turn scripts
actually read.

Determinism is `random.Random(f"{scenario}|{target}|{FIXTURE_VERSION}|{seed}")`.
Blocks are written as they are produced and the digest is updated incrementally,
so a 900K rung costs a few kilobytes of resident memory and about a second of
wall time. `verify()` re-reads the files and re-computes the digests; a
generator that silently produced half of what it promised fails there rather
than in the report.

Measured on this machine:

| rung | estimated tokens | chars | on disk |
|---|---|---|---|
| 100,000 | 100,297 | 361,070 | 0.36 MB |
| 250,000 | 250,807 | 902,905 | 0.90 MB |
| 500,000 | 500,520 | 1,801,872 | 1.80 MB |
| 700,000 | 700,369 | 2,521,328 | 2.52 MB |
| 800,000 | 800,440 | 2,881,584 | 2.88 MB |
| 900,000 | 900,256 | 3,240,922 | 3.24 MB |

### What a "token" means in that table

The generator has no tokenizer — running one would mean an API call, and the
offline harness makes none. It generates to a character budget from a declared
`CHARS_PER_TOKEN = 3.6` and records `token_basis: "estimated"` and
`chars_per_token_assumed` in every metadata file, so a reader can redo the
arithmetic with their own number.

**`synthetic_context_size` is an estimate of a generated file's size.** It is
never an observation of a Claude session's context. The two are separate
metrics with separate names and separate sources, `metrics.context_sizes` keeps
them apart, and `test_a_synthetic_context_size_is_never_an_observation` fails if
they are ever the same reading.

### Realistic context versus synthetic load

§5 of the Phase 3 specification draws the line and the code keeps it:

* **realistic engineering context** comes from the turn script's own seventeen
  turns of genuine work — reads, searches, edits, test runs, a reverted
  approach, an unresolved task;
* **synthetic load fixture** is the separate, declared mechanism for scaling
  that context up, marked `kind: synthetic_load_fixture` in every record.

The benchmark never claims that repeated generated data represents a real-world
coding session.

Fixtures are generated into the working directory at run time and are not
committed; `.gitignore` carries the rule.

---

## 8. Causal validity model

`bench/tokenburn/causal.py`. Nine links, evaluated in order:

```
A  a large conversational state existed
B  the critical operational information is absent from the fresh native state
C  Velra retained the relevant state
D  Velra staged it
E  SessionStart delivered it
F  the destination actually received it
G  the destination agent used it
H  the task continued correctly
I  the input/context burden was reduced
```

C–G describe Velra's machinery and are `n/a` on the baseline arm; `n/a` never
fails a chain. I needs two arms and is resolved by the pair verdict.

The rule the rest of the benchmark depends on: **a link that cannot be
demonstrated makes the trial INCONCLUSIVE.** Not a Velra win with a caveat, not
a win on the links that did hold. Incomplete causal evidence is the one thing a
benchmark is most tempted to round up, so it is rounded down by construction —
`evaluate` stops at the first link that is not demonstrated and names it.

Two design notes worth keeping:

**Link B is the leak scan, not the baseline's failure.** Reading a baseline
failure as proof that the information was unavailable is circular, and it would
also make a baseline that legitimately reconstructed the state look like a
scenario defect rather than the honest result it is. What link B needs is that
the state was not readable from any surface — a property of the fixture,
measured before any session ran.

**Preconditions and outcomes are separated.** On the Velra arm, A–F are
preconditions: their failure means the trial never got far enough to be
evidence either way, and the pair is `INCONCLUSIVE` with the failure class
named (`CAPTURE_FAILURE`, `STAGING_FAILURE`, `DELIVERY_FAILURE`,
`RECEIPT_FAILURE`). G and H are outcomes. On the baseline arm only A and B are
preconditions — H failing on a baseline *is* the measurement, and turning it
into an invalid trial would delete the benchmark's own result.

---

## 9. Fairness rules

Registered in `preregistration_tokenburn.json`, enforced by construction:

* same Claude Code version, model, repository fixture, permissions, tools,
  initial task, transition point and post-transition prompt on both arms —
  `live_trial.CLAUDE_FLAGS` is one tuple used for all four sessions of a pair,
  and the continuation prompt is a property of the scenario with nowhere for an
  arm-specific variant to live;
* both arms *write* the settings file, so its prior state is never a confound:
  the baseline runs `velra disable` rather than assuming the hooks are absent;
* the baseline keeps full repository access and every native feature;
* a baseline that legitimately reconstructs the state is a baseline success;
* `UNKNOWN` cache condition is never reported as `EXPIRED`;
* scenarios are frozen before the first formal trial, and are not selected
  after results are seen.

Pair identity is recorded, not assumed. `pair_key` carries the benchmark, the
scenario, the pair id, the fixture seed, the model, the Claude Code build, the
turn-script hash, the permission mode and the context-ladder rung. A pair whose
invariants disagree, or which half-exists, is **dropped whole and named**, and
the dropped pairs appear in the aggregate so the denominator stays honest.
`test_every_registered_invariant_is_actually_compared` fails if a registered
invariant stops being checked.

---

## 10. Trial-count rationale

Two stages.

**Qualification: 2 matched pairs per benchmark.** Its purpose is to find out
whether the scenario creates the claimed pain, whether the baseline behaves
legitimately, whether the transition condition is real, whether restore works,
whether the capsule reaches the destination, whether telemetry works, and
whether final correctness is measurable. A weak scenario is redesigned **here**.
Qualification does not count toward the scorecard; `counts_toward_scorecard:
false` is in the pre-registration.

**Formal: 4 matched pairs per benchmark**, after the scenario is frozen. Eight
pairs, sixteen trials, thirty-two Claude sessions (each trial drives a source
and a destination).

Why so few. The archived v0.1.1 system ran four replicates per arm because a
2×N Fisher exact test cannot clear *p* < 0.05 below that. This benchmark is not
looking for a marginal effect that needs a significance test to see: the
hypothesis is that continuing a 500K-token conversation costs an order of
magnitude more input than a fresh session plus an 800-token capsule. An effect
that size is visible in four pairs or it is not there, and running dozens of
weak trials would buy precision about a quantity whose *existence* is the open
question. The registered materiality threshold is a 25% change in total input
burden; anything smaller is a `TIE` however many pairs agree on it.

---

## 11. Live-execution safety

`bench/tokenburn/safety.py`. Three conditions, all required:

1. `--live` passed explicitly;
2. `VELRA_ALLOW_LIVE_BENCHMARK=1` in the environment — exactly `"1"`;
3. the runner is not itself inside a Claude Code session, detected through
   `CLAUDE_CODE_SSE_PORT`, `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT` and
   `CLAUDE_CODE_SESSION_ID`.

The third is not a formality: the trials register and unregister Velra's hooks
in the real user-level `settings.json`, which is the same file a nested session
is reading.

The gate is executable and checked twice — `run.phase_live` begins with
`safety.require_live`, and `live_trial.main` calls it again, because a module
that starts Claude processes must not depend on its caller having been careful.
Three structural tests enforce this: `run.py` may not import `live_trial`
outside `phase_live`; `phase_live` must begin with the gate call; and no module
in the analysis path may import `subprocess` at all.

`--selftest`, `--dry-run` and `--smoke` pass through `safety.assert_offline`,
which records `claude_processes_permitted: 0` into the readiness report. The
selftest is tested with `subprocess.run`, `Popen` and `check_output` all
monkeypatched to raise: it starts no process of any kind.

---

## 12. Provenance, readiness authority and arm isolation

Added by the pre-qualification audit. Each item was a defect found by reading
or by measurement, not a precaution.

### The gate cannot self-invalidate

The dry run writes `bench/results/v0.1.2/readiness.json`; `git status
--porcelain` then lists it; the artifact reported the tree dirty *because it
existed*. A gate that fails on its own output can never pass, and the blocker
it named was not one a reader could act on. `run.SELF_WRITTEN` names the
runner's own output paths and `is_self_written` excludes them from the
clean-tree computation — and from nothing else. Every other uncommitted path
still blocks, and the readiness record carries both counts, so the exclusion is
visible rather than silent. A rename is judged on its destination.

### Evidence names the commit it is about

`live_trial.repo_provenance` records `git_head`, the branch and the working-tree
state into every trial at the moment the trial starts, and `git_head` is a
registered pair invariant — so two arms run against different source cannot
pair. The readiness gate additionally checks the release binary against HEAD
through `bench/harness/provenance.py`, which existed and was simply never
called.

### One authoritative readiness artifact

| path | status |
|---|---|
| `bench/results/v0.1.2/readiness.json` | **the single authoritative current artifact**; says so in its own `artifact` block |
| `bench/results/v0.1.2/hardened/readiness.20260921T101557Z.json` | frozen historical snapshot of the archived hardened run, preserved byte-for-byte (sha256 `02dda3c4f06f7b8f…`) |
| `bench/results/v0.1.2/hardened/readiness.json` | the archived runner's live output slot; nothing else writes it |

The snapshot previously sat beside the current artifact under a name the
archived runner would have **overwritten** on its next invocation, which is not
what "preserved" means. It now lives in a directory nothing writes, and a test
fails if the archived runner ever names it. The hardened evaluation's actual
evidence — `trials/`, `stages/`, `controls/`, `aggregate.json`,
`verdicts_v2.json`, `provenance.json` — is untouched and not duplicated.

### Arm isolation

Every trial of every pair gets its own output directory, its own fixture
directory and its own `$VELRA_HOME`, so no arm can inherit another's tree, git
state, ledger or generated logs. `Scenario.build` removes the fixture tree
before writing it, and `run_trial` wipes the Velra home. `trial_env` strips
every nested-Claude marker. Both arms *write* the hook registration — the
baseline runs `velra disable` rather than assuming the hooks are absent — so
whatever the previous arm left behind is never a confound.

Two things could not be isolated, so they are handled instead of ignored:

* **The user-level `settings.json`** is the thing under test and cannot be
  faked. `phase_live` snapshots it before the first trial and restores it in a
  `finally`, so a run that dies mid-trial does not leave the machine
  reconfigured.
* **Anthropic's prompt cache is server-side and shared.** Whichever arm runs
  second in a pair may find a warmer cache. Running the baseline first every
  time would hand that advantage to Velra in every pair, systematically and
  invisibly, in exactly the metric being reported. `run.arm_order` alternates
  by pair index and `arm_order_index` is recorded in every trial, so the
  analysis can ask whether order mattered. Alternating does not remove the
  effect; it stops it pointing the same way every time.

### Capsule provenance

`capsule_probe.py` replays each scenario's own turn-script activity through the
real `velra hook` binary, runs the real `velra restore`, and checks the
declared `capsule_markers` against what the **production renderer** produced.
It never composes capsule text. Measured: A carries 3/3 declared markers in
1 406 chars, B carries 4/4 in 1 453 chars. The check blocks the readiness gate,
because a scenario whose markers the renderer cannot produce would turn every
Velra-arm trial into `RECEIPT_FAILURE` after the money was spent.

`live_trial.py` contains no capsule text at all and reads only what `velra
restore` staged; a test fails the build if that ever changes.

---

## 13. Known measurement limitations

Recorded now, before any live run, so they cannot be discovered conveniently
later.

1. **The cache fields may not exist.** Nothing here asserts that Claude Code's
   `result` events carry `cache_read_input_tokens` or
   `cache_creation_input_tokens`. If they do not, `total_input_tokens` is not
   computable, every pair reports `INCONCLUSIVE(TELEMETRY_UNAVAILABLE)`, and
   the benchmark's primary question goes unanswered until a source of
   structured usage telemetry exists. That is a real possible outcome of Phase
   4 and the design does not route around it.

2. **Context size is probably not observable.** Claude Code emits no structured
   context-size field that this repository knows of, so
   `actual_observed_context_size` will most likely be `unavailable` on every
   trial. The ladder's rungs will then be *targets applied*, with the achieved
   size unmeasured, and `achieved_context_size` will say so rather than
   borrowing the fixture's number.

3. **`synthetic_context_size` is an estimate at 3.6 chars/token.** It describes
   a file, not a context window, and it is a `proxy` wherever it is reported.

4. **`capsule_tokens` is Velra's own render-budget estimate** unless a
   tokenizer measurement artifact exists for that trial, in which case it is
   `measured`. The estimate is reported as a `proxy` with a `~` prefix.

5. **The ladder's upper rungs may be unreachable.** If a session cannot be
   driven to 900K, `reason_if_target_not_reached` records why and the rung is
   reported as not achieved. A synthetic 900K fixture is never reported as a
   900K Claude observation.

6. **The prompt cache is shared and server-side.** Arm order is balanced and
   recorded, not controlled. If the two arms of a pair differ mainly in cache
   warmth rather than in what they had to send, `cache_read_input_tokens` is
   where that will show, and it is reported separately from uncached input for
   exactly this reason.

7. **Effort metrics are not normalised across models.** Turns and tool calls
   are comparable within a pair, which is where they are used, and are not
   averaged across benchmarks.

8. **`first_correct_action` is a declared rule, not a judgement.** It is a tool
   set and a target-substring set fixed before the run. A session that reaches
   the right outcome by a route the rule does not describe scores `not
   reached` on A3 while still scoring correct on A4. That is a known
   conservatism and it runs against Velra.

9. **Duplicate-event deduplication keys on the strongest identity available.**
   Where an event carries no `uuid`, no message id and no request id, the key
   is a digest of the event, so two byte-identical adjacent events collapse.
   Claude Code's stream is not known to emit those; if it does, the count is
   understated and `duplicates_removed` is the field to look at.

10. **No live trial has been run.** Nothing in this document, in
   `readiness.json`, or in any artifact under `bench/results/v0.1.2/tokenburn/`
   is a measurement of Claude Code's behaviour or of Velra's effect on it.

---

## 14. Legacy benchmark status

`bench/legacy/` holds S1, S2, S3, the v0.1 fixture generator and the three
archived runners. Every historical result tree is untouched:
`bench/results/trials`, `bench/results/v0.1.1`,
`bench/results/v0.1.1_frozen_baseline`, `bench/results/superseded`,
`bench/results/v0.1.2/{trials,stages,controls}` and `BENCHMARK_REPORT.md`. The
one rename is `bench/results/v0.1.2/readiness.json` →
`readiness_hardened.json`, because the Phase 3 specification names that path
for the Token-Burn readiness report; the file was renamed rather than
overwritten and the archived runner writes the new name.

The archived scenarios remain runnable as regression tests —
`python -m pytest bench/tests -q` and `python bench/harness/selftest.py` both
still pass against them — and they contribute nothing to this scorecard. The
Token-Burn pipeline reads only `bench/results/v0.1.2/tokenburn/trials`, and
`test_the_scorecard_does_not_read_any_legacy_artifact` fails if any module in
`bench/tokenburn` so much as names a legacy artifact path.

They stopped being the primary benchmark because they answer a different
question: S1/S2/S3 are within-session experiments about compaction, and nothing
in them crosses a session boundary, which is the whole of what v0.1.2 ships.
