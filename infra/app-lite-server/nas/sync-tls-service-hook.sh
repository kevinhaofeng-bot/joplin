#!/usr/bin/env bash
# Root-only operational hook. No token required: verified unauthenticated401.
# Pin the NEW leaf public key as well as the CA: an old Docker bind must fail.
set -euo pipefail
action=${1:?restart or health}
root=${2:?managed TLS directory}
unit=${APP_LITE_TLS_RESTART_UNIT:-joplin-lite-sync.service}
port=${APP_LITE_TLS_HEALTH_PORT:-8787}
[[ $unit =~ ^joplin-lite-sync(-[a-z0-9-]+)?\.service$ ]]
[[ $port =~ ^[1-9][0-9]{0,4}$ ]] && (( port <= 65535 ))
case "$action" in
  restart) systemctl restart "$unit" ;;
  health)
    public_pin=$(openssl x509 -in "$root/current/cert.pem" -pubkey -noout | \
      openssl pkey -pubin -outform DER | openssl dgst -sha256 -binary | openssl base64 -A)
    [[ -n $public_pin ]]
    for _ in $(seq 10); do
      status=$(curl --silent --show-error --connect-timeout 1 --max-time 1 \
        --cacert "$root/ca/cert.pem" --pinnedpubkey "sha256//$public_pin" \
        -o /dev/null -w '%{http_code}' "https://localhost:$port/v1/pull" 2>/dev/null) || status=failed
      if [[ $status == 401 ]]; then echo 'Selected TLS identity verified; authentication required'; exit 0; fi
      sleep 0.25
    done
    echo 'Selected TLS identity health failed (CA, leaf pin, endpoint or authentication)' >&2
    exit 1
    ;;
  *) echo 'Unknown TLS service action' >&2; exit 2 ;;
esac
