# Strategic Roadmap & Developer Psychology Audit — Velra

> **Velra** — *Fresh context. Same work.*
> Adaptive context-lifecycle infrastructure for AI coding agents.
> Product name: **Velra** · binary `velra` · data directory `~/.velra/`.

**Pre-launch check (not yet verified):** confirm availability of `velra` on crates.io, npm, Homebrew core/tap naming, GitHub org, and a domain (e.g. `velra.dev`) before the first public release. Every build prompt treats the domain as a placeholder `{{VELRA_DOMAIN}}`.

---

## 1. The wedge in one sentence

> After `/compact`, Claude still knows **what you were doing, what is failing, and what you already tried and threw away.**

Native compaction summarises the conversation. It is weakest at **negative knowledge**: approaches that were attempted, observed to fail, and reverted. Once those disappear, the agent re-proposes the same dead end, and the developer has to say "we already tried that". That moment is the trauma. Velra v0.1 exists to remove it, and nothing else.

---

## 2. Sequencing logic: the trust ladder

Every release asks the developer for exactly one new unit of trust. Releases that ask for two at once die in the "cool, but not on my machine" phase.

| Version | New trust asked | Trauma addressed | Viral trigger | Exit criterion to advance |
|---|---|---|---|---|
| **v0.1** | "Let a binary add hooks to my `~/.claude/settings.json`" | Compaction amnesia: forgotten goal, failing test, dead ends | `⚡ Velra restored: objective · 1 failing test · 2 dead ends` appears seconds after `/compact`, and Claude's next message avoids the dead end | Acceptance suite green; ≥20 real-world compactions dogfooded with zero hook errors shown to the user |
| **v0.2** | "Let it parse my code (locally) and judge my session's health" | "Claude is looping and I can't tell"; line numbers that point into the middle of nothing | Symbol-accurate capsule plus a *correct* loop notice ("same failure 4×, same function edited back and forth") | Loop notices have ≤10% "that was wrong" rate in dogfooding; flaky tests never trigger a notice |
| **v0.3** | "Let it run a local MCP server the agent can query" | "I need the thing from two hours ago" / cold-start fresh sessions | `velra resume` opens a fresh session that starts working on turn 1 without rereading the repo | Fresh-session first-turn productivity measurably better than native `/clear` + manual note |
| **v0.4+** | "Let it run my unattended pipelines and my other agents" | Tool fragmentation (Cursor + Codex + Claude); overnight agent runs that rot | Published Continuation Fidelity benchmark numbers, including where Velra loses | Benchmark shows Velra ≥ native compaction on task success with lower rediscovery; results are public |

Why this order and not another:

1. **v0.1 before intelligence.** Structural parsing (v0.2) is impressive but invisible. A capsule that is *mostly right with file-level precision* delivers the "Aha" immediately. Precision upgrades land better once people already depend on the feature.
2. **Health signals before rotation.** Automatic session management (v0.4) is only safe once the loop detector (v0.2) has been proven honest on real humans in advisory mode. You earn the right to act by first being right while only speaking.
3. **Recovery after capture.** An MCP recovery layer (v0.3) is only useful once there is a rich, trustworthy archive to recover from, which v0.1–v0.2 build.
4. **Portability last.** Multi-agent adapters multiply the test matrix. Doing it before the core is stable multiplies bugs, not users.

---

## 3. Per-version psychology audit

### v0.1 — Lossless Compaction for Claude Code
- **Trauma:** You spend 40 minutes debugging, compaction fires (often automatically, mid-task), and the next message is a confident suggestion you already rejected. You feel robbed of your time and lose trust in the agent.
- **Why the wedge is sharp:** It is felt by every heavy Claude Code user, it recurs daily, and it is binary: either Claude remembers the dead end or it does not. Binary outcomes make great demos.
- **Aha choreography (the 6-second moment):**
  1. `curl -LsSf https://{{VELRA_DOMAIN}}/install.sh | sh && velra enable` → `✓ Velra enabled for Claude Code. Nothing else required.`
  2. Work normally for 5–10 minutes (edit, run tests, revert something).
  3. `velra inspect` → see exactly what would survive, rendered locally. (This builds trust *before* the magic.)
  4. `/compact` → `⚡ Velra checkpoint saved` → `⚡ Velra restored: objective · 1 failing test · 2 dead ends · 5 files (612 tokens)`.
  5. Claude's next message references the failing test and steers away from the reverted approach.
- **What we deliberately cut:** AST parsing, loop detection, MCP, dashboards, config UIs, any LLM usage, any network I/O, any files in the repo.
- **Anti-virality risks:** a single visible "hook error" line in the Claude Code transcript reads as "this tool breaks Claude". Fail-open is therefore a *growth* requirement, not only an engineering one.

### v0.2 — Structural Intelligence & Failure Normalization
- **Trauma:** "Claude has edited `rotateToken` back and forth for 20 minutes and the same test fails the same way." The developer senses a loop but has no evidence, so they either interrupt too early (lose progress) or too late (burn quota).
- **Aha:** A notice that is *specific and correct*: `⚡ Velra: same failure (F-3a9c) 4× · rotateToken() edited A→B→A · checkpoint ready — /compact to continue cleanly.` Correctness is the product; one false alarm on a flaky test costs more trust than ten correct alarms earn.
- **What we cut:** automatic action of any kind. v0.2 only speaks.

### v0.3 — On-Demand Recovery & Local MCP
- **Trauma:** "Why did we reject TTL=300?" is lost forever once it leaves the context. Starting a *fresh* session costs 10 minutes of rereading.
- **Aha:** The agent calls `velra_attempts` on its own and answers with evidence (edit, test run, timestamp) instead of re-running the experiment. `velra resume` produces a first turn that already knows the working set.
- **What we cut:** cloud sync, semantic embeddings, vector databases. Full-text search over a local archive is enough and is explainable.

### v0.4+ — Portability, Autonomous Rotation, Fidelity Benchmark
- **Trauma:** Teams mix agents; unattended overnight runs degrade silently as context fills with debris.
- **Aha:** The same capsule works in Cursor and Codex; `velra run` keeps a 6-hour headless job productive; and a public benchmark shows where it helps *and where it does not*.
- **Credibility trigger:** publishing losses. Developer-tool audiences reward honest benchmarks and punish marketing numbers.

---

## 4. Architectural corrections to the source specification

As the reviewing architect, these are deliberate deviations from the original product specification, each backed by the current Claude Code hook documentation. They are encoded in the build prompts.

| # | Source spec said | Velra build prompts say | Why |
|---|---|---|---|
| 1 | Inject the continuation only via the next `UserPromptSubmit` | **Primary:** `SessionStart` with source `compact` (fires right after every compaction). **Fallbacks:** first `PostToolUse` after compaction (mid-turn), then `UserPromptSubmit` with full two-phase commit | Auto-compaction frequently happens *mid-turn*. Waiting for the next user prompt means the agent keeps working amnesiac for the rest of that turn, which is exactly when dead ends get repeated |
| 2 | A global npm install as the ideal install | `curl … \| sh` / `irm … \| iex` / Homebrew as primary; npm only as a convenience channel | npm requires Node.js, which violates the zero-runtime-prerequisite rule |
| 3 | Background state reducer (implied daemon) | No daemon. Reduction is scheduled by Claude Code's own **async hooks** (`PostToolBatch`, `Stop`) and bounded at `PreCompact` | A daemon adds install friction, OS service registration, and a new failure mode. Async hooks give background execution for free |
| 4 | Event logging via `PostToolUse`/`PostToolUseFailure` only | Adds a filtered `PreToolUse` for edit tools and `git` commands | Revert detection needs the file's *pre-edit* content hash. Without it, "A→B→A" cannot be proven |
| 5 | A protocol block of "fixed continuation rules" wrapping the state | Capsule header is **factual, not imperative** | Claude Code's docs warn that injected text framed as out-of-band system commands can trigger prompt-injection defenses, causing Claude to surface it to the user instead of using it |
| 6 | "<800 tokens" as the only size rule | Also a hard **10,000-character** ceiling for any single hook output (capsule + warm pack) | Claude Code caps hook output strings at 10,000 characters and spills larger output to a file, which defeats the purpose |
| 7 | 3–5 ms cold start as a universal target | 3–5 ms p99 on macOS/Linux; Windows target is ≤15 ms wall-clock | Windows process creation alone costs several milliseconds regardless of language; promising otherwise is dishonest |

---

## 5. Tech stack verdict (summary; full defense in BUILD_PROMPT_v0.1 §3)

**Rust (stable, 2021+ edition) + statically bundled SQLite (`rusqlite`, `bundled`) + `serde_json` + `jsonc-parser` (CST) + `blake3`; distributed with `dist` (cargo-dist) as single static binaries.**

- Rust: no runtime, no GC, ~1 ms process start, deterministic latency, first-class tree-sitter bindings (needed in v0.2), official Rust MCP SDK (`rmcp`, needed in v0.3).
- Disqualified: Node/Bun/Deno (runtime or 50 MB+ binaries with 10–30 ms start), Python (runtime + venv), JVM (runtime + start time), Go (viable for v0.1, but tree-sitter and fast SQLite both require cgo, which breaks trivial cross-compilation), Zig (ecosystem too thin for JSONC CST editing and MCP).

---

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Claude Code changes a hook payload field | Tolerant parsing (unknown fields ignored, missing fields → degraded event), recorded-fixture contract tests pinned per Claude Code version, `velra doctor` reports schema drift |
| A Velra bug surfaces as a visible "hook error" | Hook subcommands always exit 0, never print invalid JSON, internal watchdog at 250 ms, panic catcher |
| Users distrust a tool that edits `settings.json` | Timestamped backup, dry-run diff (`velra enable --dry-run`), byte-identical `disable`, open source |
| The capsule is wrong and misleads the agent | Every line has a provenance tag; causal links are always `UNCONFIRMED`; the live repo is stated as authoritative |
| Scope creep into a "memory platform" | Each build prompt has an explicit out-of-scope list; memory (what happened) ≠ lifecycle (what must survive the boundary) |
| Privacy: prompts and outputs stored on disk | Local-only, 0600 permissions, secret redaction before write, sensitive paths stored as path+hash only, `velra disable --purge` |

---

## 7. Measuring success without telemetry

Velra ships with **zero telemetry**. Evidence of success comes from: GitHub stars and issues, release download counts, the public benchmark (v0.4), and structured dogfooding logs that stay on the dogfooder's machine (`velra status --report` produces a redacted, human-reviewable summary a user can choose to paste into an issue).

---

## 8. What Velra must never become

A context dashboard, a chat interface, a coding agent, a generic memory platform, a mandatory cloud service, a paid LLM dependency, a source-control replacement, an automatic destroyer of human sessions, a repository metadata generator, or a tool that blocks development when it fails.
