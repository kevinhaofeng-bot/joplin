#!/usr/bin/env bash
# Catch user paths being interpreted as SQLite dot-command syntax, and ensure
# failed publishing never modifies the source or an existing backup.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd -P)
root=$(mktemp -d "${TMPDIR:-/tmp}/joplin-backup-paths.XXXXXX")
echo "test evidence: $root"
passed=0
failed=0

backup_case() {
  local label=$1 name=$2
  local directory="$root/$label/$name"
  mkdir -p "$directory/data/blobs"
  sqlite3 "$directory/data/sync.sqlite" 'CREATE TABLE changes(id INTEGER); CREATE TABLE entities(id INTEGER); INSERT INTO changes VALUES(17); INSERT INTO entities VALUES(23);'
  printf 'backup path fixture' > "$directory/blob"
  local hash
  hash=$(shasum -a 256 "$directory/blob" | cut -d' ' -f1)
  cp "$directory/blob" "$directory/data/blobs/$hash"
  local before after
  before=$(shasum -a 256 < "$directory/data/sync.sqlite")
  if [[ "$label" == relative ]]; then
    if ! (cd "$directory" && bash "$script_dir/backup.sh" data backup) > "$root/$label.log" 2>&1; then
      echo "FAIL: $label"; cat "$root/$label.log"; failed=$((failed + 1)); return
    fi
  elif ! bash "$script_dir/backup.sh" "$directory/data" "$directory/backup" > "$root/$label.log" 2>&1; then
    echo "FAIL: $label"; cat "$root/$label.log"; failed=$((failed + 1)); return
  fi
  after=$(shasum -a 256 < "$directory/data/sync.sqlite")
  [[ "$after" == "$before" ]]
  [[ "$(sqlite3 -readonly "$directory/backup/sync.sqlite" 'SELECT id FROM changes; SELECT id FROM entities; PRAGMA integrity_check;')" == $'17\n23\nok' ]]
  [[ "$(< "$directory/backup/manifest.txt")" == $'blobs 1\nchanges 1\nentities 1' ]]
  [[ "$(< "$directory/backup/blobs.txt")" == "$hash" ]]
  cmp "$directory/blob" "$directory/backup/blobs/$hash"
  cmp "$directory/blob" "$directory/data/blobs/$hash"
  [[ -z "$(find "$directory" -maxdepth 1 -name 'backup.partial.*' -print)" ]]
  echo "PASS: $label"
  passed=$((passed + 1))
}

backup_case plain plain
backup_case unicode-spaces '中文 备份目录'
backup_case apostrophe "Kevin's 备份"
backup_case quotes '双"引号与\反斜线'
backup_case newline $'备份\n第二行'
backup_case relative "相对 Kevin's 目录"

# A retry must not overwrite an already published backup.
existing="$root/plain/plain/backup"
before=$(shasum -a 256 < "$existing/sync.sqlite")
if bash "$script_dir/backup.sh" "$root/plain/plain/data" "$existing" > "$root/target-exists.log" 2>&1; then
  echo 'FAIL: target-exists'; failed=$((failed + 1))
else
  [[ "$(shasum -a 256 < "$existing/sync.sqlite")" == "$before" ]]
  echo 'PASS: target-exists'; passed=$((passed + 1))
fi

# Corrupt source bytes fail hashing; no final or partial directory remains.
printf 'tampered' > "$root/plain/plain/data/blobs/$(< "$existing/blobs.txt")"
if bash "$script_dir/backup.sh" "$root/plain/plain/data" "$root/corrupt-backup" > "$root/corrupt-blob.log" 2>&1; then
  echo 'FAIL: corrupt-blob'; failed=$((failed + 1))
else
  [[ ! -e "$root/corrupt-backup" ]]
  [[ -z "$(find "$root" -maxdepth 1 -name 'corrupt-backup.partial.*' -print)" ]]
  echo 'PASS: corrupt-blob'; passed=$((passed + 1))
fi
printf 'results: %s passed, %s failed\n' "$passed" "$failed"
[[ "$failed" == 0 ]]
