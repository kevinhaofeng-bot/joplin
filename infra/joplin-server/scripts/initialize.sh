#!/usr/bin/env bash

set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
env_file=${JOPLIN_ENV_FILE:-$root_dir/.env}
compose_file="$root_dir/compose.yaml"
bootstrap_compose_file="$root_dir/compose.bootstrap.yaml"
verify_script="$root_dir/scripts/verify-config.sh"
bootstrap_script="$root_dir/scripts/bootstrap-admin.py"
readiness_attempts=${JOPLIN_READINESS_ATTEMPTS:-60}

fail() {
  printf 'Joplin Server initialization failed: %s\n' "$*" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail 'must run as root'

compose=(docker compose --env-file "$env_file" -f "$compose_file")
bootstrap_compose=(docker compose --env-file "$env_file" -f "$compose_file" -f "$bootstrap_compose_file")
cleanup_required=0

cleanup() {
  if [ "$cleanup_required" -eq 1 ]; then
    "${bootstrap_compose[@]}" stop app db >/dev/null 2>&1 || true
  fi
}

wait_healthy() {
  local container=$1
  local attempt=0
  local status=

  while [ "$attempt" -lt "$readiness_attempts" ]; do
    status=$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$container" 2>/dev/null || true)
    [ "$status" = healthy ] && return 0
    [ "$status" = exited ] && return 1
    attempt=$((attempt + 1))
    sleep 2
  done
  return 1
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
DEPLOY_ENV_FILE="$env_file" "$verify_script"

cleanup_required=1
"${bootstrap_compose[@]}" up -d db app
wait_healthy joplin-server-db-1 || fail 'bootstrap PostgreSQL did not become healthy'
wait_healthy joplin-server-app-1 || fail 'loopback bootstrap app did not become healthy'
python3 "$bootstrap_script" --env-file "$env_file" --base-url http://127.0.0.1:22300

"${bootstrap_compose[@]}" stop app
"${bootstrap_compose[@]}" rm -f app
"${compose[@]}" up -d db app
wait_healthy joplin-server-db-1 || fail 'production PostgreSQL did not become healthy'
wait_healthy joplin-server-app-1 || fail 'production Joplin app did not become healthy'
cleanup_required=0

printf 'Joplin Server isolated initialization: PASS\n'
