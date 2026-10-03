# Final-build replay of the frozen v0.1.2 source ledgers

Written by `bench/tokenburn/replay.py`. Every value below is copied from
`replay.json` in this directory; nothing here is written by hand.

- Binary: `velra 0.1.2 (77328b098, x86_64-pc-windows-gnu)` (sha256 `98873ebb94c4d2b4…`)
- Source commit of the replaying tree: `77328b098244975a8d4ef7d186f4b6923f6d3c65` plus uncommitted changes to `Cargo.toml`, `crates/velra/src/cli.rs`
- Inputs: the Velra-arm source ledgers of `bench/results/v0.1.2-requal`, verified against `raw_captures.sha256.json`
- Platform: Windows 11 AMD64
- Replayed: 2026-09-30T15:49:33Z

| Trial | Input verified | Workspace id matches | Tokens then → now | Chars then → now | Markers in staged capsule | Markers in delivered context | Second startup delivered | Deterministic | Status |
|---|:-:|:-:|---:|---:|---|---|:-:|:-:|---|
| A_cold_continuation-q1-velra | yes | yes | 737 → 717 | 1535 → 1477 | 3/3 | 3/3 | no | yes | PASS |
| A_cold_continuation-q2-velra | yes | yes | 737 → 710 | 1525 → 1457 | 3/3 | 3/3 | no | yes | PASS |
| B_clear_survival-q1-velra | yes | yes | 680 → 711 | 1423 → 1483 | 4/4 | 4/4 | no | yes | PASS |
| B_clear_survival-q2-velra | yes | yes | 693 → 721 | 1443 → 1497 | 4/4 | 4/4 | no | yes | PASS |

The source workspaces no longer exist, so each was recreated as an empty
directory at its recorded path. Git metadata is read from disk at render
time, so `[WORKSPACE_STATE]` reads `no git` where the live capsule named a
branch and commit; token and character counts include that difference.

Overall: **PASS**
