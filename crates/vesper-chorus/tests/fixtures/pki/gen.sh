#!/bin/sh
# Regenerates the Chorus test PKI: one CA, one certificate per host
# (node0..node15.chorus.test, usable as TLS server and client), and a rogue
# CA with a rogue certificate for node1. ECDSA P-256, valid 100 years from
# generation (generated 2026-11-14). Requires OpenSSL 3. Run from this
# directory.
set -eu
T=$(mktemp -d)
mkca() { # name
  openssl req -x509 -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$T/$1.key" -out "$T/$1.pem" -subj "/CN=$1" -days 36500 \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign
  openssl x509 -in "$T/$1.pem" -outform DER -out "$1.der"
}
mkleaf() { # file ca san
  openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$T/$1.key" -out "$T/$1.csr" -subj "/CN=$3"
  printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth,clientAuth\nsubjectAltName=DNS:%s\n' "$3" > "$T/$1.ext"
  openssl x509 -req -in "$T/$1.csr" -CA "$T/$2.pem" -CAkey "$T/$2.key" -CAcreateserial \
    -days 36500 -extfile "$T/$1.ext" -out "$T/$1.pem" 2>/dev/null
  openssl x509 -in "$T/$1.pem" -outform DER -out "$1.der"
  openssl pkcs8 -topk8 -nocrypt -in "$T/$1.key" -outform DER -out "$1.key.der"
}
mkca chorus-ca
mkca rogue-ca
i=0
while [ $i -lt 16 ]; do
  mkleaf node$i chorus-ca node$i.chorus.test
  i=$((i + 1))
done
mkleaf rogue-node1 rogue-ca node1.chorus.test
rm -rf "$T"
