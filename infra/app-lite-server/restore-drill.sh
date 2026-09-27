#!/usr/bin/env bash
# Restore drill: restore a backup into a scratch directory, start a second
# server on a free local port with a throwaway token, and let a brand-new
# client pull everything and re-hash every attachment:
#   restore-drill.sh <backup-dir> <scratch-dir> <server-binary> <sync_drill-binary>
# Nothing here touches a running production service.
set -euo pipefail
backup=${1:?backup dir}
scratch=${2:?scratch dir}
server=${3:?app-lite-server binary}
drill=${4:?sync_drill binary}
[ ! -e "$scratch" ] || { echo "scratch exists: $scratch" >&2; exit 1; }
mkdir -p "$scratch/data/blobs" "$scratch/data/uploads"
cp "$backup/sync.sqlite" "$scratch/data/sync.sqlite"
while read -r name; do
  cp "$backup/blobs/$name" "$scratch/data/blobs/$name"
  [ "$(shasum -a 256 "$scratch/data/blobs/$name" | cut -d' ' -f1)" = "$name" ] || { echo "restored blob mismatch: $name" >&2; exit 1; }
done < "$backup/blobs.txt"
token=$(head -c 32 /dev/urandom | xxd -p -c 64)
# The drill always runs plain HTTP on loopback, whatever the caller's shell sets.
env -u APP_LITE_TLS_CERT -u APP_LITE_TLS_KEY APP_LITE_SERVER_TOKEN="$token" \
  "$server" --root "$scratch/data" --listen "127.0.0.1:0" 2> "$scratch/server.log" &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT
address=
for _ in $(seq 100); do
  address=$(sed -n 's/^app-lite-server listening on \([^ ]*\).*/\1/p' "$scratch/server.log")
  [ -n "$address" ] && break
  sleep 0.1
done
[ -n "$address" ] || { echo "server did not start" >&2; cat "$scratch/server.log" >&2; exit 1; }
APP_LITE_SERVER_TOKEN="$token" "$drill" "$scratch/client" "http://$address"
