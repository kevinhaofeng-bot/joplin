#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
sampler="$script_dir/measure-memory.sh"
fixture_dir="$script_dir/test-fixtures"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/task7-sampler-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_json() {
  python3 - "$@" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
expected = sys.argv[2]
data = json.loads(path.read_text(encoding="utf-8"))
if expected == "success":
    assert data["pass"] is True, data
    assert len(data["rss_kib"]) == 6, data
    assert data["internal"]["texture_bytes"] == 0, data
elif expected == "internal-failure":
    assert data["pass"] is False, data
    assert data["gates"]["texture_bytes"] is False, data
elif expected == "rss-failure":
    assert data["pass"] is False, data
else:
    raise AssertionError(expected)
PY
}

run_sampler() {
  local mode=$1
  local binary=$2
  local output=$3
  local fixture=$4
  local marker=$5
  local status=0
  local path="$fixture_dir:$PATH"
  TASK7_FAKE_MODE="$mode" \
  TASK7_FAKE_MARKER="$marker" \
  TASK7_PS_COUNTER="$test_root/ps-count" \
  TASK7_FAKE_RSS_KIB="${TASK7_FAKE_RSS_KIB:-1000}" \
  PATH="$path" \
    "$sampler" "$binary" "$fixture" "$output" >/dev/null 2>"$output.stderr" || status=$?
  return "$status"
}

fake="$fixture_dir/fake-task7-binary.sh"
fake_alt="$fixture_dir/fake-task7-binary-alt.sh"
[[ -x "$fake" && -x "$fake_alt" ]] || fail "test fixtures are missing"

# A successful run must publish a nonempty identity-bound marker, collect exactly
# six samples, and clean its application PID on exit. This case intentionally
# has no spawned child, so child_processes=0 remains a meaningful gate.
success_dir="$test_root/success"
mkdir -p "$success_dir"
run_sampler ok "$fake" "$success_dir" empty "$success_dir/process" \
  || fail "successful sampler run failed"
assert_json "$success_dir/task-7-empty.json" success
[[ "$(<"$test_root/ps-count")" == 6 ]] || fail "sampler did not collect exactly six RSS samples"
[[ "$(<"$success_dir/process.status")" == terminated ]] || fail "application PID was not terminated"

# A real spawned child must be visible through the process-count gate. The
# sampler must reject the run, then clean both parent and grandchild PIDs.
spawn_dir="$test_root/spawn-child"
mkdir -p "$spawn_dir"
if run_sampler spawn-child "$fake" "$spawn_dir" empty "$spawn_dir/process"; then
  fail "spawn-child process gate unexpectedly passed"
fi
python3 - "$spawn_dir/task-7-empty.json" <<'PY'
import json
import pathlib
import sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert data["pass"] is False, data
assert data["child_processes"] > 0, data
PY
[[ "$(<"$spawn_dir/process.status")" == terminated ]] || fail "spawn-child leaked application PID"
child_pid=$(<"$spawn_dir/process.child")
if kill -0 "$child_pid" 2>/dev/null; then
  fail "spawned child PID was not cleaned up: $child_pid"
fi

# Empty and malformed ready markers must fail before RSS sampling and still
# clean the spawned process.
for mode in ready-empty ready-bad; do
  bad_dir="$test_root/$mode"
  mkdir -p "$bad_dir"
  if run_sampler "$mode" "$fake" "$bad_dir" empty "$bad_dir/process"; then
    fail "$mode unexpectedly passed"
  fi
  [[ "$(<"$bad_dir/process.status")" == terminated ]] || fail "$mode leaked application PID"
  [[ ! -e "$bad_dir/task-7-empty.json" ]] || fail "$mode wrote a result after readiness failure"
done

# Internal diagnostics and RSS gates must produce an explicit failed result,
# not a false successful measurement.
internal_dir="$test_root/internal"
mkdir -p "$internal_dir"
if run_sampler internal-fail "$fake" "$internal_dir" empty "$internal_dir/process"; then
  fail "internal gate failure unexpectedly passed"
fi
assert_json "$internal_dir/task-7-empty.json" internal-failure

rss_dir="$test_root/rss"
mkdir -p "$rss_dir"
TASK7_FAKE_RSS_KIB=90000 run_sampler ok "$fake" "$rss_dir" empty "$rss_dir/process" \
  && fail "RSS gate failure unexpectedly passed"
assert_json "$rss_dir/task-7-empty.json" rss-failure

# Reusing a run-set manifest with a different binary must be rejected before
# launching the fake application.
stale_dir="$test_root/stale"
mkdir -p "$stale_dir"
run_sampler ok "$fake" "$stale_dir" empty "$stale_dir/first" \
  || fail "failed to create stale-manifest baseline"
if run_sampler ok "$fake_alt" "$stale_dir" typical "$stale_dir/second"; then
  fail "stale binary manifest unexpectedly passed"
fi
grep -q "binary digest differs" "$stale_dir.stderr" \
  || fail "stale manifest rejection did not identify the digest mismatch"
[[ ! -e "$stale_dir/second.status" ]] || fail "stale manifest launched the application"

# A long run must reject an empty result whose run identity was rewritten.
long_dir="$test_root/long-mismatch"
mkdir -p "$long_dir"
run_sampler ok "$fake" "$long_dir" empty "$long_dir/empty" \
  || fail "failed to create long-run baseline"
python3 - "$long_dir/task-7-empty.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
data = json.loads(path.read_text(encoding="utf-8"))
data["run_id"] = "stale-run"
path.write_text(json.dumps(data) + "\n", encoding="utf-8")
PY
if run_sampler ok "$fake" "$long_dir" long "$long_dir/long"; then
  fail "long run with mismatched empty identity unexpectedly passed"
fi
assert_json "$long_dir/task-7-long.json" rss-failure
grep -q "does not match" "$long_dir.stderr" \
  || fail "long mismatch rejection did not identify the identity mismatch"

echo "Task 7 sampler integration tests: PASS"
