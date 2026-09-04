#!/usr/bin/env bash

set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
deploy_env_file=${DEPLOY_ENV_FILE:-}
compose_file="$root_dir/compose.yaml"
env_example="$root_dir/env.example"
socat_service="$root_dir/systemd/joplin-tls-proxy.service"
backup_script="$root_dir/scripts/backup.sh"
restore_script="$root_dir/scripts/restore-drill.sh"
readme_file="$root_dir/README.md"

fail() {
  printf 'config contract failed: %s\n' "$*" >&2
  exit 1
}

require_file() {
  [ -f "$1" ] || fail "missing required file: $1"
}

require_literal() {
  grep -Fq -- "$2" "$1" || fail "missing literal in $1: $2"
}

require_root_owned_mode_600_file() {
  local file=$1
  local ownership_and_mode=

  [ -f "$file" ] && [ ! -L "$file" ] || fail "deployment env must be a regular non-symlink file: $file"
  ownership_and_mode=$(stat -c '%u:%g:%a' "$file") || fail "cannot stat deployment env: $file"
  [ "$ownership_and_mode" = '0:0:600' ] || fail 'deployment env must be root:root mode 0600'
}

read_deployment_value() {
  local key=$1
  local line=
  local match_count=0
  local value=

  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      "$key="*)
        match_count=$((match_count + 1))
        value=${line#"$key="}
        value=${value%$'\r'}
        ;;
    esac
  done < "$deploy_env_file"

  [ "$match_count" -eq 1 ] || return 1
  printf '%s' "$value"
}

validate_deployment_password() {
  local key=$1
  local value=
  local minimum_password_length=20

  value=$(read_deployment_value "$key") || fail "deployment env must define $key exactly once"
  case "$value" in
    ''|admin|__GENERATE_AT_DEPLOYMENT__)
      fail "$key must not be empty, admin, or the deployment placeholder"
      ;;
  esac
  [ "${#value}" -ge "$minimum_password_length" ] || fail "$key must be at least $minimum_password_length characters"
}

validate_deployment_env() {
  [ -n "$deploy_env_file" ] || return 0
  [ "$(id -u)" -eq 0 ] || fail 'deployment env validation must run as root'
  require_root_owned_mode_600_file "$deploy_env_file"
  validate_deployment_password POSTGRES_PASSWORD
  validate_deployment_password DEFAULT_ADMIN_PASSWORD
}

require_file "$compose_file"
require_file "$env_example"
require_file "$socat_service"
require_file "$backup_script"
require_file "$restore_script"
require_file "$readme_file"
require_file "$root_dir/systemd/joplin-backup.service"
require_file "$root_dir/systemd/joplin-backup.timer"
require_file "$root_dir/systemd/joplin-tls-proxy-cert-watch.path"
require_file "$root_dir/systemd/joplin-tls-proxy-cert-watch.service"

require_literal "$compose_file" 'joplin/server:3.7.1@sha256:b9666df06e7e2db20aeb961d2aca19e20664b985ead96995ecd32f9d720f002c'
require_literal "$compose_file" 'postgres:16.10-bookworm@sha256:94f23d40fdaf5e60cb2fd8a98c22f02a7b8724949f310d95a0ddf075e8c8b208'
require_literal "$compose_file" '"192.168.3.3:22300:22300"'
if grep -Fq -- 'JOPLIN_BIND_ADDRESS' "$compose_file"; then
  fail 'production Joplin bind address must not be environment-overridable'
fi
require_literal "$compose_file" 'healthcheck:'
require_literal "$compose_file" 'pg_isready'
require_literal "$compose_file" '/api/ping'
require_literal "$compose_file" 'restart: unless-stopped'
require_literal "$compose_file" 'joplin-server-egress:'

db_block=$(awk '/^  db:/{inside=1; next} /^  [[:alnum:]_-]+:/{if (inside) exit} inside {print}' "$compose_file")
printf '%s\n' "$db_block" | grep -Fq 'ports:' && fail 'PostgreSQL must not publish host ports'
printf '%s\n' "$db_block" | grep -Fq 'joplin-server-internal' || fail 'PostgreSQL must use only the internal database network'
app_block=$(awk '/^  app:/{inside=1; next} /^  [[:alnum:]_-]+:/{if (inside) exit} inside {print}' "$compose_file")
printf '%s\n' "$app_block" | grep -Fq 'joplin-server-internal' || fail 'Joplin app must reach PostgreSQL over the internal network'
printf '%s\n' "$app_block" | grep -Fq 'joplin-server-egress' || fail 'Joplin app must retain egress for its startup NTP check'
require_literal "$compose_file" 'DEFAULT_ADMIN_PASSWORD: ${DEFAULT_ADMIN_PASSWORD:?set DEFAULT_ADMIN_PASSWORD in .env}'

require_literal "$env_example" 'POSTGRES_PASSWORD=__GENERATE_AT_DEPLOYMENT__'
require_literal "$env_example" 'DEFAULT_ADMIN_PASSWORD=__GENERATE_AT_DEPLOYMENT__'
if grep -Fq -- 'DEFAULT_ADMIN_PASSWORD=admin' "$env_example"; then
  fail 'example must not provide the upstream admin default password'
fi
if grep -Eq -- '-----BEGIN( [A-Z]+)? PRIVATE KEY-----|ghp_[A-Za-z0-9]{20,}|glpat-[A-Za-z0-9_-]{20,}' "$root_dir"/{compose.yaml,env.example,README.md,scripts/backup.sh,scripts/restore-drill.sh,systemd/*.service,systemd/*.timer,systemd/*.path}; then
  fail 'infrastructure artifacts must not contain committed credentials'
fi

require_literal "$socat_service" 'RuntimeDirectory=joplin-tls-proxy'
require_literal "$socat_service" 'RuntimeDirectoryMode=0700'
require_literal "$socat_service" '/run/joplin-tls-proxy/server.pem'
require_literal "$socat_service" 'OPENSSL-LISTEN:22300,reuseaddr,fork'
require_literal "$socat_service" 'min-version=TLS1.2'
require_literal "$socat_service" 'TCP:192.168.3.3:22300'
require_literal "$socat_service" 'Restart=on-failure'
require_literal "$socat_service" 'TLS_CERT_SOURCE'
require_literal "$socat_service" 'TLS_KEY_SOURCE'
require_literal "$socat_service" 'openssl x509'
require_literal "$socat_service" 'openssl pkey'
require_literal "$socat_service" 'cmp -s'

require_literal "$backup_script" 'RESTIC_PASSWORD_FILE'
require_literal "$backup_script" 'require_root_owned_mode_600_file "$backup_env_file"'
require_literal "$backup_script" 'require_root_owned_mode_600_file "$JOPLIN_ENV_FILE"'
require_literal "$backup_script" 'require_root_owned_mode_600_file "$RESTIC_PASSWORD_FILE"'
require_literal "$backup_script" "stat -c '%u:%a'"
require_literal "$restore_script" 'require_root_owned_mode_600_file "$restore_env_file"'
require_literal "$restore_script" 'require_root_owned_mode_600_file "$RESTIC_PASSWORD_FILE"'
require_literal "$restore_script" "stat -c '%u:%a'"
require_literal "$backup_script" 'pg_dump --format=custom'
require_literal "$backup_script" '--tag joplin-database'
require_literal "$backup_script" '--tag joplin-metadata'
require_literal "$backup_script" 'database.dump'
require_literal "$backup_script" 'restic backup'
if grep -Fq -- 'RESTIC_PASSWORD=' "$backup_script"; then
  fail 'backup must use a root-only password file, not RESTIC_PASSWORD'
fi

require_literal "$restore_script" 'RESTORE_PROJECT=joplin-server-restore-drill'
require_literal "$restore_script" 'RESTORE_NETWORK=joplin-server-restore-network'
require_literal "$restore_script" 'RESTORE_VOLUME=joplin-server-restore-postgres'
require_literal "$restore_script" 'RESTORE_PORT=127.0.0.1:22301'
require_literal "$restore_script" 'RESTORE_CONFIG_ONLY=${RESTORE_CONFIG_ONLY:-0}'
require_literal "$restore_script" 'RESTORE_NETWORK=$RESTORE_NETWORK'
require_literal "$restore_script" 'RESTORE_VOLUME=$RESTORE_VOLUME'
require_literal "$restore_script" 'RESTORE_PORT=$RESTORE_PORT'
require_literal "$restore_script" 'docker compose -p "$RESTORE_PROJECT" --env-file "$RESTORE_ENV" -f "$RESTORE_COMPOSE" config --quiet'
require_literal "$restore_script" 'trap cleanup EXIT'
require_literal "$restore_script" 'restic dump --tag joplin-database latest database.dump'
require_literal "$restore_script" 'pg_restore --exit-on-error --no-owner --no-privileges'
require_literal "$restore_script" 'RESTORE_DB_READY_DEADLINE'
require_literal "$restore_script" ' -lt "$RESTORE_DB_READY_DEADLINE"'
require_literal "$restore_script" 'RESTORE_APP_READY_DEADLINE'
require_literal "$restore_script" 'restore Joplin app did not become ready before the deadline'
require_literal "$restore_script" '- restore-postgres:/var/lib/postgresql/data'
require_literal "$restore_script" 'restore-postgres:'
require_literal "$restore_script" 'down --volumes --remove-orphans'
if grep -Fq -- 'joplin-server-postgres' "$restore_script"; then
  fail 'restore drill must not reference the production volume'
fi
if grep -Fq -- 'restic restore latest' "$restore_script"; then
  fail 'restore drill must select the tagged database snapshot explicitly'
fi

require_literal "$root_dir/systemd/joplin-backup.service" 'EnvironmentFile=/etc/joplin-server/backup.env'
require_literal "$root_dir/systemd/joplin-backup.service" 'RESTIC_CACHE_DIR=/srv/joplin-server/.restic-cache'
require_literal "$root_dir/systemd/joplin-backup.timer" 'RandomizedDelaySec='
require_literal "$root_dir/systemd/joplin-tls-proxy-cert-watch.path" 'PathModified=/etc/pve/local/pveproxy-ssl.pem'
require_literal "$root_dir/systemd/joplin-tls-proxy-cert-watch.path" 'PathModified=/etc/pve/local/pveproxy-ssl.key'
require_literal "$root_dir/systemd/joplin-tls-proxy-cert-watch.service" 'systemctl try-restart joplin-tls-proxy.service'
require_literal "$readme_file" 'automatically observes the authoritative PVE certificate and key paths'
require_literal "$readme_file" '/srv/joplin-server/.restic-cache'
require_literal "$readme_file" 'root-only'
require_literal "$readme_file" 'DEFAULT_ADMIN_PASSWORD'
require_literal "$readme_file" 'first initialization'
require_literal "$readme_file" 'DEPLOY_ENV_FILE'
require_literal "$readme_file" 'minimum 20 characters'
require_literal "$0" 'deploy_env_file=${DEPLOY_ENV_FILE:-}'
require_literal "$0" "stat -c '%u:%g:%a'"
require_literal "$0" '0:0:600'
require_literal "$0" 'minimum_password_length=20'
require_literal "$0" 'validate_deployment_env'
require_literal "$0" '__GENERATE_AT_DEPLOYMENT__'

validate_deployment_env

printf 'Joplin Server infrastructure static contract: PASS\n'
