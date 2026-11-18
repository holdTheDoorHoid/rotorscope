# Evidence log: what it does, what it proves, what it does not yet

PLAN.md Phase 13, first part (bench-safe). Status 2026-11-15: working on the
P4 bench hub with a **development key**. Not yet evidence-grade.

**Whether a court accepts any of this is the court's decision. Have a lawyer
review the method and these documents before relying on them.**

## In plain words

Every time the hub receives a detection from a node, it writes a record of
exactly what arrived (the raw bytes, so anyone can decode them again), what
the hub made of it, which node heard it and how strongly, where the hub was
and how good its GPS fix was, what time it was and where that time came
from, and which firmware was running. It also writes a record for anything
that could matter later: power-on, GPS gained or lost or jumping, the clock
being set or corrected, the SD card going in or out, nodes appearing or
going quiet, the airspace data version, settings changes, operator notes.

Each record contains a fingerprint (SHA-256) of the record before it. Change
one letter anywhere, remove a record or swap two, and every fingerprint
after that point stops matching. Once a minute the hub **signs** the latest
fingerprint with its key. A signature covers everything before it.

The records go to the SD card in their own folder (`ROTORSCP/EVIDENCE/`),
separate from the everyday CSV log, and stream out of the USB port. A laptop
program passes them to our server, which checks every fingerprint and
signature as they arrive, refuses anything that does not check out, and
keeps what it accepts in files it never changes. The server also keeps its
own fingerprinted log of who looked at, exported or annotated what.

An **export bundle** is one file with everything: the records exactly as
uploaded, the unit's public key, the firmware fingerprints, the server's
access log for them, and any outside time-stamps. The **verifier**
(`evidence-verify`, open source, runs on any laptop) checks all of it and
says, for any problem, exactly which file, record and byte.

## What is protected now

- **Changing, removing, reordering or cutting records** after the hub wrote
  them: caught, on the card, in an upload, on the server's disk and in a
  bundle (host tests, and on the bench 2026-11-15; see "Tested").
- **Changing the server's access log**: caught (it is chained the same way).
- **A batch altered on the way to the server**: refused on arrival.
- **Records the hub wrote to the card but that were removed later**: each
  new card file names the previous file's exact length and last record, so
  a shortened or extended earlier file shows.
- **No GPS**: records made without GPS time say "time source: none" and
  carry only time since power-on; records without a GPS position say so. The
  verifier counts and prints them. A typed-in position is marked "manual".
- **Outside time-stamp**: the server can have a public time-stamp authority
  (tested with DigiCert's free service, no account) sign a chain fingerprint,
  proving the records existed by that time. Only a fingerprint leaves the
  server. Off unless the server is started with `--tsa`.

## What is NOT protected yet (be plain about this in any report)

1. **Development key.** The signing key was made on a laptop and built into
   the firmware. Anyone with the firmware file, the laptop, or the unit
   (reading its flash) can sign records that look like this unit's. Every
   checkpoint says "DEVELOPMENT KEY, NOT EVIDENCE-GRADE", and the verifier
   repeats it. So today the signatures show the records **have not changed
   since they were signed**; they do not show **only this unit** could have
   signed them.
2. **No secure boot or flash encryption.** Someone with the unit could load
   firmware that writes false records. The firmware fingerprint in each
   record shows *which* firmware ran, but without secure boot nothing stops
   a different one from running.
3. **The last minute.** Records after the newest signature are chained but
   not yet signed. If someone cut those off the end of the last file, only
   the missing signature would show. The verifier reports them as "not
   covered by a signature".
4. **Time-stamps are not automatic.** Someone has to ask the server for one
   (button/API), and only while it is online.
5. **The re-decode shares our decoder.** The verifier decodes the raw bytes
   again, but with the same `rid` code the hub uses, so it shows the summary
   matches the bytes, not that our decoding is right. The raw Remote ID
   messages are standard ASTM F3411 messages, so the other side's expert can
   decode them with an independent decoder (e.g. opendroneid-core-c). A
   fully independent decoder in the verifier is future work.
6. **Laptop link.** The P4 has no network; a laptop carries the records.
   The laptop cannot forge them (it does not have the key), but it could
   withhold some; the server shows any gap.
7. **Bench transport.** Plain HTTP on one laptop is fine for the bench. Use
   the server's TLS option anywhere else.
8. **Access control on the server is a shared token**, and the name in the
   access log is whatever the person typed. Real accounts are later work.
9. **Over-the-air frames.** The node keeps the Remote ID messages, not the
   whole Wi-Fi or Bluetooth packet they came in.

## Privacy

Remote ID includes the pilot's location: personal data. The console stream
is on by default (it carries what the console JSON already did); the
**server upload is the owner's choice** and nothing uploads unless someone
runs the uploader. Open FPV link identifiers stay out of the summary text
(project policy) but are inside the raw bytes, as they must be for the
record to be complete. **Owner decision 2026-11-15: always keep them.** The
evidence log stores open-link frames' raw bytes in full, whatever the
`show-link-ids` setting; that setting only controls what screens, the phone
page, CSV, CoT and the summary text show. So anyone with the evidence log or
an export bundle can read link IDs; treat both as sensitive. Retention limits are still to be built
(PLAN Phase 13 item 6).

## Using it on the bench

On the hub's USB console (115200 baud):

| key | does |
|---|---|
| `n` then text, Enter | operator note into the log |
| `k` | signed checkpoint now |
| `r` | re-send the current card file over USB |
| `u` | console stream on/off (logged as a setting change) |
| `e` | "stop": shutdown event + signed checkpoint, flushed to the card. Remove the card after the next `hub-p4: evidence ...` line shows `queued` small and `flushes` up by one |

Laptop (from the repository):

```text
# once: a development key for this unit (keep it off git)
target/release/evidence-server keygen --out ~/p4-dev.evkey
cd firmware/hub-p4 && ROTORSCOPE_DEV_KEY=$(cat ~/p4-dev.evkey) cargo fw   # then flash as usual
# without ROTORSCOPE_DEV_KEY the hub uses the published test key (class "public test"),
#   and its screen says "TEST KEY: log not evidence" for as long as it runs (a dev key:
#   "DEV KEY: not evidence-grade"); the hello line names the class too
# a ROTORSCOPE_DEV_KEY that is set but not exactly 64 hex digits (or is zero) stops the
#   build (it used to fall back to the test key silently); the value is never printed

target/release/evidence-server serve --data ~/evidence-data          # http://127.0.0.1:8650
target/release/evidence-upload --server http://127.0.0.1:8650 \
    --serial /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0 --replay --register "bench P4"
#  check that the fingerprint it registers matches the one the hub prints at boot
#  ("hub-p4: evidence key ... fingerprint xxxx-xxxx-xxxx-xxxx")
curl -o bundle.json http://127.0.0.1:8650/api/devices/<fingerprint>/export
target/release/evidence-verify bundle.json
# or straight from the card in a card reader:
target/release/evidence-verify /media/<card>/ROTORSCP/EVIDENCE/
```

The map and list page is at the server's address. Every view, export and
note is written to the access log with the name you type.

Verifier exit codes: **0** verified (every record linked and signed),
**1** failed (it names the file, byte, chain and record), **2** could not
run, **3** nothing contradicts the log but some records are unsigned or
missing (listed).

## Tested

- Host: `cargo test -p evidence -p evidence-server` (16 tests): editing any of
  a spread of bytes in every record, deleting any record, swapping any two
  neighbours, cutting a record, rewriting the chain after an edit, files
  shortened or extended after the next file opened, missing files, batches
  altered in transit, re-uploads, gaps filled later, bundles with a record
  edited / deleted / truncated, a batch dropped or reordered, audit entries
  edited / dropped / reordered, the key swapped, a stored batch altered on
  the server's disk; the no-GPS marking.
- Bench, 2026-11-15, P4 CrowPanel (chip v1.3) with the `fakedrone` build,
  two real nodes via the gateway, no GPS attached: detections went hub ->
  USB -> laptop uploader -> server -> export bundle -> verifier: **VERIFIED**
  (255 records, 2 boots, 5 signed checkpoints). Copies of the bundle with a
  digit of a drone latitude changed, a record removed, two records swapped,
  an access-log name changed, or the last checkpoint removed: all
  **FAILED**, each naming the batch and record. Console replay of the card
  file gave the same bytes as the live stream. Over TLS (self-signed bench
  certificate) and with a DigiCert RFC 3161 time-stamp (`openssl ts -verify`:
  OK).

Cost on the P4: about 400 bytes per detection record (base64 on the console
about 550 bytes); building one takes up to 1.8 ms; signing a checkpoint
33 ms once a minute (software P-256 at 360 MHz); SD steps for the evidence
file at most about 18 ms (a flush, same as the CSV's); opening the evidence
file at mount about 50 ms (400 ms the very first time, when the folder is
created). Records queue in 256 KB of PSRAM (about 10 minutes of one
detection a second with no card). Main loop rate unchanged (about 520-550
passes a second with the fake drone).

## The hardware key (design; nothing burned)

**Do not do any of this without the owner's go-ahead for that specific
unit.** eFuses cannot be unburned. Never on the bench boards.

- **P4 (the evidence writer).** The ECDSA peripheral signs with a P-256 key
  held in an eFuse key block whose purpose is set to ECDSA_KEY and which is
  read-protected: software can ask for signatures but can never read the
  key. Plan: at first setup the firmware draws 256 random bits from the
  hardware RNG, burns them into a free key block with ECDSA_KEY purpose and
  read protection (so the key never exists outside the chip), then asks the
  peripheral for the public key ("export public key" mode) and prints it
  and its fingerprint for registration. A `HwSigner` implementing the
  `Signer` trait (`crates/evidence/src/sign.rs`) feeds the peripheral the
  SHA-256 digest it already computes; key class becomes 2 and nothing else
  in the format changes. To check first, on this pre-v3 (v1.3) silicon:
  that the ECDSA peripheral and its export mode work as documented; esp-hal
  has no P4 ECDSA driver yet (we would write a register-level one).
- **S3 / C5.** The S3's digital-signature (DS) peripheral is RSA only, so an
  S3 signer would need a new signature algorithm code (a format version 2).
  The C5 has an ECDSA peripheral like the P4's. Neither writes evidence
  today; the P4 does.
- **Secure boot v2 and flash encryption** on kit units: needs our own signed
  bootloader in place of Elecrow's, a signing key kept offline, and burning
  the secure-boot and encryption eFuses. With both, only our signed firmware
  runs and the flash cannot be read, which is what makes the firmware
  fingerprint in each record meaningful. Per-unit owner permission.

## Uploading through the web head (later)

The P4 has no network; the S3 web head has Wi-Fi station mode but almost no
free RAM (about 1 KB of stack margin with the web head features), so a TLS
upload client does not fit on it. Options, best first:

1. **A C5 as an uplink node** (384 KB RAM): the P4 sends batches to it over
   a UART or ESP-NOW, it uploads over TLS in station mode. Cleanest.
2. **The S3 as a pass-through**: a new link frame (protocol 7, chunked, since
   `MAX_FRAME` is 320 bytes) carries batches to the S3, which offers them on
   its phone page; the phone or laptop on the hotspot uploads them. No new
   client or buffers on the S3; the hub's signatures protect the content.
3. Plain-HTTP upload from the S3 (signed batches keep integrity but not
   confidentiality): only if 1 and 2 fail.

## Custody record (template)

One sheet per hand-over of the unit or its SD card. Keep with the export.

```text
Unit: ______________  key fingerprint: xxxx-xxxx-xxxx-xxxx  (printed at boot)
Firmware SHA-256 (first 16): ________________  SD card serial/label: ________

Date/time (UTC) | From (name, signature) | To (name, signature) | Item (unit / SD card / bundle file)
                |                        |                      | Condition / seal no.
----------------+------------------------+----------------------+------------------------------------
                |                        |                      |

Exports made:
Date/time (UTC) | By | Bundle file name | Bundle SHA-256 (from verifier or X-Bundle-SHA256) | Verifier result
```

## Methods document (outline, to write with the lawyer)

1. Purpose and scope: what the unit detects (Remote ID broadcasts, DJI
   DroneID, Wi-Fi drone networks, open FPV links) and what it does not.
2. Equipment: hub and node hardware, firmware versions and fingerprints,
   antennas, GPS receiver, the server and verifier versions.
3. How a detection is made: radio reception, node decoding, the link to the
   hub, what is stored (this document and EVIDENCE-FORMAT.md).
4. Time and position: GPS, PPS, free-running clock, how each record says
   which it used; known uncertainty of each.
5. Integrity: hash chain, signed checkpoints, key class, card-file linking,
   server checks, access log, outside time-stamps.
6. Known error rates and limits: bench and field test results (detection
   rates, false matches, range), "a detection shows a broadcast was received,
   not who was flying", spoofable broadcasts, development-key caveats while
   they apply.
7. Handling: setup, registration, custody record, export and verification
   steps, retention.
8. Validation: how to reproduce the tests; the verifier's own test suite.
9. Versions and change log.
