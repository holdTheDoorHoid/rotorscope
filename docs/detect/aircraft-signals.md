# Crewed-aircraft signals

**Level: 🟢 Beginner, then 🟡 Intermediate** · Why a drone detector listens to planes,
helicopters and gliders, which of their radio signals Rotorscope reads, how far each is proven,
and which deep dive to read next.

## 🟢 In plain words

Rotorscope's job is drones, but the most dangerous moment for a drone is when a crewed aircraft is
close: a medevac or police helicopter coming in low over a park where someone is flying. So
Rotorscope also listens to the signals crewed aircraft send about themselves, keeps them in a
**separate aircraft table**, and warns when a drone and an aircraft are getting close.

Aircraft are never treated as drones. They never sound the new-drone alert or the buzzer; they
appear in their own list on the screen, the phone page, the SD log and the other outputs. How to
read them on the unit is on the [Crewed aircraft](../use/aircraft.md) page; this page explains
the signals.

There are three families of aircraft signal:

- **ADS-B on 1090 MHz.** Airliners and most aircraft broadcast their identity, position, height
  and speed about once a second. Rotorscope reads it through a small receiver called an
  **ADSBee 1090**. This is the one aircraft signal Rotorscope has **heard for real**.
- **UAT on 978 MHz.** A US-only version used by most small private aircraft that stay below
  18,000 ft. The owner's ADSBee 1090 cannot hear it.
- **FLARM and OGN on 902-928 MHz** (868 MHz in Europe). Collision-warning beacons on gliders, some
  light aircraft and helicopters. Common in Europe, rare in the US.

UAT, FLARM and OGN need a different radio: the planned **aero node**, built around a Semtech LR2021
radio chip. Its firmware is written, but no board has been in hand, so it has never heard
anything.

Rotorscope never transmits on any aviation frequency.

## 🟡 The signals at a glance

| Signal | Band | Who uses it | Code | Proof, as of 2026-11-18 |
|---|---|---|---|---|
| ADS-B, read from the ADSBee's own decode (CSBee lines) | 1090 MHz | Airliners, most aircraft | `crates/adsb` | **Heard for real** on the laptop; on the P4 hub **simulated only** (loop-back) |
| ADS-B / Mode S, Rotorscope's own decoder | 1090 MHz | The same | `crates/modes` | **Heard for real** on the laptop (raw frames from the ADSBee); on the P4 **simulated only** (the project's own recording of real aircraft, looped back on the chip; no ADSBee wired) |
| UAT | 978 MHz | US light aircraft and helicopters below 18,000 ft | `crates/uat` | **Simulated only** |
| OGN tracker | 902-928 MHz US, 868 MHz EU | Gliders, tow planes, some paragliders | `crates/flarm` (`ogn`) | **Simulated only** |
| FLARM (legacy v6/v7) | 902-928 MHz US, 868 MHz EU | Gliders, some light aircraft and helicopters | `crates/flarm` (`legacy`) | **Simulated only** |
| The aero node (LR2021 + ESP32-C3) | 1090, 978, 902-928 / 868 MHz | Rotorscope's receiver for all of the above | `firmware/node-lr`, `crates/lr2021cfg` | **Built, not yet tried** (no hardware) |
| FLARM / OGN hop following | 902-928 MHz | The aero node | `crates/aerohop` | **Simulated only** |
| Drone near a crewed aircraft | (uses all of the above) | The hub | `crates/tracker` (`conflict.rs`) | **Simulated only** |

## 🟡 ADS-B on 1090 MHz

**Through the ADSBee.** The ADSBee 1090 decodes ADS-B itself and sends one text line per aircraft
each second, in its CSBee format with a checksum. `crates/adsb` reads those lines. On 2026-11-14,
on the laptop in central Philadelphia, 30 s gave 77 aircraft lines with 0 bad lines and 6 aircraft,
including arrivals on the approach to the airport. The P4 hub reads the ADSBee on one wire to
GPIO30; that path has been tested only by the hub feeding its own pin, not yet with the real
ADSBee wired in.

**Rotorscope's own decoder** (`crates/modes`, PLAN.md Phase 14a). The ADSBee can instead send the
raw frames it received, and Rotorscope decodes everything above the radio itself: checksums,
address recovery, small error repair, positions, velocity, identity, altitude, squawk and more. It
was compared, second by second, with the ADSBee's own decode over 15 minutes of live traffic on the
laptop (2026-11-15):

| Check | Agreed |
|---|---|
| Aircraft also in Rotorscope's table | 401 of 404 (99.3 %) |
| Callsign | 306 of 306 |
| Squawk | 19 of 19 |
| Emitter category | 327 of 327 |
| Barometric altitude | 396 of 397 (99.7 %) |
| Position | 362 of 369 (98.1 %), median distance 1 m |

Every disagreement looked at was the ADSBee's line being stale, not Rotorscope's decode being
wrong. On the P4 this decoder is off by default (`--features modes`) until the ADSBee is wired to
the hub. Details: [ADSBee 1090](../ADSBEE.md).

## 🟡 UAT, FLARM and OGN

All three decoders were built on 2026-11-17 and turn a raw radio frame into the same aircraft record
the ADSBee produces, labelled **UAT**, **OGN** or **FLARM**. None has yet seen a real radio frame.

- **UAT** (`crates/uat`). Repairs up to 6 damaged bytes in a basic message or 7 in a long one with
  the message's own error-correcting code, rejects anything worse, then reads address, position,
  altitude, speeds, call sign or squawk, aircraft type and emergency. Ground weather uplinks are not
  decoded.
- **OGN trackers** (`crates/flarm`, module `ogn`). Open-source glider beacons: the packet's parity
  checks must all pass, then position, altitudes, climb, speed, heading and aircraft type are read.
  Encrypted OGN packets are skipped.
- **FLARM** (`crates/flarm`, module `legacy`). FLARM scrambles its packets with keys that change
  with the time and the aircraft's address. Unscrambling also needs the hub's GPS time and its own
  position.

These decoders are Rust ports of other people's open-source work, with the credit kept in the
source: UAT from dump978 by Oliver Jowett, FLARM legacy and OGN from SoftRF by Linar Yusupov
(legacy decoder by Stanislaw Pusep) and the OGN tracker library by Pawel Jalocha. The ADSBee is by
Pants for Birds, and `crates/modes` follows Junzi Sun's *The 1090 Megahertz Riddle* and the
pyModeS test suite ([Credits and licences](../project/credits.md)).

**FLARM: the licence and the keys,** exactly as [Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md) records
them:

- FLARM's air protocol was proprietary and scrambled; FLARM objected to the Open Glider
  Network's decoding on "legal and IP aspects" in 2014/2015 before working with it. In May 2026
  FLARM published its radio protocol (the "FLARM FAMP Public Protocol") with a free licence for
  **non-commercial, receive-only** use; commercial use needs a paid licence.
- **The owner reports having permission to use the FLARM legacy keys (2026-11-17)**, and asked for
  FLARM legacy decoding. The permission covers using the keys, not publishing them.
- **The keys are never published.** No key is in the repository. The hub reads them at run time
  from `ROTORSCP/FLARMKEY.TXT` on its SD card, and they are never committed, printed, logged or
  sent between boards.
- **Receive only. Never transmit FLARM.** The crate has no transmit path.
- The decoder sits behind a build switch (`legacy`) that is off in the crate and always on in the
  P4 hub build, and idle until the owner's key file is on the SD card: without the file, FLARM
  stays presence-only.
- If Rotorscope units are ever sold, the commercial terms (FLARM's licence and the key permission)
  must be checked again before shipping FLARM decoding in them.

Aircraft whose owners set FLARM's "no-track" or "stealth" flags are logged and shown like every
other aircraft (owner decision 2026-11-17).

## 🟡 The aero node and hop following

The aero node is an ESP32-C3 with an LR2021 radio, reporting to the hub like the other nodes. It
listens to one thing at a time, chosen by the hub: by default UAT for 10 s, FLARM for 10 s and OGN
for 5 s in turn, adding 1090 MHz if the hub has not heard its ADSBee for a minute. The radio command
set the node uses contains no transmit command at all, and a test checks that. It checks each
message is intact and sends it to the hub unopened; the hub decodes.

**Hop following** (`crates/aerohop`). In the US, FLARM and OGN hop to a new channel twice a second,
over 65 channels from 902.2 MHz, 400 kHz apart. A receiver parked on one channel hears about 1 slot
in 65. The node takes the time from the hub's GPS, once a second, and jumps to the right channel at
the right moment. Without time for 10 s it parks on one channel and says so ("LR FLARM parked"). A
time message far from the node's own estimate is only believed when three agree in a row, so a
single forged message cannot move it into the wrong slot. Details:
[The aero node](../AERO-NODE.md).

## 🟡 Drone near a crewed aircraft

Once a second the hub compares every drone with every aircraft and warns at three levels, from
"Advisory" (within 3 km and about 1,000 ft) to "Warning" (within 300 m and about the same height, or
predicted to pass that close in the next 20 s). At Caution or Warning the status bar shows
**AIRCRAFT**, which takes priority over JAMMED and ATTACK, and each pair is logged as evidence. It
compares GPS and pressure heights carefully, predicts the closest approach over the next minute,
and only uses confirmed drones. It is a prompt to look up, not a finding of fault. **Simulated
only**. Details: [Drone near a crewed aircraft](../AIRCRAFT-CONFLICT.md).

Aircraft are also a witness for GPS interference: when several aircraft nearby report degraded
position quality together, the hub raises "GPS interference nearby" (see
[Attacks and interference](threats.md)).

## Where this lives in the code

- [`crates/adsb`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/adsb): ADSBee CSBee
  lines and the shared aircraft record
- [`crates/modes`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/modes) and
  `tools/modes-live`: Rotorscope's own 1090 MHz decoder and its live comparison
- [`crates/uat`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/uat) and
  [`crates/flarm`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/flarm): UAT, OGN
  and FLARM
- [`crates/aerohop`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/aerohop),
  [`crates/lr2021cfg`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/lr2021cfg)
  and [`firmware/node-lr`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/node-lr):
  the aero node
- [`crates/tracker/src/conflict.rs`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/crates/tracker/src/conflict.rs):
  the drone-near-aircraft check
- Deep dives: [ADSBee 1090](../ADSBEE.md), [Beyond 1090 MHz: UAT and FLARM](../AIRCRAFT-SIGNALS.md),
  [The aero node](../AERO-NODE.md), [Drone near a crewed aircraft](../AIRCRAFT-CONFLICT.md)

**Next:** [ADSBee 1090](../ADSBEE.md)
