#!/usr/bin/env bash
set -euo pipefail

ready_file=""
diagnostics_file=""
for ((index = 1; index <= $#; index++)); do
  case "${!index}" in
    --ready-file)
      index=$((index + 1))
      ready_file=${!index}
      ;;
    --diagnostics-file)
      index=$((index + 1))
      diagnostics_file=${!index}
      ;;
  esac
done
[[ -n "$ready_file" && -n "$diagnostics_file" ]]

marker=${TASK7_FAKE_MARKER:?}
mode=${TASK7_FAKE_MODE:-ok}
printf '%s\n' "$$" > "$marker.parent"
child_pid=""

cleanup() {
  set +e
  if [[ -n "$child_pid" ]]; then
    kill -KILL "$child_pid" 2>/dev/null || true
    wait "$child_pid" 2>/dev/null || true
  fi
  printf 'terminated\n' > "$marker.status"
  exit 0
}
trap cleanup TERM INT
trap '[[ -e "$marker.status" ]] || printf "terminated\n" > "$marker.status"' EXIT

case "$mode" in
  internal-fail)
    texture_bytes=50331649
    ;;
  *)
    texture_bytes=0
    ;;
esac
printf '{"texture_bytes":%s,"layout_cache_bytes":0,"undo_bytes":0,"transaction_p95_us":1,"render_commit_p95_us":1}\n' \
  "$texture_bytes" > "$diagnostics_file"

if [[ "$mode" == spawn-child ]]; then
  (
    trap 'exit 0' TERM INT
    while :; do /bin/sleep 1; done
  ) &
  child_pid=$!
  printf '%s\n' "$child_pid" > "$marker.child"
fi

case "$mode" in
  ready-empty)
    : > "$ready_file"
    ;;
  ready-bad)
    printf 'not-task7-ready\n' > "$ready_file"
    ;;
  *)
    printf 'task7-ready|%s|%s\n' "${TASK7_RUN_ID:?}" "${TASK7_BINARY_SHA256:?}" > "$ready_file"
    ;;
esac

while :; do /bin/sleep 1; done
