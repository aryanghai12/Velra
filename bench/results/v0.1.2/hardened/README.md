# v0.1.2 hardened evaluation — historical readiness snapshots

Frozen. Nothing writes into this directory.

`readiness.20260921T101557Z.json` is the readiness gate the archived
`bench/legacy/run_hardened_eval.py` produced on 2026-09-21, preserved
byte-for-byte (sha256 `02dda3c4f06f7b8f…`). It was called
`bench/results/v0.1.2/readiness.json` at the time.

That name now belongs to the **Token-Burn benchmark**, which is the v0.1.2
efficacy scorecard: `bench/results/v0.1.2/readiness.json` is the single
authoritative current readiness artifact, and it says so in its own
`artifact` block. If the archived runner is ever run again it writes
`bench/results/v0.1.2/hardened/readiness.json` — a different path from both
this snapshot and the current artifact, so none of the three can overwrite
another.

The hardened evaluation's actual evidence — `trials/`, `stages/`, `controls/`,
`aggregate.json`, `verdicts_v2.json`, `provenance.json` — is untouched in
`bench/results/v0.1.2/` and is not duplicated here.
