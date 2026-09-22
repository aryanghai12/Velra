# Legacy benchmarks — archived, preserved, not scored

Everything in this directory is **historical evidence**. It was the primary
efficacy benchmark up to and including the v0.1.2 hardened evaluation, and it
is kept working so its results stay reproducible and so its scenarios can go on
serving as regression tests.

**None of it contributes to the v0.1.2 Token-Burn efficacy scorecard.** That
scorecard is produced by `bench/tokenburn/` and by nothing else. The two systems
share no results file, no verdict logic and no aggregate.

## What is here

```
bench/legacy/
  scenarios/                  S1, S2, S3 and everything they are built from
    base.py                   shared ledger fixture, ground truth, leak lint
    s1_dead_end_pair.py       [DEAD_ENDS]
    s2_hidden_constraint.py   [ROOT_TASK_OBJECTIVE]
    s3_working_set.py         [WORKING_FILES]
    targets.py  leaks.py  registry.py
  fixture/                    the v0.1 fixture generator and its noise modules
  run_full_benchmark.py       the v0.1 runner        -> bench/results/
  run_efficacy_benchmark.py   the v0.1.1 runner      -> bench/results/v0.1.1/
  run_hardened_eval.py        the v0.1.2 hardened    -> bench/results/v0.1.2/
  resume_benchmark.py         partial-run resumption for the v0.1.1 runner
  run.sh, run_full_benchmark.{sh,ps1}
```

The harness these runners drive (`bench/harness/`) was **not** moved. It holds
modules the Token-Burn benchmark also uses — `claude_binary.py`,
`provenance.py`, `measure_tokens.py`, `verify_env.py` — and
`bench/harness/selftest.py` is still the legacy pipeline's own offline check,
named as such in the release checklist. `bench/README.md` says which of those
modules are legacy-only.

## What was preserved, and where

| Artifact | Location | Status |
|---|---|---|
| S1/S2/S3 scenario source | `bench/legacy/scenarios/` | moved, unchanged |
| v0.1 fixture generator | `bench/legacy/fixture/` | moved, unchanged |
| v0.1 raw trial artifacts | `bench/results/trials/` | untouched |
| v0.1 report | `BENCHMARK_REPORT.md` | untouched |
| v0.1.1 results | `bench/results/v0.1.1/` | untouched |
| v0.1.1 frozen baseline | `bench/results/v0.1.1_frozen_baseline/` | untouched |
| superseded trials | `bench/results/superseded/` | untouched |
| v0.1.2 hardened results | `bench/results/v0.1.2/{trials,stages,controls,aggregate.json,verdicts_v2.json}` | untouched |
| v0.1.2 hardened readiness | `bench/results/v0.1.2/readiness_hardened.json` | renamed from `readiness.json` |
| pre-registrations | `bench/harness/preregistration{,_v2}.json` | byte-for-byte unchanged |

The one rename is the only change to any recorded result. `readiness.json`
under `bench/results/v0.1.2/` now belongs to the Token-Burn benchmark, because
the Phase 3 specification names that exact path; the hardened runner writes
`readiness_hardened.json` instead, and the file that was there was renamed
rather than overwritten.

## Running the archived benchmarks

Paths changed, behaviour did not:

```bash
python bench/legacy/scenarios/registry.py --list
python bench/legacy/run_hardened_eval.py --dry-run
python bench/legacy/run_efficacy_benchmark.py --scenarios s1-dead-end-pair --replicates 2
```

The imports still read `from scenarios import ...`. `bench/legacy` is placed on
`sys.path` by the modules that need it, so the move did not touch a single
import statement and the fixture seeds — which hash `base.py`, the scenario
module and `noise.py` — are unchanged. `python -m pytest bench/tests -q` and
`python bench/harness/selftest.py` both still pass against the archived
scenarios, which is what proves the move was lossless.

## Why they stopped being the primary benchmark

They answer a different question. S1/S2/S3 are *within-session* experiments: a
conversation compacts, and the question is whether the capsule preserved
something the compaction summary dropped. v0.1.2 ships cross-session restore,
and the workflow it exists for is not compaction at all — it is a developer
abandoning an expensive conversation on purpose, starting a brand-new session
and carrying a small bounded capsule across. Nothing in S1/S2/S3 crosses a
session boundary, so none of them can measure it.

They also cannot measure input burden. Their primary metric is behavioural
correctness after compaction; token accounting appears only as the capsule's
own size against the native summary's. The Token-Burn benchmark's first
question is what the continuation *cost*, which needs structured usage
telemetry those scenarios never collected.
