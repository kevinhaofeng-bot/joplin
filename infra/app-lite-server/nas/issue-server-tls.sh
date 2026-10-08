#!/usr/bin/env bash
# Issue a new identity under an existing private CA. Never replaces live TLS.
# NAS OpenSSL 3: issue-server-tls.sh <ca-dir> <new-identity-dir> [days=365]
set -euo pipefail
ca=${1:?private CA directory}
output=${2:?new identity directory}
days=${3:-365}
fail() { echo "TLS candidate refused: $*" >&2; exit 1; }
[[ $days =~ ^[1-9][0-9]{0,2}$ ]] && (( days <= 825 )) || fail 'invalid lifetime'
[[ -d $ca && ! -L $ca ]] || fail 'invalid CA directory'
for file in cert.pem key.pem; do
  [[ -f $ca/$file && ! -L $ca/$file ]] || fail 'missing or linked CA material'
done
[[ ! -e $output && ! -L $output ]] || fail 'destination already exists'
parent=$(cd "$(dirname "$output")" && pwd -P)
name=$(basename "$output")
[[ $name != . && $name != .. && -n $name ]] || fail 'invalid destination'
output="$parent/$name"
openssl verify -check_ss_sig -CAfile "$ca/cert.pem" "$ca/cert.pem" > /dev/null
openssl x509 -in "$ca/cert.pem" -checkend "$(( (days + 1) * 86400 ))" -noout > /dev/null || fail 'CA expires too soon'
cert_public=$(openssl x509 -in "$ca/cert.pem" -pubkey -noout | openssl pkey -pubin -outform DER | openssl dgst -sha256)
key_public=$(openssl pkey -in "$ca/key.pem" -pubout -outform DER | openssl dgst -sha256)
[[ -n $cert_public && $cert_public == "$key_public" ]] || fail 'CA key does not match certificate'
umask 077
staging=$(mktemp -d "$parent/.tls-candidate.XXXXXXXX")
trap 'rm -rf "$staging"' EXIT
openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -subj '/CN=Joplin Lite sync' \
  -addext 'subjectAltName=DNS:localhost,DNS:kevin-nas.local,IP:127.0.0.1,IP:192.168.5.170' \
  -addext 'basicConstraints=critical,CA:FALSE' \
  -addext 'keyUsage=critical,digitalSignature' \
  -addext 'extendedKeyUsage=serverAuth' \
  -keyout "$staging/key.pem" -out "$staging/request.pem" > /dev/null 2>&1
serial=$(openssl rand -hex 16)
openssl x509 -req -in "$staging/request.pem" -CA "$ca/cert.pem" -CAkey "$ca/key.pem" \
  -set_serial "0x$serial" -days "$days" -sha256 -copy_extensions copy \
  -out "$staging/cert.pem" > /dev/null 2>&1
openssl verify -CAfile "$ca/cert.pem" -purpose sslserver -verify_hostname localhost "$staging/cert.pem" > /dev/null
openssl verify -CAfile "$ca/cert.pem" -purpose sslserver -verify_ip 192.168.5.170 "$staging/cert.pem" > /dev/null
chmod 400 "$staging/key.pem" "$staging/cert.pem"
rm "$staging/request.pem"
# -T treats output as the destination itself, never as an existing directory.
# This is a candidate publication only; callers do not give it a live TLS path.
mv -T -n "$staging" "$output"
[[ ! -d $staging ]] || fail 'destination appeared during issuance'
trap - EXIT
echo 'Verified new TLS candidate published; existing CA and live identity unchanged'
