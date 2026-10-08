#!/usr/bin/env bash
# Actual SQLite + restic integration; no mock backup tool.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd -P)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/joplin-backup-test.XXXXXXXX")
trap 'rm -rf "$test_root"' EXIT
mkdir "$test_root/data" "$test_root/data/blobs" "$test_root/work"
printf 'test encrypted backup, not user content\n' > "$test_root/original"
hash=$(shasum -a 256 "$test_root/original" | cut -d' ' -f1)
cp "$test_root/original" "$test_root/data/blobs/$hash"
sqlite3 "$test_root/data/sync.sqlite" 'CREATE TABLE changes(id INTEGER); INSERT INTO changes VALUES(1); CREATE TABLE entities(id INTEGER); INSERT INTO entities VALUES(7);'
openssl rand -hex 32 > "$test_root/password"
chmod 600 "$test_root/password"
export RESTIC_REPOSITORY="$test_root/restic"
export RESTIC_PASSWORD_FILE="$test_root/password"
restic init >/dev/null
bash "$script_dir/scheduled-backup.sh" "$test_root/data" "$test_root/work" "$script_dir/../backup.sh"
[[ $(restic snapshots --json | python3 -c 'import sys,json; print(len(json.load(sys.stdin)))') == 1 ]]
restic restore latest --target "$test_root/restored" >/dev/null
restored=$(find "$test_root/restored" -type f -name sync.sqlite)
[[ $(sqlite3 "$restored" 'SELECT id FROM entities') == 7 ]]
restored_blob=$(find "$test_root/restored" -type f -name "$hash")
cmp "$test_root/original" "$restored_blob"
[[ $(find "$test_root/work" -mindepth 1 | wc -l) == 0 ]]
echo 'PASS encrypted snapshot restores actual database and blob; staging cleaned'
printf 'corrupt\n' >> "$test_root/data/blobs/$hash"
if bash "$script_dir/scheduled-backup.sh" "$test_root/data" "$test_root/work" "$script_dir/../backup.sh"; then
  echo 'FAIL corrupt blob reported backup success' >&2; exit 1
fi
[[ $(restic snapshots --json | python3 -c 'import sys,json; print(len(json.load(sys.stdin)))') == 1 ]]
[[ $(find "$test_root/work" -mindepth 1 | wc -l) == 0 ]]
echo 'PASS corrupt source never publishes an encrypted snapshot; staging cleaned'
cp "$test_root/original" "$test_root/data/blobs/$hash"
openssl rand -hex 32 > "$test_root/wrong-password"
export RESTIC_PASSWORD_FILE="$test_root/wrong-password"
if bash "$script_dir/scheduled-backup.sh" "$test_root/data" "$test_root/work" "$script_dir/../backup.sh"; then
  echo 'FAIL wrong password reported backup success' >&2; exit 1
fi
export RESTIC_PASSWORD_FILE="$test_root/password"
[[ $(restic snapshots --json | python3 -c 'import sys,json; print(len(json.load(sys.stdin)))') == 1 ]]
[[ $(find "$test_root/work" -mindepth 1 | wc -l) == 0 ]]
echo 'PASS wrong repository password fails without affecting prior snapshot'
