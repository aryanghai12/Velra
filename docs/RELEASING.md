# Releasing Velra

The maintainer's runbook. It covers what publishes what, and the order that
keeps the installers, the npm launcher and `cargo binstall` consistent.

## State of v0.1.2 (2026-09-23)

| Channel | Published | This branch |
|---|---|---|
| GitHub Releases (installers, npm launcher, binstall download from here) | **v0.1.1** | 0.1.2 prepared |
| npm `velra` | **0.1.1** | `npm/velra/package.json` = 0.1.2 |
| crates.io `velra`, `velra-core` | **0.1.1** | `Cargo.toml` workspace version = 0.1.2 |
| git tag `v0.1.2` | not created | — |

Nothing for 0.1.2 has been published. Until it is, `install.ps1`/`install.sh`
resolve `releases/latest`, which is 0.1.1.

## What depends on what

- **GitHub release artifacts** come from `.github/workflows/release.yml`. It
  builds six targets (Linux musl x64/arm64, macOS x64/arm64, Windows MSVC
  x64/arm64), packages them with `.sha256` files and a `sha256.sum`, attests
  build provenance, and creates the release. The *publish* job runs **only on
  a pushed `v*` tag**. `workflow_dispatch` with a tag input builds and uploads
  workflow artifacts but publishes no release (a dry run).
- **Installers** (`install/install.ps1`, `install/install.sh`) download
  `releases/latest/download/…`, or `releases/download/v$VELRA_VERSION/…` when
  pinned. They verify the `.sha256`.
- **npm launcher** (`npm/velra/bin/velra.js`) downloads
  `releases/download/v<package version>/…`. A published npm 0.1.2 therefore
  needs a GitHub release **v0.1.2** to exist, or `npx velra` fails on first
  run.
- **`cargo binstall`** uses `package.metadata.binstall` in
  `crates/velra/Cargo.toml` → `releases/download/v{version}/velra-{target}.{tar.gz|zip}`.
  It has the same dependency on the GitHub release.
- **crates.io**: `velra` depends on `velra-core = "0.1.2"`, so `velra-core`
  must be published first.
- **Homebrew**: `install/homebrew/velra.rb` is a *template* for the
  separate `aryanghai12/homebrew-tap` repository, with placeholder checksums.
  It is not installable from this repository. Fill in the checksums from
  `sha256.sum` and publish the tap after the release assets exist. Tap
  publishing is not automated.

## Order

The agreed sequence: benchmark evidence → docs/release preparation →
**package/artifact publication** → final validation → merge to `main` → final
tag.

Because the GitHub release is created by a tag push, the artifact step and
the tag step are coupled. Pick one of these deliberately:

- **A. Tag-driven (what the workflow supports).** Merge, tag `v0.1.2` on the
  merged commit, and let `release.yml` publish the six archives. Then publish
  npm and crates.io, update Homebrew, and validate. The tag comes before
  npm/crates.io, not after.
- **B. Artifacts before the tag.** Run `release.yml` by `workflow_dispatch`
  (dry run) to build the archives, then create the GitHub release and upload
  the archives, `.sha256` files and `sha256.sum` by hand. Doing that creates
  the tag unless you use a draft release. A draft is invisible to
  `releases/latest` and to `releases/download/v0.1.2/…` for anonymous
  clients, so the installers and npm cannot be validated against it until
  it is published.

Either way, never move or recreate an existing tag.

## Checklist

Before publishing:

- [ ] `cargo fmt --all -- --check`, `cargo clippy … -D warnings`, `cargo test --workspace --all-features` green on the release commit (CI on all three OSes)
- [ ] `python -m pytest bench/tests -q`, `python bench/tokenburn/run.py --selftest`, `python scripts/smoke.py` green
- [ ] `velra --version` from a release build reports `0.1.2` and the release commit
- [ ] `cargo publish --dry-run -p velra-core` succeeds
- [ ] `npm pack --dry-run` in `npm/velra` lists only `bin/velra.js`, `README.md`, `package.json`
- [ ] [`docs/E2E_CHECKLIST.md`](E2E_CHECKLIST.md) completed on a real Claude Code install, including the restore steps

Publish (maintainer):

- [ ] GitHub release `v0.1.2` with all six archives, their `.sha256` files and `sha256.sum`
- [ ] `cargo publish -p velra-core`, then `cargo publish -p velra`
- [ ] `npm publish` from `npm/velra`
- [ ] (optional) Homebrew tap formula filled from `sha256.sum` and published

Validate after publishing:

- [ ] Windows: `irm …/install/install.ps1 | iex` in a clean profile → `velra --version` shows 0.1.2; `velra restore --help` works
- [ ] macOS/Linux: `install.sh` the same
- [ ] `npx velra@0.1.2 --version` downloads, verifies and runs 0.1.2
- [ ] `cargo binstall velra@0.1.2` fetches the archive (no compile)
- [ ] Update the "Release status" notes in `README.md` and `docs/INSTALL.md`, and date the `[0.1.2]` heading in `CHANGELOG.md`
