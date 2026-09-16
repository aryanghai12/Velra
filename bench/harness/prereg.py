#!/usr/bin/env python3
"""Load the pre-registration, and pin it to a hash.

The point of writing success criteria down before the experiment is only worth
anything if the file cannot quietly change afterwards. Every artifact the run
produces carries ``preregistration_sha256``; if two result sets disagree on it,
they were decided under different rules and must not be pooled.
"""

from __future__ import annotations

import hashlib
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
PATH = HERE / "preregistration.json"


def raw() -> bytes:
    return PATH.read_bytes()


def digest() -> str:
    return hashlib.sha256(raw()).hexdigest()


def load() -> dict:
    data = json.loads(raw().decode("utf-8"))
    data["_sha256"] = digest()
    data["_path"] = str(PATH)
    return data


def hypothesis(prereg: dict, hid: str) -> dict:
    for h in prereg["hypotheses"]:
        if h["id"] == hid:
            return h
    raise KeyError(hid)


def stamp(prereg: dict | None = None) -> dict:
    """The provenance block every result artifact embeds."""
    prereg = prereg or load()
    return {
        "preregistration_version": prereg["preregistration_version"],
        "preregistration_sha256": prereg["_sha256"],
        "minimum_replicates_per_arm": prereg["replicates"]["minimum_per_arm"],
    }


if __name__ == "__main__":
    print(json.dumps(stamp(), indent=2))
