#!/usr/bin/env python3
"""Phase 4: extract telemetry from one trial's raw capture.

Reads a trial directory produced by ``run_trial.py`` and emits
``analysis.json`` plus a short human-readable summary. Nothing here talks to
the network or to Claude; it is a pure function of the bytes on disk, so the
numbers in the report can be recomputed at any time from the raw capture.

The measured turn is the continuation prompt ("Fix the remaining test
failure.") issued immediately after ``/compact``. Every headline metric is
scoped to that single turn, because that is the turn where compaction amnesia
either shows up or does not.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sqlite3
import sys

# Tools that pull file content into context.
READ_TOOLS = {"Read", "NotebookRead", "ReadLocalFile", "View"}
SEARCH_TOOLS = {"Grep", "Glob"}
EDIT_TOOLS = {"Edit", "MultiEdit", "Write", "NotebookEdit", "EditFile"}

# A Bash command that is really a file read in disguise.
BASH_READ_RE = re.compile(
    r"\b(cat|head|tail|sed\s+-n|less|more|type|Get-Content|nl)\b", re.I)

DEAD_END_MARKERS = re.compile(
    r"ROUND_HALF_EVEN|banker'?s?\s+rounding|half[-_ ]?even", re.I)


def load_stream(path: pathlib.Path):
    """Split the raw stream into turns using the harness's turn_start markers."""
    turns: dict[int, dict] = {}
    current = None
    with open(path, encoding="utf-8") as fh:
        for raw in fh:
            raw = raw.strip()
            if not raw:
                continue
            try:
                obj = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if obj.get("_velra_bench") == "turn_start":
                current = obj["turn"]
                turns[current] = {"turn": current, "prompt": obj["text"],
                                  "lines": [], "tool_calls": [], "text": [],
                                  "result": None, "system": []}
                continue
            if current is None:
                continue
            t = turns[current]
            t["lines"].append(obj)
            kind = obj.get("type")
            if kind == "assistant":
                for block in obj.get("message", {}).get("content", []):
                    if block.get("type") == "tool_use":
                        t["tool_calls"].append({
                            "name": block.get("name"),
                            "input": block.get("input", {}),
                            "id": block.get("id"),
                        })
                    elif block.get("type") == "text":
                        t["text"].append(block.get("text", ""))
            elif kind == "system":
                t["system"].append(obj)
            elif kind == "result":
                t["result"] = obj
    return turns


def tool_target(call: dict) -> str:
    """The file a tool call is aimed at, as written by the model."""
    inp = call.get("input") or {}
    for key in ("file_path", "path", "notebook_path", "filePath"):
        if key in inp and isinstance(inp[key], str):
            return inp[key]
    if call.get("name") in ("Bash", "PowerShell"):
        return (inp.get("command") or "")[:200]
    if call.get("name") in SEARCH_TOOLS:
        return f"{inp.get('pattern','')} in {inp.get('path','.')}"
    return ""


def norm(path: str) -> str:
    return path.replace("\\", "/").lower()


def classify(call: dict) -> str:
    """Bucket a tool call for the post-compaction usage breakdown."""
    name = call.get("name") or ""
    if name in READ_TOOLS:
        return "read"
    if name in SEARCH_TOOLS:
        return "search"
    if name in EDIT_TOOLS:
        return "edit"
    if name in ("Bash", "PowerShell"):
        cmd = (call.get("input") or {}).get("command") or ""
        if BASH_READ_RE.search(cmd):
            return "read"          # a file read wearing a shell's clothes
        return "bash"
    return "other"


def settle_span(engine_src: str) -> tuple[int, int]:
    """Line span (1-based, inclusive) of ``def settle`` in engine.py."""
    lines = engine_src.splitlines()
    start = next(i for i, l in enumerate(lines) if l.startswith("def settle("))
    end = len(lines)
    for i in range(start + 1, len(lines)):
        if lines[i] and not lines[i][0].isspace():
            end = i
            break
    return start + 1, end


def analyse_measured_turn(turn: dict, fixture: pathlib.Path, manifest: dict) -> dict:
    """Everything hypotheses 1 and 2 are decided on."""
    true_file = norm(manifest["true_fix_file"])
    dead_file = norm(manifest["dead_end_file"])

    calls = turn["tool_calls"]
    buckets = {"read": 0, "search": 0, "edit": 0, "bash": 0, "other": 0}
    by_tool: dict[str, int] = {}
    for c in calls:
        buckets[classify(c)] += 1
        by_tool[c["name"]] = by_tool.get(c["name"], 0) + 1

    # --- H1: re-reads of source files before the first mutation -----------
    reads_before_first_edit = 0
    read_targets_before_edit = []
    first_edit = None
    first_edit_index = None
    for i, c in enumerate(calls):
        kind = classify(c)
        if kind == "edit":
            first_edit = c
            first_edit_index = i
            break
        if kind == "read":
            reads_before_first_edit += 1
            read_targets_before_edit.append(tool_target(c))

    # Only count re-reads of the project's own source/test files. A read of
    # something outside the repo is not a symptom of compaction amnesia.
    def is_project_source(target: str) -> bool:
        t = norm(target)
        return ("src/ledger/" in t) or ("tests/test_engine.py" in t) or ("conftest.py" in t)

    source_rereads_before_edit = sum(
        1 for t in read_targets_before_edit if is_project_source(t))

    # --- immediate target accuracy ----------------------------------------
    first_edit_file = norm(tool_target(first_edit)) if first_edit else None
    hit_true_file = bool(first_edit_file and first_edit_file.endswith(true_file))

    hit_true_symbol = False
    if first_edit and hit_true_file:
        engine_path = fixture / "src" / "ledger" / "engine.py"
        # Compare against the committed version, i.e. the text as it was when
        # the turn began.
        try:
            import subprocess
            original = subprocess.run(
                ["git", "show", "HEAD:src/ledger/engine.py"],
                cwd=str(fixture), capture_output=True, text=True,
                encoding="utf-8", errors="replace").stdout
        except Exception:
            original = engine_path.read_text(encoding="utf-8") if engine_path.exists() else ""
        if original:
            lo, hi = settle_span(original)
            body = "\n".join(original.splitlines()[lo - 1:hi])
            inp = first_edit.get("input") or {}
            probes = []
            if isinstance(inp.get("old_string"), str):
                probes.append(inp["old_string"])
            for e in inp.get("edits", []) or []:
                if isinstance(e, dict) and isinstance(e.get("old_string"), str):
                    probes.append(e["old_string"])
            if inp.get("content") and not probes:
                # A whole-file Write: check the new content changes the loop.
                probes.append("")
                hit_true_symbol = "discount_for(subtotal)" in inp["content"].replace(" ", "")
            for p in probes:
                if p and p.strip() and p.strip() in body:
                    hit_true_symbol = True
                    break

    # --- H2: dead-end re-exploration --------------------------------------
    edited_dead_end_file = any(
        classify(c) == "edit" and norm(tool_target(c)).endswith(dead_file)
        for c in calls)
    prose = "\n".join(turn["text"])
    proposed_dead_end_in_prose = bool(DEAD_END_MARKERS.search(prose))
    dead_end_in_edit_payload = False
    for c in calls:
        if classify(c) != "edit":
            continue
        blob = json.dumps(c.get("input") or {})
        if DEAD_END_MARKERS.search(blob):
            dead_end_in_edit_payload = True
    reexplored = bool(edited_dead_end_file or dead_end_in_edit_payload)

    return {
        "prompt": turn["prompt"],
        "tool_call_count": len(calls),
        "buckets": buckets,
        "by_tool": by_tool,
        "tool_sequence": [{"name": c["name"], "bucket": classify(c),
                           "target": tool_target(c)} for c in calls],
        "reads_before_first_edit": reads_before_first_edit,
        "source_rereads_before_first_edit": source_rereads_before_edit,
        "read_targets_before_first_edit": read_targets_before_edit,
        "first_edit_index": first_edit_index,
        "first_edit_file": first_edit_file,
        "first_edit_hits_true_file": hit_true_file,
        "first_edit_hits_true_symbol": hit_true_symbol,
        "dead_end_file_edited": edited_dead_end_file,
        "dead_end_in_edit_payload": dead_end_in_edit_payload,
        "dead_end_mentioned_in_prose": proposed_dead_end_in_prose,
        "dead_end_reexplored": reexplored,
        "assistant_text": prose,
        "result_cost_usd": (turn.get("result") or {}).get("total_cost_usd"),
        "result_duration_ms": (turn.get("result") or {}).get("duration_ms"),
    }


# Velra registers three handlers with "async": true — the PostToolBatch and
# Stop reducer passes, and PostCompact. Claude Code does not wait for those, so
# the gap between their hook_started and hook_response lines on the output
# stream measures when a background job happened to finish, not how long the
# agent was held up. Only the synchronous events below are latency.
#
# Stop is deliberately excluded: it carries one synchronous handler *and* one
# async reducer pass, and the stream labels both simply "Stop".
SYNCHRONOUS_HOOK_EVENTS = {
    "SessionStart", "UserPromptSubmit", "PreToolUse",
    "PostToolUse", "PostToolUseFailure", "PreCompact", "SessionEnd",
}
ASYNC_HOOK_EVENTS = {"PostToolBatch", "PostCompact"}
MIXED_HOOK_EVENTS = {"Stop"}


def analyse_native_summary(transcript: pathlib.Path, manifest: dict) -> dict:
    """Claude Code's own compaction summary: how big, and what did it keep?

    This is the thing Velra's capsule is competing with, so it is worth
    measuring rather than assuming. The keyword checks below are deliberately
    literal -- they ask whether the summary names the failing test, the
    reverted approach and the mechanism of the revert, which are exactly the
    three categories the capsule carries.
    """
    if not transcript.exists():
        return {"present": False}
    text = None
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
        break
    if text is None:
        return {"present": False}

    keywords = {
        "failing_test": "test_exact_payment_settles_invoice",
        "observed_status": manifest.get("observed_status", "UNDERPAID"),
        "failure_line": str(manifest.get("failing_assertion_line", "")),
        "dead_end_approach": "ROUND_HALF_EVEN",
        "dead_end_revert": "git restore",
        "true_fix_symbol": "settle(",
    }
    return {
        "present": True,
        "chars": len(text),
        "bytes": len(text.encode("utf-8")),
        # 4 characters per token is the usual English-prose approximation; the
        # capsule is measured properly with a real tokenizer elsewhere, this is
        # only for an order-of-magnitude comparison.
        "approx_tokens": round(len(text) / 4),
        "mentions": {name: text.count(needle) if needle else None
                     for name, needle in keywords.items()},
        "head": text[:400],
    }


def analyse_delivered_capsule(stream_path: pathlib.Path, manifest: dict) -> dict:
    """What the capsule that was actually delivered contained.

    Hypothesis 2 is about the *capsule*, not about the database. A dead end can
    be recorded perfectly in SQLite and still be filtered out of the rendered
    capsule -- snapshot.rs selects dead ends `WHERE reapplied = 0` -- in which
    case the agent never sees it and the claim fails. So the section is checked
    in the delivered bytes.
    """
    delivered = None
    hook_name = None
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
        except (json.JSONDecodeError, KeyError):
            continue
        hook_name = obj.get("hook_name")
        break

    if delivered is None:
        return {"delivered": False}

    dead_file = manifest["dead_end_file"].replace("\\", "/")
    sections = re.findall(r"^\[([A-Z_]+)\]", delivered, re.M)
    has_dead_ends = "DEAD_ENDS" in sections
    return {
        "delivered": True,
        "hook_name": hook_name,
        "chars": len(delivered),
        "sections": sections,
        "has_dead_ends_section": has_dead_ends,
        "has_recent_attempts_section": "RECENT_ATTEMPTS" in sections,
        # the thing H2 actually claims: the reverted approach is in front of the agent
        "dead_end_file_in_capsule": dead_file in delivered.replace("\\", "/"),
        "dead_end_approach_in_capsule": bool(DEAD_END_MARKERS.search(delivered)),
        "names_failing_line": str(manifest.get("failing_assertion_line", "")) in delivered,
        "names_objective": "ROOT_TASK_OBJECTIVE" in sections,
    }


def analyse_hooks(timing_path: pathlib.Path) -> dict:
    """In-session hook round-trip times, and the stdout/stderr cleanliness check."""
    if not timing_path.exists():
        return {"observed": 0}
    started: dict[str, dict] = {}
    durations = []
    per_event: dict[str, list] = {}
    dirty_stderr = 0
    dirty_stdout = 0
    nonzero_exit = 0
    responses = 0
    with open(timing_path, encoding="utf-8") as fh:
        for raw in fh:
            o = json.loads(raw)
            if o["subtype"] == "hook_started":
                started[o["hook_id"]] = o
            elif o["subtype"] == "hook_response":
                responses += 1
                if o.get("exit_code") not in (0, None):
                    nonzero_exit += 1
                if o.get("stderr_len"):
                    dirty_stderr += 1
                # A hook that injects context legitimately writes JSON to
                # stdout; that is the documented contract, not pollution.
                if o.get("stdout_len"):
                    dirty_stdout += 1
                s = started.get(o["hook_id"])
                if s:
                    ms = (o["t_recv"] - s["t_recv"]) * 1000.0
                    durations.append(ms)
                    per_event.setdefault(o.get("hook_event") or "?", []).append(ms)

    def pct(xs, p):
        if not xs:
            return None
        xs = sorted(xs)
        k = max(0, min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1)))))
        return round(xs[k], 3)

    sync = [ms for event, xs in per_event.items()
            if event in SYNCHRONOUS_HOOK_EVENTS for ms in xs]

    return {
        "observed": responses,
        "paired": len(durations),
        # Headline numbers cover the synchronous handlers only -- the ones that
        # actually sit in front of the agent.
        "sync_n": len(sync),
        "sync_p50_ms": pct(sync, 50),
        "sync_p95_ms": pct(sync, 95),
        "sync_p99_ms": pct(sync, 99),
        "sync_max_ms": round(max(sync), 3) if sync else None,
        # Kept for completeness, but these conflate async completion with latency.
        "all_p50_ms": pct(durations, 50),
        "all_p99_ms": pct(durations, 99),
        "all_max_ms": round(max(durations), 3) if durations else None,
        "nonzero_exit": nonzero_exit,
        "responses_with_stderr": dirty_stderr,
        "responses_with_stdout": dirty_stdout,
        "per_event_p99_ms": {k: pct(v, 99) for k, v in sorted(per_event.items())},
        "per_event_p50_ms": {k: pct(v, 50) for k, v in sorted(per_event.items())},
        "per_event_count": {k: len(v) for k, v in sorted(per_event.items())},
        "per_event_kind": {
            k: ("sync" if k in SYNCHRONOUS_HOOK_EVENTS else
                "async" if k in ASYNC_HOOK_EVENTS else
                "mixed" if k in MIXED_HOOK_EVENTS else "unknown")
            for k in sorted(per_event)
        },
    }


def analyse_db(db_path: pathlib.Path) -> dict:
    if not db_path.exists():
        return {"present": False}
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    out: dict = {"present": True}

    def rows(sql):
        try:
            return [dict(r) for r in con.execute(sql).fetchall()]
        except sqlite3.Error as exc:
            return [{"error": str(exc)}]

    out["checkpoints"] = rows(
        'SELECT checkpoint_id, session_id, "trigger", created_ms, head_commit, '
        "branch, event_watermark, partial, capsule_tokens_est, length(capsule) AS capsule_chars "
        "FROM checkpoints ORDER BY created_ms")
    out["continuations"] = rows(
        "SELECT checkpoint_id, session_id, state, attach_count, attached_ms, "
        "attached_channel, confirmed_ms, confirm_event_id FROM continuations")
    out["injections"] = rows(
        "SELECT injection_id, checkpoint_id, channel, ts_ms FROM injections ORDER BY ts_ms")
    out["dead_ends"] = rows(
        "SELECT id, path, mechanism, command_text, resolved_ms, reapplied FROM dead_ends")
    out["edits"] = rows(
        "SELECT id, path, tool_name, status, mechanism, lines_added, lines_removed, ts_ms "
        "FROM edits ORDER BY ts_ms")
    out["commands"] = rows(
        "SELECT id, kind, command_text, outcome, exit_code, ts_ms FROM commands ORDER BY ts_ms")
    out["compactions"] = rows(
        'SELECT id, session_id, checkpoint_id, "trigger", pre_ms, post_ms FROM compactions')
    out["event_count"] = rows("SELECT COUNT(*) AS n FROM events")[0].get("n")
    out["capsules"] = rows("SELECT checkpoint_id, capsule FROM checkpoints ORDER BY created_ms")
    con.close()
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--trial", required=True)
    args = ap.parse_args()

    trial = pathlib.Path(args.trial).resolve()
    meta = json.loads((trial / "trial_meta.json").read_text(encoding="utf-8"))
    manifest = meta["manifest"]
    fixture = pathlib.Path(meta["fixture"])

    turns = load_stream(trial / "stream.jsonl")
    measured_idx = meta["measured_turn_index"]
    compact_idx = meta["compact_turn_index"]

    measured = turns.get(measured_idx)
    if measured is None:
        print(f"FATAL: measured turn {measured_idx} not present in stream", file=sys.stderr)
        return 1

    analysis = {
        "arm": meta["arm"],
        "replicate": meta["replicate"],
        "model": meta["model"],
        "session_id": meta["session_id"],
        "wall_seconds": meta["wall_seconds"],
        "compact_status": meta["compact_status"],
        "turns_observed": sorted(turns),
        "compact_turn": {
            "index": compact_idx,
            "system_messages": [
                {k: v for k, v in o.items() if k in
                 ("subtype", "status", "compact_result", "compact_error", "uuid")}
                for o in turns.get(compact_idx, {}).get("system", [])
            ],
        },
        "measured_turn": analyse_measured_turn(measured, fixture, manifest),
        "hooks": analyse_hooks(trial / "stream_timing.jsonl"),
        "native_compaction_summary": analyse_native_summary(
            trial / "transcript.jsonl", manifest),
        "delivered_capsule": analyse_delivered_capsule(
            trial / "stream.jsonl", manifest),
        "velra_db": analyse_db(trial / "velra.db"),
        "final_pytest_exit": meta["final_pytest_exit"],
        "final_pytest_tail": meta["final_pytest_tail"],
        "total_cost_usd": (turns[max(turns)].get("result") or {}).get("total_cost_usd"),
        "manifest": manifest,
    }

    # Per-turn tool counts across the whole session, for context.
    analysis["per_turn_tool_counts"] = {
        str(i): {"prompt": t["prompt"][:60],
                 "tools": len(t["tool_calls"]),
                 "names": sorted({c["name"] for c in t["tool_calls"]})}
        for i, t in sorted(turns.items())
    }

    (trial / "analysis.json").write_text(
        json.dumps(analysis, indent=2), encoding="utf-8", newline="")

    m = analysis["measured_turn"]
    print(f"=== {meta['arm']} r{meta['replicate']}  session {meta['session_id']} ===")
    print(f"  compaction:            {meta['compact_status']}")
    print(f"  measured-turn tools:   {m['tool_call_count']}  {m['buckets']}")
    print(f"  source re-reads:       {m['source_rereads_before_first_edit']}")
    print(f"  first edit:            {m['first_edit_file']}")
    print(f"  hits true file:        {m['first_edit_hits_true_file']}")
    print(f"  hits true symbol:      {m['first_edit_hits_true_symbol']}")
    print(f"  dead end re-explored:  {m['dead_end_reexplored']}")
    print(f"  final pytest exit:     {analysis['final_pytest_exit']}")
    h = analysis["hooks"]
    if h.get("observed"):
        print(f"  hook responses:        {h['observed']} "
              f"(sync n={h['sync_n']}: p50 {h['sync_p50_ms']}ms, "
              f"p99 {h['sync_p99_ms']}ms, max {h['sync_max_ms']}ms)")
        print(f"  hook cleanliness:      nonzero-exit {h['nonzero_exit']}, "
              f"stderr {h['responses_with_stderr']}, stdout {h['responses_with_stdout']}")
    ns = analysis["native_compaction_summary"]
    if ns.get("present"):
        print(f"  native summary:        {ns['chars']} chars "
              f"(~{ns['approx_tokens']} tokens); mentions {ns['mentions']}")
    dc = analysis["delivered_capsule"]
    if dc.get("delivered"):
        print(f"  capsule delivered:     {dc['chars']} chars via {dc['hook_name']}")
        print(f"    sections:            {dc['sections']}")
        print(f"    DEAD_ENDS present:   {dc['has_dead_ends_section']}   "
              f"names the reverted file: {dc['dead_end_file_in_capsule']}   "
              f"names the approach: {dc['dead_end_approach_in_capsule']}")
    db = analysis["velra_db"]
    if db.get("present"):
        print(f"  checkpoints:           {len(db['checkpoints'])}")
        print(f"  continuations:         {[c.get('state') for c in db['continuations']]}")
        print(f"  dead_ends recorded:    {len(db['dead_ends'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
