# velra

**Local-first session continuity for Claude Code. Clear the context, keep the state.**

Velra records what a Claude Code session does and hands a small, bounded
record of where the work stands to the next context: after `/compact`, or
(with `velra restore`, 0.1.2+) in a brand-new session. It never replays the
conversation, calls no model, and never touches the network at runtime.

Full documentation: <https://github.com/aryanghai12/Velra>

## Install

```bash
npm install -g velra
velra enable
```

or without installing anything permanently:

```bash
npx velra enable
```

## Use

```bash
velra restore        # stage a previous session's state for your next new session
velra status         # enabled? tracking? anything staged here?
velra inspect        # preview the capsule; --trace <fact> shows where a fact was lost
velra doctor         # diagnose the installation
```

## What this package is

Velra is a native binary, not a Node program. This package is a launcher. On
first run it downloads the prebuilt binary **for this package's version**
from [GitHub Releases](https://github.com/aryanghai12/velra/releases),
verifies its SHA-256 against the published checksum, caches it under
`~/.velra/cache/`, and execs it.

There are no npm dependencies, no postinstall script, and no C toolchain
required. `velra enable` registers the *binary's* absolute path with Claude
Code, so your hooks never start Node.

Supported platforms: macOS (arm64, x64), Linux (x64, arm64, musl-static),
Windows (x64, arm64).

## Verify

```bash
velra --version
velra doctor
```

## Uninstall

```bash
velra disable          # restores ~/.claude/settings.json byte for byte
                       # (or: velra disable --purge  — also deletes ~/.velra, keeping backups/)
npm uninstall -g velra
```

## Environment

| Variable | Effect |
|---|---|
| `VELRA_BINARY` | Path to a velra binary to use as-is; skips all resolution. |
| `VELRA_HOME` | State and cache root (default `~/.velra`). |
| `VELRA_VERSION` | Release to fetch (default: this package's version). |
| `VELRA_DOWNLOAD_BASE` | Alternate archive source: an `https://` base URL or a local directory. |
| `VELRA_NO_DOWNLOAD=1` | Never reach the network; fail if the binary is not already cached. |

## License

MIT.
