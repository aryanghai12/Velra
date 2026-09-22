#!/usr/bin/env python3
"""The production parser: raw trial artifacts in, one structured record out.

Everything downstream — telemetry, metrics, the causal chain, aggregation, the
verdict — reads :class:`ParsedTrial` and never the files. That is what lets
:mod:`mock_adapter` exercise the real evaluator: it writes the same artifacts a
live trial writes, into the same layout, and this module cannot tell the
difference except by the ``telemetry_source`` recorded in them.

The trial directory
-------------------

::

    <trial>/
      trial_meta.json        identity, arm, pairing, declared operational state
      stream.jsonl           the destination session, Claude Code stream-json
      source_stream.jsonl    the source session, same format          (optional)
      transcript.jsonl       the destination session artifact         (optional)
      usage.json             structured usage rollup                  (optional)
      velra_restore.json     restore + staging + claim lifecycle      (optional)
      final_state.json       end state and task-correctness checks    (optional)
      context_fixture.json   which load fixture this trial ran under  (optional)

Every one of those but ``trial_meta.json`` is optional, and an absent artifact
produces unavailable metrics and a broken causal link rather than an exception.
A benchmark that crashes on a half-captured trial loses the trials either side
of it too.

Three properties this parser is responsible for
-----------------------------------------------

**Duplicates are removed, once.** Claude Code's stream can repeat an event when
a connection is retried, and a duplicated ``result`` event would double a
session's token count. Events are keyed on the strongest identity they carry —
``uuid``, then a message id, then a content digest — and the number removed is
reported rather than hidden.

**Malformed lines do not stop the analysis.** They are counted, sampled and
carried into the record, and a trial whose capture is more than
:data:`MALFORMED_FRACTION_LIMIT` garbage is marked unusable *explicitly*.

**stdout is a JSON field here, never a surface to scrape.** Hook responses are
located by ``subtype == "hook_response"`` and their capsule read out of
``json.loads(event["stdout"])["hookSpecificOutput"]["additionalContext"]``.
That is structured data that happens to be nested. No number is ever recovered
by matching text.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import pathlib
from typing import Any, Sequence

if __package__ in (None, ""):
    import sys
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import telemetry  # type: ignore[no-redef]
else:
    from . import telemetry

Metric = telemetry.Metric

#: A capture with more malformed lines than this is not evidence.
MALFORMED_FRACTION_LIMIT = 0.25

#: Tools that read the repository. ``file_reads`` counts the first group only;
#: searches are counted separately because "how many files did it open" and
#: "how hard did it look" are different questions.
READ_TOOLS = ("Read", "NotebookRead")
SEARCH_TOOLS = ("Grep", "Glob", "WebSearch")
EDIT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")

#: The tag that opens a rendered Velra capsule. Used to recognise a hook
#: response as a delivery without depending on the hook's name.
CAPSULE_OPEN = "<VELRA_WORKSPACE_STATE"

#: ``SessionStart`` sources. ``startup`` is the one a staged capsule is for.
SOURCE_STARTUP = "startup"

#: The kinds of usage-bearing record a trial can hold. They are alternative
#: reports of the same spend, never additive parts of it.
USAGE_CLASS_RESULT = "result"
USAGE_CLASS_ASSISTANT = "assistant"
USAGE_CLASS_ROLLUP = "rollup"
USAGE_CLASS_TRANSCRIPT = "transcript"

#: Which class is believed when a trial carries more than one, best first.
#:
#: ``result`` is one record per completed turn and is Claude Code's own
#: accounting for that turn, which is the quantity the benchmark reports.
#: ``assistant`` is per message and sums to the same total by a different
#: route. A rollup is written by the driver from what it saw. The session
#: artifact is last because it is the furthest from the execution interface.
USAGE_CLASS_PREFERENCE = (USAGE_CLASS_RESULT, USAGE_CLASS_ROLLUP,
                          USAGE_CLASS_ASSISTANT, USAGE_CLASS_TRANSCRIPT)


# --------------------------------------------------------------------------
# records
# --------------------------------------------------------------------------


@dataclasses.dataclass
class Delivery:
    """One capsule that reached a session through a hook response."""

    stream_index: int
    hook_name: str | None
    hook_event: str | None
    session_start_source: str | None
    exit_code: int | None
    chars: int
    text: str
    system_message: str | None = None

    @property
    def on_startup(self) -> bool:
        label = (self.session_start_source or self.hook_name or "").lower()
        return SOURCE_STARTUP in label

    def to_json(self) -> dict:
        out = dataclasses.asdict(self)
        out.pop("text")
        out["on_startup"] = self.on_startup
        return out


@dataclasses.dataclass
class Action:
    """One thing the agent did, in order, in the measured stretch."""

    index: int
    turn: int
    kind: str          # "tool" | "text"
    tool: str | None
    target: str | None  # file path, command, or pattern
    text: str

    def to_json(self) -> dict:
        out = dataclasses.asdict(self)
        out["text"] = self.text[:400]
        return out


@dataclasses.dataclass
class ParsedTrial:
    """Everything the rest of the pipeline is allowed to look at."""

    trial_dir: pathlib.Path
    meta: dict
    scenario: str
    arm: str
    pair_id: str | None
    replicate: int | None

    session_id: str | None
    source_session_id: str | None
    destination_session_id: str | None

    turns: int
    tool_calls: int
    file_reads: int
    searches: int
    edits: int
    actions: list[Action]
    assistant_text: str

    deliveries: list[Delivery]
    usage_records: list[dict]
    #: Which class of usage record the totals came from, and what was rejected.
    usage_selection: dict

    malformed_lines: list[dict]
    total_lines: int
    duplicates_removed: int

    restore: dict
    final_state: dict
    context_fixture: dict

    @property
    def malformed_fraction(self) -> float:
        return (len(self.malformed_lines) / self.total_lines) if self.total_lines else 0.0

    @property
    def capture_usable(self) -> bool:
        return self.malformed_fraction <= MALFORMED_FRACTION_LIMIT

    @property
    def telemetry_source(self) -> str:
        """Where this trial's usage numbers came from, as one label."""
        sources = {r["source"] for r in self.usage_records}
        for preferred in (telemetry.SOURCE_STRUCTURED_USAGE,
                          telemetry.SOURCE_SESSION_ARTIFACT,
                          telemetry.SOURCE_SYNTHETIC_FIXTURE):
            if preferred in sources:
                return preferred
        return telemetry.SOURCE_UNAVAILABLE

    def to_json(self) -> dict:
        return {
            "trial": self.trial_dir.name,
            "scenario": self.scenario,
            "arm": self.arm,
            "pair_id": self.pair_id,
            "replicate": self.replicate,
            "session_id": self.session_id,
            "source_session_id": self.source_session_id,
            "destination_session_id": self.destination_session_id,
            "turns": self.turns,
            "tool_calls": self.tool_calls,
            "file_reads": self.file_reads,
            "searches": self.searches,
            "edits": self.edits,
            "deliveries": [d.to_json() for d in self.deliveries],
            "usage_record_count": len(self.usage_records),
            "usage_selection": self.usage_selection,
            "telemetry_source": self.telemetry_source,
            "malformed_lines": len(self.malformed_lines),
            "malformed_sample": self.malformed_lines[:5],
            "malformed_fraction": round(self.malformed_fraction, 4),
            "capture_usable": self.capture_usable,
            "duplicates_removed": self.duplicates_removed,
            "total_lines": self.total_lines,
            "restore": self.restore,
            "context_fixture": self.context_fixture,
            "actions": [a.to_json() for a in self.actions[:200]],
        }


# --------------------------------------------------------------------------
# deduplication
# --------------------------------------------------------------------------


def event_identity(event: dict) -> str:
    """The strongest identity an event carries, for duplicate removal.

    Preference order matters. ``uuid`` is unique per emitted event. A message
    id plus the event type is unique per message but repeats across the
    ``assistant``/``result`` pair that quotes the same message, so the type is
    part of the key. Failing both, a digest of the sorted JSON is used, which
    means two genuinely identical events one after another collapse into one —
    correct for a retry, and the only case where it is wrong is a capture that
    legitimately repeats a byte-identical event, which Claude Code's stream
    does not do because every event carries a distinct timestamp or index.
    """
    for key in ("uuid", "event_id", "hook_id"):
        value = event.get(key)
        if isinstance(value, str) and value:
            return f"{key}:{value}"
    message = event.get("message")
    if isinstance(message, dict) and isinstance(message.get("id"), str):
        return f"msg:{event.get('type')}:{event.get('subtype')}:{message['id']}"
    request = event.get("requestId") or event.get("request_id")
    if isinstance(request, str) and request:
        return f"req:{event.get('type')}:{request}"
    blob = json.dumps(event, sort_keys=True, default=str)
    return "sha:" + hashlib.sha256(blob.encode("utf-8")).hexdigest()[:32]


def deduplicate(events: Sequence[dict]) -> tuple[list[dict], int]:
    """Keep the first occurrence of each identity; count what went."""
    seen: set[str] = set()
    out: list[dict] = []
    removed = 0
    for event in events:
        # Bench turn markers are positional, not events: two turns may carry
        # identical text and both are real.
        if "_velra_bench" in event:
            out.append(event)
            continue
        identity = event_identity(event)
        if identity in seen:
            removed += 1
            continue
        seen.add(identity)
        out.append(event)
    return out, removed


# --------------------------------------------------------------------------
# stream reading
# --------------------------------------------------------------------------


def _content_blocks(event: dict) -> list[dict]:
    message = event.get("message")
    if not isinstance(message, dict):
        return []
    content = message.get("content")
    if isinstance(content, list):
        return [b for b in content if isinstance(b, dict)]
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    return []


def _tool_target(name: str, inputs: dict) -> str | None:
    if not isinstance(inputs, dict):
        return None
    for key in ("file_path", "path", "notebook_path", "command", "pattern",
                "query", "url"):
        value = inputs.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def _delivery_from(event: dict, index: int) -> Delivery | None:
    """A hook response carrying a capsule, or ``None``.

    ``stdout`` here is a structured field of a structured event. It is parsed
    as JSON; if that fails, the event is not a delivery. Nothing is matched
    against it as text.
    """
    if event.get("subtype") != "hook_response":
        return None
    raw = event.get("stdout") or event.get("output") or ""
    if not isinstance(raw, str) or CAPSULE_OPEN not in raw:
        return None
    try:
        payload = json.loads(raw)
        specific = payload["hookSpecificOutput"]
        capsule = specific["additionalContext"]
    except (json.JSONDecodeError, KeyError, TypeError):
        return None
    if not isinstance(capsule, str):
        return None
    return Delivery(
        stream_index=index,
        hook_name=event.get("hook_name"),
        hook_event=event.get("hook_event"),
        session_start_source=(event.get("session_start_source")
                              or event.get("source")),
        exit_code=event.get("exit_code"),
        chars=len(capsule),
        text=capsule,
        system_message=payload.get("systemMessage"),
    )


def _usage_record(event: dict, index: int, ref: str) -> dict | None:
    """One structured usage observation, with its provenance and field set.

    The record keeps the raw usage object so that field *presence* stays
    inspectable downstream: it is the difference between a zero and a silence.

    ``usage_class`` is the important part. Claude Code reports the same turn's
    usage twice — once on the ``assistant`` event that carries the message, and
    again on the ``result`` event that closes the turn — and a record that did
    not say which it was would be summed alongside its own duplicate. See
    :func:`select_usage_records`.
    """
    usage = event.get("usage")
    usage_class = USAGE_CLASS_RESULT if event.get("type") == "result" \
        else USAGE_CLASS_ASSISTANT
    if not isinstance(usage, dict):
        message = event.get("message")
        if isinstance(message, dict) and isinstance(message.get("usage"), dict):
            usage = message["usage"]
            usage_class = USAGE_CLASS_ASSISTANT
        else:
            return None
    declared = event.get("_telemetry_source")
    source = declared if declared in telemetry.SOURCES \
        else telemetry.SOURCE_STRUCTURED_USAGE
    return {
        "stream_index": index,
        "type": event.get("type"),
        "subtype": event.get("subtype"),
        "usage_class": usage_class,
        "session_id": event.get("session_id"),
        "source": source,
        "ref": ref,
        "usage": dict(usage),
        "fields_present": sorted(usage),
    }


def parse_stream(path: pathlib.Path) -> dict:
    """One ``stream.jsonl``, read once, into everything derived from it."""
    events, malformed = telemetry.read_jsonl(path)
    total_lines = len(events) + len(malformed)
    events, duplicates = deduplicate(events)

    ref = path.name
    turn = 0
    turns_seen = 0
    session_id = None
    actions: list[Action] = []
    deliveries: list[Delivery] = []
    usage_records: list[dict] = []
    prose: list[str] = []
    context_observations: list[dict] = []

    for index, event in enumerate(events):
        if event.get("_velra_bench") == "turn_start":
            turn = int(event.get("turn") or 0)
            continue
        if session_id is None and isinstance(event.get("session_id"), str):
            session_id = event["session_id"]

        delivery = _delivery_from(event, index)
        if delivery is not None:
            deliveries.append(delivery)

        usage = _usage_record(event, index, ref)
        if usage is not None:
            usage_records.append(usage)

        # An explicit, structured context-size report. Claude Code does not
        # currently emit one; the field is read when present and left
        # unavailable when not, which is the whole rule.
        for key in ("context_size", "context_size_at_transition",
                    "context_tokens"):
            if isinstance(event.get(key), (int, float)):
                context_observations.append(
                    {"stream_index": index, "field": key,
                     "value": event[key],
                     "source": event.get("_telemetry_source")
                     or telemetry.SOURCE_STRUCTURED_USAGE})

        if event.get("type") == "result":
            turns_seen += 1
            continue

        if event.get("type") != "assistant":
            continue
        for block in _content_blocks(event):
            kind = block.get("type")
            if kind == "text":
                text = block.get("text") or ""
                prose.append(text)
                actions.append(Action(len(actions), turn, "text", None, None,
                                      text))
            elif kind == "tool_use":
                name = block.get("name") or "?"
                inputs = block.get("input") or {}
                actions.append(Action(len(actions), turn, "tool", name,
                                      _tool_target(name, inputs),
                                      json.dumps(inputs, default=str)))

    tool_actions = [a for a in actions if a.kind == "tool"]
    return {
        "events": events,
        "malformed": malformed,
        "total_lines": total_lines,
        "duplicates_removed": duplicates,
        "session_id": session_id,
        "turns": turns_seen,
        "actions": actions,
        "tool_calls": len(tool_actions),
        "file_reads": sum(1 for a in tool_actions if a.tool in READ_TOOLS),
        "searches": sum(1 for a in tool_actions if a.tool in SEARCH_TOOLS),
        "edits": sum(1 for a in tool_actions if a.tool in EDIT_TOOLS),
        "deliveries": deliveries,
        "usage_records": usage_records,
        "assistant_text": "\n".join(prose),
        "context_observations": context_observations,
    }


# --------------------------------------------------------------------------
# the whole trial
# --------------------------------------------------------------------------


def select_usage_records(records: Sequence[dict]) -> dict:
    """Choose the one class of usage record this trial's totals come from.

    The defect this exists to prevent, measured on a realistic capture: an
    ``assistant`` event carrying ``message.usage`` and the ``result`` event
    that closes the same turn both describe that turn's spend. Summing the two
    together reported exactly twice the true input, and it did so on both arms
    — but not by the same factor, because the number of assistant messages per
    turn varies with how much tool use a session did. A benchmark whose
    headline is a percentage difference cannot survive that.

    So the classes are alternatives and exactly one of them is believed. The
    chosen class is the *expected set* against which completeness is judged:
    a field has to be present on every record of that class, and records of
    every other class — and every line of the capture that carries no usage
    object at all — are not expected telemetry and are never counted as
    missing.
    """
    by_class: dict[str, list[dict]] = {}
    for record in records:
        by_class.setdefault(record.get("usage_class") or "?", []).append(record)

    for name in USAGE_CLASS_PREFERENCE:
        if by_class.get(name):
            rejected = {k: len(v) for k, v in sorted(by_class.items())
                        if k != name}
            tail = ("no other class was present" if not rejected
                    else f"the others are not added to it: {rejected}")
            return {
                "selected_class": name,
                "selected": by_class[name],
                "expected_record_count": len(by_class[name]),
                "available_classes": {k: len(v) for k, v in
                                      sorted(by_class.items())},
                "rejected_classes": rejected,
                "why": (f"{len(by_class[name])} {name} record(s); the classes "
                        f"are alternative reports of the same spend, so "
                        f"{tail}"),
            }
    return {
        "selected_class": None,
        "selected": [],
        "expected_record_count": 0,
        "available_classes": {},
        "rejected_classes": {},
        "why": "this trial carries no usage-bearing record of any class",
    }


def _usage_from_rollup(path: pathlib.Path) -> list[dict]:
    """``usage.json``: a structured usage rollup the interface exposed.

    Shape::

        {"source": "structured_usage",
         "records": [{"session_id": ..., "usage": {...}}, ...]}

    A rollup that does not name its source is refused rather than assumed to be
    structured usage: an unlabelled file could be anything, including something
    a human typed.
    """
    data = telemetry.read_json(path)
    if not isinstance(data, dict):
        return []
    source = data.get("source")
    if source not in telemetry.SOURCES or source == telemetry.SOURCE_UNAVAILABLE:
        return []
    out = []
    for index, record in enumerate(data.get("records") or []):
        if not isinstance(record, dict) or not isinstance(record.get("usage"), dict):
            continue
        out.append({
            "stream_index": -1,
            "type": "rollup",
            "usage_class": USAGE_CLASS_ROLLUP,
            "subtype": record.get("kind"),
            "session_id": record.get("session_id"),
            "source": source,
            "ref": f"{path.name}#records[{index}]",
            "usage": dict(record["usage"]),
            "fields_present": sorted(record["usage"]),
        })
    return out


def _transcript_usage(path: pathlib.Path) -> list[dict]:
    """Usage explicitly present in the session artifact.

    Claude Code's transcript lines carry a ``message.usage`` object on
    assistant entries. Only entries that actually have one are returned; the
    rest contribute nothing, which is different from contributing zero.
    """
    entries, _ = telemetry.read_jsonl(path)
    out = []
    for index, entry in enumerate(entries):
        message = entry.get("message")
        usage = message.get("usage") if isinstance(message, dict) else None
        if not isinstance(usage, dict):
            continue
        out.append({
            "stream_index": index,
            "type": "transcript",
            "usage_class": USAGE_CLASS_TRANSCRIPT,
            "subtype": entry.get("type"),
            "session_id": entry.get("sessionId") or entry.get("session_id"),
            "source": telemetry.SOURCE_SESSION_ARTIFACT,
            "ref": f"{path.name}#{index}",
            "usage": dict(usage),
            "fields_present": sorted(usage),
        })
    return out


def parse_trial(trial_dir: pathlib.Path) -> ParsedTrial:
    """Read one trial directory into the record the pipeline runs on."""
    trial_dir = pathlib.Path(trial_dir)
    meta = telemetry.read_json(trial_dir / "trial_meta.json") or {}
    stream = parse_stream(trial_dir / "stream.jsonl")

    candidates = list(stream["usage_records"])
    candidates.extend(_usage_from_rollup(trial_dir / "usage.json"))
    if not candidates:
        # Only read when nothing closer to the execution interface exists.
        candidates.extend(_transcript_usage(trial_dir / "transcript.jsonl"))
    # Exactly one class is believed. `selection` records which, what else was
    # on offer and why, so the choice is inspectable rather than implicit.
    selection = select_usage_records(candidates)
    usage_records = selection["selected"]

    restore = telemetry.read_json(trial_dir / "velra_restore.json") or {}
    final_state = telemetry.read_json(trial_dir / "final_state.json") or {}
    fixture = telemetry.read_json(trial_dir / "context_fixture.json") or {}

    destination = (meta.get("destination_session_id")
                   or stream["session_id"])
    return ParsedTrial(
        trial_dir=trial_dir,
        meta=meta,
        scenario=meta.get("scenario") or "?",
        arm=meta.get("arm") or "?",
        pair_id=meta.get("pair_id"),
        replicate=meta.get("replicate"),
        session_id=stream["session_id"] or meta.get("session_id"),
        source_session_id=meta.get("source_session_id") or restore.get("source_session_id"),
        destination_session_id=destination,
        turns=stream["turns"],
        tool_calls=stream["tool_calls"],
        file_reads=stream["file_reads"],
        searches=stream["searches"],
        edits=stream["edits"],
        actions=stream["actions"],
        assistant_text=stream["assistant_text"],
        deliveries=stream["deliveries"],
        usage_records=usage_records,
        usage_selection={k: v for k, v in selection.items() if k != "selected"},
        malformed_lines=stream["malformed"],
        total_lines=stream["total_lines"],
        duplicates_removed=stream["duplicates_removed"],
        restore=restore,
        final_state=final_state,
        context_fixture=fixture,
    )


def context_observations(trial_dir: pathlib.Path) -> list[dict]:
    """Structured context-size reports from the stream, if there are any."""
    return parse_stream(trial_dir / "stream.jsonl")["context_observations"]


def load_trials(root: pathlib.Path) -> list[ParsedTrial]:
    """Every trial directory under ``root`` that has a ``trial_meta.json``."""
    out = []
    for path in sorted(pathlib.Path(root).iterdir()) if pathlib.Path(root).is_dir() else []:
        if (path / "trial_meta.json").exists():
            out.append(parse_trial(path))
    return out


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description="Parse one trial directory.")
    ap.add_argument("--trial", required=True)
    args = ap.parse_args()
    parsed = parse_trial(pathlib.Path(args.trial).resolve())
    print(json.dumps(parsed.to_json(), indent=2, default=str))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
