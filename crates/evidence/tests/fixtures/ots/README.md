# Synthetic OpenTimestamps fixtures (test only)

OpenTimestamps proofs and a Bitcoin block header, **all synthetic**, made
offline by `make-fixtures.py` (Python 3, its own `hashlib` only; no network,
no calendar, no Bitcoin node). They let the tests check the Rust code
(`crates/evidence/src/ots.rs`, `tools/evidence-server/src/ots.rs`) against
bytes it did not make: the script was written separately from the published
format (python-opentimestamps, `opentimestamps/core/op.py`, `timestamp.py`,
`notary.py`, `serialize.py`), and the Rust tests check that their own
builder produces exactly these bytes.

| file | what |
|---|---|
| `pending.ots` | a pending proof of `imprint` (the fixed test bundle's access-log imprint, `../tsa/imprint.hex`): append a fixed 16-byte salt, SHA-256, then a calendar's prepend / append / SHA-256 and a pending attestation for `https://alice.calendar.test` |
| `complete.ots` | the same proof completed: at the calendar's commitment, the pending attestation stays and a path follows through a calendar tree, a transaction (including a RIPEMD-160, as early real proofs have) and a block Merkle tree to a Bitcoin attestation at height 905000 |
| `submit-reply.bin` | what a calendar returns for the submitted digest (no file header) |
| `upgrade-reply.bin` | what a calendar returns later for its commitment |
| `headers.txt` | the 80-byte header "of block 905000": **not a real block**. Its Merkle root is the one `complete.ots` leads to; it was mined here at the easiest difficulty Bitcoin's main chain allows (bits `1d00ffff`), so it meets its stated target |
| `expected.txt` | the values the script computed (submitted digest, commitment, Merkle root, block hash, block time) |
| `make-fixtures.py` | the script that made all of the above |

## How they were made

```sh
# The imprint is the one the RFC 3161 fixtures use (../tsa/README.md).
python3 -I crates/evidence/tests/fixtures/ots/make-fixtures.py "$(cat crates/evidence/tests/fixtures/tsa/imprint.hex)"
```

Mining the header takes about 2^32 double SHA-256 attempts: about an hour
in Python on this laptop (no SHA instructions). The committed header was
found by a throwaway search program (Rust, the `sha2` crate, all cores,
38 minutes; not part of the project) given the 76 bytes that
`make-fixtures.py IMPRINT --root-only` prints; the files were then written,
and the header checked (its Merkle root, and that it meets its target), by
the script itself:

```sh
python3 -I crates/evidence/tests/fixtures/ots/make-fixtures.py "$(cat crates/evidence/tests/fixtures/tsa/imprint.hex)" \
  --header 00000020448dd6117ef8a27f6bf0bb7a6004dbe585f22827ee5bf117200d162f5576af7b09841e8efd37d2dc47addafd473758e66b4d9a5ec7fec25bd50db45f9a7fea5a3d8cfd6affff001d4e19a93d
```

Any header with that Merkle root that meets bits `1d00ffff` would do.

## What these do NOT show

That this code reads proofs made by the reference OpenTimestamps client or
by the real calendars. Nothing in this project may download a real proof,
so that is **unverified**. `tests/ots.rs` has an ignored test,
`reference_client_proof`, that checks a real proof once one is put in
`reference/` here (the test's comment says which files it needs).
