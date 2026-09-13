# Tokenizer calibration fixtures

Two `<VELRA_CONTINUATION>` blocks that Claude Code actually received during the
v0.1 benchmark, byte for byte as they were injected, together with the token
count each one really cost.

`measured.json` holds those counts. They were **measured, not estimated**: for
each block, two otherwise identical minimal sessions were run with all tools
disabled and the difference in billed input tokens recorded (see
`bench/harness/measure_tokens.py`). The method was validated first — repeating
one prompt gave a delta of 0, and two copies of a block cost exactly twice one
copy — so the numbers are the tokenizer's own, not a model of it.

`estimate_tokens` in `crates/velra-core/src/text.rs` is calibrated against
these, and `crates/velra/tests/capsule.rs::estimator_is_above_real_tokenizer_counts`
asserts it stays at or above them. That test is the guard on the H3 defect: the
original estimator read these two blocks as 643 and 743 tokens against a
declared 800-token budget, so the truncation ladder never ran and Velra shipped
951 and 1,147.
