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
script_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$script_dir/../../.." && pwd)
binary_sha256=$(shasum -a 256 "$binary" | awk '{print $1}')
git_commit=$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)
manifest_file="$output_dir/task-7-run.json"
if [[ -s "$manifest_file" ]]; then
  run_id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["run_id"])' "$manifest_file")
  manifest_sha=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["binary_sha256"])' "$manifest_file")
  manifest_commit=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["git_commit"])' "$manifest_file")
  [[ "$manifest_sha" == "$binary_sha256" ]] || {
    echo "binary digest differs from this run set" >&2
    exit 1
  }
  [[ "$manifest_commit" == "$git_commit" ]] || {
    echo "git commit differs from this run set" >&2
    exit 1
  }
  created_at=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["created_at"])' "$manifest_file")
else
  run_id="task7-$(date -u +%Y%m%dT%H%M%SZ)-$$"
  created_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  python3 - "$manifest_file" "$run_id" "$binary" "$binary_sha256" "$git_commit" "$created_at" <<'PY'
import json
import pathlib
import sys

path, run_id, binary, digest, commit, created_at = sys.argv[1:]
pathlib.Path(path).write_text(json.dumps({
    "run_id": run_id,
    "binary_path": binary,
    "binary_sha256": digest,
    "git_commit": commit,
    "created_at": created_at,
}, sort_keys=True) + "\n", encoding="utf-8")
PY
fi
expected_ready="task7-ready|${run_id}|${binary_sha256}"
# Keep the release sampler contract explicit: readiness is bounded by 30 s,
# followed by exactly six five-second RSS samples. Tests replace `sleep` via
# PATH; these production constants must not be silently overridden by env.
readonly ready_poll_attempts=60
readonly ready_poll_seconds=0.5
readonly sample_interval_seconds=5
run_dir=$(mktemp -d "${TMPDIR:-/tmp}/velotype-task7.XXXXXX")
ready_file="$run_dir/ready"
diagnostics_file="$run_dir/diagnostics.json"
retained_diagnostics_file="$output_dir/task-7-${fixture}.diagnostics.json"
log_file="$output_dir/task-7-${fixture}.log"
rss_file="$output_dir/task-7-${fixture}.rss"
vmmap_file="$output_dir/task-7-${fixture}.vmmap.txt"
footprint_file="$output_dir/task-7-${fixture}.footprint.json"
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

TASK7_RUN_ID="$run_id" TASK7_BINARY_SHA256="$binary_sha256" "$binary" --evernote-spike --fixture "$fixture" \
  --ready-file "$ready_file" --diagnostics-file "$diagnostics_file" \
  >"$log_file" 2>&1 &
pid=$!

ready_value=""
for _ in $(seq 1 "$ready_poll_attempts"); do
  if [[ -s "$ready_file" ]]; then
    ready_value=$(tr -d '\n' < "$ready_file")
    [[ "$ready_value" == "$expected_ready" && -s "$diagnostics_file" ]] && break
  fi
  sleep "$ready_poll_seconds"
done
if [[ "$ready_value" != "$expected_ready" || ! -s "$diagnostics_file" ]]; then
  echo "Task 7 did not reach first-frame/cache-settle readiness; see $log_file" >&2
  exit 1
fi

: > "$rss_file"
for _ in $(seq 1 6); do
  sleep "$sample_interval_seconds"
  ps -o rss= -p "$pid" | tr -d ' ' >> "$rss_file"
done

# RSS remains a legacy observation. macOS footprint is the physical-memory
# gate; retain its raw JSON for exact PID/provenance auditing.
footprint_raw_file="$run_dir/footprint.json"
footprint_bin=$(command -v footprint || true)
if [[ -n "$footprint_bin" ]]; then
  "$footprint_bin" -j "$footprint_raw_file" "$pid" >"$run_dir/footprint.stdout" 2>&1 || true
fi
if [[ -f "$footprint_raw_file" ]]; then
  cp "$footprint_raw_file" "$footprint_file"
else
  : > "$footprint_file"
fi

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

python3 - "$retained_diagnostics_file" "$rss_file" "$footprint_file" "$result_file" "$fixture" "$pid" "$child_processes" "$webkit_linked" "$vmmap_file" "$binary" "$binary_sha256" "$git_commit" "$run_id" "$created_at" "$expected_ready" <<'PY'
import json
import pathlib
import sys

(
    diagnostics_path, rss_path, footprint_path, result_path, fixture, pid, child_processes,
    webkit, vmmap_file, binary, binary_sha256, git_commit, run_id, created_at,
    expected_ready,
) = sys.argv[1:]
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

def parse_footprint(path, expected_pid):
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return None, None, f"footprint JSON is missing or malformed: {error}"
    if data.get("unit") != "byte" or data.get("bytes per unit") != 1:
        return None, None, "footprint JSON must report byte units"
    processes = data.get("processes")
    if not isinstance(processes, list):
        return None, None, "footprint JSON has no processes array"
    matches = [process for process in processes if process.get("pid") == expected_pid]
    if len(matches) != 1:
        return None, None, f"footprint JSON does not contain exactly one record for pid {expected_pid}"
    process = matches[0]
    current = process.get("footprint")
    auxiliary = process.get("auxiliary")
    peak = auxiliary.get("phys_footprint_peak") if isinstance(auxiliary, dict) else None
    if not isinstance(current, int) or isinstance(current, bool) or current < 0:
        return None, None, "footprint process footprint is not a non-negative integer"
    if not isinstance(peak, int) or isinstance(peak, bool) or peak < current:
        return None, None, "footprint phys_footprint_peak is invalid"
    return current, peak, None

physical_footprint, physical_footprint_peak, physical_footprint_error = parse_footprint(
    pathlib.Path(footprint_path), int(pid)
)
result = {
    "fixture": fixture,
    "pid": int(pid),
    "binary_path": binary,
    "binary_sha256": binary_sha256,
    "git_commit": git_commit,
    "run_id": run_id,
    "created_at": created_at,
    "ready_marker": expected_ready,
    "child_processes": int(child_processes),
    "webkit_linked": int(webkit),
    "rss_kib": rss,
    "rss_stable_kib": rss[-1],
    "rss_peak_kib": max(rss),
    "physical_footprint_bytes": physical_footprint,
    "physical_footprint_peak_bytes": physical_footprint_peak,
    "physical_footprint_error": physical_footprint_error,
    "internal": diagnostics,
    "gates": gates,
    "legacy_rss_gates": {},
    "pass": False,
    "diagnostics_file": diagnostics_path,
    "footprint_file": footprint_path,
    "vmmap_file": vmmap_file,
}
stable = rss[-1]
legacy_rss_gates = {}
if fixture == "empty":
    legacy_rss_gates["stable"] = stable <= 81_920
    physical_limit = 80 * 1024 * 1024
elif fixture == "typical":
    legacy_rss_gates["stable"] = stable <= 122_880
    physical_limit = 120 * 1024 * 1024
else:
    physical_limit = 40 * 1024 * 1024
if fixture != "long":
    gates["physical_footprint"] = (
        physical_footprint is not None and physical_footprint <= physical_limit
    )
provenance_error = None
if fixture == "long":
    empty_path = pathlib.Path(result_path).with_name("task-7-empty.json")
    if not empty_path.is_file():
        provenance_error = "run the empty fixture first so long physical delta is measurable"
        physical_delta = None
    else:
        empty = json.loads(empty_path.read_text())
        identity = ("binary_path", "binary_sha256", "git_commit", "run_id")
        if empty.get("fixture") != "empty" or not empty.get("pass"):
            provenance_error = "sibling empty result is not a successful run-set result"
        elif any(empty.get(key) != result[key] for key in identity):
            provenance_error = "sibling empty result does not match this binary/run set"
        else:
            empty_physical = empty.get("physical_footprint_bytes")
            if not isinstance(empty_physical, int):
                provenance_error = "sibling empty result has no valid physical footprint"
                physical_delta = None
            else:
                physical_delta = (
                    None
                    if physical_footprint is None
                    else physical_footprint - empty_physical
                )
                gates["physical_footprint"] = (
                    physical_delta is not None and physical_delta <= physical_limit
                )
                legacy_rss_gates["stable_delta"] = stable - int(empty["rss_stable_kib"]) <= 40_960
                legacy_rss_gates["physical_delta_bytes"] = physical_delta
    gates["provenance"] = provenance_error is None
else:
    gates["provenance"] = True
result["legacy_rss_gates"] = legacy_rss_gates
result["gates"] = gates
result["provenance_error"] = provenance_error
result["pass"] = bool(
    all(gates.values()) and int(child_processes) == 0 and int(webkit) == 0
)
pathlib.Path(result_path).write_text(json.dumps(result, sort_keys=True) + "\n", encoding="utf-8")
if physical_footprint_error:
    raise SystemExit(physical_footprint_error)
if provenance_error:
    raise SystemExit(provenance_error)
all_pass = all(gates.values()) and int(child_processes) == 0 and int(webkit) == 0
result["pass"] = bool(all_pass)
pathlib.Path(result_path).write_text(json.dumps(result, sort_keys=True) + "\n", encoding="utf-8")
if not all_pass:
    raise SystemExit("Task 7 fixed-capacity, physical-footprint, provenance, or isolation gate failed")
PY

echo "$result_file"
