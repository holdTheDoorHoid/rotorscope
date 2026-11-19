# Rotorscope evidence log: format, byte for byte

This document is enough to check a Rotorscope evidence log with your own
tools, without our code. The reference implementation is `crates/evidence`
(`format.rs`, `writer.rs`, `verify.rs`); if the two ever disagree, this
document says what was intended and the code has a bug.

Format version 1. All integers little-endian. "SHA-256" is FIPS 180-4.

## Where the files are

On the hub's SD card (FAT32): `ROTORSCP/EVIDENCE/EVnnnnnn.EVL`, six decimal
digits. The hub creates a new file each time it mounts the card (each boot,
and again after a card is reinserted), numbered one past the highest file
already there; when no number above it is free (an `EV999999.EVL` on the
card), the lowest free number, with no previous file to link to. Only
regular files count; a name taken by a folder is skipped. It never writes
to an existing file. A file is just records
back to back, no file header.

File numbers are **not** unique over the card's life: if the newest files
are deleted, the next mount reuses their numbers. The boot counter (below,
"Continuity across boots") is what numbers the boots.

The same folder holds the boot counter's two state files, `CHAINA.STA` and
`CHAINB.STA` (96 bytes each, below). They are rewritten as the hub runs;
they are not evidence files and are not signed.

## Record

| offset | size | field |
|---:|---:|---|
| 0 | 4 | magic `52 53 45 56` ("RSEV") |
| 4 | 1 | format version, `01` |
| 5 | 1 | kind: 1 event, 2 detection, 3 checkpoint |
| 6 | 2 | `len`: total record length in bytes, trailer included (u16) |
| 8 | 8 | boot id: random, chosen at power-on; names one chain |
| 16 | 8 | sequence number (u64), from 0 per boot id |
| 24 | 32 | SHA-256 of the previous record of this boot id (all zero for #0) |
| 56 | 8 | hub uptime, milliseconds since power-on (u64) |
| 64 | 8 | UTC, Unix milliseconds (u64); 0 = unknown |
| 72 | 1 | time source: 0 none, 1 free-running, 2 gps, 3 gps_pps, 4 untrusted |
| 73 | 1 | GPS receiver: 0 absent, 1 searching, 2 2D fix, 3 3D fix |
| 74 | 1 | hub position source: 0 none, 1 gps, 2 manual (typed in) |
| 75 | 1 | satellites used; 255 unknown |
| 76 | 4 | time age: ms since UTC was last taken from the GPS (u32); `FFFFFFFF` n/a |
| 80 | 4 | hub latitude, degrees x 10^7 (i32); 0 when position source is 0 |
| 84 | 4 | hub longitude, degrees x 10^7 (i32) |
| 88 | 4 | hub height above the WGS-84 ellipsoid, mm (i32); `80000000` unknown |
| 92 | 2 | horizontal accuracy, decimetres (u16, u-blox receivers); `FFFF` unknown |
| 94 | 2 | HDOP x 100 (u16, NMEA receivers); `FFFF` unknown |
| 96 | 4 | position age, ms (u32); `FFFFFFFF` n/a |
| 100 | 32 | firmware hash: SHA-256 of the firmware ELF file that was flashed |
| 132 | ... | body, by kind (below) |
| len-4 | 4 | `len` again (u32), so a reader can step backwards from the end |

**Record hash** = SHA-256 of all `len` bytes. Record *n+1* carries the hash
of record *n* at offset 24.

Time source meanings: **none**: no UTC since power-on (UTC field is 0, the
record carries uptime only). **free-running**: UTC carried forward on the
hub's crystal since the GPS last gave it (the time-age field says how
long). **gps**: UTC from the receiver's messages, within about 2.5 s of the
last one, not locked to the pulse-per-second line (tens of ms uncertain).
**gps_pps**: UTC anchored to the receiver's PPS edge. **untrusted**: the
GPS gave a time the hub did not trust (spoofing suspected with nothing
trusted to hold, or a time before the build date or the remembered time;
docs/GNSS-INTEGRITY.md); the UTC field is 0.

Body fields marked "lv" are a u16 length followed by that many bytes.

### Kind 1: event

| size | field |
|---:|---|
| 2 | event code (u16) |
| lv | text (UTF-8, for people) |
| lv | data (by code, below) |

| code | name | data |
|---:|---|---|
| 1 | boot | key class (1), public key (65), chip revision major (1), minor (1); from WP-14a also build date (u64, Unix s: when the build script ran, never later than the build), source commit (20, SHA-1, zero unknown), source tree (1: 0 clean, 1 changed tracked files, 2 unknown), boot counter source (1: 0 none, 1 SD card state file, 2 secure element), how the boot id was made (1: 0 hardware RNG alone, 1 mixed, see "Boot id"). Text: `hub-p4 <version> git <commit>[-dirty] last reset: <why>; bench: <features or none>`. Always and only record #0. |
| 2 | card_mounted | this file number (u32), previous file number (u32, 0 none), previous file length at mount (u32), SHA-256 of the previous file's last whole record (32), previous file status (1): 0 none, 1 ends on a whole record, 2 ends inside an incomplete record, 3 unreadable |
| 3 | card_lost | queued bytes that go to the next file (u32) |
| 4 | setting | new value (1); text says which setting |
| 5 | clock | old UTC (u64, 0 none), new UTC (u64), step ms (i64), PPS-locked (1) |
| 6 | gps_fix | (none) |
| 7 | gps_lost | (none) |
| 8 | gps_jump | distance m (u32), seconds (u32) |
| 9 | node_join | station address (6) |
| 10 | node_leave | station address (6) |
| 11 | pack | airspace pack build date, Unix s (u32), length (u32), pack format version (u16); from WP-14a also the SHA-256 of the pack file as loaded (32) |
| 12 | note | (none); the text is the operator's note |
| 13 | not_logged | frames received but not recorded (queue full) (u32) |
| 14 | shutdown | (none); a shutdown checkpoint follows |
| 15 | gps_receiver | 1 answering, 0 silent |
| 16 | o4_decrypted | session key ID (4), record selector (1); text is what it decrypted to, never the key |
| 17 | threat | kind (1, `link::ThreatKind` index), reporting node's station address (6); from WP-14a also episode (u32, 1 = the alarm entry's first), how it was raised (1: 0 first, 1 again after an earlier episode ended, 2 still sounding when its mute or snooze ended), raises within the last minute (1). Text describes it. One record per raise (see "Threat episodes" below). For a node's report the raw `Threat` frame is the record before it (a MAVLink alarm announced when its suspicion is backed may come later); hub-raised kinds (area_gnss, id_flood, blinded) have no frame; their address is the node they are about (blinded, id_flood of table `node`) or all zero |
| 18 | gnss_jamming | kind (1), level (i16); see docs/GNSS-INTEGRITY.md. Kind 0 = back to normal |
| 19 | gnss_spoofing | kind (1), level (i32); see docs/GNSS-INTEGRITY.md. Kind 0 = back to normal |
| 20 | spoof_suspect | check (1, `tracker::spoof::Check` code), confidence 0-100 (1); text names the track and the evidence (docs/SPOOFING.md) |
| 21 | link_forgery | claimed station address (6), count (u32) (reserved; forged ESP-NOW packets currently arrive as `threat` kind 15) |
| 22 | aircraft_conflict | level (1: 1 advisory, 2 caution, 3 warning), drone track id (u32), aircraft address (u32), aircraft source (1); text: the pair, distance, height difference, predicted closest approach |
| 23, 24 | (reserved) | taken by the acoustic work: 23 `sound`, 24 `sound_clip` (on the acoustic branch) |
| 25 | id_disputed | drone track id (u32 LE), what (1: bit 0 position, 1 Self-ID, 2 operator ID, 3 a second ID), other claim's source (1: 0 Remote ID, 1 DroneID, 2 DJI Wi-Fi, 3 control link, 4 decrypted O4, 255 none); text: two transmitters claim this ID, how far apart, neither verified (tracker `claims`, docs/SPOOFING.md) |
| 26 | threat_ended | kind (1), node station address (6), episode (u32), active ms (u64: first to last report of the episode), frames (u64), reports (u32), peak level (i8: dBm, dB above the floor for jamming), raises in the episode (u32), flags (1: bit 0 held as flapping, bit 1 its alarm entry was removed while it was still sounding), end records lost before this one (u32). Text: the same in words. Closes the latest `threat` record (17) of the same alarm |
| 27 | continuity | source (1: 1 SD card state file, 2 secure element), status (1: 0 continued, 1 fresh: no counter on the card, 2 the stored counter was unreadable, 3 the new counter could not be saved, 4 the store could not be read), boot counter (u64; 0 unknown), previous boot id (8; zero none), the previous boot's records known stored (u64: its next sequence number), SHA-256 of the last of them (32). Text: the same in words. See "Continuity across boots" |

Unknown codes are reported, not rejected (a newer firmware may add some).
Fields added at the end of an event's data by a later firmware are
ignored by an older reader: read the fields you know by position, and
accept data longer than that.

**Threat episodes** (code 17 and 26, from WP-14a). A threat (an alarm entry
on the hub: one kind from one node, against one target or band) is
*raised* when first reported, and *raised again* when reported after it
had ended. It *ends* after 10 s with no report. Each raise is a `threat`
record and each end of a raised episode a `threat_ended` record, so an
alarm tripped briefly and then used again for a real attack leaves two
episodes on record, not one. A threat that **flaps** is bounded: once it
has been raised 3 times within 60 s, its episode ends only after 60 s with
no report, and the raises in between are counted into that episode
(`raises`, flag "held") instead of each getting a record. The third raise's
text says the hold begins. Records per alarm entry are thus at most about
four a minute. A silenced (muted or snoozed) alarm is not raised and so
has no end record; its raw frames are logged as before.

### Continuity across boots

The hub keeps a **boot counter** and, for the latest boot, how far its
chain has reached the card (record count and the hash of the last record
flushed). Each boot records where it stands in a `continuity` event (27):
its counter and what it found about the boot before it. Where the counter
lives is named in the boot event (data byte 97) and in the event:

- **SD card state file** (source 1, today). Read, advanced and saved at
  the boot's first card mount, so the event comes after the `card_mounted`
  event, not at boot; a card with a different counter mounted later in the
  same boot gets its own `continuity` event. After each flush of the
  evidence file (at most every 10 s, and at once after an eject) the hub
  rewrites the state with this boot's record count and head. Two files,
  written in turn, so a power cut during a write leaves the other.
- **Secure element** (source 2, planned): a monotonic counter, read right
  after the boot event. Same event, same layout.

State file, 96 bytes: magic `52 53 43 53` ("RSCS"), version `01`, 3 zero
bytes, counter (u64), boot id (8), records stored (u64), hash of the last
(32), SHA-256 of bytes 0..63 (32: catches a torn write, not an edit). The
newer of the two valid files (higher counter, then more records) counts.

**What a checker does with it.** For a boot whose `continuity` event names
a previous boot with *n* records and head *h*: if that boot's records are
supplied, record *n-1* must be there and hash to *h*. A previous log that
ends before *n-1* was cut short (or not supplied in full), a different
record *n-1* was changed: both failures. A previous boot not supplied while
earlier boots are is a missing boot (a failure, or with `--allow-gaps` a
gap). The earliest boot supplied has its predecessor named, outside what
was checked. A previous boot's records after its own last checkpoint, up
to *n-1*, are covered by the next boot's signature once its `continuity`
record is signed (it names their head).

**What it cannot show.** The state file is on the card, which its holder
can rewrite or delete (the next boot then says "fresh" and starts again at
1). It shows that the boots supplied follow each other and that an older
boot's log was not cut after a later boot had recorded its head. It cannot
show that nothing came after the newest boot supplied (the newest files
deleted, a wiped card), nor protect the newest boot's last records before
the next boot starts. A secure-element counter that cannot go back, with
its current value read off the unit by whoever checks, closes the first.

### Boot id

Eight bytes naming one boot's chain; they must differ between boots. The
ESP32-P4's hardware random number generator is only documented as truly
random while a radio or an ADC feeds it noise, and the P4 has no radio, so
since WP-14a (boot data byte 98 = 1) the hub takes the first 8 bytes of
SHA-256 over, each labelled and length-prefixed: 64 bytes of the hardware
RNG read in 16 pieces, the CPU cycle counter and system timer sampled
between the pieces, the time since power-on, the reset cause, and the
chip's eFuse unique ID and base MAC. The construction is in
`crates/evidence/src/bootid.rs`. Two boots that share an id would merge
into one chain and fail the check (two different records #0).

### Kind 2: detection

| size | field |
|---:|---|
| 6 | node station address (zero for a node wired to the hub) |
| 1 | node's link RSSI to the gateway, dBm (i8); 127 n/a |
| 1 | node index on the hub (0 wired) |
| 1 | band: 0 unknown, 1 Wi-Fi 2.4 GHz, 2 Wi-Fi 5 GHz, 3 Bluetooth, 4 raw-capture RF (see frequency) |
| 1 | frame kind: 1 Remote ID, 2 open FPV link, 3 DJI DroneID, 4 RF signature, 5 DJI Wi-Fi beacon, 6 Wi-Fi drone network, 7 aircraft frame from an aero (LR2021) node, 8 drone control link (`Frame::Control`), 9 control-band energy window (`Frame::ControlEnergy`) |
| 2 | channel (u16); 0 unknown |
| 4 | frequency, 100 kHz units (u32); 0 unknown |
| 1 | signal RSSI at the node, dBm (i8); 127 n/a |
| 1 | raw format: 1 = one `link::Frame`, postcard-serialized, link protocol 6 |
| lv | raw bytes |
| lv | decode summary (UTF-8, at most 200 bytes) |

**Raw bytes, format 1.** The frame exactly as the node sent it to the hub,
minus the COBS framing and zero delimiter (`crates/link`, `Frame`; postcard
encoding: <https://postcard.jamesmunns.com/wire-format>). For Remote ID the
frame is variant 1 `Detection { transport, channel, rssi, mac, counter,
uptime_us, messages }` and each message is the 25-byte ASTM F3411 message as
received over the air, which anyone can decode with an F3411 decoder (e.g.
opendroneid-core-c). The node has already removed the Wi-Fi or Bluetooth
wrapper; the over-the-air frame itself is not kept (future work).

**Summary.** What the hub made of the raw bytes, e.g.
`remote_id wifi_beacon ch6 rssi-45 tx=02:52:53:0d:00:01 ctr=3 id=RSTEST-SD-0001 pos=39.8721000,-75.2407000 alt_hae=129.5 h=160.0 op=39.8829000,-75.2407000`.
It is a convenience: the raw bytes are the record.

### Kind 3: checkpoint

| size | field |
|---:|---|
| 1 | reason: 0 periodic (every 60 s), 1 shutdown, 2 requested |
| 1 | key class: 0 public test key, 1 development software key, 2 hardware key (eFuse), 3 hardware key (secure element chip) |
| 65 | public key, SEC1 uncompressed (`04` x y), NIST P-256 |
| 8 | record count: equals this record's own sequence number (u64) |
| lv | note (ASCII), e.g. `DEVELOPMENT KEY, NOT EVIDENCE-GRADE (software key in flash)` |
| 64 | signature r ‖ s, 32 bytes each, big-endian |

The signature is ECDSA P-256 with SHA-256 (`ecdsa-with-SHA256`) over the
record's bytes from offset 0 up to, not including, the signature. `s` is
always in the **low-S** form (s <= n/2, n the P-256 group order); a
signature with s > n/2 is refused even though plain ECDSA would accept it,
because anyone can turn (r, s) into (r, n - s) and so make a second valid
copy of a checkpoint. (Firmware built before 2026-11-18 did not normalise
`s`; about half its checkpoints are high-S and fail.) Those bytes
include the previous-record hash (the chain head), the count and the time,
so one valid checkpoint vouches for every record before it in its chain.

**Algorithm.** A checkpoint of format version 1 is ECDSA P-256 with
SHA-256, by definition: the record's version byte is its algorithm tag,
and the public key it carries says which key. The hub refuses to write a
version 1 checkpoint with any other signer. A later algorithm or key type
gets a new checkpoint version with its own layout; version 1 logs keep
verifying unchanged, and a checker that meets a version it does not know
reports it instead of guessing. Whatever form of `s` a signer returns (a
secure element may return either), the hub writes the low-S form.

The published test key (class 0) has secret scalar
SHA-256("rotorscope evidence public test key - not secret - do not trust").
A signature with it proves nothing; the reference verifier refuses it unless
told to accept it for testing.

The **key class** byte (here and in the boot event) is written by whoever
signs. It is a claim, not a proof: a key generated on any laptop can be
written with class 2.

## Checking a log by hand

1. Read records from the start of each file: check the magic, version,
   `len` and trailer. A file may end inside a record (power cut while
   writing); the hub writes that record again, whole, in its next file.
2. Group by boot id. In each group sequence numbers run 0, 1, 2, ... with no
   gaps; #0 is a boot event with an all-zero previous hash; every other
   record's previous hash is the SHA-256 of the record before it.
3. Check every checkpoint's signature with the public key named in the boot
   event; check its count equals its sequence number. Then confirm that
   key **from outside the log** (the key the unit printed at setup, kept
   with the custody record): the log names its own key, so a log that
   checks against the key it names shows only that it is self-consistent.
   Reject the published test key.
4. Records after the last checkpoint are linked but not yet signed.
5. Files: in file *n*'s card_mounted event, the previous file's length and
   last whole record must match file *n-1* exactly.
6. Boots: each `continuity` event's previous boot, record count and head
   must match that boot's log, and the counters must follow on (see
   "Continuity across boots").
7. Re-decode each detection's raw bytes and compare with the summary.
8. Time, with no outside reference (records with time source "none" or
   "untrusted" carry no UTC and are left out): within a boot, UTC minus
   uptime stays put except at a `clock` event (the hub logs every change of
   0.5 s or more, within a second or so of the records that show it) and
   drifts by no more than the hub's 20 ppm steering; no UTC is before the
   build date in the boot event (the hub never writes such time as GPS
   time); and a boot's first UTC is not before the previous boot's last.

None of these checks can show that the log is complete: the newest boot's
records after the last one copied off, the newest files deleted or a
reformatted card leave nothing behind to check. The boot counters show
whether boots are missing *between* the ones you have, and whether an older
boot's log was cut after the next boot started.

## Console stream

The hub prints each record on its USB console as one line:
`@EV1 ` + base64 (RFC 4648, padded) of the record + newline. Console command
`r` re-sends the current card file the same way.

## Export bundle

JSON, from evidence-server (`/api/devices/{id}/export`):

```text
{ "format": "rotorscope-evidence-bundle/1",
  "created_utc_ms": ..., "server": "...",
  "device": { "key_id", "public_key" (hex), "name", "registered_utc_ms" },
  "batches": [ { "n", "received_utc_ms", "sha256" (hex), "records" (base64) } ],
  "audit": [ { "entry": "<JSON text>", "hash": hex } ],
  "firmware_hashes": [hex],
  "timestamps": [ { "kind": "rfc3161", "authority", "imprint", "boot", "seq", "head", "token" (base64) } ] }
```

- **Batches** are the uploads exactly as received: records back to back,
  one boot's contiguous run, ending on a signed checkpoint.
- **Audit chain**: `hash` = SHA-256(previous entry's hash bytes ‖ `entry`
  bytes), the first entry's previous hash being 32 zero bytes. Entry 0 is
  the registration; each upload entry names its batch number and SHA-256.
  Actions: register, upload, view, annotate, export, timestamp.
- **key_id**: the first 8 bytes of SHA-256 of the public key, hex, grouped
  `xxxx-xxxx-xxxx-xxxx`.
- **device.public_key** is the key the server registered for the device.
  It is the server's word, not a confirmation of who signed: check the
  records against the unit's key obtained independently.
- **Time-stamps**: `imprint` = SHA-256(boot id ‖ seq as u64 LE ‖ record
  hash); `token` is the RFC 3161 TimeStampResp. Check it with
  `openssl ts -verify -digest <imprint> -in token.tsr -CAfile <roots>`.
- **Bundle hash**: SHA-256 of the bundle file. The server returns it in the
  `X-Bundle-SHA256` header and records it in the export audit entry that
  follows (that entry is not inside the bundle it describes).
