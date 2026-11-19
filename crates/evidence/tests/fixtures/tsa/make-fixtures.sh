#!/bin/sh
# Remake the canned RFC 3161 fixtures in this folder (see README.md).
# Needs only openssl (3.x). Talks to no network. The test authorities'
# private keys live in a temporary folder and are deleted at the end:
# nothing here can sign anything again, and no key is ever committed.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
imprint=${1:?usage: make-fixtures.sh IMPRINT_HEX (from: cargo test -p evidence --test stamps -- --ignored --nocapture)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# request IMPRINT NONCE OUT: a DER TimeStampReq exactly as the server builds
# it (SHA-256, the given 8-byte nonce, certReq true).
request() {
    printf '30430201013031300d060960864801650304020105000420%s0208%s0101ff' "$1" "$2" | xxd -r -p > "$3"
}

# authority NAME: a throwaway root CA and a time-stamping certificate it
# issues (P-256, valid 100 years so the fixtures do not expire).
authority() {
    d="$work/$1"; mkdir -p "$d"
    openssl ecparam -name prime256v1 -genkey -noout -out "$d/ca.key"
    openssl req -x509 -new -key "$d/ca.key" -sha256 -days 36500 -subj "/CN=Rotorscope test TSA root $1 (throwaway)" \
        -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" -out "$d/ca.pem"
    openssl ecparam -name prime256v1 -genkey -noout -out "$d/tsa.key"
    openssl req -new -key "$d/tsa.key" -subj "/CN=Rotorscope test TSA $1 (throwaway)" -out "$d/tsa.csr"
    printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,timeStamping\n' > "$d/ext"
    openssl x509 -req -in "$d/tsa.csr" -CA "$d/ca.pem" -CAkey "$d/ca.key" -CAcreateserial -sha256 -days 36500 -extfile "$d/ext" -out "$d/tsa.pem"
    echo 01 > "$d/serial"
    cat > "$d/ts.cnf" <<CNF
[ tsa ]
default_tsa = t
[ t ]
dir = $d
serial = $d/serial
crypto_device = builtin
signer_cert = $d/tsa.pem
certs = $d/ca.pem
signer_key = $d/tsa.key
signer_digest = sha256
default_policy = 1.3.6.1.4.1.99999.1
other_policies = 1.3.6.1.4.1.99999.2
digests = sha256
accuracy = secs:1
ordering = no
tsa_name = no
ess_cert_id_chain = no
ess_cert_id_alg = sha256
CNF
    mkdir -p "$here/$1"
    cp "$d/ca.pem" "$here/$1/ca.pem"
    cp "$d/tsa.pem" "$here/$1/tsa.pem"
}

# stamp NAME OUTFILE IMPRINT NONCE
stamp() {
    request "$3" "$4" "$work/req.tsq"
    openssl ts -reply -config "$work/$1/ts.cnf" -queryfile "$work/req.tsq" -out "$here/$1/$2" 2>/dev/null
}

other=$(printf 'not this log' | openssl dgst -sha256 -r | cut -d' ' -f1)
authority tsa-a
authority tsa-b
stamp tsa-a bundle.tsr "$imprint" 1122334455667788
stamp tsa-b bundle.tsr "$imprint" 0102030405060708
stamp tsa-a other.tsr "$other" 1122334455667788
echo "$imprint" > "$here/imprint.hex"
echo "made fixtures for imprint $imprint"
