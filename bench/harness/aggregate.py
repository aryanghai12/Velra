#!/usr/bin/env python3
"""Aggregate every analysed trial into one comparison table.

Reads ``analysis.json`` from each trial directory given on the command line and
writes ``bench/results/aggregate.json`` plus a Markdown table that the report
embeds verbatim. Trials are grouped by (protocol, arm) so that replicates are
summarised rather than listed one by one.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import statistics
import sys


def load(trial_dir: pathlib.Path) -> dict | None:
    path = trial_dir / "analysis.json"
    if not path.exists():
        print(f"skipping {trial_dir}: no analysis.json", file=sys.stderr)
        return None
    a = json.loads(path.read_text(encoding="utf-8"))
    meta = json.loads((trial_dir / "trial_meta.json").read_text(encoding="utf-8"))
    a["protocol"] = meta.get("protocol", "short")
    a["noise"] = meta.get("noise", False)
    a["dir"] = trial_dir.name
    tok = trial_dir / "token_measurement.json"
    a["token_measurement"] = json.loads(tok.read_text(encoding="utf-8")) if tok.exists() else None
    return a


def summarise(rows: list[dict]) -> dict:
    def mean(vals):
        vals = [v for v in vals if v is not None]
        return round(statistics.fmean(vals), 2) if vals else None

    m = [r["measured_turn"] for r in rows]
    return {
        "n": len(rows),
        "sessions": [r["session_id"] for r in rows],
        "compaction_succeeded": sum(
            1 for r in rows
            if (r.get("compact_status") or {}).get("compact_result") == "success"),
        "source_rereads_mean": mean([x["source_rereads_before_first_edit"] for x in m]),
        "source_rereads_each": [x["source_rereads_before_first_edit"] for x in m],
        "tool_calls_mean": mean([x["tool_call_count"] for x in m]),
        "tool_calls_each": [x["tool_call_count"] for x in m],
        "hit_true_file": sum(1 for x in m if x["first_edit_hits_true_file"]),
        "hit_true_symbol": sum(1 for x in m if x["first_edit_hits_true_symbol"]),
        "dead_end_reexplored": sum(1 for x in m if x["dead_end_reexplored"]),
        "dead_end_mentioned_in_prose": sum(
            1 for x in m if x["dead_end_mentioned_in_prose"]),
        "fixed_the_test": sum(1 for r in rows if r["final_pytest_exit"] == 0),
        "read_bucket_mean": mean([x["buckets"]["read"] for x in m]),
        "edit_bucket_mean": mean([x["buckets"]["edit"] for x in m]),
        "bash_bucket_mean": mean([x["buckets"]["bash"] for x in m]),
        "search_bucket_mean": mean([x["buckets"]["search"] for x in m]),
        "cost_usd_each": [r.get("total_cost_usd") for r in rows],
        "wall_seconds_mean": mean([r.get("wall_seconds") for r in rows]),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("trials", nargs="+")
    ap.add_argument("--out", default="bench/results/aggregate.json")
    args = ap.parse_args()

    rows = [r for r in (load(pathlib.Path(t)) for t in args.trials) if r]
    groups: dict[tuple[str, str], list[dict]] = {}
    for r in rows:
        groups.setdefault((r["protocol"], r["arm"]), []).append(r)

    agg = {
        "trials": [
            {k: r[k] for k in ("dir", "protocol", "arm", "replicate", "model",
                               "session_id", "final_pytest_exit")}
            for r in rows
        ],
        "groups": {f"{p}/{a}": summarise(rs) for (p, a), rs in sorted(groups.items())},
    }

    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(agg, indent=2), encoding="utf-8", newline="")

    # --- Markdown -------------------------------------------------------
    lines = []
    for protocol in sorted({p for p, _ in groups}):
        base = agg["groups"].get(f"{protocol}/baseline")
        velra = agg["groups"].get(f"{protocol}/velra")
        if not (base and velra):
            continue
        lines.append(f"#### Protocol: `{protocol}`\n")
        lines.append("| Metric (measured turn) | Baseline | Velra |")
        lines.append("|---|---:|---:|")
        def row(label, key, fmt="{}"):
            lines.append(f"| {label} | {fmt.format(base[key])} | {fmt.format(velra[key])} |")
        lines.append(f"| Replicates | {base['n']} | {velra['n']} |")
        lines.append(f"| Compaction succeeded | {base['compaction_succeeded']}/{base['n']} "
                     f"| {velra['compaction_succeeded']}/{velra['n']} |")
        row("Source file re-reads before first edit (mean)", "source_rereads_mean")
        row("Total tool calls (mean)", "tool_calls_mean")
        lines.append(f"| First edit hit the true file | {base['hit_true_file']}/{base['n']} "
                     f"| {velra['hit_true_file']}/{velra['n']} |")
        lines.append(f"| First edit hit `engine.settle` | {base['hit_true_symbol']}/{base['n']} "
                     f"| {velra['hit_true_symbol']}/{velra['n']} |")
        lines.append(f"| Re-explored the reverted dead end | {base['dead_end_reexplored']}/{base['n']} "
                     f"| {velra['dead_end_reexplored']}/{velra['n']} |")
        lines.append(f"| Test suite green at the end | {base['fixed_the_test']}/{base['n']} "
                     f"| {velra['fixed_the_test']}/{velra['n']} |")
        lines.append("")

    md = "\n".join(lines)
    out.with_suffix(".md").write_text(md, encoding="utf-8", newline="")
    print(md)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
