#!/usr/bin/env python3
"""Generate the v0.1.2 benchmark charts in docs/assets/v0.1.2/ from evidence.

    python scripts/doc_assets.py

Reads only committed evidence:

    bench/results/v0.1.2-requal/aggregate.json     live qualification (frozen)
    bench/results/v0.1.2-final/replay/replay.json  final-build replay
    bench/results/v0.1.2-final/latency/*.json      hook latency and its floor

and writes four SVG files. Output is deterministic (no timestamps, fixed
number formatting), so re-running it on unchanged evidence changes nothing.
Every value drawn is also printed as a label; nothing is read off a scale.
The diagrams in the same directory are drawn by hand and not touched here.
"""
from __future__ import annotations

import json
import pathlib
from html import escape

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "assets" / "v0.1.2"
REQUAL = ROOT / "bench" / "results" / "v0.1.2-requal" / "aggregate.json"
REPLAY = ROOT / "bench" / "results" / "v0.1.2-final" / "replay" / "replay.json"
LATENCY = ROOT / "bench" / "results" / "v0.1.2-final" / "latency"

# Reference palette (dataviz skill), validated for these two slots on the
# light surface: CVD dE 24.7, normal-vision dE 33.6, contrast >= 3:1.
SURFACE = "#fcfcfb"
INK = "#0b0b0b"
INK_2 = "#52514e"
INK_3 = "#76756f"
GRID = "#e4e3df"
BLUE = "#2a78d6"    # slot 1: Velra / final build / p50
ORANGE = "#eb6834"  # slot 2: baseline / live-run build / p99
FONT = "system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif"

W = 760


def fmt_int(n: int) -> str:
    return f"{n:,}"


class Svg:
    def __init__(self, width: int, height: int, title: str, desc: str):
        self.w, self.h = width, height
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="t d">',
            f"<title id=\"t\">{escape(title)}</title>",
            f"<desc id=\"d\">{escape(desc)}</desc>",
            f'<rect width="{width}" height="{height}" rx="8" fill="{SURFACE}"/>',
        ]

    def text(self, x, y, s, size=13, fill=INK, anchor="start", weight="normal"):
        self.parts.append(
            f'<text x="{x:.1f}" y="{y:.1f}" font-family="{FONT}" font-size="{size}" '
            f'fill="{fill}" text-anchor="{anchor}" font-weight="{weight}">{escape(s)}</text>')

    def line(self, x1, y1, x2, y2, stroke=GRID, width=1, dash=None):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        self.parts.append(
            f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
            f'stroke="{stroke}" stroke-width="{width}"{d}/>')

    def vbar(self, x, base, top, w, fill):
        """A vertical bar anchored at `base`, 4px rounded at its data end."""
        r = min(4.0, w / 2, max(0.0, base - top))
        self.parts.append(
            f'<path d="M{x:.1f},{base:.1f} V{top + r:.1f} Q{x:.1f},{top:.1f} {x + r:.1f},{top:.1f} '
            f'H{x + w - r:.1f} Q{x + w:.1f},{top:.1f} {x + w:.1f},{top + r:.1f} V{base:.1f} Z" fill="{fill}"/>')

    def hbar(self, base, y, end, h, fill):
        """A horizontal bar anchored at `base`, 4px rounded at its data end."""
        r = min(4.0, h / 2, max(0.0, end - base))
        self.parts.append(
            f'<path d="M{base:.1f},{y:.1f} H{end - r:.1f} Q{end:.1f},{y:.1f} {end:.1f},{y + r:.1f} '
            f'V{y + h - r:.1f} Q{end:.1f},{y + h:.1f} {end - r:.1f},{y + h:.1f} H{base:.1f} Z" fill="{fill}"/>')

    def legend(self, x, y, items):
        for label, color in items:
            self.parts.append(f'<rect x="{x:.1f}" y="{y - 10:.1f}" width="12" height="12" rx="2" fill="{color}"/>')
            self.text(x + 18, y, label, size=12, fill=INK_2)
            x += 30 + 7.2 * len(label)

    def write(self, path: pathlib.Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(self.parts + ["</svg>"]) + "\n")


def header(svg: Svg, title: str, subtitle: str, source: str):
    svg.text(24, 34, title, size=17, weight="600")
    svg.text(24, 56, subtitle, size=12.5, fill=INK_2)
    svg.text(24, svg.h - 16, source, size=11, fill=INK_3)


def grouped_vbars(svg, groups, series, top, bottom, left, right, vmax, ticks, tick_fmt,
                  value_fmt, notes=None, inside=False):
    """groups: labels; series: [(name, color, values)]."""
    plot_h = bottom - top
    for t in ticks:
        y = bottom - plot_h * t / vmax
        svg.line(left, y, right, y)
        svg.text(left - 8, y + 4, tick_fmt(t), size=11, fill=INK_3, anchor="end")
    n = len(series)
    gw = (right - left) / len(groups)
    bw = min(46.0, (gw - 40) / n)
    for gi, g in enumerate(groups):
        gx = left + gi * gw + (gw - (n * bw + (n - 1) * 2)) / 2
        for si, (_, color, values) in enumerate(series):
            v = values[gi]
            x = gx + si * (bw + 2)
            y = bottom - plot_h * v / vmax
            svg.vbar(x, bottom, y, bw, color)
            if inside:
                svg.text(x + bw / 2, y + 18, value_fmt(v), size=11.5, fill="#ffffff",
                         anchor="middle", weight="600")
            else:
                svg.text(x + bw / 2, y - 6, value_fmt(v), size=11.5, anchor="middle")
        svg.text(left + gi * gw + gw / 2, bottom + 20, g, size=12.5, anchor="middle", weight="600")
        if notes:
            svg.text(left + gi * gw + gw / 2, bottom + 37, notes[gi], size=11.5, fill=INK_2, anchor="middle")
    svg.line(left, bottom, right, bottom, stroke=INK_3)


def requal_charts(agg: dict):
    rows = agg["trial_rows"]
    pairs = [p["pair_id"] for p in agg["pairs"]]
    label = {p: p.replace("A_cold_continuation#", "A ").replace("B_clear_survival#", "B ") for p in pairs}
    by = {(r["pair_id"], r["arm"]): r for r in rows}
    change = {p["pair_id"]: p["burden_change_pct"] for p in agg["pairs"]}
    verdict = {p["pair_id"]: p["verdict"] for p in agg["pairs"]}

    base = [by[(p, "baseline")]["total_input_tokens"] for p in pairs]
    velra = [by[(p, "velra")]["total_input_tokens"] for p in pairs]
    svg = Svg(W, 420, "Total input tokens per matched pair, v0.1.2 live qualification",
              "Grouped bars for four matched pairs. Baseline and Velra destination sessions: "
              + "; ".join(f"{label[p]} baseline {fmt_int(b)}, Velra {fmt_int(v)} ({change[p]:+.1f}%)"
                          for p, b, v in zip(pairs, base, velra)) + ".")
    header(svg, "Total input tokens of the destination session, per matched pair",
           "v0.1.2 live qualification · n = 4 pairs (8 trials) · Claude Code 2.1.280, Sonnet · build 51b96cb",
           "Source: bench/results/v0.1.2-requal/aggregate.json (uncached input + cache reads + cache creation)")
    svg.legend(24, 84, [("Baseline (fresh session)", ORANGE), ("Velra (fresh session + capsule)", BLUE)])
    notes = [f"{change[p]:+.1f}% · {verdict[p]}" for p in pairs]
    grouped_vbars(svg, [label[p] for p in pairs], [("baseline", ORANGE, base), ("velra", BLUE, velra)],
                  top=110, bottom=340, left=84, right=W - 24, vmax=500_000,
                  ticks=[0, 100_000, 200_000, 300_000, 400_000, 500_000],
                  tick_fmt=lambda t: f"{t // 1000}K", value_fmt=fmt_int, notes=notes)
    svg.write(OUT / "requal-total-input.svg")

    base_s = [by[(p, "baseline")]["first_correct_action"] for p in pairs]
    velra_s = [by[(p, "velra")]["first_correct_action"] for p in pairs]
    svg = Svg(W, 380, "Tool steps to the first correct action, v0.1.2 live qualification",
              "Grouped bars for four matched pairs: "
              + "; ".join(f"{label[p]} baseline {b}, Velra {v}" for p, b, v in zip(pairs, base_s, velra_s)) + ".")
    header(svg, "Tool steps before the first correct action",
           "v0.1.2 live qualification · n = 4 pairs (8 trials) · fewer is better · build 51b96cb",
           "Source: bench/results/v0.1.2-requal/aggregate.json (first Read/Edit/Grep of the target file)")
    svg.legend(24, 84, [("Baseline", ORANGE), ("Velra", BLUE)])
    grouped_vbars(svg, [label[p] for p in pairs], [("baseline", ORANGE, base_s), ("velra", BLUE, velra_s)],
                  top=110, bottom=320, left=64, right=W - 24, vmax=8, ticks=[0, 2, 4, 6, 8],
                  tick_fmt=str, value_fmt=str)
    svg.write(OUT / "requal-steps-to-first-correct-action.svg")


def replay_chart(rep: dict):
    trials = rep["trials"]
    labels = [t["trial"].replace("A_cold_continuation-", "A ").replace("B_clear_survival-", "B ")
              .replace("-velra", "") for t in trials]
    then = [t["frozen"]["tokens"] for t in trials]
    now = [t["staged"]["tokens"] for t in trials]
    notes = [f"{len(t['delivery']['markers_present'])}/{len(t['markers'])} markers delivered" for t in trials]
    svg = Svg(W, 420, "Capsule size from the frozen source ledgers: live-run build versus final build",
              "Grouped bars, estimated tokens per trial. "
              + "; ".join(f"{l}: live run {a}, final build {b}, {n}" for l, a, b, n in zip(labels, then, now, notes))
              + ". Dashed line: the 740-token default target.")
    header(svg, "Capsule size for the same source ledgers, before and after hardening",
           f"Velra-arm source ledgers of the live run, n = {len(trials)} · estimated tokens (Velra's estimator, not a tokenizer)",
           "Source: bench/results/v0.1.2-final/replay/replay.json · live-run capsules from bench/results/v0.1.2-requal/trials/*/velra_restore.json")
    svg.legend(24, 84, [("Live run (build 51b96cb)", ORANGE), ("Final build, replayed offline", BLUE)])
    svg.line(470, 80, 494, 80, stroke=INK_2, width=1.5, dash="6 4")
    svg.text(500, 84, "740 default target (axis top: 1,000 hard ceiling)", size=12, fill=INK_2)
    top, bottom, left, right, vmax = 110, 340, 64, W - 24, 1000
    grouped_vbars(svg, labels, [("then", ORANGE, then), ("now", BLUE, now)],
                  top=top, bottom=bottom, left=left, right=right, vmax=vmax,
                  ticks=[0, 250, 500, 750, 1000], tick_fmt=str, value_fmt=str, notes=notes,
                  inside=True)
    y = bottom - (bottom - top) * 740 / vmax
    svg.line(left, y, right, y, stroke=INK_2, width=1.5, dash="6 4")
    svg.write(OUT / "final-replay-capsule-size.svg")


def latency_chart():
    def pct(times, p):
        ms = sorted(t * 1000 for t in times)
        return ms[min(len(ms) - 1, int(round(p / 100 * (len(ms) - 1))))]

    rows = [("PostToolUse, 2 KiB read", "post-tool-use-small", "2 / 5"),
            ("PostToolUse, 50 KiB edit", "post-tool-use-edit", "3 / 6"),
            ("PreToolUse, edit", "pre-tool-use-edit", "3 / 6"),
            ("UserPromptSubmit", "user-prompt-submit", "2 / 4"),
            ("PreCompact", "pre-compact", "6 / 10"),
            ("PostToolUse, 8 MiB output", "post-tool-use-8mib", "25 / 25"),
            ("Kill switch (process floor)", "kill-switch-floor", "—")]
    data = []
    for label, name, budget in rows:
        times = json.loads((LATENCY / f"{name}.json").read_text(encoding="utf-8"))["results"][0]["times"]
        data.append((label, pct(times, 50), pct(times, 99), budget, len(times)))
    env = json.loads((LATENCY / "environment.json").read_text(encoding="utf-8"))
    n = data[0][4]

    left, right, top, rh = 210, W - 120, 110, 38
    h = top + rh * len(data) + 70
    svg = Svg(W, h, "Hook wall time on the final build, Windows 11, built-in timer",
              "Horizontal bars, p50 and p99 milliseconds per hook: "
              + "; ".join(f"{l} p50 {a:.2f} ms, p99 {b:.2f} ms" for l, a, b, _, _ in data) + ".")
    header(svg, "Hook wall time, spawn to exit, against a 100,000-event database",
           f"Final build · {env['platform']} · n = {n} runs per hook · Python built-in timer (indicative, includes process start)",
           "Source: bench/results/v0.1.2-final/latency/ (run.sh, spawn_floor.py) · budget: §4 p50 / p99 ms; run.sh allows 15 ms on Windows")
    svg.legend(24, 84, [("p50", BLUE), ("p99", ORANGE)])
    vmax = 50.0
    for t in (0, 10, 20, 30, 40, 50):
        x = left + (right - left) * t / vmax
        svg.line(x, top - 6, x, top + rh * len(data), stroke=GRID)
        svg.text(x, top + rh * len(data) + 16, f"{t} ms", size=11, fill=INK_3, anchor="middle")
    svg.text(W - 24, top - 12, "budget", size=11, fill=INK_3, anchor="end")
    for i, (label, p50, p99, budget, _) in enumerate(data):
        y = top + i * rh
        svg.text(left - 10, y + 20, label, size=12, anchor="end")
        bh = 12
        svg.hbar(left, y + 5, left + (right - left) * p50 / vmax, bh, BLUE)
        svg.hbar(left, y + 5 + bh + 2, left + (right - left) * p99 / vmax, bh, ORANGE)
        svg.text(left + (right - left) * p50 / vmax + 6, y + 15, f"{p50:.2f}", size=11)
        svg.text(left + (right - left) * p99 / vmax + 6, y + 29, f"{p99:.2f}", size=11)
        svg.text(W - 24, y + 22, budget, size=11.5, fill=INK_2, anchor="end")
    svg.line(left, top - 6, left, top + rh * len(data), stroke=INK_3)
    svg.write(OUT / "hook-latency-windows.svg")


def main():
    requal_charts(json.loads(REQUAL.read_text(encoding="utf-8")))
    replay_chart(json.loads(REPLAY.read_text(encoding="utf-8")))
    latency_chart()
    for p in sorted(OUT.glob("*.svg")):
        print(p.relative_to(ROOT).as_posix())


if __name__ == "__main__":
    main()
