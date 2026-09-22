#!/usr/bin/env python3
"""The causal chain, A to I, evaluated link by link.

A trial is evidence about Velra only if the whole chain holds::

    A  a large conversational state existed
    B  the critical operational information is absent from the fresh native
       state
    C  Velra retained the relevant state
    D  Velra staged it
    E  SessionStart delivered it
    F  the destination actually received it
    G  the destination agent used it
    H  the task continued correctly
    I  the input/context burden was reduced

The rule the rest of the benchmark depends on: **a link that cannot be
demonstrated makes the trial INCONCLUSIVE.** Not a Velra win with a caveat, not
a win on the links that did hold. Incomplete causal evidence is the one thing a
benchmark is most tempted to round up, so it is rounded down here by
construction — :func:`evaluate` stops at the first link that is not
demonstrated and names it.

C through G describe Velra's machinery and are ``n/a`` on the baseline arm.
``n/a`` never fails a chain; it is recorded and skipped. The baseline's chain is
A, B, H and the pair-level I, which is exactly the claim a baseline trial makes:
a large state existed, the information was not lying around, and here is what
the session cost and whether it got the work right.

I is decided at pair level, because "reduced" needs two arms. A single trial
records it as ``deferred``, and :mod:`verdict` resolves it.
"""

from __future__ import annotations

import pathlib
from typing import Sequence

if __package__ in (None, ""):
    import sys
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import parse  # type: ignore[no-redef]
    import telemetry  # type: ignore[no-redef]
else:
    from . import parse, telemetry

LINKS = ("A_large_state", "B_absent_natively", "C_retained", "D_staged",
         "E_delivered", "F_received", "G_used", "H_correct", "I_burden_reduced")

#: Human labels, for the report.
LINK_TITLE = {
    "A_large_state": "a large conversational state existed",
    "B_absent_natively": "the operational state is absent from a fresh native session",
    "C_retained": "Velra retained the relevant state",
    "D_staged": "Velra staged it",
    "E_delivered": "SessionStart delivered it",
    "F_received": "the destination actually received it",
    "G_used": "the destination agent used it",
    "H_correct": "the task continued correctly",
    "I_burden_reduced": "the input/context burden was reduced",
}

#: Links that only exist on the Velra arm.
VELRA_ONLY = ("C_retained", "D_staged", "E_delivered", "F_received", "G_used")

VALID_COMPLETE = "COMPLETE"
VALID_INCOMPLETE = "INCOMPLETE"
VALID_DEFERRED = "DEFERRED"

#: A source session has to be big enough for the scenario's claim to mean
#: anything. Below this the trial is not testing a cold continuation, it is
#: testing a short one, and the chain says so.
MIN_SOURCE_TURNS = 15


def _link(status: str, why: str, **evidence) -> dict:
    return {"status": status, "why": why, "evidence": evidence}


# --------------------------------------------------------------------------
# A -- a large conversational state existed
# --------------------------------------------------------------------------


def link_a(parsed: parse.ParsedTrial) -> dict:
    """Evidence that the source session was actually large.

    Three independent things count, and the honest one is named in the result:
    the number of source turns recorded, the context-load fixture that was
    applied, and — only if the runtime reported it — the observed context size.
    A synthetic fixture size is evidence that a large *input* was constructed;
    it is never quoted as an observed Claude context size, and this link says
    which of the two it had.
    """
    meta = parsed.meta or {}
    source = meta.get("source_session") or {}
    turns = source.get("turns")
    fixture = parsed.context_fixture or {}
    observed = [o for o in parse.context_observations(parsed.trial_dir)]

    evidence = {
        "source_session_turns": turns,
        "target_context_size": fixture.get("target_context_size"),
        "synthetic_context_size": fixture.get("synthetic_context_size"),
        "actual_observed_context_size": observed[-1]["value"] if observed else None,
        "context_generation_method": fixture.get("context_generation_method"),
        "reason_if_target_not_reached": fixture.get("reason_if_target_not_reached"),
        "size_basis": ("observed" if observed
                       else "synthetic_fixture" if fixture.get("synthetic_context_size")
                       else "turn_count_only"),
    }
    if not isinstance(turns, int):
        return _link("fail", "the trial recorded no source-session turn count",
                     **evidence)
    if turns < MIN_SOURCE_TURNS:
        return _link("fail",
                     f"the source session ran {turns} turns, below the "
                     f"registered minimum of {MIN_SOURCE_TURNS}", **evidence)
    if not (observed or fixture.get("synthetic_context_size")):
        return _link("fail",
                     "no context-load evidence: neither an observed context "
                     "size nor a load fixture was recorded", **evidence)
    return _link("pass", f"{turns} source turns with a recorded context load",
                 **evidence)


# --------------------------------------------------------------------------
# B -- absent from the fresh native state
# --------------------------------------------------------------------------


def link_b(parsed: parse.ParsedTrial) -> dict:
    """The leak scan is the evidence, and it is scenario-level.

    Deliberately *not* "the baseline failed". Reading a baseline failure as
    proof that the information was unavailable is circular, and it would also
    make a baseline that legitimately reconstructed the state look like a
    scenario defect rather than the honest result it is. What this link needs
    is that the state was not readable from the repository, the tests, git, the
    prompts or the environment — which is a property of the fixture, measured
    before any session ran.
    """
    scan = (parsed.meta.get("leak_scan") or {})
    if not scan:
        return _link("fail",
                     "no leak scan was recorded for this trial's fixture",
                     leak_scan=None)
    evidence = {
        "surfaces_scanned": scan.get("surfaces_scanned"),
        "hits_by_surface": scan.get("hits_by_surface"),
        "fatal_hits": (scan.get("fatal_hits") or [])[:5],
        "terms": (scan.get("terms") or [])[:20],
    }
    if not scan.get("clean"):
        return _link("fail",
                     "the operational state is readable from a surface the "
                     "destination session can see", **evidence)
    return _link("pass", "no surface an agent can read carries the state",
                 **evidence)


# --------------------------------------------------------------------------
# C -- retained
# --------------------------------------------------------------------------


def link_c(parsed: parse.ParsedTrial) -> dict:
    restore = parsed.restore or {}
    ledger = restore.get("ledger_evidence") or {}
    evidence = {
        "markers_present": ledger.get("markers_present"),
        "markers_missing": ledger.get("markers_missing"),
        "source_session_id": restore.get("source_session_id"),
        "restore_exit": restore.get("restore_exit"),
    }
    if not restore:
        return _link("fail", "no velra_restore.json: nothing records what the "
                             "ledger held", **evidence)
    if ledger.get("markers_missing"):
        return _link("fail",
                     "the ledger did not hold every piece of the declared "
                     "operational state: this is a CAPTURE failure, not a "
                     "delivery one", **evidence)
    if not ledger.get("markers_present"):
        return _link("fail", "the restore recorded no ledger evidence at all",
                     **evidence)
    return _link("pass", "every declared marker was in the ledger before the "
                         "capsule was built", **evidence)


# --------------------------------------------------------------------------
# D -- staged
# --------------------------------------------------------------------------


def link_d(parsed: parse.ParsedTrial) -> dict:
    """The staged record, checked against the things that make it deliverable.

    A capsule staged for the wrong workspace, or one that has aged past its
    TTL, is a staging failure even though a file exists — and both are cases
    the benchmark has to be able to tell apart from "nothing was staged", which
    is why they are separate reasons rather than one boolean.
    """
    restore = parsed.restore or {}
    staged = restore.get("staged") or {}
    evidence = {
        "staged_path": restore.get("staged_path"),
        "workspace_id": staged.get("workspace_id"),
        "expected_workspace_id": restore.get("workspace_id"),
        "intent": staged.get("intent"),
        "deliver_on": staged.get("deliver_on"),
        "source_session_id": staged.get("source_session_id"),
        "tokens": staged.get("tokens"),
        "content_hash": staged.get("content_hash"),
        "stale": restore.get("stale"),
        "age_ms": restore.get("age_ms"),
    }
    if not staged:
        return _link("fail", "`velra restore` staged nothing", **evidence)
    if restore.get("workspace_id") and staged.get("workspace_id") \
            and restore["workspace_id"] != staged["workspace_id"]:
        return _link("fail",
                     "the staged capsule belongs to a different workspace than "
                     "the destination session's", **evidence)
    if restore.get("stale"):
        return _link("fail", "the staged capsule was past its TTL when the "
                             "destination session started", **evidence)
    if "startup" not in (staged.get("deliver_on") or []):
        return _link("fail",
                     "the staged capsule is not eligible for SessionStart "
                     "startup delivery", **evidence)
    if not staged.get("capsule"):
        return _link("fail", "the staged record carries no capsule text",
                     **evidence)
    return _link("pass", "a startup-eligible capsule was staged for this "
                         "workspace", **evidence)


# --------------------------------------------------------------------------
# E -- SessionStart delivered it
# --------------------------------------------------------------------------


def link_e(parsed: parse.ParsedTrial) -> dict:
    restore = parsed.restore or {}
    claim = restore.get("claim") or {}
    attempts = claim.get("attempts") or []
    claimed = [a for a in attempts if a.get("claimed")]
    evidence = {"attempts": attempts[:6], "claimed_count": len(claimed)}

    startup = [d for d in parsed.deliveries if d.on_startup]
    evidence["startup_deliveries"] = [d.to_json() for d in startup]
    evidence["all_deliveries"] = [d.to_json() for d in parsed.deliveries]

    if not parsed.deliveries:
        return _link("fail", "no hook response carrying a capsule appears in "
                             "the destination capture", **evidence)
    if not startup:
        labels = sorted({(d.session_start_source or d.hook_name or "?")
                         for d in parsed.deliveries})
        return _link("fail",
                     f"a capsule was delivered, but not on SessionStart "
                     f"startup: {labels}", **evidence)
    if attempts and len(claimed) > 1:
        return _link("fail",
                     f"the staged capsule was claimed {len(claimed)} times; "
                     f"delivery must be exactly once", **evidence)
    bad_exit = [d.to_json() for d in parsed.deliveries
                if d.exit_code not in (0, None)]
    if bad_exit:
        return _link("fail", "a delivering hook exited non-zero",
                     failed_exits=bad_exit, **evidence)
    return _link("pass", "SessionStart(startup) emitted the capsule", **evidence)


# --------------------------------------------------------------------------
# F -- the destination received it
# --------------------------------------------------------------------------


def link_f(parsed: parse.ParsedTrial, manifest: dict) -> dict:
    """Received once, by a session that is genuinely a different session.

    The transcript-replay check is the one that makes this benchmark about what
    it claims to be about. If the destination's capture contains the old
    conversation, the comparison is not "fresh session plus capsule" against
    "large native continuation", and no token difference means anything.
    """
    startup = [d for d in parsed.deliveries if d.on_startup]
    markers = (manifest.get("capsule_markers") or [])
    text = startup[0].text if startup else ""
    low = text.replace("\\", "/").lower()
    found = [m for m in markers if m.replace("\\", "/").lower() in low]
    missing = [m for m in markers if m not in found]

    source_id = parsed.source_session_id
    dest_id = parsed.destination_session_id
    budget = manifest.get("capsule_token_ceiling")

    evidence = {
        "delivery_count": len(startup),
        "capsule_chars": startup[0].chars if startup else 0,
        "markers_found": found,
        "markers_missing": missing,
        "source_session_id": source_id,
        "destination_session_id": dest_id,
        "capsule_token_ceiling": budget,
        "old_transcript_replayed": bool(parsed.meta.get("old_transcript_replayed")),
    }
    if not startup:
        return _link("fail", "the destination capture holds no startup capsule",
                     **evidence)
    if len(startup) != 1:
        return _link("fail",
                     f"the destination received {len(startup)} startup "
                     f"capsules; exactly one is the contract", **evidence)
    if not source_id or not dest_id:
        return _link("fail", "the trial does not record both session ids",
                     **evidence)
    if source_id == dest_id:
        return _link("fail",
                     "the destination session id equals the source's: this is "
                     "not a new session", **evidence)
    if parsed.meta.get("old_transcript_replayed"):
        return _link("fail",
                     "the old conversation was replayed into the destination; "
                     "the comparison is void", **evidence)
    if missing:
        return _link("fail",
                     "the delivered bytes did not carry every declared piece "
                     "of operational state", **evidence)
    return _link("pass", "one bounded capsule, in a genuinely new session",
                 **evidence)


# --------------------------------------------------------------------------
# G -- used
# --------------------------------------------------------------------------


def link_g(evaluation: dict) -> dict:
    """Used, judged by what the agent did rather than by what it narrated.

    Either the destination identified the declared operational state, or its
    first action was the declared correct one. Requiring prose acknowledgement
    would score writing style; requiring both would fail an agent that went
    straight to the right file without commentary, which is the behaviour the
    capsule is supposed to produce.
    """
    recovery = evaluation.get("state_recovery") or {}
    first = evaluation.get("first_correct_action") or {}
    evidence = {
        "identified_items": recovery.get("identified_items"),
        "declared_items": recovery.get("declared_items"),
        "per_item": recovery.get("per_item"),
        "first_correct_action": first,
    }
    if not recovery.get("declared_items") and not first.get("declared"):
        return _link("fail", "the scenario declared nothing to check use "
                             "against", **evidence)
    if recovery.get("complete") or first.get("found"):
        return _link("pass", "the destination acted on the restored state",
                     **evidence)
    return _link("fail", "the destination neither identified the restored "
                         "state nor took the declared correct action",
                 **evidence)


# --------------------------------------------------------------------------
# H -- correct
# --------------------------------------------------------------------------


def link_h(evaluation: dict) -> dict:
    correctness = evaluation.get("final_correctness") or {}
    evidence = {"checks": correctness.get("checks"),
                "declared_checks": correctness.get("declared_checks")}
    if not correctness.get("evaluable"):
        return _link("fail", "final correctness could not be evaluated from "
                             "the captured end state", **evidence)
    if not correctness.get("correct"):
        return _link("fail", "the task was not completed correctly", **evidence)
    return _link("pass", "the task was completed correctly", **evidence)


# --------------------------------------------------------------------------
# the chain
# --------------------------------------------------------------------------


def evaluate(parsed: parse.ParsedTrial, evaluation: dict) -> dict:
    """Every link for one trial, and where the chain first broke."""
    manifest = parsed.meta.get("manifest") or {}
    velra = parsed.arm == "velra"

    links: dict[str, dict] = {}
    links["A_large_state"] = link_a(parsed)
    links["B_absent_natively"] = link_b(parsed)
    for name, fn in (("C_retained", lambda: link_c(parsed)),
                     ("D_staged", lambda: link_d(parsed)),
                     ("E_delivered", lambda: link_e(parsed)),
                     ("F_received", lambda: link_f(parsed, manifest)),
                     ("G_used", lambda: link_g(evaluation))):
        links[name] = fn() if velra else _link(
            "n/a", "this link describes Velra's machinery; the baseline arm "
                   "has none")
    links["H_correct"] = link_h(evaluation)
    links["I_burden_reduced"] = _link(
        "deferred", "'reduced' needs two arms; resolved by the pair verdict")

    broken = None
    for name in LINKS:
        if links[name]["status"] == "fail":
            broken = name
            break

    if broken is not None:
        validity = VALID_INCOMPLETE
    elif links["I_burden_reduced"]["status"] == "deferred":
        validity = VALID_DEFERRED
    else:
        validity = VALID_COMPLETE

    return {
        "links": links,
        "first_broken_link": broken,
        "causal_validity": validity,
        "summary": ("; ".join(
            f"{name}={links[name]['status']}" for name in LINKS)),
    }


def report(chain: dict) -> str:
    lines = [f"causal validity: {chain['causal_validity']}"
             + (f" (broken at {chain['first_broken_link']})"
                if chain["first_broken_link"] else "")]
    for name in LINKS:
        link = chain["links"][name]
        mark = {"pass": "PASS", "fail": "FAIL", "n/a": " -- ",
                "deferred": "....."}[link["status"]]
        lines.append(f"  {mark}  {name}: {LINK_TITLE[name]}")
        if link["status"] == "fail":
            lines.append(f"          {link['why']}")
    return "\n".join(lines)
