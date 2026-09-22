#!/usr/bin/env bash
# H1: enforces the §4 performance budgets with hyperfine.
#
#   cargo build --release -p velra && bash bench/run.sh
#
# Measures full process wall time (spawn → exit) against a database
# pre-populated with 100,000 events, and fails when a p50/p99 budget is missed.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/velra"
[ -f "$BIN" ] || BIN="$ROOT/target/release/velra.exe"
[ -f "$BIN" ] || { echo "build the release binary first: cargo build --release -p velra"; exit 1; }

PY_BIN="$(command -v python3 || command -v python || true)"
[ -n "$PY_BIN" ] || { echo "python3 is required to build the payloads and read the results"; exit 1; }
if command -v hyperfine >/dev/null 2>&1; then
  RUNNER=hyperfine
else
  RUNNER=builtin
  echo "note: hyperfine not found; using the built-in timer. Its own spawn cost"
  echo "      (a few ms) is included, so treat the numbers as indicative and do"
  echo "      not gate on them. For CI-grade numbers: cargo install hyperfine."
fi

RESULTS="$ROOT/bench/results"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/velra-bench.XXXXXX")"
export VELRA_HOME="$WORK/home"
export CLAUDE_PROJECT_DIR="$WORK/project"
export TZ=UTC
mkdir -p "$RESULTS" "$VELRA_HOME" "$CLAUDE_PROJECT_DIR"
trap 'rm -rf "$WORK"' EXIT

WARMUP="${VELRA_BENCH_WARMUP:-20}"
RUNS="${VELRA_BENCH_RUNS:-500}"
SEED_EVENTS="${VELRA_BENCH_EVENTS:-100000}"

echo "Seeding $SEED_EVENTS events into $VELRA_HOME ..."
cargo run --release --quiet -p velra --example seed -- "$VELRA_HOME" "$SEED_EVENTS" "$CLAUDE_PROJECT_DIR"

# ---------------------------------------------------------------- payloads
"$PY_BIN" - "$WORK" "$CLAUDE_PROJECT_DIR" <<'PY'
import json, os, sys
work, project = sys.argv[1], sys.argv[2]
os.makedirs(os.path.join(work, "payloads"), exist_ok=True)

def write(name, obj):
    with open(os.path.join(work, "payloads", name), "w") as f:
        json.dump(obj, f)

common = {"session_id": "bench-session", "cwd": project, "transcript_path": "/dev/null"}

# 2 KiB payload, non-edit tool
write("post_tool_small.json", {**common, "hook_event_name": "PostToolUse", "tool_name": "Read",
      "tool_use_id": "toolu_bench_1", "tool_input": {"file_path": os.path.join(project, "src", "main.rs")},
      "tool_response": {"content": "x" * 2000}})

# Edit of a 50 KiB file
big = os.path.join(project, "src", "big.rs")
os.makedirs(os.path.dirname(big), exist_ok=True)
with open(big, "w") as f:
    f.write("fn main() {}\n" * 4000)
write("post_tool_edit.json", {**common, "hook_event_name": "PostToolUse", "tool_name": "Edit",
      "tool_use_id": "toolu_bench_2", "tool_input": {"file_path": big, "old_string": "fn main() {}", "new_string": "fn main() { run(); }"},
      "tool_response": {"filePath": big, "originalFile": "fn main() {}\n" * 4000}})
write("pre_tool_edit.json", {**common, "hook_event_name": "PreToolUse", "tool_name": "Edit",
      "tool_use_id": "toolu_bench_3", "tool_input": {"file_path": big, "old_string": "a", "new_string": "b"}})

write("user_prompt.json", {**common, "hook_event_name": "UserPromptSubmit", "prompt_id": "p-bench",
      "prompt": "why is the login test still failing after the retry change?"})
write("pre_compact.json", {**common, "hook_event_name": "PreCompact", "trigger": "manual", "custom_instructions": None})

# 8 MiB tool_response
write("post_tool_huge.json", {**common, "hook_event_name": "PostToolUse", "tool_name": "Bash",
      "tool_use_id": "toolu_bench_4", "tool_input": {"command": "cargo test"},
      "tool_response": {"stdout": "y" * (8 * 1024 * 1024), "stderr": "", "interrupted": False}})
PY

P="$WORK/payloads"

# ------------------------------------------------------------------ budgets
# name | argv | payload | p50 ms | p99 ms
BUDGETS=$(cat <<'EOF'
post-tool-use-small|hook post-tool-use|post_tool_small.json|2|5
post-tool-use-edit|hook post-tool-use|post_tool_edit.json|3|6
pre-tool-use-edit|hook pre-tool-use|pre_tool_edit.json|3|6
user-prompt-submit|hook user-prompt-submit|user_prompt.json|2|4
pre-compact|hook pre-compact|pre_compact.json|6|10
post-tool-use-8mib|hook post-tool-use|post_tool_huge.json|25|25
EOF
)

# Windows pays a higher process-spawn cost; §4 allows p99 ≤ 15 ms there.
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) WINDOWS=1 ;;
  *) WINDOWS=0 ;;
esac

# Runs the binary $RUNS times and writes hyperfine's JSON shape, so the
# percentile check below is identical either way.
measure_builtin() {
  "$PY_BIN" - "$BIN" "$1" "$2" "$WARMUP" "$RUNS" "$3" <<'MEASURE'
import json, subprocess, sys, time
binary, argv, payload = sys.argv[1], sys.argv[2].split(), sys.argv[3]
warmup, runs, out = int(sys.argv[4]), int(sys.argv[5]), sys.argv[6]
data = open(payload, "rb").read()
times = []
for i in range(warmup + runs):
    start = time.perf_counter()
    subprocess.run([binary, *argv], input=data,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    elapsed = time.perf_counter() - start
    if i >= warmup:
        times.append(elapsed)
json.dump({"results": [{"times": times}]}, open(out, "w"))
MEASURE
}

failures="$WORK/failures"
: > "$failures"
while IFS='|' read -r name argv payload p50 p99; do
  [ -n "$name" ] || continue
  json="$RESULTS/$name.json"
  echo
  echo "== $name (budget p50 ${p50}ms, p99 ${p99}ms)"
  if [ "$RUNNER" = hyperfine ]; then
    hyperfine --warmup "$WARMUP" --runs "$RUNS" --shell=none \
      --input "$P/$payload" --export-json "$json" \
      "$BIN $argv" >/dev/null
  else
    measure_builtin "$argv" "$P/$payload" "$json"
  fi

  "$PY_BIN" - "$json" "$name" "$p50" "$p99" "$WINDOWS" <<'PY' || echo "$name" >> "$failures"
import json, sys
path, name, p50_budget, p99_budget, windows = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4]), sys.argv[5] == "1"
times = sorted(t * 1000 for t in json.load(open(path))["results"][0]["times"])
def pct(p):
    return times[min(len(times) - 1, int(round(p / 100 * (len(times) - 1))))]
p50, p99 = pct(50), pct(99)
if windows:
    p99_budget = max(p99_budget, 15.0)
    p50_budget = max(p50_budget, 15.0)
status = "OK"
code = 0
if p50 > p50_budget or p99 > p99_budget:
    status = "OVER BUDGET"
    code = 1
print(f"   p50 {p50:.2f} ms (budget {p50_budget}) | p99 {p99:.2f} ms (budget {p99_budget}) -> {status}")
if code:
    print(f"::error::{name} exceeded its budget")
sys.exit(code)
PY
done <<BUDGET_ROWS
$BUDGETS
BUDGET_ROWS

echo
echo "Results written to $RESULTS"
if [ -s "$failures" ]; then
  echo "Over budget:"; cat "$failures"
  if [ "$RUNNER" = hyperfine ] || [ "${VELRA_BENCH_STRICT:-0}" = 1 ]; then
    exit 1
  fi
  echo "(built-in timer: reported, not enforced. Set VELRA_BENCH_STRICT=1 to gate.)"
  exit 0
fi
echo "All budgets met."
