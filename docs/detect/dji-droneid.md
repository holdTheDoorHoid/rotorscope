# DJI DroneID

**Level: 🟢 Beginner, then 🟡 Intermediate** · What DJI's own identification broadcast is, how it
differs from Remote ID, what Rotorscope does with each DJI radio generation, how far that has been
proven, and where the expert detail lives.

## 🟢 In plain words

**DroneID** is DJI's own identification broadcast. DJI built it before Remote ID existed, for its
own receivers (sold as AeroScope). A DJI drone sends a short radio burst, a little over half a
millisecond long, about every 640 ms, but only while its motors are running. Each burst can carry
the drone's serial number and the positions of the drone, the pilot and the take-off point.

How it differs from [Remote ID](remote-id.md):

| | Remote ID | DJI DroneID |
|---|---|---|
| Who defines it | A public standard (ASTM F3411) | DJI alone; known from published research |
| Which radio | Ordinary Wi-Fi and Bluetooth | DJI's own video-link radio, called **OcuSync** |
| Who can hear it | Any phone with an app | Only a receiver that reads raw radio samples |
| Encrypted? | Never | Not on older drones; yes on the newest (O4) |

Because DroneID is not Wi-Fi or Bluetooth, Rotorscope cannot hear it the normal way. Instead it
turns an ESP32-C5 into a **raw radio sampler**: the chip's radio hands over the raw signal and
Rotorscope's own code finds and decodes the bursts.

### DJI's radio generations, in plain words

- **Wi-Fi DJI drones** (Spark, Mavic Air, Mini SE and others) do not use OcuSync at all. They put
  a DroneID record in their Wi-Fi beacons, which the ordinary Wi-Fi listener decodes (see
  [Remote ID](remote-id.md), "Two formats that are not ASTM").
- **OcuSync 2** (Mini 2, Mini 4K, Air 2, Mavic 2): plain, unencrypted DroneID. Rotorscope decodes
  serial number and positions (**proven on recordings** of a Mini 2 and a Mavic Air 2).
- **OcuSync 3 / 3+** (Mini 3, Air 2S, Avata, Mavic 3): reported as mostly plain; one report says
  the Inspire 3 encrypts. Same decoder, but no recording of these models has been through it yet.
  (A public recording of the DJI FPV drone gave plain records through a computer model of the C5.)
- **O4 / O4+** (Mini 4 Pro, Air 3, Avata 2, Neo, Flip, Mavic 4): **encrypted**. Rotorscope can tell
  an O4 drone is there and follow it as a numbered session (`DJI O4 <key id>`), but cannot read its
  serial or position without a key (below).

**Proof, as of 2026-11-18:** decoding is **proven on recordings**. It decodes every burst of two
public recordings (a DJI Mini 2 and a Mavic Air 2, from RUB-SysSec), also when those recordings
are played through the C5's own capture path. Public RFUAV recordings of two O4 drones gave valid
encrypted O4 records through a computer model of the C5's capture.
Rotorscope has **never heard a live DJI drone**. It **never transmits DroneID**, not even on the
bench: the test signals exist only as sample arrays inside laptop tests.

### O4 and the key

Each O4 flight picks a fresh session key; the records carry a 4-byte **key ID** in clear, which
works as a session label but proves nothing about who sent it. The page
[O4 DroneID in depth](../DRONEID-O4.md) states the position plainly:

- The project owner authorised O4 decryption on 2026-11-16, and the P4 hub has the code for it
  (`crates/djicrypt`).
- **It works only with key material the owner supplies at run time**, as a file on the hub's SD
  card (`ROTORSCP/O4KEY.TXT`). The repository contains no private or public key and no
  session-key prediction, and this project publishes no key. The key is never printed, logged, sent between boards
  or written to the evidence log.
- **No public key exists** that decrypts real aircraft: DJI's AeroScope private key has never been
  published. Without the file, nothing changes: O4 stays presence and session only.
- Decryption has been tested only with a key made up inside the tests, never on a real aircraft.
  The plan notes that decrypting may lose legal protections that plain broadcasts have, so only a
  lawfully obtained key should ever be used.

## 🟡 How a C5 is used as a raw radio sampler

The DroneID job runs on a dedicated XIAO ESP32-C5 (it needs the built-in PSRAM) with the
`firmware/node-sdr` firmware. The hub assigns the job; the same board can instead run the FPV scan
([FPV video links](fpv.md)).

- **Capture** (`crates/rawcap`, from facts published by C5VRX and the esp-sdr projects). The
  modem's internal debug bus is routed out to eight pins and straight back in, sampled at
  40 million samples a second into a 1.43 ms ring: 4 bits of I and 4 of Q per sample. A second,
  snapshot path gives 10-bit samples at 20 MS/s for 822 µs. While capturing the node is
  receive-only: the Wi-Fi transmit queues are stopped and checked.
- **Decode** (`crates/droneid`, fixed point, no `std`). An energy trigger freezes a window; the
  decoder resamples it to DJI's 15.36 MS/s, finds the two Zadoff-Chu reference symbols, equalises,
  descrambles, runs an LTE-style turbo decoder and checks the CRC. On the chip a decode takes
  about 150 ms, well inside the gap between bursts.
- **Where it listens** (`crates/coverage/src/plans.rs`, `DRONEID_PLAN` in node-sdr): 13 hops of
  700 ms. At 5.8 GHz every 10 MHz from 5746.5 to 5816.5 MHz (each capture also covers the channel
  5 MHz below), and at 2.4 GHz 2414.5, 2429.5, 2444.5, 2459.5 and 2399.5 MHz. The C5's capture
  reach was measured to 5800 MHz; the hops above it, up to 5816.5 MHz, were seen to tune normally
  on a C5 (2026-11-17), but no DroneID burst has been received there.
- **Following O4 drones.** Parked on one frequency a C5 hears an O4 drone about once in seven
  publications. Once three packets of a session agree, the node uses the published channel
  schedule to be on the right frequency for the next one, taking at most about half its time.
  How this behaves under forged packets is set out in [O4 DroneID in depth](../DRONEID-O4.md).
- **On the hub.** A plaintext DroneID track merges with the same drone's Remote ID by serial number,
  showing drone, pilot and home. O4 sessions show as `DJI O4 <key id>` with a state such as
  "GPS locked" or "probably flying", always marked model-dependent. The DJI pilot account name
  that some O4 records carry is never taken in.
- **Privacy switch.** Plaintext DroneID fields are shown by default, like Remote ID. The tracker
  can hide DroneID identities; as of 2026-11-18 only the laptop viewer exposes that
  (`rid-view --hide-droneid-ids`), not the hub.

## 🟡 DJI link fingerprints

DroneID is only part of what a DJI drone sends. The same raw capture, taken during the FPV scan,
is checked by `crates/djilink` for the shape of DJI's video and controller links. It reports
presence only: it never demodulates video or control data.

| Report (screen label) | What it recognises |
|---|---|
| **DJI video** | OcuSync video: OFDM with DJI's 15 kHz carrier spacing, video width, on DJI's channel raster |
| **DJI O4 video** | O4 video, which uses 30 kHz carrier spacing (measured), on any band; or DJI-style video in 5.170-5.250 GHz |
| **DJI FPV air unit** | DJI-style video on a whole-MHz DJI FPV channel (kept for older air units; not confirmed on any recording) |
| **DJI controller** | The pilot's uplink: short narrow bursts, confirmed once they appear on two hop centres within 6 s |

The rules were checked against four public RFUAV recordings (DJI Mini 4 Pro, DJI Avata 2, DJI FPV
Combo, Autel EVO Nano), passed through a computer model of the C5's 4-bit capture, and tuned where
they disagreed. Known limits from [DJI radio links in depth](../DJI-LINKS.md): Autel uses the same
15 kHz spacing as older DJI links, so some Autel bursts can still read as DJI; a drone and a DJI air
unit on the same raster cannot be told apart; DJI FPV and Walksnail reportedly share a channel plan
and need recordings to separate.

## 🟡 Proof level

| What | Proof | Evidence |
|---|---|---|
| Plaintext DroneID decode, laptop | **Proven on recordings** | Both RUB-SysSec recordings (Mini 2, Mavic Air 2, OcuSync 2): 13 of 13 bursts CRC-valid; also CRC-valid records from the RFUAV DJI FPV recording through the C5 capture model (2026-11-16) |
| Plaintext DroneID decode, on the C5 | **Proven on recordings** | The same bursts sent into the chip's live capture path (`tools/droneid-hil`): 13/13 decoded at 9.4 dB and 6.9 dB, median about 150 ms. These sightings are never sent to the hub, so a recording can never appear as a live drone |
| Not fooled by real Wi-Fi | **Proven on the air** (ordinary Wi-Fi around the bench, no drone) | 200 s on the air with the DroneID job: 1,559 Wi-Fi frames of burst length went through the decoder, 0 DroneID sightings sent |
| O4 records detected | **Proven on recordings** | RFUAV Mini 4 Pro and Avata 2: CRC-valid O4 records, through the C5 capture model (2026-11-16) |
| O4 decryption | **Simulated only** | A made-up key in the tests; no real key exists for the project to try |
| O4 follower | **Simulated only** | A host model of the node, and bench runs with simulated forged sessions |
| DJI link fingerprints | **Proven on recordings** | RFUAV recordings through a model of the C5; not yet run on the chip against a live signal |
| Anything from a live DJI drone | not yet | Needs a real DJI drone with props off, or a field visit |

## Credits

From the [README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/README.md)
acknowledgements. The C5 raw-radio work builds on findings from C5VRX by Twotoz and the C5VRX
contributors, ESPARGOS/esp-sdr, cyber-murmel/esp-sdr-zephyr and CapnBry/widscan58. The DroneID work
builds on RUB-SysSec's DroneSecurity research, proto17/dji_droneid and
anarkiwi/samples2djidroneid, and on the O4 record and radio description in
EdwardBlair/dji-droneid-spec; the O4 page also draws model behaviour from
luyii-code-1/dji-ocusync-droneid-research. Rotorscope uses these for protocol facts only and
contains no code from any of them. The two test recordings come from RUB-SysSec/DroneSecurity
(AGPL-3.0): `tools/fetch-droneid-samples.sh` downloads them on demand, and they are never committed.
The RFUAV recordings (Hugging Face `kitofrank/RFUAV`) are downloaded one at a time for local checks
with `tools/rfuav-check`; only derived numbers are kept ([Credits and licences](../project/credits.md)).

## Where this lives in the code

- [`crates/droneid`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/droneid): the
  burst decoder, O4 checks (`payload.rs`), the follower (`follow.rs`)
- [`crates/rawcap`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/rawcap): the C5
  capture logic
- [`firmware/node-sdr`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/node-sdr):
  the raw-capture firmware, with its own README and bring-up results
- [`crates/djilink`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/djilink): link
  fingerprints
- [`crates/djicrypt`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/djicrypt): O4
  decryption with an owner-supplied key
- `tools/droneid-file` and `tools/droneid-hil`: decode recordings on the laptop, or through the chip
- Deep dives: [O4 DroneID in depth](../DRONEID-O4.md), [DJI radio links in depth](../DJI-LINKS.md),
  [Signal catalogue](../SIGNALS.md), [Spoofed drones](../SPOOFING.md)

**Next:** [FPV video links](fpv.md)
