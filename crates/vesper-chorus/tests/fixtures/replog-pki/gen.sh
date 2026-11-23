#!/bin/sh
# Fixed-size TLS signatures keep the replicated-log transport transcript
# reproducible. Run from this directory with OpenSSL 3.
set -eu
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

openssl req -x509 -new -newkey ed25519 -nodes \
  -keyout "$tmp/ca.key" -out "$tmp/ca.pem" -subj '/CN=Chorus replog test CA' \
  -days 36500 -addext basicConstraints=critical,CA:TRUE \
  -addext keyUsage=critical,keyCertSign,cRLSign
openssl x509 -in "$tmp/ca.pem" -outform DER -out ca.der

for i in 0 1 2 3 4 5 6; do
  name="node$i.chorus.test"
  openssl req -new -newkey ed25519 -nodes \
    -keyout "$tmp/node.key" -out "$tmp/node.csr" -subj "/CN=$name"
  printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth,clientAuth\nsubjectAltName=DNS:%s\n' "$name" > "$tmp/node.ext"
  openssl x509 -req -in "$tmp/node.csr" -CA "$tmp/ca.pem" -CAkey "$tmp/ca.key" \
    -set_serial "$((i + 1))" -days 36500 -extfile "$tmp/node.ext" \
    -outform DER -out "node$i.der"
  openssl pkcs8 -topk8 -nocrypt -in "$tmp/node.key" -outform DER -out "node$i.key.der"
done
