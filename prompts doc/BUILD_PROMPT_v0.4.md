# BUILD_PROMPT_v0.4.md — Velra v0.4+ "Cross-Agent Portability & Adaptive Policies"

## 0. Instructions to the implementing agent

You are extending a working **Velra v0.3** codebase to **v0.4**. This document is self-contained: §1 restates invariants you MUST preserve; the rest specifies new behavior.

- RFC 2119 keywords apply. Implement only what is in scope. Record ambiguity resolutions in `DECISIONS.md`.
- **Verification gate (mandatory):** third-party agent hook systems change frequently. For every adapter, before writing code you MUST: (a) read the vendor's current official hooks/plugin documentation, (b) record the doc URL, the date, and the agent version in `adapters/<agent>/VERIFIED.md`, (c) capture real payload fixtures from that version. Any capability you cannot verify MUST be declared `false`. Never infer a capability from another agent's behavior.
- Autonomous behavior is **opt-in, supervised, and shadow-first**. Nothing in v0.4 changes the default human experience.

---

## 1. Carried-forward invariants (v0.1–v0.3; MUST NOT regress)

1. Hooks always exit 0 (or the target agent's documented non-blocking success code); output is empty or one valid document in the agent's expected format; nothing on stderr; watchdogs; kill switch.
2. Sync hook budgets p99 ≤ 5–6 ms (macOS/Linux), barrier ≤ 10 ms, Windows ≤ 15 ms wall — for **every** adapter.
3. Append-only events, spool, cursor reducer, immutable checkpoints, one live continuation per session, idempotent two-phase delivery.
4. Deterministic, factual, zero-LLM capsule with provenance classes; causality `UNCONFIRMED`; HOT ≤ 800 est. tokens; any single injected string within the target agent's documented limit (Claude Code: ≤ 9,500 chars).
5. Health notices advisory by default; read-only MCP; no network except what a benchmark run explicitly performs through the agent under test; no telemetry; no repository files; no daemon.

---

## 2. Scope

### In scope (v0.4)
1. **Canonical Event Model (CEM)** and **adapter layer**; refactor the Claude Code integration into the first adapter without behavior change.
2. Adapters: **Cursor**, **OpenAI Codex CLI**, **Cline**, **OpenCode** (each behind the verification gate; ship only those that pass it).
3. **Capability-driven delivery planner** that picks the best available channel per agent and degrades gracefully.
4. **Rotation Engine** with human mode (existing advisory) and **autonomous mode** for supervised headless pipelines (`velra run`).
5. **Adaptive policies:** transparent, bounded, local threshold tuning from recorded outcomes.
6. **Continuation Fidelity benchmark suite** (`velra bench`).

### Out of scope (v0.4) — MUST NOT implement
Cloud sync, team state, enterprise policy servers, learned ML models or ML dependencies, LLM-based summarization, IDE UI extensions, automatic rotation of interactive human sessions, writing any file into user repositories (adapters use user-level config locations only, unless an agent offers no user-level location — then the adapter is marked unsupported rather than writing into the repo).

---

## 3. Canonical Event Model

### 3.1 Event kinds
`session.start{source: startup|resume|clear|compact|fork|unknown}`, `session.end{reason}`, `prompt.submit{text}`, `tool.pre{class: edit|shell|read|search|mcp|other, path?, command?}`, `tool.post{class, …, output_tail?, exit_code?}`, `tool.fail{class, error}`, `file.edit{path, edits?}` (for agents that report edits directly instead of as tool calls), `turn.end`, `compact.pre{trigger}`, `compact.post{summary?}`.
Every CEM event carries: `agent` (`claude_code|cursor|codex|cline|opencode`), `agent_version?`, `session_id` (agent-native id, namespaced as `{agent}:{id}`), `project_root`, `ts_ms`, `subagent_id?`, `raw_ref` (dedupe key).

### 3.2 Capability flags (per adapter, per detected agent version)
`observe_prompts`, `observe_tool_results`, `observe_shell_output`, `observe_edits`, `pre_edit_hook`, `compact_pre`, `compact_post`, `inject_session_start`, `inject_after_compact`, `inject_prompt`, `inject_post_tool`, `user_message`, `async_hooks`, `headless_mode`, `session_resume_by_id`, `max_injection_chars`.

### 3.3 Storage
`events` gains `agent TEXT NOT NULL DEFAULT 'claude_code'` and stores CEM-normalized payloads; the reducer consumes only CEM. Migration `user_version = 4`.

---

## 4. Adapter contract

Each adapter is a module providing, declaratively:
1. **Detection:** how to find the agent and its version (binary on PATH, config dir), without network access.
2. **Config target:** exact user-level config file path(s) per OS and the file format.
3. **Installer:** idempotent, comment/format-preserving (where the format allows comments), backed-up, atomic merge of Velra hook entries; exact inverse on disable. Reuses v0.1 settings-merge guarantees (byte-identical enable→disable round trip on fixtures).
4. **Input normalizer:** agent payload → CEM (tolerant parsing, unknown fields ignored).
5. **Output encoder:** delivery/notice → the agent's documented output shape; empty output when the channel is not supported.
6. **Capability table** (verified).
7. **Contract tests** over recorded fixtures for every hook event used.

`velra enable [--agent <name>|--all]` enables detected agents (default: Claude Code only, preserving v0.1 behavior; `--all` enables every detected, verified adapter). `velra status` lists each adapter with version and capability summary.

### 4.1 Adapter targets (starting points; each MUST pass the verification gate)
| Agent | Expected integration surface to verify | Expected notes |
|---|---|---|
| **Claude Code** | `~/.claude/settings.json` hooks (existing) | Reference adapter; all capabilities true per v0.1–v0.3 |
| **Cursor** | User-level `hooks.json` (`version: 1`) with events such as `sessionStart`, `beforeSubmitPrompt`, `preToolUse`/`postToolUse`/`postToolUseFailure`, `afterFileEdit`, `afterShellExecution`, `preCompact`, `stop`, `sessionEnd` | Verify which events can return context to the model; `preCompact` may be observe-only; there may be no post-compaction event |
| **Codex CLI** | User-level `hooks.json` with Claude-style nested schema (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`) | Verify compaction events and per-event `additionalContext` support (known to differ by event); timeouts in seconds |
| **Cline** | Verify current hook or extension mechanism | If no stable hook surface exists, ship as unsupported with a documented reason |
| **OpenCode** | Plugin system (plugin runs inside OpenCode's own runtime; the plugin shim only forwards events to the `velra` binary via stdin/stdout) | The shim adds no new host prerequisite because it runs in the agent's existing runtime |

---

## 5. Capability-driven delivery planner

Given a live continuation and an adapter's capabilities, the planner chooses channels in order:
1. `inject_after_compact` (or `inject_session_start` with a compact source) — immediate.
2. `inject_post_tool` — mid-turn fallback.
3. `inject_prompt` — next-turn fallback with two-phase re-delivery on aborted turns.
4. If none exist: **fresh-session handoff only** — the user is told (via `user_message` if available, else via `velra status`) that the checkpoint is available through `velra resume --agent <name>` (if `session_resume_by_id` or headless launch supported) or `velra export`.
The two-phase state machine (PENDING/ATTACHED/CONFIRMED) is agent-agnostic; `CONFIRMED` evidence = any subsequent `tool.post`, `tool.fail`, or `turn.end` in the same session. Capsule wording: identical sections; the `[CONTEXT]` sentence names the agent (`…from {Agent Name} tool events…`). Injection length respects `max_injection_chars` (truncation rules from v0.1 §16.3, then warm pack omitted).

Agents without `compact_pre`: checkpoints are created at every `turn.end` whose reducer state changed since the last checkpoint (rolling checkpoint, keep last 3 per session), so a continuation is always available.

---

## 6. Rotation Engine

### 6.1 Separation of concerns
- **Session health** (v0.2 signals S1–S4, P1–P2) = evidence.
- **Rotation eligibility** = a policy decision over evidence plus a safe boundary.
- **Rotation action** = checkpoint + transition to a fresh context.

### 6.2 Human mode (default; unchanged)
Advisory notices only. Velra never compacts, stops, or restarts an interactive session.

### 6.3 Autonomous mode (opt-in, supervised headless only)
- Entry point: `velra run [--agent claude_code|codex|…] [--policy <file>] [--shadow] --prompt <text> [--until "<shell check>"] [--max-iterations N] [--max-rotations-per-hour R] [-- <agent args>]`.
- `velra run` supervises a loop of headless agent invocations (e.g. Claude Code print mode). Each iteration it decides **CONTINUE** (resume the same agent session by id) or **ROTATE** (start a fresh session receiving Capsule + Warm Start Pack via the v0.3 resume-token mechanism).
- **Rotation eligibility (all MUST hold):**
  1. Safe boundary: the previous iteration ended with `turn.end`, no in-flight tools, no background tasks reported.
  2. Health = `LOOPING`, or (`DRAGGING` and S4 ≥ policy `pressure_min`, default 0.85).
  3. No P1 progress within the last `progress_window` runs (default 3).
  4. No P2 flakiness in the S1 fingerprint set.
  5. At least `min_iterations_between` iterations since the last rotation (default 3) and `max_rotations_per_hour` not exceeded (default 4).
- `--until` check passing ends the run successfully; `--max-iterations` bounds cost.
- **Shadow mode** (`--shadow`, and the default for the first run with a new policy): decisions are computed and logged, but the loop always CONTINUEs.
- Every decision is logged to `rotation_decisions (run_id, iteration, decision, eligible, evidence_json, policy_version, ts_ms)`; `velra run --explain <run_id>` prints them.
- Autonomous mode MUST refuse to start when stdin is an interactive TTY without `--yes`, and MUST NOT be enabled through configuration alone.

---

## 7. Adaptive policies

- Policy file (TOML), versioned, with bounded parameters: signal thresholds (N, K, S3 count, S4 ratio), `pressure_min`, `progress_window`, warm pack budget (1,000–6,000 chars), capsule budget (600–1,000 tokens).
- **Outcome records** per transition (compaction delivery, resume, rotation): post-transition duplicate reads (Read of a file whose hash was already read before the transition and is unchanged), dead-end repetitions (an edit producing a region `body_hash` equal to a dead end's version), turns-to-first-new-test-outcome, loop recurrence within 10 turns.
- **Adaptation rules** (transparent, deterministic, per project, applied at most once per 20 recorded transitions, each step within ±10% of the parameter's range and clamped to bounds):
  - Duplicate reads after transitions above a threshold → raise warm pack budget one step.
  - Dead-end repetitions after transitions → raise DEAD_ENDS priority in truncation order (move above RECENT_ATTEMPTS removal).
  - Loops recurring after autonomous rotations → raise rotation eligibility threshold (N + 1) — rotation that doesn't help is stopped.
  - Zero loops and zero duplicate reads for 3 consecutive windows → lower budgets one step (smaller context).
- Every adaptation is logged with before/after values; `velra policy explain` prints current parameters and the history; `velra policy reset` restores defaults. No hidden state, no ML.

---

## 8. Continuation Fidelity benchmark (`velra bench`)

### 8.1 Purpose
Answer honestly: *can Velra approach the quality of a disciplined manual micro-session workflow while removing the manual burden?* Publish results, including losses.

### 8.2 Task manifest (YAML; schema versioned)
`id`, `repo` (git URL or local path), `commit`, `setup` (shell commands), `prompt`, `subtasks` (ordered list with optional oracle handoff notes), `success_check` (shell command; exit 0 = success), `transition_points` (after subtask k, or at token/turn counts), `max_turns`, `timeout_minutes`, `language`, `tags`.

### 8.3 Conditions
| Id | Condition | Definition |
|---|---|---|
| A | Monolithic | One long session; no forced transitions (native auto-compaction allowed and recorded) |
| B | Manual micro-session (oracle) | At each transition point, a fresh session receives the manifest's human-written handoff note |
| C | Native compaction | At each transition point, native compaction is forced; Velra disabled |
| D | Velra | Same forced transitions as C with Velra enabled (capsule delivery) |
| E | Velra fresh-session | At each transition point, `velra resume`-style fresh session (capsule + warm pack) |

Forcing a transition in headless mode MUST use a documented agent mechanism (verify at build time); if none exists for an agent, that condition is marked unavailable for that agent rather than approximated.

### 8.4 Execution
- Each run executes in an isolated temporary clone (or git worktree) at the pinned commit; separate `VELRA_HOME` per run; agent config isolated via documented config-dir environment variables where available.
- Default n = 5 runs per task × condition; randomized condition order; wall-clock and turn limits enforced.
- The benchmark uses the user's own agent account; `velra bench` MUST print an estimated run count and require `--yes` before starting.

### 8.5 Metrics (computed from Velra's event log; definitions exact)
Task success (success_check), time to completion, total tool calls, duplicate reads (as §7), dead-end repetitions (as §7), repeated failure fingerprints after a transition, human explanation count (condition B note length as a reference cost), first-post-transition-turn productivity (tool calls until the first non-reverted edit or new test outcome), transition latency (barrier + delivery hook durations), injected context size (est. tokens), total transcript bytes processed.

### 8.6 Reporting
- Output: raw JSONL per run + a Markdown report with medians, IQR, and bootstrap 95% CIs per condition; per-task breakdowns; a "where Velra lost" section listing every task where D or E underperformed C or B.
- A starter corpus of ≥ 12 tasks across ≥ 4 languages with seeded, reproducible bugs MUST be included (`bench/tasks/`), each with an oracle handoff note for condition B.

---

## 9. Schema additions (migration to `user_version = 4`)
`events.agent`; `adapters (agent PK, version, capabilities_json, verified_doc_url, verified_ms)`; `rolling_checkpoints` flag on `checkpoints` (new column `rolling INTEGER NOT NULL DEFAULT 0`); `runs`, `run_iterations`, `rotation_decisions`; `transition_outcomes`; `policy_versions (project_id, version, params_json, created_ms, reason)`; `bench_runs`, `bench_metrics`.

---

## 10. Acceptance tests (in addition to v0.1–v0.3 suites)

### X. Adapters
- X1 Claude Code refactor: every v0.1–v0.3 test passes unchanged through the CEM path; budgets unchanged.
- X2 For each shipped adapter: `VERIFIED.md` present with URL, date, version; fixtures for every used event; normalizer produces expected CEM; encoder produces exact documented output; enable→disable byte-identical round trip on config fixtures.
- X3 Capability degradation: adapters lacking post-compaction injection fall back per §5; lacking all injection → no output, handoff message path works.
- X4 Rolling checkpoints for agents without `compact_pre`: at most 3 per session, immutable, superseded correctly.
- X5 Budgets met for every adapter hook on all platforms.

### Y. Rotation & policies
- Y1 Eligibility truth table: each of the five conditions individually false → CONTINUE.
- Y2 Shadow mode never rotates; decisions logged identically to live mode.
- Y3 Rate limits enforced (`min_iterations_between`, `max_rotations_per_hour`).
- Y4 `velra run` refuses interactive TTY without `--yes`; cannot be enabled by config alone.
- Y5 Policy adaptation: synthetic outcome streams produce the specified parameter steps, clamped to bounds; `policy explain`/`reset` correct.

### Z. Benchmark
- Z1 Manifest schema validation with helpful errors.
- Z2 Isolation: concurrent runs never share clones, `VELRA_HOME`, or agent config.
- Z3 Metrics computed from fixture event logs match hand-computed values.
- Z4 Report includes CIs and the "where Velra lost" section even when empty (stating "none observed").
- Z5 Starter corpus: every task's success_check fails at the pinned commit and passes with the reference fix.

---

## 11. Milestones & Definition of Done
1. **M1** CEM + Claude Code adapter refactor (X1).
2. **M2** Delivery planner + rolling checkpoints (X3, X4).
3. **M3** Adapters one at a time, each gated (X2, X5).
4. **M4** Rotation engine + `velra run` + shadow mode (Y1–Y4).
5. **M5** Adaptive policies (Y5).
6. **M6** Benchmark harness + starter corpus + first published report (Z1–Z5).

**Done** = all suites green on all platforms; every shipped adapter verified against current vendor docs; a first benchmark report is published with raw data, including every condition where Velra did not win.
