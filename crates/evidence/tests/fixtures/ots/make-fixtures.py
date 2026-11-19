#!/usr/bin/env python3
"""Remake the synthetic OpenTimestamps fixtures in this folder (README.md).

Written independently of the Rust code (crates/evidence/src/ots.rs) from the
published format (python-opentimestamps, opentimestamps/core/{op,timestamp,
notary,serialize}.py): it uses only Python's own hashlib. Talks to no
network and to no calendar. Every value is synthetic: no proof here was
made by a real calendar, and the block header was mined here, at the
easiest difficulty Bitcoin allows (bits 0x1d00ffff), so it is NOT a real
Bitcoin block.

usage: make-fixtures.py IMPRINT_HEX [--header HEX | --root-only]

  IMPRINT_HEX  the fixed test bundle's access-log imprint (../tsa/imprint.hex)
  --header HEX use this header (checked here: its Merkle root and its proof
               of work), instead of mining one
  --root-only  print the Merkle root and the first 76 header bytes, for a
               faster miner, and stop

Mining the header takes about 2^32 double-SHA-256 attempts: about an hour
in Python on six cores. A faster search (any tool that finds a 4-byte
nonce for the printed 76 bytes) can be used instead; this script then
checks the result with --header.
"""

import hashlib
import multiprocessing
import os
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
MAGIC = b"\x00OpenTimestamps\x00\x00Proof\x00\xbf\x89\xe2\xe8\x84\xe8\x92\x94"
PENDING_TAG = bytes.fromhex("83dfe30d2ef90c8e")
BITCOIN_TAG = bytes.fromhex("0588960d73d71901")

# Fixed values (a real stamp uses random ones).
SALT = bytes.fromhex("00112233445566778899aabbccddeeff")  # the server's nonce
CAL_PREPEND = bytes.fromhex("5f2a1c00")  # a calendar's own prefix
CAL_APPEND = bytes.fromhex("a1a2a3a4a5a6a7a8")  # and suffix
CAL_URI = b"https://alice.calendar.test"
TIP_SIBLING = hashlib.sha256(b"another submission in the calendar's tree").digest()
TX_PREFIX = bytes.fromhex("0100000001") + b"\x11" * 32 + bytes.fromhex("00000000") + bytes.fromhex("16") + b"\x6a\x14"
TX_SUFFIX = bytes.fromhex("ffffffff0100000000000000000000000000")
MERKLE_RIGHT = hashlib.sha256(b"a sibling transaction").digest()
MERKLE_LEFT = hashlib.sha256(b"another sibling transaction").digest()
HEIGHT = 500000  # below the checker's first height-based floor (600000): the synthetic header is at difficulty 1
HEADER_VERSION = 0x20000000
HEADER_PREV = hashlib.sha256(b"rotorscope synthetic previous block").digest()
HEADER_TIME = 1795001400  # 2026-11-18T..., after the fixture's uploads
HEADER_BITS = 0x1D00FFFF


def varuint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def varbytes(b):
    return varuint(len(b)) + b


def sha256(b):
    return hashlib.sha256(b).digest()


def ripemd160(b):
    return hashlib.new("ripemd160", b).digest()


# An op is (serialized bytes, function); a path is a list of ops ending in
# one attestation (each fixture proof here is a single path, except the
# merged one).
def append(x):
    return (b"\xf0" + varbytes(x), lambda m: m + x)


def prepend(x):
    return (b"\xf1" + varbytes(x), lambda m: x + m)


SHA256 = (b"\x08", sha256)
RIPEMD160 = (b"\x03", ripemd160)


def pending(uri):
    return PENDING_TAG + varbytes(varbytes(uri))


def bitcoin(height):
    return BITCOIN_TAG + varbytes(varuint(height))


def serialize_path(ops, attestation):
    """A timestamp that is one chain of ops ending in one attestation."""
    return b"".join(op for op, _ in ops) + b"\x00" + attestation


def replay(msg, ops):
    for _, f in ops:
        msg = f(msg)
    return msg


def dsha(b):
    return sha256(sha256(b))


def header_bytes(root, nonce, time=HEADER_TIME):
    return struct.pack("<I", HEADER_VERSION) + HEADER_PREV + root + struct.pack("<III", time, HEADER_BITS, nonce)


def target(bits):
    exp, mant = bits >> 24, bits & 0x007FFFFF
    return mant << (8 * (exp - 3))


def meets(h):
    return int.from_bytes(dsha(h), "little") <= target(HEADER_BITS)


def mine_part(args):
    root, k, step = args
    # Each worker owns its own header time (k seconds later), so no two
    # search the same headers; the first 64 bytes are hashed once (midstate).
    time = HEADER_TIME + k
    head = header_bytes(root, 0, time)
    mid = hashlib.sha256(head[:64])
    tail = head[64:76]
    t = target(HEADER_BITS)
    for nonce in range(0, 1 << 32):
        h = mid.copy()
        h.update(tail + struct.pack("<I", nonce))
        if int.from_bytes(hashlib.sha256(h.digest()).digest(), "little") <= t:
            return head[:76] + struct.pack("<I", nonce)
    return None


def mine(root):
    n = max(1, (os.cpu_count() or 2) - 2)
    with multiprocessing.Pool(n) as p:
        for r in p.imap_unordered(mine_part, [(root, k, n) for k in range(n)]):
            if r:
                p.terminate()
                return r
    raise SystemExit("no header found")


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    imprint = bytes.fromhex(sys.argv[1])
    assert len(imprint) == 32
    header_hex = None
    if "--header" in sys.argv:
        header_hex = sys.argv[sys.argv.index("--header") + 1]

    # What the server sends: SHA-256(imprint || salt).
    server_ops = [append(SALT), SHA256]
    submitted = replay(imprint, server_ops)
    # The calendar's reply to that submission: its own ops to its commitment,
    # then a pending attestation naming itself.
    cal_ops = [prepend(CAL_PREPEND), append(CAL_APPEND), SHA256]
    commitment = replay(submitted, cal_ops)
    submit_reply = serialize_path(cal_ops, pending(CAL_URI))
    # Later: from the commitment, through the calendar's tree, a transaction
    # (here committing to a RIPEMD-160 of the tip, as early proofs do) and
    # the block's Merkle tree, to the block's Merkle root.
    up_ops = [
        append(TIP_SIBLING), SHA256,
        RIPEMD160, prepend(TX_PREFIX), append(TX_SUFFIX), SHA256, SHA256,  # txid
        append(MERKLE_RIGHT), SHA256, SHA256,
        prepend(MERKLE_LEFT), SHA256, SHA256,
    ]
    root = replay(commitment, up_ops)
    upgrade_reply = serialize_path(up_ops, bitcoin(HEIGHT))

    head = b"".join(op for op, _ in server_ops + cal_ops)
    file_head = MAGIC + b"\x01" + b"\x08" + imprint
    pending_ots = file_head + head + b"\x00" + pending(CAL_URI)
    # The upgraded proof: at the commitment the pending attestation stays and
    # the path to the block follows ("\xff" before every item but the last;
    # attestations before ops).
    complete_ots = file_head + head + b"\xff\x00" + pending(CAL_URI) + upgrade_reply

    if "--root-only" in sys.argv:
        # For a faster miner: the Merkle root the header must carry, and the
        # first 76 header bytes (the nonce follows).
        print("merkle_root", root.hex())
        print("header76", header_bytes(root, 0)[:76].hex())
        return
    if header_hex:
        header = bytes.fromhex(header_hex)
        assert header[36:68] == root, "the given header does not carry this Merkle root: mine again"
    else:
        header = mine(root)
    assert len(header) == 80 and meets(header), "header does not meet its target"
    block_hash = dsha(header)[::-1].hex()
    time = struct.unpack("<I", header[68:72])[0]

    def w(name, data):
        with open(os.path.join(HERE, name), "wb") as f:
            f.write(data)

    w("pending.ots", pending_ots)
    w("complete.ots", complete_ots)
    w("submit-reply.bin", submit_reply)
    w("upgrade-reply.bin", upgrade_reply)
    w("headers.txt", (
        "# Synthetic Bitcoin block header for the test proofs (NOT a real block).\n"
        "# height  80-byte header, hex\n"
        f"{HEIGHT} {header.hex()}\n").encode())
    w("expected.txt", (
        f"imprint {imprint.hex()}\n"
        f"salt {SALT.hex()}\n"
        f"submitted {submitted.hex()}\n"
        f"commitment {commitment.hex()}\n"
        f"merkle_root {root.hex()}\n"
        f"height {HEIGHT}\n"
        f"block_hash {block_hash}\n"
        f"block_time {time}\n").encode())
    print("block hash", block_hash, "time", time)


if __name__ == "__main__":
    main()
