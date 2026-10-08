#!/usr/bin/env bash
# Exercise the actual backup script and SQLite; refuse invalid paths without
# creating source databases or replacing caller-owned links.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd -P)
root=$(mktemp -d "${TMPDIR:-/tmp}/joplin-backup-preflight.XXXXXX")
trap 'rm -rf "$root"' EXIT
passed=0
failed=0
pass() { echo "PASS: $1"; passed=$((passed + 1)); }
fail() { echo "FAIL: $1" >&2; failed=$((failed + 1)); }

mkdir -p "$root/missing/data/blobs"
if bash "$script_dir/backup.sh" "$root/missing/data" "$root/missing/backup" > "$root/missing.log" 2>&1; then
  fail 'missing database accepted'
elif [[ -e "$root/missing/data/sync.sqlite" || -e "$root/missing/backup" ]]; then
  fail 'missing database failure changed source or published backup'
else
  pass 'missing database leaves source untouched'
fi

mkdir -p "$root/linked/data/blobs"
sqlite3 "$root/original.sqlite" 'CREATE TABLE changes(id INTEGER); CREATE TABLE entities(id INTEGER); INSERT INTO entities VALUES(23);'
source_before=$(shasum -a 256 < "$root/original.sqlite")
ln -s "$root/original.sqlite" "$root/linked/data/sync.sqlite"
if bash "$script_dir/backup.sh" "$root/linked/data" "$root/linked/backup" > "$root/linked.log" 2>&1; then
  fail 'symlinked source database accepted'
elif [[ ! -L "$root/linked/data/sync.sqlite" || -e "$root/linked/backup" || "$(shasum -a 256 < "$root/original.sqlite")" != "$source_before" ]]; then
  fail 'symlinked database failure changed source or published backup'
else
  pass 'symlinked database rejected without source changes'
fi

mkdir -p "$root/schema/data/blobs"
sqlite3 "$root/schema/data/sync.sqlite" 'CREATE TABLE unrelated(id INTEGER); INSERT INTO unrelated VALUES(7);'
schema_before=$(shasum -a 256 < "$root/schema/data/sync.sqlite")
if bash "$script_dir/backup.sh" "$root/schema/data" "$root/schema/backup" > "$root/schema.log" 2>&1; then
  fail 'database without server tables published as successful backup'
elif [[ -e "$root/schema/backup" || "$(shasum -a 256 < "$root/schema/data/sync.sqlite")" != "$schema_before" ]]; then
  fail 'wrong schema failure changed source or published backup'
else
  pass 'wrong schema fails without publishing backup'
fi

mkdir -p "$root/no-blobs/data"
cp "$root/original.sqlite" "$root/no-blobs/data/sync.sqlite"
if bash "$script_dir/backup.sh" "$root/no-blobs/data" "$root/no-blobs/backup" > "$root/no-blobs.log" 2>&1; then
  fail 'missing blob directory published as empty backup'
elif [[ -e "$root/no-blobs/backup" || -e "$root/no-blobs/data/blobs" ]]; then
  fail 'missing blob directory failure created output or altered source'
else
  pass 'missing blob directory rejected without changes'
fi

mkdir -p "$root/dangling/data/blobs"
cp "$root/original.sqlite" "$root/dangling/data/sync.sqlite"
ln -s "$root/not-created" "$root/dangling/backup"
if bash "$script_dir/backup.sh" "$root/dangling/data" "$root/dangling/backup" > "$root/dangling.log" 2>&1; then
  fail 'dangling backup target accepted'
elif [[ ! -L "$root/dangling/backup" || "$(readlink "$root/dangling/backup")" != "$root/not-created" || -e "$root/not-created" ]]; then
  fail 'dangling target failure replaced link or created referent'
else
  pass 'dangling backup target preserved'
fi

# A regular read-only source must still produce a usable snapshot. This catches
# rejecting all backups as a false fix and checks SQLite's real backup API.
mkdir -p "$root/valid/data/blobs"
cp "$root/original.sqlite" "$root/valid/data/sync.sqlite"
chmod 400 "$root/valid/data/sync.sqlite"
valid_before=$(shasum -a 256 < "$root/valid/data/sync.sqlite")
if bash "$script_dir/backup.sh" "$root/valid/data" "$root/valid/backup" > "$root/valid.log" 2>&1 \
  && [[ "$(sqlite3 -readonly "$root/valid/backup/sync.sqlite" 'SELECT id FROM entities; PRAGMA integrity_check;')" == $'23\nok' ]] \
  && [[ "$(shasum -a 256 < "$root/valid/data/sync.sqlite")" == "$valid_before" ]] \
  && [[ ! -e "$root/valid/data/sync.sqlite-journal" ]]; then
  pass 'regular read-only source snapshots without modification'
else
  fail 'regular read-only source cannot be backed up safely'
fi

# Keep a real WAL writer alive throughout the online backup. Both entity
# counts change in one transaction; the snapshot must not split them.
mkdir -p "$root/wal/data/blobs"
if python3 - "$script_dir/backup.sh" "$root/wal" <<'PY'
import pathlib
import sqlite3
import subprocess
import sys
import threading

script, root = sys.argv[1], pathlib.Path(sys.argv[2])
database = root / 'data' / 'sync.sqlite'
connection = sqlite3.connect(database, check_same_thread=False)
connection.execute('PRAGMA journal_mode=WAL')
connection.executescript('CREATE TABLE changes(id INTEGER); CREATE TABLE entities(id INTEGER);')
stop, started = threading.Event(), threading.Event()
errors = []

def write():
    try:
        while not stop.is_set():
            with connection:
                connection.execute('INSERT INTO changes VALUES(1)')
                connection.execute('INSERT INTO entities VALUES(1)')
            started.set()
            stop.wait(0.005)
    except Exception as error:
        errors.append(error)
        started.set()

worker = threading.Thread(target=write)
worker.start()
try:
    assert started.wait(5) and worker.is_alive() and not errors
    result = subprocess.run(['bash', script, str(root / 'data'), str(root / 'backup')], capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stderr
    with sqlite3.connect((root / 'backup' / 'sync.sqlite').as_uri() + '?mode=ro', uri=True) as snapshot:
        assert snapshot.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        changes = snapshot.execute('SELECT count(*) FROM changes').fetchone()[0]
        entities = snapshot.execute('SELECT count(*) FROM entities').fetchone()[0]
        assert changes == entities and changes > 0, (changes, entities)
    assert worker.is_alive() and not errors
finally:
    stop.set()
    worker.join(5)
    connection.close()
assert not worker.is_alive() and not errors
PY
then
  pass 'online WAL writer produces consistent snapshot'
else
  fail 'online WAL writer cannot be backed up consistently'
fi
printf 'results: %s passed, %s failed\n' "$passed" "$failed"
[[ "$failed" == 0 ]]
