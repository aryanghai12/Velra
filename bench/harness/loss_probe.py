#!/usr/bin/env python3
"""Per-scenario control: did compaction lose anything in *this* protocol?

The v0.1 benchmark ran one lossiness probe against one hand-written turn
script, and its answer decided two hypotheses. On 2026-09-13 it said compaction
was lossy; on 2026-09-14, on the same probe, it said the opposite — and that
reversal is what moved H1 and H2 to INCONCLUSIVE. A single global control
carrying that much weight is a design flaw, for two reasons: it measures a
protocol nobody ran, and one sample decides everything.

This replaces it with a probe that replays the *scenario's own* turns up to the
compaction boundary, so the control describes the experiment rather than
something adjacent to it. Velra is disabled throughout: this measures vanilla
Claude Code and nothing else.

Method, unchanged where it was right:

  1. A no-tool turn immediately before ``/compact``. One API iteration, so the
     billed input token count *is* the context the model is carrying.
  2. ``/compact``.
  3. The same no-tool turn twice more. The compaction request's own usage is
     billed to the turn after it, so the first reading is contaminated and the
     second is the measurement.
  4. A verbatim-detail question: one constant, read incidentally in an early
     turn, never asked about and never written into any reply. Tools are
     forbidden and UNKNOWN is offered, so a miss is a miss rather than a guess.
  5. **One probe per declared target fact.** This is the part that makes the
     control describe the experiment rather than something next to it.

Step 4 measures whether compaction forgot *anything*. Step 5 measures whether
it forgot *the thing the scenario is about*, which is a different question and
the only one that licenses attributing a behavioural difference to the capsule.
A scenario whose target fact survives compaction is NOT CAUSALLY TESTABLE: its
arms may differ for a dozen reasons and none of them is Velra. Reporting that
plainly costs one probe turn and is worth more than any number it could replace.

``--replicates`` runs the whole thing more than once, because one sample
deciding a verdict is what went wrong last time.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import statistics
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO_ROOT / "bench"))

import claude_binary  # noqa: E402
import prereg  # noqa: E402
from scenarios import registry  # noqa: E402

CLAUDE = claude_binary.resolve()
PROBE = "Reply with exactly: OK"

# Reduction below which compaction is not treated as having removed context.
LOSSY_REDUCTION_PCT = 20


def canary_question(canary: dict) -> str:
    return ("Do not use any tools and do not read any files. Answer purely "
            "from what you still have in context: what is the exact string "
            f"assigned to {canary['const']} in {canary['file']}? Reply with "
            "just that string, or with the single word UNKNOWN if that detail "
            "is no longer available to you.")


def build_turns(scenario) -> tuple[list[str], dict]:
    """The scenario's own turns, up to the boundary, plus the probe sequence.

    Target-fact probes come last, after the canary, so they cannot prime it --
    and each one is asked in its own turn, because a single question covering
    several facts lets a partial answer look like a whole one.
    """
    before = list(scenario.turns[:scenario.compact_index])
    turns = before + [PROBE, "/compact", PROBE, PROBE,
                      canary_question(scenario.canary)]
    indices = {
        "probe_before": len(before),
        "compact": len(before) + 1,
        "probe_contaminated": len(before) + 2,
        "probe_after": len(before) + 3,
        "canary": len(before) + 4,
    }
    fact_indices = {}
    for fact in scenario.target_facts:
        fact_indices[fact.id] = len(turns)
        turns.append(fact.question())
    indices["target_facts"] = fact_indices
    return turns, indices


def one_run(scenario, fixture: pathlib.Path, model: str,
            max_budget_usd: float) -> dict:
    scenario.build(fixture)
    turns, idx = build_turns(scenario)

    env = dict(os.environ)
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)

    proc = subprocess.Popen(
        [str(CLAUDE), "-p", "--input-format", "stream-json",
         "--output-format", "stream-json", "--verbose",
         "--model", model, "--permission-mode", "bypassPermissions",
         "--permission-prompts", "none", "--strict-mcp-config",
         "--max-budget-usd", str(max_budget_usd)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        cwd=str(fixture), env=env, text=True, encoding="utf-8",
        errors="replace", bufsize=1)

    def send(text: str) -> None:
        proc.stdin.write(json.dumps({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        }) + "\n")
        proc.stdin.flush()

    turn = 0
    records: list[dict] = []
    texts: dict[int, list[str]] = {}
    compact_status = None
    send(turns[0])

    for line in proc.stdout:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("type") == "assistant":
            for block in obj.get("message", {}).get("content", []):
                if block.get("type") == "text":
                    texts.setdefault(turn, []).append(block["text"])
                elif block.get("type") == "tool_use":
                    texts.setdefault(turn, []).append(f"<TOOL:{block.get('name')}>")
        if obj.get("type") == "system" and obj.get("compact_result"):
            compact_status = obj.get("compact_result")
        if obj.get("type") != "result":
            continue
        usage = obj.get("usage", {}) or {}
        total = (usage.get("input_tokens", 0)
                 + usage.get("cache_creation_input_tokens", 0)
                 + usage.get("cache_read_input_tokens", 0))
        records.append({
            "turn": turn,
            "prompt": turns[turn][:90],
            "total_input": total,
            "api_iterations": len(usage.get("iterations") or []),
            "subtype": obj.get("subtype"),
            "text": " ".join(texts.get(turn, []))[:600],
        })
        print(f"    turn {turn}: total_input={total} "
              f"iterations={records[-1]['api_iterations']}", flush=True)
        turn += 1
        if turn < len(turns):
            send(turns[turn])
        else:
            proc.stdin.close()
            break

    try:
        proc.wait(timeout=120)
    except subprocess.TimeoutExpired:
        proc.kill()

    def rec(i):
        return next((r for r in records if r["turn"] == i), None)

    before = rec(idx["probe_before"])
    after = rec(idx["probe_after"])
    canary = rec(idx["canary"])
    facts = {}
    for fact in scenario.target_facts:
        record = rec(idx["target_facts"][fact.id])
        facts[fact.id] = fact.recalled((record or {}).get("text", ""))
        facts[fact.id]["turn"] = idx["target_facts"][fact.id]
        facts[fact.id]["answered"] = record is not None
    answer = (canary or {}).get("text", "").strip()
    value = scenario.canary["value"]
    recalled = value in answer
    b = (before or {}).get("total_input")
    a = (after or {}).get("total_input")
    reduction_pct = round(100.0 * (b - a) / b, 1) if (b and a) else None

    return {
        "compact_status": compact_status,
        "turns": records,
        "indices": idx,
        "context_before_compaction": b,
        "context_after_compaction": a,
        "context_after_compaction_contaminated":
            (rec(idx["probe_contaminated"]) or {}).get("total_input"),
        "context_reduction_tokens": (b - a) if (b and a) else None,
        "context_reduction_pct": reduction_pct,
        "canary": dict(scenario.canary),
        "canary_answer": answer,
        "canary_recalled": recalled,
        "canary_said_unknown": "UNKNOWN" in answer.upper(),
        "canary_used_tool": "<TOOL:" in answer,
        "target_facts": facts,
        # A run where compaction did not happen tells us nothing either way.
        "valid": compact_status == "success" and b is not None and a is not None,
        "compaction_is_lossy": bool(
            compact_status == "success"
            and reduction_pct is not None
            and reduction_pct > LOSSY_REDUCTION_PCT
            and not recalled),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--scenario", required=True)
    ap.add_argument("--fixture", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--replicates", type=int, default=2)
    ap.add_argument("--max-budget-usd", type=float, default=12.0)
    args = ap.parse_args()

    scenario = registry.get(args.scenario)
    runs = []
    for replicate in range(1, args.replicates + 1):
        print(f"  loss probe {scenario.name} r{replicate}", flush=True)
        runs.append(one_run(scenario,
                            pathlib.Path(args.fixture).resolve().with_name(
                                pathlib.Path(args.fixture).name + f"-r{replicate}"),
                            args.model, args.max_budget_usd))

    valid = [r for r in runs if r["valid"]]
    lossy = [r for r in valid if r["compaction_is_lossy"]]
    reductions = [r["context_reduction_pct"] for r in valid
                  if r["context_reduction_pct"] is not None]
    out = {
        "scenario": scenario.name,
        "model": args.model,
        "replicates": len(runs),
        "valid_replicates": len(valid),
        "lossy_replicates": len(lossy),
        "reduction_pct_each": reductions,
        "reduction_pct_median": round(statistics.median(reductions), 1) if reductions else None,
        "canary_recalled_each": [r["canary_recalled"] for r in valid],
        # Stage 1 of the causal chain reads this. One entry per declared target
        # fact, one row per replicate; `stages.stage_compaction_loss` applies
        # the majority rule to it.
        "target_facts": {
            fact.id: [
                {**r["target_facts"][fact.id], "valid": r["valid"]}
                for r in runs if fact.id in (r.get("target_facts") or {})
            ]
            for fact in scenario.target_facts
        },
        "target_facts_declared": [fact.id for fact in scenario.target_facts],
        # The gate the verdict script reads. A scenario counts as lossy only
        # if a majority of its valid control runs lost the canary: one sample
        # deciding two hypotheses is what went wrong in v0.1.
        "compaction_is_lossy": bool(valid and len(lossy) * 2 > len(valid)),
        "lossy_rule": ("a majority of valid control replicates show >"
                       f"{LOSSY_REDUCTION_PCT}% context reduction and no "
                       "verbatim canary recall"),
        "runs": runs,
        **prereg.stamp(),
    }
    target = pathlib.Path(args.out)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(out, indent=2), encoding="utf-8", newline="")

    print(f"\n{scenario.name}: {len(lossy)}/{len(valid)} valid control runs lossy; "
          f"median reduction {out['reduction_pct_median']}%; "
          f"-> compaction_is_lossy = {out['compaction_is_lossy']}")
    for fact_id, rows in out["target_facts"].items():
        usable = [r for r in rows if r["valid"]]
        lost = [r for r in usable if not r["recalled"]]
        verdict = ("LOST" if usable and len(lost) * 2 > len(usable)
                   else "SURVIVED -- scenario NOT CAUSALLY TESTABLE")
        print(f"  target fact {fact_id}: forgotten in {len(lost)}/{len(usable)} "
              f"valid runs -> {verdict}")
        for r in usable:
            print(f"      {'miss' if not r['recalled'] else 'RECALL'}: "
                  f"{r['answer'][:110]!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
