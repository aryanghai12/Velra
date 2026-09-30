# Security and privacy

Velra is local-first. It has no accounts, no telemetry and no API keys, and
makes **no network requests at runtime**. The only network access in the
project is an installer or the npm launcher downloading a release archive
from GitHub and checking its SHA-256.

This page lists what Velra stores, what it protects against, and what it
does not. It makes no general claim that Velra is "secure".

- [Reporting a vulnerability](#reporting-a-vulnerability)
- [Persistence surfaces](#persistence-surfaces)
- [What is captured from a session](#what-is-captured-from-a-session)
- [Transcripts](#transcripts)
- [Redaction](#redaction)
- [Staged capsules and cross-session exposure](#staged-capsules-and-cross-session-exposure)
- [Paths, processes and settings](#paths-processes-and-settings)
- [Diagnostics](#diagnostics)
- [Fail-open design](#fail-open-design)
- [Residual risks](#residual-risks)
- [Turning it off and purging](#turning-it-off-and-purging)
- [Supported versions](#supported-versions)

---

## Reporting a vulnerability

Open a **private security advisory** on the GitHub repository rather than a
public issue. Include the Velra version (`velra --version`), your platform
and a reproduction. We aim to acknowledge within 72 hours.

## Persistence surfaces

Everything Velra writes is under `$VELRA_HOME` (default `~/.velra`,
`%USERPROFILE%\.velra` on Windows), plus one edit to your Claude Code
settings file. On POSIX the directory is created `0700` and files `0600`
(`security::state_files_are_private_on_posix`). On Windows the files take
the default permissions of your user profile; Velra sets no ACLs of its own.

| Path | Contents |
|---|---|
| `velra.db` (+ `-wal`, `-shm`) | the redacted event log and derived task state (SQLite) |
| `spool/` | redacted events written while the database was busy |
| `staged/<workspace_id>/capsule.<gen>.json` | a capsule staged by `velra restore`, until delivered or 7 days old |
| `logs/errors.log`, `logs/debug.log` | internal errors and refusals; per-hook timing only with `VELRA_LOG=debug` |
| `backups/` | copies of your Claude Code settings file, taken before each edit |
| `state.json`, `config.toml`, `disabled` | what `enable` did; your settings; the kill switch |
| `velra.db.corrupt-*` | a database moved aside because SQLite reported it corrupt |

Velra **never writes inside your repository** and never touches
project-level `.claude/settings*.json`. It edits only its own handlers in
your user-level Claude Code settings file.

## What is captured from a session

Per hook event, redacted and then size-capped (a whole event is at most
16 KiB):

- **user prompts**, up to 4 KiB. Constraints and rejected approaches are
  extracted from the full prompt before it is capped. Context a client
  injects around a prompt (for example `<ide_opened_file>`) is recognised
  and not treated as the user's words;
- **file activity**: paths, content hashes, line counts, and a short
  before/after excerpt of an edit;
- **commands**, up to 2 KiB, and the last 8 KiB of their output,
  ANSI-stripped;
- **test, build and lint outcomes**, with a short failure excerpt;
- **lifecycle facts**: session start and end, compaction triggers;
- **Claude Code's compaction summary**, when `PostCompact` supplies one:
  redacted, capped at 32 KiB, stored for diagnostics. It is text a model
  wrote about your conversation. No capsule is built from it.

Velra does not store whole files (only hashes), does not record the
assistant's replies, and does not call any model.

## Transcripts

Two read-only paths open Claude Code's transcripts under
`~/.claude/projects/`:

- `velra restore` reads the first 256 KiB of each transcript in the current
  workspace to label sessions in its picker;
- `velra inspect --trace` searches the chosen session's transcript to
  report whether a string ever appeared in it.

Nothing read from a transcript is stored, and no capsule is built from one.

## Redaction

Every string is redacted before it is written anywhere, the spool
included, and a staged capsule is redacted again before it is written.
Matches become `[REDACTED:<kind>]`. The detectors (`crates/velra-core/src/redact.rs`):

| Kind | Matches |
|---|---|
| `private_key` | PEM and PGP private key blocks |
| `aws_access_key`, `aws_secret_key` | AWS access key ids; secret-key assignments |
| `github_token` | `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_` tokens |
| `api_key` | `sk-` and `sk-ant-` style keys |
| `slack_token`, `google_api_key`, `stripe_key` | those providers' key formats |
| `jwt` | JSON Web Tokens |
| `url_credentials` | `scheme://user:password@host` |
| `bearer_token`, `basic_auth` | `Authorization: Bearer …` and `Basic …` headers, including `Proxy-Authorization` |
| `cli_credential` | a credential passed as a flag's argument: `--password …`, `--token=…`, `--api-key …` |
| `generic_secret` | `key`/`secret`/`token`/`password`/`auth` `=` or `:` assignments |

**Sensitive paths** are recorded as path and hash only; no excerpt of their
contents is stored or rendered: `.env`, `.env.*`, `*.pem`, `*.key`,
`*.p12`, `*.pfx`, `*.kdbx`, `id_rsa*`, `id_ed25519*`, `.npmrc`, `.pypirc`,
`.netrc`, names containing `credentials` or `secret`, and anything under
`.ssh/` or `.aws/`.

Tests plant secrets in prompts, commands, output and file edits, and check
that none reaches the database, the spool, a capsule or any printed output
(`security::a_planted_secret_reaches_no_persisted_or_printed_surface`,
`j1_secrets_never_reach_the_database_or_the_capsule`).

## Staged capsules and cross-session exposure

`velra restore` copies the rendered, redacted capsule of a session **you
name** into `$VELRA_HOME/staged/<workspace_id>/`. The next new Claude Code
session in the **same workspace** receives it as context.

- A capsule quotes your prompts (the objective, the constraints you
  stated) and names file paths and test ids. Anything you typed into a
  prompt in the source session can reach the next session in that
  workspace.
- It is never written inside the repository, so `git add -A` cannot reach
  it.
- At delivery it is checked: content hash, owning workspace, format
  version, age (7 days) and size. A file that fails any check is refused.
  A staged file that was edited or copied between workspaces is therefore
  not delivered.
- `velra restore --clear` discards it. Automatic delivery never crosses
  sessions; only this explicit path does.

## Paths, processes and settings

- **Workspace identity** is derived from the canonical path, so symlinks,
  case and separator differences cannot make one directory two workspaces,
  or deliver one workspace's capsule into another.
- **Claude Code detection** runs `claude --version` only from absolute
  directories on `PATH`, never from the current directory, so a `claude`
  checked into a repository is not executed. It is stopped after 3
  seconds; on Windows, together with every process it started (`taskkill /T`).
- **Settings edits** preserve comments, key order and a byte order mark,
  take a backup first, and replace the file atomically. `velra disable`
  restores it byte for byte when Velra's hooks were the only ones added.
- **Atomic writes** (settings, backups, staged capsules, spool files) use a
  temp file in the same directory, fsync and rename.

## Diagnostics

`logs/errors.log` is always on and records errors and refusals, with file
paths and session ids. `logs/debug.log` exists only with `VELRA_LOG=debug`
and records per-hook timing and delivery events. Every log line is
redacted before it is written (D124). Log lines are meant for identifiers,
paths, errors and timings, not prompt text, but an error or panic message
can quote a value it was handling. `velra inspect` and
`velra restore --dry-run` print capsule text, which quotes your prompts.
Review any of these before you paste them into a bug report.

## Fail-open design

Hooks always exit 0, never write to stderr, and write at most one JSON
object. A 250 ms watchdog bounds how long a synchronous hook can hold Claude
Code up. A locked, corrupt, read-only or missing database never surfaces as
a Claude Code error. The trade-off: a problem shows up as Velra *not doing
something*, recorded in `errors.log`, never as a visible failure.

## Residual risks

- **Redaction is pattern matching.** A secret in an unusual format can pass
  through into the database, a capsule, or a restored session. If you work
  with such secrets, run that session with `VELRA_DISABLE=1`.
- **Prompts travel.** Whatever you type into a prompt can be carried into
  the next session in that workspace by a restore.
- **Local access.** The database and staged capsules are plain files. Anyone
  who can read your user account's files can read them. Velra does not
  encrypt at rest.
- **Windows permissions** follow your profile's defaults, not an explicit
  private ACL.
- **Settings races.** A concurrent writer to Claude Code's settings file
  can, in a narrow window, have a save overwritten by `enable`/`disable`
  (D143). The backup taken first is the recovery.
- **Claude Code interfaces.** Hook payloads and transcripts are not a
  stable public interface; Velra parses them tolerantly and fails open.

## Turning it off and purging

```sh
VELRA_DISABLE=1 claude       # one session, no database access at all
touch ~/.velra/disabled      # every session, until you delete the file
velra restore --clear        # discard a staged capsule for this workspace
velra disable                # remove the hooks from Claude Code settings
velra disable --purge        # also delete ~/.velra, keeping backups/
rm -rf ~/.velra              # delete everything, backups included
```

## Supported versions

Security fixes land on the latest release. At the time of writing that is
0.1.x: the published 0.1.1, and 0.1.2 once it is released.
