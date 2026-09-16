# Velra benchmarks

Two benchmark systems live here. They measure different things and they do not
share results.

| | What it answers | Runner | Results |
|---|---|---|---|
| **v0.1** | Is the capsule delivered, bounded and deterministic? Do the hooks stay out of the way? | `bench/run_full_benchmark.py` | `bench/results/` |
| **v0.1.1 efficacy** | Does the capsule change what the agent *does* after compaction? | `bench/run_efficacy_benchmark.py` | `bench/results/v0.1.1/` |

The v0.1 report ([`BENCHMARK_REPORT.md`](../BENCHMARK_REPORT.md)) settled the
first question and could not settle the second: its control measured Claude
Code's compaction as *not lossy* on its task, so both of its behavioural
hypotheses came out INCONCLUSIVE. The v0.1.1 system exists to answer the second
question properly, and its design is a direct response to why the first one
could not.

---

## Running the efficacy benchmark

```bash
python bench/run_efficacy_benchmark.py
```

Run it from an ordinary terminal, **not** from inside a Claude Code session:
the trials drive their own Claude Code processes and mutate the same
user-level `settings.json` a nested session is reading.

Useful flags:

```bash
# Everything free: fixtures, ground truth, leak scan, cargo test, provenance.
python bench/harness/regression_gate.py --binary target/release/velra.exe

# The whole analysis pipeline on synthetic captures. ~15 s, no API calls.
python bench/harness/selftest.py

# The harness's own unit tests, including checks against the recorded v0.1 captures.
python -m pytest bench/tests -q

# A cheaper live pass: one scenario, fewer replicates. Reports INCONCLUSIVE
# for the behavioural hypothesis below four replicates, and says so up front.
python bench/run_efficacy_benchmark.py --scenarios s1-dead-end-pair --replicates 2
```

### Cost

Each replicate is two sessions of fourteen to seventeen turns. On Sonnet,
budget roughly **$2.50–$3.50 per replicate per scenario**, plus about $3 per
control replicate and about $0.50 per trial for the tokenizer measurements. The
registered default — four replicates, three scenarios, two control replicates
each — is on the order of **$40–60**. `--max-budget-usd` caps each individual
session.

### Prerequisites

* Python 3.11+, `pytest`, Git, a Rust toolchain.
* An authenticated Claude Code install. The harness finds it itself
  (`bench/harness/claude_binary.py`); `$VELRA_BENCH_CLAUDE` pins a specific
  build.
* On a host whose default rustup toolchain cannot link (this one defaults to
  MSVC with no MSVC linker installed), the gate auto-selects the toolchain
  matching the release binary's target triple.
  `$VELRA_BENCH_CARGO_TOOLCHAIN` overrides.

---

## Why the design looks like this

Every structural decision below is a response to something that went wrong in
the v0.1 run, recorded so nobody re-litigates it.

**Success criteria are pre-registered.**
[`harness/preregistration.json`](harness/preregistration.json) names the
hypotheses, the primary metric for each, the success and failure conditions,
the valid-trial criteria and the replicate minimum. It is hashed into every
artifact, and `scenario_verdict.py` refuses to evaluate an aggregate produced
under a different hash. Changing it after a run invalidates the run rather than
reinterpreting it.

**Four replicates per arm is a floor, not a preference.** With a 2×N table and
a perfect split, the one-sided Fisher exact p is `1/C(2n, n)`: 0.050 at n=3,
0.014 at n=4. Below four, a clean sweep cannot clear p < 0.05 however
convincing it looks, and the verdict script reports INCONCLUSIVE rather than
rounding it up.

**The control is per scenario, replicated, and runs the scenario's own turns.**
The v0.1 probe ran one hand-written script, said "lossy" on 2026-09-13 and "not
lossy" on 2026-09-14, and that reversal decided two hypotheses.
`harness/loss_probe.py` replays the scenario's actual turns up to the boundary
and takes a majority of valid runs.

**Each scenario is unanswerable from the repository.** The v0.1 fixture's
defect was recoverable from the failing assertion, so both arms scored 4/4 and
nothing separated them. Worse, its failing test carried the docstring *"The
discount is a property of the invoice, not of each line"* — the fix in one
sentence — and a recorded replicate quotes it back as its reasoning.
`scenarios/base.lint_tree` fails the build if a generated fixture contains a
phrase naming its own fix, and the gate runs it before anything is paid for.
The same leak is fixed in the v0.1 generator, so
`bench/run_full_benchmark.py` no longer reproduces it either.

**Ground truth is verified, not asserted.** `Scenario.build` runs the real
pytest suite against the fixture as generated, against every declared dead end,
and against every declared fix. A dead end that quietly becomes a fix fails the
build.

**Both sides of the size comparison use the same instrument.** The v0.1 report
compares a real-tokenizer capsule against a `chars/4` estimate of the native
summary. `harness/native_tokens.py` measures the summary the same way
`measure_tokens.py` measures the capsule.

**Provenance is taken from git, not from the binary.** `build.rs` declares
`rerun-if-changed` on `.git/HEAD`, which does not change when you commit on a
branch, so the embedded sha goes stale — the v0.1 four-replicate run is
attributed to `e9f40151c` while the tree was several commits further on.
`harness/provenance.py` records git's answer and flags the mismatch;
`build.rs` now also watches the ref `HEAD` points at.

---

## The scenarios

Each targets one capsule section, and is built so the repository cannot answer
it.

| Scenario | Mechanism | The question only memory answers |
|---|---|---|
| `s1-dead-end-pair` | `[DEAD_ENDS]` | Two hypotheses were tried and reverted through git before compaction. Does the agent go back to burned ground? |
| `s2-hidden-constraint` | `[ROOT_TASK_OBJECTIVE]` | Two fixes both make the suite green. A constraint stated once, in turn 0, decides which is right. |
| `s3-working-set` | `[WORKING_FILES]` | "Now make that fix" — after 84 unrelated modules have been read. Nothing in the repo says which file. |

`s3` is expected to be **hard for Velra as it stands**: §18 of the v0.1 report
records `[WORKING_FILES]` absent from 4 of 4 delivered capsules, because the
ladder steps `working_max` from four straight to zero. That expectation is
written into the pre-registration so a failure is reported as a measurement,
not a surprise, and
`capsule.rs::the_working_files_ladder_still_steps_from_four_to_zero` pins the
cliff so the planned `working_max = 2` rung has to flip it deliberately.

```bash
python bench/scenarios/registry.py --list
python bench/scenarios/registry.py s1-dead-end-pair /tmp/fx   # build and verify one
```

---

## Layout

```
bench/
  run_efficacy_benchmark.py     the v0.1.1 runner
  run_full_benchmark.py         the v0.1 runner, unchanged
  scenarios/
    base.py                     shared ledger sources, ground truth, leak lint
    s1_dead_end_pair.py         [DEAD_ENDS]
    s2_hidden_constraint.py     [ROOT_TASK_OBJECTIVE]
    s3_working_set.py           [WORKING_FILES]
    registry.py                 lookup + a CLI for building one fixture
  harness/
    preregistration.json        hypotheses and criteria, fixed before the run
    prereg.py                   loads it and pins it to a hash
    regression_gate.py          RC1-RC4: everything free that must pass first
    provenance.py               git truth, cross-checked against the binary
    scenario_trial.py           drives one session, one arm
    loss_probe.py               the per-scenario information-loss control
    behaviour.py                the metrics; arm-blind by construction
    validity.py                 valid-replicate rules
    scenario_analyze.py         one trial -> analysis.json
    native_tokens.py            the native summary, same tokenizer as the capsule
    scenario_aggregate.py       pooling, grouped by scenario and arm
    stats.py                    Fisher's exact, and what n can support
    scenario_verdict.py         the verdicts, read out of the pre-registration
    selftest.py                 the whole pipeline, offline, on scripted outcomes
    measure_tokens.py           shared with v0.1
    hook_overhead.py            shared with v0.1
    verify_env.py               shared with v0.1
    claude_binary.py            shared with v0.1
  tests/
    test_bench_units.py         the harness's own tests
```

---

## Reading the results

Do not trust `verdicts.json` on its own. Every number in it is recomputable
from the raw captures:

```bash
python bench/harness/scenario_analyze.py --trial bench/results/v0.1.1/trials/<trial>
```

`analysis.json` is a pure function of `stream.jsonl`, `transcript.jsonl`,
`final_state.json` and `velra.db`. The delivered capsule is written out
verbatim as `delivered_capsule.txt` and the native summary as
`native_summary.txt`, so both can be read rather than described.
