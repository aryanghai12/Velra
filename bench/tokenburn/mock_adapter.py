#!/usr/bin/env python3
"""MockClaudeAdapter: deterministic synthetic trials in the live artifact shape.

The rule this module exists to serve, from §2 of the Phase 3 specification:

    The MockClaudeAdapter MUST feed the SAME parser, telemetry extraction,
    aggregation, pairing, metrics and verdict logic used by the eventual live
    benchmark.

So it does not evaluate anything. It writes files — ``stream.jsonl``,
``transcript.jsonl``, ``usage.json``, ``velra_restore.json``,
``final_state.json``, ``context_fixture.json``, ``trial_meta.json`` — into a
trial directory, in exactly the layout :mod:`live_trial` writes, and
:mod:`aggregate` runs over the result without knowing which produced it. There
is no second evaluator anywhere in this package, and
``test_tokenburn_pipeline.py`` asserts that the two writers agree on the
artifact set.

What it can model
-----------------

Session creation, a source session and a distinct destination session, user
turns, assistant turns, tool calls, tool results, context growth, compaction,
``/clear``, resume, ``SessionStart`` events, ``velra restore``, staged capsule
creation, capsule injection, cache state, input and output token usage, and the
final task outcome. Each is a field on :class:`MockSpec`, and the eighteen
scripted cases in :mod:`selftest` are eighteen values of that dataclass.

Determinism
-----------

Everything varying is drawn from ``random.Random(spec.seed_string())``, which
hashes the trial name, the scenario and the seed. The same spec produces
byte-identical artifacts on any machine, which is what makes a failing selftest
reproducible rather than a story about a flaky run.

Honesty
-------

Synthetic usage is written with ``_telemetry_source:
"synthetic_fixture"`` on every event that carries a ``usage`` object, so the
provenance that reaches the report says ``synthetic_fixture`` and not
``structured_usage``. A number this module invented can never be mistaken
downstream for a number Claude reported — and a selftest that produced
``structured_usage`` provenance would be lying about the one thing the
telemetry design exists to guarantee.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import pathlib
import random
import sys
from typing import Sequence

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import isolation  # type: ignore[no-redef]
    import prereg  # type: ignore[no-redef]
    import scenarios  # type: ignore[no-redef]
    import telemetry  # type: ignore[no-redef]
else:
    from . import isolation, prereg, scenarios, telemetry

#: Written into every artifact this module produces, so a synthetic trial
#: directory can never be mistaken for a live one on disk either.
SYNTHETIC_MARK = "MockClaudeAdapter"

#: A rendered capsule, in the shape `velra restore` stages. The body is filled
#: from the scenario's declared operational state, so the markers the causal
#: chain checks for are the ones the scenario registered and not a copy that
#: could drift from them.
CAPSULE_TEMPLATE = (
    '<VELRA_WORKSPACE_STATE v="1" checkpoint="{checkpoint}" '
    'captured="{captured}" trigger="restore">\n'
    "[ABOUT_THIS_RECORD]\n"
    "Velra is a local tool that recorded this task state. It was staged by "
    "`velra restore` from session {source_short} and delivered at "
    "SessionStart.\n"
    "[ROOT_TASK_OBJECTIVE] (OBSERVED | user prompt)\n{objective}\n"
    "[ACTIVE_FAILURE] (OBSERVED | test run)\n{failure}\n"
    "[WORKING_FILES] (OBSERVED)\n{files}\n"
    "[IMPORTANT_DECISIONS] (OBSERVED | user prompt)\n{decisions}\n"
    "[REVERTED_EDITS] (OBSERVED)\n{reverted}\n"
    "[NEXT_ACTION] (OBSERVED | user prompt)\n{next_action}\n"
    "[RECORD_DETAIL]\nFull detail for any section: `velra inspect`\n"
    "</VELRA_WORKSPACE_STATE>"
)


# --------------------------------------------------------------------------
# the spec
# --------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class MockSpec:
    """One synthetic trial, described entirely by data.

    Every knob is here rather than in the generator, so a selftest case reads
    as a statement about what is being modelled -- ``emit_cache_fields=False``,
    ``injections=2``, ``wrong_workspace=True`` -- and the eighteen required
    cases are eighteen of these.
    """

    trial: str
    scenario: str
    pair_id: str
    arm: str                                # "baseline" | "velra"
    replicate: int = 1
    seed: int = 0
    model: str = "mock-sonnet"
    claude_version: str = "mock-0.0.0"
    permission_mode: str = "bypassPermissions"

    # -- source session and context load ---------------------------------
    source_turns: int = 18
    ladder_rung: int = 250_000
    synthetic_context_size: int | None = 250_000
    emit_context_observation: bool = False
    observed_context_size: int | None = None
    reason_if_target_not_reached: str | None = None

    # -- transition -------------------------------------------------------
    #: "new_session" (Benchmark A), "clear" (Benchmark B) or "resume".
    transition: str = "new_session"
    same_session_id: bool = False
    old_transcript_replayed: bool = False
    emit_compaction: bool = False

    # -- telemetry --------------------------------------------------------
    emit_usage: bool = True
    usage_in_rollup: bool = False
    usage_in_transcript_only: bool = False
    emit_cache_fields: bool = True
    #: Drop the cache fields from this many usage events, leaving the rest --
    #: the partial-field case, which must report UNAVAILABLE rather than a
    #: partial sum.
    cache_fields_missing_from: int = 0
    cache_hit: bool = True
    input_tokens_per_turn: int = 4_000
    output_tokens_per_turn: int = 600
    cache_read_per_turn: int = 90_000
    cache_creation_per_turn: int = 0

    # -- capture health ---------------------------------------------------
    malformed_lines: int = 0
    duplicate_events: int = 0
    malformed_fraction_override: float | None = None

    # -- Velra machinery --------------------------------------------------
    restore_invoked: bool = True
    restore_exit: int = 0
    ledger_missing_markers: Sequence[str] = ()
    staged: bool = True
    stale_capsule: bool = False
    wrong_workspace: bool = False
    deliver_on: Sequence[str] = ("startup",)
    delivery_source: str = "startup"
    injections: int = 1
    claim_successes: int = 1
    claim_attempts: int = 1
    capsule_markers_present: bool = True
    capsule_tokens: int = 612
    delivery_exit_code: int = 0

    # -- behaviour and outcome --------------------------------------------
    uses_state: bool = True
    correct: bool = True
    turns: int = 4
    reads_before_target: int = 0
    extra_reads: int = 0
    extra_searches: int = 0
    touches_dead_end_first: bool = False
    edits_forbidden_file: bool = False

    # -- fixture-level ----------------------------------------------------
    leak_clean: bool = True

    # -- trial validity (isolation.trial_validity) -------------------------
    #: Both memory controls verified in force.
    auto_memory_disabled: bool = True
    #: A file in the fixture's auto-memory directory before the destination.
    memory_leak: bool = False
    #: The target test's status at the source handoff: "FAIL" is the scenario.
    handoff_target: str = "FAIL"
    handoff_invariant_as_generated: bool = True
    handoff_worktree_violations: Sequence[str] = ()

    def validity(self) -> tuple[dict, dict, dict]:
        """(handoff, memory scan, trial_validity), exactly as the live driver
        derives them -- the same `isolation.trial_validity` call."""
        reasons = []
        if self.handoff_target != "FAIL":
            reasons.append(f"target test is {self.handoff_target} at handoff")
        if not self.handoff_invariant_as_generated:
            reasons.append("invariant not as generated")
        reasons.extend(f"worktree: {v}" for v in self.handoff_worktree_violations)
        handoff = {
            "source_handoff_valid": not reasons,
            "source_handoff_reason": "; ".join(reasons) if reasons
            else "target FAIL, invariant as generated, worktree as generated",
            "target_status_at_handoff": self.handoff_target,
            "invariant_status_at_handoff": {
                "holds": self.handoff_invariant_as_generated, "expected": True,
                "matches_expected": self.handoff_invariant_as_generated},
            "worktree_violations": list(self.handoff_worktree_violations),
        }
        scan = {"clean": not self.memory_leak,
                "files": ["MEMORY.md"] if self.memory_leak else []}
        validity = isolation.trial_validity(
            auto_memory_disabled=self.auto_memory_disabled,
            memory_scan_clean=scan["clean"], handoff=handoff)
        return handoff, scan, validity

    def seed_string(self) -> str:
        return f"{SYNTHETIC_MARK}|{self.scenario}|{self.trial}|{self.seed}"

    def rng(self) -> random.Random:
        return random.Random(self.seed_string())

    def session_ids(self) -> tuple[str, str]:
        """Source and destination ids, derived from the spec, not random.

        ``same_session_id`` is the case where the "new" session is not new;
        the causal chain has to catch it, so the adapter has to be able to
        produce it.
        """
        digest = hashlib.sha256(self.seed_string().encode("utf-8")).hexdigest()
        source = f"src-{digest[:12]}"
        destination = source if self.same_session_id else f"dst-{digest[12:24]}"
        return source, destination


# --------------------------------------------------------------------------
# capsule
# --------------------------------------------------------------------------


def render_capsule(spec: MockSpec, manifest: dict, source_id: str) -> str:
    """A capsule carrying the scenario's declared operational state.

    When ``capsule_markers_present`` is false the declared markers are replaced
    with a plausible but wrong working set -- the delivery-succeeded-but-carried-
    the-wrong-thing case, which is a different defect from no delivery at all
    and has to be distinguishable in the report.
    """
    recovery = manifest.get("state_recovery") or {}
    if spec.capsule_markers_present:
        objective = (f"Fix {manifest.get('target_test')} without introducing "
                     f"process-wide state.")
        failure = f"FAILED {manifest.get('target_test')}"
        files = "\n".join(f"- {f}" for f in (recovery.get("relevant_files") or []))
        reverted = (f"- {(manifest.get('dead_end') or {}).get('description')}")
        next_action = " ".join(recovery.get("next_action") or [])
        held = (manifest.get("invariant") or {}).get("must_contain") or []
        forbidden = (manifest.get("invariant") or {}).get("must_not_contain") or []
        stated = []
        if held:
            stated.append("- must hold: " + ", ".join(held))
        if forbidden:
            stated.append("- must not reappear: " + ", ".join(forbidden))
        decisions = "\n".join(stated) or "- none recorded"
    else:
        objective = "Continue the ledger fee rounding work."
        failure = "FAILED tests/test_ledger.py::test_apply_fee_on_empty_total"
        files = "- src/payments/ledger.py"
        reverted = "- nothing recorded"
        next_action = "inspect apply_fee"
        decisions = "- none recorded"
    return CAPSULE_TEMPLATE.format(
        checkpoint=f"ckpt_{hashlib.sha256(spec.seed_string().encode()).hexdigest()[:24]}",
        captured="2026-09-20T10:00:00Z",
        source_short=source_id[:8],
        objective=objective, failure=failure, files=files,
        decisions=decisions, reverted=reverted, next_action=next_action)


# --------------------------------------------------------------------------
# stream construction
# --------------------------------------------------------------------------


def _usage(spec: MockSpec, index: int) -> dict:
    """One synthetic usage object, with the fields the spec says exist."""
    usage: dict = {
        "input_tokens": spec.input_tokens_per_turn,
        "output_tokens": spec.output_tokens_per_turn,
    }
    drop_cache = (not spec.emit_cache_fields) or index < spec.cache_fields_missing_from
    if not drop_cache:
        usage["cache_read_input_tokens"] = (
            spec.cache_read_per_turn if spec.cache_hit else 0)
        usage["cache_creation_input_tokens"] = (
            spec.cache_creation_per_turn if spec.cache_hit
            else spec.cache_read_per_turn)
    return usage


def _tool(name: str, uid: str, **inputs) -> dict:
    return {"type": "tool_use", "id": uid, "name": name, "input": inputs}


def _assistant(session_id: str, uid: str, text: str,
               tools: Sequence[dict]) -> dict:
    return {"type": "assistant", "session_id": session_id, "uuid": uid,
            "message": {"id": f"msg_{uid}", "role": "assistant",
                        "content": [{"type": "text", "text": text},
                                    *tools]}}


def _tool_result(session_id: str, uid: str, tool_id: str, text: str) -> dict:
    return {"type": "user", "session_id": session_id, "uuid": uid,
            "message": {"role": "user",
                        "content": [{"type": "tool_result",
                                     "tool_use_id": tool_id,
                                     "content": text}]}}


def _hook_response(session_id: str, uid: str, capsule: str, source: str,
                   exit_code: int, summary: str) -> dict:
    payload = {
        "hookSpecificOutput": {"hookEventName": "SessionStart",
                               "additionalContext": capsule},
        "systemMessage": summary,
    }
    return {"type": "system", "subtype": "hook_response", "uuid": uid,
            "hook_id": uid, "hook_name": f"SessionStart:{source}",
            "hook_event": "SessionStart", "session_start_source": source,
            "exit_code": exit_code, "stdout": json.dumps(payload),
            "stderr": "", "session_id": session_id}


def _actions_for(spec: MockSpec, manifest: dict) -> list[tuple[str, str, str]]:
    """The tool calls the destination makes, as (tool, target, result).

    The order encodes the behaviour being modelled: a session that used the
    restored state goes to the declared file first; one that did not searches
    around, reads the wrong files, and may touch the dead end on the way.
    """
    recovery = manifest.get("state_recovery") or {}
    target_file = (recovery.get("relevant_files") or ["src/payments/retry.py"])[0]
    forbidden = ((manifest.get("dead_end") or {}).get("must_not_edit")
                 or ["src/payments/ledger.py"])
    out: list[tuple[str, str, str]] = []

    if spec.touches_dead_end_first:
        out.append(("Read", forbidden[0], "… 40 lines …"))
    for index in range(spec.reads_before_target):
        out.append(("Read", forbidden[index % len(forbidden)], "… 60 lines …"))
    for _ in range(spec.extra_searches):
        out.append(("Grep", "value_date|make_key", "12 matches"))

    if spec.uses_state:
        out.append(("Read", target_file, "…the function under repair…"))
        out.append(("Edit", target_file, "applied"))
    else:
        out.append(("Bash", "python -m pytest -q", "3 failed"))
        out.append(("Read", "README.md", "…"))

    for _ in range(spec.extra_reads):
        out.append(("Read", "src/payments/feed.py", "…"))
    if spec.edits_forbidden_file:
        out.append(("Edit", forbidden[0], "applied"))
    out.append(("Bash", "python -m pytest -q " + str(manifest.get("target_test")),
                "1 passed" if spec.correct else "1 failed"))
    return out


def build_stream(spec: MockSpec, manifest: dict, source_id: str,
                 destination_id: str, capsule: str) -> list[str]:
    """The destination session's capture, as stream-json lines."""
    lines: list[str] = []
    rng = spec.rng()
    continuation = manifest.get("continuation_prompt") or "Continue."

    lines.append(json.dumps({"_velra_bench": "turn_start", "turn": 0,
                             "text": continuation, "t": 0.0}))
    lines.append(json.dumps({
        "type": "system", "subtype": "init", "uuid": f"init-{destination_id}",
        "session_id": destination_id, "source": (
            "startup" if spec.transition == "new_session"
            else "clear" if spec.transition == "clear" else "resume"),
        "_synthetic": SYNTHETIC_MARK}))

    if spec.emit_compaction:
        lines.append(json.dumps({
            "type": "system", "subtype": "compact_boundary",
            "uuid": f"compact-{destination_id}",
            "compact_result": "success", "session_id": destination_id}))

    if spec.arm == "velra" and spec.injections:
        for index in range(spec.injections):
            lines.append(json.dumps(_hook_response(
                destination_id, f"hook-{index}-{destination_id}", capsule,
                spec.delivery_source, spec.delivery_exit_code,
                f"⚡ Velra restored task state from session "
                f"{source_id[:8]} ({spec.capsule_tokens} tokens)")))

    if spec.old_transcript_replayed:
        lines.append(json.dumps({
            "type": "user", "session_id": destination_id,
            "uuid": f"replay-{destination_id}",
            "message": {"role": "user",
                        "content": [{"type": "text",
                                     "text": "…full prior transcript…"}]}}))

    actions = _actions_for(spec, manifest)
    per_turn = max(1, -(-len(actions) // max(1, spec.turns)))
    uid = 0
    for turn in range(spec.turns):
        chunk = actions[turn * per_turn:(turn + 1) * per_turn]
        tools = []
        for tool, target, _result in chunk:
            uid += 1
            key = ("command" if tool == "Bash"
                   else "pattern" if tool == "Grep" else "file_path")
            tools.append(_tool(tool, f"toolu_{uid}", **{key: target}))
        prose = ("Continuing the task recorded in the workspace state."
                 if spec.uses_state and spec.arm == "velra"
                 else "Let me work out where this was left.")
        uid += 1
        lines.append(json.dumps(_assistant(destination_id, f"a{uid}", prose,
                                           tools)))
        for (tool, _target, result), block in zip(chunk, tools):
            uid += 1
            lines.append(json.dumps(_tool_result(
                destination_id, f"r{uid}", block["id"], result)))

        result_event: dict = {
            "type": "result", "subtype": "success", "uuid": f"res-{turn}",
            "session_id": destination_id,
            "duration_ms": rng.randint(4_000, 30_000),
            "num_turns": 1,
        }
        if spec.emit_usage and not spec.usage_in_rollup \
                and not spec.usage_in_transcript_only:
            result_event["usage"] = _usage(spec, turn)
            result_event["_telemetry_source"] = telemetry.SOURCE_SYNTHETIC_FIXTURE
        if spec.emit_context_observation:
            result_event["context_size"] = (
                spec.observed_context_size
                if spec.observed_context_size is not None
                else spec.ladder_rung)
            result_event["_telemetry_source"] = telemetry.SOURCE_SYNTHETIC_FIXTURE
        lines.append(json.dumps(result_event))

    for index in range(spec.duplicate_events):
        # A retried connection repeats an event verbatim, uuid included. The
        # parser must key on that uuid and drop it; if it ever stopped doing
        # so, this trial's token total would silently inflate.
        lines.append(lines[-1])

    for index in range(spec.malformed_lines):
        lines.append('{"type": "assistant", "message": {"content": [' * (index + 1))

    return lines


def build_transcript(spec: MockSpec, destination_id: str) -> list[str]:
    """The session artifact. Carries usage only in the transcript-only case."""
    lines = []
    for turn in range(spec.turns):
        entry: dict = {
            "type": "assistant", "sessionId": destination_id,
            "uuid": f"t{turn}-{destination_id}",
            "message": {"role": "assistant", "content": "…"},
        }
        if spec.usage_in_transcript_only and spec.emit_usage:
            entry["message"]["usage"] = _usage(spec, turn)
        lines.append(json.dumps(entry))
    return lines


# --------------------------------------------------------------------------
# the other artifacts
# --------------------------------------------------------------------------


def build_restore(spec: MockSpec, manifest: dict, source_id: str,
                  capsule: str) -> dict:
    """``velra_restore.json``: the restore, staging and claim lifecycle."""
    markers = list(manifest.get("capsule_markers") or [])
    missing = [m for m in markers if m in set(spec.ledger_missing_markers)]
    present = [m for m in markers if m not in set(missing)]
    workspace = "ws-" + hashlib.sha256(
        spec.scenario.encode("utf-8")).hexdigest()[:16]
    staged_workspace = workspace + ("-other" if spec.wrong_workspace else "")

    staged = None
    if spec.staged:
        staged = {
            "version": 2,
            "workspace_id": staged_workspace,
            "workspace_root": f"/fixtures/{spec.scenario}",
            "intent": "new_session",
            "deliver_on": list(spec.deliver_on),
            "source_session_id": source_id,
            "source_checkpoint_id": f"ckpt_{source_id[-8:]}",
            "created_ms": 1_790_000_000_000,
            "render_version": 1,
            "tokens": spec.capsule_tokens,
            "content_hash": hashlib.blake2b(
                capsule.encode("utf-8"), digest_size=16).hexdigest(),
            "summary": "continue the payments task",
            "capsule": capsule,
        }
    attempts = []
    for index in range(spec.claim_attempts):
        claimed = index < spec.claim_successes
        attempts.append({
            "source": spec.delivery_source,
            "claimed": claimed,
            "reason": None if claimed else "already claimed by another consumer",
        })
    return {
        "_synthetic": SYNTHETIC_MARK,
        "restore_invoked": spec.restore_invoked,
        "restore_exit": spec.restore_exit,
        "source_session_id": source_id,
        "workspace_id": workspace,
        "workspace_root": f"/fixtures/{spec.scenario}",
        "staged_path": f"$VELRA_HOME/staged/{staged_workspace}/staged_capsule",
        "ledger_evidence": {"markers_present": present,
                            "markers_missing": missing},
        "staged": staged,
        "stale": spec.stale_capsule,
        "age_ms": (8 * 24 * 3600 * 1000) if spec.stale_capsule else 60_000,
        "ttl_ms": 7 * 24 * 3600 * 1000,
        "claim": {"attempts": attempts},
    }


def build_final_state(spec: MockSpec, manifest: dict) -> dict:
    """``final_state.json``: the end of the destination session's work."""
    correctness = manifest.get("final_correctness") or {}
    required = correctness.get("required_final_state") or []
    files: dict[str, str] = {}
    for entry in required:
        rel = entry.get("file")
        good = "\n".join(entry.get("must_contain") or ["# repaired"])
        bad = "\n".join(entry.get("must_not_contain") or ["# broken"])
        files[rel] = good if spec.correct else bad
    invariant = manifest.get("invariant") or {}
    rel = invariant.get("must_hold_in_file")
    if rel:
        text = files.get(rel, "")
        additions = ("\n".join(invariant.get("must_contain") or [])
                     if spec.correct
                     else "\n".join(invariant.get("must_not_contain") or []))
        files[rel] = (text + "\n" + additions).strip()
    ceiling = correctness.get("max_suite_failures")
    return {
        "_synthetic": SYNTHETIC_MARK,
        "files": files,
        "target_exit": 0 if spec.correct else 1,
        "target_tail": ["1 passed" if spec.correct else "1 failed"],
        "suite_exit": 1,
        "suite_failures": (ceiling if spec.correct else (ceiling or 2) + 1)
        if ceiling is not None else None,
        "git_status": "",
    }


def build_context_fixture(spec: MockSpec) -> dict:
    """``context_fixture.json``, in the shape :mod:`context_fixture` writes."""
    return {
        "kind": "synthetic_load_fixture",
        "_synthetic": SYNTHETIC_MARK,
        "scenario": spec.scenario,
        "seed": spec.seed,
        "target_context_size": spec.ladder_rung,
        "synthetic_context_size": spec.synthetic_context_size,
        "token_basis": "estimated",
        "chars_per_token_assumed": 3.6,
        "context_generation_method": (
            "MockClaudeAdapter: no file was generated; this records the rung "
            "the trial declares"),
        "reason_if_target_not_reached": spec.reason_if_target_not_reached,
        "actual_observed_context_size": None,
    }


def build_meta(spec: MockSpec, manifest: dict, source_id: str,
               destination_id: str) -> dict:
    """``trial_meta.json``, including the pair key pairing matches on."""
    handoff, memory_scan, validity = spec.validity()
    leak_scan = {
        "clean": spec.leak_clean,
        "surfaces_scanned": ["tree", "filenames", "git_history", "git_refs",
                             "claude_md", "project_memory", "environment",
                             "post_prompt", "pre_prompts"],
        "fatal_surfaces": ["tree", "filenames", "git_history", "git_refs",
                           "claude_md", "project_memory", "environment",
                           "post_prompt"],
        "hits_by_surface": {} if spec.leak_clean else {"tree": 1},
        "fatal_hits": [] if spec.leak_clean else [{
            "surface": "tree", "where": "README.md:4",
            "term": "the active task",
            "context": "…the active task is the reconcile window…"}],
        "terms": list(manifest.get("leak_terms") or [])[:20],
    }
    return {
        "_synthetic": SYNTHETIC_MARK,
        "adapter": SYNTHETIC_MARK,
        "scenario": spec.scenario,
        "arm": spec.arm,
        "replicate": spec.replicate,
        "pair_id": spec.pair_id,
        "pair_key": {
            "benchmark": spec.scenario,
            "scenario": spec.scenario,
            "pair_id": spec.pair_id,
            "fixture_seed": manifest.get("fixture_seed"),
            "model": spec.model,
            "claude_version": spec.claude_version,
            "turn_script_hash": manifest.get("turn_script_hash"),
            "permission_mode": spec.permission_mode,
            "context_ladder_rung": spec.ladder_rung,
        },
        "model": spec.model,
        "session_id": destination_id,
        "source_session_id": source_id,
        "destination_session_id": destination_id,
        "source_session": {
            "session_id": source_id,
            "turns": spec.source_turns,
            "transition": spec.transition,
        },
        "old_transcript_replayed": spec.old_transcript_replayed,
        "auto_memory_disabled": spec.auto_memory_disabled,
        "memory_scan_clean": memory_scan["clean"],
        "source_handoff_valid": handoff["source_handoff_valid"],
        "source_handoff_reason": handoff["source_handoff_reason"],
        "target_status_at_handoff": handoff["target_status_at_handoff"],
        "invariant_status_at_handoff": handoff["invariant_status_at_handoff"],
        "invalidation_reason": validity["invalidation_reason"],
        "trial_validity": validity,
        "memory_isolation": {
            "controls": {isolation.MEMORY_ENV: isolation.MEMORY_ENV_VALUE,
                         isolation.MEMORY_SETTING: False},
            "scans": {"before_destination": memory_scan},
            "identical_for_both_arms": True,
        },
        "source_handoff": handoff,
        "destination_skipped": not validity["valid"],
        "manifest": manifest,
        "leak_scan": leak_scan,
        "telemetry_declared_source": (
            telemetry.SOURCE_SYNTHETIC_FIXTURE if spec.emit_usage
            else telemetry.SOURCE_UNAVAILABLE),
        **prereg.stamp(),
    }


# --------------------------------------------------------------------------
# writing a trial
# --------------------------------------------------------------------------


def write_trial(root: pathlib.Path, spec: MockSpec) -> pathlib.Path:
    """Write one synthetic trial directory and return its path."""
    manifest = scenarios.manifest_for(spec.scenario)
    source_id, destination_id = spec.session_ids()
    capsule = render_capsule(spec, manifest, source_id)

    trial = pathlib.Path(root) / spec.trial
    trial.mkdir(parents=True, exist_ok=True)

    if not spec.validity()[2]["valid"]:
        # An invalid trial stops before its destination session, live and
        # mock alike: there is no stream, transcript or restore to write, and
        # nothing downstream may try to read one.
        (trial / "final_state.json").write_text(
            json.dumps(build_final_state(spec, manifest), indent=2),
            encoding="utf-8", newline="")
        (trial / "context_fixture.json").write_text(
            json.dumps(build_context_fixture(spec), indent=2),
            encoding="utf-8", newline="")
        (trial / "trial_meta.json").write_text(
            json.dumps(build_meta(spec, manifest, source_id, None),
                       indent=2), encoding="utf-8", newline="")
        return trial

    lines = build_stream(spec, manifest, source_id, destination_id, capsule)
    if spec.malformed_fraction_override is not None:
        # Drive the capture straight past the usability limit, for the case
        # where the question is what the pipeline does with rubbish rather
        # than whether it survives a little of it.
        want = spec.malformed_fraction_override
        extra = int(len(lines) * want / max(1e-9, 1 - want)) + 1
        lines.extend(['{"broken": ' for _ in range(extra)])
    (trial / "stream.jsonl").write_text("\n".join(lines) + "\n",
                                        encoding="utf-8", newline="")

    transcript = build_transcript(spec, destination_id)
    (trial / "transcript.jsonl").write_text("\n".join(transcript) + "\n",
                                            encoding="utf-8", newline="")

    if spec.usage_in_rollup and spec.emit_usage:
        (trial / "usage.json").write_text(json.dumps({
            "source": telemetry.SOURCE_SYNTHETIC_FIXTURE,
            "_synthetic": SYNTHETIC_MARK,
            "records": [{"session_id": destination_id, "kind": "turn",
                         "usage": _usage(spec, turn)}
                        for turn in range(spec.turns)],
        }, indent=2), encoding="utf-8", newline="")

    if spec.arm == "velra":
        (trial / "velra_restore.json").write_text(
            json.dumps(build_restore(spec, manifest, source_id, capsule),
                       indent=2), encoding="utf-8", newline="")

    (trial / "final_state.json").write_text(
        json.dumps(build_final_state(spec, manifest), indent=2),
        encoding="utf-8", newline="")
    (trial / "context_fixture.json").write_text(
        json.dumps(build_context_fixture(spec), indent=2),
        encoding="utf-8", newline="")
    (trial / "trial_meta.json").write_text(
        json.dumps(build_meta(spec, manifest, source_id, destination_id),
                   indent=2), encoding="utf-8", newline="")
    return trial


def write_pair(root: pathlib.Path, baseline: MockSpec,
               velra: MockSpec) -> tuple[pathlib.Path, pathlib.Path]:
    return write_trial(root, baseline), write_trial(root, velra)


#: The artifact set a trial directory holds. :mod:`live_trial` writes the same
#: names, and ``test_tokenburn_pipeline`` checks the two agree.
TRIAL_ARTIFACTS = ("trial_meta.json", "stream.jsonl", "transcript.jsonl",
                   "final_state.json", "context_fixture.json")
VELRA_ONLY_ARTIFACTS = ("velra_restore.json",)


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description="Write one synthetic trial.")
    ap.add_argument("--into", required=True)
    ap.add_argument("--trial", default="mock-velra-r1")
    ap.add_argument("--scenario", default="A_cold_continuation")
    ap.add_argument("--arm", default="velra", choices=("baseline", "velra"))
    ap.add_argument("--pair-id", default="mock#r1")
    args = ap.parse_args()
    path = write_trial(pathlib.Path(args.into),
                       MockSpec(trial=args.trial, scenario=args.scenario,
                                pair_id=args.pair_id, arm=args.arm))
    print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
