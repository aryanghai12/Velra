# Installing Velra

Velra is one native binary. It has no runtime dependencies, no daemon and no
network access at runtime. Installing it has two steps: put the binary on
your `PATH`, then register its hooks with Claude Code (`velra enable`).

- [Requirements](#requirements)
- [Which version you get](#which-version-you-get)
- [Install a prebuilt binary](#install-a-prebuilt-binary)
- [Build from source](#build-from-source)
- [Register the hooks](#register-the-hooks)
- [Verify](#verify)
- [Upgrade](#upgrade)
- [Uninstall](#uninstall)

---

## Requirements

| | |
|---|---|
| **Claude Code** | Any version with hooks. `SessionStart`, which restore delivery depends on, arrived in 1.0.62. Velra detects the installed version and registers only the hooks it supports ([compatibility](CONFIGURATION.md#claude-code-version-compatibility)). The v0.1.2 benchmark ran on **2.1.280**. |
| **Operating system** | Windows 10/11 (x64, ARM64), macOS (Apple silicon, Intel), Linux (x64, aarch64). CI builds and tests every change on Windows, macOS and Linux. The live benchmark ran on Windows 11 x64 only. |
| **Prebuilt install** | Nothing else: no compiler, no Rust, no Node (unless you use the npm launcher). |
| **Building from source** | Rust **1.98** or newer, plus a C compiler for the bundled SQLite ([below](#build-from-source)). |

Prebuilt artifacts, per release:

| OS | Architectures | Archive |
|---|---|---|
| Linux | x86_64, aarch64 | `velra-<arch>-unknown-linux-musl.tar.gz`, statically linked, any distro |
| macOS | arm64, x86_64 | `velra-<arch>-apple-darwin.tar.gz` |
| Windows | x64, ARM64 | `velra-<arch>-pc-windows-msvc.zip`, static CRT, also runs on Windows-GNU hosts |

Every archive has a published `.sha256`, and every install path below
verifies it. Release archives carry GitHub build-provenance attestations.

## Which version you get

**`velra restore` and staged `SessionStart` delivery need Velra 0.1.2 or
later.** Check with:

```bash
velra --version
# velra 0.1.2 (<commit>, <target>)
```

The installers, the npm launcher and `cargo binstall` all fetch a **published
GitHub release**. The installers default to the latest one.

> **Release status (2026-09-23):** this repository is at **0.1.2**. The
> latest *published* release on GitHub Releases, npm and crates.io is
> **0.1.1**, which predates `velra restore`. Until 0.1.2 is published, get
> 0.1.2 by [building from source](#build-from-source). Once it is published,
> the commands below install it, and `VELRA_VERSION=0.1.2` pins it.

If `velra --version` shows 0.1.1, the only visible difference is that `velra
restore` is an unrecognised subcommand. Everything else works.

## Install a prebuilt binary

Each method installs to `~/.velra/bin` (`%USERPROFILE%\.velra\bin` on
Windows) unless noted. None needs administrator rights.

### Windows — PowerShell

```powershell
irm https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.ps1 | iex
```

From **cmd.exe**, start PowerShell for it:

```bat
powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.ps1 | iex"
```

To install **and** register the hooks in one step:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.ps1))) -Enable
```

The script downloads `velra-<arch>-pc-windows-msvc.zip`, checks its SHA-256,
installs `velra.exe` to `%USERPROFILE%\.velra\bin` and adds that directory to
your **user** `PATH`. Open a new terminal afterwards. Options:

| Option | Effect |
|---|---|
| `-Enable` | run `velra enable` after installing |
| `-NoModifyPath` or `$env:VELRA_NO_MODIFY_PATH = "1"` | leave `PATH` alone |
| `$env:VELRA_VERSION = "0.1.2"` | install that release instead of the latest |
| `$env:VELRA_HOME = "D:\tools\velra"` | install root (the binary goes in `<root>\bin`) |

### macOS and Linux — shell

```bash
curl -LsSf https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.sh | sh
```

With hook registration:

```bash
curl -LsSf https://raw.githubusercontent.com/aryanghai12/velra/main/install/install.sh | sh -s -- --enable
```

If `~/.velra/bin` is not already on `PATH`, the script appends
`export PATH="$HOME/.velra/bin:$PATH"` (tagged `# added by velra`) once to
your shell's rc file (`.zshrc`, `.bashrc`/`.bash_profile`, fish
`config.fish`, else `.profile`), when that file exists. Options: `--enable`,
`--no-modify-path` (or `VELRA_NO_MODIFY_PATH=1`), `VELRA_VERSION`,
`VELRA_HOME`.

### npm (any platform)

```bash
npm install -g velra
velra enable
```

or without a global install:

```bash
npx velra enable
```

The npm package is a launcher with zero dependencies and no postinstall
script. On first run it downloads the release binary **matching the
package's own version**, verifies it, caches it under
`~/.velra/cache/v<version>/`, and execs it. `velra enable` registers the
native binary's path, so hooks never start Node. Environment:
`VELRA_BINARY` (use this binary), `VELRA_VERSION`, `VELRA_DOWNLOAD_BASE`,
`VELRA_NO_DOWNLOAD=1` (offline; fail if not cached), `VELRA_HOME`.

### cargo-binstall

```bash
cargo binstall velra
```

This downloads the same release archive (no compilation). Install
`cargo-binstall` itself from a prebuilt binary, not with `cargo install`,
which compiles hundreds of crates and needs the C toolchain this path avoids:

```powershell
# Windows
irm https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.ps1 | iex
```

```bash
# macOS / Linux
curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
```

## Build from source

Needed for contributors, and for anyone who wants 0.1.2 before its artifacts
are published. The build compiles SQLite from source (`rusqlite` with
`bundled`), so it needs a C compiler:

| Platform | C toolchain |
|---|---|
| Windows (MSVC Rust host, the default) | Visual Studio Build Tools with "Desktop development with C++" |
| Windows (GNU Rust host, `x86_64-pc-windows-gnu`) | MinGW-w64 GCC on `PATH` (for example WinLibs); `gcc.exe` and `dlltool.exe` must resolve |
| macOS | Xcode Command Line Tools (`xcode-select --install`) |
| Linux | `cc` (for example `build-essential` or `gcc`) |

```bash
git clone https://github.com/aryanghai12/Velra.git
cd Velra
cargo build --release -p velra
```

The binary is at `target/release/velra` (`target\release\velra.exe` on
Windows). To put it on your `PATH` through Cargo:

```bash
cargo install --path crates/velra --locked
```

That installs to `~/.cargo/bin`, which rustup already adds to `PATH`.

`velra enable` registers the path of **the binary you run it with**, so run
it from the location you intend to keep. Running it from `target/release`
ties your hooks to your build directory.

The repository pins no toolchain file. The minimum supported Rust version
(1.98) is declared in `Cargo.toml` and checked by its own CI job.

## Register the hooks

```bash
velra enable
```

```
✓ Velra enabled for Claude Code.
  Settings: ~/.claude/settings.json  (backup: ~/.velra/backups/settings.json.<timestamp>.bak)

Nothing else required. Keep coding normally.
```

This writes Velra's handlers into your **user-level** Claude Code settings
(`~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR/settings.json`). They then
apply to every project on the machine. A timestamped backup is taken first.
Comments, key order and formatting in the file are preserved. Project-level
`.claude/settings*.json` files are never touched.

Preview the exact change without writing anything:

```bash
velra enable --dry-run
```

Hooks registered, 13 handlers across 10 events on a current Claude Code:
`SessionStart`, `UserPromptSubmit`, `PreToolUse` (edits, Bash, PowerShell),
`PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Stop` (hook and async
reducer), `PreCompact`, `PostCompact`, `SessionEnd`. On older Claude Code
versions, unsupported events are skipped and listed.

Running `velra enable` again is safe. It also repairs a moved binary path.

## Verify

```bash
velra --version
velra status
velra doctor
```

`velra status` (exit 0 when healthy):

```
✓ Enabled (13 hook handlers registered)
  Claude Code: 2.1.280
  Binary:      /home/you/.velra/bin/velra
  Database:    /home/you/.velra/velra.db (244 KiB)
  Tracking:    3 session(s), 78 event(s)
  Last event:  1h ago
  Continuations: none live
```

`velra doctor` exits non-zero if any check fails:

```
✓ settings parse: /home/you/.claude/settings.json
✓ binary: /home/you/.velra/bin/velra
✓ 13 hook handlers registered across 10 events
✓ database: WAL, schema v2
✓ last hook event 1h ago
✓ no recent errors
```

The database appears after the first hook event, so start one Claude Code
session before expecting `Tracking` to be non-zero. If something is wrong,
see [Troubleshooting](TROUBLESHOOTING.md).

## Upgrade

Re-run the install method you used (with `VELRA_VERSION` to pick a release),
then run `velra enable` once so the registered path and hook set match the
new binary. Upgrades keep your database, since the schema migrates in place.

## Uninstall

```bash
velra disable
```

This removes Velra's handlers from your Claude Code settings. If Velra's hooks
were the only ones in the file, the file is restored byte for byte. Preview
with `velra disable --dry-run`.

Then remove the binary and, optionally, Velra's data:

| How you installed | Remove the binary |
|---|---|
| install script | delete `~/.velra/bin/velra` (`%USERPROFILE%\.velra\bin\velra.exe`) and the `# added by velra` PATH line, or the user `PATH` entry on Windows |
| npm | `npm uninstall -g velra` |
| cargo install / binstall | `cargo uninstall velra` |

```bash
velra disable --purge        # also delete ~/.velra, keeping backups/ (asks first; --yes to skip)
```

---

[← README](../README.md) · [CLI reference](CLI.md) · [Configuration](CONFIGURATION.md) · [Troubleshooting](TROUBLESHOOTING.md)
