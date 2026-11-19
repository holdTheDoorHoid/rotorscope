# Canned RFC 3161 fixtures (test only)

Time-stamp replies from two **throwaway test authorities** (`tsa-a`,
`tsa-b`), made offline with `openssl` so the tests never touch a real
time-stamp service. Nothing here is trusted by anything outside the tests.

| file | what |
|---|---|
| `tsa-a/ca.pem`, `tsa-b/ca.pem` | each test authority's self-signed root (P-256, 100 years) |
| `tsa-a/tsa.pem`, `tsa-b/tsa.pem` | its time-stamping certificate (extendedKeyUsage timeStamping, critical) |
| `tsa-a/bundle.tsr` | TimeStampResp over `imprint.hex`, nonce `1122334455667788` |
| `tsa-b/bundle.tsr` | TimeStampResp over `imprint.hex`, nonce `0102030405060708` |
| `tsa-a/other.tsr` | TimeStampResp over SHA-256("not this log"), nonce `1122334455667788` |
| `imprint.hex` | the access-log imprint of the fixed test bundle (`tests/common/bundle.rs`, audit entry #2) |
| `make-fixtures.sh` | the script that made all of the above |

## How they were made

```sh
# 1. The imprint the fixed test bundle yields (deterministic):
cargo test -p evidence --test stamps -- --ignored --nocapture print_fixture_imprint
# 2. Two throwaway authorities and their replies:
crates/evidence/tests/fixtures/tsa/make-fixtures.sh <IMPRINT>
```

The script builds each request byte for byte as the server does
(`tools/evidence-server/src/tsa.rs`, SHA-256, a fixed 8-byte nonce, certReq
true), answers it with `openssl ts -reply`, and copies out only the
certificates and replies. The authorities' **private keys stay in a
temporary folder that the script deletes**: they were never committed, and
these fixtures cannot be extended; to add one, run the script again (which
makes new authorities and replaces every file here).

Made 2026-11-19 with OpenSSL 3.0.13. If `tests/common/bundle.rs` or the
record format changes, the imprint changes and the tests say so: re-run
both steps.

Check one by hand:

```sh
openssl ts -reply -in tsa-a/bundle.tsr -text
openssl ts -verify -in tsa-a/bundle.tsr -digest $(cat imprint.hex) -CAfile tsa-a/ca.pem
```
