#!/usr/bin/env bash

set -euo pipefail

backup_env_file=${BACKUP_ENV_FILE:-/etc/joplin-server/backup.env}

die() {
  printf 'joplin backup failed: %s\n' "$*" >&2
  exit 1
}

require_root_owned_mode_600_file() {
  local file=$1
  local description=$2
  local ownership_and_mode=

  [ -f "$file" ] && [ ! -L "$file" ] || die "missing root-only $description"
  ownership_and_mode=$(stat -c '%u:%a' "$file") || die "cannot stat $description"
  [ "$ownership_and_mode" = '0:600' ] || die "$description must be a root-owned regular file with mode 0600"
}

[ "$(id -u)" -eq 0 ] || die 'must run as root'
require_root_owned_mode_600_file "$backup_env_file" 'backup environment'

set -a
. "$backup_env_file"
set +a

: "${JOPLIN_COMPOSE_DIR:?missing JOPLIN_COMPOSE_DIR}"
: "${JOPLIN_ENV_FILE:?missing JOPLIN_ENV_FILE}"
: "${RESTIC_REPOSITORY:?missing RESTIC_REPOSITORY}"
: "${RESTIC_PASSWORD_FILE:?missing RESTIC_PASSWORD_FILE}"

require_root_owned_mode_600_file "$JOPLIN_ENV_FILE" 'Joplin Compose environment'
require_root_owned_mode_600_file "$RESTIC_PASSWORD_FILE" 'restic password file'

set -a
. "$JOPLIN_ENV_FILE"
set +a

export RESTIC_REPOSITORY RESTIC_PASSWORD_FILE

docker compose --project-directory "$JOPLIN_COMPOSE_DIR" --env-file "$JOPLIN_ENV_FILE" exec -T db \
  pg_dump --format=custom --username="$POSTGRES_USER" --dbname="$POSTGRES_DATABASE" | \
  restic backup --tag joplin-database --stdin --stdin-filename database.dump

tar -C "$JOPLIN_COMPOSE_DIR" -czf - compose.yaml env.example README.md | \
  restic backup --tag joplin-metadata --stdin --stdin-filename server-metadata.tar.gz

restic forget --keep-daily 14 --keep-weekly 8 --keep-monthly 12
