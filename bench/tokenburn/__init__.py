"""The v0.1.2 Token-Burn benchmark.

One question, and the whole package exists to answer it honestly: can a
developer abandon a large Claude Code conversation on purpose, start a
brand-new session, restore a small bounded operational state, and continue the
work -- and does that actually cost less input than continuing the old
conversation would have?

Modules, in pipeline order::

    scenarios.py        the two benchmarks and the repository they run in
    context_fixture.py  the context-load ladder, generated programmatically
    leakscan.py         nine surfaces a destination session can read
    live_trial.py       the live driver (built; never executed in Phase 3)
    mock_adapter.py     synthetic trials in the live artifact shape
    parse.py            raw artifacts -> ParsedTrial
    telemetry.py        metrics that carry their own provenance
    metrics.py          ParsedTrial -> the numbers, A1 to A4
    causal.py           the A..I chain
    pairing.py          matched pairs on recorded identity
    verdict.py          one of four values per pair
    aggregate.py        the pipeline, end to end
    report.py           the tables, with nothing rounded up
    safety.py           the live-execution gate
    smoke.py            restore -> SessionStart, offline, real binary
    selftest.py         eighteen scripted cases
    run.py              --selftest / --dry-run / --smoke / --live

The archived S1/S2/S3 system lives in ``bench/legacy`` and contributes nothing
to this scorecard.
"""
