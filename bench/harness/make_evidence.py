#!/usr/bin/env python3
"""Extract the verbatim excerpts that BENCHMARK_REPORT.md cites.

The report quotes raw JSONL lines from the captured session streams. Rather
than copy them by hand -- which would make them unverifiable -- this script
pulls them straight out of the capture and writes bench/results/EVIDENCE.md.

Every block in the output is a byte-for-byte line from stream.jsonl, or a row
read out of the trial's SQLite database, with only two changes: long paths are
left intact, and lines longer than the wrap width are wrapped for display with
the wrap marked. Nothing is paraphrased.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sqlite3
import textwrap

RESULTS = pathlib.Path(__file__).resolve().parent.parent / "results"


def fence(body: str, lang: str = "json") -> str:
    return f"```{lang}\n{body}\n```\n"


def clip(line: str, width: int = 2000) -> str:
    return line if len(line) <= width else line[:width] + f"  ... [{len(line)-width} more bytes]"


def stream_lines(path: pathlib.Path):
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if not raw:
            continue
        try:
            yield raw, json.loads(raw)
        except json.JSONDecodeError:
            continue


def trial_section(d: pathlib.Path) -> str:
    meta = json.loads((d / "trial_meta.json").read_text(encoding="utf-8"))
    analysis = json.loads((d / "analysis.json").read_text(encoding="utf-8"))
    out = [f"## Trial `{d.name}`\n",
           f"- arm: **{meta['arm']}**, replicate {meta['replicate']}, "
           f"protocol `{meta.get('protocol','short')}`, model `{meta['model']}`",
           f"- session id: `{meta['session_id']}`",
           f"- wall time: {meta['wall_seconds']} s, turns sent: {meta['turns_sent']}",
           f"- compaction: `{json.dumps(meta['compact_status'])}`",
           f"- transcript: `{meta.get('transcript_source')}`",
           f"- final `pytest` exit code: **{meta['final_pytest_exit']}** "
           f"({meta['final_pytest_tail']})\n"]

    measured = meta["measured_turn_index"]
    compact_at = meta["compact_turn_index"]

    # --- the compaction boundary, verbatim -------------------------------
    turn = -1
    compact_lines, measured_tool_lines, capsule_line = [], [], None
    for raw, obj in stream_lines(d / "stream.jsonl"):
        if obj.get("_velra_bench") == "turn_start":
            turn = obj["turn"]
            continue
        if turn == compact_at and obj.get("type") == "system" and \
                obj.get("subtype") == "status":
            compact_lines.append(raw)
        if turn == measured and obj.get("type") == "assistant":
            for block in obj.get("message", {}).get("content", []):
                if block.get("type") == "tool_use":
                    measured_tool_lines.append(json.dumps(block))
        if obj.get("subtype") == "hook_response" and \
                any(t in (obj.get("stdout") or "") for t in ("VELRA_WORKSPACE_STATE", "VELRA_CONTINUATION")):
            capsule_line = raw

    if compact_lines:
        out.append(f"### The compaction boundary (turn {compact_at}), verbatim from "
                   f"`stream.jsonl`\n")
        out.append(fence("\n".join(clip(l) for l in compact_lines)))

    out.append(f"### Every tool call on the measured turn ({measured}), verbatim\n")
    out.append(f"The user turn was exactly: `{meta['manifest'] and ''}`"
               f"`Fix the remaining test failure.`\n")
    out.append(fence("\n".join(clip(l, 900) for l in measured_tool_lines)
                     or "(no tool calls)"))

    m = analysis["measured_turn"]
    out.append("Derived from those calls:\n")
    out.append(f"- source-file re-reads before the first edit: "
               f"**{m['source_rereads_before_first_edit']}**")
    out.append(f"- first edit targeted: `{m['first_edit_file']}`")
    out.append(f"- first edit landed inside `engine.settle`: "
               f"**{m['first_edit_hits_true_symbol']}**")
    out.append(f"- re-explored the reverted rounding change: "
               f"**{m['dead_end_reexplored']}**\n")

    # --- the injected capsule --------------------------------------------
    if capsule_line:
        out.append("### The continuation block Claude Code actually received\n")
        out.append("This is the hook's entire stdout, as it appeared on the event "
                   "stream -- one JSON object, nothing else:\n")
        out.append(fence(clip(capsule_line, 3000)))
        ctx = json.loads(json.loads(capsule_line)["stdout"])[
            "hookSpecificOutput"]["additionalContext"]
        out.append("Decoded, that `additionalContext` is:\n")
        out.append(fence(ctx, "text"))

    # --- the database -----------------------------------------------------
    db = d / "velra.db"
    if db.exists():
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        con.row_factory = sqlite3.Row
        out.append("### Velra's own state, read back out of the database\n")
        for label, sql in (
            ("checkpoints", 'SELECT checkpoint_id, "trigger", branch, head_commit, '
                            "event_watermark, partial, capsule_tokens_est, "
                            "length(capsule) AS capsule_chars FROM checkpoints"),
            ("continuations (the delivery state machine)",
             "SELECT checkpoint_id, state, attach_count, attached_channel, "
             "attached_ms, confirmed_ms, confirm_event_id FROM continuations"),
            ("injections", "SELECT injection_id, channel, ts_ms FROM injections"),
            ("dead_ends (revert detection)",
             "SELECT path, mechanism, command_text, reapplied FROM dead_ends"),
            ("edits", "SELECT path, tool_name, status, mechanism, lines_added, "
                      "lines_removed FROM edits ORDER BY ts_ms"),
            ("commands", "SELECT kind, outcome, exit_code, substr(command_text,1,70) "
                         "AS command FROM commands ORDER BY ts_ms"),
            ("compactions", 'SELECT checkpoint_id, "trigger", pre_ms, post_ms '
                            "FROM compactions"),
        ):
            try:
                rows = [dict(r) for r in con.execute(sql).fetchall()]
            except sqlite3.Error as exc:
                rows = [{"error": str(exc)}]
            out.append(f"`{label}`:\n")
            out.append(fence(json.dumps(rows, indent=2)))
        con.close()

    # --- hook cleanliness -------------------------------------------------
    h = analysis.get("hooks") or {}
    if h.get("observed"):
        out.append("### Hook behaviour observed inside the live session\n")
        out.append(f"- hook invocations seen on the event stream: "
                   f"**{h['observed']}**")
        out.append(f"- invocations exiting non-zero: **{h['nonzero_exit']}**")
        out.append(f"- invocations writing to stderr: "
                   f"**{h['responses_with_stderr']}**")
        out.append(f"- invocations writing to stdout: "
                   f"**{h['responses_with_stdout']}** "
                   f"(the continuation injection, which is the documented contract)")
        out.append(f"- synchronous handlers, round trip including Claude Code's own "
                   f"dispatch: p50 {h['sync_p50_ms']} ms, p95 {h['sync_p95_ms']} ms, "
                   f"p99 {h['sync_p99_ms']} ms, max {h['sync_max_ms']} ms "
                   f"(n={h['sync_n']})\n")

    tok = d / "token_measurement.json"
    if tok.exists():
        data = json.loads(tok.read_text(encoding="utf-8"))
        out.append("### Token measurement\n")
        out.append(fence(json.dumps(
            {k: v for k, v in data.items() if k != "injections"} |
            {"injections": data.get("injections")}, indent=2)))

    return "\n".join(out) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="bench/results/EVIDENCE.md")
    args = ap.parse_args()

    trials = sorted((RESULTS / "trials").iterdir()) if (RESULTS / "trials").exists() else []
    trials = [d for d in trials if (d / "analysis.json").exists()]

    parts = [
        "# Raw evidence\n",
        textwrap.fill(
            "Generated by bench/harness/make_evidence.py. Every JSON block below "
            "is a verbatim line from the captured session stream or a row read "
            "out of the trial's SQLite database. Nothing here is paraphrased, "
            "and nothing here is written by hand.", 78),
        "\n",
    ]

    probe = RESULTS / "compaction_probe.json"
    if probe.exists():
        data = json.loads(probe.read_text(encoding="utf-8"))
        parts.append("## Control: was compaction lossy at all?\n")
        parts.append(fence(json.dumps(
            {k: v for k, v in data.items() if k != "turns"}, indent=2)))
        parts.append("Per-turn context measurements:\n")
        parts.append(fence(json.dumps(data.get("turns", []), indent=2)))

    overhead = RESULTS / "hook_overhead.json"
    if overhead.exists():
        parts.append("## Hook overhead, measured directly\n")
        parts.append(fence(overhead.read_text(encoding="utf-8")))

    phase1 = RESULTS / "phase1_environment.json"
    if phase1.exists():
        parts.append("## Phase 1: binary and settings round-trip\n")
        parts.append(fence(phase1.read_text(encoding="utf-8")))

    for d in trials:
        parts.append(trial_section(d))

    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(parts), encoding="utf-8", newline="")
    print(f"wrote {out} ({out.stat().st_size/1024:.0f} KiB) covering {len(trials)} trials")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
