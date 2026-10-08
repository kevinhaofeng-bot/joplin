#!/usr/bin/env bash
# Real OpenSSL/filesystem tests; only the external service manager is doubled.
set -euo pipefail
scripts=$(cd "$(dirname "$0")" && pwd -P)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/joplin-tls-renew.XXXXXXXX")
trap 'rm -rf "$fixture"' EXIT
umask 077
mkdir "$fixture/base" "$fixture/base/ca" "$fixture/base/identities"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -days 3650 -subj '/CN=Joplin Lite renewal acceptance CA' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -keyout "$fixture/base/ca/key.pem" -out "$fixture/base/ca/cert.pem" >/dev/null 2>&1
bash "$scripts/issue-server-tls.sh" "$fixture/base/ca" "$fixture/base/identities/leaf-original" 1
ln -s identities/leaf-original "$fixture/base/current"
# A service-manager double is necessary to avoid restarting live NAS services.
# Issuance, CA verification, locking, links and certificate bytes remain real.
cp "$scripts/tls-renew-test-service.sh" "$fixture/hook"
chmod 700 "$fixture/hook"
fresh() { cp -a "$fixture/base" "$fixture/$1"; bash "$fixture/hook" restart "$fixture/$1"; }
run() { bash "$scripts/renew-server-tls.sh" "$fixture/$1" "$fixture/hook" "${2:-30}" 365; }
same_original() {
  [[ $(readlink "$fixture/$1/current") == identities/leaf-original ]]
  cmp "$fixture/base/identities/leaf-original/cert.pem" "$fixture/$1/current/cert.pem"
  cmp "$fixture/base/identities/leaf-original/key.pem" "$fixture/$1/current/key.pem"
}
# Break caught: missing orchestration leaves the expiring identity selected.
fresh renewed
if ! run renewed; then echo 'FAIL expiring identity was not renewed' >&2; exit 1; fi
[[ $(readlink "$fixture/renewed/current") != identities/leaf-original ]]
[[ $(sha256sum "$fixture/renewed/current/cert.pem" | cut -d' ' -f1) == $(< "$fixture/renewed/loaded") ]]
openssl verify -CAfile "$fixture/renewed/ca/cert.pem" -purpose sslserver -verify_hostname localhost "$fixture/renewed/current/cert.pem"
cmp "$fixture/base/ca/cert.pem" "$fixture/renewed/ca/cert.pem"
cmp "$fixture/base/ca/key.pem" "$fixture/renewed/ca/key.pem"
cmp "$fixture/base/identities/leaf-original/key.pem" "$fixture/renewed/identities/leaf-original/key.pem"
echo 'PASS expiring identity activated, old identity and stable root retained'
before=$(readlink "$fixture/renewed/current")
loaded=$(< "$fixture/renewed/loaded")
run renewed
[[ $(readlink "$fixture/renewed/current") == "$before" && $(< "$fixture/renewed/loaded") == "$loaded" ]]
[[ $(find "$fixture/renewed/identities" -mindepth 1 -maxdepth 1 -type d | wc -l) == 2 ]]
echo 'PASS healthy long-lived identity is a no-op, no restart or extra candidate'
fresh unhealthy
touch "$fixture/unhealthy/new-health-fails"
if run unhealthy; then echo 'FAIL bad new identity reported success' >&2; exit 1; fi
same_original unhealthy
[[ $(sha256sum "$fixture/unhealthy/current/cert.pem" | cut -d' ' -f1) == $(< "$fixture/unhealthy/loaded") ]]
echo 'PASS failed new-leaf health rolls pointer and running identity back'
fresh failed_restart
touch "$fixture/failed_restart/restart-fails"
if run failed_restart; then echo 'FAIL restart failure reported success' >&2; exit 1; fi
same_original failed_restart
echo 'PASS failed activation restart preserves usable original identity'
fresh bad_ca
cp "$fixture/bad_ca/current/key.pem" "$fixture/bad_ca/ca/key.pem"
if run bad_ca; then echo 'FAIL mismatched CA reported success' >&2; exit 1; fi
same_original bad_ca
[[ $(find "$fixture/bad_ca/identities" -mindepth 1 -maxdepth 1 -type d | wc -l) == 1 ]]
echo 'PASS failed issuance never selects or publishes a new active identity'
fresh unsafe
rm "$fixture/unsafe/current"
ln -s "$fixture/renewed/current" "$fixture/unsafe/current"
if run unsafe; then echo 'FAIL external pointer accepted' >&2; exit 1; fi
[[ $(readlink "$fixture/unsafe/current") == "$fixture/renewed/current" ]]
echo 'PASS external identity pointer refused without following or replacing it'
fresh locked
flock "$fixture/locked/renew.lock" bash -c 'touch "$1/lock-ready"; while [[ ! -f $1/release-lock ]]; do sleep 0.05; done' _ "$fixture/locked" &
lock_pid=$!
for attempt in $(seq 1 100); do [[ ! -f $fixture/locked/lock-ready ]] || break; sleep 0.02; done
[[ -f $fixture/locked/lock-ready ]]
set +e
run locked
lock_status=$?
set -e
touch "$fixture/locked/release-lock"
wait "$lock_pid"
[[ $lock_status == 75 ]]
same_original locked
echo 'PASS concurrent renewal refused with retryable exit75, identity unchanged'
fresh broken_rollback
touch "$fixture/broken_rollback/all-health-fails"
if run broken_rollback > "$fixture/rollback.log" 2>&1; then echo 'FAIL unhealthy rollback reported success' >&2; exit 1; fi
same_original broken_rollback
grep -q 'rollback health failed' "$fixture/rollback.log"
echo 'PASS rollback health failure remains explicit, never reported healthy'
# Break caught: SIGKILL/reboot after pointer switch leaves an unverified long-
# lived leaf selected, so an expiry-only check would silently skip recovery.
fresh interrupted
bash "$scripts/issue-server-tls.sh" "$fixture/interrupted/ca" "$fixture/interrupted/identities/leaf-interrupted" 365
ln -s identities/leaf-original "$fixture/interrupted/activation-pending"
ln -s identities/leaf-interrupted "$fixture/interrupted/.new-current"
mv -Tf "$fixture/interrupted/.new-current" "$fixture/interrupted/current"
run interrupted > "$fixture/interrupted-recovery.log" 2>&1
[[ ! -e $fixture/interrupted/activation-pending && ! -L $fixture/interrupted/activation-pending ]]
[[ $(sha256sum "$fixture/interrupted/current/cert.pem" | cut -d' ' -f1) == $(< "$fixture/interrupted/loaded") ]]
grep -q 'Interrupted TLS activation restored' "$fixture/interrupted-recovery.log"
echo 'PASS durable pending marker recovers interrupted activation before expiry no-op'
