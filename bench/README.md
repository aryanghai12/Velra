# Velra benchmarks

Two things live here. One is the current efficacy benchmark; the other is an
archive that is kept working and kept out of the scorecard.

| | What it answers | Runner | Results |
|---|---|---|---|
| **v0.1.2 Token-Burn** | Does leaving a large conversation behind and restoring a bounded capsule into a fresh session cost less input — and still do the work correctly? | `bench/tokenburn/run.py` | `--results-root <PATH>` (frozen v0.1.2 qualification: `bench/results/v0.1.2/`) |
| **archive** (v0.1, v0.1.1, v0.1.2-hardened) | Historical. Did the capsule change what the agent did *after compaction*? | `bench/legacy/` | `bench/results/` |

The archive is described in [`legacy/README.md`](legacy/README.md). Nothing in
it contributes to the Token-Burn scorecard: separate trials root, separate
pre-registration, and a test that fails the build if a Token-Burn module so
much as names a legacy artifact path.

---

## The Token-Burn benchmark

The thesis, stated once:

> A developer can intentionally leave a huge Claude Code conversation behind,
> start a brand-new session, restore only a small bounded operational state,
> and continue the work without rehydrating the entire old conversation.

```
LARGE / EXPENSIVE CONVERSATIONAL STATE
            ↓  intentional exit
  NEW FRESH CLAUDE SESSION
            ↓  velra restore → SessionStart(startup)
  SMALL VELRA OPERATIONAL STATE
            ↓
       CONTINUE WORK
```

Two benchmarks, one fixture:

| | Transition | The question |
|---|---|---|
| `A_cold_continuation` | end the session, start a new one | Does a large native continuation cost more input than a fresh session plus a bounded capsule? |
| `B_clear_survival` | `/clear` | After `/clear`, what does a fresh native session have to spend to get back to where the work was? |

The fixture is a small payments service with **three genuinely failing tests**.
Nothing in the tree says which one the developer was working on, which approach
they had already tried and reverted, or what they had decided about the fix —
because those are facts about a conversation, not about a repository. A fresh
session can read every file and still not know them, and it can also
legitimately work them out, which is a baseline success and is scored as one.

The abandoned approach is never committed. It is an edit made during the
session and reverted with `git restore`, so `git log`, `git reflog` and
`git stash list` carry no trace of it — and the leak scanner treats all three as
fatal surfaces.

### Running it

```bash
# The pipeline on synthetic trials. 23 scripted cases, ~5 s, spends nothing.
python bench/tokenburn/run.py --selftest

# Memory isolation and source-handoff validation, proven offline.
python bench/tokenburn/run.py --preflight

# Readiness: fixtures, ground truth, leak scans, the ladder, telemetry, the
# smoke test, the preflight and the plan. Writes <root>/readiness.json.
python bench/tokenburn/run.py --dry-run --results-root bench/results/<run>

# restore → SessionStart against the real binary. Offline; no Claude process.
python bench/tokenburn/run.py --smoke

# The expensive part. Refuses without VELRA_ALLOW_LIVE_BENCHMARK=1 as well,
# and refuses outright from inside a Claude Code session.
python bench/tokenburn/run.py --live --results-root bench/results/<run> --stage qualification
```

A run writes everything it owns — `trials/`, `readiness.json`,
`run_state.json`, `aggregate.json`, `verdicts.json`, `report.md`,
`settings-backup/`, `quarantine/` — under its `--results-root`, and aggregates
only that root's trials. Relative roots resolve against the current directory.
`bench/results/v0.1.2/` is the frozen qualification evidence (7a09e65, tag
`tokenburn-qualification-v0.1.2`): the runner refuses to write there, or to
any root inside or containing it, and `--dry-run`/`--live` without a
`--results-root` would write there, so they refuse too.

`--resume`, `--only`, `--pairs` and `--force` make a partial run restartable
without overwriting anything: a re-run quarantines the old trial rather than
deleting it.

The dry run ends in `READY FOR LIVE EVALUATION` or in a list of blockers.

### Why the design looks like this

**Every number carries its provenance.** A metric cannot exist without naming
its source, the artifact it was read out of, and whether it was measured,
proxied or simply unavailable. `bench/tokenburn/telemetry.py`.

**Missing telemetry is never zero.** If `cache_read_input_tokens` is not
explicitly present in structured data, it is `unavailable`, arithmetic over it
returns `inconclusive`, and the pair's verdict is `INCONCLUSIVE`. It is never
recovered by pattern-matching stdout, stderr or a status line —
`scan_for_terminal_scraping` enforces that over the package's own source, and a
test feeds it a deliberate offender to prove the guard fires.

**A partial field is not a total.** If the cache fields are on eleven of twelve
usage records, the sum of the eleven is reported `unavailable` with the count.
Reporting it would understate a baseline's cached input, which is the direction
that flatters Velra.

**UNKNOWN is not EXPIRED.** A run whose cache state was not reported did not
demonstrate a cache miss.

**Correctness outranks tokens.** A Velra arm that spent a tenth of the input
and wrote the wrong fix loses to a baseline that spent everything and got it
right. The saving is still printed — that row is the most informative one the
benchmark can produce — but the verdict goes against Velra.

**A synthetic fixture's size is never a Claude observation.**
`synthetic_context_size` and `achieved_context_size` are separate metrics with
separate sources, and a test fails if they are ever the same reading.

**Incomplete causal evidence rounds down.** Nine links, A to I; the first one
that cannot be demonstrated makes the trial `INCONCLUSIVE` and names the
failure class.

**Pairs match on recorded identity.** Benchmark, scenario, pair id, fixture
seed, model, Claude Code build, turn-script hash, permission mode and ladder
rung. A pair that half-exists or disagrees is dropped whole and named.

### The context-load ladder

100K, 250K, 500K, 700K, 800K, 900K — **targets, not capability claims**.
Generated programmatically by `bench/tokenburn/context_fixture.py` from a seed,
streamed to disk, verified by re-reading. The generated text is realistic
engineering output (build logs, pytest output, diffs, shell transcripts, tool
results, source excerpts), not filler, and it is labelled
`kind: synthetic_load_fixture` everywhere so its size can never be quoted as a
property of a real session.

```bash
python bench/tokenburn/context_fixture.py --plan
python bench/tokenburn/context_fixture.py --target 900000 --into /tmp/fx
```

Fixtures are generated at run time and are not committed; a 900K rung is
3.2 MB.

### Trial counts

Two stages, registered in `preregistration_tokenburn.json`:

* **qualification** — 2 matched pairs per benchmark. Its job is to find out
  whether the scenario creates the claimed pain and whether everything measures.
  A weak scenario is redesigned here. It does not count toward the scorecard.
* **formal** — 4 matched pairs per benchmark, after the scenario is frozen.
  8 pairs, 16 trials.

Few and brutal rather than many and weak: the hypothesis is that continuing a
500K-token conversation costs an order of magnitude more input than a fresh
session plus an 800-token capsule. An effect that size is visible in four pairs
or it is not there. The registered materiality threshold is 25%.

---

## Layout

```
bench/
  tokenburn/          the v0.1.2 Token-Burn benchmark  (see __init__.py)
  legacy/             S1/S2/S3 and their runners, archived and still working
    scenarios/  fixture/  run_*.py
  harness/            shared: claude_binary, provenance, measure_tokens,
                      verify_env, plus the archive's own analysis modules
                      (selftest.py, stages.py, scenario_*.py, loss_probe.py,
                      behaviour.py, validity.py, verdict.py, aggregate.py,
                      analyze.py, hardened_verdict.py, regression_gate.py,
                      run_trial.py, compaction_probe.py, native_tokens.py,
                      prereg.py, stats.py, make_*.py, hook_overhead.py)
  tests/              pytest for both systems
  results/            every result tree, historical ones untouched
```

`bench/harness/` was deliberately not moved: it holds modules both systems use,
and `python bench/harness/selftest.py` is still the archive's own offline
check, named as such in the release checklist.

---

## Tests

```bash
python -m pytest bench/tests -q            # both systems, ~4 s without --slow
python -m pytest bench/tests -q -m slow    # fixture builds and the 900K rung
python bench/tokenburn/selftest.py         # the 18 scripted cases
python bench/harness/selftest.py           # the archive's pipeline
python bench/tokenburn/smoke.py            # restore → SessionStart, offline
```

## Reading the results

Do not trust a verdict on its own. Every number is recomputable from the raw
captures:

```bash
python bench/tokenburn/parse.py --trial <trial-dir>
python bench/tokenburn/aggregate.py --trials <trials-dir> --out <out-dir>
```

`analysis.json` is a pure function of `stream.jsonl`, `final_state.json`,
`velra_restore.json` and `context_fixture.json`. In the report, `unavailable`
means the structured data did not carry the field — not zero, and not something
recovered from terminal output; `~n` means a proxy, and each trial's
`analysis.json` says what it stands in for.

---

## The archive, in one paragraph

The v0.1 report ([`BENCHMARK_REPORT.md`](../BENCHMARK_REPORT.md)) settled
whether the capsule is delivered, bounded and deterministic, and could not
settle whether it changes behaviour: its control measured compaction as *not
lossy* on its task. v0.1.1 tried to settle that and produced a number nobody
could interpret. v0.1.2-hardened broke that one number into five stages so a
capture failure, a delivery failure and a task failure stopped being reported
as the same result. All three are within-session experiments about compaction,
and none of them crosses a session boundary — which is the whole of what
v0.1.2 ships, and why the Token-Burn benchmark exists. See
[`legacy/README.md`](legacy/README.md).
