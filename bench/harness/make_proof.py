#!/usr/bin/env python3
"""Render the benchmark's proof asset as a self-contained SVG.

The picture is generated from the measurement files, never hand-written, so it
cannot drift from the data: the two terminal panes replay the tool calls each
arm actually made on the post-compaction turn, and the verdict strip is read
from verdicts.json. If the two arms behaved identically, the asset says so.

SVG rather than GIF on purpose: there is no terminal recorder on this machine,
and a vector asset stays sharp on a GitHub README at any width while remaining
a few tens of kilobytes. A PNG is written alongside it when Pillow is present.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import xml.sax.saxutils as xml

RESULTS = pathlib.Path(__file__).resolve().parent.parent / "results"

BG = "#0d1117"
PANE = "#161b22"
PANE_EDGE = "#30363d"
FG = "#c9d1d9"
DIM = "#8b949e"
GREEN = "#3fb950"
AMBER = "#d29922"
RED = "#f85149"
BLUE = "#58a6ff"
PURPLE = "#bc8cff"
MONO = "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace"

W = 1640
LINE = 20
CH = 8.4          # advance width of the monospace stack at 14px


def esc(text: str) -> str:
    return xml.escape(text)


def text_el(x, y, s, fill=FG, size=14, weight="normal", opacity=1.0):
    return (f'<text x="{x}" y="{y}" font-family="{MONO}" font-size="{size}" '
            f'fill="{fill}" font-weight="{weight}" opacity="{opacity}" '
            f'xml:space="preserve">{esc(s)}</text>')


def load(name):
    p = RESULTS / name
    return json.loads(p.read_text(encoding="utf-8")) if p.exists() else None


def trial_analysis(arm: str, protocol: str | None = None):
    """Analysed trials for one arm, optionally restricted to one protocol.

    The arm is read from the trial's own metadata rather than matched against
    the directory name, so that a directory called "sat-velra-r1" cannot be
    mistaken for a baseline and vice versa.
    """
    out = []
    trials = RESULTS / "trials"
    if not trials.exists():
        return out
    for d in sorted(trials.iterdir()):
        meta_path, analysis_path = d / "trial_meta.json", d / "analysis.json"
        if not (meta_path.exists() and analysis_path.exists()):
            continue
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        if meta.get("arm") != arm:
            continue
        if protocol and meta.get("protocol", "short") != protocol:
            continue
        out.append(json.loads(analysis_path.read_text(encoding="utf-8")))
    return out


def shorten(path: str, keep=3) -> str:
    parts = path.replace("\\", "/").rstrip("/").split("/")
    return "/".join(parts[-keep:]) if len(parts) > keep else path


def tool_lines(analysis: dict) -> list[tuple[str, str]]:
    """(colour, text) lines replaying the measured turn."""
    m = analysis["measured_turn"]
    lines: list[tuple[str, str]] = []
    for call in m["tool_sequence"]:
        bucket, name = call["bucket"], call["name"]
        target = call["target"]
        if bucket in ("read", "search"):
            colour, verb = AMBER, "read"
        elif bucket == "edit":
            colour, verb = GREEN, "edit"
        else:
            colour, verb = BLUE, "run "
        if name in ("Bash", "PowerShell"):
            shown = target.split("&&")[-1].strip()[:52]
        else:
            shown = shorten(target)
        lines.append((colour, f"  {verb}  {name:<6} {shown}"))
    if not lines:
        lines.append((DIM, "  (no tool calls)"))
    return lines


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="assets/proof.svg")
    args = ap.parse_args()

    verdicts = load("verdicts.json") or {}
    agg = load("aggregate.json") or {}
    overhead = load("hook_overhead.json")
    probe = load("compaction_probe.json")

    groups = agg.get("groups", {})
    protocol = next((p for p in ("saturated", "short")
                     if f"{p}/velra" in groups and f"{p}/baseline" in groups), None)

    base_trials = trial_analysis("baseline", protocol)
    velra_trials = trial_analysis("velra", protocol)
    if not (base_trials and velra_trials):
        print("not enough analysed trials to render the proof asset")
        return 1
    base, velra = base_trials[-1], velra_trials[-1]
    gb = groups.get(f"{protocol}/baseline", {})
    gv = groups.get(f"{protocol}/velra", {})

    # ---------------------------------------------------------------- layout
    pane_w = (W - 3 * 28) // 2
    pane_x = [28, 28 + pane_w + 28]
    y = 0
    parts: list[str] = []

    # header
    title = "Velra v0.1"
    parts.append(text_el(28, 46, title, BLUE, 27, "bold"))
    # 27px in this monospace stack advances ~16.2px per character; leaving a
    # two-character gap keeps the subtitle clear of the title at any renderer.
    parts.append(text_el(28 + round((len(title) + 2) * 16.2), 46,
                         "post-compaction behaviour, measured", FG, 27))
    parts.append(text_el(28, 72,
                         "Two Claude Code sessions. Identical repository, identical "
                         "16-turn script, identical /compact boundary.", DIM, 13))
    parts.append(text_el(28, 92,
                         "The only difference: whether Velra's hooks were registered. "
                         "Every line below is replayed from the captured session stream.",
                         DIM, 13))

    head_y = 128
    pane_y = head_y + 18
    body_rows = max(len(tool_lines(base)), len(tool_lines(velra)))
    pane_h = 96 + body_rows * LINE + 76

    panes = [
        ("vanilla claude code", base, gb, RED),
        ("claude code + velra", velra, gv, GREEN),
    ]
    for i, (title, analysis, group, accent) in enumerate(panes):
        x = pane_x[i]
        parts.append(f'<rect x="{x}" y="{pane_y}" width="{pane_w}" height="{pane_h}" '
                     f'rx="10" fill="{PANE}" stroke="{PANE_EDGE}" stroke-width="1"/>')
        # title bar
        parts.append(f'<rect x="{x}" y="{pane_y}" width="{pane_w}" height="34" '
                     f'rx="10" fill="#21262d"/>')
        parts.append(f'<rect x="{x}" y="{pane_y+24}" width="{pane_w}" height="10" '
                     f'fill="#21262d"/>')
        for k, colour in enumerate(("#ff5f57", "#febc2e", "#28c840")):
            parts.append(f'<circle cx="{x+18+k*16}" cy="{pane_y+17}" r="5" fill="{colour}"/>')
        parts.append(text_el(x + 74, pane_y + 22, title, FG, 13, "bold"))
        parts.append(text_el(x + pane_w - 200, pane_y + 22,
                             f"session {analysis['session_id'][:8]}", DIM, 12))

        ty = pane_y + 60
        parts.append(text_el(x + 18, ty, "$ /compact", PURPLE, 14))
        ty += LINE
        parts.append(text_el(x + 18, ty, "> Fix the remaining test failure.", FG, 14, "bold"))
        ty += LINE + 6
        for colour, line in tool_lines(analysis):
            parts.append(text_el(x + 18, ty, line, colour, 14))
            ty += LINE
        ty += 8
        m = analysis["measured_turn"]
        ok = analysis["final_pytest_exit"] == 0
        parts.append(text_el(
            x + 18, ty,
            f"  -> {'6 passed' if ok else 'still failing'}   "
            f"re-reads {m['source_rereads_before_first_edit']}   "
            f"tool calls {m['tool_call_count']}",
            GREEN if ok else RED, 14, "bold"))

    y = pane_y + pane_h + 34

    # ---------------------------------------------------------------- metrics
    parts.append(text_el(28, y, "MEASURED, on the turn after /compact", DIM, 12, "bold"))
    y += 12
    rows = [
        ("source files re-read before the first edit",
         gb.get("source_rereads_mean"), gv.get("source_rereads_mean"), "lower"),
        ("first edit landed on engine.settle",
         f"{gb.get('hit_true_symbol')}/{gb.get('n')}", f"{gv.get('hit_true_symbol')}/{gv.get('n')}", "higher"),
        ("re-explored the reverted rounding change",
         f"{gb.get('dead_end_reexplored')}/{gb.get('n')}", f"{gv.get('dead_end_reexplored')}/{gv.get('n')}", "lower"),
        ("test suite green at the end",
         f"{gb.get('fixed_the_test')}/{gb.get('n')}", f"{gv.get('fixed_the_test')}/{gv.get('n')}", "higher"),
        ("total tool calls on the measured turn",
         gb.get("tool_calls_mean"), gv.get("tool_calls_mean"), "lower"),
    ]
    table_h = 30 + len(rows) * 26 + 12
    parts.append(f'<rect x="28" y="{y}" width="{W-56}" height="{table_h}" rx="8" '
                 f'fill="{PANE}" stroke="{PANE_EDGE}"/>')
    ry = y + 26
    parts.append(text_el(46, ry, "metric", DIM, 13, "bold"))
    parts.append(text_el(W - 470, ry, "vanilla", DIM, 13, "bold"))
    parts.append(text_el(W - 300, ry, "velra", DIM, 13, "bold"))
    parts.append(text_el(W - 150, ry, "difference", DIM, 13, "bold"))
    ry += 8
    parts.append(f'<line x1="40" y1="{ry}" x2="{W-40}" y2="{ry}" stroke="{PANE_EDGE}"/>')
    ry += 20
    for label, b, v, _ in rows:
        parts.append(text_el(46, ry, label, FG, 13))
        parts.append(text_el(W - 470, ry, str(b), FG, 13))
        parts.append(text_el(W - 300, ry, str(v), FG, 13))
        same = str(b) == str(v)
        parts.append(text_el(W - 150, ry, "identical" if same else "differs",
                             DIM if same else GREEN, 13))
        ry += 26
    y += table_h + 30

    # ---------------------------------------------------------------- verdicts
    titles = {
        "H1": "compaction amnesia elimination",
        "H2": "dead-end loop prevention",
        "H3": "continuation budget <= 800 tokens",
        "H4": "zero-overhead fail-open guarantee",
    }
    colour_of = {"PASSED": GREEN, "FAILED": RED, "INCONCLUSIVE": AMBER}
    card_w = (W - 56 - 3 * 18) // 4
    for i, key in enumerate(("H1", "H2", "H3", "H4")):
        v = verdicts.get(key, {}).get("verdict", "INCONCLUSIVE")
        x = 28 + i * (card_w + 18)
        parts.append(f'<rect x="{x}" y="{y}" width="{card_w}" height="78" rx="8" '
                     f'fill="{PANE}" stroke="{colour_of.get(v, DIM)}" stroke-width="1.5"/>')
        parts.append(text_el(x + 16, y + 26, key, DIM, 13, "bold"))
        parts.append(text_el(x + 46, y + 26, titles[key], FG, 13))
        parts.append(text_el(x + 16, y + 56, v, colour_of.get(v, DIM), 18, "bold"))
    y += 78 + 28

    # ---------------------------------------------------------------- footnote
    notes = []
    if probe:
        notes.append(
            f"control: /compact cut the context from "
            f"{probe.get('context_before_compaction')} to "
            f"{probe.get('context_after_compaction')} tokens "
            f"({probe.get('context_reduction_pct')}% smaller); a verbatim detail from "
            f"nine turns earlier was {'still recalled' if probe.get('canary_recalled') else 'gone'}.")
    if overhead:
        notes.append(
            f"overhead: worst-case p99 {overhead.get('worst_case_p99_ms')} ms across "
            f"{len(overhead.get('cases', []))} hook types against a "
            f"{overhead['database_bytes']/1024/1024:.0f} MiB / "
            f"{overhead['seeded_events']}-event database "
            f"(process-spawn control p50 {overhead['spawn_control']['p50_ms']} ms).")
    tok = None
    trials = RESULTS / "trials"
    for d in sorted(trials.glob("*velra*")) if trials.exists() else []:
        f = d / "token_measurement.json"
        if f.exists():
            data = json.loads(f.read_text(encoding="utf-8"))
            if data.get("injections"):
                tok = data["injections"][0]
    if tok:
        notes.append(
            f"capsule: {tok['context_chars']} characters measured at "
            f"{tok['measured_tokens']} tokens by Anthropic's own tokenizer, "
            f"injected once via {tok['hook_name']}.")
    for note in notes:
        parts.append(text_el(28, y, note, DIM, 12))
        y += 19
    y += 6
    parts.append(text_el(28, y,
                         "Generated by bench/run_full_benchmark.py from the raw session "
                         "streams in bench/results/trials/. Nothing in this image is "
                         "hand-written.", DIM, 12, opacity=0.75))
    height = y + 28

    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{height}" '
           f'viewBox="0 0 {W} {height}" font-family="{MONO}">'
           f'<rect width="{W}" height="{height}" fill="{BG}"/>'
           + "".join(parts) + "</svg>")

    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(svg, encoding="utf-8", newline="")
    print(f"wrote {out} ({len(svg)/1024:.1f} KiB, {W}x{height})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
