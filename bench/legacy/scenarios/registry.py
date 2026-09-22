#!/usr/bin/env python3
"""Scenario lookup, and a CLI for generating one fixture.

    python bench/legacy/scenarios/registry.py --list
    python bench/legacy/scenarios/registry.py s1-dead-end-pair /tmp/fx

Generating a fixture runs its ground-truth verification and its leak scan, so
this command is also how the scenarios are checked without spending anything
on a live session.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

if __package__ in (None, ""):
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from scenarios import s1_dead_end_pair, s2_hidden_constraint, s3_working_set
    from scenarios.base import Scenario
else:
    from . import s1_dead_end_pair, s2_hidden_constraint, s3_working_set
    from .base import Scenario

SCENARIOS: dict[str, Scenario] = {
    s.name: s for s in (
        s1_dead_end_pair.SCENARIO,
        s2_hidden_constraint.SCENARIO,
        s3_working_set.SCENARIO,
    )
}

DEFAULT_ORDER = tuple(SCENARIOS)


def get(name: str) -> Scenario:
    try:
        return SCENARIOS[name]
    except KeyError:
        raise SystemExit(
            f"unknown scenario {name!r}; known: {', '.join(SCENARIOS)}") from None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("scenario", nargs="?")
    ap.add_argument("destination", nargs="?")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--no-verify", action="store_true",
                    help="skip the ground-truth run and the leak scan")
    args = ap.parse_args()

    if args.list or not args.scenario:
        for name, scenario in SCENARIOS.items():
            print(f"{name:22} {scenario.mechanism:24} {scenario.title}")
            print(f"{'':22} turns={len(scenario.turns)} "
                  f"compact@{scenario.compact_index} "
                  f"measured@{scenario.measured_index} "
                  f"initial-suite={scenario.expect_initial}")
        return 0

    if not args.destination:
        ap.error("destination is required when a scenario is named")
    scenario = get(args.scenario)
    manifest = scenario.build(pathlib.Path(args.destination),
                              verify=not args.no_verify)
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
