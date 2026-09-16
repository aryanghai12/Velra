#!/usr/bin/env python3
"""Measure Claude Code's own compaction summary with the same tokenizer.

The v0.1 report compares Velra's capsule, measured on Anthropic's tokenizer,
against a native summary estimated at four characters per token. That is not a
comparison: the capsule's real density is about 2.1 characters per token
because it is dense with paths, brackets and ULIDs, while prose sits nearer
4.0. Estimating one side and measuring the other flatters whichever side is
estimated generously.

This runs the native summary through exactly the method
``measure_tokens.py`` uses for the capsule — two minimal print-mode sessions
with all tools disabled, differing only by the text under test, with a control
pair that must differ by zero — so both numbers come off the same instrument.

A summary is large, so this costs more per measurement than the capsule does.
``--max-chars`` refuses anything above a sane ceiling rather than silently
spending a lot of money on one trial.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from measure_tokens import SENTINEL, total_input_tokens  # noqa: E402


def measure(text: str, model: str) -> dict:
    control_a = total_input_tokens(SENTINEL, model)
    control_b = total_input_tokens(SENTINEL, model)
    delta = control_b["total_input_tokens"] - control_a["total_input_tokens"]
    base = control_a["total_input_tokens"]
    measured = total_input_tokens(text + "\n" + SENTINEL, model)
    return {
        "control": {"a": base, "b": control_b["total_input_tokens"], "delta": delta},
        "control_valid": delta == 0,
        "chars": len(text),
        "measured_tokens": measured["total_input_tokens"] - base,
        "with_sentinel_total": measured["total_input_tokens"],
        "baseline_total": base,
        "chars_per_token": round(
            len(text) / max(1, measured["total_input_tokens"] - base), 3),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--trial", required=True)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--max-chars", type=int, default=120_000,
                    help="refuse to measure a summary larger than this")
    args = ap.parse_args()

    trial = pathlib.Path(args.trial).resolve()
    path = trial / "native_summary.txt"
    if not path.exists():
        print(f"{trial.name}: no native summary captured; nothing to measure")
        (trial / "native_summary_tokens.json").write_text(
            json.dumps({"present": False}, indent=2), encoding="utf-8", newline="")
        return 0

    text = path.read_text(encoding="utf-8")
    if len(text) > args.max_chars:
        result = {"present": True, "measured": False, "chars": len(text),
                  "reason": f"summary exceeds --max-chars ({args.max_chars})"}
    else:
        result = {"present": True, "measured": True, "model": args.model,
                  **measure(text, args.model)}

    (trial / "native_summary_tokens.json").write_text(
        json.dumps(result, indent=2), encoding="utf-8", newline="")
    if result.get("measured"):
        print(f"{trial.name}: native summary {result['chars']} chars -> "
              f"{result['measured_tokens']} tokens "
              f"({result['chars_per_token']} chars/token), "
              f"control delta {result['control']['delta']}")
    else:
        print(f"{trial.name}: {result.get('reason')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
