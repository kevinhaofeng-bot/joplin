#!/usr/bin/env bash
# Real OpenSSL validation of candidate issuance; no live certificate replacement.
set -euo pipefail
scripts=$(cd "$(dirname "$0")" && pwd -P)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/joplin-tls-issue.XXXXXXXX")
trap 'rm -rf "$fixture"' EXIT
umask 077
mkdir "$fixture/ca" "$fixture/wrong" "$fixture/expired"
make_ca() {
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -days "$2" -subj '/CN=Joplin Lite acceptance CA' \
    -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign' \
    -keyout "$1/key.pem" -out "$1/cert.pem" > /dev/null 2>&1
}
make_ca "$fixture/ca" 3650
make_ca "$fixture/wrong" 3650
make_ca "$fixture/expired" 1
ca_before=$(sha256sum "$fixture/ca/"*.pem)
bash "$scripts/issue-server-tls.sh" "$fixture/ca" "$fixture/leaf1" 365
openssl verify -CAfile "$fixture/ca/cert.pem" -purpose sslserver -verify_hostname localhost "$fixture/leaf1/cert.pem"
openssl verify -CAfile "$fixture/ca/cert.pem" -purpose sslserver -verify_ip 192.168.5.170 "$fixture/leaf1/cert.pem"
[[ $(stat -c '%a' "$fixture/leaf1") == 700 ]]
[[ $(stat -c '%a' "$fixture/leaf1/key.pem") == 400 ]]
echo 'PASS new identity has verified localhost/LAN SAN, server usage and private modes'
bash "$scripts/issue-server-tls.sh" "$fixture/ca" "$fixture/leaf2" 365
openssl verify -CAfile "$fixture/ca/cert.pem" "$fixture/leaf2/cert.pem"
[[ $(sha256sum "$fixture/leaf1/key.pem" | cut -d' ' -f1) != $(sha256sum "$fixture/leaf2/key.pem" | cut -d' ' -f1) ]]
[[ $(openssl x509 -in "$fixture/leaf1/cert.pem" -serial -noout) != $(openssl x509 -in "$fixture/leaf2/cert.pem" -serial -noout) ]]
[[ $(sha256sum "$fixture/ca/"*.pem) == "$ca_before" ]]
echo 'PASS renewal changes leaf key/serial, preserves the same client trust root'
before=$(sha256sum "$fixture/leaf1/"*.pem)
if bash "$scripts/issue-server-tls.sh" "$fixture/ca" "$fixture/leaf1" 365; then exit 1; fi
[[ $(sha256sum "$fixture/leaf1/"*.pem) == "$before" ]]
echo 'PASS existing identity refused and unchanged'
cp "$fixture/ca/cert.pem" "$fixture/wrong/cert.pem"
if bash "$scripts/issue-server-tls.sh" "$fixture/wrong" "$fixture/mismatch" 365; then exit 1; fi
[[ ! -e $fixture/mismatch ]]
echo 'PASS mismatched CA key cannot publish a candidate'
if bash "$scripts/issue-server-tls.sh" "$fixture/expired" "$fixture/too-long" 365; then exit 1; fi
[[ ! -e $fixture/too-long ]]
echo 'PASS CA cannot expire before the new leaf'
ln -s "$fixture/nonexistent" "$fixture/dangling"
if bash "$scripts/issue-server-tls.sh" "$fixture/ca" "$fixture/dangling" 365; then exit 1; fi
[[ -L $fixture/dangling && ! -e $fixture/nonexistent ]]
echo 'PASS dangling destination link refused without following it'
if bash "$scripts/issue-server-tls.sh" "$fixture/ca" "$fixture/zero" 0; then exit 1; fi
[[ ! -e $fixture/zero ]]
echo 'PASS zero lifetime rejected without publishing'
