# v0.1.2 release-build evidence

Offline measurements of the Velra 0.1.2 release binary, taken on
2026-09-30. Written up in [`docs/BENCHMARK.md`](../../../docs/BENCHMARK.md),
sections 2 and 3. This tree is protected (`bench/tokenburn/runroot.py`,
D150): tools refuse to write into it. Re-run into a new directory.

| Path | Written by | Contents |
|---|---|---|
| `replay/replay.json` | `bench/tokenburn/replay.py` | the four Velra-arm source ledgers of [`../v0.1.2-requal/`](../v0.1.2-requal/), verified against its `raw_captures.sha256.json`, replayed through `velra restore`, `velra inspect --trace` and `velra hook session-start`; per trial: input hash, workspace id check, staged capsule (full text), marker presence, delivery, second-startup check, determinism |
| `replay/replay.md` | `bench/tokenburn/replay.py` | the same results as a table |
| `latency/<hook>.json` | `bench/legacy/run.sh` with `VELRA_BENCH_RESULTS` | 500 wall-time samples per hook, in seconds, against a 100,000-event database (built-in Python timer) |
| `latency/kill-switch-floor.json` | `bench/legacy/spawn_floor.py` | 500 samples of the same binary exiting at the kill switch: the process-start floor |
| `latency/environment.json` | `bench/legacy/spawn_floor.py` | binary version and SHA-256, OS, CPU, Python, timer, run counts |

Both sets of measurements used one binary, `velra 0.1.2 (77328b098,
x86_64-pc-windows-gnu)`, sha256 `98873ebb…55fae8`. Its source is commit
`77328b0` plus the release's command-line help and package-metadata
changes (`replay.json → working_tree`).

Nothing here is a live Claude Code session, and nothing here is pooled with
the live results in `../v0.1.2-requal/`.
