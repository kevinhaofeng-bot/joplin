#!/usr/bin/env bash

set -euo pipefail

restore_env_file=${BACKUP_ENV_FILE:-/etc/joplin-server/backup.env}
RESTORE_CONFIG_ONLY=${RESTORE_CONFIG_ONLY:-0}
RESTORE_PROJECT=joplin-server-restore-drill
RESTORE_NETWORK=joplin-server-restore-network
RESTORE_VOLUME=joplin-server-restore-postgres
RESTORE_PORT=127.0.0.1:22301
RESTORE_ROOT=
RESTORE_ENV=
RESTORE_COMPOSE=
RESTORE_DB_READY_DEADLINE=
RESTORE_APP_READY_DEADLINE=
RESTORE_DOCKER_ACTIVE=0

die() {
  printf 'joplin restore drill failed: %s\n' "$*" >&2
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

cleanup() {
  if [ "$RESTORE_DOCKER_ACTIVE" = 1 ] && [ -n "$RESTORE_COMPOSE" ] && [ -f "$RESTORE_COMPOSE" ]; then
    docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" down --volumes --remove-orphans || true
    docker network rm "$RESTORE_NETWORK" >/dev/null 2>&1 || true
    docker volume rm "$RESTORE_VOLUME" >/dev/null 2>&1 || true
  fi
  [ -z "$RESTORE_ROOT" ] || rm -rf "$RESTORE_ROOT"
  [ -z "$RESTORE_ENV" ] || rm -f "$RESTORE_ENV"
  [ -z "$RESTORE_COMPOSE" ] || rm -f "$RESTORE_COMPOSE"
}
trap cleanup EXIT

[ "$(id -u)" -eq 0 ] || die 'must run as root'
[ "$RESTORE_CONFIG_ONLY" = 0 ] || [ "$RESTORE_CONFIG_ONLY" = 1 ] || die 'RESTORE_CONFIG_ONLY must be 0 or 1'

command -v docker >/dev/null
command -v openssl >/dev/null

umask 077
RESTORE_ROOT=$(mktemp -d /var/tmp/joplin-server-restore.XXXXXX)
RESTORE_ENV=$(mktemp /var/tmp/joplin-server-restore-env.XXXXXX)
RESTORE_COMPOSE=$(mktemp /var/tmp/joplin-server-restore-compose.XXXXXX)
RESTORE_DB_PASSWORD=$(openssl rand -hex 32)

cat > "$RESTORE_ENV" <<EOF
POSTGRES_DATABASE=joplin_restore
POSTGRES_USER=joplin_restore
POSTGRES_PASSWORD=$RESTORE_DB_PASSWORD
APP_BASE_URL=http://127.0.0.1:22301
RESTORE_NETWORK=$RESTORE_NETWORK
RESTORE_VOLUME=$RESTORE_VOLUME
RESTORE_PORT=$RESTORE_PORT
EOF

cat > "$RESTORE_COMPOSE" <<EOF
services:
  db:
    image: postgres:16.10-bookworm@sha256:94f23d40fdaf5e60cb2fd8a98c22f02a7b8724949f310d95a0ddf075e8c8b208
    environment:
      POSTGRES_DB: \${POSTGRES_DATABASE}
      POSTGRES_USER: \${POSTGRES_USER}
      POSTGRES_PASSWORD: \${POSTGRES_PASSWORD}
    volumes:
      - restore-postgres:/var/lib/postgresql/data
    networks:
      - restore
  app:
    image: joplin/server:3.7.1@sha256:b9666df06e7e2db20aeb961d2aca19e20664b985ead96995ecd32f9d720f002c
    depends_on:
      db:
        condition: service_started
    ports:
      - \${RESTORE_PORT}:22300
    environment:
      APP_PORT: 22300
      APP_BASE_URL: \${APP_BASE_URL}
      DB_CLIENT: pg
      POSTGRES_HOST: db
      POSTGRES_PORT: 5432
      POSTGRES_DATABASE: \${POSTGRES_DATABASE}
      POSTGRES_USER: \${POSTGRES_USER}
      POSTGRES_PASSWORD: \${POSTGRES_PASSWORD}
    networks:
      - restore
networks:
  restore:
    name: \${RESTORE_NETWORK}
volumes:
  restore-postgres:
    name: \${RESTORE_VOLUME}
EOF

docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" config --quiet
[ "$RESTORE_CONFIG_ONLY" = 1 ] && exit 0

require_root_owned_mode_600_file "$restore_env_file" 'backup environment'
set -a
. "$restore_env_file"
set +a

: "${RESTIC_REPOSITORY:?missing RESTIC_REPOSITORY}"
: "${RESTIC_PASSWORD_FILE:?missing RESTIC_PASSWORD_FILE}"
require_root_owned_mode_600_file "$RESTIC_PASSWORD_FILE" 'restic password file'
command -v restic >/dev/null

export RESTIC_REPOSITORY RESTIC_PASSWORD_FILE
restic dump --tag joplin-database latest database.dump > "$RESTORE_ROOT/database.dump"
[ -s "$RESTORE_ROOT/database.dump" ] || die 'tagged snapshot does not contain database.dump'

RESTORE_DOCKER_ACTIVE=1
docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" up -d db
RESTORE_DB_READY_DEADLINE=$((SECONDS + 120))
until docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" exec -T db pg_isready -U joplin_restore -d joplin_restore; do
  [ "$SECONDS" -lt "$RESTORE_DB_READY_DEADLINE" ] || die 'restore PostgreSQL did not become ready before the deadline'
  sleep 2
done
docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" exec -T db \
  pg_restore --exit-on-error --no-owner --no-privileges --username=joplin_restore --dbname=joplin_restore < "$RESTORE_ROOT/database.dump"
docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" up -d app
RESTORE_APP_READY_DEADLINE=$((SECONDS + 120))
until curl --fail --silent --show-error --max-time 15 "http://$RESTORE_PORT/api/ping" >/dev/null; do
  [ "$SECONDS" -lt "$RESTORE_APP_READY_DEADLINE" ] || die 'restore Joplin app did not become ready before the deadline'
  sleep 2
done
printf 'restore_table_counts_begin\n'
docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" exec -T db \
  psql -At --username=joplin_restore --dbname=joplin_restore -c \
  "SELECT 'users=' || count(*) FROM users;
SELECT 'items=' || count(*) FROM items;
SELECT 'item_resources=' || count(*) FROM item_resources;
SELECT 'files=' || count(*) FROM files;"
printf 'restore_table_counts_end\n'
