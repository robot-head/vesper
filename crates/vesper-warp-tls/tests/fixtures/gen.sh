#!/bin/sh
# Regenerates the test PKI (ECDSA P-256, valid 100 years from generation; generated 2026-11-12).
# Requires OpenSSL 3. Run from this directory.
set -eu
T=$(mktemp -d)
mkca() { # name
  openssl req -x509 -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$T/$1.key" -out "$T/$1.pem" -subj "/CN=$1" \
    -days 36500 \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign
  openssl x509 -in "$T/$1.pem" -outform DER -out "$1.der"
}
mkleaf() { # name ca san
  openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$T/$1.key" -out "$T/$1.csr" -subj "/CN=$1"
  printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:%s\n' "$3" > "$T/$1.ext"
  openssl x509 -req -in "$T/$1.csr" -CA "$T/$2.pem" -CAkey "$T/$2.key" -CAcreateserial \
    -days 36500 -extfile "$T/$1.ext" -out "$T/$1.pem"
  openssl x509 -in "$T/$1.pem" -outform DER -out "$1.der"
  openssl pkcs8 -topk8 -nocrypt -in "$T/$1.key" -outform DER -out "$1.key.der"
}
mkca test-ca
mkca rogue-ca
mkleaf server test-ca warp.test
mkleaf rogue-server rogue-ca warp.test
rm -rf "$T"
