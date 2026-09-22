#!/usr/bin/env python3
"""Target facts: the thing a scenario claims compaction destroys.

A scenario is only evidence about Velra if three things are true at once, and
before v0.1.2 the benchmark checked none of them separately:

  1. the fact existed before the compaction boundary;
  2. native compaction actually lost it;
  3. the post-compaction decision genuinely depends on it.

``TargetFact`` is where each scenario writes those claims down so they can be
tested rather than assumed. Every field exists to answer one stage of the causal
chain:

  ``probe`` / ``recalled_markers``   STAGE 1. A no-tool question put to a
      *baseline* session after it has compacted. If the baseline answers it, the
      information survived and the scenario is NOT CAUSALLY TESTABLE: nothing
      the capsule does afterwards can be credited.

  ``ledger_sql``                     STAGE 2. Run against the trial's own
      ``velra.db``, restricted to events at or before the checkpoint watermark.
      Answers "was it captured, and where", which is what separates a capture
      failure from a delivery failure.

  ``capsule_markers``                STAGE 3. Checked against the bytes Claude
      Code actually received, not against the database.

  ``necessary_because``              STAGE 5. Prose, for the report. It is the
      argument that the measured behaviour depends on this fact, and it is
      written before the run so it cannot be fitted to the result afterwards.

``leak_terms`` is separate and stricter: those are strings that would hand the
answer to an agent that had forgotten everything, and the scan that enforces
them runs over the prompts as well as the tree.
"""

from __future__ import annotations

import dataclasses
import re
from typing import Sequence

UNKNOWN = "UNKNOWN"


@dataclasses.dataclass(frozen=True)
class TargetFact:
    """One piece of operational state a scenario is built around."""

    id: str
    #: One line, for the report.
    what: str
    #: A question answerable only from context. Tools are forbidden when it is
    #: asked, and ``UNKNOWN`` is offered, so a miss is a miss and not a guess.
    probe: str
    #: Substrings that, in an answer, mean the fact was recalled. Matched case
    #: insensitively against the reply with whitespace collapsed.
    recalled_markers: Sequence[str]
    #: SQL against the trial's velra.db. Must take one parameter, the checkpoint
    #: event watermark, and return one row per piece of supporting evidence.
    #: Zero rows means CAPTURE FAILURE.
    ledger_sql: str
    #: Where the evidence lives, for the report: "constraints", "dead_ends", …
    ledger_table: str
    #: Substrings that must appear in the delivered capsule for STAGE 3 to pass.
    capsule_markers: Sequence[str]
    #: Why the measured behaviour depends on this fact. Pre-registered prose.
    necessary_because: str

    def question(self) -> str:
        """The probe as it is actually sent, with the no-tool framing."""
        return (
            "Do not use any tools and do not read any files. Answer only from "
            f"what you still have in context. {self.probe} If that is no longer "
            f"available to you, reply with the single word {UNKNOWN}."
        )

    def recalled(self, answer: str) -> dict:
        """Did this answer demonstrate recall?

        Conservative in the direction that matters: an answer that says UNKNOWN
        is never a recall even if it happens to contain a marker, and an answer
        that used a tool is discarded rather than counted.
        """
        text = re.sub(r"\s+", " ", answer or "").strip()
        low = text.lower()
        used_tool = "<tool:" in low
        said_unknown = bool(re.search(r"\b" + UNKNOWN.lower() + r"\b", low))
        hits = [m for m in self.recalled_markers if m.lower() in low]
        return {
            "target_fact": self.id,
            "answer": text[:600],
            "markers": list(self.recalled_markers),
            "markers_found": hits,
            "said_unknown": said_unknown,
            "used_tool": used_tool,
            "recalled": bool(hits) and not said_unknown and not used_tool,
        }

    def in_capsule(self, capsule_text: str | None) -> dict:
        """Did the delivered bytes carry it?"""
        if not capsule_text:
            return {"target_fact": self.id, "delivered": False, "present": False,
                    "markers_found": []}
        haystack = capsule_text.replace("\\", "/").lower()
        hits = [m for m in self.capsule_markers
                if m.replace("\\", "/").lower() in haystack]
        return {
            "target_fact": self.id,
            "delivered": True,
            "markers": list(self.capsule_markers),
            "markers_found": hits,
            "present": bool(hits),
        }
