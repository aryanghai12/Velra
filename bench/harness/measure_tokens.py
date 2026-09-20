#!/usr/bin/env python3
"""Measure the injected continuation block with Anthropic's own tokenizer.

Velra reports its own estimate (``capsule_tokens_est``), and an estimate is not
evidence. This script measures the real thing.

Method. Two minimal print-mode sessions are run with every tool disabled, so
the only difference between them is the text of the user message:

    A:  <sentinel>
    B:  <capsule> <sentinel>

For each, the total number of input tokens the API actually billed is

    input_tokens + cache_creation_input_tokens + cache_read_input_tokens

and the difference B - A is the capsule's token count. Because the system
prompt, the tool schemas and the sentinel are byte-identical across the two
runs, everything except the capsule cancels.

A control pair (A measured twice) is run first. Its difference must be zero;
if it is not, the measurement is not trustworthy and the script says so
instead of reporting a number.

The capsule text is taken from the trial's raw stream -- the literal
``additionalContext`` bytes that Claude Code received -- not from Velra's
database, so what is measured is what was actually delivered.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys

import claude_binary

# Resolved, never pinned: the VS Code extension auto-updates, so a literal
# version in this path goes stale without warning. See claude_binary.py.
CLAUDE = claude_binary.resolve()

SENTINEL = "Reply with the single word: ACK"


def extract_injected(stream_path: pathlib.Path) -> list[dict]:
    """Every continuation block Claude Code actually received, in order."""
    found = []
    for line in stream_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("subtype") != "hook_response":
            continue
        raw = obj.get("stdout") or obj.get("output") or ""
        if not any(t in raw for t in ("VELRA_WORKSPACE_STATE", "VELRA_CONTINUATION")):
            continue
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError:
            continue
        ctx = (payload.get("hookSpecificOutput") or {}).get("additionalContext")
        if ctx:
            found.append({
                "hook_name": obj.get("hook_name"),
                "hook_event": obj.get("hook_event"),
                "exit_code": obj.get("exit_code"),
                "stdout_bytes": len(raw.encode("utf-8")),
                "stdout_is_single_json_object": True,
                "context": ctx,
            })
    return found


def total_input_tokens(prompt: str, model: str) -> dict:
    env = dict(os.environ)
    for key in ("CLAUDE_CODE_SSE_PORT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
        env.pop(key, None)
    proc = subprocess.run(
        [str(CLAUDE), "-p", prompt, "--output-format", "json",
         "--model", model, "--tools", "", "--strict-mcp-config",
         "--no-session-persistence"],
        capture_output=True, text=True, encoding="utf-8", errors="replace", env=env)
    if proc.returncode != 0:
        raise RuntimeError(f"claude failed: {proc.stderr[:500]}")
    obj = json.loads(proc.stdout)
    usage = obj.get("usage", {})
    total = (usage.get("input_tokens", 0)
             + usage.get("cache_creation_input_tokens", 0)
             + usage.get("cache_read_input_tokens", 0))
    return {"total_input_tokens": total, "usage": usage,
            "session_id": obj.get("session_id")}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--trial", required=True)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--budget", type=int, default=800)
    args = ap.parse_args()

    trial = pathlib.Path(args.trial).resolve()
    injected = extract_injected(trial / "stream.jsonl")
    if not injected:
        print("no continuation block was injected in this trial", file=sys.stderr)
        return 1

    out: dict = {"trial": str(trial), "model": args.model,
                 "budget_tokens": args.budget, "injections": []}

    print("control pair (identical prompts; the difference must be 0) ...")
    c1 = total_input_tokens(SENTINEL, args.model)
    c2 = total_input_tokens(SENTINEL, args.model)
    control_delta = c2["total_input_tokens"] - c1["total_input_tokens"]
    out["control"] = {"a": c1["total_input_tokens"], "b": c2["total_input_tokens"],
                      "delta": control_delta}
    print(f"  baseline A = {c1['total_input_tokens']}, "
          f"baseline B = {c2['total_input_tokens']}, delta = {control_delta}")
    if control_delta != 0:
        out["control_valid"] = False
        print("  control is non-zero: measurement is NOT reliable", file=sys.stderr)
    else:
        out["control_valid"] = True

    base = c1["total_input_tokens"]
    for i, inj in enumerate(injected):
        ctx = inj["context"]
        measured = total_input_tokens(ctx + "\n" + SENTINEL, args.model)
        tokens = measured["total_input_tokens"] - base
        record = {
            "index": i,
            "hook_name": inj["hook_name"],
            "exit_code": inj["exit_code"],
            "hook_stdout_bytes": inj["stdout_bytes"],
            "context_chars": len(ctx),
            "context_bytes": len(ctx.encode("utf-8")),
            "measured_tokens": tokens,
            "budget_tokens": args.budget,
            "within_budget": tokens <= args.budget,
            "with_sentinel_total": measured["total_input_tokens"],
            "baseline_total": base,
            "first_line": ctx.splitlines()[0] if ctx else "",
        }
        out["injections"].append(record)
        print(f"  injection {i} via {inj['hook_name']}: "
              f"{len(ctx)} chars -> {tokens} tokens "
              f"({'WITHIN' if tokens <= args.budget else 'OVER'} the "
              f"{args.budget}-token budget)")

    (trial / "token_measurement.json").write_text(
        json.dumps(out, indent=2), encoding="utf-8", newline="")
    (trial / "injected_context.txt").write_text(
        "\n\n===== NEXT INJECTION =====\n\n".join(
            i["context"] for i in injected), encoding="utf-8", newline="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
