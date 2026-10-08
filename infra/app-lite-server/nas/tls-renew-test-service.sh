#!/usr/bin/env bash
# Test-only service-manager boundary. Never installed as a runtime hook.
set -euo pipefail
action=$1
root=$2
case "$action" in
  restart)
    if [[ -f $root/restart-fails && $(readlink "$root/current") != identities/leaf-original ]]; then exit 1; fi
    sha256sum "$root/current/cert.pem" | cut -d' ' -f1 > "$root/loaded"
    ;;
  health)
    [[ ! -f $root/all-health-fails ]]
    if [[ -f $root/new-health-fails && $(readlink "$root/current") != identities/leaf-original ]]; then exit 1; fi
    [[ $(sha256sum "$root/current/cert.pem" | cut -d' ' -f1) == $(< "$root/loaded") ]]
    ;;
  *) exit 2 ;;
esac
