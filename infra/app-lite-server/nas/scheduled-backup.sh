#!/usr/bin/env bash
# Create a checked online snapshot, then publish it to an initialized,
# encrypted restic repository. Never copy live SQLite/WAL with restic.
# Usage: scheduled-backup.sh <data> <private-work-dir> <backup.sh>
# RESTIC_REPOSITORY and RESTIC_PASSWORD_FILE come from a root-only env file.
set -euo pipefail
data=${1:?server data directory}
work=${2:?private staging directory}
backup_script=${3:?checked backup.sh path}
: "${RESTIC_REPOSITORY:?initialized encrypted repository required}"
: "${RESTIC_PASSWORD_FILE:?protected password file required}"
[[ -d $work && ! -L $work ]]
[[ -f $RESTIC_PASSWORD_FILE && ! -L $RESTIC_PASSWORD_FILE ]]
umask 077
staging=$(mktemp -d "$work/snapshot.XXXXXXXX")
# Only this invocation's mktemp directory is removed; repository/source remain.
trap 'rm -rf "$staging"' EXIT
bash "$backup_script" "$data" "$staging/checked"
restic backup --tag joplin-lite-checked "$staging/checked"
echo 'Encrypted checked snapshot published'
