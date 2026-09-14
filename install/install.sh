#!/bin/sh
# Velra installer for macOS and Linux.
#
#   curl -LsSf https://github.com/aryanghai12/Velra/install.sh | sh
#   curl -LsSf https://github.com/aryanghai12/Velra/install.sh | sh -s -- --enable
#
# Environment:
#   VELRA_VERSION          version to install (default: latest release)
#   VELRA_HOME             install root (default: $HOME/.velra)
#   VELRA_NO_MODIFY_PATH=1 do not touch shell rc files
#   VELRA_DOWNLOAD_BASE    archive source: an https:// base URL or a local
#                          directory (used by tests)
#
# No sudo. No admin. Nothing runs unless you pass --enable.

set -eu

REPO="${VELRA_REPO:-aryanghai12/velra}"
RELEASES="${VELRA_DOWNLOAD_BASE:-https://github.com/${REPO}/releases}"
INSTALL_ROOT="${VELRA_HOME:-$HOME/.velra}"
BIN_DIR="$INSTALL_ROOT/bin"
RUN_ENABLE=0

for arg in "$@"; do
  case "$arg" in
    --enable) RUN_ENABLE=1 ;;
    --no-modify-path) VELRA_NO_MODIFY_PATH=1 ;;
    -h|--help)
      sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "velra: unknown option: $arg" >&2; exit 1 ;;
  esac
done

say() { printf '%s\n' "$*"; }
err() { printf '%s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "velra: required command not found: $1"; }

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *) err "velra: unsupported macOS architecture: $arch" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64|amd64) echo "x86_64-unknown-linux-musl" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-musl" ;;
        *) err "velra: unsupported Linux architecture: $arch" ;;
      esac
      ;;
    *) err "velra: unsupported operating system: $os (Windows users: use install.ps1)" ;;
  esac
}

fetch() {
  # fetch <url-or-path> <destination>
  case "$1" in
    http://*|https://*)
      if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
      elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
      else
        err "velra: need curl or wget to download"
      fi
      ;;
    *) cp "$1" "$2" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    err "velra: need sha256sum or shasum to verify the download"
  fi
}

need tar
need mktemp

TARGET="$(detect_target)"
VERSION="${VELRA_VERSION:-latest}"
ARCHIVE="velra-${TARGET}.tar.gz"

if [ "$VERSION" = "latest" ]; then
  BASE="$RELEASES/latest/download"
else
  BASE="$RELEASES/download/v${VERSION#v}"
fi
case "$RELEASES" in
  http://*|https://*) ;;
  *) BASE="$RELEASES" ;;   # local directory (tests)
esac

TMP="$(mktemp -d "${TMPDIR:-/tmp}/velra-install.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM

say "Downloading velra ($TARGET)..."
fetch "$BASE/$ARCHIVE" "$TMP/$ARCHIVE" || err "velra: download failed: $BASE/$ARCHIVE"
fetch "$BASE/$ARCHIVE.sha256" "$TMP/$ARCHIVE.sha256" || err "velra: checksum download failed: $BASE/$ARCHIVE.sha256"

EXPECTED="$(awk '{print $1}' < "$TMP/$ARCHIVE.sha256" | tr -d '\r')"
ACTUAL="$(sha256_of "$TMP/$ARCHIVE")"
if [ -z "$EXPECTED" ] || [ "$EXPECTED" != "$ACTUAL" ]; then
  err "velra: checksum mismatch for $ARCHIVE
  expected: $EXPECTED
  actual:   $ACTUAL
Refusing to install. The download may be corrupt or tampered with."
fi

tar -xzf "$TMP/$ARCHIVE" -C "$TMP" || err "velra: could not extract $ARCHIVE"
BIN="$(find "$TMP" -type f -name velra -perm -u+x 2>/dev/null | head -n 1)"
[ -n "$BIN" ] || BIN="$(find "$TMP" -type f -name velra | head -n 1)"
[ -n "$BIN" ] || err "velra: archive did not contain a velra binary"

mkdir -p "$BIN_DIR"
chmod 700 "$INSTALL_ROOT" 2>/dev/null || true
# Atomic replace: write next to the target, then rename over it.
cp "$BIN" "$BIN_DIR/velra.new"
chmod 755 "$BIN_DIR/velra.new"
mv -f "$BIN_DIR/velra.new" "$BIN_DIR/velra"

VERSION_OUT="$("$BIN_DIR/velra" --version 2>/dev/null || echo "velra")"
INSTALLED_VERSION="$(printf '%s' "$VERSION_OUT" | awk '{print $2}')"

add_path_line() {
  rc="$1"
  line="$2"
  [ -f "$rc" ] || return 0
  if grep -Fq '# added by velra' "$rc" 2>/dev/null; then
    return 0
  fi
  printf '\n%s # added by velra\n' "$line" >> "$rc"
  say "  Added $BIN_DIR to PATH in $rc"
}

if [ "${VELRA_NO_MODIFY_PATH:-0}" != "1" ]; then
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
      shell_name="$(basename "${SHELL:-sh}")"
      case "$shell_name" in
        zsh)  add_path_line "${ZDOTDIR:-$HOME}/.zshrc" "export PATH=\"$BIN_DIR:\$PATH\"" ;;
        bash) add_path_line "$HOME/.bashrc" "export PATH=\"$BIN_DIR:\$PATH\"" ||
              add_path_line "$HOME/.bash_profile" "export PATH=\"$BIN_DIR:\$PATH\"" ;;
        fish) mkdir -p "$HOME/.config/fish"
              add_path_line "$HOME/.config/fish/config.fish" "set -gx PATH $BIN_DIR \$PATH" ;;
        *)    add_path_line "$HOME/.profile" "export PATH=\"$BIN_DIR:\$PATH\"" ;;
      esac
      ;;
  esac
fi

if [ "$RUN_ENABLE" = "1" ]; then
  "$BIN_DIR/velra" enable || err "velra: `velra enable` failed"
fi

say ""
say "✓ velra ${INSTALLED_VERSION} installed to $BIN_DIR/velra"
if [ "$RUN_ENABLE" = "1" ]; then
  say "Next: keep coding — run \`velra inspect\` any time."
else
  say "Next: velra enable"
fi
