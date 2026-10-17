#!/usr/bin/env bash
# Make a throwaway private CA and a 127.0.0.1/localhost server certificate for the
# e2e line forwarders, plus a bundle (system roots + the private CA) that Codex and
# Superba trust through SSL_CERT_FILE / CODEX_CA_CERTIFICATE.
#   mkcert.sh OUT_DIR
set -euo pipefail
out=${1:?usage: mkcert.sh OUT_DIR}
mkdir -p "$out"
cd "$out"
if [[ -s bundle.pem && -s server.pem && -s server.key ]]; then
  exit 0
fi
openssl req -x509 -newkey rsa:2048 -nodes -days 7 -sha256 \
  -subj "/CN=Superba e2e throwaway CA" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -keyout ca.key -out ca.pem 2>/dev/null
openssl req -newkey rsa:2048 -nodes -sha256 -subj "/CN=127.0.0.1" \
  -keyout server.key -out server.csr 2>/dev/null
cat > server.ext <<'EXT'
basicConstraints=CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=IP:127.0.0.1,DNS:localhost
EXT
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
  -days 7 -sha256 -extfile server.ext -out server.pem 2>/dev/null
system=""
for f in /etc/ssl/certs/ca-certificates.crt /etc/pki/tls/certs/ca-bundle.crt /etc/ssl/cert.pem; do
  if [[ -s $f ]]; then system=$f; break; fi
done
{ [[ -n $system ]] && cat "$system"; cat ca.pem; } > bundle.pem
chmod 600 ca.key server.key
