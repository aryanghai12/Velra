# Benchmark results

One tree in this directory is the current benchmark evidence. Everything else
is historical: kept byte for byte because reports, tags and hashes point at it,
and **not** part of the current release evidence.

## Current

| Tree | What it is |
|---|---|
| [`v0.1.2-requal/`](v0.1.2-requal/) | **Velra v0.1.2 Token-Burn requalification.** Preregistration `velra-tokenburn` v1.1.0 (sha256 `c13150b6…ffcd2`). 4 matched pairs, 8 trials, 16 Claude Code sessions. Claude Code 2.1.280, Sonnet, run at commit `51b96cb`, 250K synthetic-context rung. All 8 trials valid. |

Start with [`v0.1.2-requal/report.md`](v0.1.2-requal/report.md), the pipeline's
own output. [`aggregate.json`](v0.1.2-requal/aggregate.json) is the same data
with every number's provenance attached. Each directory under
[`v0.1.2-requal/trials/`](v0.1.2-requal/trials/) holds one trial:

| File | Contents |
|---|---|
| `trial_meta.json` | identity, pair key, memory isolation controls and scans, source handoff, validity |
| `source_handoff.json` | the tree at the transition: target test FAIL, invariant as generated, worktree as generated |
| `context_fixture.json` | the synthetic context-load fixture the source session read (a proxy, never a Claude observation) |
| `velra_restore.json` | Velra arm only: the `velra restore` result, the staged record and the ledger/capsule marker scans |
| `final_state.json` | the destination's end state: target test, full suite, invariant file |
| `analysis.json` | every metric with its source, artifact and measurement status; correctness checks; first correct action |
| `causal_chain.json` | links A–I, each with its evidence and reason |
| `arm_setup.txt` | output of `velra enable` / `velra disable` for the arm |

Raw captures (`stream.jsonl`, `source_stream.jsonl`, `transcript.jsonl`,
`velra_home/`, `memory_quarantine-*/`) are not committed: the transcripts carry
account context Claude Code injects into every session, and all of them embed
local paths. [`raw_captures.sha256.json`](v0.1.2-requal/raw_captures.sha256.json)
lists each by size and SHA-256, so they can be verified if they are shared.
The tracked derived files are kept byte for byte as the run wrote them, which
includes the run machine's absolute paths (fixture and settings locations).

## Historical — not part of the current release evidence

| Tree | What it was | Status |
|---|---|---|
| [`v0.1.2/tokenburn/`](v0.1.2/tokenburn/) | First Token-Burn qualification, preregistration 1.0.0, Claude Code 2.1.272, product `c7b1caa`. Tag `tokenburn-qualification-v0.1.2`. | **Historical / invalidated.** All 4 pairs INCONCLUSIVE (CAPTURE_FAILURE), and the run was confounded: Claude Code auto-memory carried scenario state into both arms, and some source sessions solved the target before the transition. Neither was detectable under 1.0.0. Superseded by `v0.1.2-requal/`. |
| [`v0.1.2/`](v0.1.2/) (everything except `tokenburn/`) | v0.1.2-hardened re-run of the archived S1–S3 efficacy scenarios. | Superseded historical benchmark (`bench/legacy/`). |
| [`v0.1.1/`](v0.1.1/) | v0.1.1 efficacy benchmark, S1–S3. | Superseded historical benchmark. |
| `aggregate.*`, `verdicts.json`, `EVIDENCE.md`, `trials/`, `hook_overhead*.json`, `compaction_probe.json`, `phase1_environment.json` (this directory) | v0.1 `/compact` benchmark, written up in [`BENCHMARK_REPORT.md`](../../BENCHMARK_REPORT.md). | Superseded historical benchmark. |

The runner refuses to write into any historical tree (`bench/tokenburn/runroot.py`).
