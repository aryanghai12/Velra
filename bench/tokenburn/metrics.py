#!/usr/bin/env python3
"""From a parsed trial to the numbers the report is allowed to print.

Every quantity leaves here as a :class:`telemetry.Metric`, so the report cannot
print one without its source and its measurement status. Nothing in this module
estimates a token count. If the structured data did not carry a field, the
metric is ``unavailable`` and stays that way through aggregation.

The four primary metric families, named as the specification names them:

    A1  actual input burden      uncached input, cache reads, cache creation,
                                 totals, output, and the context size *actually
                                 observed* — never the size of a synthetic
                                 fixture
    A2  state recovery           did the destination identify the task, the
                                 active failure, the working files, the next
                                 action
    A3  recovery effort          turns, tool calls, file reads, steps to the
                                 first correct action
    A4  final correctness        the task, done right. A2 and A3 decide nothing
                                 on their own: a session that spent a tenth of
                                 the tokens and wrote the wrong fix has lost.

Scoring is driven by the scenario manifest carried in ``trial_meta.json``, not
by code branching on the scenario name. The manifest is written before the run
and hashed into the readiness report, so the rules cannot be fitted to results
afterwards, and the mock adapter and the live driver are scored by the same
declarations.
"""

from __future__ import annotations

import pathlib
import re
from typing import Iterable, Sequence

if __package__ in (None, ""):
    import sys
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import parse  # type: ignore[no-redef]
    import telemetry  # type: ignore[no-redef]
else:
    from . import parse, telemetry

Metric = telemetry.Metric
MetricSet = telemetry.MetricSet

#: Usage fields read straight out of structured telemetry, and the metric name
#: each becomes. ``total_input_tokens`` is derived, not read: see below.
USAGE_FIELDS = {
    "input_tokens": "input_tokens",
    "output_tokens": "output_tokens",
    "cache_read_input_tokens": "cache_read_input_tokens",
    "cache_creation_input_tokens": "cache_creation_input_tokens",
}


# --------------------------------------------------------------------------
# A1: input burden
# --------------------------------------------------------------------------


def sum_usage_field(records: Sequence[dict], field: str, name: str,
                    ref: str) -> Metric:
    """Sum one field across a trial's usage records, or refuse to.

    The rule is all-or-nothing on purpose. If eleven of twelve records carry
    ``cache_read_input_tokens`` and one does not, the sum of the eleven is not
    the trial's cache reads — it is the sum of an unknown subset, and reporting
    it as the total would understate the baseline's cached input, which is
    exactly the direction that flatters Velra. A partial field is reported
    UNAVAILABLE, with the count, so the gap is visible instead of absorbed.
    """
    if not records:
        return Metric.unavailable(
            name, "tokens", ref,
            "no usage-bearing record of any class in this trial")
    present = [r for r in records if field in r["usage"]]
    if not present:
        return Metric.unavailable(
            name, "tokens", ref,
            f"{field!r} was in none of the {len(records)} records of the "
            f"selected usage class")
    if len(present) != len(records):
        return Metric.unavailable(
            name, "tokens", ref,
            f"{field!r} was present in only {len(present)} of {len(records)} "
            f"records of the selected usage class; a partial sum is not this "
            f"trial's total")

    sources = {r["source"] for r in records}
    source = (telemetry.SOURCE_STRUCTURED_USAGE
              if telemetry.SOURCE_STRUCTURED_USAGE in sources
              else sorted(sources)[0])
    values = []
    for record in records:
        one = telemetry.usage_field(record["usage"], field, name, source,
                                    record["ref"])
        if not one.usable:
            return Metric.unavailable(name, "tokens", record["ref"], one.note)
        values.append(one.value)
    return Metric.measured(name, sum(values), "tokens", source, ref,
                           f"summed over {len(values)} structured usage records")


def input_burden(parsed: parse.ParsedTrial, into: MetricSet) -> MetricSet:
    """A1, in full, with every absence labelled."""
    ref = f"{parsed.trial_dir.name}/stream.jsonl"
    records = parsed.usage_records
    for field, name in USAGE_FIELDS.items():
        into.add(sum_usage_field(records, field, name, ref))

    # `input_tokens` in Anthropic's usage accounting is the *uncached* input.
    # The total a session paid for is that plus what it read from cache plus
    # what it wrote to cache, and it is only computable when all three exist.
    into.add(telemetry.total(
        "total_input_tokens",
        [into.get("input_tokens"), into.get("cache_read_input_tokens"),
         into.get("cache_creation_input_tokens")],
        "tokens", ref,
        note="uncached input + cache reads + cache creation"))

    into.add(Metric.measured("usage_records", len(records), "records",
                             telemetry.SOURCE_DERIVED, ref)
             if records else
             Metric.unavailable("usage_records", "records", ref,
                                "no usage telemetry of any kind"))
    return into


def context_sizes(parsed: parse.ParsedTrial, into: MetricSet) -> MetricSet:
    """Target, synthetic and actually-observed context size, kept apart.

    A synthetic fixture generated to 900 000 tokens is a fact about a file on
    disk. It is not an observation of a Claude session holding 900 000 tokens,
    and the two never share a metric name here.
    """
    fixture = parsed.context_fixture or {}
    fixture_ref = f"{parsed.trial_dir.name}/context_fixture.json"

    target = fixture.get("target_context_size")
    into.add(Metric.measured("target_context_size", target, "tokens",
                             telemetry.SOURCE_DERIVED, fixture_ref,
                             "a plan value, not an observation")
             if isinstance(target, (int, float)) else
             Metric.unavailable("target_context_size", "tokens", fixture_ref,
                                "this trial declared no context-load target"))

    # A PROXY, not a measurement, whenever the generator estimated it: it is a
    # file's character count divided by a declared chars-per-token ratio, and
    # calling that "measured" would be the first step towards quoting it as a
    # context-window observation.
    synthetic = fixture.get("synthetic_context_size")
    estimated = fixture.get("token_basis") == "estimated"
    if not isinstance(synthetic, (int, float)):
        into.add(Metric.unavailable("synthetic_context_size", "tokens",
                                    fixture_ref,
                                    "no synthetic load fixture was used"))
    elif estimated:
        into.add(Metric.proxy(
            "synthetic_context_size", synthetic, "tokens",
            telemetry.SOURCE_SYNTHETIC_FIXTURE, fixture_ref,
            f"generated fixture size, estimated at "
            f"{fixture.get('chars_per_token_assumed')} chars/token; NOT a "
            f"Claude observation"))
    else:
        into.add(Metric.measured(
            "synthetic_context_size", synthetic, "tokens",
            telemetry.SOURCE_SYNTHETIC_FIXTURE, fixture_ref,
            "generated fixture size; NOT a Claude observation"))

    observed = parse.context_observations(parsed.trial_dir)
    ref = f"{parsed.trial_dir.name}/stream.jsonl"
    if observed:
        last = observed[-1]
        source = last.get("source") or telemetry.SOURCE_STRUCTURED_USAGE
        into.add(Metric.measured("actual_observed_context_size", last["value"],
                                 "tokens", source,
                                 f"{ref}#{last['stream_index']}",
                                 f"structured field {last['field']!r}"))
    else:
        into.add(Metric.unavailable(
            "actual_observed_context_size", "tokens", ref,
            "no structured context-size field was present in the capture; "
            "the runtime does not expose one and it is not inferred"))

    # `achieved_context_size` is the observed one and nothing else. It exists
    # so the report has one name for "what the session actually held", and it
    # is unavailable exactly when that was not observed.
    achieved = into.get("actual_observed_context_size")
    into.add(Metric(
        "achieved_context_size", achieved.value, "tokens", achieved.source,
        achieved.raw_artifact_reference, achieved.measurement_status,
        "alias of actual_observed_context_size; never the synthetic size"))

    return into


def context_provenance(parsed: parse.ParsedTrial) -> dict:
    """How the load was produced, and why a target was not reached.

    Not a metric: neither field is a number, and forcing them into a
    :class:`Metric` would mean inventing a value to carry a string in its note.
    They belong to the trial's record beside the metrics.
    """
    fixture = parsed.context_fixture or {}
    return {
        "context_generation_method": fixture.get("context_generation_method"),
        "reason_if_target_not_reached": fixture.get(
            "reason_if_target_not_reached"),
        "token_basis": fixture.get("token_basis"),
        "chars_per_token_assumed": fixture.get("chars_per_token_assumed"),
        "fixture_kind": fixture.get("kind"),
    }


def cache_state(into: MetricSet) -> dict:
    """HIT / MISS / UNKNOWN, decided only by structured cache fields."""
    return telemetry.cache_condition(into.get("cache_read_input_tokens"),
                                     into.get("cache_creation_input_tokens"))


def capsule_size(parsed: parse.ParsedTrial, into: MetricSet) -> MetricSet:
    """How big the capsule that crossed the boundary was.

    A real tokenizer measurement, when one exists on disk, is MEASURED. Velra's
    own estimate from the staged record is a PROXY and is labelled as one --
    it is the renderer's budget arithmetic, not a tokenizer's answer, and the
    report must not present the two as the same kind of number.
    """
    ref = f"{parsed.trial_dir.name}/capsule_tokens.json"
    measured = telemetry.read_json(parsed.trial_dir / "capsule_tokens.json")
    if isinstance(measured, dict) and isinstance(measured.get("measured_tokens"), int):
        into.add(Metric.measured("capsule_tokens", measured["measured_tokens"],
                                 "tokens", telemetry.SOURCE_STRUCTURED_USAGE,
                                 ref, f"tokenizer: {measured.get('model')}"))
    else:
        staged = (parsed.restore or {}).get("staged") or {}
        estimate = staged.get("tokens")
        restore_ref = f"{parsed.trial_dir.name}/velra_restore.json"
        if isinstance(estimate, int):
            into.add(Metric.proxy("capsule_tokens", estimate, "tokens",
                                  telemetry.SOURCE_SESSION_ARTIFACT, restore_ref,
                                  "Velra's own render-budget estimate, not a "
                                  "tokenizer measurement"))
        else:
            into.add(Metric.unavailable("capsule_tokens", "tokens", ref,
                                        "no tokenizer measurement and no "
                                        "staged record to fall back on"))

    delivered = [d for d in parsed.deliveries]
    chars = max((d.chars for d in delivered), default=None)
    into.add(Metric.measured("capsule_chars", chars, "characters",
                             telemetry.SOURCE_STRUCTURED_USAGE,
                             f"{parsed.trial_dir.name}/stream.jsonl")
             if chars is not None else
             Metric.unavailable("capsule_chars", "characters",
                                f"{parsed.trial_dir.name}/stream.jsonl",
                                "no capsule reached this session"))
    return into


# --------------------------------------------------------------------------
# A2: state recovery
# --------------------------------------------------------------------------


def _norm(text: str) -> str:
    return re.sub(r"\s+", " ", (text or "").replace("\\", "/")).lower()


def _any_marker(haystack: str, markers: Iterable[str]) -> list[str]:
    low = _norm(haystack)
    return [m for m in markers if _norm(m) and _norm(m) in low]


def state_recovery(parsed: parse.ParsedTrial, manifest: dict) -> dict:
    """A2: did the destination identify the four things it needed?

    Scored over the assistant prose *and* the targets of the tools it used: an
    agent that opens the right file without narrating it has identified the
    working file, and demanding that it say so would score prose style.
    """
    declared = (manifest.get("state_recovery") or {})
    surface = parsed.assistant_text + "\n" + "\n".join(
        f"{a.tool} {a.target}" for a in parsed.actions if a.kind == "tool")
    per_item = {}
    for key in ("current_task", "active_failure", "relevant_files",
                "next_action"):
        markers = declared.get(key) or []
        if not markers:
            per_item[key] = {"declared": False, "identified": None,
                             "markers_found": [],
                             "why": "the scenario declared no markers for this"}
            continue
        hits = _any_marker(surface, markers)
        per_item[key] = {"declared": True, "identified": bool(hits),
                         "markers": list(markers), "markers_found": hits}
    scored = [v for v in per_item.values() if v["declared"]]
    return {
        "per_item": per_item,
        "declared_items": len(scored),
        "identified_items": sum(1 for v in scored if v["identified"]),
        "complete": bool(scored) and all(v["identified"] for v in scored),
    }


# --------------------------------------------------------------------------
# A3: recovery effort
# --------------------------------------------------------------------------


def first_correct_action(parsed: parse.ParsedTrial, manifest: dict) -> dict:
    """The first action that actually uses the state the scenario is about.

    Declared as a tool set and a target substring set, because "correct first
    action" has to be a fact about the transcript rather than a judgement made
    after reading it. ``steps`` counts every action before it, so a session
    that opens the right file immediately scores 0.
    """
    spec = manifest.get("first_correct_action") or {}
    tools = spec.get("tools") or []
    targets = spec.get("target_contains") or []
    forbidden = spec.get("must_not_precede") or []
    if not tools and not targets:
        return {"declared": False, "found": None, "steps": None,
                "why": "the scenario declared no first-correct-action rule"}

    violated_at = None
    for action in parsed.actions:
        if action.kind != "tool":
            continue
        target = _norm(action.target or "")
        if forbidden and any(_norm(f) in target for f in forbidden):
            violated_at = violated_at if violated_at is not None else action.index
        tool_ok = (not tools) or action.tool in tools
        target_ok = (not targets) or any(_norm(t) in target for t in targets)
        if tool_ok and target_ok:
            steps = sum(1 for a in parsed.actions
                        if a.kind == "tool" and a.index < action.index)
            return {"declared": True, "found": True, "steps": steps,
                    "action_index": action.index, "tool": action.tool,
                    "target": action.target,
                    "dead_end_touched_first": (violated_at is not None
                                               and violated_at < action.index)}
    return {"declared": True, "found": False, "steps": None,
            "dead_end_touched_first": violated_at is not None,
            "why": "no action in this session matched the declared rule"}


def recovery_effort(parsed: parse.ParsedTrial, manifest: dict,
                    into: MetricSet) -> dict:
    ref = f"{parsed.trial_dir.name}/stream.jsonl"
    into.add(Metric.measured("turns", parsed.turns, "turns",
                             telemetry.SOURCE_STRUCTURED_USAGE, ref))
    into.add(Metric.measured("tool_calls", parsed.tool_calls, "calls",
                             telemetry.SOURCE_STRUCTURED_USAGE, ref))
    into.add(Metric.measured("file_reads", parsed.file_reads, "reads",
                             telemetry.SOURCE_STRUCTURED_USAGE, ref))
    into.add(Metric.measured("searches", parsed.searches, "searches",
                             telemetry.SOURCE_STRUCTURED_USAGE, ref))
    first = first_correct_action(parsed, manifest)
    into.add(Metric.measured("steps_to_first_correct_action", first["steps"],
                             "steps", telemetry.SOURCE_STRUCTURED_USAGE, ref)
             if first.get("steps") is not None else
             Metric.unavailable("steps_to_first_correct_action", "steps", ref,
                                first.get("why") or "never reached"))
    return first


# --------------------------------------------------------------------------
# A4: final correctness
# --------------------------------------------------------------------------


def final_correctness(parsed: parse.ParsedTrial, manifest: dict) -> dict:
    """The task, judged from the end state, by rules written before the run.

    Four independent checks, all of which must hold:

      * every required file ends up containing what it must and none of what
        it must not;
      * the suite passes, when the scenario says it must;
      * the invariant that existed only in the old conversation is respected;
      * the dead end that was already eliminated was not re-entered.

    A session that reduced its input burden and failed any of these has not
    won anything, and the verdict logic treats it accordingly.
    """
    spec = manifest.get("final_correctness") or {}
    files = parsed.final_state.get("files") or {}
    checks: dict[str, dict] = {}

    required = spec.get("required_final_state") or []
    file_results = []
    for entry in required:
        rel = entry.get("file")
        content = files.get(rel)
        if content is None:
            file_results.append({"file": rel, "ok": False,
                                 "why": "file was not captured in final_state"})
            continue
        low = _norm(content)
        missing = [s for s in (entry.get("must_contain") or [])
                   if _norm(s) not in low]
        present = [s for s in (entry.get("must_not_contain") or [])
                   if _norm(s) in low]
        file_results.append({"file": rel, "ok": not missing and not present,
                             "missing": missing, "forbidden_present": present})
    checks["required_final_state"] = {
        "declared": bool(required),
        "ok": all(r["ok"] for r in file_results) if required else None,
        "files": file_results,
    }

    # The scenario's own failing test, run on its own. The suite as a whole
    # stays red either way -- two unrelated failures are deliberately left in
    # the fixture -- so the suite's exit code cannot be the correctness signal
    # and the target node id is.
    target_required = bool(spec.get("target_test_must_pass"))
    target_exit = parsed.final_state.get("target_exit")
    checks["target_test"] = {
        "declared": target_required,
        "ok": (target_exit == 0) if target_required else None,
        "node_id": manifest.get("target_test"),
        "exit": target_exit,
        "tail": (parsed.final_state.get("target_tail") or [])[:5],
    }

    # A fix that turns the target green by breaking something else is not a
    # fix. The ceiling is the number of failures the fixture ships with, minus
    # the one under repair.
    ceiling = spec.get("max_suite_failures")
    failures = parsed.final_state.get("suite_failures")
    checks["no_regression"] = {
        "declared": ceiling is not None,
        "ok": (isinstance(failures, int) and failures <= ceiling)
        if ceiling is not None else None,
        "suite_failures": failures,
        "ceiling": ceiling,
        "suite_exit": parsed.final_state.get("suite_exit"),
    }

    invariant = manifest.get("invariant") or {}
    if invariant.get("must_hold_in_file"):
        rel = invariant["must_hold_in_file"]
        content = files.get(rel)
        low = _norm(content or "")
        missing = [s for s in (invariant.get("must_contain") or [])
                   if _norm(s) not in low]
        forbidden = [s for s in (invariant.get("must_not_contain") or [])
                     if _norm(s) in low]
        checks["invariant"] = {"declared": True, "file": rel,
                               "ok": content is not None and not missing
                               and not forbidden,
                               "missing": missing, "forbidden_present": forbidden}
    else:
        checks["invariant"] = {"declared": False, "ok": None}

    dead_end = manifest.get("dead_end") or {}
    forbidden_paths = dead_end.get("must_not_edit") or []
    edited = [_norm(a.target or "") for a in parsed.actions
              if a.kind == "tool" and a.tool in parse.EDIT_TOOLS]
    touched = sorted({p for p in forbidden_paths
                      if any(_norm(p) in e for e in edited)})
    checks["dead_end_avoided"] = {
        "declared": bool(forbidden_paths),
        "ok": (not touched) if forbidden_paths else None,
        "touched": touched,
    }

    decided = [c for c in checks.values() if c.get("ok") is not None]
    return {
        "checks": checks,
        "declared_checks": len(decided),
        "correct": bool(decided) and all(c["ok"] for c in decided),
        "evaluable": bool(decided),
    }


# --------------------------------------------------------------------------
# the whole trial
# --------------------------------------------------------------------------


def evaluate(parsed: parse.ParsedTrial) -> dict:
    """Every metric for one trial, with the manifest it was scored against."""
    manifest = (parsed.meta.get("manifest") or {})
    metrics = MetricSet()
    input_burden(parsed, metrics)
    context_sizes(parsed, metrics)
    capsule_size(parsed, metrics)
    first = recovery_effort(parsed, manifest, metrics)
    recovery = state_recovery(parsed, manifest)
    correctness = final_correctness(parsed, manifest)
    cache = cache_state(metrics)

    missing = metrics.missing(("input_tokens", "output_tokens"))
    measurement_status = (telemetry.STATUS_MEASURED if not missing
                          else telemetry.STATUS_INCONCLUSIVE)
    if not parsed.capture_usable:
        measurement_status = telemetry.STATUS_INCONCLUSIVE

    return {
        "trial": parsed.trial_dir.name,
        "scenario": parsed.scenario,
        "arm": parsed.arm,
        "pair_id": parsed.pair_id,
        "replicate": parsed.replicate,
        "pair_key": dict(parsed.meta.get("pair_key") or {}),
        "source_session": parsed.source_session_id,
        "destination_session": parsed.destination_session_id,
        "telemetry_source": parsed.telemetry_source,
        "usage_selection": parsed.usage_selection,
        "measurement_status": measurement_status,
        "missing_primary_metrics": missing,
        "cache_condition": cache,
        "context_provenance": context_provenance(parsed),
        "metrics": metrics.to_json(),
        "state_recovery": recovery,
        "first_correct_action": first,
        "final_correctness": correctness,
        "capture": {
            "total_lines": parsed.total_lines,
            "malformed_lines": len(parsed.malformed_lines),
            "malformed_fraction": round(parsed.malformed_fraction, 4),
            "duplicates_removed": parsed.duplicates_removed,
            "usable": parsed.capture_usable,
        },
        "deliveries": [d.to_json() for d in parsed.deliveries],
    }
