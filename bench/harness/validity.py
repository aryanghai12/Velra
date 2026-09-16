#!/usr/bin/env python3
"""Decide whether a trial is a valid replicate, against the registered rules.

The v0.1 benchmark lost a baseline replicate to a session that reached the
measured turn without ever compacting. It was caught by reading
`analysis.json` afterwards and noticing `compact_status: null`. That is one
reader's diligence standing between the report and a contaminated denominator,
so it is a function now.

Each criterion is reported separately, because an invalid trial is excluded
only from the measures that depend on the criterion it failed. A trial that
never compacted still counts for hook cleanliness; it does not count for
anything about post-compaction behaviour.
"""

from __future__ import annotations

import json
import pathlib

# Criterion -> which measures it gates. "behavioural" covers everything scored
# on the measured turn; "hooks" covers the fail-open guarantee; "budget"
# covers the token measurement.
GATES = {
    "all_turns_completed": ("behavioural", "budget"),
    "no_budget_abort": ("behavioural", "budget"),
    "compaction_occurred": ("behavioural", "budget"),
    "compaction_summary_present": ("behavioural",),
    "measured_turn_present": ("behavioural", "budget"),
    "arm_integrity": ("behavioural", "budget", "hooks"),
}


def evaluate(meta: dict, turns: dict, hooks: dict, summary: dict) -> dict:
    """Apply every registered validity criterion to one trial."""
    expected_turns = meta.get("turns_expected")
    sent = meta.get("turns_sent")
    measured_index = meta.get("measured_turn_index")
    compact = meta.get("compact_status") or {}
    arm = meta.get("arm")

    subtypes = [(turns[i].get("result") or {}).get("subtype") for i in sorted(turns)]
    aborted = [s for s in subtypes
               if s and s not in ("success", "error_max_turns")]

    observed_hooks = hooks.get("observed") or 0
    criteria = {
        "all_turns_completed": bool(expected_turns and sent == expected_turns),
        "no_budget_abort": not aborted,
        "compaction_occurred": compact.get("compact_result") == "success",
        "compaction_summary_present": bool(summary.get("present")),
        "measured_turn_present": measured_index in turns,
        # Cross-contamination: the arms must differ in the one way they claim
        # to, and a baseline session that fired Velra's hooks is not a
        # baseline.
        "arm_integrity": (observed_hooks > 0) if arm == "velra" else (observed_hooks == 0),
    }

    failed = sorted(k for k, ok in criteria.items() if not ok)
    excluded = sorted({gate for k in failed for gate in GATES.get(k, ())})
    return {
        "criteria": criteria,
        "failed_criteria": failed,
        "valid": not failed,
        "excluded_from": excluded,
        "detail": {
            "turns_sent": sent,
            "turns_expected": expected_turns,
            "result_subtypes": subtypes,
            "aborted_subtypes": aborted,
            "compact_status": compact or None,
            "hook_invocations_observed": observed_hooks,
            "arm": arm,
        },
    }


def usable_for(validity: dict, measure: str) -> bool:
    """Is this trial admissible for ``measure``?"""
    return measure not in validity["excluded_from"]


def summarise(trials: list[dict]) -> dict:
    """A run-level account of what was valid and what was not."""
    out = {"total": len(trials), "valid": 0, "invalid": 0, "invalid_trials": [],
           "usable": {"behavioural": 0, "budget": 0, "hooks": 0}}
    for trial in trials:
        v = trial["validity"]
        if v["valid"]:
            out["valid"] += 1
        else:
            out["invalid"] += 1
            out["invalid_trials"].append({
                "trial": trial.get("dir") or trial.get("trial"),
                "arm": trial.get("arm"),
                "scenario": trial.get("scenario"),
                "failed_criteria": v["failed_criteria"],
                "excluded_from": v["excluded_from"],
            })
        for measure in out["usable"]:
            if usable_for(v, measure):
                out["usable"][measure] += 1
    return out


if __name__ == "__main__":
    import sys
    for path in sys.argv[1:]:
        data = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
        print(path, json.dumps(data.get("validity"), indent=2))
