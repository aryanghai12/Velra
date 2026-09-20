# Tokenizer calibration fixtures

Ten continuation blocks that Claude Code actually received during a benchmark
run, byte for byte as they were injected, together with the token count each one
really cost.

| fixture | format | capsules |
|---|---|---|
| `capsule-r*.txt` | v0.1 `<VELRA_CONTINUATION>` | 2 |
| `v0.1.1-*.txt` | v0.1.1 `<VELRA_WORKSPACE_STATE>` | 8 |

`measured.json` holds those counts and names the run each block came from. They
were **measured, not estimated**: for each block, two otherwise identical
minimal sessions were run with all tools disabled and the difference in billed
input tokens recorded (see `bench/harness/measure_tokens.py`). The method was
validated first — repeating one prompt gave a delta of 0, and two copies of a
block cost exactly twice one copy — so the numbers are the tokenizer's own, not
a model of it. Every `v0.1.1-*` measurement carries a control delta of 0.

`estimate_tokens` in `crates/velra-core/src/text.rs` is calibrated against all
ten, and
`crates/velra/tests/capsule.rs::estimator_is_above_real_tokenizer_counts`
asserts it stays at or above every one of them.

## Why there are two formats, and why that matters

The two v0.1 blocks were the whole calibration set until v0.1.2, and they were
misleading. They are path-dense, so the walk over-reads them by 3–6%, and the
estimator's docstring claimed on that basis that it cleared "the worst observed
under-read thirty times over".

The v0.1.1 capsule opens with a ~340-character prose paragraph. Against the
eight blocks below, the old walk read *under* the real cost every single time,
by as much as 7.9% of its own reading — which is how a capsule rendered to a
745-token target came to cost 804 real tokens and fail hypothesis E4 of the
v0.1.1 efficacy benchmark (`bench/results/v0.1.1/verdicts.json`).

Both formats are kept because that is the lesson: an estimator calibrated on one
capsule shape says nothing about the next one. Any future format change adds its
own measured blocks here before the budget is trusted again.

## Measured error, as of v0.1.2

`cargo run -p velra --example token_calibration -- tests/fixtures/tokenizer`

The walk now reads at or above the real count on all ten. Its tightest margin is
1.7% (790 estimated against 777 real) and its widest over-read is 13.6%. The
over-read is content the capsule declines to carry; that is the side a budget
has to be wrong on.
