#!/usr/bin/env python3
"""Decide each hypothesis from the collected evidence and print the summary.

Two questions are kept apart deliberately, because collapsing them is how
benchmarks end up dishonest:

  target met      did Velra do what it claims to do, in absolute terms?
  beat baseline   did it do something vanilla Claude Code failed to do?

A hypothesis can pass the first and fail the second. That is a real outcome and
this script reports it as one rather than rounding it up to a win.

Hypotheses 1 and 2 additionally depend on a control: they are only meaningful
if Claude Code's own compaction actually loses the information in question. If
bench/results/compaction_probe.json says compaction was not lossy, those two
are reported as INCONCLUSIVE no matter how the arms scored.
"""

from __future__ import annotations

import json
import pathlib
import sys

RESULTS = pathlib.Path(__file__).resolve().parent.parent / "results"

PASS, FAIL, INCONCLUSIVE = "PASSED", "FAILED", "INCONCLUSIVE"


def load(name: str):
    path = RESULTS / name
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def load_token_measurements() -> list[dict]:
    out = []
    trials = RESULTS / "trials"
    if trials.exists():
        for d in sorted(trials.iterdir()):
            tok = d / "token_measurement.json"
            if tok.exists():
                data = json.loads(tok.read_text(encoding="utf-8"))
                data["trial"] = d.name
                out.append(data)
    return out


def rule(char="-", width=78):
    return char * width


def main() -> int:
    phase1 = load("phase1_environment.json")
    agg = load("aggregate.json")
    overhead = load("hook_overhead.json")
    probe = load("compaction_probe.json")
    tokens = load_token_measurements()

    print(rule("="))
    print("  VELRA v0.1 - EMPIRICAL BENCHMARK: FINAL EVALUATION")
    print(rule("="))

    verdicts: dict[str, dict] = {}

    # ---- the control -----------------------------------------------------
    print("\nCONTROL - was Claude Code's own compaction lossy at all?")
    if not probe:
        print("  no compaction probe was run; hypotheses 1 and 2 cannot be decided")
        lossy = None
    else:
        lossy = probe.get("compaction_is_lossy")
        print(f"  context before /compact:  {probe.get('context_before_compaction')} tokens")
        print(f"  context after  /compact:  {probe.get('context_after_compaction')} tokens "
              f"({probe.get('context_reduction_pct')}% smaller)")
        print(f"  verbatim detail recalled: {probe.get('canary_recalled')} "
              f"(answered {probe.get('canary_answer','')[:40]!r})")
        print(f"  -> compaction is lossy:   {lossy}")

    # ---- pick the arms ---------------------------------------------------
    groups = (agg or {}).get("groups", {})
    protocol = None
    for candidate in ("saturated", "short"):
        if f"{candidate}/velra" in groups and f"{candidate}/baseline" in groups:
            protocol = candidate
            break
    base = groups.get(f"{protocol}/baseline") if protocol else None
    velra = groups.get(f"{protocol}/velra") if protocol else None

    # ---- H1 --------------------------------------------------------------
    print("\n" + rule())
    print("HYPOTHESIS 1 - Compaction amnesia elimination")
    print("  Target: post-compaction source re-reads = 0, and the first edit")
    print("          lands on the failing symbol.")
    if not (base and velra):
        verdicts["H1"] = {"verdict": INCONCLUSIVE, "why": "no paired trials"}
        print("  no paired trials found")
    else:
        n = velra["n"]
        target_met = (velra["source_rereads_mean"] == 0
                      and velra["hit_true_symbol"] == n)
        beat = (base["source_rereads_mean"] > velra["source_rereads_mean"]
                or base["hit_true_symbol"] < velra["hit_true_symbol"])
        print(f"  Velra    : re-reads {velra['source_rereads_mean']} "
              f"{velra['source_rereads_each']}, hit symbol {velra['hit_true_symbol']}/{n}, "
              f"tools {velra['tool_calls_mean']}")
        print(f"  Baseline : re-reads {base['source_rereads_mean']} "
              f"{base['source_rereads_each']}, hit symbol {base['hit_true_symbol']}/{base['n']}, "
              f"tools {base['tool_calls_mean']}")
        if lossy is False:
            v = INCONCLUSIVE
            why = ("compaction was not lossy in this harness, so the "
                   "post-compaction turn did not test recall")
        elif target_met and beat:
            v, why = PASS, "target met and baseline beaten"
        elif target_met:
            v, why = PASS, ("target met in absolute terms; the baseline met it "
                            "too, so no improvement over vanilla was "
                            "demonstrated on this task")
        else:
            v, why = FAIL, "target not met"
        verdicts["H1"] = {"verdict": v, "why": why, "target_met": target_met,
                          "beat_baseline": beat}
        print(f"  -> {v}: {why}")

    # ---- H2 --------------------------------------------------------------
    print("\n" + rule())
    print("HYPOTHESIS 2 - Dead-end loop prevention")
    print("  Target: 0% re-exploration of the reverted approach, and the dead")
    print("          end recorded in the capsule.")
    if not (base and velra):
        verdicts["H2"] = {"verdict": INCONCLUSIVE, "why": "no paired trials"}
        print("  no paired trials found")
    else:
        n = velra["n"]
        in_db = 0
        in_capsule = 0
        trials = RESULTS / "trials"
        for d in sorted(trials.glob("*velra*")):
            a = d / "analysis.json"
            if not a.exists():
                continue
            data = json.loads(a.read_text(encoding="utf-8"))
            db = data.get("velra_db") or {}
            if db.get("present") and db.get("dead_ends"):
                in_db += 1
            # The claim is about the capsule, not the database. A dead end can
            # sit in SQLite and still be filtered out of what is delivered.
            cap = data.get("delivered_capsule") or {}
            if cap.get("has_dead_ends_section") and cap.get("dead_end_approach_in_capsule"):
                in_capsule += 1

        target_met = (velra["dead_end_reexplored"] == 0 and in_capsule == n)
        beat = base["dead_end_reexplored"] > velra["dead_end_reexplored"]
        print(f"  Velra    : re-explored {velra['dead_end_reexplored']}/{n}; "
              f"dead end in the database {in_db}/{n}, "
              f"but in the DELIVERED capsule {in_capsule}/{n}")
        print(f"  Baseline : re-explored {base['dead_end_reexplored']}/{base['n']}")
        if in_db > in_capsule:
            print(f"  note     : {in_db - in_capsule} trial(s) recorded the dead end "
                  f"but did not deliver it -- snapshot.rs filters dead ends on "
                  f"`reapplied = 0`, and\n             an out-of-order git_pre "
                  f"observation set that flag falsely.")
        if lossy is False:
            v, why = INCONCLUSIVE, ("compaction was not lossy, so nothing was "
                                    "forgotten for either arm to re-explore")
        elif target_met and beat:
            v, why = PASS, "target met and baseline beaten"
        elif target_met:
            v, why = PASS, ("target met in absolute terms; the baseline also "
                            "re-explored nothing, so no improvement over "
                            "vanilla was demonstrated")
        elif in_capsule < n:
            v, why = FAIL, (f"the reverted approach reached the capsule in only "
                            f"{in_capsule}/{n} trials; neither arm re-explored it, "
                            f"but Velra did not deliver what the hypothesis claims "
                            f"it delivers")
        else:
            v, why = FAIL, "target not met"
        verdicts["H2"] = {"verdict": v, "why": why,
                          "dead_end_in_db": in_db, "dead_end_in_capsule": in_capsule,
                          "replicates": n}
        print(f"  -> {v}: {why}")

    # ---- H3 --------------------------------------------------------------
    print("\n" + rule())
    print("HYPOTHESIS 3 - Continuation budget <= 800 tokens")
    if not tokens:
        verdicts["H3"] = {"verdict": INCONCLUSIVE, "why": "no token measurement"}
        print("  no token measurement was taken")
    else:
        rows = [(t["trial"], inj) for t in tokens for inj in t.get("injections", [])]
        controls_ok = all(t.get("control_valid") for t in tokens)
        worst = max((inj["measured_tokens"] for _, inj in rows), default=None)
        for trial, inj in rows:
            print(f"  {trial}: {inj['context_chars']} chars -> "
                  f"{inj['measured_tokens']} tokens "
                  f"(budget {inj['budget_tokens']}) via {inj['hook_name']}")
        print(f"  measurement control valid (identical prompts differ by 0): {controls_ok}")
        ok = worst is not None and worst <= 800 and controls_ok
        verdicts["H3"] = {"verdict": PASS if ok else FAIL,
                          "worst_tokens": worst, "control_valid": controls_ok}
        print(f"  -> {PASS if ok else FAIL}: worst measured block is {worst} tokens")

    # ---- H4 --------------------------------------------------------------
    print("\n" + rule())
    print("HYPOTHESIS 4 - Zero-overhead fail-open guarantee")
    clean = []
    trials = RESULTS / "trials"
    for d in sorted(trials.glob("*velra*")):
        a = d / "analysis.json"
        if not a.exists():
            continue
        h = json.loads(a.read_text(encoding="utf-8")).get("hooks") or {}
        if h.get("observed"):
            clean.append(h)
    nonzero = sum(h["nonzero_exit"] for h in clean)
    stderrs = sum(h["responses_with_stderr"] for h in clean)
    observed = sum(h["observed"] for h in clean)
    print(f"  in-session hook invocations observed: {observed}")
    print(f"  non-zero exits: {nonzero}     stderr writes: {stderrs}")
    if clean:
        worst_sync = max((h["sync_p99_ms"] for h in clean
                          if h.get("sync_p99_ms") is not None), default=None)
        print(f"  synchronous in-session p99 (includes Claude Code dispatch): "
              f"{worst_sync} ms")
    fail_open_ok = (nonzero == 0 and stderrs == 0 and observed > 0)
    print(f"  (a) fail-open, no pollution: "
          f"{PASS if fail_open_ok else FAIL if observed else INCONCLUSIVE}")

    if overhead:
        print(f"\n  (b) latency, measured directly against a "
              f"{overhead['database_bytes']/1024/1024:.0f} MiB database "
              f"({overhead['seeded_events']} events, {overhead['runs_per_case']} runs/case):")
        print(f"    process spawn control: p50 "
              f"{overhead['spawn_control']['p50_ms']} ms, "
              f"p99 {overhead['spawn_control']['p99_ms']} ms  "
              f"<- the cost of starting any process here")
        marginal_budget = overhead.get("windows_marginal_budget_p99_ms", 15)
        for case in overhead["cases"]:
            flag = " OVER" if case.get("marginal_p99_ms", 0) > marginal_budget else ""
            print(f"    {case['label']:34} p50 {case['p50_ms']:8.3f} ms  "
                  f"p99 {case['p99_ms']:8.3f} ms  "
                  f"marginal p50 {case['marginal_p50_ms']:+8.3f} ms{flag}")
        wall_ok = overhead["within_windows_budget"]
        worst = overhead["worst_case_p99_ms"]
        spawn_p99 = overhead["spawn_control"]["p99_ms"]
        # Two numbers, and they answer different questions. The spec's 15 ms is
        # wall time, and on Windows an empty process can exceed it on its own --
        # the spawn control above is the proof -- so a wall-time verdict there
        # grades the operating system, not Velra. The hypothesis is decided on
        # marginal cost (DECISIONS.md D54); the wall-time figure is printed
        # every time beside it, never replaced by it.
        budget_ok = overhead.get("within_windows_marginal_budget")
        worst_marginal = overhead.get("worst_case_marginal_p99_ms")
        print(f"    worst-case p99, total:    {worst} ms against the spec's "
              f"{overhead['windows_budget_p99_ms']} ms wall-time budget -> "
              f"{'WITHIN' if wall_ok else 'OVER'}")
        print(f"    worst-case p99, marginal: {worst_marginal} ms against a "
              f"{marginal_budget} ms marginal budget -> "
              f"{'WITHIN' if budget_ok else 'OVER'}")
        if not wall_ok:
            print(f"    the spawn control alone is {spawn_p99} ms of the total, so the "
                  f"tail is dominated by\n    Windows process creation rather than by "
                  f"Velra's own work; the verdict is taken on the marginal figure.")
    else:
        budget_ok = None
        wall_ok = None
        worst_marginal = None
        print("  (b) no direct overhead benchmark was run")

    ok = fail_open_ok and budget_ok is True
    verdicts["H4"] = {
        "verdict": PASS if ok else (FAIL if budget_ok is False else INCONCLUSIVE),
        "fail_open": PASS if fail_open_ok else FAIL,
        "latency_budget": (PASS if budget_ok else FAIL) if budget_ok is not None else INCONCLUSIVE,
        "nonzero_exits": nonzero, "stderr_writes": stderrs,
        "within_budget": budget_ok,
        "within_wall_time_budget": wall_ok,
        "worst_case_p99_ms": (overhead or {}).get("worst_case_p99_ms"),
        "worst_case_marginal_p99_ms": worst_marginal,
        "spawn_control_p99_ms": ((overhead or {}).get("spawn_control") or {}).get("p99_ms"),
        "budget_basis": "marginal cost over the spawn control (DECISIONS.md D54)",
        "why": ("the fail-open half holds without exception; Velra's marginal "
                "cost is within budget, but total wall time is not -- on this "
                "machine starting any process at all costs more than the "
                "spec's wall-time budget at the tail")
        if (fail_open_ok and budget_ok is True and wall_ok is False) else
        ("the fail-open half holds without exception; Velra's own marginal "
         "cost exceeds the budget")
        if (fail_open_ok and budget_ok is False) else None,
    }
    print(f"  -> {verdicts['H4']['verdict']}")

    # ---- summary ---------------------------------------------------------
    print("\n" + rule("="))
    print("  SUMMARY")
    print(rule("="))
    titles = {
        "H1": "Compaction amnesia elimination",
        "H2": "Dead-end loop prevention",
        "H3": "Continuation budget <= 800 tokens",
        "H4": "Zero-overhead fail-open guarantee",
    }
    for key in ("H1", "H2", "H3", "H4"):
        v = verdicts.get(key, {"verdict": INCONCLUSIVE})
        print(f"  {key}  {titles[key]:42} {v['verdict']}")
    if phase1:
        print(f"\n  Phase 1 environment checks: "
              f"{'PASSED' if phase1.get('all_checks_passed') else 'FAILED'}")
    print(rule("="))

    (RESULTS / "verdicts.json").write_text(
        json.dumps(verdicts, indent=2), encoding="utf-8", newline="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
