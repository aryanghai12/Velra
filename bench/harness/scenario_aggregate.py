#!/usr/bin/env python3
"""Pool every analysed trial into one comparison, grouped by scenario and arm.

Only trials the validity rules admit for a measure are counted towards that
measure. Invalid trials are listed in full with the criterion they failed, so
the denominator behind every rate is visible and nothing disappears quietly.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import prereg  # noqa: E402
import stats  # noqa: E402
import validity  # noqa: E402


def load(trial_dir: pathlib.Path) -> dict | None:
    path = trial_dir / "analysis.json"
    if not path.exists():
        print(f"skipping {trial_dir.name}: no analysis.json", file=sys.stderr)
        return None
    row = json.loads(path.read_text(encoding="utf-8"))
    row["dir"] = trial_dir.name
    for name, key in (("token_measurement.json", "token_measurement"),
                      ("native_summary_tokens.json", "native_summary_tokens")):
        candidate = trial_dir / name
        row[key] = json.loads(candidate.read_text(encoding="utf-8")) \
            if candidate.exists() else None
    return row


def capsule_tokens(row: dict) -> list[int]:
    tm = row.get("token_measurement") or {}
    return [i["measured_tokens"] for i in tm.get("injections", [])]


def summarise(rows: list[dict]) -> dict:
    usable = [r for r in rows if validity.usable_for(r["validity"], "behavioural")]
    behaviours = [r["behaviour"] for r in usable if r.get("behaviour")]
    primary = behaviours[0]["primary"] if behaviours else None

    measured = [r["measured_turn"] for r in usable if r.get("measured_turn")]
    native = [r["native_summary_tokens"] for r in rows
              if (r.get("native_summary_tokens") or {}).get("measured")]

    capsules = [t for r in rows for t in capsule_tokens(r)]
    delivered = [r for r in rows if (r.get("delivered_capsule") or {}).get("delivered")]

    return {
        "n_trials": len(rows),
        "n_usable_behavioural": len(usable),
        "sessions": [r["session_id"] for r in rows],
        "primary_metric": primary,
        "primary_each": [b.get(b["primary"]) for b in behaviours] if primary else [],
        "primary_successes": sum(1 for b in behaviours if b.get(b["primary"])) if primary else 0,
        "success_each": [b["success"] for b in behaviours],
        "successes": sum(1 for b in behaviours if b["success"]),
        "suite_green": sum(1 for b in behaviours if b.get("suite_green")),
        "tool_calls_each": [m["tool_call_count"] for m in measured],
        "tool_calls_median": stats.median([m["tool_call_count"] for m in measured]),
        "source_rereads_each": [m["source_rereads_before_first_edit"] for m in measured],
        "source_rereads_median": stats.median(
            [m["source_rereads_before_first_edit"] for m in measured]),
        "wall_seconds_median": stats.median([r["wall_seconds"] for r in rows]),
        "cost_usd_each": [round(r.get("total_cost_usd") or 0, 4) for r in rows],
        "cost_usd_total": round(sum(r.get("total_cost_usd") or 0 for r in rows), 4),
        # Capsule-side measures. Empty for the baseline arm by construction.
        "capsules_delivered": len(delivered),
        "capsule_tokens_each": capsules,
        "capsule_tokens_median": stats.median(capsules),
        "capsule_tokens_worst": max(capsules) if capsules else None,
        "capsule_sections_each": [r["delivered_capsule"].get("sections") for r in delivered],
        "working_files_section_present": sum(
            1 for r in delivered if r["delivered_capsule"].get("has_working_files_section")),
        "dead_ends_section_present": sum(
            1 for r in delivered if r["delivered_capsule"].get("has_dead_ends_section")),
        "capsule_rejected": sum(
            1 for r in rows if (r.get("prompt_rejection") or {}).get("rejected")),
        "working_file_recall_each": [
            r["working_file_relevance"].get("recall") for r in rows
            if (r.get("working_file_relevance") or {}).get("delivered")],
        "constraint_in_capsule": sum(
            1 for r in rows if (r.get("capsule_constraint") or {}).get("constraint_in_capsule")),
        "dead_ends_all_named": sum(
            1 for r in rows if (r.get("capsule_dead_ends") or {}).get("all_named")),
        "dead_end_attribution": [
            a for r in rows for a in (r.get("capsule_dead_ends") or {}).get("attribution", [])],
        # Native summary, measured on the same tokenizer as the capsule.
        "native_summary_present": sum(
            1 for r in rows if (r.get("native_compaction_summary") or {}).get("present")),
        "native_summary_tokens_each": [n["measured_tokens"] for n in native],
        "native_summary_tokens_median": stats.median([n["measured_tokens"] for n in native]),
        # Hook contract.
        "hook_invocations": sum((r.get("hooks") or {}).get("observed") or 0 for r in rows),
        "hook_nonzero_exits": sum((r.get("hooks") or {}).get("nonzero_exit") or 0 for r in rows),
        "hook_stderr_writes": sum(
            (r.get("hooks") or {}).get("responses_with_stderr") or 0 for r in rows),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("trials", nargs="+")
    ap.add_argument("--out", default="bench/results/v0.1.1/aggregate.json")
    args = ap.parse_args()

    rows = [r for r in (load(pathlib.Path(t)) for t in args.trials) if r]
    groups: dict[tuple[str, str], list[dict]] = {}
    for row in rows:
        groups.setdefault((row["scenario"], row["arm"]), []).append(row)

    agg = {
        **prereg.stamp(),
        "trials": [{k: r.get(k) for k in
                    ("dir", "scenario", "arm", "replicate", "model", "session_id")}
                   | {"valid": r["validity"]["valid"],
                      "failed_criteria": r["validity"]["failed_criteria"]}
                   for r in rows],
        "validity": validity.summarise(rows),
        "groups": {f"{s}/{a}": summarise(rs) for (s, a), rs in sorted(groups.items())},
        "provenance": next((r.get("provenance") for r in rows if r.get("provenance")), None),
    }

    # Cross-arm comparisons, one per scenario, on the registered primary
    # metric. Computed here so the verdict script only has to read them.
    comparisons = {}
    for scenario in sorted({s for s, _ in groups}):
        velra = agg["groups"].get(f"{scenario}/velra")
        base = agg["groups"].get(f"{scenario}/baseline")
        if not (velra and base):
            continue
        comparisons[scenario] = {
            "primary_metric": velra["primary_metric"],
            "primary": stats.compare([bool(x) for x in velra["primary_each"]],
                                     [bool(x) for x in base["primary_each"]]),
            "success": stats.compare(velra["success_each"], base["success_each"]),
            "tool_calls_median": {"velra": velra["tool_calls_median"],
                                  "baseline": base["tool_calls_median"]},
            "tool_calls_each": {"velra": velra["tool_calls_each"],
                                "baseline": base["tool_calls_each"]},
        }
    agg["comparisons"] = comparisons

    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(agg, indent=2), encoding="utf-8", newline="")

    lines = []
    for scenario, cmp in comparisons.items():
        velra = agg["groups"][f"{scenario}/velra"]
        base = agg["groups"][f"{scenario}/baseline"]
        lines.append(f"#### `{scenario}` — primary metric `{cmp['primary_metric']}`\n")
        lines.append("| Measure | Baseline | Velra |")
        lines.append("|---|---:|---:|")
        lines.append(f"| Valid replicates | {base['n_usable_behavioural']}/{base['n_trials']} "
                     f"| {velra['n_usable_behavioural']}/{velra['n_trials']} |")
        lines.append(f"| {cmp['primary_metric']} | "
                     f"{base['primary_successes']}/{base['n_usable_behavioural']} | "
                     f"{velra['primary_successes']}/{velra['n_usable_behavioural']} |")
        lines.append(f"| Overall success | {base['successes']}/{base['n_usable_behavioural']} "
                     f"| {velra['successes']}/{velra['n_usable_behavioural']} |")
        lines.append(f"| Suite green | {base['suite_green']}/{base['n_usable_behavioural']} "
                     f"| {velra['suite_green']}/{velra['n_usable_behavioural']} |")
        lines.append(f"| Tool calls, median | {base['tool_calls_median']} "
                     f"| {velra['tool_calls_median']} |")
        lines.append(f"| Native summary, median tokens | "
                     f"{base['native_summary_tokens_median']} "
                     f"| {velra['native_summary_tokens_median']} |")
        lines.append(f"| Capsule, median tokens | — | {velra['capsule_tokens_median']} |")
        lines.append(f"\nOne-sided Fisher exact p on the primary metric: "
                     f"**{cmp['primary']['fisher_p_one_sided']}** "
                     f"(best achievable at this n: "
                     f"{cmp['primary']['best_achievable_p_at_this_n']})\n")
    md = "\n".join(lines)
    out.with_suffix(".md").write_text(md, encoding="utf-8", newline="")
    print(md)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
