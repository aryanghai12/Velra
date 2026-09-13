# Launch assets

Two things: a video script and a social post. Both are written against numbers
that are actually in `bench/results/`. If you change a number here, change it
there first.

**Claims you can make, because they are measured:**

- The delivered capsule was 703 real tokens on Anthropic's tokenizer, control delta 0.
- The native compaction summary ranged 670 to 6,937 tokens across four runs of one identical script.
- 0 non-zero exits and 0 stderr bytes across every hook invocation recorded.
- H1, H2, H3 passed. H4 failed.

**Claims you cannot make, because nothing here measured them:**

- Any specific dollar or token saving. Velra runs alongside the native summary, it does not replace it.
- Anything about prompt cache behaviour or rate limit quotas.
- That Velra made the agent solve the task faster. Both arms solved it in two tool calls.

---

## 1. Launch video script (75 to 90 seconds)

### Hook (0:00 to 0:15)

| Visual / Terminal Action | Voiceover Audio |
|---|---|
| Screen recording of a real Claude Code session, scrolled back far enough that the turn count is visible. The context meter is near full. Type `/compact` and let it run. | "Every long Claude Code session ends the same way. You hit the context limit, you compact, and a model writes a summary of everything you just did." |
| Cut to a side by side of two compaction summaries from the same script: one short, one very long. Overlay the numbers 670 and 6,937. | "Here is the same session, same script, summarised twice. Six hundred and seventy tokens one time. Nearly seven thousand the next. You cannot budget for that, and you cannot predict what it dropped." |

### The solution (0:15 to 0:35)

| Visual / Terminal Action | Voiceover Audio |
|---|---|
| Terminal clears. Title card: Velra. Under it, the tagline "Deterministic context persistence for Claude Code." | "Velra is an open source Rust tool that runs alongside that summary and adds the one thing it cannot promise: a record that is bounded, reproducible, and traceable." |
| Animate the pipeline: tool call, arrow to a SQLite icon, arrow to a `PreCompact` barrier, arrow to a capsule block. Keep it simple. | "It hooks Claude Code's tool events and writes them to a local SQLite log. Edits, reads, shell commands, and critically, git reverts. When a file is edited and then restored, that is an event, not a guess." |
| Show `[DEAD_ENDS]` highlighted inside a rendered capsule. | "So the thing a summary drops first, the approach you already abandoned, is the thing Velra tracks structurally." |

### Terminal walkthrough (0:35 to 0:55)

| Visual / Terminal Action | Voiceover Audio |
|---|---|
| Clean terminal. Type: `git clone <REPO_URL> && cd Velra` | "Three steps. Clone it." |
| Type: `cargo build --release -p velra`. Let the compile scroll, cut when it prints `Finished`. | "Build it. One crate, no runtime dependencies." |
| Type: `./target/release/velra enable`. Show the checkmark output naming `~/.claude/settings.json` and the backup path. | "And enable it. That writes hook registrations into your user level Claude settings, once." |
| Split screen: two different project directories, both running `velra status`, both showing enabled. | "User level means every project on the machine, automatically. There is no daemon, nothing sitting in memory. Claude Code starts a short lived process when an event fires and it exits." |

### The proof (0:55 to 1:15)

| Visual / Terminal Action | Voiceover Audio |
|---|---|
| Show the full `<VELRA_CONTINUATION>` block that was delivered in the benchmark. Scroll it once, slowly, so the tagged sections are readable. | "This is what gets handed back after compaction. Seven hundred and three tokens, measured on Anthropic's own tokenizer, not estimated." |
| Highlight the `(OBSERVED | test run)` and `(INFERRED | failure-location)` tags. | "Every line is tagged with where it came from. Observed means a tool call produced it. Inferred means Velra worked it out. You always know which." |
| Cut to the post compaction turn: the Edit on `engine.py`, then `pytest -q`, then `6 passed`. | "On the turn after compaction, the agent went straight to the defective function and fixed it in two tool calls, with zero source files re-read." |
| Cut to `assets/proof.png`, full frame. Hold on the H4 FAILED box for a beat. | "And in the same benchmark, vanilla Claude Code did exactly the same thing. It did not need the help on this particular bug, and the report says so. One of the four hypotheses failed outright. That is in the README too." |

### Outro (1:15 to 1:30)

| Visual / Terminal Action | Voiceover Audio |
|---|---|
| Back to the repo page. Cursor moves toward the star button but does not oversell it. | "If your sessions are long enough that compaction actually hurts, clone it and point it at a real one. The benchmark is reproducible with one command, and everything it produced is in the repo, including the parts that did not work." |
| End card: repo URL, and the line "v0.2 open problems are listed in the README." | "Open problems are listed. Bug reports welcome. Star it if it is useful." |

---

## 2. Social post

### Version A: X / Twitter

> Long Claude Code sessions all end the same way: you hit the context limit, `/compact` runs, and a model writes a summary of your session.
>
> I benchmarked that summary. Same repo, same 16 turn script, four runs.
>
> It came back at 670 tokens once and 6,937 another time. Twice the model read its own compaction template as a prompt injection and refused to summarise at all.
>
> That is not a thing you can budget for.
>
> So I built Velra: a Rust tool that hooks Claude Code's tool events into a local SQLite log, freezes a checkpoint at `PreCompact`, and hands back a bounded capsule on the other side.
>
> 703 real tokens, measured on Anthropic's tokenizer. Deterministic: same snapshot, same bytes. Every line tagged with whether it was observed or inferred.
>
> It tracks git reverts structurally, so the approach you already abandoned is recorded as an event rather than left to whatever the summary happened to keep.
>
> Three commands, user level install, works across every repo on the machine, no daemon.
>
> The honest part: in the same benchmark, vanilla Claude Code solved the task in the same two tool calls. It did not need the help on that bug. And one of my four hypotheses failed outright, on latency. Both are in the README and the full report.
>
> Open source, MIT: <REPO_URL>
>
> Star it if it is useful. Tell me what the capsule got wrong on your sessions.

### Version B: LinkedIn

> **What a Claude Code compaction summary actually costs you**
>
> When a long Claude Code session runs out of context, `/compact` replaces the conversation with a model written summary. I wanted to know how reliable that summary is, so I built a controlled benchmark: identical repository, identical 16 turn script, identical compaction boundary, one variable.
>
> Across four runs of the same script, the native summary ranged from **670 to 6,937 tokens**. Twice, the model treated its own compaction template as a prompt injection and declined to summarise.
>
> The variance is the problem. A continuation payload you cannot predict the size of is one you cannot budget for, and the thing a summary compresses out first is the negative result: the approach you already tried and rejected.
>
> **Velra** is what I built about it. It is a local first Rust tool that:
>
> - Hooks Claude Code's tool events into an append only SQLite log
> - Detects reverts by comparing content hashes rather than asking a model what happened
> - Freezes a checkpoint at `PreCompact` and renders a capsule through a deterministic truncation ladder
> - Delivers it exactly once after compaction, via a `PENDING -> ATTACHED -> CONFIRMED` state machine
>
> The capsule that shipped in the benchmark measured **703 real tokens** on Anthropic's own tokenizer, with a validated measurement control. Same snapshot in, same bytes out, on every platform.
>
> Install is three commands and writes to your user level Claude settings once, so it applies to every project on the machine. No daemon, nothing resident in memory.
>
> **The part I am not going to dress up:** in that same benchmark, vanilla Claude Code solved the planted defect in the same two tool calls, with the same zero file re-reads. On that task it did not need Velra. And one of my four hypotheses failed: `PreCompact` runs at 18.45 ms marginal p99 against a 15 ms budget. I considered raising the threshold and decided that any number I picked after seeing the failure would not be worth publishing.
>
> Both facts are in the README and the full benchmark report, alongside two open problems I have not solved yet.
>
> Open source under MIT: <REPO_URL>
>
> If you run long Claude Code sessions, I would genuinely like to know what the capsule gets wrong on yours.
