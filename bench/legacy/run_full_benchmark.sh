#!/usr/bin/env bash
# Run the whole Velra benchmark suite.
#
# A thin wrapper around bench/run_full_benchmark.py; every argument is passed
# straight through:
#
#     bash bench/run_full_benchmark.sh --replicates 1
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PY="$(command -v python3 || command -v python || true)"
[ -n "$PY" ] || { echo "Python 3.11+ is required and was not found on PATH." >&2; exit 1; }

# The Windows-under-Git-Bash case needs the portable MinGW toolchain on PATH
# for the release build (bundled SQLite wants dlltool.exe).
WINLIBS="$HOME/AppData/Local/Programs/winlibs-mingw64/mingw64/bin"
[ -d "$WINLIBS" ] && export PATH="$WINLIBS:$PATH"
[ -d "$HOME/.cargo/bin" ] && export PATH="$HOME/.cargo/bin:$PATH"

exec "$PY" "$HERE/run_full_benchmark.py" "$@"
