#!/usr/bin/env bash
# Real isolated service -> online backup -> restored service -> fresh client.
# Arguments: app-lite-server, sync_drill, multimodal_probe, PNG, PDF.
set -euo pipefail
server=${1:?server}; drill=${2:?sync_drill}; seed=${3:?multimodal_probe}
png=${4:?png fixture}; pdf=${5:?pdf fixture}
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(mktemp -d "${TMPDIR:-/tmp}/joplin-populated-restore.XXXXXX")
echo "test evidence: $root"
token=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
env -u APP_LITE_TLS_CERT -u APP_LITE_TLS_KEY APP_LITE_SERVER_TOKEN="$token" \
  "$server" --root "$root/server" --listen 127.0.0.1:0 > "$root/server.stdout" 2> "$root/server.log" &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
address=
for _ in $(seq 100); do
  address=$(sed -n 's/^app-lite-server listening on \([^ ]*\).*/\1/p' "$root/server.log")
  [ -n "$address" ] && break
  kill -0 "$pid" || { cat "$root/server.log"; exit 1; }
  sleep 0.1
done
[ -n "$address" ] || { echo 'server startup failed' >&2; exit 1; }
"$seed" seed "$root/source" "$png" "$pdf"
APP_LITE_SERVER_TOKEN="$token" "$drill" "$root/source" "http://$address" > "$root/source-sync.log"
grep -qx 'sync.pending 0' "$root/source-sync.log"
grep -qx 'resources.rehashed 5 mismatched 0' "$root/source-sync.log"
bash "$script_dir/backup.sh" "$root/server" "$root/backup"
bash "$script_dir/restore-drill.sh" "$root/backup" "$root/restored" "$server" "$drill" > "$root/restored-sync.log"
grep -qx 'sync.pending 0' "$root/restored-sync.log"
grep -qx 'resources.rehashed 5 mismatched 0' "$root/restored-sync.log"
for profile in source restored/client; do
  db="$root/$profile/library.sqlite"
  # Both probes have exited. Never use immutable mode on a live/WAL database.
  [ ! -e "$db-wal" ] || { echo 'uncheckpointed client database' >&2; exit 1; }
  uri="file:$db?mode=ro&immutable=1"
  [ "$(sqlite3 "$uri" 'SELECT count(*) FROM notes')" = 5 ]
  [ "$(sqlite3 "$uri" 'SELECT count(*) FROM note_resources')" = 5 ]
  [ "$(sqlite3 "$uri" 'PRAGMA integrity_check')" = ok ]
done
# Canonical note content, attachment metadata, ordering and associations must
# match; local revisions and transport bookkeeping intentionally need not.
snapshot() {
  sqlite3 "file:$1?mode=ro&immutable=1" 'SELECT id,hex(title),hex(body_html),notebook_id,coalesce(selected_thumbnail_id, ""),deleted_time FROM notes ORDER BY id;
    SELECT note_id,position,resource_id,is_associated FROM note_resources ORDER BY note_id,position;
    SELECT id,sha256,hex(title),mime,file_extension,size FROM resources ORDER BY id;'
}
snapshot "$root/source/library.sqlite" > "$root/source.rows"
snapshot "$root/restored/client/library.sqlite" > "$root/restored.rows"
cmp "$root/source.rows" "$root/restored.rows"
echo 'PASS: 5 notes, 5 associated resources, canonical content and attachment hashes restored'
