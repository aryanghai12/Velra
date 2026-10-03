#!/usr/bin/env python3
"""Matching a baseline trial to a Velra trial, on recorded identity.

Pairing on a replicate number is an assumption about how the runner was
invoked. Pairing on ``pair_key`` is a fact about the two trials: it carries the
benchmark, the scenario, the fixture seed, the model, the Claude Code build,
the hash of the turn script, the permission mode and the context-ladder rung,
and every one of them has to agree or the two sessions were not run under the
same conditions and their difference is not attributable to Velra.

A pair that half-exists, or whose invariants disagree, or that contains a trial
which declared itself invalid (`isolation.trial_validity`: a memory channel,
or a source session that did not leave the scenario's handoff state), is
**dropped whole and named**. It is never repaired by taking the arm that is present, and the
dropped pairs appear in the aggregate so the denominator stays honest.
"""

from __future__ import annotations

import pathlib
from typing import Iterable, Sequence

if __package__ in (None, ""):
    import sys
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import prereg  # type: ignore[no-redef]
else:
    from . import prereg

ARMS = ("baseline", "velra")


def pair_key_of(evaluation: dict) -> dict:
    """The recorded identity, restricted to the registered invariants."""
    key = evaluation.get("pair_key") or {}
    return {name: key.get(name) for name in prereg.pair_invariants()}


def mismatch(a: dict, b: dict) -> list[dict]:
    """Invariants on which two pair keys disagree.

    A field that is ``None`` on both sides counts as agreeing: it was not
    recorded on either arm, which is a gap in the record rather than a
    difference between the arms. A field present on one side and absent on the
    other is a mismatch, because that means the two trials were not produced by
    the same runner configuration.
    """
    out = []
    for name in prereg.pair_invariants():
        left, right = a.get(name), b.get(name)
        if left == right:
            continue
        out.append({"field": name, "baseline": left, "velra": right})
    return out


def pair_up(evaluations: Sequence[dict]) -> dict:
    """Group per-trial evaluations into matched pairs.

    ``evaluations`` are the dicts :mod:`metrics` produced, each carrying the
    trial's ``pair_id``, ``arm`` and ``pair_key``. Returns the usable pairs and
    every reason a trial did not make it into one.
    """
    by_pair: dict[str, dict] = {}
    unassigned: list[dict] = []
    for evaluation in evaluations:
        pair_id = evaluation.get("pair_id")
        arm = evaluation.get("arm")
        if not pair_id or arm not in ARMS:
            unassigned.append({"trial": evaluation.get("trial"),
                               "why": f"no pair_id, or unknown arm {arm!r}"})
            continue
        slot = by_pair.setdefault(pair_id, {})
        if arm in slot:
            unassigned.append({"trial": evaluation.get("trial"),
                               "why": f"a second {arm} trial claims pair "
                                      f"{pair_id!r}; the first one is kept"})
            continue
        slot[arm] = evaluation

    pairs: list[dict] = []
    dropped: list[dict] = []
    for pair_id in sorted(by_pair):
        slot = by_pair[pair_id]
        # First, because it is the most specific statement about the pair:
        # whatever else is true of it, an invalid trial means its comparison
        # would not be about the scenario.
        invalid = {arm: (slot[arm].get("trial_validity") or {})
                   .get("invalidation_reason") or "invalid"
                   for arm in ARMS if arm in slot
                   and (slot[arm].get("trial_validity") or {}).get("valid") is False}
        if invalid:
            dropped.append({"pair_id": pair_id, "reason": "INVALID_TRIAL",
                            "invalid_arms": invalid,
                            "present": sorted(slot)})
            continue
        missing = [arm for arm in ARMS if arm not in slot]
        if missing:
            dropped.append({"pair_id": pair_id, "reason": "INCOMPLETE_PAIR",
                            "missing_arms": missing,
                            "present": sorted(slot)})
            continue
        keys = {arm: pair_key_of(slot[arm]) for arm in ARMS}
        differences = mismatch(keys["baseline"], keys["velra"])
        if differences:
            dropped.append({"pair_id": pair_id, "reason": "IDENTITY_MISMATCH",
                            "differences": differences})
            continue
        pairs.append({
            "pair_id": pair_id,
            "benchmark": keys["baseline"].get("benchmark"),
            "scenario": keys["baseline"].get("scenario")
                        or slot["baseline"].get("scenario"),
            "pair_key": keys["baseline"],
            "arms": slot,
        })
    return {"pairs": pairs, "dropped": dropped, "unassigned": unassigned,
            "n_pairs": len(pairs), "n_dropped": len(dropped)}
