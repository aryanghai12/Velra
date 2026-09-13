#!/usr/bin/env python3
"""Is there any amnesia to eliminate? A control for the whole benchmark.

Both arms of the main benchmark scored perfectly on the post-compaction turn,
which has two possible explanations:

  (a) Claude Code's own compaction preserves everything that matters on this
      task, so there is no amnesia for Velra to fix; or
  (b) compaction did not actually drop anything in this harness, in which case
      the post-compaction turn tested nothing at all and neither arm's score
      means anything.

Telling those apart is not optional -- hypotheses 1 and 2 are only meaningful
under (a). This script measures it directly, with Velra uninvolved:

  1. A no-tool turn immediately before `/compact` measures the context the
     model is actually carrying (a single API iteration, so the billed input
     token count *is* the context size).
  2. `/compact`.
  3. The same no-tool turn again, measuring the context afterwards.
  4. A verbatim-detail question: one integer constant, from one file read many
     turns earlier, that no reasonable summary would ever keep. The model is
     told to answer UNKNOWN rather than guess, and forbidden from using tools.

If the context shrinks sharply and the detail is gone, compaction is lossy and
the benchmark is measuring something real. If the context barely moves and the
detail survives, the compaction step is not doing its job in this harness.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys

import claude_binary

HERE = pathlib.Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent

# Resolved, never pinned: the VS Code extension auto-updates, so a literal
# version in this path goes stale without warning. See claude_binary.py.
CLAUDE = claude_binary.resolve()

# The needle must be a detail that was *incidental*: present in the raw
# transcript because a file was read, but never asked about and never written
# out in any reply. adyen_uk.py sets API_KEY_PREFIX = "AQE1" near the top; the
# file is read in turn 0 for its DECLINE_STATUS, and the prefix is never
# mentioned again by anybody.
#
# An earlier version of this probe asked for that file's DECLINE_STATUS, which
# was the explicit subject of turn 0 and therefore exactly the sort of thing a
# good summary keeps on purpose. Recalling it proved nothing.
NEEDLE_FILE = "src/ledger/adapters/adyen_uk.py"
NEEDLE_CONST = "API_KEY_PREFIX"
NEEDLE_VALUE = "AQE1"

PROBE = "Reply with exactly: OK"

TURNS = [
    "Read src/ledger/adapters/mollie_apac.py, src/ledger/adapters/stripe_eu.py "
    "and src/ledger/adapters/adyen_uk.py. For each, give me its DECLINE_STATUS "
    "and DECLINE_REASON.",

    "Now read every module under src/ledger/reporting and list each one's "
    "COLUMNS tuple.",

    "Now read every module under src/ledger/importers and give me each "
    "RECORD_KIND and DELIMITER.",

    "Now read every module under src/ledger/validation and list each "
    "MAX_LENGTH.",

    "Read src/ledger/money.py, src/ledger/rules.py and src/ledger/engine.py "
    "and explain how a settlement total is computed.",

    PROBE,          # index 5: context size before compaction
    "/compact",     # index 6
    # The compaction request's own token usage is reported against the turn
    # *after* it, so the first probe here is contaminated and the second one is
    # the clean measurement.
    PROBE,          # index 7: contaminated
    PROBE,          # index 8: clean post-compaction context size
    # index 9: the incidental-detail canary
    "Do not use any tools and do not read any files. Answer purely from what "
    f"you still have in context: what is the exact string assigned to "
    f"{NEEDLE_CONST} in {NEEDLE_FILE}? Reply with just that string, or with "
    "the single word UNKNOWN if that detail is no longer available to you.",
]

PROBE_BEFORE, COMPACT_AT, PROBE_DIRTY, PROBE_AFTER, CANARY_AT = 5, 6, 7, 8, 9


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="bench/results/compaction_probe.json")
    ap.add_argument("--fixture", required=True)
    ap.add_argument("--model", default="sonnet")
    args = ap.parse_args()

    fixture = pathlib.Path(args.fixture).resolve()
    mk = subprocess.run(
        [sys.executable, str(REPO_ROOT / "bench" / "fixture" / "make_fixture.py"),
         str(fixture), "--noise"], capture_output=True, text=True,
        encoding="utf-8", errors="replace")
    if mk.returncode != 0:
        print(mk.stderr, file=sys.stderr)
        return 1

    env = dict(os.environ)
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)

    proc = subprocess.Popen(
        [str(CLAUDE), "-p", "--input-format", "stream-json",
         "--output-format", "stream-json", "--verbose",
         "--model", args.model, "--permission-mode", "bypassPermissions",
         "--permission-prompts", "none", "--strict-mcp-config"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        cwd=str(fixture), env=env, text=True, encoding="utf-8",
        errors="replace", bufsize=1)

    def send(text: str) -> None:
        proc.stdin.write(json.dumps(
            {"type": "user",
             "message": {"role": "user", "content": [{"type": "text", "text": text}]}}) + "\n")
        proc.stdin.flush()

    turn = 0
    records: list[dict] = []
    texts: dict[int, list[str]] = {}
    send(TURNS[0])

    for line in proc.stdout:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("type") == "assistant":
            for block in obj.get("message", {}).get("content", []):
                if block.get("type") == "text":
                    texts.setdefault(turn, []).append(block["text"])
                elif block.get("type") == "tool_use":
                    texts.setdefault(turn, []).append(f"<TOOL:{block.get('name')}>")
        if obj.get("type") != "result":
            continue
        usage = obj.get("usage", {}) or {}
        iterations = usage.get("iterations") or []
        records.append({
            "turn": turn,
            "prompt": TURNS[turn][:80],
            "input_tokens": usage.get("input_tokens", 0),
            "cache_creation": usage.get("cache_creation_input_tokens", 0),
            "cache_read": usage.get("cache_read_input_tokens", 0),
            "total_input": (usage.get("input_tokens", 0)
                            + usage.get("cache_creation_input_tokens", 0)
                            + usage.get("cache_read_input_tokens", 0)),
            "api_iterations": len(iterations),
            "text": " ".join(texts.get(turn, []))[:600],
        })
        print(f"  turn {turn}: total_input={records[-1]['total_input']} "
              f"iterations={records[-1]['api_iterations']}", flush=True)
        turn += 1
        if turn < len(TURNS):
            send(TURNS[turn])
        else:
            proc.stdin.close()
            break

    proc.wait(timeout=120)

    def rec(i):
        return next((r for r in records if r["turn"] == i), None)

    before, after, canary = rec(PROBE_BEFORE), rec(PROBE_AFTER), rec(CANARY_AT)
    dirty = rec(PROBE_DIRTY)
    answer = (canary or {}).get("text", "").strip()
    recalled = NEEDLE_VALUE in answer
    said_unknown = "UNKNOWN" in answer.upper()
    used_tool = "<TOOL:" in answer

    out = {
        "fixture": str(fixture),
        "model": args.model,
        "turns": records,
        "context_before_compaction": (before or {}).get("total_input"),
        "context_after_compaction": (after or {}).get("total_input"),
        "context_after_compaction_contaminated": (dirty or {}).get("total_input"),
        "context_before_iterations": (before or {}).get("api_iterations"),
        "context_after_iterations": (after or {}).get("api_iterations"),
        "canary_file": NEEDLE_FILE,
        "canary_value": NEEDLE_VALUE,
        "canary_answer": answer,
        "canary_recalled": recalled,
        "canary_said_unknown": said_unknown,
        "canary_used_tool": used_tool,
    }
    b, a = out["context_before_compaction"], out["context_after_compaction"]
    if b and a:
        out["context_reduction_tokens"] = b - a
        out["context_reduction_pct"] = round(100.0 * (b - a) / b, 1)
    out["compaction_is_lossy"] = bool(
        out.get("context_reduction_pct", 0) > 20 and not recalled)

    target = pathlib.Path(args.out)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(out, indent=2), encoding="utf-8", newline="")

    print()
    print(f"context before /compact: {b} tokens")
    print(f"context after  /compact: {a} tokens "
          f"(first, contaminated probe: {out['context_after_compaction_contaminated']})")
    if b and a:
        print(f"reduction:               {out['context_reduction_tokens']} tokens "
              f"({out['context_reduction_pct']}%)")
    print(f"canary ({NEEDLE_FILE} {NEEDLE_CONST} = {NEEDLE_VALUE}):")
    print(f"  answer:    {answer[:200]!r}")
    print(f"  recalled:  {recalled}   said UNKNOWN: {said_unknown}   used tool: {used_tool}")
    print(f"\ncompaction is lossy: {out['compaction_is_lossy']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
