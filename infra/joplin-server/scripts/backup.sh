#!/usr/bin/env bash

set -euo pipefail

backup_env_file=${BACKUP_ENV_FILE:-/etc/joplin-server/backup.env}

die() {
  printf 'joplin backup failed: %s\n' "$*" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || die 'must run as root'
[ -f "$backup_env_file" ] && [ ! -L "$backup_env_file" ] || die 'missing root-only backup environment'

set -a
. "$backup_env_file"
set +a

: "${JOPLIN_COMPOSE_DIR:?missing JOPLIN_COMPOSE_DIR}"
: "${JOPLIN_ENV_FILE:?missing JOPLIN_ENV_FILE}"
: "${RESTIC_REPOSITORY:?missing RESTIC_REPOSITORY}"
: "${RESTIC_PASSWORD_FILE:?missing RESTIC_PASSWORD_FILE}"

[ -f "$RESTIC_PASSWORD_FILE" ] && [ ! -L "$RESTIC_PASSWORD_FILE" ] || die 'missing restic password file'
password_mode=$(stat -c '%a' "$RESTIC_PASSWORD_FILE")
[ "$password_mode" = 600 ] || die 'restic password file must be mode 0600'

set -a
. "$JOPLIN_ENV_FILE"
set +a

export RESTIC_REPOSITORY RESTIC_PASSWORD_FILE

docker compose --project-directory "$JOPLIN_COMPOSE_DIR" --env-file "$JOPLIN_ENV_FILE" exec -T db \
  pg_dump --format=custom --username="$POSTGRES_USER" --dbname="$POSTGRES_DATABASE" | \
  restic backup --tag joplin-database --stdin --stdin-filename database.dump

tar -C "$JOPLIN_COMPOSE_DIR" -czf - compose.yaml env.example README.md | \
  restic backup --tag joplin-metadata --stdin --stdin-filename server-metadata.tar.gz

restic forget --keep-daily 14 --keep-weekly 8 --keep-monthly 12 --prune
