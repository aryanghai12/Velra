# velra

**Deterministic context persistence and token governance for Claude Code.**

Velra watches what Claude Code does, freezes it before `/compact`, and hands
back a bounded, reproducible record of the task on the other side.

Full documentation: <https://github.com/aryanghai12/velra>

## Install

```bash
npm install -g velra
velra enable
```

or without installing anything permanently:

```bash
npx velra enable
```

## What this package is

Velra is a native binary, not a Node program. This package is a launcher: on
first run it downloads the prebuilt binary for your platform from
[GitHub Releases](https://github.com/aryanghai12/velra/releases), verifies its
SHA-256 against the published checksum, caches it under `~/.velra/cache/`, and
execs it.

There are no npm dependencies, no postinstall script, and no C toolchain
required. `velra enable` registers the *binary's* absolute path with Claude
Code, so your hooks never start Node.

Supported platforms: macOS (arm64, x64), Linux (x64, arm64, musl-static),
Windows (x64, arm64).

## Verify

```bash
velra status
velra doctor
```

## Uninstall

```bash
velra disable        # restores ~/.claude/settings.json byte for byte
npm uninstall -g velra
rm -rf ~/.velra
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
