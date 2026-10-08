#!/usr/bin/env bash
# Consistent backup of an app-lite-server data directory:
#   backup.sh <data-dir> <new-backup-dir>
# SQLite is copied with the online backup API (safe while the server runs);
# blobs are content-addressed and immutable, so each is copied and its name
# re-checked against its SHA-256. Partial uploads are not part of a backup.
set -euo pipefail
data=${1:?data dir}
out=${2:?new backup dir}
[ ! -e "$out" ] && [ ! -L "$out" ] || { echo "backup target exists: $out" >&2; exit 1; }
# Never let SQLite create a missing source database, or silently treat a
# missing attachment directory as an empty one.
[ -f "$data/sync.sqlite" ] && [ ! -L "$data/sync.sqlite" ] || { echo 'invalid source database' >&2; exit 1; }
[ -d "$data/blobs" ] && [ ! -L "$data/blobs" ] || { echo 'invalid source blobs directory' >&2; exit 1; }
tmp="$out.partial.$$"
# Never leave a half-written backup that could be mistaken for a good one.
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/blobs"
# Order matters: the database snapshot comes first. Clients finish a blob
# before publishing the attachment that references it, so every blob the
# snapshot references is already on disk when the copy loop runs.
# Keep user paths in argv, not SQLite's dot-command grammar (quotes in a
# destination name otherwise break .backup). Resolve before changing cwd so
# relative data directories keep their original meaning.
database="$(cd "$data" && pwd -P)/sync.sqlite"
( cd "$tmp" && sqlite3 -readonly "$database" '.backup sync.sqlite' )
[ "$(sqlite3 "$tmp/sync.sqlite" 'PRAGMA integrity_check')" = ok ]
# Keep fallible SQLite reads outside echo substitutions: echo returns success
# even when the nested query fails and would publish a malformed manifest.
changes=$(sqlite3 -readonly "$tmp/sync.sqlite" 'SELECT count(*) FROM changes')
entities=$(sqlite3 -readonly "$tmp/sync.sqlite" 'SELECT count(*) FROM entities')
count=0
for blob in "$data"/blobs/*; do
  [ -e "$blob" ] || continue
  name=$(basename "$blob")
  cp "$blob" "$tmp/blobs/$name"
  [ "$(shasum -a 256 "$tmp/blobs/$name" | cut -d' ' -f1)" = "$name" ] || { echo "blob hash mismatch: $name" >&2; exit 1; }
  count=$((count + 1))
done
( cd "$tmp/blobs" && ls | sort ) > "$tmp/blobs.txt"
echo "blobs $count" > "$tmp/manifest.txt"
echo "changes $changes" >> "$tmp/manifest.txt"
echo "entities $entities" >> "$tmp/manifest.txt"
mv "$tmp" "$out"
trap - EXIT
cat "$out/manifest.txt"
