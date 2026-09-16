#!/usr/bin/env python3
"""Behavioural metrics computed from one trial's raw capture.

Everything here is a pure function of bytes already on disk. Nothing in this
module knows which arm it is looking at: no function takes an ``arm`` argument
and none reads one out of the metadata it is given. That is deliberate — an
analysis that can branch on the arm is an analysis that can be tuned until the
right arm wins.

The generic stream handling (splitting a capture into turns, bucketing a tool
call, reading the SQLite telemetry, timing the hooks) is imported from the v0.1
``analyze.py`` rather than rewritten, so both reports are computed by the same
code where they measure the same thing.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from analyze import (BASH_READ_RE, EDIT_TOOLS, READ_TOOLS, SEARCH_TOOLS,  # noqa: E402
                     analyse_db, analyse_hooks, classify, load_stream,
                     tool_target)

# Language an agent uses when it decides the injected block is not to be
# trusted. Matched against the agent's own prose on the measured turn, and
# against the whole post-compaction transcript.
REJECTION_RE = re.compile(
    r"(prompt\s+injection|injected\s+(?:content|context|text|block|instructions)"
    r"|(?:appears|seems|looks)\s+to\s+be\s+(?:a\s+)?(?:fabricat|inject|synthet|spoof)"
    r"|fabricated\s+(?:content|context|history|record)"
    r"|untrusted\s+(?:content|context|input)"
    r"|(?:ignore|disregard|discount)\s+(?:the\s+)?(?:above|preceding|injected|VELRA)"
    r"|not\s+(?:from|written\s+by)\s+the\s+user"
    r"|VELRA_CONTINUATION)",
    re.I)

# The capsule's own opening tag, so a rejection scan can tell "the agent quoted
# the block" apart from "the agent refused it".
CAPSULE_TAG = "<VELRA_CONTINUATION"


def norm(path: str) -> str:
    return (path or "").replace("\\", "/").lower()


def measured_turn(turn: dict) -> dict:
    """Arm-blind description of what happened on the turn being scored."""
    calls = turn["tool_calls"]
    buckets = {"read": 0, "search": 0, "edit": 0, "bash": 0, "other": 0}
    by_tool: dict[str, int] = {}
    for call in calls:
        buckets[classify(call)] += 1
        by_tool[call["name"]] = by_tool.get(call["name"], 0) + 1

    reads_before_edit: list[str] = []
    first_edit = None
    first_edit_index = None
    for index, call in enumerate(calls):
        kind = classify(call)
        if kind == "edit":
            first_edit, first_edit_index = call, index
            break
        if kind == "read":
            reads_before_edit.append(tool_target(call))

    edit_targets = [tool_target(c) for c in calls if classify(c) == "edit"]
    prose = "\n".join(turn["text"])
    return {
        "prompt": turn["prompt"],
        "tool_call_count": len(calls),
        "buckets": buckets,
        "by_tool": by_tool,
        "tool_sequence": [{"name": c["name"], "bucket": classify(c),
                           "target": tool_target(c)} for c in calls],
        "reads_before_first_edit": len(reads_before_edit),
        "read_targets_before_first_edit": reads_before_edit,
        "first_edit_index": first_edit_index,
        "first_edit_file": norm(tool_target(first_edit)) if first_edit else None,
        "edit_count": len(edit_targets),
        "edit_targets": edit_targets,
        "assistant_text": prose,
        "result_cost_usd": (turn.get("result") or {}).get("total_cost_usd"),
        "result_duration_ms": (turn.get("result") or {}).get("duration_ms"),
    }


def source_rereads(measured: dict, manifest: dict) -> int:
    """Re-reads of the project's own source before the first mutation.

    A read of something outside the repository is not a symptom of compaction
    amnesia, so only paths inside the fixture's source and test trees count.
    """
    def is_project_source(target: str) -> bool:
        t = norm(target)
        return "src/ledger/" in t or "/tests/" in t or t.endswith("conftest.py")

    return sum(1 for t in measured["read_targets_before_first_edit"]
               if is_project_source(t))


def delivered_capsule(stream_path: pathlib.Path) -> dict:
    """The literal bytes Claude Code received, and what they contained.

    Read from the raw stream rather than from Velra's database: the claim is
    about what was delivered, and a capsule can be rendered perfectly and still
    never reach the session.
    """
    delivered = None
    hook_name = None
    exit_code = None
    for raw in stream_path.read_text(encoding="utf-8").splitlines():
        if not raw.strip():
            continue
        try:
            obj = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if obj.get("subtype") != "hook_response":
            continue
        out = obj.get("stdout") or obj.get("output") or ""
        if "VELRA_CONTINUATION" not in out:
            continue
        try:
            delivered = json.loads(out)["hookSpecificOutput"]["additionalContext"]
        except (json.JSONDecodeError, KeyError, TypeError):
            continue
        hook_name = obj.get("hook_name")
        exit_code = obj.get("exit_code")
        break

    if delivered is None:
        return {"delivered": False}

    sections = re.findall(r"^\[([A-Z_]+)\]", delivered, re.M)
    return {
        "delivered": True,
        "hook_name": hook_name,
        "exit_code": exit_code,
        "chars": len(delivered),
        "sections": sections,
        "text": delivered,
        "has_dead_ends_section": "DEAD_ENDS" in sections,
        "has_working_files_section": "WORKING_FILES" in sections,
        "has_recent_attempts_section": "RECENT_ATTEMPTS" in sections,
        "has_active_failure_section": "ACTIVE_FAILURE" in sections,
        "has_root_objective_section": "ROOT_TASK_OBJECTIVE" in sections,
    }


def capsule_carries_dead_ends(capsule: dict, manifest: dict) -> dict:
    """Did the capsule identify the abandoned work, and how?

    Velra records which *file* was reverted, never which *idea* was tried: it
    derives everything from tool events and deliberately does not retain edit
    bodies, so the hypothesis itself is not in its data. Both readings are
    reported and only the file-level one is gated on.
    """
    if not capsule.get("delivered"):
        return {"applicable": False}
    text = capsule["text"].replace("\\", "/")
    files = [f.replace("\\", "/") for f in manifest.get("dead_end_files") or []]
    named = [f for f in files if f in text]
    mechanisms = re.findall(r"reverted via `([^`]*)`|changed (outside the agent)", text)
    return {
        "applicable": True,
        "section_present": capsule["has_dead_ends_section"],
        "dead_end_files_expected": files,
        "dead_end_files_named": named,
        "all_named": bool(files) and len(named) == len(files),
        "any_named": bool(named),
        "attribution": [m[0] or m[1] for m in mechanisms],
        "attributed_to_git_command": any(m[0] for m in mechanisms),
        "attributed_as_external": any(m[1] for m in mechanisms),
        # The stricter reading, reported and never gated on.
        "approach_named_verbatim": bool(
            re.search(r"ROUND_HALF_EVEN|half[-_ ]?even|0\.0740", text, re.I)),
    }


def capsule_carries_constraint(capsule: dict, manifest: dict) -> dict:
    """Did a conversation-only constraint survive into the delivered bytes?

    Separating "the constraint never arrived" from "the constraint arrived and
    was ignored" is the difference between a delivery defect and a persuasion
    problem, and they need different fixes.
    """
    markers = manifest.get("constraint_markers") or []
    if not markers:
        return {"applicable": False}
    if not capsule.get("delivered"):
        return {"applicable": True, "delivered": False, "constraint_in_capsule": False}
    text = capsule["text"].lower()
    hit = [m for m in markers if m.lower() in text]
    return {
        "applicable": True,
        "delivered": True,
        "markers": markers,
        "markers_found": hit,
        "constraint_in_capsule": bool(hit),
    }


def working_file_relevance(capsule: dict, manifest: dict) -> dict:
    """Precision and recall of `[WORKING_FILES]` against the files that matter.

    The relevant set is whatever the scenario declares its task runs through:
    the true fix file, the files burned as dead ends, and the failing test.
    Nothing here is inferred from the trial, so the set is the same for both
    arms and cannot be tuned after the fact.
    """
    relevant = {norm(p) for p in filter(None, [
        manifest.get("true_fix_file"),
        *(manifest.get("dead_end_files") or []),
        (manifest.get("failing_test") or "").split("::")[0] or None,
    ])}
    if not capsule.get("delivered"):
        return {"delivered": False, "relevant": sorted(relevant)}
    if not capsule["has_working_files_section"]:
        return {"delivered": True, "section_present": False,
                "relevant": sorted(relevant), "listed": [],
                "precision": None, "recall": 0.0 if relevant else None}

    listed = []
    inside = False
    for line in capsule["text"].splitlines():
        if line.startswith("[WORKING_FILES]"):
            inside = True
            continue
        if inside:
            if line.startswith("["):
                break
            if line.startswith("- "):
                listed.append(norm(line[2:].split(" | ", 1)[0]))
    hits = [p for p in listed if any(p.endswith(r) or r.endswith(p) for r in relevant)]
    return {
        "delivered": True,
        "section_present": True,
        "relevant": sorted(relevant),
        "listed": listed,
        "relevant_listed": hits,
        "precision": round(len(hits) / len(listed), 3) if listed else None,
        "recall": round(len(hits) / len(relevant), 3) if relevant else None,
    }


def prompt_rejection(turns: dict, measured_index: int, capsule: dict) -> dict:
    """Did the agent treat the injected block as something to distrust?

    Made a first-class metric here rather than something grepped for after the
    fact. The scan is deliberately wider than the measured turn: a rejection
    can land on the turn after the one being scored.
    """
    if not capsule.get("delivered"):
        return {"applicable": False}
    hits = []
    for index in sorted(turns):
        if index < measured_index:
            continue
        prose = "\n".join(turns[index]["text"])
        for match in REJECTION_RE.finditer(prose):
            start = max(0, match.start() - 120)
            hits.append({"turn": index, "match": match.group(0),
                         "context": prose[start:match.end() + 120].strip()})
    return {
        "applicable": True,
        "rejected": bool(hits),
        "hits": hits,
        "scanned_turns": [i for i in sorted(turns) if i >= measured_index],
    }


def native_summary(transcript: pathlib.Path) -> dict:
    """Claude Code's own compaction summary, as raw text.

    Only extraction happens here. Its token count is measured by
    ``native_tokens.py`` with the same tokenizer used for the capsule, because
    comparing a measured number against a chars/4 estimate is not a comparison.
    """
    if not transcript.exists():
        return {"present": False, "reason": "no transcript captured"}
    for raw in transcript.read_text(encoding="utf-8").splitlines():
        if not raw.strip():
            continue
        try:
            row = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if not row.get("isCompactSummary"):
            continue
        content = (row.get("message") or {}).get("content")
        if isinstance(content, str):
            text = content
        elif isinstance(content, list):
            text = "".join(b.get("text", "") for b in content if isinstance(b, dict))
        else:
            continue
        return {"present": True, "chars": len(text),
                "bytes": len(text.encode("utf-8")), "text": text}
    return {"present": False, "reason": "no isCompactSummary entry in the transcript"}


def final_tree(trial: pathlib.Path) -> dict:
    """The fixture's end state, as captured by the trial driver."""
    path = trial / "final_state.json"
    if not path.exists():
        return {"present": False, "pytest_exit": None, "files": {}}
    data = json.loads(path.read_text(encoding="utf-8"))
    data["present"] = True
    return data


__all__ = [
    "analyse_db", "analyse_hooks", "load_stream", "classify", "tool_target",
    "measured_turn", "source_rereads", "delivered_capsule",
    "capsule_carries_dead_ends", "capsule_carries_constraint",
    "working_file_relevance", "prompt_rejection", "native_summary",
    "final_tree", "norm",
]
