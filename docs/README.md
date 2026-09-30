# Velra documentation

Documentation for Velra **0.1.2**. Everything under "Current" describes this
release and is kept consistent with the code. Everything under "Historical"
is kept unchanged for provenance and does **not** describe current behaviour.

## Current

**Using Velra**

| Page | For |
|---|---|
| [INSTALL.md](INSTALL.md) | requirements, every install method, build from source, verify, upgrade, uninstall |
| [RESTORE.md](RESTORE.md) | `velra restore` end to end: workspaces, sessions, staging, delivery, stale state, one-shot behaviour |
| [CLI.md](CLI.md) | every command, flag, output and exit code |
| [CONFIGURATION.md](CONFIGURATION.md) | files, `config.toml`, environment variables, hooks, `SessionStart`, version compatibility |
| [TROUBLESHOOTING.md](TROUBLESHOOTING.md) | diagnostic matrix, `inspect --trace`, capture versus staging versus delivery |

**How it works and what it promises**

| Page | For |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | the pipeline from capture to delivery, storage, ordering, reduction, rendering, fail-open design |
| [GUARANTEES.md](GUARANTEES.md) | every claim sorted into guaranteed, supported, known limitation and unverified; platform support |
| [../SECURITY.md](../SECURITY.md) | persistence surfaces, redaction, cross-session exposure, residual risks, reporting |
| [BENCHMARK.md](BENCHMARK.md) | the v0.1.2 benchmark: live qualification, release-build replay, hook latency |

**Working on Velra**

| Page | For |
|---|---|
| [DEVELOPMENT.md](DEVELOPMENT.md) | toolchain, repository layout, building, benchmark tooling, release build metadata |
| [TESTING.md](TESTING.md) | every test suite, the validation matrix, platform coverage, known intermittent tests |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | ground rules and the pull-request checklist |
| [RELEASING.md](RELEASING.md) | maintainer release runbook and published status |
| [E2E_CHECKLIST.md](E2E_CHECKLIST.md) | the manual pre-release pass against a real Claude Code |
| [../DECISIONS.md](../DECISIONS.md) | every design decision and deviation, D1–D150 (append-only) |
| [../CHANGELOG.md](../CHANGELOG.md) | release notes |
| [../bench/README.md](../bench/README.md) | the benchmark harness |

Diagrams and charts used by these pages live in [`assets/v0.1.2/`](assets/v0.1.2/).

## Historical

Kept byte for byte. Read them as records of earlier versions, not as
descriptions of 0.1.2.

| Document | What it was |
|---|---|
| [ARCHITECTURE_AUDIT_v0.1.2.md](ARCHITECTURE_AUDIT_v0.1.2.md) | the audit of Velra before the v0.1.2 ledger work |
| [ARCHITECTURE_AUDIT_TOKEN_STATE.md](ARCHITECTURE_AUDIT_TOKEN_STATE.md) | the pre-qualification audit of the Token-Burn benchmark |
| [../BENCHMARK_REPORT.md](../BENCHMARK_REPORT.md) | the v0.1 `/compact` benchmark |
| [../HANDOFF.md](../HANDOFF.md) | v0.1 working notes |
| [../bench/results/README.md](../bench/results/README.md) | which result trees are current and which are historical |
| [`../prompts doc/`](../prompts%20doc/) | the original build specifications that `DECISIONS.md` section numbers refer to |
| [`../assets/`](../assets/) | the v0.1 benchmark proof image (`proof.svg`, `proof.png`), cited by `BENCHMARK_REPORT.md` |

[← README](../README.md)
