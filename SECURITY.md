# Security and privacy

Velra is local-first. It has no accounts, no telemetry, no API keys, and makes
**no network requests at runtime** — the only network access in the whole
project is the installer downloading a release archive from GitHub.

## What is stored, and where

Everything lives under `$VELRA_HOME` (default `~/.velra`, `%USERPROFILE%\.velra`
on Windows), created with mode `0700` on POSIX; files are `0600`:

| Path | Contents |
|---|---|
| `velra.db` (+ `-wal`, `-shm`) | The event log and derived task state (SQLite, WAL) |
| `spool/` | Events written while the database was locked, pending ingestion |
| `staged/<workspace_id>/capsule.<gen>.json` | A capsule staged by `velra restore`, waiting for the next new session in that workspace (deleted on delivery, discarded after 7 days) |
| `logs/errors.log` | One line per internal error (rotated at 1 MiB, 3 files) |
| `logs/debug.log` | Per-invocation timing, only when `VELRA_LOG=debug` |
| `backups/` | Timestamped copies of your Claude Code settings, taken before each edit |
| `state.json` | The binary path and settings path recorded by `velra enable` |
| `config.toml` | Optional settings you create yourself |

Velra **never writes inside your repository** and never modifies project-level
`.claude/settings*.json`. The only file it edits is your user-level Claude Code
settings file, and only the hook handlers it owns.

## What is captured from your session

Per tool event, after redaction and truncation (≤ 16 KiB per event):

- user prompts (≤ 4 KiB) — the task objective the capsule restores;
- file paths, content hashes, line counts, and a two-line before/after excerpt
  of the first changed line of an edit;
- shell commands (≤ 2 KiB) and the last 8 KiB of stdout/stderr, ANSI-stripped;
- test/build/lint outcomes and a ≤ 8-line failure excerpt;
- hook lifecycle facts (session start/end, compaction triggers).

Velra does **not** copy your conversation into its database, does not call an
LLM, and does not copy whole files into the database — only hashes. Two
read-only exceptions touch Claude Code's transcripts: `velra restore` reads
the first 256 KiB of each transcript in the current workspace to label
sessions in its picker, and `velra inspect --trace` searches the chosen
session's transcript to report whether a string ever appeared in it.
Nothing read this way is stored.

## Cross-session restore

`velra restore` copies the rendered, redacted capsule of a session you name
into `$VELRA_HOME/staged/`. The next new Claude Code session **in the same
workspace** receives it as context. A capsule quotes your own prompts
(for example, the constraints you stated) and file paths, so anything you
typed into a prompt can reach the next session in that workspace. It is
redacted again before staging, it is never written inside the repository,
and `velra restore --clear` discards it. A staged file whose content hash or
workspace id does not match is refused, not delivered.

## Redaction

Every string is redacted before it is written anywhere, including the spool.
Matches are replaced with `[REDACTED:{kind}]`. Detectors cover AWS access keys
and secret-key assignments, GitHub tokens (`ghp_`, `gho_`, `ghu_`, `ghs_`,
`ghr_`, `github_pat_`), `sk-`/`sk-ant-` API keys, Slack tokens, Google API keys,
Stripe keys, JWTs, PEM private key blocks, URLs with embedded credentials,
bearer tokens, and generic `key/secret/token/password/auth = …` assignments.

**Sensitive paths** — `.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`,
`id_rsa*`, `id_ed25519*`, `.npmrc`, `.pypirc`, `.netrc`, `*credentials*`,
`*secret*`, `.ssh/**`, `.aws/**`, `*.kdbx` — are recorded as **path and hash
only**. No excerpt of their contents is ever stored or rendered.

Redaction is best-effort pattern matching. It is not a guarantee that an
unusual secret format will be caught. If you work with secrets in an unusual
format, prefer `VELRA_DISABLE=1`.

## Turning it off and purging

```sh
VELRA_DISABLE=1 claude       # one session, no database access at all
touch ~/.velra/disabled      # every session, until you delete the file
velra disable                # remove the hooks from Claude Code settings
velra disable --purge        # also delete ~/.velra (backups are kept)
rm -rf ~/.velra              # delete everything, backups included
```

`velra disable` restores your settings file byte for byte if Velra added the
only hooks in it.

## Supported versions

Security fixes land on the latest release. At the time of writing that is
0.1.x: the published 0.1.1, and 0.1.2 once released.

## Reporting a vulnerability

Please open a private security advisory on the GitHub repository rather than a
public issue. Include the Velra version (`velra --version`), your platform, and
a reproduction. We aim to acknowledge within 72 hours.
