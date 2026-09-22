#!/usr/bin/env python3
"""Generate the context-load ladder programmatically, straight to disk.

    python bench/tokenburn/context_fixture.py --target 900000 --into /tmp/fx \\
        --seed 7 --scenario A_cold_continuation

A 900 000-token fixture is roughly three and a half megabytes of text. It
cannot be written by a language model into a file, and it must not be: the only
way to get a fixture that size honestly is to generate it with code, from a
seed, and verify what came out. This module is that code.

What it generates
-----------------

Not filler. Six kinds of realistic engineering output, interleaved:
compiler/build logs, pytest output, unified diffs, shell transcripts,
structured tool results and source excerpts. All of it is about the payments
fixture the scenarios use, so a session reading it is doing something a
developer would recognise — reading CI logs to find out what broke.

**It is still a load fixture, and it is labelled as one everywhere.** §5 of the
Phase 3 specification draws the line: realistic engineering context comes from
the turn script's own fifteen-plus turns of work, and this is the separate,
declared mechanism for scaling that context up. The metadata record carries
``kind: synthetic_load_fixture`` so nothing downstream can quote its size as a
property of a real session.

Sizes, and what a "token" means here
------------------------------------

The ladder is written in tokens because that is what a context window is
measured in, but this generator has no tokenizer — running one would mean an
API call, and the whole point of the offline harness is that it makes none. It
therefore generates to a **character** budget derived from
:data:`CHARS_PER_TOKEN`, and records:

    target_context_size      what was asked for, in tokens
    synthetic_context_size   the estimate, in tokens
    chars                    what is actually on disk -- measured
    token_basis              "estimated", always, for this generator

So ``synthetic_context_size`` is an estimate of a generated file's size, and it
is marked as a proxy downstream. It is never, under any circumstances, an
observation of a Claude session's context.

Memory
------

Blocks are written as they are produced and the digest is updated
incrementally. Nothing holds the generated text; a 900 000-token rung costs a
few kilobytes of resident memory and about a second of wall time.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import pathlib
import random
import sys

HERE = pathlib.Path(__file__).resolve().parent
if __package__ in (None, ""):
    sys.path.insert(0, str(HERE))
    import prereg  # type: ignore[no-redef]
else:
    from . import prereg

#: Bumped whenever the generated text changes shape. Part of the seed, so a
#: change to the generator changes every fixture it produces and the fixture
#: seed recorded in a trial stops matching — which is the point.
FIXTURE_VERSION = 1

#: The declared characters-per-token assumption. Log-like ASCII with long
#: repeated identifiers runs denser than prose; 3.6 is a deliberately
#: conservative figure for this kind of text, and it is recorded in every
#: metadata file so a reader can redo the arithmetic with their own number.
CHARS_PER_TOKEN = 3.6

#: How the ladder's budget is split across the four log files the turn scripts
#: actually read. Two build logs and two test logs, uneven on purpose.
LOG_FILES = (
    ("logs/build-01.log", 0.34, "build"),
    ("logs/test-01.log", 0.26, "test"),
    ("logs/build-02.log", 0.24, "build"),
    ("logs/test-02.log", 0.16, "test"),
)

MODULES = ("payments.retry", "payments.idempotency", "payments.ledger",
           "payments.reconcile", "payments.feed", "payments.transport",
           "payments.settlement", "payments.window")

FILES = ("src/payments/retry.py", "src/payments/idempotency.py",
         "src/payments/ledger.py", "src/payments/reconcile.py",
         "src/payments/feed.py", "tests/test_retry.py",
         "tests/test_ledger.py", "tests/test_reconcile.py")

WARNINGS = (
    "unused variable `attempt`",
    "comparison between `date` and `datetime` is always False",
    "this import is redundant",
    "function `close_window` is never used in this configuration",
    "implicit conversion from `int` to `float` loses precision",
    "missing return type annotation",
)


# --------------------------------------------------------------------------
# block generators
# --------------------------------------------------------------------------


def block_build(rng: random.Random, index: int) -> str:
    lines = [f"=== build {index:05d} :: cargo-like runner, profile=dev ==="]
    for step in range(rng.randint(6, 14)):
        module = rng.choice(MODULES)
        lines.append(f"   Compiling {module} v0.{rng.randint(1, 9)}."
                     f"{rng.randint(0, 40)}")
        if rng.random() < 0.35:
            path = rng.choice(FILES)
            line_no = rng.randint(4, 220)
            col = rng.randint(1, 60)
            lines.append(f"warning: {rng.choice(WARNINGS)}")
            lines.append(f"  --> {path}:{line_no}:{col}")
            lines.append(f"   |")
            lines.append(f"{line_no:>3} |     "
                         f"{'key = idempotency.make_key(account, amount_minor)' if step % 3 else 'stamp = row[\"value_date\"]'}")
            lines.append(f"   |     {'^' * rng.randint(6, 30)}")
            lines.append(f"   = note: `#[warn(unused)]` on by default")
    lines.append(f"    Finished dev profile in {rng.randint(2, 90)}."
                 f"{rng.randint(10, 99)}s")
    return "\n".join(lines) + "\n\n"


def block_pytest(rng: random.Random, index: int) -> str:
    total = rng.randint(40, 180)
    failed = rng.randint(0, 4)
    lines = [f"=== test run {index:05d} :: pytest -q ==="]
    lines.append("." * (total - failed) + "F" * failed)
    for _ in range(failed):
        test = rng.choice(FILES[5:])
        name = rng.choice(("test_window_totals", "test_backoff_doubles",
                           "test_apply_fee_rounds", "test_parse_row",
                           "test_send_retries"))
        lines.append("=" * 70)
        lines.append(f"FAILED {test}::{name}")
        lines.append(f"{test}:{rng.randint(10, 90)}: AssertionError")
        lines.append(f"    assert {rng.randint(0, 5000)} == {rng.randint(0, 5000)}")
        lines.append("     +  where both sides are minor units")
    lines.append(f"{total - failed} passed, {failed} failed in "
                 f"{rng.randint(1, 40)}.{rng.randint(10, 99)}s")
    return "\n".join(lines) + "\n\n"


def block_diff(rng: random.Random, index: int) -> str:
    path = rng.choice(FILES)
    start = rng.randint(1, 180)
    lines = [f"--- a/{path}", f"+++ b/{path}",
             f"@@ -{start},{rng.randint(4, 12)} +{start},{rng.randint(4, 12)} @@"]
    for _ in range(rng.randint(4, 16)):
        mark = rng.choice((" ", "-", "+", " ", " "))
        body = rng.choice((
            "    last = None",
            "    for attempt in range(attempts):",
            "        key = idempotency.make_key(account, amount_minor)",
            "    return base_ms * (2 ** (attempt - 1))",
            "    return sum(r['amount_minor'] for r in rows)",
            "        transport.sleep(retry_backoff(attempt))",
        ))
        lines.append(f"{mark}{body}")
    return "\n".join(lines) + "\n\n"


def block_shell(rng: random.Random, index: int) -> str:
    lines = [f"$ git status --porcelain", "", f"$ ls -la src/payments"]
    for name in ("__init__.py", "retry.py", "idempotency.py", "ledger.py",
                 "reconcile.py", "feed.py"):
        lines.append(f"-rw-r--r-- 1 ci ci {rng.randint(200, 9000):>6} "
                     f"Mar {rng.randint(1, 28):>2} 1{rng.randint(0, 9)}:"
                     f"{rng.randint(10, 59)} {name}")
    lines.append(f"$ python -m pytest -q --collect-only | tail -1")
    lines.append(f"{rng.randint(30, 200)} tests collected")
    return "\n".join(lines) + "\n\n"


def block_tool_result(rng: random.Random, index: int) -> str:
    payload = {
        "tool": rng.choice(("Grep", "Read", "Bash")),
        "run": index,
        "matches": [
            {"file": rng.choice(FILES), "line": rng.randint(1, 200),
             "text": rng.choice(("value_date", "booking_date",
                                 "idempotency.make_key", "retry_backoff",
                                 "close_window", "amount_minor"))}
            for _ in range(rng.randint(3, 12))
        ],
        "truncated": bool(rng.random() < 0.2),
    }
    return json.dumps(payload, indent=1) + "\n\n"


def block_excerpt(rng: random.Random, index: int) -> str:
    path = rng.choice(FILES)
    start = rng.randint(1, 120)
    lines = [f"===== {path} (lines {start}-{start + 18}) ====="]
    for offset in range(19):
        lines.append(f"{start + offset:>4}| " + rng.choice((
            "def close_window(rows, start, end):",
            "    \"\"\"Total the rows that belong to this window.\"\"\"",
            "    return sum(r[\"amount_minor\"] for r in rows)",
            "        except TransportError as exc:",
            "            transport.sleep(retry_backoff(attempt))",
            "    blob = f\"{account}:{amount_minor}\".encode(\"utf-8\")",
            "",
        )))
    return "\n".join(lines) + "\n\n"


#: In the order they are cycled through, so a file is a plausible mixture
#: rather than a thousand copies of one shape.
BLOCKS = (block_build, block_pytest, block_diff, block_shell,
          block_tool_result, block_excerpt)

#: Which block kinds a build log and a test log are made of.
KIND_BLOCKS = {
    "build": (block_build, block_diff, block_shell, block_excerpt),
    "test": (block_pytest, block_tool_result, block_diff, block_excerpt),
}


# --------------------------------------------------------------------------
# generation
# --------------------------------------------------------------------------


@dataclasses.dataclass
class GeneratedFile:
    path: str
    chars: int
    blocks: int
    sha256: str

    def to_json(self) -> dict:
        return dataclasses.asdict(self)


def seed_for(scenario: str, target: int, seed: int) -> str:
    """Deterministic from scenario, target size, fixture version and seed."""
    return f"{scenario}|{target}|{FIXTURE_VERSION}|{seed}"


def generate_file(path: pathlib.Path, char_budget: int, kind: str,
                  rng: random.Random) -> GeneratedFile:
    """Write one log file up to ``char_budget``, streaming.

    The loop stops the moment the budget is met or exceeded, and the block that
    crossed it is written whole: truncating a log mid-line would produce a file
    no CI system has ever emitted, and the realism is the point.
    """
    generators = KIND_BLOCKS[kind]
    digest = hashlib.sha256()
    written = 0
    blocks = 0
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as fh:
        header = (f"# generated load fixture -- {kind} log\n"
                  f"# synthetic; not a record of any real build\n\n")
        fh.write(header)
        digest.update(header.encode("utf-8"))
        written += len(header)
        while written < char_budget:
            text = generators[blocks % len(generators)](rng, blocks)
            fh.write(text)
            digest.update(text.encode("utf-8"))
            written += len(text)
            blocks += 1
    return GeneratedFile(path=path.name, chars=written, blocks=blocks,
                         sha256=digest.hexdigest()[:32])


def generate(into: pathlib.Path, target_tokens: int, scenario: str,
             seed: int = 0) -> dict:
    """The whole rung: four log files and one metadata record.

    Returns the record that a trial stores as ``context_fixture.json``.
    """
    into = pathlib.Path(into)
    rng = random.Random(seed_for(scenario, target_tokens, seed))
    char_budget = int(round(target_tokens * CHARS_PER_TOKEN))

    files: list[GeneratedFile] = []
    for rel, share, kind in LOG_FILES:
        files.append(generate_file(into / rel, int(char_budget * share), kind,
                                   rng))

    chars = sum(f.chars for f in files)
    estimated = int(round(chars / CHARS_PER_TOKEN))
    record = {
        "kind": "synthetic_load_fixture",
        "fixture_version": FIXTURE_VERSION,
        "scenario": scenario,
        "seed": seed,
        "seed_string": seed_for(scenario, target_tokens, seed),
        "target_context_size": target_tokens,
        "synthetic_context_size": estimated,
        "token_basis": "estimated",
        "chars_per_token_assumed": CHARS_PER_TOKEN,
        "chars": chars,
        "bytes_on_disk": sum((into / rel).stat().st_size
                             for rel, _, _ in LOG_FILES),
        "files": [f.to_json() for f in files],
        "context_generation_method": (
            "programmatic synthetic build/test logs, read by the session as "
            "part of its turn script"),
        "reason_if_target_not_reached": None,
        "actual_observed_context_size": None,
        "warning": ("synthetic_context_size is an ESTIMATE of a generated "
                    "file's size at a declared chars-per-token ratio. It is "
                    "NOT an observation of a Claude session's context and must "
                    "never be reported as one."),
    }
    (into / "logs" / "fixture.meta.json").parent.mkdir(parents=True,
                                                       exist_ok=True)
    (into / "logs" / "fixture.meta.json").write_text(
        json.dumps(record, indent=2), encoding="utf-8", newline="")
    return record


def verify(into: pathlib.Path, record: dict,
           tolerance: float = 0.02) -> dict:
    """Re-measure what is on disk against what the record claims.

    A generator that silently produced half of what it promised would make
    every rung above it a fiction, and nothing downstream would notice: the
    record is what the report quotes. This re-reads the files.
    """
    into = pathlib.Path(into)
    problems: list[str] = []
    measured = 0
    for entry in record["files"]:
        path = into / "logs" / entry["path"]
        if not path.is_file():
            problems.append(f"missing: {entry['path']}")
            continue
        digest = hashlib.sha256()
        chars = 0
        with open(path, "r", encoding="utf-8", newline="") as fh:
            while True:
                chunk = fh.read(1 << 16)
                if not chunk:
                    break
                digest.update(chunk.encode("utf-8"))
                chars += len(chunk)
        measured += chars
        if digest.hexdigest()[:32] != entry["sha256"]:
            problems.append(f"digest mismatch: {entry['path']}")
        if chars != entry["chars"]:
            problems.append(
                f"size mismatch: {entry['path']} has {chars} chars, "
                f"record says {entry['chars']}")

    target_chars = record["target_context_size"] * record["chars_per_token_assumed"]
    shortfall = (target_chars - measured) / target_chars if target_chars else 0
    if shortfall > tolerance:
        problems.append(
            f"generated {measured} chars against a target of "
            f"{int(target_chars)}: {shortfall:.1%} short")
    return {
        "ok": not problems,
        "problems": problems,
        "measured_chars": measured,
        "declared_chars": record["chars"],
        "target_chars": int(target_chars),
        "shortfall_fraction": round(shortfall, 6),
    }


def ladder_plan(scenario: str, seed: int = 0) -> list[dict]:
    """Every registered rung, described without generating anything."""
    return [{
        "target_context_size": rung,
        "estimated_chars": int(round(rung * CHARS_PER_TOKEN)),
        "estimated_megabytes": round(rung * CHARS_PER_TOKEN / 1e6, 2),
        "seed_string": seed_for(scenario, rung, seed),
        "note": ("a TARGET, not a capability claim: whether the runtime can "
                 "actually hold this is an open question the live run answers"),
    } for rung in prereg.ladder()]


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--target", type=int, required=False,
                    help="target context size in tokens")
    ap.add_argument("--into", required=False,
                    help="fixture repository root; logs/ is written under it")
    ap.add_argument("--scenario", default="A_cold_continuation")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--plan", action="store_true",
                    help="print the ladder without generating anything")
    args = ap.parse_args()

    if args.plan or not args.target:
        print(json.dumps(ladder_plan(args.scenario, args.seed), indent=2))
        return 0
    if not args.into:
        ap.error("--into is required when --target is given")

    into = pathlib.Path(args.into).resolve()
    record = generate(into, args.target, args.scenario, args.seed)
    check = verify(into, record)
    record["verification"] = check
    print(json.dumps({k: v for k, v in record.items() if k != "files"},
                     indent=2))
    if not check["ok"]:
        print("VERIFICATION FAILED:", check["problems"], file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
