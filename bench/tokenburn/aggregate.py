#!/usr/bin/env python3
"""The one pipeline: raw artifacts to verdicts, for mock and live alike.

    python bench/tokenburn/aggregate.py --trials <dir> --out <dir>

Nothing branches on where the artifacts came from. :mod:`mock_adapter` writes
into a trials directory and this runs over it; the live driver writes into a
trials directory and this runs over it. If the two ever diverged, the selftest
would be testing a fake evaluator, which is the failure mode the whole design
is arranged against.

Stages, in order::

    parse.parse_trial      raw artifacts   -> ParsedTrial
    metrics.evaluate       ParsedTrial     -> metrics + A2/A3/A4 scoring
    causal.evaluate        both            -> the A..I chain
    pairing.pair_up        evaluations     -> matched pairs, dropped pairs named
    verdict.pair_verdict   pair + chains   -> one of four values
    verdict.pool           verdicts        -> counts and comparable summaries
    report.*               everything      -> the tables §17 asks for
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import causal  # type: ignore[no-redef]
    import isolation  # type: ignore[no-redef]
    import metrics  # type: ignore[no-redef]
    import pairing  # type: ignore[no-redef]
    import parse  # type: ignore[no-redef]
    import prereg  # type: ignore[no-redef]
    import report as report_mod  # type: ignore[no-redef]
    import runroot  # type: ignore[no-redef]
    import verdict as verdict_mod  # type: ignore[no-redef]
else:
    from . import runroot
    from . import causal, isolation, metrics, pairing, parse, prereg
    from . import report as report_mod
    from . import verdict as verdict_mod


def invalid_evaluation(trial_dir: pathlib.Path, meta: dict) -> dict:
    """The whole record of a trial that declared itself invalid.

    Nothing is scored: an invalid trial has no destination session to parse,
    or one that ran under conditions the scenario did not intend. It carries
    its identity so `pairing` can drop its pair by name, and its reasons.
    """
    return {
        "trial": trial_dir.name,
        "scenario": meta.get("scenario"),
        "arm": meta.get("arm"),
        "pair_id": meta.get("pair_id"),
        "pair_key": meta.get("pair_key"),
        "trial_validity": meta.get("trial_validity"),
        "invalidation_reason": meta.get("invalidation_reason"),
        "auto_memory_disabled": meta.get("auto_memory_disabled"),
        "memory_scan_clean": meta.get("memory_scan_clean"),
        "source_handoff_valid": meta.get("source_handoff_valid"),
        "source_handoff_reason": meta.get("source_handoff_reason"),
        "target_status_at_handoff": meta.get("target_status_at_handoff"),
        "scored": False,
    }


def analyse_trial(trial_dir: pathlib.Path) -> tuple[dict, dict]:
    """One trial: its evaluation and its causal chain."""
    meta = json.loads((trial_dir / "trial_meta.json").read_text(
        encoding="utf-8"))
    if isolation.is_invalid(meta):
        return invalid_evaluation(trial_dir, meta), {
            "causal_validity": "INVALID_TRIAL", "first_broken_link": None,
            "invalidation_reason": meta.get("invalidation_reason")}
    parsed = parse.parse_trial(trial_dir)
    evaluation = metrics.evaluate(parsed)
    chain = causal.evaluate(parsed, evaluation)
    evaluation["causal_validity"] = chain["causal_validity"]
    evaluation["first_broken_link"] = chain["first_broken_link"]
    evaluation["trial_validity"] = parsed.meta.get("trial_validity")
    return evaluation, chain


def run(trials_root: pathlib.Path, out_dir: pathlib.Path,
        *, write: bool = True) -> dict:
    """The whole pipeline over one trials directory.

    Reads only ``trials_root`` -- nothing outside it is discovered -- and
    with ``write`` refuses to touch protected evidence: aggregating writes
    analysis.json and causal_chain.json into every trial directory, so
    re-running it over frozen trials would regenerate the evidence.
    """
    trials_root = pathlib.Path(trials_root)
    out_dir = pathlib.Path(out_dir)
    if write:
        runroot.assert_writable(trials_root, "per-trial analysis")
        runroot.assert_writable(out_dir, "the aggregate")
    dirs = [p for p in sorted(trials_root.iterdir())
            if p.is_dir() and (p / "trial_meta.json").exists()] \
        if trials_root.is_dir() else []

    evaluations: list[dict] = []
    chains: dict[str, dict] = {}
    for trial_dir in dirs:
        evaluation, chain = analyse_trial(trial_dir)
        evaluations.append(evaluation)
        chains[evaluation["trial"]] = chain
        if write:
            (trial_dir / "analysis.json").write_text(
                json.dumps(evaluation, indent=2, default=str),
                encoding="utf-8", newline="")
            (trial_dir / "causal_chain.json").write_text(
                json.dumps(chain, indent=2, default=str),
                encoding="utf-8", newline="")

    paired = pairing.pair_up(evaluations)
    verdicts = [verdict_mod.pair_verdict(pair, chains)
                for pair in paired["pairs"]]
    pooled = verdict_mod.pool(verdicts)

    result = {
        "trials_root": str(trials_root),
        "n_trials": len(evaluations),
        "pairing": {"n_pairs": paired["n_pairs"],
                    "dropped": paired["dropped"],
                    "unassigned": paired["unassigned"]},
        "pairs": verdicts,
        "pooled": pooled,
        # Invalid trials are listed by themselves and appear nowhere else:
        # not in the rows, not in a pair, not in a pool.
        "invalid_trials": [e for e in evaluations
                           if e.get("scored") is False],
        "trial_rows": [report_mod.trial_row(e, chains[e["trial"]])
                       for e in evaluations if e.get("scored") is not False],
        **prereg.stamp(),
    }

    if write:
        out_dir.mkdir(parents=True, exist_ok=True)
        (out_dir / "aggregate.json").write_text(
            json.dumps(result, indent=2, default=str),
            encoding="utf-8", newline="")
        (out_dir / "verdicts.json").write_text(
            json.dumps({"pairs": verdicts, "pooled": pooled, **prereg.stamp()},
                       indent=2, default=str),
            encoding="utf-8", newline="")
        (out_dir / "report.md").write_text(
            report_mod.markdown(result), encoding="utf-8", newline="")
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--trials", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()
    result = run(pathlib.Path(args.trials), pathlib.Path(args.out))
    if not args.quiet:
        print(report_mod.summary(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
