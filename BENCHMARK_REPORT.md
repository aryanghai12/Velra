# Velra v0.1 — empirical benchmark report

> **Superseded historical benchmark — not part of the current release evidence.**
> This is the v0.1 `/compact` benchmark (September 2026, Velra v0.1). Its
> verdicts describe that version and that experiment only. The current
> benchmark is the v0.1.2 requalification: [docs/BENCHMARK.md](docs/BENCHMARK.md),
> evidence in [bench/results/v0.1.2-requal/](bench/results/v0.1.2-requal/).
> This file is kept unchanged below this note for provenance.

**Date:** 2026-09-14 · **Host:** Windows 11 Home Single Language (10.0.26200), x86_64, `x86_64-pc-windows-gnu`
**Binary under test:** `velra 0.1.0 (e9f40151c, x86_64-pc-windows-gnu)`, release, 5,385,728 bytes
**Agent under test:** Claude Code **2.1.270** (native binary shipped with the VS Code extension), model `claude-sonnet-5`
**Dataset:** **4 replicates per arm — 8 live sessions**, 2,627 s total wall time, **$8.97** in API cost
**Everything below is reproducible with:** `python bench/run_full_benchmark.py --replicates 4`

---

## 1. Verdicts

Measured across **four replicates per arm** on 2026-09-14. Medians are over
the four runs; every per-run figure is in §17.

| # | Hypothesis | Verdict | The number that decides it |
|---|---|---|---|
| **H1** | Compaction amnesia elimination | **INCONCLUSIVE** | the control says compaction was **not lossy** in this harness, so the post-compaction turn never tested recall. Target met in absolute terms — re-reads **0/4 in both arms**, first edit on `engine.settle` **4/4 in both arms**, suite green **4/4 in both arms** |
| **H2** | Dead-end loop prevention | **INCONCLUSIVE** | `[DEAD_ENDS]` reached the delivered capsule **4/4**, re-exploration **0/4** — but the baseline also re-explored **0/4**, and with nothing forgotten there was nothing for either arm to re-explore |
| **H3** | Continuation budget ≤ 800 tokens | **PASSED** | **705, 715, 721, 722 real tokens** on Anthropic's tokenizer — 4/4 under the 800 ceiling at the 745 calibrated budget, measurement control delta **0** |
| **H4** | Zero-overhead fail-open guarantee | **FAILED** | fail-open half perfect — **481 in-session hook invocations, 0 non-zero exits, 0 stderr bytes**. Latency half fails: worst marginal p99 **163.31 ms** against a 15 ms allowance |

Phase 1 environment checks: **PASSED**.

> **Reading this document.** §1–§14 were written against `75ed4bbd3` and are
> kept as originally measured. §15 records the fixes, §16 what a re-run would
> settle, and §17 now carries the **authoritative four-replicate dataset for
> `e9f40151c`**, which supersedes the §1 table for anything about the current
> binary. §18 records the two open findings, re-measured across four runs.

### The one-paragraph finding

The headline of this run is a **negative control result that invalidates two of
the four hypotheses as posed**. Velra's capsule did everything it claims to do:
it was delivered in 4/4 replicates, carried a provenance-tagged `[DEAD_ENDS]`
section in 4/4, and measured **705–722 tokens** against its 800 ceiling with a
valid zero-delta control. But the dedicated compaction probe — vanilla Claude
Code, Velra disabled — found that `/compact` **was not lossy on this task**: it
cut live context by only 22.1% (54,999 → 42,856 tokens) and the canary value
`AQE1`, planted nine turns earlier, was still recalled verbatim *without a tool
call*. If nothing was forgotten, then a capsule that prevents forgetting cannot
be shown to help, and H1 and H2 are **INCONCLUSIVE rather than passed**: both
arms re-read zero source files, both put the first edit on `engine.settle` 4/4,
both finished green 4/4. The one behavioural difference that did survive four
replicates is narrow but perfectly consistent: the Velra arm closed the task in
**exactly 2 tool calls in all four runs**, against a baseline that took 3, 2, 4
and 3. Velra's genuine, measured advantage remains what it was — a bounded,
deterministic, provenance-tagged record, **705–722 tokens across four runs**,
against a native summary that ranged from **777 to 4,977 tokens** across the
same eight sessions. H4's fail-open guarantee held without a single exception
across 481 observed hook invocations; its latency allowance did not, though the
figure that fails it is a tail artifact rather than a systematic cost (§17).

---

## 2. What was measured, and how

Two arms, identical in every respect except one:

| | Baseline arm | Velra arm |
|---|---|---|
| Repository | identical generated fixture | identical generated fixture |
| Turn script | identical 16 turns, byte for byte | identical 16 turns, byte for byte |
| Model / flags | `sonnet`, `--strict-mcp-config`, `bypassPermissions` | same |
| Compaction boundary | `/compact` at turn 14 | `/compact` at turn 14 |
| Measured turn | turn 15, `Fix the remaining test failure.` | turn 15, same |
| **Difference** | `velra disable` | `velra enable` |

Sessions are driven non-interactively through
`claude -p --input-format stream-json --output-format stream-json`, so every
user turn is a fixed string sent in a fixed order and the agent's own behaviour
is the only free variable. `--include-hook-events` puts every hook
`hook_started` / `hook_response` pair on the same stream, which is how hook
exit codes, stdout and stderr are observed from outside the process.

Both arms *write* to the settings file — the baseline explicitly runs
`velra disable` rather than assuming the hooks are absent — so the state of
that file is never a confound.

Raw captures are written verbatim to `bench/results/trials/<trial>/stream.jsonl`
and analysed afterwards by `bench/harness/analyze.py`, which is a pure function
of the bytes on disk. Verbatim excerpts backing every number in this report are
in **[`bench/results/EVIDENCE.md`](bench/results/EVIDENCE.md)** (43 KiB).

### The eight trials

Four replicates per arm, run 2026-09-14 against `velra 0.1.0 (e9f40151c)` under
Claude Code 2.1.270. "Native summary" is the size of Claude Code's own
compaction summary; "Velra capsule" is the delivered continuation block as
measured by Anthropic's tokenizer.

| Trial | Arm | Session id | Wall | Cost | Native summary | Velra capsule |
|---|---|---|---:|---:|---:|---:|
| `saturated-baseline-r1` | baseline | `add39c56-e8bd-4a84-a3ed-4e2a4ed29c28` | 270 s | $1.05 | ~3,311 tok | — |
| `saturated-baseline-r2` | baseline | `7c39008a` | 213 s | $1.30 | **none produced** | — |
| `saturated-baseline-r3` | baseline | `d5fb8f07` | 312 s | $1.09 | ~4,977 tok | — |
| `saturated-baseline-r4` | baseline | `fb7dccad` | 247 s | $1.51 | ~3,794 tok | — |
| `saturated-velra-r1` | velra | `e4aef295` | 243 s | $0.94 | ~843 tok | **715 tok** |
| `saturated-velra-r2` | velra | `dc028e50` | 229 s | $1.07 | ~777 tok | **722 tok** |
| `saturated-velra-r3` | velra | `c5e342a2` | 266 s | $0.98 | ~838 tok | **705 tok** |
| `saturated-velra-r4` | velra | `02610e47` | 305 s | $1.02 | ~4,210 tok | **721 tok** |

Medians — baseline wall 258.7 s, cost $1.19; Velra wall 254.6 s, cost $1.00.
Total API cost for the suite: **$8.97**.

> **One trial is not a valid replicate for the compaction-dependent
> hypotheses.** `saturated-baseline-r2` reached turn 15 without compacting:
> `analysis.json` records `compact_status: null` and
> `native_compaction_summary.present: false`. Compaction succeeded in **3/4
> baseline** and **4/4 Velra** trials, so H1's cross-arm comparison rests on
> three baseline replicates, not four. It is counted as a trial everywhere the
> measure does not depend on compaction having happened (tool calls, first-edit
> accuracy, suite green), and excluded from the native-summary range.


---

## 3. Phase 1 — environment and binary verification

`bench/harness/verify_env.py`, evidence in `bench/results/phase1_environment.json`.

*Refreshed by the 2026-09-14 four-replicate run; the binary is `e9f40151c`.*

```
velra --version -> velra 0.1.0 (e9f40151c, x86_64-pc-windows-gnu) (exit 0)
real settings: enable -> 13 handlers across 10 events
real settings: disable -> restored byte for byte = True
jsonc settings: foreign hook kept = True, comments kept = True, restored byte for byte = True
```

`velra enable` registered **13 handlers across 10 hook events**
(`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
`PostToolUseFailure`, `PostToolBatch`, `Stop`, `PreCompact`, `PostCompact`,
`SessionEnd`). `status` and `doctor` both exit 0 when enabled and 1 when not,
which is the documented contract.

**Non-destructiveness is proven twice.** Against the user's real
`~/.claude/settings.json`, the SHA-256 before `enable` and after `disable` is
identical:

```
before  cce7710585fa8aa8c6b31e8c3736f526ca421f287435550bee5a4fefffff1f5e  (146 bytes)
enabled 5e9ff30dcbdf768b119eab056e71f85a6a3cc83ba0f265bc3202e0e0e7d84232  (4546 bytes)
after   cce7710585fa8aa8c6b31e8c3736f526ca421f287435550bee5a4fefffff1f5e  (146 bytes)
```

`enable` grew the file from 146 bytes to 4,546 and `disable` returned it to the
same 146 bytes with the same digest (`restored_byte_for_byte: true`).

That alone is weak evidence — a file with no comments would survive a naive
JSON reparse. So the check is repeated against a synthetic JSONC file carrying
a line comment, a block comment, an unusual key order, and **a foreign
`PostToolUse` hook belonging to somebody else**. After `enable` the foreign
hook is still present and both comments survive; after `disable` the file is
byte-for-byte identical again. A timestamped backup is written to
`~/.velra/backups/` on each mutation.

---

## 4. Phase 2 — the fixture

`bench/fixture/make_fixture.py` builds a deterministic Git repository: 88 Python
files, 7,282 lines, 253 KB — roughly 63,000 tokens if read in full.

Three interdependent modules carry the defect (`money` → `rules` → `engine`),
surrounded by a plausible billing platform (84 generated modules across
`adapters`, `reporting`, `importers`, `validation`) whose only purpose is to
make a session that works across the codebase accumulate real context.

**The defect.** `engine.settle()` takes the 7.4% promotional discount *per line
item* and sums the rounded results, instead of discounting the invoice subtotal
once. The three line amounts ($126.00, $476.00, $826.00) each produce a product
whose fractional part is exactly 0.4, so every line rounds down and the three
together under-discount by exactly one cent. `test_exact_payment_settles_invoice`
asserts `SETTLED` and gets `UNDERPAID`, at `tests/test_engine.py:54`.

**The dead end is deliberately the intuitive hypothesis.** A one-cent gap smells
like a rounding-mode problem in `Money.scaled`. It is not: because no value in
the calculation lands on a tie, switching to `ROUND_HALF_EVEN` changes none of
the three discounts, leaves the failure exactly as it was, *and* additionally
breaks `test_discount_rounds_half_up`, whose 18.5-cent case is a genuine tie.
Verified as ground truth before any trial ran:

```
dead end   (ROUND_HALF_EVEN in money.scaled) -> 2 failed, 4 passed   (strictly worse)
real fix   (discount the subtotal once)      -> 6 passed
```

Git history carries a committed-and-reverted attempt at that same rounding
change, and the working tree carries an uncommitted, unrelated change to
`rules.py` (a late-payment surcharge nothing calls yet).

> **A flaw found and fixed mid-benchmark.** The first version of this fixture
> put a `discount_for_total()` helper in that uncommitted change whose `TODO`
> said, in as many words, that the engine should call it instead of summing per
> line item. That is the answer written down in the working tree: both arms
> found it with a single `git diff` and were finished in one step, measuring
> nothing but how fast an agent reads a TODO. Four trials were run before this
> was caught; they are kept in `bench/results/superseded/` and none of their
> numbers appear in this report. The results below come entirely from the
> corrected fixture, in which the answer appears nowhere in the tree.

---

## 5. Phase 3 — the protocol

Sixteen turns. Turns 0–5 find the failure, read the three modules, try the
rounding hypothesis, discard it with `git restore`, read the tests, and check
the history. Turns 6–13 are a real, unrelated audit across the 84 surrounding
modules. Turn 14 is `/compact`. Turn 15 is the measured turn.

Two design decisions matter:

1. **Nothing before the compaction boundary ever asks the agent to diagnose the
   bug.** An earlier protocol ended with "summarise in two sentences where we
   stand", which made the agent write out the full diagnosis immediately before
   `/compact` and handed the summariser a ready-made answer. That turn was
   removed.
2. **The saturation turns are real work, not padding.** By the time `/compact`
   arrives the failing test is eleven turns old and the summariser has an audit
   to describe as well.

---

## 6. The control — is there any amnesia to eliminate?

> **⚠️ Superseded by [§17](#17-the-four-replicate-run-2026-09-14--verdicts-as-re-measured).**
> This section measured compaction as *lossy* (41.3% reduction, canary lost).
> The 2026-09-14 re-run measured the opposite on the same probe — 22.1%
> reduction and the canary recalled verbatim — which is what moves H1 and H2
> to INCONCLUSIVE. The reversal is the single most important change in this
> document; read §17's control table before citing anything below.

H1 and H2 are only meaningful if Claude Code's own compaction actually loses
something. `bench/harness/compaction_probe.py` measures that directly, with
Velra disabled, by sending a no-tool turn (`Reply with exactly: OK`) either side
of `/compact` — a single API iteration, so the billed input token count *is* the
context — and then asking for one incidental detail.

```
context before /compact:  75,459 tokens
context after  /compact:  44,272 tokens   (41.3% smaller)
canary: API_KEY_PREFIX in src/ledger/adapters/adyen_uk.py  (= "AQE1")
  answer: 'UNKNOWN'   recalled: False   used a tool: False
-> compaction is lossy: True
```

**Compaction is genuinely lossy.** The agent read `adyen_uk.py` in turn 0, the
constant sat in its context for nine turns, and after `/compact` it was gone.

Two measurement traps were found and fixed here, and both are worth recording:

- **The compaction request's token usage is billed to the turn *after* it.**
  The first post-compaction probe reported 321,645 tokens — larger than any
  pre-compaction turn — which briefly suggested compaction had *grown* the
  context. The probe now sends two identical no-tool turns after `/compact` and
  uses the second.
- **The first canary was the wrong canary.** It asked for `mollie_apac.py`'s
  `DECLINE_STATUS`, which had been the explicit subject of turn 0 and which the
  agent recalled perfectly — exactly the sort of thing a good summary keeps on
  purpose. Recalling it proved nothing. The canary is now a constant that was
  never asked about and never written into any reply.

---

## 7. H1 — compaction amnesia elimination

> *Target: post-compaction file re-reads drop to 0, and the agent targets the
> failing symbol immediately.*

| Metric, measured turn only | Baseline (n=2) | Velra (n=2) |
|---|---:|---:|
| Source files re-read before the first edit | **0** (0, 0) | **0** (0, 0) |
| Total tool calls | 2.0 | 2.0 |
| First edit hit `src/ledger/engine.py` | 2/2 | 2/2 |
| First edit landed inside `engine.settle` | **2/2** | **2/2** |
| Test suite green at the end | 2/2 | 2/2 |

Every trial in both arms produced the same two-call turn: one `Edit` to
`engine.settle`, one `Bash` running pytest. Verbatim, from
`saturated-velra-r2`:

```json
{"type": "tool_use", "id": "toolu_01HSZVUEdR1Kz6dqZPx8bmGR", "name": "Edit", "input":
 {"replace_all": false, "file_path": "...\\fixtures\\s2-velra-r2\\src\\ledger\\engine.py",
  "old_string": "    subtotal = invoice.subtotal()\n\n    discount = ZERO\n    for item in invoice.items:\n        discount = discount + rules.discount_for(item.amount)\n\n    total_due = subtotal - discount + rules.fee_for(subtotal)",
  "new_string": "    subtotal = invoice.subtotal()\n\n    discount = rules.discount_for(subtotal)\n\n    total_due = subtotal - discount + rules.fee_for(subtotal)"},
 "caller": {"type": "direct"}}
```

(`file_path` is abbreviated at the front only; everything else is byte for byte.)

**Verdict: PASSED, with an important qualification.** Velra met the target
exactly. It did not beat the baseline, which met it too.

**Why the baseline succeeds.** Claude Code's native compaction summary is not a
lossy prose paragraph — it is a structured hand-off that explicitly enumerates
intent, files, and work already attempted. In `saturated-baseline-r1` it ran to
17,640 characters and mentioned the failing test 4 times, `ROUND_HALF_EVEN`
7 times, and `git restore` 5 times. The information Velra's capsule is designed
to rescue was never lost, so restoring it changed nothing.

---

## 8. H2 — dead-end loop prevention

> *Target: approaches attempted and reverted via Git are recorded under
> `[RECENT_ATTEMPTS]`, giving 0% re-exploration of discarded paths.*

| | Baseline | Velra |
|---|---:|---:|
| Re-explored the reverted `ROUND_HALF_EVEN` change | **0/2** | **0/2** |
| Dead end present in Velra's *database* | n/a | 2/2 |
| Dead end present in the *delivered capsule* | n/a | **1/2** |

Neither arm ever re-proposed the rounding change, so the *outcome* half of the
target held. The *mechanism* half did not, and that is what decides this
hypothesis: in `saturated-velra-r1` the capsule the agent actually received
contained **no `[DEAD_ENDS]` section at all**. Its sections were:

```
CONTEXT, ROOT_TASK_OBJECTIVE, LATEST_REQUEST, STATUS,
ACTIVE_FAILURE, WORKING_FILES, NEXT_KNOWN_TARGET, RECOVERY
```

The dead end was in the database the whole time. It was filtered out on the way
to the page.

### Root cause, traced

`money.py` is byte-identical to `HEAD` in r1's final working tree — `git diff`
shows only `engine.py` and `rules.py` — so the revert unquestionably stuck. Yet
Velra recorded `dead_ends.reapplied = 1` and both `money.py` edits as
`REAPPLIED`. The `file_versions` table explains why:

| # | hash | size | source |
|---|---|---:|---|
| 1 | `34998199…` (original) | 1310 | `pre_edit` |
| 2 | `1279deac…` | 1327 | `post_edit` |
| 3 | `1279deac…` | 1327 | `pre_edit` |
| 4 | `384a2d44…` | 1334 | `post_edit` |
| **5** | **`34998199…` (original)** | **1310** | **`turn_scan`** |
| **6** | **`384a2d44…`** | **1334** | **`git_pre`** |
| 7 | `34998199…` (original) | 1310 | `git_post` |

Row 5 is the turn-end scan correctly seeing the file back at its original
content after `git restore`; that is what opened the dead end. Row 6 is the
`PreToolUse` snapshot **taken before the restore ran**, carrying the dead-end
content — but ingested *after* row 5. `revert::reapplied`
([revert.rs:95](crates/velra-core/src/revert.rs#L95)) treats any newer
observation whose hash matches one of the dead end's post-edit hashes as proof
the change came back. Row 6 matches edit 2's `post_hash` exactly, so the dead
end is flagged re-applied. `snapshot.rs`
([line 182](crates/velra-core/src/snapshot.rs#L182)) then selects dead ends
`WHERE ... reapplied = 0`, and the section vanishes.

The row is not newer in reality, only in ingestion order.

`saturated-velra-r2` avoided the bug by accident: that agent wrapped its command
as `cd "…" && git restore …`, which does not match the `Bash(git *)` condition on
the `PreToolUse` registration, so no `git_pre` row was ever written. Its capsule
kept the section — but with mechanism `external` and `command_text: null`,
rendered as *"changed outside the agent at 13:05"* rather than being attributed
to the `git restore` the agent itself had just run.

So across two trials, Velra's revert attribution produced two different wrong
answers about the same event: a false re-application that suppressed the section,
and a correct section that misattributed the cause. A bare `git restore` triggers
the first; a `cd … && git restore` triggers the second.

**Verdict: FAILED.** The hypothesis claims reverted approaches are recorded in
the continuation capsule. In half the trials they were not.

One wording note: the reverted approach belongs under `[DEAD_ENDS]`, not
`[RECENT_ATTEMPTS]` as the hypothesis states; `[RECENT_ATTEMPTS]` holds edits
that are still live. That is the renderer's documented arrangement and is not
counted as a failure here.

---

## 9. H3 — the continuation budget

> *Target: the injected continuation block measures strictly ≤ 800 tokens.*

The block Velra emits is tagged `<VELRA_CONTINUATION>` (the hypothesis calls it
`<CYCLE_CONTINUATION>`; same artifact). It is delivered as the entire stdout of
the `SessionStart:compact` hook — one JSON object, nothing else:

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<VELRA_CONTINUATION v=\"1\" checkpoint=\"ckpt_01M2BCMZ6NYNNQ5RAVXDCFF2JP\" ...
```

Token counts were measured with **Anthropic's own tokenizer**, not an estimate:
two otherwise identical minimal sessions are run with all tools disabled, and
the difference in billed input tokens is the block's cost.

| Trial | Characters | Velra's own estimate | **Measured** | Budget |
|---|---:|---:|---:|---|
| `saturated-velra-r1` | 2,056 | 643 | **951** | ✗ over |
| `saturated-velra-r2` | 2,377 | 743 | **1,147** | ✗ over |

The method was validated before the failure was accepted:

```
control    identical prompts twice           delta = 0        (measurement is stable)
linearity  2 x (1 capsule) = 1902 tokens
           1 x (2 capsules) = 1902 tokens    ratio = 1.000    (perfectly linear)
known text "hello " x 500 (3000 chars)       = 1002 tokens    (sane)
```

**Verdict: FAILED**, and the root cause is pinned.
[`crates/velra-core/src/text.rs:161`](crates/velra-core/src/text.rs#L161):

```rust
/// Estimated token count used for capsule budgets: `ceil(chars / 3.2)`.
pub fn estimate_tokens(s: &str) -> u32 {
```

The capsule's real density is **2.07-2.16 characters per token**, not 3.2. It is
dense on purpose — bracketed section tags, Windows and POSIX paths, a 26-character
ULID, and quoted code fragments all tokenize far more finely than prose. So the
estimator under-reads by **32–35%**, the truncation ladder in `render.rs` stops
trimming while it believes it is at ~650 of 800 tokens, and Velra ships a block
that is really ~950–1,150. The budget is not being enforced; a miscalibrated
constant is.

The fix is to calibrate `estimate_tokens` against the capsule's own format
(≈ 2.0 chars/token, rounding down so the estimate errs high), which will make
the existing truncation ladder enforce the
real budget without any other change.

---

## 10. H4 — zero-overhead fail-open guarantee

> *Target: p99 ≤ 15 ms on Windows, ≤ 3 ms on Linux; exits 0 without polluting
> stdout or terminal buffers.*

This hypothesis bundles two separate claims, and they do not get the same answer.

### (a) Fail-open and no pollution — **PASSED, without exception**

Across the two Velra trials, **288 hook invocations** were observed on Claude
Code's own event stream:

| | Count |
|---|---:|
| Invocations observed | 288 |
| Non-zero exits | **0** |
| Invocations writing to stderr | **0** |
| Invocations writing to stdout | **2** — one per trial, the continuation injection |

Every single hook exited 0 with an empty stderr. The only stdout ever produced
was the one continuation block per session, which is the documented contract,
not pollution. The direct benchmark below adds a further 4,050 invocations under
load (9 hook types x 225 invocations x 2 database sizes), every one of which
also exited 0 with empty stderr — the harness asserts
on both and would have aborted otherwise.

### (b) The latency allowance — **FAILED on this machine**

`bench/harness/hook_overhead.py` times the binary directly. Each hook is run 200
times after 25 warm-up runs, and a **spawn control** — the same binary with
`VELRA_DISABLE=1`, which exits immediately without touching the database —
isolates the cost of starting a process on this box from Velra's own work.

Against a 38.2 MiB database of 100,000 events (the load the project's own CI
uses):

| Hook | p50 | p99 | marginal p50 |
|---|---:|---:|---:|
| *spawn control* | *4.27 ms* | *24.95 ms* | — |
| `post-tool-use` Read (small) | 8.10 | 10.04 | +3.83 |
| `post-tool-use` Edit | 12.62 | **35.49** | +8.35 |
| `post-tool-use` Bash | 14.85 | **25.15** | +10.58 |
| `post-tool-use-failure` Bash | 14.12 | **26.18** | +9.85 |
| `pre-tool-use` Edit | 11.93 | **25.84** | +7.66 |
| `user-prompt-submit` | 8.06 | **16.55** | +3.80 |
| `session-start` | 7.63 | 8.72 | +3.37 |
| `stop` | 7.94 | 12.46 | +3.67 |
| `pre-compact` | 12.51 | **18.98** | +8.25 |

Worst-case p99 **35.5 ms** against a 15 ms allowance. The same run against a
0.5 MiB / 1,000-event database is much tighter — 7 of 9 hooks land inside the
allowance, and the spawn control's own p99 falls from 24.95 ms to 7.22 ms:

| Hook | p50 | p99 |
|---|---:|---:|
| *spawn control* | *3.87 ms* | *7.22 ms* |
| `post-tool-use` Read / Edit / failure | 7.71 / 8.25 / 8.89 | 9.78 / 10.02 / 10.17 |
| `pre-tool-use`, `user-prompt-submit`, `session-start`, `stop` | 7.9–8.0 | 9.1–12.1 |
| `post-tool-use` Bash | 9.44 | **19.09** |
| `pre-compact` | 14.85 | **38.64** |

**How to read this.** The tail is dominated by Windows process creation, not by
Velra: starting the binary and doing *nothing* costs a p99 of 7–25 ms here, so a
15 ms p99 allowance for a process that must also open a 38 MiB SQLite database
is not achievable on this machine regardless of how Velra is written. Velra's
own marginal cost is modest and steady: **+3.4 to +11.0 ms at p50**, and it
barely moves between a 0.5 MiB and a 38 MiB database, which says the storage
engine is not the problem either. `DECISIONS.md` D54 already records that the
p50 budgets describe Linux and that `db-open` alone costs 1.3 ms on this box;
this benchmark extends that finding to the p99 allowance, which D54 assumed
Windows would meet and which it does not.

Two caveats, stated plainly. The timings include the Python harness's own
`subprocess.run` overhead; because the spawn control is measured identically,
the **marginal** column is trustworthy and the absolute columns are an upper
bound. And the project gates on Linux with `hyperfine` in CI, which is not
measured here — this result speaks only to Windows.

**Verdict: FAILED** overall, because the latency allowance is part of the
hypothesis as stated. The fail-open guarantee — the half that protects the
user's session — holds perfectly across 4,338 observed invocations.

---

## 11. What the benchmark actually says about Velra

Stripping out the claims that did not separate the arms, three findings survive
that are worth acting on.

**1. The native summary is unbounded and wildly variable; Velra's is meant to be
neither.** Across four runs of the identical script, Claude Code's own summary
measured 670, 802, 4,410 and 6,937 tokens — a **10× spread** with no ceiling.
Velra's capsule measured 951 and 1,147 tokens.

The only fair comparisons are the two sessions where both existed, and they
split:

| Session | Native summary | Velra capsule | |
|---|---:|---:|---|
| `saturated-velra-r1` | 6,937 tok | 951 tok | Velra **7.3× smaller** |
| `saturated-velra-r2` | 802 tok | 1,147 tok | Velra **1.4× larger** |

So Velra is not reliably smaller today — it is reliably *bounded*, which is the
claim that actually matters and the one H3 shows is currently broken. At its
documented 800-token ceiling Velra would have won both comparisons; at its real
951–1,147 it wins one. Fixing the estimator (recommendation 3) is what converts
this from a split result into the advantage the design intends.

**2. Velra's record is deterministic; the native summary is a model output, and
model outputs can be refused.** In two of the superseded trials, the model
treated Claude Code's internal compaction template as a prompt injection, said
so, and declined to produce the structured summary — the "summary" stored for
those sessions was the model's refusal plus an answer to the previous question.
Compaction still reported `success`. Velra's capsule is rendered from tool
events by a pure function and cannot be talked out of existence this way. This
was an incidental observation, not a designed test, but it is reproducible in
the captures.

**3. The delivery machinery is correct, even where the content is not.** In both
Velra trials the full state machine ran exactly once: a single checkpoint frozen
at `PreCompact`, the continuation advancing `PENDING → ATTACHED → CONFIRMED`, one
injection on the `session_start` channel, and nothing emitted on the second
channel. Read back out of `saturated-velra-r2`:

```json
continuations: [{"checkpoint_id": "ckpt_01M2CV7AMZ4FQD55KM68VN2DBX", "state": "CONFIRMED",
                 "attach_count": 1, "attached_channel": "session_start",
                 "attached_ms": 1789285105498, "confirmed_ms": 1789285113865,
                 "confirm_event_id": 67}]
injections:    [{"injection_id": "ckpt_01M2CV7AMZ4FQD55KM68VN2DBX:inj:1",
                 "channel": "session_start", "ts_ms": 1789285105498}]
compactions:   [{"trigger": "manual", "pre_ms": 1789285083804, "post_ms": 1789285105574}]
```

Exactly-once delivery, the two-phase barrier and the checkpoint immutability
trigger all behaved as designed in every trial. The defects this benchmark found
are in what gets *into* the capsule (H2, H3), not in how it is delivered.

Two content-quality defects beyond the H2 bug:

- Under saturation, `[WORKING_FILES]` filled with modules touched during the
  audit (`validation/invoice_number_fr.py` and friends) and **crowded out
  `engine.py` and `rules.py`** — the files that actually mattered. Recency is
  weighted over relevance to the active failure.
- `[ACTIVE_FAILURE]`'s "Observed afterward" line in `saturated-velra-r2` quotes a
  198-character `cd "C:\Users\…" && git log …` command truncated mid-path, which
  spends capsule budget on a path prefix that carries no information.

---

## 12. Threats to validity

- **n = 2 per arm.** The H1 metrics were unanimous across replicates and are
  categorical (0 vs 0, 2/2 vs 2/2), so more replicates would sharpen confidence
  in a null result rather than change its direction. This is not enough to
  detect a small difference, and it is not claimed to be.
- **The H2 failure rate is not 50%.** The two trials failed for two *different*
  reasons, both triggered by how the agent happened to phrase one shell command.
  A bare `git restore` hits the false-re-application bug; a `cd … && git restore`
  slips past the `Bash(git *)` matcher instead. The correct reading is not "this
  breaks half the time" but "both observed phrasings of the revert produced a
  wrong answer, in different ways". The underlying defects are deterministic and
  are reproduced by the row sequence in §8, not by chance.
- **One task, one model, one Claude Code version.** The finding that vanilla
  compaction preserves task state is a statement about Claude Code 2.1.269 with
  `claude-sonnet-5` on this defect. A weaker summariser, a longer session, or a
  task with several competing dead ends could separate the arms.
- **`/compact` was issued as a user turn**, which is how the print-mode harness
  must trigger it. Auto-compaction under genuine context pressure is a different
  code path and was not exercised; the control confirms the path used here does
  really compact.
- **The task may be too easy post-compaction.** Both arms solved it in two tool
  calls. A defect requiring several coordinated edits would leave more room for
  the arms to diverge.
- **H4 is Windows-only**, measured through a Python harness, on a machine where
  bare process creation already costs more than the allowance at the tail.

---

## 13. Reproduction

```bash
python bench/run_full_benchmark.py                 # everything, 3 replicates
python bench/run_full_benchmark.py --replicates 1  # a quicker pass
bash  bench/run_full_benchmark.sh                  # POSIX wrapper
.\bench\run_full_benchmark.ps1                     # Windows wrapper (sets up MinGW on PATH)
```

The runner builds the release binary, verifies the settings round-trip, builds a
fresh fixture per trial, runs both arms, extracts telemetry, measures the
injected block with Anthropic's tokenizer, runs the compaction control and the
overhead benchmark, aggregates, writes `EVIDENCE.md` and `assets/proof.svg`, and
prints the verdicts.

It costs roughly **$2.80 per replicate** (two 16-turn Sonnet sessions). The four
trials, the compaction control and the token measurements behind this report
cost **$7.00** in total: $5.19 of trials, about $1.30 for the control, and about
$0.50 for the tokenizer measurements.

> The trials register and unregister Velra's hooks in the *real* user-level
> Claude Code settings file, because that is the thing under test. The file is
> restored and verified byte for byte, and a timestamped backup is written to
> `~/.velra/backups/` regardless.

### Artifacts

| Path | What |
|---|---|
| `bench/run_full_benchmark.py` | one-command reproduction runner |
| `bench/fixture/make_fixture.py`, `noise.py` | the fixture generator |
| `bench/harness/run_trial.py` | drives one controlled session |
| `bench/harness/analyze.py` | telemetry extraction |
| `bench/harness/compaction_probe.py` | the lossiness control |
| `bench/harness/measure_tokens.py` | tokenizer-based budget measurement |
| `bench/harness/hook_overhead.py` | direct latency benchmark |
| `bench/harness/verdict.py` | the verdict logic |
| `bench/results/EVIDENCE.md` | verbatim excerpts behind every number |
| `assets/proof.svg`, `assets/proof.png` | the proof asset |

---

## 14. Recommendations

In priority order.

1. **Fix the false re-application that deletes dead ends** — the most damaging
   finding here, because it silently removes the product's headline feature.
   `revert::reapplied` ([revert.rs:95](crates/velra-core/src/revert.rs#L95))
   must not treat a `git_pre` observation as evidence of re-application: that
   row records the file's state *before* the git command ran, so by construction
   it carries the pre-revert content and will always match a dead end's
   post-edit hash. Either exclude `git_pre` as a re-application source, or
   compare observations by the event's logical order rather than by insertion
   order. A regression test that replays the exact sequence in §8 —
   `pre_edit, post_edit, pre_edit, post_edit, turn_scan, git_pre, git_post` —
   would have caught it.
2. **Match `git` commands that are not the first word.** The `PreToolUse`
   condition `Bash(git *)` missed `cd "…" && git restore …` entirely, which is
   why the two trials failed differently. Until that is handled, revert
   attribution depends on how the agent happens to phrase its shell command.
3. **Calibrate `estimate_tokens`** (`crates/velra-core/src/text.rs:161`) to the
   capsule's measured density of 2.07-2.16 chars/token -- use 2.0 so the
   estimate errs high. This is the single change
   that turns H3 from a fail into a pass, and it needs no other work: the
   truncation ladder already enforces whatever the estimator reports.
4. **Add a real-tokenizer assertion to the golden tests** so the budget cannot
   silently drift again, and **assert that a session containing a reverted edit
   renders a `[DEAD_ENDS]` section** so recommendation 1 cannot regress.
5. **Weight `[WORKING_FILES]` by relevance to the active failure**, not by
   recency, so an unrelated audit cannot evict the files named in the failing
   traceback. Truncate shell commands from the *right* of the binary name rather
   than quoting 198 characters of `cd "C:\Users\…"`.
6. **Restate the Windows performance budget** in `README.md` and D54 as a p99
   against *marginal* cost, or raise it: 15 ms of total wall time is not
   reachable on a platform where an empty process costs 7–25 ms at p99.
7. **Re-run this benchmark against a harder defect** — one needing several
   coordinated edits, or two plausible dead ends — before claiming an advantage
   over vanilla compaction on H1. The honest current position is that Velra's
   capsule is bounded and deterministic where the native summary is neither, and
   that on this task Claude Code did not need it.

---

## 15. Addendum — what was fixed, 2026-09-13

Everything above describes `velra 0.1.0 (75ed4bbd3)` and is left exactly as it
was measured. This section records what changed afterwards, so a reader does not
take the verdict table as the current state of the code. **The trials have not
been re-run**: every claim here is backed by a test, not by a new measurement,
and §16 says what a re-run would need to settle.

All nine recommendations in §14 are addressed except the last, which is a
request for more measurement rather than a code change.

### H3 — the budget

`estimate_tokens` no longer divides the character count by 3.2. It walks the
string and charges per run the way a byte-pair tokenizer does: a word after a
space absorbs about four letters per token, a word glued to punctuation about
two, digits group in threes, and every other ASCII byte is its own token, plus a
1/32 margin so it errs high. Against the two capsules measured in §9:

| Trial | Characters | Measured | Old estimate | New estimate |
|---|---:|---:|---:|---:|
| `saturated-velra-r1` | 2,056 | 951 | 643 (−32%) | 1,042 (+9.6%) |
| `saturated-velra-r2` | 2,377 | 1,147 | 743 (−35%) | 1,218 (+6.2%) |

Both blocks are now *over* the budget by the estimator's own reckoning, which is
the point: the truncation ladder — unchanged — trims them to fit rather than
stopping while it believes it is at ~650 of 800. Both capsules are kept verbatim
in `tests/fixtures/tokenizer/` with their measured counts, and
`capsule.rs::estimator_is_above_real_tokenizer_counts` fails if the estimate
ever falls below the real tokenizer again, or overshoots it by more than a
quarter.

Making the estimator honest broke the hard ceiling: the E2 property test found a
1,001-token case within seconds. The ceiling ladder gained further steps and a
backstop that drops whole lines while keeping the opening tag through
`[STATUS]` and the `[RECOVERY]` tail, so "≤ 1,000 tokens and ≤ 9,500 characters"
is now a property rather than an expectation.

### H2 — the dead end that vanished

Both failure modes in §8 are fixed, and both are covered by tests that run
offline in milliseconds, with no Claude Code session and no cost.

**The false re-application.** `revert::reapplied` now requires two things of an
observation before it will accept it as evidence that a discarded change came
back: a *settled* source (`post_edit`, `git_post`, `turn_scan` — never
`git_pre`, `pre_edit` or `original`, all of which describe the state *before*
their event's effect), and a timestamp no older than the dead end itself.
Version history is read in hook-timestamp order rather than row-id order, and
`record_version` draws no conclusions at all from an observation older than
something already recorded — which is exactly what a spooled event looks like
when the reducer finally ingests it.
`tracking.rs::f5c_a_late_git_pre_row_does_not_resurrect_a_dead_end` replays the
seven-row sequence from §8 directly into the event log and asserts the section
reaches the capsule;
`f5d_a_later_edit_that_restores_the_content_still_reapplies` asserts a genuine
re-application still closes the dead end.

**The mis-attributed revert.** Two separate bugs produced r2's *"changed outside
the agent"*:

- `Bash(git *)` is a prefix match, so the `PreToolUse` registration never fired
  for `cd "…" && git restore …` and no `git_pre` row was ever written. There is
  no rule syntax for "git anywhere in the command line", so the `if` rule is
  gone and the filter moved into the binary, which already parses the command
  and returns before opening the database when no git subcommand is present.
- More seriously, and not diagnosed in §8: the agent's call was
  `cd … && git restore … && pytest`, the suite still failed, and Claude Code
  reported the **whole call as a failure**. Velra read git effects only from
  calls that succeeded, so it took no `git_post` observation either. That is the
  ordinary shape of discarding an attempt — you restore the file and re-run the
  suite, which still fails — and it lost the revert on both sides. Git effects
  are now read from failed calls too, since every rule involved compares hashes
  measured after the call returned. Commit detection still requires success: an
  edit that was never committed hashes the same as one that was.

`ipc_contract.rs::b1_a_chained_git_restore_is_observed_on_both_sides_and_on_failure`
drives the real binary with r2's exact command shape and asserts both sides
record the restore;
`tracking.rs::f2b_a_restore_chained_with_a_failing_command_is_still_attributed`
asserts the resulting dead end is attributed to the git command with its text.

### The two content defects in §11

`[WORKING_FILES]` ranked ties by most-recent touch, which handed the whole list
to whatever the agent did last — in r1, an audit sweep across 84 modules read
once each, which evicted both files the failing test ran through. Ranking now
separates scarce evidence (an edit, a name in the failing output, a file another
section already cites) from abundant evidence (one read), and breaks ties
towards *first* touch: among equally thin candidates, the files the session
opened with outrank the ones a late sweep touched.
`capsule.rs::an_unrelated_read_sweep_does_not_evict_the_files_the_task_is_about`
replays the sweep. `inspect --section files` uses the same order.

Quoted commands now have their leading setup subcommands (`cd`, `pushd`,
`export`, `source`, …) stripped, as a slice of the original line, so a
90-character absolute Windows path no longer consumes the 160-character
allowance before the runner is named. Only a leading run is dropped and the
remainder is verbatim, so pipelines keep their meaning; the raw line is still
stored and still shown by `inspect --section failure`.

### H4 — the latency allowance

No code changed, because the measurement in §10(b) does not describe a defect in
Velra: a process that does nothing costs a p99 of 7–25 ms on that machine, so a
15 ms allowance for one that must also open a 38 MiB SQLite database is not
reachable however the program is written. What changed is the claim. README and
`DECISIONS.md` D54 now state the Windows budget as **marginal cost over the
spawn control** — the figure §10(b) shows to be modest and stable, +3.4 to
+11.0 ms at p50, barely moving between a 0.5 MiB and a 38 MiB database — rather
than as total wall time. The p50 budgets continue to describe Linux, which is
where CI gates them with `hyperfine`.

The fail-open half of H4 needed nothing: 4,338 observed invocations, zero
non-zero exits, zero bytes of stderr.

### Found while fixing the above

- A `git commit` in a call that failed marked its edits COMMITTED, on the
  strength of a hash that is identical whether or not the commit ran.
- `revert::reapplied` accepted `absent` and `unreadable` as matching hashes.
  They are sentinels, not digests: two of either compare equal without meaning
  the same bytes are on disk.
- The redaction prefilter indexed a fixed-size array sized by hand to 16 against
  a table of 12 detectors. Adding a thirteenth would have been fine and adding a
  seventeenth would have panicked — on the hook path, where the panic is caught
  and the only visible effect is that redaction quietly stops happening. The
  array is now sized from the table.
- The ceiling backstop could emit a second closing tag, and lost the tag
  entirely if `[RECOVERY]` were ever absent.
- Schema v2 (`file_stats.first_touch_ms`) had no test that migrated a *populated*
  v1 database. It has one now: a migration that fails on real data fails only in
  the field.

The suite is 157 passing tests (plus one that needs a live `claude` binary), from 139.

---

## 16. What a re-run would settle

This addendum claims the defects are fixed, not that the verdicts have changed.
Three of the four hypotheses need a fresh measurement before their rows can be
rewritten, and one of them needs a better experiment than this one.

| # | What a re-run would establish | Cost |
|---|---|---|
| **H3** | That the *delivered* block measures ≤ 800 tokens on Anthropic's tokenizer, not merely that the estimator now reads high. `bench/harness/measure_tokens.py` answers this on its own, against a capsule from a single trial. | ~$3 |
| **H2** | That a `[DEAD_ENDS]` section reaches the capsule in every trial, and carries `git_command` with the command text rather than `external`. The unit tests assert the mechanism; only a live session proves the hook registration fires for a command the agent phrased itself. | ~$3 per replicate |
| **H1** | Nothing, on this task. §12 is right that both arms solved it in two tool calls and that more replicates would sharpen a null result rather than move it. Recommendation 7 stands: a defect needing several coordinated edits, or two plausible dead ends, before any claim of advantage over vanilla compaction. | ~$3 per replicate |
| **H4** | Nothing. The finding is about Windows process creation and will reproduce exactly. | — |

The honest position is unchanged from §11 and is not improved by these fixes
alone: Velra's capsule is bounded, deterministic and provenance-tagged where the
native summary is none of those, and on *this* task Claude Code did not need it.
What the fixes buy is that the bounded claim is now true, and that the dead end
— the one thing the native summary cannot be relied upon to keep, and the thing
this benchmark found missing from a delivered capsule — actually arrives.

---

## 17. The four-replicate run, 2026-09-14 — verdicts as re-measured

§15 fixed the defects and §16 said what a re-run would settle. A single
replicate was run on 2026-09-13; this section now carries **four replicates per
arm**, run 2026-09-14, and supersedes both. **This section supersedes §1 for the
current binary**; §1 is left standing because it describes `75ed4bbd3`, which is
a different program.

**Binary under test:** `velra 0.1.0 (e9f40151c)`, release, `x86_64-pc-windows-gnu`, 5,385,728 bytes
**Agent:** Claude Code **2.1.270**, model `claude-sonnet-5` — **both arms on the same build**, which closes the confound §18 named
**Trials:** 4 replicates × 2 arms, saturated protocol, 16 turns, `/compact` at
turn 14, measured turn 15. Session ids and per-run telemetry in §2.

| # | Hypothesis | Verdict | The number that decides it |
|---|---|---|---|
| **H1** | Compaction amnesia elimination | **INCONCLUSIVE** | re-reads **0** (median, `[0,0,0,0]` both arms), first edit on `engine.settle` **4/4 both arms**, suite green **4/4 both arms**, tool calls **2.0 Velra vs 3.0 baseline** (medians). The control says compaction was not lossy, so recall was never tested |
| **H2** | Dead-end loop prevention | **INCONCLUSIVE** | dead end in the database **4/4**, in the **delivered** capsule **4/4**, re-explored **0/4** — and **0/4** in the baseline too |
| **H3** | Continuation budget ≤ 800 tokens | **PASSED** | **705 / 715 / 721 / 722** real tokens; median **718**, worst **722**, all under the 800 ceiling at the 745 calibrated budget; control delta **0** in all four |
| **H4** | Zero-overhead fail-open guarantee | **FAILED** | fail-open perfect (**481 invocations, 0 non-zero exits, 0 stderr bytes**); worst marginal p99 **163.31 ms** against 15 ms |

Phase 1 environment checks: **PASSED**.

### The control, and why it changes two verdicts

The control is the load-bearing measurement in this run, and it came out the
other way from 2026-09-13:

| | 2026-09-13 | **2026-09-14 (4-replicate run)** |
|---|---:|---:|
| context before `/compact` | — | 54,999 tok |
| context after `/compact` | — | 42,856 tok |
| reduction | 41.3% | **22.1%** |
| verbatim canary recalled | no | **yes — answered `"AQE1"`, no tool call** |
| → compaction is lossy | **true** | **false** |

H1 and H2 are both claims of the form *the capsule prevents a loss that
compaction causes*. When the control reports no loss, those claims are not
refuted — they are **untested**. Reporting them as PASSED on the strength of
"re-reads 0, first edit correct" would be reporting the absence of a problem as
evidence of a cure, when the baseline posts identical numbers. Hence
INCONCLUSIVE, which is the harness's own verdict (`bench/results/verdicts.json`).

Note this is a property of the *task*, not a fix: the fixture's defect is
recoverable from the failing assertion alone, so a summariser that keeps the
test name keeps enough. §16's "harder test" item is now the blocking work for
H1 and H2, not a nice-to-have.

### H1, in absolute terms

Every absolute target H1 names was met, in every replicate, in **both** arms:

| Measure | Baseline (n=4) | Velra (n=4) |
|---|---|---|
| source files re-read before the first edit | 0 `[0,0,0,0]` | 0 `[0,0,0,0]` |
| first edit landed on `engine.settle` | 4/4 | 4/4 |
| test suite green at the end | 4/4 | 4/4 |
| tool calls on the measured turn | 3.0 median `[3,2,4,3]` | **2.0 median `[2,2,2,2]`** |
| turns to defect resolution | 1 (the measured turn) in 4/4 | 1 (the measured turn) in 4/4 |

Both arms resolved the defect **within the single measured turn** in all eight
sessions. The only measure that separates them is tool-call count, where Velra
is both lower and — across four runs — perfectly stable at 2, while the
baseline varies between 2 and 4. That is a real and reproducible difference,
but it is an efficiency signal, not the amnesia signal H1 is about.

### H2, and what the capsule actually carried

`[DEAD_ENDS]` reached the delivered capsule in **4/4** replicates. The §15 fix
for the vanishing section holds across four runs, not one. Attribution is
provenance-tagged in every case:

```
[DEAD_ENDS] (OBSERVED)
- src/ledger/money.py | 2 edit(s) | reverted via `git restore src/ledger/money.py` at 15:22
  Observed afterward: `python -m pytest -q` FAIL. Causal link: UNCONFIRMED.
```

Three of four name the revert mechanism verbatim (`reverted via git restore`);
**r2 attributes it as `changed outside the agent`** instead, because the revert
reached the file by a route Velra did not observe as an agent edit. The section
is still correct and still names the right file — but the attribution string is
not stable across replicates, which is worth knowing before it is quoted as a
feature.

As in the single-replicate run, the capsule does **not** name `ROUND_HALF_EVEN`:
`dead_end_approach_named_verbatim` is **false in 4/4**. H2 should be read as
*the abandoned file is carried across compaction*, never as *the abandoned
approach is*.

Re-exploration was **0/4** in the Velra arm — and **0/4** in the baseline, which
is why the verdict is inconclusive rather than passed.

### H3, settled across four replicates

The one hypothesis this run settles cleanly.

| Trial | Capsule chars | **Measured tokens** | Budget | Control delta |
|---|---:|---:|---:|---:|
| `saturated-velra-r1` | 1,527 | **715** | 800 | 0 |
| `saturated-velra-r2` | 1,516 | **722** | 800 | 0 |
| `saturated-velra-r3` | 1,492 | **705** | 800 | 0 |
| `saturated-velra-r4` | 1,527 | **721** | 800 | 0 |

Median **718**, range **705–722**, worst case **722** — a **78-token margin**
under the 800 ceiling. Every measurement is Anthropic's own tokenizer, and the
measurement control (two identical prompts differing only by the capsule) reads
a delta of exactly 0 in all four, so the figures are the capsule and nothing
else. `render::DEFAULT_BUDGET_TOKENS` is **745**: the spec's 800 less the margin
that covers the estimator's measured ~6% under-read. Delivery was
`SessionStart:compact`, exit 0, in 4/4.

### H4, fail-open perfect and latency still failing

**The fail-open half holds without exception, at four times the prior sample
size.** Across the four Velra sessions the stream recorded **481 in-session hook
invocations** spanning all ten registered events, with **0 non-zero exits** and
**0 bytes written to stderr**. That is the guarantee that matters for safety,
and it has never once failed a measurement.

The latency half fails, measured directly against a 38 MiB / 100,000-event
database, 200 runs per case, spawn control p50 3.958 ms / p99 8.952 ms:

| Case | p50 | p99 | marginal p50 | marginal p99 |
|---|---:|---:|---:|---:|
| `PostToolUse`, Read (small) | 7.738 | **172.259** | +3.780 | **+163.307** |
| `PostToolUse`, Edit | 8.017 | 11.909 | +4.059 | +2.957 |
| `PostToolUse`, Bash | 9.244 | 14.175 | +5.286 | +5.223 |
| `PostToolUseFailure`, Bash | 8.504 | 10.290 | +4.546 | +1.338 |
| `PreToolUse`, Edit | 7.806 | 12.594 | +3.848 | +3.642 |
| `UserPromptSubmit` | 7.785 | 9.807 | +3.827 | +0.855 |
| `SessionStart`, startup | 7.413 | 52.205 | +3.455 | +43.253 |
| `Stop` | 7.615 | 11.534 | +3.657 | +2.582 |
| `PreCompact` | 14.593 | 23.709 | **+10.635** | +14.757 |

**Marginal p50 is 3.455–10.635 ms across all nine cases — every one of them
under the 15 ms allowance.** The verdict is decided entirely by the p99 column,
and there the honest reading is that **two tail outliers dominate**:
`PostToolUse, Read (small)` at 163.31 ms marginal p99 and `SessionStart,
startup` at 43.25 ms. Read (small) is the *cheapest* work Velra does — its p50
is 7.738 ms, second-lowest in the table — so a p99 twenty-two times its own p50
is a scheduling artifact on a shared Windows desktop (this run shared the
machine with a VS Code session), not a cost inherent to the hook.

That caveat does not rescue the verdict, and it is not offered as one.
`PreCompact` remains the case with a real, systematic cost: **+10.635 ms
marginal p50**, the only case above 10 ms, and §4 line 89 independently budgets
`pre-compact` at `p99 ≤ 10 ms`. Its marginal p99 of 14.757 ms passes 15 ms only
by 0.24 ms. The work §17 named on 2026-09-13 is unchanged: find the ~10 ms p50
that `PreCompact` spends above process spawn (checkpoint freeze, capsule render,
database write) and reduce it. Raising the threshold was considered and rejected
again — any number chosen after seeing the measurement fail is not a budget.

The two false leads recorded on 2026-09-13 still stand and should not be
re-chased: the reducer tail is zero (`batch: 500` sizes a batch, it does not cap
the total), and the 15 ms allowance is if anything too generous against a spawn
control of 8.95 ms p99.

---

## 18. Open findings, re-measured across four replicates

### `[WORKING_FILES]` is dropped in 4/4 — the ladder cliff is now reproducible

`SPEC_STEPS` in `render.rs` sets `working_max = 4` as its first step and
`working_max = 0` as its last, with nothing in between. The section therefore
goes from four files to none in a single move.

On 2026-09-13 this flipped on and off across three budgets. **Across the four
replicates of 2026-09-14 it is no longer intermittent: `[WORKING_FILES]` is
absent from all four delivered capsules.** Every capsule carried exactly the
same eight sections and no others:

```
[CONTEXT] [ROOT_TASK_OBJECTIVE] [LATEST_REQUEST] [STATUS]
[ACTIVE_FAILURE] [DEAD_ENDS] [NEXT_KNOWN_TARGET] [RECOVERY]
```

`has_recent_attempts_section` is likewise **false in 4/4**. This is the settled
retention behaviour of the current ladder at the 745 budget: with
`[ACTIVE_FAILURE]` present, the ladder takes its last step before the estimate
settles, and the section is crowded out by what the checkpoint holds at freeze
time rather than by the budget number. Each capsule landed at 705–722 real
tokens — **78 or more tokens under the ceiling** — and still lost the section.

**Planned for v0.2:** a `working_max = 2` step between the existing two. The two
top-ranked files (by D59's weights, here `engine.py` and `tests/test_engine.py`)
cost roughly 35 estimated tokens, which the measured 78-token headroom covers
comfortably.

### The prompt-injection rejection rate is 0/4 in this run

On 2026-09-13, one of three Velra replicates opened its measured turn by
refusing the capsule as fabricated content. **Across the four replicates of
2026-09-14 that did not recur: the rejection rate is 0/4 (0%).**

The transcripts were scanned for any language classifying the block as
injected, fabricated, untrusted or to-be-ignored, and for any mention of
`VELRA_CONTINUATION` in the agent's own prose; there are **zero matches in all
four sessions**. In each one the agent accepted the continuation capsule
silently and acted on it, opening the measured turn with the substantive fix:

> "The bug matches the test's own hint: discount must be computed once on the
> invoice subtotal, not summed per line item." — `saturated-velra-r4`

All four then executed the same two-call sequence — `Edit src/ledger/engine.py`,
`Bash python -m pytest -q` — and finished green.

**This does not close the finding.** Four clean replicates against one prior
rejection gives an observed rate of 1/7 across all Velra replicates ever run,
and the two runs differ in a way that plausibly matters: the 2026-09-13
rejection occurred when the arms were split across Claude Code 2.1.269/2.1.270,
whereas both arms here ran 2.1.270. A rate this low needs far more than seven
samples to bound. The v0.2 plan is unchanged — reframe the capsule as passive
workspace state rather than anything resembling an instruction, and make
rejection rate a first-class metric the harness computes, rather than something
found by grepping transcripts after the fact.

### The cross-build confound is closed

§18 of the 2026-09-13 run recorded that its two arms ran under different Claude
Code builds (2.1.269 baseline, 2.1.270 Velra), which mattered because the
comparator for H1 and H2 *is* the native compaction summariser. **In this run
all eight sessions ran under Claude Code 2.1.270**, resolved by
`bench/harness/claude_binary.py` once and reused for every trial. That confound
no longer applies to any figure in §17.

### What still limits this dataset

1. **The control invalidates the premise, not the product.** Compaction was not
   lossy here, so H1 and H2 are untested rather than refuted. A fixture whose
   defect cannot be recovered from the failing assertion alone is the blocking
   work.
2. **One baseline replicate never compacted** (`saturated-baseline-r2`), leaving
   3 valid baseline replicates for compaction-dependent measures.
3. **n=4 is still small** for anything expressed as a rate, including the 0/4
   rejection rate above.
4. **Single host, single OS.** Every figure is from one Windows 11 desktop that
   was not otherwise idle; the H4 p99 tail in §17 shows what that costs.

---
