# DECISIONS.md

Every place where `prompts doc/BUILD_PROMPT_v0.1.md` was ambiguous, plus each
deviation from its letter, with a one-line rationale. The spec's tie-breaker
order is applied throughout: (1) never block or visibly disturb Claude Code,
(2) preserve data, (3) choose the simplest option.

## Claude Code compatibility (§6.3, `compat`)

| # | Decision | Rationale |
|---|---|---|
| D1 | `args` exec form gated at 2.1.139, `if` field at 2.1.85, `PostCompact` at 2.1.76, PowerShell tool at 2.1.84, `SessionStart` 1.0.62, `SessionEnd` 1.0.85, `PreCompact` 1.0.48 | Versions named in the official changelog. |
| D2 | Async hooks gated at 2.1.23; `PostToolUseFailure` at 2.1.119; `PostToolBatch` at 2.1.268 | The changelog never names an introducing version for these. The floor is the earliest version with direct evidence (a changelog line, or presence verified in the shipped 2.1.268 binary). Registering an event an older Claude Code does not know risks a settings validation error, so the conservative floor is safer than guessing lower; the system degrades gracefully without all three. |
| D3 | `velra enable` falls back to reading the version from an installed VS Code extension directory (`anthropic.claude-code-<version>-…`) when `claude --version` is absent | The spec says "absent → assume latest known"; this is strictly better information, and Claude Code is frequently installed only as an IDE extension (no `claude` on PATH). Still falls back to "assume latest known". |
| D4 | The `PreCompact` `systemMessage` ("⚡ Velra checkpoint saved: …") is emitted as specified even though Claude Code documents that it discards `systemMessage` (and `continue`) on `PreCompact` and `PostCompact` | The spec mandates the output; emitting it is harmless, and it becomes visible if Claude Code ever starts surfacing it. The user-visible confirmation in practice is the delivery message on the next `SessionStart`. |
| D5 | Shell-form registrations (Claude Code < 2.1.139) write the binary path with forward slashes on Windows | Shell form runs under Git Bash there; backslashes inside a double-quoted Bash string are error-prone, forward slashes always work. |

## Settings editing (§6.2, §6.4)

| # | Decision | Rationale |
|---|---|---|
| D6 | Edits are minimal text splices computed from `jsonc-parser` **AST byte ranges** rather than its CST mutation API | The hard requirement is that `enable` → `disable` returns a byte-identical file (A3) and that no non-Velra byte moves (A2). Splicing exact ranges — reusing the file's own comma/newline/indentation style, and restoring `[]`/`{}` when a container empties — makes that a property of the algorithm instead of a property of a third-party formatter. `jsonc-parser` is still the parser, as prescribed. |
| D7 | A backup is written only when a write will actually happen | Step 3 of §6.2 writes the backup before computing the change, but `enable` run twice must be a no-op; writing ten identical backups would evict real history from the 10-backup window. |
| D8 | Velra handlers that no longer match any desired registration (stale matchers, events unsupported by the detected Claude Code) are removed during `enable` | Otherwise a Velra upgrade or Claude Code downgrade leaves handlers that fire twice or fail; §6.2 step 5 covers only add/update. |
| D9 | When the settings file does not exist, `enable` creates it; `disable` afterwards leaves an empty `{}` file rather than deleting it | The byte-identical guarantee in §6.4 is stated for a file that existed before `enable`; deleting a file Velra created is more surprising than leaving `{}`. |
| D10 | Semantic verification (step 7) compares both documents after stripping Velra handlers **and** dropping empty `hooks`/event/group containers on both sides | Makes the comparison symmetric, so a pre-existing empty `"hooks": {}` does not read as a difference. |
| D11 | `disableAllHooks` is checked in the target settings file only, not in merged project/managed settings | Velra never reads project-level settings (§6.1); a wrong "hooks are disabled" warning is worse than a missing one. |

## Storage and reducer (§10, §11)

| # | Decision | Rationale |
|---|---|---|
| D12 | Extra secondary indexes (`ix_*`) are created alongside the normative schema | The normative schema has no index for the reducer's hot lookups (edits by session+path, commands by epoch); they change no semantics and keep the PreCompact barrier inside its 10 ms budget. |
| D13 | `trigger` is quoted as `"trigger"` in all SQL | It is a SQLite keyword; quoting avoids depending on fallback-token behaviour. |
| D14 | Turn-end scan hashing happens **outside** the batch transaction: the reducer splits the batch so a `Stop` event starts a new batch, hashes files first, then applies the event, re-checking the cursor | Hashing up to 64 files inside `BEGIN IMMEDIATE` would hold the write lock for tens of milliseconds and push concurrent hooks into the spool. |
| D15 | Spool files that fail to parse are left in place for 5 s before being quarantined into `spool/bad/` | A file may be mid-write by another process; never discard data that might still be complete. |
| D16 | Spool entries carry the project row (`root_path`, `is_git`) in addition to the event columns | The reducer needs the project root to resolve paths; the spec's "exact normalized event row" has no room for it and the extra field is ignored elsewhere. |
| D17 | Events are appended with `INSERT OR IGNORE` on `dedupe_key`, and the reducer advances its cursor to the last event it actually applied | Gives idempotent replay after a crash mid-batch (C3) without a lease. |

## Intents, files, reverts (§12, §13)

| # | Decision | Rationale |
|---|---|---|
| D18 | Revert detection collapses runs of identical hashes before searching for the return point, and uses the **latest** matching earlier version | Without collapsing, an unchanged re-observation (e.g. the `pre_edit` hash of the next edit) makes the edit that restored the original look reverted, producing a phantom dead end. |
| D19 | An observation that reapplies a dead end never also creates a new dead end | After `A→B`, revert to `A`, then `A→B` again, the re-application is the interesting fact; marking the intermediate restoring edit as a new dead end would be noise. |
| D20 | A git restore-family command counts as having touched a file only when `git_pre ≠ git_post` (or, without a pre observation, when the file no longer matches its last post-edit hash) | Prevents `git restore other.txt` from discarding edits to an unrelated file that something else had already changed. |
| D21 | `git commit` also triggers file rehashing in `pre-tool-use`/`post-tool-use`, and marks ACTIVE edits **up to and including** the edit whose hash matches the committed file as COMMITTED | §13.3 needs a "last COMMITTED marker" to exist; marking only the exact-hash edit would leave superseded earlier edits ACTIVE and render them as live attempts after they were committed. |
| D22 | `pre_edit`, `git_pre` and `original` observations that reveal an unexpected hash use mechanism `external` | §13.2 names mechanisms only for `git_post`, `post_edit` and `turn_scan`; "changed outside the agent" is the accurate description for the rest. |
| D23 | Windows path identity uses the canonical on-disk spelling where the file exists, and the project id is case-folded | §18 requires case-folded identity; canonicalizing gives a single identity per file while still displaying the real casing. |

## Commands and failures (§14)

| # | Decision | Rationale |
|---|---|---|
| D24 | Across subcommands, kind is chosen by priority `test > build > lint > git > other` rather than "first subcommand that matches anything" | `git stash && npm test` is a test run; classifying it as `git` would hide the failure that matters. |
| D25 | Failure markers are evaluated after neutralizing zero counts (`0 failed`, `0 errors`), and weak markers (`Error:`, bare `failed`, `FAIL `) are overridden by a strong pass summary on a successful exit | The literal marker list in §14 classifies `test result: ok. 3 passed; 0 failed` as FAIL. Strong markers (`FAILED`, `--- FAIL`, `error[`, `Traceback`, …) still win, so `pytest \| tail` (which masks the exit code) is still detected. |
| D26 | A `PostToolUse` shell event implies exit code 0 for outcome purposes, without storing a synthetic `exit_code` | Claude Code reports non-zero exits as `PostToolUseFailure`; `tsc` and friends print nothing on success, which would otherwise be UNKNOWN forever. |
| D27 | For `PostToolUseFailure`, the exit code and output come from parsing Claude Code's `Exit code N\n…` first line of `error` | That is the documented shape, and it is the only place a failed command's output survives. |
| D28 | Mentioned paths resolve against the command's `cwd` first, then the project root, and at most 16 are stored | Agents routinely `cd` into a subdirectory; unbounded `stat` calls would blow the reducer budget. |

## Capsule (§16)

| # | Decision | Rationale |
|---|---|---|
| D29 | With no ROOT captured, the section renders as `[ROOT_TASK_OBJECTIVE]` + `(not captured)` with no provenance parenthetical | Writing `(OBSERVED | user prompt | --:--)` would claim an observation that does not exist. |
| D30 | An UNKNOWN last-test outcome renders literally as `UNKNOWN` in STATUS | The template lists `PASS\|FAIL\|INTERRUPTED\|none`; UNKNOWN is a real stored outcome and hiding it as `none` would misreport. |
| D31 | After the §16.3 truncation ladder, additional steps (drop "Observed afterward", drop NEXT_KNOWN_TARGET, attempts → 0, failure excerpt → 0, shorter subtask/path/command caps, drop LATEST) run **only** when the result is still above the 1,000-token hard ceiling | §16.3 says to emit as-is between target and hard ceiling, but E2 requires the hard ceiling always. The extra steps make the ceiling a guarantee for any input. |
| D32 | E3 ("no causal language") is enforced on the renderer's own template text, not by rewriting evidence | Deleting the word "because" from a captured test excerpt would corrupt the evidence the capsule exists to preserve. A test asserts the template literals never contain the banned words. |
| D33 | Counted items are pluralized (`1 file`, `2 files`; `1 dead end`, `2 dead ends`) | The template's `{k} files` reads wrong at k=1; `{k} edit(s)` is kept literally as written. |
| D34 | `velra inspect` without `--checkpoint` renders with `checkpoint="preview"` and a RECOVERY line without `--checkpoint` | No checkpoint exists to reference, and §7 forbids creating one. |
| D35 | The capsule ends with `</VELRA_CONTINUATION>` and no trailing newline | Nothing depends on it and it keeps the golden snapshots unambiguous. |

## Delivery (§15)

| # | Decision | Rationale |
|---|---|---|
| D36 | The capsule is written to stdout **inside** the delivery transaction, immediately before commit; if the write fails the transaction rolls back and the continuation stays deliverable | Matches §18's accepted worst case (a crash between emit and commit re-emits once) while never losing a delivery because the commit succeeded and the write did not. |
| D37 | `reconcile` reads first and only takes the write lock when a transition is actually due | Keeps the common `UserPromptSubmit`/`PostToolUse` path to a single indexed read (§4 budgets). |
| D38 | Reconciliation also runs on `SessionStart(compact\|resume)`, `Stop` and `PostToolUseFailure` | T7 allows "any hook"; these are the events where a missed T3 would otherwise strand an ATTACHED continuation. |

## Runtime and safety (§4, §8, §9)

| # | Decision | Rationale |
|---|---|---|
| D39 | The watchdog thread takes the stdout guard before calling `exit(0)` | Guarantees the process never dies mid-write, so stdout is always either empty or one complete JSON object (§8.1 rule 2). |
| D40 | Fault injection (`VELRA_TEST_PANIC`, `VELRA_TEST_STALL_MS`) exists only under the `fault-injection` cargo feature | G2/G3 need it; shipping fault injection in release binaries does not. |
| D41 | Long output is tailed with a 1 KiB margin, redacted, then tailed again to the final size | A secret straddling the first cut would otherwise survive as an unmatched fragment. |
| D42 | The retained-payload budget is enforced by shrinking the largest text field (stdout/stderr/error/summary/prompt/command) until the serialized payload fits 16 KiB | §9.1's per-field caps (2 KiB command + 8 KiB + 8 KiB tails) can exceed the 16 KiB total; the total is the binding constraint. |
| D43 | `TZ=UTC` (and `Etc/UTC`, `GMT`, `UTC0`) forces a zero offset on every platform | Windows ignores `TZ`, and E1 requires byte-identical goldens across operating systems. |
| D44 | Unparseable stdin with a recoverable `session_id` is recorded as a `malformed` event; without one it is a silent no-op | §18 asks for both behaviours; the salvage scan is a plain substring search, no regex on the hot path. |
| D45 | The async `reduce` watchdog fires at 1,000 ms while the reducer's own deadline is 900 ms | Lets the reducer finish and commit its current batch instead of being killed mid-transaction. |
