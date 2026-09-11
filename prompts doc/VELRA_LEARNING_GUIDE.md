# Velra Learning Guide — the "why" and "how" behind every decision

This guide is for you, not for the coding agent. The build prompts are deliberately dry ("what, not how"). This file explains, in plain language, why each rule exists and how the mechanism works, so you can defend the design in an interview, a pitch, or a code review.

---

## Part 1 — The core concepts (read in order)

### 1. What a "hook" is
- **What:** Claude Code lets you register small programs that it runs automatically at specific moments: when a session starts, when you submit a prompt, after every tool call, before compaction, and so on.
- **Analogy:** A doorbell wired to a light. Every time someone presses the bell (an event), the light (your program) turns on for a moment, then turns off.
- **Why it matters for Velra:** Hooks are the only *supported* way to observe what the agent does and to add information to its context. Velra never hacks the terminal or Claude's internals; it only answers doorbells.
- **Where in the spec:** v0.1 §6.3 (which doorbells Velra listens to), §8 (what it does each time).

### 2. How Claude Code and Velra talk (stdin / stdout)
- **What:** When a hook fires, Claude Code starts the `velra` program and pipes a JSON message into it ("stdin"). Velra can reply by printing JSON ("stdout"). The exit code (a number returned when the program finishes) tells Claude Code whether things went fine.
- **Analogy:** Passing a note under a door. Claude slides a note in; Velra may slide one note back; then the door closes.
- **Why the strict rules (exit 0, one JSON object, nothing on stderr):** Some exit codes have powerful meanings. Exit code 2 on `PreCompact` *blocks compaction*; on `UserPromptSubmit` it *erases your prompt*. Plain text printed on some events is *added to Claude's context*. A tiny bug could therefore sabotage the session. So Velra is only allowed one safe reply shape.
- **Where:** v0.1 §8.1.

### 3. Why a new process starts on every event (and why speed is everything)
- **What:** Each hook is a brand-new program launch. Velra starts, does its job, and exits, hundreds of times per session.
- **Why the 3–5 ms budget:** If each launch took 50 ms, and Claude made 300 tool calls, you would add 15 seconds of waiting. Developers feel lag instantly and uninstall.
- **Where:** v0.1 §4.

### 4. Why Rust (and not Python, Node, or Go)
- **Reasoning, step by step:**
  1. Python and Node need a *runtime* installed first (the "zero prerequisites" rule forbids that) and take 20–100 ms just to wake up.
  2. Bun/Deno can bundle into one file, but the file is huge and still slow to start.
  3. Go starts fast, but the parts Velra needs later (the code parser tree-sitter, and fast SQLite) are C libraries; Go needs a C bridge ("cgo") for them, which makes building for six platforms painful.
  4. Rust compiles to plain machine code with no runtime, starts in about a millisecond, and links C libraries like SQLite and tree-sitter directly into one file.
- **Analogy:** A ready-to-eat meal (Rust binary) versus a meal kit that needs a kitchen (Python needs an interpreter).
- **Where:** v0.1 §3.

### 5. What a "single static binary" means
- **What:** One file that contains everything it needs. No installer wizard, no libraries to hunt for.
- **Why:** That is what makes `curl … | sh` installs work like `fzf` or `zoxide`. It also means "works on my machine" problems mostly disappear.
- **Where:** v0.1 §3, §5.

### 6. Why `curl | sh` and not `npm install -g`
- **Reasoning:** `npm` only exists if Node.js is installed. Requiring Node breaks the zero-prerequisite promise. npm is kept as an optional extra for people who already have it.
- **Safety detail:** the installer checks a SHA-256 checksum, a fingerprint of the file, so a tampered download is refused.
- **Where:** v0.1 §5.

### 7. Editing `settings.json` safely
- **What `velra enable` does:** it adds Velra's hook entries to your Claude Code settings file.
- **Why so many rules (backup, comment preservation, atomic write, byte-identical disable):** This file belongs to the user. If Velra deletes their comments or corrupts it, trust is gone forever. "Atomic write" means: write the new version to a temporary file, then swap it in one instant step, so a crash can never leave a half-written file.
- **Analogy:** Editing a signed contract. You photocopy it first (backup), you use correction tape only on your own clause (touch only Velra entries), and you swap the pages in one motion (atomic rename).
- **Where:** v0.1 §6.

### 8. SQLite and WAL mode
- **What:** SQLite is a tiny database stored in one file. WAL ("write-ahead log") mode lets many readers read while one writer writes.
- **Analogy:** A shared notebook. In WAL mode, writers jot new entries on sticky notes stuck to the back (the log) while readers keep reading the clean pages. Periodically, the sticky notes are copied into the notebook.
- **Why "tiny writes" matter:** only one writer at a time. If each hook's write takes 0.1 ms, hooks almost never wait for each other. If each did heavy work, they would queue up and slow Claude down.
- **Where:** v0.1 §10.

### 9. The spool (the "never lose an event" fallback)
- **What:** If the database is busy for too long, the hook writes its event into a tiny separate file instead, and exits immediately. Later, the reducer imports those files.
- **Analogy:** If the post office is closed, you drop the letter in the overnight box. It gets processed in the morning; nothing is lost.
- **Where:** v0.1 §10.3.

### 10. Append-only events + a reducer
- **What:** Hooks only *append* raw facts ("Edit happened on file X"). A separate step, the reducer, reads new events and updates the summary tables ("file X edited 3 times; last test failed").
- **Why split it:** appending is fast and safe; summarising is slower. Separating them keeps hooks fast and makes the summary rebuildable from raw facts (great for debugging).
- **How the reducer runs without a background service:** Claude Code supports *async hooks* that run in the background. Velra uses two of those moments (`PostToolBatch`, `Stop`) as a free scheduler. No daemon to install.
- **Why the cursor:** the reducer remembers the last event it processed ("bookmark"). Each batch reads the bookmark and moves it forward inside one transaction, so two reducers can never process the same event twice.
- **Where:** v0.1 §11.

### 11. The PreCompact "barrier"
- **What:** Right before Claude compacts, Velra finishes any remaining summarising (with a strict 6 ms deadline), freezes the state into a checkpoint, and exits.
- **Why it must never read the transcript:** the transcript can be 100 MB. Reading it would take seconds and could time out. Because the state was built incrementally all along, there is nothing left to discover at this moment; only a quick "save".
- **Analogy:** A photographer who has been adjusting the lighting all day only needs to press the shutter when the moment arrives.
- **Where:** v0.1 §15.2.

### 12. Delivery: why three channels
- **The original spec** delivered state only on your *next prompt*.
- **The problem found:** auto-compaction often happens *in the middle* of Claude working. Claude keeps going for many steps without a new prompt from you, and during those steps it would have forgotten everything. That is precisely when it repeats dead ends.
- **The fix:** Claude Code fires `SessionStart` with source `compact` right after compaction. Velra delivers there first. Fallbacks: the first tool call after compaction, then the next prompt.
- **Where:** v0.1 §15.3, and the roadmap's corrections table.

### 13. Two-phase commit (PENDING → ATTACHED → CONFIRMED)
- **What:** Velra does not assume a delivery "worked" just because it sent it. It waits for evidence (Claude actually used a tool or finished a turn afterwards).
- **Why:** If you press Ctrl+C right after sending a prompt, the context attached to that prompt may never have been processed. Velra then re-delivers on your next prompt.
- **Analogy:** Registered mail. "Sent" is not "received"; you wait for the signature.
- **Where:** v0.1 §15.4.

### 14. Idempotency (the "elevator button" rule)
- **What:** Doing the same operation twice has the same effect as doing it once.
- **Why:** Hooks can be retried, and parallel tool calls can race. A unique `delivery_key` and a "only one process may flip PENDING to ATTACHED" rule ensure the capsule is never injected twice by accident.
- **Analogy:** Pressing the elevator call button five times still calls one elevator.
- **Where:** v0.1 §15.5.

### 15. Fail-open
- **What:** If Velra breaks, Claude keeps working normally, as if Velra were not installed.
- **Analogy:** A turnstile that swings open during a power cut rather than trapping people.
- **Why it is also a growth rule:** one visible error message in Claude's transcript reads as "this tool breaks Claude", and people uninstall.
- **Where:** v0.1 §18.

### 16. Content hashes and dead-end detection
- **What a hash is:** a short fingerprint computed from a file's exact contents. Same contents, same fingerprint; one character changed, completely different fingerprint.
- **How Velra spots a dead end:** it records the fingerprint before and after every edit. If a file goes A → B → A, then B was tried and abandoned. If Claude runs `git restore file`, the uncommitted edits were discarded. Both become "dead ends".
- **Why this is the killer feature:** native compaction summaries tend to lose "we tried X and threw it away". Velra keeps it, with the exact changed lines.
- **Where:** v0.1 §13.

### 17. Deterministic template instead of an AI summary
- **What:** The capsule is filled in by fixed rules from database rows. No LLM writes it.
- **Why:** (1) no API key or cost; (2) same input, same output, so it can be tested byte-for-byte; (3) it cannot invent things.
- **"No invented causal claims":** Velra writes "Observed afterward: test failed. Causal link: UNCONFIRMED" rather than "the change caused the failure". Correlation is not causation, and saying so keeps the agent from building on false beliefs.
- **Why the header is factual, not bossy:** Claude Code's docs warn that injected text phrased like out-of-band system commands can trigger Claude's prompt-injection defenses. "Velra recorded this state" works; "YOU MUST FOLLOW THESE RULES" backfires.
- **Where:** v0.1 §16.

### 18. The token budget (≤800) and the 10,000-character ceiling
- **Why small:** the whole point is a *clean* context. A 5,000-token capsule would recreate the clutter compaction just removed.
- **Why the character ceiling:** Claude Code caps hook output at 10,000 characters and moves anything longer into a file, which the model then has to go and read. So Velra stays below 9,500.
- **How truncation stays fair:** there is a fixed order of what gets trimmed first (least important sections first), so the objective, the failure, and the dead ends survive.
- **Where:** v0.1 §16.3.

---

## Part 2 — v0.2 concepts

### 19. AST parsing with tree-sitter
- **What:** Turning source code into a tree of its structure: this is a class, inside it a method, inside that an `if`.
- **Analogy:** Diagramming a sentence into subject, verb, and object, instead of seeing it as a string of letters.
- **Why it matters:** "lines 40–75" may slice through the middle of a function. "rotateToken(oldToken: string)" is a complete, meaningful unit the agent can act on. That is "boundary-complete".
- **Why only in the background:** parsing takes 5–50 ms. Hooks must stay at 5 ms, so parsing happens in the async reducer.

### 20. Failure fingerprints and normalization
- **Problem:** the same failing test prints slightly different output every run (timestamps, durations, temp folders, memory addresses).
- **Fix:** strip the noise first (replace `12:03:44` with `<TS>`, `/tmp/abc123` with `<TMP>`), then fingerprint what remains. Same bug, same fingerprint.
- **Why it enables loop detection:** you can now say "this exact failure happened 4 times".

### 21. Flaky tests vs loops
- **Flaky:** fail, pass, fail, pass *with no code changes in between*: the test is unreliable, not the agent.
- **Loop:** same failure repeatedly *while the code keeps changing back and forth*.
- **Why the difference is sacred:** a false "you're looping" alarm on a flaky test destroys trust faster than ten correct alarms build it.

### 22. Evidence classes (VERIFIED / OBSERVED / INFERRED / UNCONFIRMED / STALE)
- **Why:** so the agent knows how much to trust each line. A fact re-checked against the disk right now is stronger than one recorded an hour ago; a file that changed since makes the note stale.

---

## Part 3 — v0.3 concepts

### 23. MCP (Model Context Protocol)
- **What:** A standard way for an AI agent to call external tools. Velra runs a small local MCP server with read-only tools like `velra_attempts`.
- **Analogy:** A library catalogue. The capsule is the sticky note on your desk; MCP is the catalogue you can search when you need the full book.
- **Why read-only:** a server that only reads can never slow down or corrupt the hooks that write.

### 24. Hot / Warm / Cold memory
- **Analogy:** Desk (hot: always in front of you), drawer (warm: fetched when starting a fresh session), basement archive (cold: searchable, never in the way).
- **Why "never delete":** over-pruning is dangerous; you just move things further away.

### 25. `velra resume`
- **How it works:** Velra creates a one-time token, starts `claude` with that token in its environment, and the `SessionStart` hook sees the token and delivers the capsule plus working set. The token is consumed, so it is delivered exactly once.

---

## Part 4 — v0.4 concepts

### 26. Adapters and capability flags
- **Analogy:** Travel plug adapters. The appliance (Velra's core) stays the same; each country (Cursor, Codex…) gets an adapter.
- **Why capability flags:** agents differ (some cannot inject context after compaction). Velra checks what each agent can do and picks the best available path instead of pretending.
- **Why the "verification gate":** vendor APIs change monthly. Writing adapters from memory produces broken integrations.

### 27. Autonomous rotation
- **What:** For unattended pipelines only, Velra may decide to continue in a fresh session when the evidence (loop + no progress + safe moment) is strong.
- **Why shadow mode first:** it logs what it *would* have done, so you can check its judgment before letting it act.

### 28. The benchmark (conditions A–E)
- **Why it exists:** without measurement, every claim is marketing. Comparing against the "manual expert workflow" (condition B) answers the real question: does Velra match a disciplined human without the effort?
- **Why publish losses:** developer audiences trust honest numbers and punish inflated ones.

---

## Part 5 — How to drive these build prompts with Cursor (no coding background needed)

1. **Create an empty repository and open it in Cursor.** *Reason:* the agent needs a clean workspace so nothing unrelated confuses it.
2. **Paste the whole of `BUILD_PROMPT_v0.1.md` as the first message and ask: "Produce PLAN.md: a milestone-by-milestone plan for M1–M6 with the acceptance tests each milestone must pass. Do not write code yet."** *Reason:* a plan you can read lets you catch misunderstandings before thousands of lines exist.
3. **Review PLAN.md against §2 (scope) and §22 (milestones).** Ask: "Which parts of the spec did you leave out?" *Reason:* agents silently skip hard parts; making them list omissions surfaces it.
4. **Implement one milestone per session: "Implement M1 only. Stop when its acceptance tests pass. Show me the test output."** *Reason:* small steps keep each change reviewable and match the micro-session workflow Velra itself is built around.
5. **After each milestone, ask the agent to update DECISIONS.md.** *Reason:* this becomes your record of every judgment call, and a strong artifact for your portfolio.
6. **Run the tests yourself** with the command the agent gives you (usually `cargo test`), and paste any failures back. *Reason:* you verify reality instead of trusting the agent's claim.
7. **Before release, do the manual E2E checklist (v0.1 §21-I) yourself in real Claude Code.** *Reason:* automated tests prove the parts; only a real session proves the experience.
8. **Do not start v0.2 until v0.1's Definition of Done is met.** *Reason:* each version stands on the previous one's invariants; cracks compound.
