#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 /absolute/path/to/velotype empty|typical|long [output-dir]" >&2
  exit 2
fi
binary=$1
fixture=$2
output_dir=${3:-"$(pwd)/task-7-evidence"}
case "$fixture" in
  empty|typical|long) ;;
  *) echo "fixture must be empty, typical, or long" >&2; exit 2 ;;
esac
[[ "$binary" = /* ]] || binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")
[[ -x "$binary" ]] || { echo "binary is not executable: $binary" >&2; exit 2; }
mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd)
run_dir=$(mktemp -d "${TMPDIR:-/tmp}/velotype-task7.XXXXXX")
ready_file="$run_dir/ready"
diagnostics_file="$run_dir/diagnostics.json"
retained_diagnostics_file="$output_dir/task-7-${fixture}.diagnostics.json"
log_file="$output_dir/task-7-${fixture}.log"
rss_file="$output_dir/task-7-${fixture}.rss"
vmmap_file="$output_dir/task-7-${fixture}.vmmap.txt"
result_file="$output_dir/task-7-${fixture}.json"
pid=""

cleanup() {
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$run_dir"
}
trap cleanup EXIT

"$binary" --evernote-spike --fixture "$fixture" \
  --ready-file "$ready_file" --diagnostics-file "$diagnostics_file" \
  >"$log_file" 2>&1 &
pid=$!

for _ in $(seq 1 60); do
  [[ -f "$ready_file" && -s "$diagnostics_file" ]] && break
  sleep 0.5
done
if [[ ! -f "$ready_file" || ! -s "$diagnostics_file" ]]; then
  echo "Task 7 did not reach first-frame/cache-settle readiness; see $log_file" >&2
  exit 1
fi

: > "$rss_file"
for _ in $(seq 1 6); do
  sleep 5
  ps -o rss= -p "$pid" | tr -d ' ' >> "$rss_file"
done
child_processes=$(ps -axo ppid= | awk -v pid="$pid" '$1 == pid {n++} END {print n+0}')
if command -v vmmap >/dev/null 2>&1; then
  vmmap -summary "$pid" > "$vmmap_file"
else
  : > "$vmmap_file"
fi
webkit_linked=0
if otool -L "$binary" 2>/dev/null | grep -q 'WebKit'; then
  webkit_linked=1
fi
cp "$diagnostics_file" "$retained_diagnostics_file"

python3 - "$retained_diagnostics_file" "$rss_file" "$result_file" "$fixture" "$pid" "$child_processes" "$webkit_linked" "$vmmap_file" <<'PY'
import json
import pathlib
import sys

diagnostics_path, rss_path, result_path, fixture, pid, child_processes, webkit, vmmap_file = sys.argv[1:]
with open(diagnostics_path, encoding="utf-8") as handle:
    diagnostics = json.load(handle)
required = {
    "texture_bytes", "layout_cache_bytes", "undo_bytes",
    "transaction_p95_us", "render_commit_p95_us",
}
if set(diagnostics) != required or not all(isinstance(diagnostics[key], int) for key in required):
    raise SystemExit("diagnostics JSON does not match the five-number contract")
gates = {
    "texture_bytes": diagnostics["texture_bytes"] <= 48 * 1024 * 1024,
    "layout_cache_bytes": diagnostics["layout_cache_bytes"] <= 16 * 1024 * 1024,
    "undo_bytes": diagnostics["undo_bytes"] <= 16 * 1024 * 1024,
    "transaction_p95_us": diagnostics["transaction_p95_us"] <= 8_000,
    "render_commit_p95_us": diagnostics["render_commit_p95_us"] <= 16_000,
}
rss = [int(line) for line in pathlib.Path(rss_path).read_text().split() if line]
if len(rss) != 6:
    raise SystemExit("expected six RSS samples")
result = {
    "fixture": fixture,
    "pid": int(pid),
    "child_processes": int(child_processes),
    "webkit_linked": int(webkit),
    "rss_kib": rss,
    "rss_stable_kib": rss[-1],
    "rss_peak_kib": max(rss),
    "internal": diagnostics,
    "gates": gates,
    "diagnostics_file": diagnostics_path,
    "vmmap_file": vmmap_file,
}
pathlib.Path(result_path).write_text(json.dumps(result, sort_keys=True) + "\n", encoding="utf-8")
stable = rss[-1]
if fixture == "empty" and stable > 81_920:
    raise SystemExit("empty RSS exceeds 81920 KiB")
if fixture == "typical" and stable > 122_880:
    raise SystemExit("typical RSS exceeds 122880 KiB")
if fixture == "long":
    empty_path = pathlib.Path(result_path).with_name("task-7-empty.json")
    if not empty_path.is_file():
        raise SystemExit("run the empty fixture first so long RSS delta is measurable")
    empty = json.loads(empty_path.read_text())
    if stable - int(empty["rss_stable_kib"]) > 40_960:
        raise SystemExit("long RSS delta exceeds 40960 KiB")
if not all(gates.values()) or int(child_processes) != 0 or int(webkit) != 0:
    raise SystemExit("Task 7 fixed-capacity or isolation gate failed")
PY

echo "$result_file"
