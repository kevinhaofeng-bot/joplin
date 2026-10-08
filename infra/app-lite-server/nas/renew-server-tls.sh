#!/usr/bin/env bash
# Linux NAS: same-CA renewal with atomic selection and checked rollback.
# Usage: renew-server-tls.sh <managed-dir> <service-hook> [before-days=30] [days=365]
# Hook receives: restart|health <managed-dir>. health MUST pin the selected leaf.
set -euo pipefail
root=${1:?managed TLS directory}
hook=${2:?trusted restart/health hook}
before=${3:-30}
days=${4:-365}
scripts=$(cd "$(dirname "$0")" && pwd -P)
fail() { echo "TLS renewal refused: $*" >&2; exit 1; }
[[ $before =~ ^(0|[1-9][0-9]{0,2})$ && $days =~ ^[1-9][0-9]{0,2}$ ]] || fail 'invalid renewal policy'
(( before < days && days <= 825 )) || fail 'invalid renewal policy'
[[ $root == /* && -d $root && ! -L $root ]] || fail 'invalid managed directory'
root=$(cd "$root" && pwd -P)
[[ -f $hook && ! -L $hook ]] || fail 'invalid service hook'
for dir in ca identities; do [[ -d $root/$dir && ! -L $root/$dir ]] || fail 'invalid material directory'; done
[[ ! -L $root/renew.lock ]] || fail 'linked lock file'
umask 077
exec 9> "$root/renew.lock"
flock -n 9 || { echo 'TLS renewal already in progress' >&2; exit 75; }
[[ -L $root/current ]] || fail 'active identity must be a managed pointer'
for file in cert.pem key.pem; do
  [[ -f $root/ca/$file && ! -L $root/ca/$file ]] || fail 'missing or linked CA material'
done
validate_identity() {
  local identity=$1 cert_public key_public
  [[ $identity =~ ^identities/leaf-[A-Za-z0-9_-]+$ ]] || fail 'active pointer outside managed identities'
  [[ -d $root/$identity && ! -L $root/$identity ]] || fail 'invalid active identity'
  for file in cert.pem key.pem; do
    [[ -f $root/$identity/$file && ! -L $root/$identity/$file ]] || fail 'missing or linked identity material'
  done
  # An expired leaf is renewable; identity/usage/chain still must be correct.
  openssl verify -no_check_time -CAfile "$root/ca/cert.pem" -purpose sslserver -verify_hostname localhost "$root/$identity/cert.pem" >/dev/null
  cert_public=$(openssl x509 -in "$root/$identity/cert.pem" -pubkey -noout | openssl pkey -pubin -outform DER | openssl dgst -sha256)
  key_public=$(openssl pkey -in "$root/$identity/key.pem" -pubout -outform DER | openssl dgst -sha256)
  [[ -n $cert_public && $cert_public == "$key_public" ]] || fail 'active certificate/key mismatch'
}
nonce=$(openssl rand -hex 12)
pending="$root/.current-$nonce"
select_identity() { ln -s "$1" "$pending"; mv -Tf "$pending" "$root/current"; sync -f "$root"; }
hook_action() { timeout 30s bash "$hook" "$1" "$root"; }
marker="$root/activation-pending"
if [[ -e $marker || -L $marker ]]; then
  [[ -L $marker ]] || fail 'invalid pending activation marker'
  recovery=$(readlink "$marker")
  validate_identity "$recovery"
  select_identity "$recovery"
  if ! hook_action restart || ! hook_action health; then fail 'rollback health failed after interrupted activation'; fi
  rm "$marker"
  sync -f "$root"
  echo 'Interrupted TLS activation restored and pinned health verified'
fi
old=$(readlink "$root/current")
validate_identity "$old"
if openssl x509 -in "$root/$old/cert.pem" -checkend "$(( before * 86400 ))" -noout >/dev/null; then
  echo 'TLS identity outside renewal window; unchanged'
  exit 0
fi
new="identities/leaf-$nonce"
bash "$scripts/issue-server-tls.sh" "$root/ca" "$root/$new" "$days"
# Preserve container readability without exposing the private key to others.
chown --reference="$root/$old" "$root/$new"
for file in cert.pem key.pem; do chown --reference="$root/$old/$file" "$root/$new/$file"; done
activated=0
cleanup() {
  status=$?
  trap - EXIT
  [[ ! -L $pending ]] || rm "$pending"
  if (( activated )); then
    echo 'TLS activation failed; restoring previous identity' >&2
    if ! select_identity "$old"; then
      echo 'TLS rollback pointer failed; manual intervention required' >&2
    elif ! hook_action restart || ! hook_action health; then
      echo 'TLS rollback health failed; manual intervention required' >&2
    else
      rm "$marker"
      sync -f "$root"
      echo 'Previous TLS identity restored and healthy' >&2
    fi
    (( status != 0 )) || status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
ln -s "$old" "$marker"
sync -f "$root"
activated=1
select_identity "$new"
hook_action restart
hook_action health
rm "$marker"
sync -f "$root"
activated=0
echo 'New TLS identity activated and pinned health verified; previous identity retained'
