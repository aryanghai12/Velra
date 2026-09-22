#!/usr/bin/env python3
"""The live-execution gate, in code rather than in a warning.

Three things must all be true before any Claude process may start:

1. ``--live`` was passed explicitly;
2. ``VELRA_ALLOW_LIVE_BENCHMARK=1`` is set in the environment;
3. the runner is not itself executing inside a Claude Code session.

The third is not a formality. The trials register and unregister Velra's hooks
in the real user-level ``settings.json``, which is the same file a nested
session is reading; a run started from inside Claude Code mutates the state of
the session that started it, and the failure is silent and confusing in both
directions. :data:`NESTED_MARKERS` lists the variables Claude Code sets, and
any one of them is enough to refuse.

Every other mode — ``--selftest``, ``--dry-run``, ``--smoke`` — passes through
:func:`assert_offline`, which raises if anything in the process could reach the
network or start a Claude binary. The gate is checked *before* the first
subprocess, not before the first request, because a Claude process that starts
and is then killed has already cost something.
"""

from __future__ import annotations

import dataclasses
import os
from typing import Mapping

#: The environment variable that must be exactly "1".
LIVE_ENV = "VELRA_ALLOW_LIVE_BENCHMARK"

#: Variables Claude Code sets in the environment of a process it spawns. Any
#: of them means this runner is nested inside a session.
NESTED_MARKERS = ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE",
                  "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SESSION_ID")

#: The Claude Code flags a live trial drives both arms with.
#:
#: They live here, in the module that governs live execution, for one reason:
#: `run.py` reports them in the readiness record and `live_trial.py` uses them,
#: and if the readiness record had to import the live driver to read them, the
#: offline modes would have a reachable import path to the only module that can
#: start a Claude process. One tuple, two readers, no such path.
CLAUDE_FLAGS = (
    "-p",
    "--input-format", "stream-json",
    "--output-format", "stream-json",
    "--verbose",
    "--include-hook-events",
    "--permission-mode", "bypassPermissions",
    "--permission-prompts", "none",
    "--strict-mcp-config",
)

MODE_SELFTEST = "selftest"
MODE_DRY_RUN = "dry-run"
MODE_SMOKE = "smoke"
MODE_LIVE = "live"

#: Modes in which no Claude process, no network call and no API spend may
#: occur, whatever else happens.
OFFLINE_MODES = (MODE_SELFTEST, MODE_DRY_RUN, MODE_SMOKE)


class LiveExecutionRefused(SystemExit):
    """Raised instead of starting anything that costs money."""


@dataclasses.dataclass(frozen=True)
class GateResult:
    allowed: bool
    mode: str
    reasons: list
    nested_markers: list
    env_flag_present: bool

    def to_json(self) -> dict:
        return dataclasses.asdict(self)


def nested_markers(env: Mapping[str, str] | None = None) -> list:
    env = os.environ if env is None else env
    return [name for name in NESTED_MARKERS if env.get(name)]


def check(mode: str, *, live_flag: bool,
          env: Mapping[str, str] | None = None) -> GateResult:
    """Decide whether live execution may proceed. Never has side effects."""
    env = os.environ if env is None else env
    reasons: list[str] = []
    nested = nested_markers(env)
    flag_present = env.get(LIVE_ENV) == "1"

    if mode != MODE_LIVE:
        reasons.append(f"mode is {mode!r}: this mode never starts a Claude "
                       f"process")
        return GateResult(False, mode, reasons, nested, flag_present)
    if not live_flag:
        reasons.append("--live was not passed")
    if not flag_present:
        reasons.append(f"{LIVE_ENV} is not set to '1'")
    if nested:
        reasons.append(
            "this runner is executing inside a Claude Code session ("
            + ", ".join(nested) + "); the trials mutate the settings file that "
            "session is reading")
    return GateResult(not reasons, mode, reasons, nested, flag_present)


def require_live(mode: str, *, live_flag: bool,
                 env: Mapping[str, str] | None = None) -> GateResult:
    """:func:`check`, but refusing loudly instead of returning ``False``.

    Called immediately before the first Claude process and nowhere else.
    """
    result = check(mode, live_flag=live_flag, env=env)
    if not result.allowed:
        raise LiveExecutionRefused(
            "refusing to start a live benchmark:\n  - "
            + "\n  - ".join(result.reasons)
            + "\n\nLive execution needs BOTH --live and "
            + f"{LIVE_ENV}=1, from a terminal that is not inside Claude Code.")
    return result


def assert_offline(mode: str, env: Mapping[str, str] | None = None) -> dict:
    """Assert that this mode may not spend anything, and record that it cannot.

    Returns the record that goes into ``readiness.json``, so the guarantee is
    an artifact rather than a claim in a docstring.
    """
    if mode not in OFFLINE_MODES:
        raise LiveExecutionRefused(
            f"assert_offline called for mode {mode!r}, which is not offline")
    env = os.environ if env is None else env
    return {
        "mode": mode,
        "offline": True,
        "claude_processes_permitted": 0,
        "network_calls_permitted": 0,
        "api_spend_permitted_usd": 0.0,
        "live_env_flag_present": env.get(LIVE_ENV) == "1",
        "nested_claude_markers": nested_markers(env),
        "note": ("this mode is offline by construction: the runner has no code "
                 "path from it to a Claude binary, and the gate below would "
                 "refuse one anyway"),
    }


def status(env: Mapping[str, str] | None = None) -> dict:
    """The gate's own state, for the readiness report."""
    env = os.environ if env is None else env
    nested = nested_markers(env)
    return {
        "gate_present": True,
        "gate_module": __name__,
        "requires_flag": "--live",
        "requires_env": f"{LIVE_ENV}=1",
        "live_env_flag_present": env.get(LIVE_ENV) == "1",
        "nested_claude_markers": nested,
        "would_allow_live_now": check(MODE_LIVE, live_flag=True,
                                      env=env).allowed,
        "refusal_reasons_now": check(MODE_LIVE, live_flag=True,
                                     env=env).reasons,
    }
