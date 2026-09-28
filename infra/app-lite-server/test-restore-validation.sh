#!/usr/bin/env bash
# Malformed backups must fail before publishing any restored working directory.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(mktemp -d "${TMPDIR:-/tmp}/joplin-restore-validation.XXXXXX")
echo "test evidence: $root"
mkdir "$root/backup" "$root/backup/blobs"
sqlite3 "$root/backup/sync.sqlite" 'CREATE TABLE changes(id INTEGER); CREATE TABLE entities(id INTEGER);'
printf 'blobs 1\nchanges 0\nentities 0\n' > "$root/backup/manifest.txt"
printf '../outside\n' > "$root/backup/blobs.txt"
if bash "$script_dir/restore-drill.sh" "$root/backup" "$root/restored" /usr/bin/false /usr/bin/false > "$root/result.log" 2>&1; then
  echo 'FAIL: invalid blob name accepted' >&2; exit 1
fi
if [ -e "$root/restored" ]; then
  echo 'FAIL: invalid manifest produced a restored directory before validation' >&2; exit 1
fi
echo 'PASS: invalid backup rejected before restore'

reject() {
  local label=$1
  if bash "$script_dir/restore-drill.sh" "$root/backup" "$root/$label" /usr/bin/false /usr/bin/false > "$root/$label.log" 2>&1; then
    echo "FAIL: $label accepted" >&2; exit 1
  fi
  [ ! -e "$root/$label" ] || { echo "FAIL: $label began restore" >&2; exit 1; }
  echo "PASS: $label"
}
printf 'blob fixture' > "$root/blob"
hash=$(shasum -a 256 "$root/blob" | cut -d' ' -f1)
cp "$root/blob" "$root/backup/blobs/$hash"
printf '%s\n%s\n' "$hash" "$hash" > "$root/backup/blobs.txt"
printf 'blobs 2\nchanges 0\nentities 0\n' > "$root/backup/manifest.txt"
reject duplicate
printf '%s\n' "$hash" > "$root/backup/blobs.txt"
reject count-mismatch
printf 'blobs 1\nchanges 0\nentities 0\n' > "$root/backup/manifest.txt"
printf 'tampered' > "$root/backup/blobs/$hash"
reject corrupt-blob
cp "$root/blob" "$root/backup/blobs/$hash"
printf 'blobs 1\nchanges 1\nentities 0\n' > "$root/backup/manifest.txt"
reject database-count-mismatch

# Optional actual server/client path: exercises the successful chain, not mocks.
if [ "$#" -eq 2 ]; then
  server=$1; drill=$2
  APP_LITE_SERVER_TOKEN=restore-validation-test-token-00000000 "$server" --root "$root/live" --listen 127.0.0.1:0 > "$root/live.stdout" 2> "$root/live.log" &
  pid=$!
  trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
  for _ in $(seq 100); do
    if grep -q 'listening on' "$root/live.log"; then break; fi
    kill -0 "$pid" || { cat "$root/live.log"; exit 1; }
    sleep 0.1
  done
  cp "$root/blob" "$root/live/blobs/$hash"
  bash "$script_dir/backup.sh" "$root/live" "$root/valid"
  mkdir "$root/bin"
  printf '#!/bin/sh\nexit 127\n' > "$root/bin/xxd"
  chmod +x "$root/bin/xxd"
  PATH="$root/bin:$PATH" bash "$script_dir/restore-drill.sh" "$root/valid" "$root/success" "$server" "$drill"
  cmp "$root/blob" "$root/success/data/blobs/$hash"
  [ "$(sqlite3 "$root/success/client/library.sqlite" 'PRAGMA integrity_check')" = ok ]
  echo 'PASS: real backup/restore/client reopen without xxd'
fi
