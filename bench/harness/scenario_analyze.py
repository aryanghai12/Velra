#!/usr/bin/env python3
"""Turn one captured trial into ``analysis.json``.

Pure function of the bytes on disk: no network, no Claude, no fixture
directory. Re-running this against a trial captured months ago produces the
same numbers, which is what makes the raw captures worth keeping.

The scoring is split in two on purpose:

  generic    tool calls, re-reads, hook cleanliness, capsule contents, the
             Velra database, the native summary. Identical for every scenario.
  scenario   the behavioural outcome the scenario was built to measure,
             computed by the scenario's own ``score`` function from the final
             repository state.

Neither half is told which arm it is looking at.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO_ROOT / "bench"))

import behaviour  # noqa: E402
import validity  # noqa: E402
from scenarios import registry  # noqa: E402


def analyse(trial: pathlib.Path) -> dict:
    meta = json.loads((trial / "trial_meta.json").read_text(encoding="utf-8"))
    manifest = meta["manifest"]
    scenario = registry.get(meta["scenario"])

    turns = behaviour.load_stream(trial / "stream.jsonl")
    measured_index = meta["measured_turn_index"]
    hooks = behaviour.analyse_hooks(trial / "stream_timing.jsonl")
    summary = behaviour.native_summary(trial / "transcript.jsonl")
    checks = validity.evaluate(meta, turns, hooks, summary)

    capsule = behaviour.delivered_capsule(trial / "stream.jsonl")
    final = behaviour.final_tree(trial)

    measured = None
    scored = None
    if measured_index in turns:
        measured = behaviour.measured_turn(turns[measured_index])
        measured["source_rereads_before_first_edit"] = behaviour.source_rereads(
            measured, manifest)
        scored = scenario.score(final, measured, manifest)

    analysis = {
        "trial": trial.name,
        "scenario": meta["scenario"],
        "mechanism": scenario.mechanism,
        "arm": meta["arm"],
        "replicate": meta["replicate"],
        "pair_id": meta.get("pair_id"),
        "pair_key": meta.get("pair_key"),
        "model": meta["model"],
        "session_id": meta["session_id"],
        "wall_seconds": meta["wall_seconds"],
        "compact_status": meta["compact_status"],
        "turns_observed": sorted(turns),
        "validity": checks,
        "measured_turn": measured,
        "behaviour": scored,
        "hooks": hooks,
        "delivered_capsule": {k: v for k, v in capsule.items() if k != "text"},
        "capsule_dead_ends": behaviour.capsule_carries_dead_ends(capsule, manifest),
        "capsule_constraint": behaviour.capsule_carries_constraint(capsule, manifest),
        "working_file_relevance": behaviour.working_file_relevance(capsule, manifest),
        "prompt_rejection": behaviour.prompt_rejection(turns, measured_index, capsule),
        "native_compaction_summary": {k: v for k, v in summary.items() if k != "text"},
        "velra_db": behaviour.analyse_db(trial / "velra.db"),
        "final_state": {k: v for k, v in final.items() if k != "files"},
        "total_cost_usd": sum(
            m.get("total_cost_usd") or 0 for m in meta.get("turn_marks", [])),
        "per_turn": {
            str(i): {"prompt": t["prompt"][:70],
                     "tools": len(t["tool_calls"]),
                     "names": sorted({c["name"] for c in t["tool_calls"]})}
            for i, t in sorted(turns.items())
        },
        "provenance": meta.get("provenance"),
        "preregistration_sha256": meta.get("preregistration_sha256"),
        "manifest": manifest,
    }

    # The delivered bytes are kept beside the analysis rather than inside it,
    # so the JSON stays readable and the capsule stays quotable verbatim.
    if capsule.get("delivered"):
        (trial / "delivered_capsule.txt").write_text(
            capsule["text"], encoding="utf-8", newline="")
    if summary.get("present"):
        (trial / "native_summary.txt").write_text(
            summary["text"], encoding="utf-8", newline="")
    return analysis


def report(analysis: dict) -> None:
    print(f"=== {analysis['scenario']} / {analysis['arm']} "
          f"r{analysis['replicate']}  session {analysis['session_id']} ===")
    v = analysis["validity"]
    print(f"  valid replicate:       {v['valid']}"
          + ("" if v["valid"] else f"  failed: {v['failed_criteria']}"))
    m = analysis["measured_turn"]
    if m:
        print(f"  measured-turn tools:   {m['tool_call_count']}  {m['buckets']}")
        print(f"  source re-reads:       {m['source_rereads_before_first_edit']}")
        print(f"  first edit:            {m['first_edit_file']}")
    b = analysis["behaviour"]
    if b:
        print(f"  outcome ({b['primary']}): {b.get(b['primary'])}   "
              f"success: {b['success']}")
    c = analysis["delivered_capsule"]
    if c.get("delivered"):
        print(f"  capsule delivered:     {c['chars']} chars via {c['hook_name']}")
        print(f"    sections:            {c['sections']}")
    de = analysis["capsule_dead_ends"]
    if de.get("applicable"):
        print(f"    dead ends named:     {de['dead_end_files_named']} "
              f"(all: {de['all_named']}, attribution: {de['attribution']})")
    ct = analysis["capsule_constraint"]
    if ct.get("applicable"):
        print(f"    constraint carried:  {ct.get('constraint_in_capsule')}")
    wf = analysis["working_file_relevance"]
    if wf.get("delivered"):
        print(f"    working files:       present={wf.get('section_present')} "
              f"recall={wf.get('recall')} precision={wf.get('precision')}")
    pr = analysis["prompt_rejection"]
    if pr.get("applicable"):
        print(f"    capsule rejected:    {pr['rejected']}")
    h = analysis["hooks"]
    if h.get("observed"):
        print(f"  hooks:                 {h['observed']} invocations, "
              f"nonzero-exit {h['nonzero_exit']}, stderr {h['responses_with_stderr']}")
    ns = analysis["native_compaction_summary"]
    print(f"  native summary:        "
          + (f"{ns['chars']} chars" if ns.get("present") else f"absent ({ns.get('reason')})"))
    print(f"  final pytest exit:     {analysis['final_state'].get('pytest_exit')}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--trial", required=True)
    args = ap.parse_args()
    trial = pathlib.Path(args.trial).resolve()
    analysis = analyse(trial)
    (trial / "analysis.json").write_text(
        json.dumps(analysis, indent=2), encoding="utf-8", newline="")
    report(analysis)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
