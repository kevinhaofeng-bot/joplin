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
# Validate the entire backup before creating anything that resembles a restored
# service. Treat filenames from the manifest as untrusted, never as paths.
fail() { echo "invalid backup: $*" >&2; exit 1; }
for file in sync.sqlite blobs.txt manifest.txt; do
  [ -f "$backup/$file" ] && [ ! -L "$backup/$file" ] || fail "missing or symlinked $file"
done
[ -d "$backup/blobs" ] && [ ! -L "$backup/blobs" ] || fail 'invalid blobs directory'
awk 'NF != 2 || $2 !~ /^[0-9]+$/ { exit 1 }
     $1 != "blobs" && $1 != "changes" && $1 != "entities" { exit 1 }
     seen[$1]++ { exit 1 }
     END { if (NR != 3) exit 1 }' "$backup/manifest.txt" || fail 'invalid counts manifest'
[ "$(sqlite3 -readonly "$backup/sync.sqlite" 'PRAGMA integrity_check')" = ok ] || fail 'database integrity'
for table in changes entities; do
  expected=$(awk -v key="$table" '$1 == key { print $2 }' "$backup/manifest.txt")
  actual=$(sqlite3 -readonly "$backup/sync.sqlite" "SELECT count(*) FROM $table")
  [ "$actual" = "$expected" ] || fail "$table count mismatch"
done
count=0
while IFS= read -r name || [ -n "$name" ]; do
  [[ "$name" =~ ^[0-9a-f]{64}$ ]] || fail 'invalid blob name'
  [ -f "$backup/blobs/$name" ] && [ ! -L "$backup/blobs/$name" ] || fail "missing or symlinked blob $name"
  [ "$(shasum -a 256 "$backup/blobs/$name" | cut -d' ' -f1)" = "$name" ] || fail "blob hash mismatch $name"
  count=$((count + 1))
done < "$backup/blobs.txt"
[ -z "$(sort "$backup/blobs.txt" | uniq -d)" ] || fail 'duplicate blob name'
expected=$(awk '$1 == "blobs" { print $2 }' "$backup/manifest.txt")
[ "$count" = "$expected" ] || fail 'blob count mismatch'
mkdir -p "$scratch/data/blobs" "$scratch/data/uploads"
cp "$backup/sync.sqlite" "$scratch/data/sync.sqlite"
while IFS= read -r name || [ -n "$name" ]; do
  cp "$backup/blobs/$name" "$scratch/data/blobs/$name"
  [ "$(shasum -a 256 "$scratch/data/blobs/$name" | cut -d' ' -f1)" = "$name" ] || { echo "restored blob mismatch: $name" >&2; exit 1; }
done < "$backup/blobs.txt"
token=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
[[ "$token" =~ ^[0-9a-f]{64}$ ]] || { echo 'could not generate restore token' >&2; exit 1; }
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
