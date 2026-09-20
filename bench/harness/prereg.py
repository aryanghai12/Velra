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

#: The v0.1.2 hardened design. `preregistration.json` stays exactly as it was,
#: because every artifact under `bench/results/v0.1.1/` carries its hash and
#: those results have to remain interpretable under the rules they were
#: collected beneath. A new file is the only way to register new rules without
#: rewriting the old ones.
PATH_V2 = HERE / "preregistration_v2.json"


def raw(path: pathlib.Path = PATH) -> bytes:
    return path.read_bytes()


def digest(path: pathlib.Path = PATH) -> str:
    return hashlib.sha256(raw(path)).hexdigest()


def load(path: pathlib.Path = PATH) -> dict:
    data = json.loads(raw(path).decode("utf-8"))
    data["_sha256"] = digest(path)
    data["_path"] = str(path)
    return data


def load_v2() -> dict:
    return load(PATH_V2)


def stamp_v2() -> dict:
    """The provenance block every v0.1.2 artifact embeds."""
    prereg = load_v2()
    return {
        "preregistration_version": prereg["preregistration_version"],
        "preregistration_sha256": prereg["_sha256"],
        "minimum_replicates_per_arm": prereg["replicates"]["minimum_per_arm"],
    }


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
