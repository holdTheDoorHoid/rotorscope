# Remote ID

**Level: 🟢 Beginner, then 🟡 Intermediate and 🔴 Expert** · What Remote ID is, what each of its
messages says, the five radio forms it takes and which Rotorscope listener hears each, how the
decoder is tested, how far each part has been proven, and what Remote ID cannot tell you.

## 🟢 In plain words

**Remote ID** is a short message a drone broadcasts about once a second, over ordinary Wi-Fi
and Bluetooth, saying who it is and where it is. People call it a "digital licence plate". It is
**public by design**: it exists so that anyone on the ground can find out what is flying overhead.

Which rules require it, as the repository describes them:

- **The standard.** The messages are defined by ASTM F3411. Europe's standard, ASD-STAN
  EN 4709-002, uses the same message layouts, and Rotorscope's decoder reads both, whatever
  protocol version a drone announces (its test encoder writes the 2022 edition, F3411-22a).
- **The United States.** The FAA's Remote ID rule is
  [14 CFR Part 89](https://www.ecfr.gov/current/title-14/chapter-I/subchapter-F/part-89/subpart-F).
  It makes most drones broadcast Remote ID, and requires their serial numbers to follow the
  ANSI/CTA-2063-A format. One exception the code knows about: at an FAA-recognised identification
  area (FRIA) a drone may fly without it ([FAA rule flags](../use/faa-flags.md)).
- **China.** GB 46750-2025 is a different broadcast format, not compatible with F3411. Reports say
  DJI's 2026 firmware sends it inside the same Wi-Fi element as ASTM Remote ID, so Rotorscope
  decodes it too.

What a listener learns: the drone's serial number (or registration), what kind of aircraft it is,
its position, height and speed, and the position of the pilot or the take-off point. Some drones
also send a line of free text and the operator's registration number.

**What Rotorscope has proven.** As of 2026-11-18, Rotorscope has **not yet heard a real drone**.
Every Remote ID form is **proven on the air**: real boards decoded it from the project's own
bench transmitter, `rid-beacon`, which sends a plainly fake drone (`TESTDRONE0000000001`) at
the lowest power, all the way into the hub's track table ([bench tests](../BENCH.md)).

## 🟢 What Remote ID does not tell you

- **Whether it is true.** Remote ID has no proof attached. Anyone with a cheap Wi-Fi chip can
  broadcast a convincing fake drone, or copy a real drone's serial number. Rotorscope checks
  each drone's story against physics and against what else it hears, but it cannot prove a
  broadcast genuine. See [Spoofed drones](../SPOOFING.md).
- **About drones that do not send it.** Home-built, FPV and older drones, or any drone with
  Remote ID off, send nothing here. Other parts of Rotorscope listen for their other radios: [DJI DroneID](dji-droneid.md), [FPV video links](fpv.md) and
  [control links](../CONTROL-LINKS.md).
- **Who the pilot is.** A serial number is not a name, and Rotorscope never looks anything up
  online. Whether a flight is legal is not something it decides either
  ([FAA rule flags](../use/faa-flags.md)).

## 🟡 The messages

Every Remote ID message is exactly 25 bytes. Its first half-byte says which of six types it is.
The fields below are what `crates/rid` decodes; every "unknown" value the standard defines becomes
"no value" rather than a wrong number.

| Message | What it carries |
|---|---|
| **Basic ID** | The ID type (ANSI/CTA-2063-A serial number, civil aviation registration, a UTM-assigned ID, or a specific session ID), the aircraft type (aeroplane, multirotor, glider and so on), and the 20-byte ID itself. A drone may send two, for example a serial number and a registration. |
| **Location** | Latitude and longitude, pressure and GPS altitude, height above take-off or ground, speeds, direction, status (ground, airborne, emergency), accuracies, and the time to a tenth of a second. |
| **Authentication** | A signature, split over up to 16 pages. Rotorscope **does not verify signatures**; it only notices pages that contradict themselves. |
| **Self-ID** | Up to 23 characters of free text, such as the purpose of the flight. |
| **System** | Where the operator (or take-off point) is, group-operation area figures, the EU category and class, and a timestamp. |
| **Operator ID** | The operator's registration, for example an EU operator number. |
| **Message Pack** | Not a seventh kind of information: a wrapper holding up to 9 of the messages above in one transmission. |

A Bluetooth 4 advert has room for only one message, so a drone on Bluetooth 4 sends them one at a
time in turn. Every other transport can send a Message Pack.

## 🟡 The five transports, and who hears each

A **transport** is the radio form a message travels in. Rotorscope's listener boards are called
**nodes** ([glossary](../project/glossary.md)); the hub tells each node which job to do.

| Transport | Band and channels | How it is marked | Node that hears it |
|---|---|---|---|
| **Wi-Fi beacon** | 2.4 GHz, any channel, mostly 6 (also 1 and 11); 5 GHz, anchor channel 149, also 153-165 and 36-48 | A vendor element with the code `FA:0B:BC`, type `0x0D` | Wi-Fi 2.4 (ESP32-S3, or ESP32-C5) and Wi-Fi 5 (ESP32-C5 only) |
| **Wi-Fi NAN** | 2.4 GHz channel 6 only; 5 GHz channel 149 | An action frame sent to `51:6f:9a:01:00:00` | The same Wi-Fi nodes |
| **Bluetooth 4 legacy** | Advertising channels 37, 38, 39 | Service data `0xFFFA`, application code `0x0D`; one message per advert | Bluetooth node (S3 or C5) |
| **Bluetooth 5 extended** | Announced on 37-39, data on channels 0-36 | The same service data, in a longer advert that can carry a Message Pack | Bluetooth node |
| **Bluetooth 5 Long Range** (coded PHY) | Same channels, slower and more robust coding | The same | Bluetooth node |

A lone 2.4 GHz node spends three quarters of its time on channel 6 and visits 1 and 11; a lone
5 GHz node sits on 149 between short visits to 153-165 and 36-48. With more nodes, one parks on
the anchor channel and the others sweep (`crates/coverage/src/plans.rs`). The Bluetooth node
alternates the two Bluetooth radio modes, 100 ms each, so it hears all three Bluetooth transports.

On a laptop, `rid-view --ble` hears Bluetooth 4 only; a Sonoff dongle flashed with Sniffle hears
all three Bluetooth transports ([Sonoff dongle](../SONOFF.md)).

## 🟡 Two formats that are not ASTM

`crates/rid` also decodes two broadcasts that are not ASTM Remote ID:

- **DJI's legacy Wi-Fi DroneID.** Older Wi-Fi DJI drones (Spark, Mavic Air, Mini SE and others)
  put a vendor element with the code `26:37:12` in their Wi-Fi beacons, carrying a serial number,
  the drone's position and the pilot's. The hub merges it with the same drone's Remote ID by
  serial. **Simulated only** (hand-built frames); Rotorscope never transmits it.
- **China's GB 46750-2025.** One variable-length packet whose flag bits announce which fields
  follow, decoded exactly as core-c's Chinese module (`libopendroneidcn`) does.
  **Simulated only**: testing it on the air needs a test-beacon mode or a DJI drone on 2026
  firmware.

DJI's OcuSync DroneID, a different radio burst altogether, has its own page:
[DJI DroneID](dji-droneid.md).

## 🟡 How the decoder is tested

The reference everyone uses is **opendroneid-core-c**, the official open-source C library. A copy
lives in `third_party/` and is used only by the laptop tests. `crates/rid` is compared against it
field by field (`crates/rid/tests/`):

| Test | Cases per run | What must hold |
|---|---|---|
| Random valid messages of each type, encoded by core-c | 50,000 each for Basic ID, Authentication, Self-ID and Operator ID; 250,000 each for Location and System | Both decoders agree on every field; Rotorscope's encoder makes the same bytes as core-c's |
| Random bytes under each message type | 1,000,000 (over 300,000 decode) | Both decode them the same way |
| Message Packs built by core-c, and random pack-shaped bytes | 50,000 and 200,000 | The same messages accepted and rejected |
| Whole beacon and NAN frames, each side reading the other's | 20,000 per test | Extracted and decoded the same |
| GB 46750 packets against core-c's Chinese decoder | 50,000 encoded, 300,000 structured random | Same packets accepted and rejected, same fields |
| GB 46750 random bytes | 1,000,000 | Nothing crashes |
| Garbage: random bytes and mutated real frames through every entry point | 2,000,000 per test by default | Nothing crashes or hangs; whatever decodes re-encodes cleanly |

All of this is **simulated only**: it proves the decoder agrees with the reference, not that a
radio delivered the frame. The radios are proven separately, below.

## 🟡 Proof level, transport by transport

From the [README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/README.md) and
[bench tests](../BENCH.md) (first results 2026-11-12; through the P4 hub 2026-11-15). The
transmitter is always the project's own `rid-beacon`, or the laptop's Bluetooth 4 test advert.

| Transport | ESP32-S3 listener | ESP32-C5 listener | Through the P4 hub's tracker |
|---|---|---|---|
| Wi-Fi beacon, 2.4 GHz | **Proven on the air** | **Proven on the air** (2.4 GHz build) | **Proven on the air**, channel 6 |
| Wi-Fi NAN, 2.4 GHz | **Proven on the air** | **Proven on the air**, from a C5 and from an S3 | **Proven on the air**, channel 6 |
| Wi-Fi beacon, 5 GHz | cannot (no 5 GHz radio) | **Proven on the air**, channel 149 | **Proven on the air**, channel 149 |
| Wi-Fi NAN, 5 GHz | cannot | **Proven on the air**, channel 149 | **Proven on the air**, channel 149 |
| Bluetooth 4 legacy | **Proven on the air** (laptop advert and C5 beacon) | **Proven on the air** | **Proven on the air** |
| Bluetooth 5 extended | **Proven on the air** (from a C5) | **Proven on the air** | **Proven on the air** |
| Bluetooth 5 Long Range | **Proven on the air** (from a C5) | **Proven on the air** | **Proven on the air** |
| GB 46750 | **Simulated only** | **Simulated only** | **Simulated only** |
| DJI legacy Wi-Fi DroneID | **Simulated only** | **Simulated only** | **Simulated only** |

Through the hub, each transport reached the track table within 1.6 to 2.8 seconds, as one track.
None of this is **heard for real**: range against a real drone is not yet known.

## 🔴 Frame-parsing notes worth knowing

These are recorded in the code and the node READMEs; each was learned on the bench or from
another project's measurements.

- **The 4 bytes at the end of a promiscuous Wi-Fi frame are not the frame.** esp-radio sizes the
  packet by `sig_len`, which includes the FCS slot, and those 4 bytes are not a valid CRC.
  `firmware/node-wifi` drops them before parsing (`chip::FCS_LEN`). Checked on an S3: 2,886 of
  2,886 beacons in 60 s had their element list end exactly at `sig_len - 4`. The node keeps
  counting this (`iefit` on its stats line) in case another chip or driver differs.
- **Check frame control yourself.** The callback drops anything that is not a management frame by
  reading frame control, as well as setting the driver's management-only filter. The parser
  rejects protected frames and, when the Order bit is set, allows for the 4-byte HT Control field
  (a 28-byte header instead of 24).
- **A cheap pre-check first.** The node only calls the full extractor on beacons that contain the
  bytes `FA 0B BC 0D` and on action frames addressed to the NAN Remote ID address.
- **NAN is parsed by structure, not fixed offsets.** The extractor walks the NAN attributes to
  the Service Descriptor with the Remote ID service ID and skips whatever optional fields its
  Service Control bits announce, since real transmitters may add fields core-c's frames do not.
- **Bluetooth: one extended scan for everything.** The node runs an extended scan on the 1M and
  coded PHYs together, passive, with the duplicate filter **off** (repeated adverts carry fresh
  positions). Legacy Bluetooth 4 adverts arrive as extended reports with the legacy bit set
  (verified on the S3: the old-style report counter stays at 0). Chained extended adverts are
  reassembled when a pack does not fit in one HCI event. The transport is decided from the report:
  legacy PDU, extended on 1M, or coded on either leg. HCI does not report which advertising
  channel was used, so Bluetooth sightings carry channel 0.
- **A controller reset under Long Range traffic.** With ESP-NOW running, `node-ble` reset when
  the controller ended a timed scan while coded-PHY reports were in flight. The host now stops
  each 800 ms scan itself: 0 resets in 180 s ([bench tests](../BENCH.md)).
- **Lenient where the standard reserves values.** An unknown code becomes `Reserved(n)`, not an
  error, so a newer transmitter still yields everything an older receiver understands. The only
  hard errors are an unknown message type and a self-contradicting Authentication page.
- **Raw messages travel, not decoded ones.** Nodes forward the raw 25-byte messages, and every
  board decodes with the same crate, so every chip agrees on what a byte means.

## Where this lives in the code

- [`crates/rid`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/rid): frame
  extraction (`wifi.rs`, `ble.rs`), messages (`message.rs`), encoder (`encode.rs`), DJI legacy
  Wi-Fi (`dji.rs`), GB 46750 (`cn.rs`), and the tests against core-c (`tests/`)
- [`firmware/node-wifi`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/node-wifi)
  and [`firmware/node-ble`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/node-ble):
  the listeners
- [`firmware/rid-beacon`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/rid-beacon):
  the bench-only test transmitter
- [`crates/coverage/src/plans.rs`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/crates/coverage/src/plans.rs):
  channel plans
- Deep dives: [Signal catalogue](../SIGNALS.md), [Spoofed drones](../SPOOFING.md),
  [Bench tests](../BENCH.md)

**Next:** [DJI DroneID](dji-droneid.md)
