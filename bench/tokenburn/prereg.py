#!/usr/bin/env python3
"""Load the Token-Burn pre-registration and pin it to a hash.

Every artifact the benchmark writes carries ``preregistration_sha256``. The
verdict refuses to score an aggregate produced under a different hash, so
changing the rules after a run invalidates the run instead of reinterpreting
it. That is the whole mechanism; there is nothing else to it.
"""

from __future__ import annotations

import functools
import hashlib
import json
import pathlib

PATH = pathlib.Path(__file__).resolve().parent / "preregistration_tokenburn.json"


def digest(path: pathlib.Path = PATH) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@functools.lru_cache(maxsize=1)
def load() -> dict:
    return json.loads(PATH.read_text(encoding="utf-8"))


def stamp() -> dict:
    """What every artifact carries so it can be re-scored under its own rules."""
    doc = load()
    return {
        "preregistration_id": doc["id"],
        "preregistration_version": doc["version"],
        "preregistration_sha256": digest(),
    }


def rules() -> dict:
    return load()["verdict_rules"]


def material_burden_pct() -> float:
    return float(rules()["material_burden_change_pct"])


def material_effort_steps() -> int:
    return int(rules()["material_effort_change_steps"])


def failure_class(link: str) -> str:
    return rules()["failure_classes"].get(link, "UNKNOWN_FAILURE")


def pair_invariants() -> tuple[str, ...]:
    return tuple(load()["pair_identity_invariants"])


def ladder() -> tuple[int, ...]:
    return tuple(load()["context_ladder"])


def capsule_token_ceiling() -> int:
    return int(load()["capsule_token_ceiling"])


def minimum_source_turns() -> int:
    return int(load()["minimum_source_turns"])


if __name__ == "__main__":
    print(json.dumps(stamp(), indent=2))
