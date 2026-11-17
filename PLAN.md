# Rotorscope — Requirements and Plan

Name: **Rotorscope** (chosen 2026-11-09). Drafted 2026-11-09.
Research behind every claim here is in [RESEARCH.md](RESEARCH.md).

## In one paragraph

A pocket-or-tripod box with several cheap ESP32 chips. Each chip listens to one kind of drone
radio signal, and a "hub" chip merges what they hear. You see the result on a small built-in
screen, on your phone's browser, in a log on an SD card, and in tools like ATAK. It starts with
**Remote ID**, the "digital license plate" most drones must broadcast. It grows to cover
ADS-B aircraft (via an ADSBee) and analog FPV video transmitters. The device **only listens** and
never transmits anything that could interfere with a drone.

## Decisions already made (2026-11-09)

| Question | Answer |
|---|---|
| How it is used | **Both** handheld (battery, screen) and fixed sensor (USB power, network feeds), one board |
| Where detections show | **All of:** phone web page, on-device screen, TAK/MQTT feeds, SD-card log |
| Parts budget per unit | **$100–$200** |
| Audience | **Open source + kits** to sell at cons. This needs clean build docs and a browser flasher. |
| Language | Rust wherever the ecosystem allows; exceptions written down (see "Rust plan") |

## What "cooperative" and "semi-cooperative" mean here

- **Cooperative:** the drone deliberately announces itself. Examples are Remote ID (Wi-Fi and
  Bluetooth) and ADS-B.
- **Semi-cooperative:** the drone isn't trying to be identified, but its radios give it away.
  Examples are DJI's older Wi-Fi "DroneID" beacons, the Wi-Fi networks of Wi-Fi-controlled
  drones, and analog FPV video carriers.
- **Out of scope:** drones that are radio-silent, and DJI's OcuSync DroneID. OcuSync needs a
  real SDR such as a USRP, not an ESP32.

---

## Requirements

"Must" = needed for the first complete Remote ID unit. "Should" = wanted, can slip.
"Later" = a future phase.

### Detection

| ID | Requirement | Priority |
|---|---|---|
| D1 | Decode every ASTM F3411 / ASD-STAN Remote ID message type: Basic ID, Location, Authentication, Self-ID, System (operator position), Operator ID, and Message Pack. | Must |
| D2 | 2.4 GHz Wi-Fi Remote ID: **beacon** frames and **NAN** frames. Stay mostly on channel 6 and briefly visit 1 and 11. | Must |
| D3 | Bluetooth Remote ID: **BLE 4 legacy** adverts **and BLE 5 Long Range** (coded PHY, extended advertising). | Must |
| D4 | 5 GHz Wi-Fi Remote ID: anchor on channel 149, hop 153–165, and optionally 36–48. | Must |
| D5 | Merge reports of the same drone heard on different radios into one "track". Show which radios heard it, its signal strength and when it was last seen. | Must |
| D6 | Show a newly powered drone within **3 s** of it coming into range. Remote ID repeats at least once a second. | Must |
| D7 | Track at least **32** drones at once, and 64 as a goal. | Must |
| D8 | Range: at least as good as a phone RID app. Measure the real range with stock and external antennas and publish the numbers. | Must (measure) |
| D9 | Decode China's GB 46750-2025 format. DJI's 2026 firmware sends it in the same Wi-Fi element. | Should |
| D10 | Decode DJI's legacy Wi-Fi DroneID beacons (vendor tag, OUI `26:37:12`). No ESP32 project does this today. | Should |
| D11 | Flag known drone-maker Wi-Fi networks by maker address prefix and network name. This is semi-cooperative detection. | Later |
| D12 | ADS-B (1090 MHz) and UAT (978 MHz) aircraft via an ADSBee, shown on the same map. | Later |
| D13 | Analog FPV video detection on 5.8 GHz: channel, signal strength, and "a video transmitter is active". | Later |
| D14 | 2.4 GHz control-link activity (ELRS etc.) as energy only, without identifying it. | Later / research |

### Position and time

| ID | Requirement | Priority |
|---|---|---|
| P1 | GPS (u-blox MAX-M10S) gives the unit's own position and accurate time. | Must |
| P2 | Show **distance and direction** from you to each drone and to its operator. | Must |
| P3 | The GPS PPS pulse goes to every chip so all their timestamps agree to well under 1 ms. | Should |
| P4 | Works without a GPS fix: detections still show, with "own position unknown" and no direction. | Must |

### Outputs

| ID | Requirement | Priority |
|---|---|---|
| O1 | **Phone web page.** The unit runs its own Wi-Fi hotspot and serves a list and a "radar" view relative to you. This works with no internet. A map view loads tiles only when the phone has data. | Must |
| O2 | **On-device screen.** Drone count, nearest drone with distance and bearing, and new-drone alert. Works with no phone. | Must |
| O3 | **SD-card log.** Every detection, with time and the unit's own position, as CSV or JSON lines. | Must |
| O4 | **USB stream.** JSON lines over USB, so a laptop (or DragonSync/WarDragon) can consume it. | Must |
| O5 | **Fixed-sensor mode.** Join an existing Wi-Fi network as a client and send TAK Cursor-on-Target, MQTT, and Home Assistant discovery. | Should |
| O6 | Alerts: buzzer or vibration on a new drone. Optional geofence ("drone within X m"). | Should |
| O7 | ADS-B tracks in all of the above. | Later |

### Physical and cost

| ID | Requirement | Priority |
|---|---|---|
| H1 | One board for both modes. USB-C power for fixed use; battery for handheld use. | Must |
| H2 | Handheld runtime of **at least 4 hours**. This is a target to confirm by measurement, since four radios plus a screen draw about 2 W. | Should |
| H3 | Antenna connectors (u.FL inside, SMA on the case) on every radio chip so better antennas can be fitted. | Must |
| H4 | The Remote ID unit's parts cost **$120 or less**. The ADSBee tier should cost **$200 or less** (ADSBee price not yet confirmed). | Must |
| H5 | Kit buyers can update every chip from a web page without opening the case. | Should |

### Software and conduct

| ID | Requirement | Priority |
|---|---|---|
| S1 | Firmware in Rust, bare-metal (`esp-hal` + `esp-radio`). Any exception is written down here with the reason. | Must |
| S2 | One shared Rust library holds the message formats and the Remote ID decoder. It is tested on a laptop against the official opendroneid-core-c decoder. | Must |
| S3 | **Receive-only.** No jamming, no spoofing. The only transmissions are the hub's own hotspot or network link. | Must |
| S4 | A short legal and privacy note in the README. Remote ID is public by design, and the note says what the device does and doesn't do. | Must |

### Not doing

- Taking drones down or interfering with them in any way.
- Radar, acoustic or camera detection.
- Decoding DJI OcuSync.
- 900 MHz control links. No ESP32 can tune there, so this needs a separate sub-GHz radio later.

---

## Architecture

```
            ┌──────────── GPS MAX-M10S ────────────┐
            │ position/time            PPS pulse ──┼──► every chip
            ▼                                      │
 ┌────────────────────────── HUB (ESP32-S3 w/ PSRAM) ─────────────────────────┐
 │ merge + dedupe tracks · range/bearing · screen · SD log · web page/hotspot │
 │ USB JSON · TAK/MQTT (fixed mode)                                           │
 └──▲──────────────▲──────────────▲──────────────▲────────────────▲───────────┘
    │ UART         │ UART         │ UART         │ (later)        │ (later)
 ┌──┴─────────┐ ┌──┴─────────┐ ┌──┴─────────┐ ┌──┴───────────┐ ┌──┴─────────┐
 │ NODE A  S3 │ │ NODE B  S3 │ │ NODE C  C5 │ │ NODE D  C5   │ │ ADSBee     │
 │ 2.4 GHz    │ │ Bluetooth  │ │ 5 GHz      │ │ FPV 5.8 GHz  │ │ 1090 / 978 │
 │ Wi-Fi RID  │ │ RID (LR)   │ │ Wi-Fi RID  │ │ scanner      │ │ CSBEE out  │
 └────────────┘ └────────────┘ └────────────┘ └──────────────┘ └────────────┘
```

**Why one chip per radio.** Bluetooth and Wi-Fi share one antenna and one receiver on each
chip. One open-source project measured what happens when a single chip time-shares them: it
caught 4 Wi-Fi Remote ID frames in the time it caught 234 Bluetooth ones. Giving each band its
own chip removes that tradeoff, and the chips cost about $8 each.

**Why a separate hub.** The radio chips are busy listening. Their Wi-Fi can't also run a hotspot
for your phone without going deaf. The hub is the only chip that transmits. Its hotspot sits on
channel 11, away from the channel-6 Remote ID traffic. Phase 4 will test whether that causes any
interference.

**Talking between chips.** Every chip speaks the same small binary message format, defined in
the shared Rust library. The prototype uses serial (UART) links. The hub has 3 UARTs, so the
GPS moves to I2C (the MAX-M10S supports it). Once the FPV node and ADSBee arrive, there are more
devices than UARTs. The PCB design step (Phase 5) has to choose between two fixes:
- A shared SPI bus, with the hub as master and a "data ready" line from each node.
- Having the ADSBee join the hub's hotspot and send its data over Wi-Fi.

The message format doesn't change either way.

**Decided 2026-11-15: many nodes on one UART is the target.** Instead of one UART per node, every
node shares a single hub UART: the hub's TX fans out to every node's RX, and every node's TX joins
one return wire (open-drain, one pull-up). The hub polls each node in turn ("node N, send"), so no
two nodes ever talk at once. Three wires (TX, RX, ground) serve any number of nodes, nodes stay
receive-only on the air, and the ESP-NOW gateway C5 becomes another listener. See Phase 5a.

---

## Rust plan: what's feasible, and the exceptions

The research found **no hard blocker** for Rust on any Remote ID role. Espressif's Rust HAL
reached 1.0 in October 2025, and the ESP32-C5 is supported in it, including 5 GHz.

| Part | Stack | Confidence | Main risk |
|---|---|---|---|
| Node A: 2.4 GHz Wi-Fi RID (S3, or C5 built `band24`) | `esp-hal` 1.2 + `esp-radio` sniffer | High | Done on both chips: beacon and NAN decoded on air (C5 listener 2026-11-12). |
| Node B: Bluetooth RID (S3 or C5) | `esp-radio` BLE + `trouble-host` extended scan, both PHYs | Medium | Long Range scanning from Rust is unproven on both chips. Phase 2 tests S3 first and switches to a C5 if needed. |
| Node C: 5 GHz Wi-Fi RID (C5) | `esp-radio` sniffer + 5 GHz band + country "US" | Medium | The 5 GHz sniffer is new. A recent change to the default country can block channel 149, so set "US" explicitly. |
| Hub, GPS, screen, SD, web | `esp-hal` + embassy, `ublox`/`nmea` crates | High | The web UI and TLS-free hotspot are ordinary work. |
| Remote ID decoder | Pure Rust, reusing or modelled on `pocket-rid`'s Apache-2.0 decoder (fuzz-tested against the official C decoder) | High | The `remote-id` crate on crates.io is GPL and **crashes on Message Packs**, so it is avoided. |
| Node D: FPV | **Likely an exception (C).** Both ESP32 raw-radio projects (C5VRX, ESP-SDR) are C on ESP-IDF and GPL-3.0. | — | Decide in Phase 7 (see below). |
| ADSBee | Runs its own C++ firmware. The hub reads its output. | High | — |

**Fallback.** For any radio role where bare-metal Rust falls short, drop to Rust on top of ESP-IDF
(`esp-idf-svc`) before considering C. That route is still maintained and exposes the same
promiscuous-mode and Bluetooth calls the working C projects use.

**Toolchain note.** The S3 is an Xtensa chip, so it needs Espressif's Rust compiler fork,
installed with `espup`. The C5 builds on normal stable Rust.

---

## Phases

Each phase ends with something you can watch work. Hardware needed is listed so you can order
ahead.

### Phase 0: Bench setup (can start now)
- Install the Rust ESP toolchain (`espup`), and verify that a blink/hello build flashes to the
  attached S3.
- Build a **test transmitter**: [ArduRemoteID](https://github.com/ArduPilot/ArduRemoteID) on a
  second S3 or C3, sending all four Remote ID types at low power with an obviously fake test ID.
  This gives a known signal to test against whenever we want. A real Remote ID drone or
  broadcast module is the second check.
- Flash one existing open-source receiver as a **reference**, so our results have something to
  be compared to. Orecchino or colonelpanichacks are the candidates.
- **Done when:** the reference receiver shows our test transmitter on every transport.

### Phase 1: Node A, 2.4 GHz Wi-Fi Remote ID, in Rust (attached S3)
- Shared library: message format and Remote ID decoder, with laptop tests against the official C
  decoder's test vectors.
- Sniffer firmware: beacon and NAN, channel 6 home with visits to 1 and 11, JSON lines over USB.
- A small laptop viewer script, used for development only.
- **Done when:** it decodes both the test transmitter's beacon and NAN messages, with the same
  values as the reference receiver.

### Phase 2: Node B, Bluetooth Remote ID
- Extended scan on 1M and coded PHY.
- Bench-test Long Range on the S3 first. Switch to a C5 if it doesn't hear coded-PHY adverts.
- **Done when:** both the legacy and the Long Range test signals decode, and a range walk is
  recorded.

### Phase 3: Node C, 5 GHz Wi-Fi Remote ID (C5)
- Same firmware as Node A, plus 5 GHz band and channel plan.
- Known C5 quirk from DRONK: the frame length includes 4 extra bytes.
- **Done when:** it hears a 5 GHz Remote ID source on channel 149. The test transmitter is 2.4
  only, so this needs a C5-based test transmitter or a real drone such as a DJI Avata or Mini 4.

### Phase 4: Hub prototype on dev boards
- Merge and dedupe tracks, GPS, distance and bearing, screen, SD log, phone web page over the
  hotspot, USB JSON.
- Fixed-sensor extras: TAK CoT, MQTT. Laptop output for the WarDragon ecosystem: `rid-view --dragonsync` (ZeroMQ JSON that DragonSync subscribes to).
- Check whether the hub's hotspot hurts the sniffers.
- **Done when:** a breadboard unit shows a test drone on the screen and phone, with distance and
  direction, and logs it.

### Phase 5: Kit hardware
- A carrier PCB with the chips as modules or castellated boards, antenna connectors, battery and
  power path, and a case.
- ~~Decide the SPI-bus versus Wi-Fi-link question here.~~ Superseded by Phase 5a.

### Phase 5a: Many nodes on one UART (objective, added 2026-11-15)
Goal: drop ESP-NOW as the default node link. That gives back the listening time it costs (Bluetooth
node ~18% of adverts, 5 GHz node's channel-6 visits, DroneID node's send pauses), stops nodes
transmitting at all (no unauthenticated radio input), and frees the gateway C5 to be a listener.

- **Bus:** hub UART TX → every node RX (addressed commands; others ignore). Node TX pins
  open-drain onto one shared return wire with one pull-up. 921600 baud to start.
- **Polling:** the hub asks each node in turn ("node N, go ahead"); the node sends its queued
  frames and a "done" marker, or "nothing". A ~20 ms cycle keeps latency below today's ESP-NOW.
- **Joining:** a periodic "anyone new?" slot; a new node answers after a random delay and is given
  a short bus address tied to its MAC, so it keeps it across image switches. Collisions only
  happen at join and are caught by the frame checksum.
- **Same frames:** `link` frames unchanged; the poll/address messages are new `Command` and
  `Frame` variants appended to the protocol, like protocol 4. `Via` attribution comes from the
  poll address.
- **Long cables:** the same design over RS-485 transceivers (~$0.50 each end) for runs beyond about
  1 m. CAN (TWAI, ~$1 transceiver per board) is the fallback if polling proves limiting.
- **Power:** nodes powered separately on the kit; the P4 header's 5 V is not sized for several C5s.
- **Keep ESP-NOW** as an opt-in build for a node that can't be wired.
- **Rejected:** I2C (the P4's only I2C header is the touch bus, its I2C controller doesn't work so
  it is bit-banged, short wires only); SPI (a chip-select wire per node, not enough free pins).
- **Done when:** c5a, b and c (gateway turned listener) share UART3 into the P4 with 0 corrupt
  frames, every role assignment arrives over the wire, and the listening-time gain over ESP-NOW is
  measured.
- Browser flasher that updates every chip, building on the earlier Orbic web-flasher work.
- Build guide.
- **Done when:** someone other than us builds one from the guide.

### Phase 6: ADSBee
**BUILT 2026-11-09, untested on hardware:** `crates/adsb` (CSBee parser), hub feature `adsbee`
(ADSBee TX → hub GPIO41, takes node C's UART), aircraft on phone/screen/CSV/CoT/rid-view.
Setup and live test in `docs/ADSBEE.md`. A 4th UART without losing a node needs an SC16IS752 on the carrier PCB.

- Read CSBEE over UART or Wi-Fi into the hub, and show aircraft on the same views.
- Share the GPS with the ADSBee: one GPS output wire can feed two inputs.
- Note that small drones almost never carry ADS-B. This is for awareness of crewed aircraft
  nearby, such as medevac helicopters and crop dusters.

### Phase 7: Analog FPV detection (Node D)
Decided 2026-11-09 (see RESEARCH.md "Expansion research"):

1. **Start with an RX5808 module (about $6) on the hub.**
   - It takes 1–2 days in Rust and sweeps all 48 channels in about 1.5 s.
   - Weakness: 5 GHz Wi-Fi can look like a video transmitter.
2. **Optional upgrade: an ESP32-C5 doing detection only, in Rust.**
   - About 6–10 days.
   - It confirms real video by the 15.6/15.7 kHz line rate, so Wi-Fi is not mistaken for video.
   - The proprietary radio library our crates already ship is the exact version C5VRX was built against.
   - It needs no BitScrambler. esp-hal has no driver for that yet, which is what blocks a Rust video port.
3. **Live video to phones is deferred.**
   - A Rust port of C5VRX's video path is 35–60 engineer-days.
   - Its demodulator table is GPL-3.0 work, which our GPL-3.0 license lets us reuse.
   - The C5 can't time-share its radio between receiving video and running Wi-Fi.
   - Realistic route if wanted later: run stock C5VRX as a separate GPL binary on its own C5. Capture its digital video pins with an S3 (LCD_CAM), compress to JPEG in software, and serve to phones. That gives roughly 10–30 fps at 160×120–320×240 (unverified).
   - An ESP32-P4 only makes sense here, and only as a separate video module written in ESP-IDF C.
4. **2.4 GHz analog video (2026-11-15, branch `fpv-24ghz`): built, synthetic-only.**
   - The C5 FPV job now also sweeps the 15 common 2.4 GHz analog video channels (2370–2510 MHz;
     the 20 MHz and 18 MHz grids, docs/SIGNALS.md 3b). All are inside the C5's measured 2.4 GHz
     reach (2200–2650 MHz). One sweep = 15 + 31 channels, ~2.8 s; two FPV nodes split them.
   - Same line-rate confirmation, plus 2.4 GHz-only guards: its own noise floor, an 802.11b DSSS
     recogniser, a line-shape check (inverter microwave ovens can fake a line rate), and inside
     2400–2500 MHz only confirmed video is reported.
   - Tested on synthetic signals only: PAL/NTSC video (with audio subcarrier, under Wi-Fi and
     Bluetooth), OFDM Wi-Fi on channels 1/6/11, DSSS, Bluetooth hopping, ovens, noise.
   - **Untested on air.** To validate: a cheap 2.4 GHz analog camera / AV sender (4-channel
     2414–2468 kit) or a 2.4 GHz FPV VTX near a C5 in FPV mode; also one capture of ordinary
     2.4 GHz Wi-Fi to confirm the noise floor, gain (83 vs `gain_max` 81) and analysis time.

### Phase 8: LoRa mesh backhaul (several units sharing detections)
- Use a **stock Meshtastic or MeshCore board** (SX1262; Heltec V3/V4, XIAO + Wio-SX1262), wired to the hub's serial port as a modem.
- No mesh code of our own: both are C++, and no Rust stack is usable yet.
- Send two message types:
  - plain-text lines compatible with drone-mesh-mapper, so that ecosystem can read us;
  - a compact binary message for other Rotorscope hubs.
- Airtime is scarce. Send only on a new or significantly changed track, plus a heartbeat every 30–60 s, on a private channel using a 500 kHz preset. Those presets are the ones that clearly meet FCC 15.247.
- **Never share one LoRa radio between backhaul and detection.** A mesh node must keep listening to relay packets, and it is deaf to drone links whenever it transmits.

### Phase 9: Control-link detection node (LR2021; switched from LR1121 on 2026-11-15)
- One **Semtech LR2021** radio covers both bands drone control links use: 900 MHz and 2.4 GHz
  (150–960 MHz and 1.5–2.5 GHz, one band at a time).
- **Why LR2021 over LR1121** (research 2026-11-15, Semtech LR20xx datasheet rev 2.2):
  - SX1280-compatible **FLRC**, so it hears ELRS's high-rate racing mode. The separate SX1280 idea is dropped.
  - **Multi-SF receive**: up to 4 spreading factors at once on one channel, so several ELRS rates are
    heard together instead of tried in turn (within its processing budget, e.g. 3 SFs at 812 kHz).
  - Receive bandwidth up to ~3 MHz (vs ~0.5 MHz), so energy maps need far fewer steps.
  - About 4 dB more LoRa sensitivity; chip about the same price (~$8–10).
  - Buy the **LR2021** itself: the LR2022 and LR2012 lack FLRC.
- **Boards:** none cheap on sale yet. ExpressLRS master (not yet released) has LR2021 targets for
  ESP32-C3 receivers (GEPRC LINKFLOW LR20, BlackSheep ES4, generic C3), the same shape as the
  RadioMaster XR1; wait for one of those for the kit. For bench work now: Seeed/Semtech LR2021
  evaluation kit (~$99, two radios) or a Waveshare Core2021 / Elecrow LR2021 module wired to a spare
  ESP32. Keep one RadioMaster XR1 (ESP32-C3 + LR1121, ~$12–17) as the fallback.
- **Rust driver:** the standalone `lr2021` crate (no_std, async; young, one maintainer, not yet
  tried on an ESP32). `lora-phy` has no LR2021 driver (lora-rs issue #438); its LR11xx driver is
  the fallback path.
- **Backhaul (Phase 8) stays SX1262.** An LR2021 Meshtastic board also works over the air, so it is
  allowed, not required.
- **Ignore** the undocumented raw-IQ "SDR" mode (depends on the chip's ROM version).
- Detects ExpressLRS (the most common FPV control link) on both bands **without knowing the pilot's binding phrase**:
  - It parks on ELRS's fixed sync channel and tries each rate setting.
  - It also builds energy maps of both bands.
- Also catches mLRS, and the hopping energy pattern of SiK/RFD900 autopilot telemetry.
- It cannot decode Crossfire, Tracer or Ghost; those show up as energy only. ELRS's high-rate racing mode (FLRC) is covered by the LR2021 (it was the LR1121's gap).
- **Legal line.** A 2020 DOJ/FAA/FCC/DHS advisory warns that monitoring drone control links can fall under wiretap and pen-register law.
  - Presence and energy detection are the default.
  - Reading a link's identifier (ELRS UID bytes) is **opt-in**.
  - Decoding link *content* is out of scope.
- The hub flags any detection made while its own LoRa backhaul was transmitting.

### Phase 10: DJI OcuSync detection and DroneID decoding (C5 as a radio sampler)
Decided 2026-11-09 (see RESEARCH.md "DJI OcuSync and DroneID").

**What we can get, by generation:**

| DJI link | Example drones | Detect | Decode serial, drone and pilot position |
|---|---|---|---|
| OcuSync 2 | Mini 2, Mini 4K, Air 2, Mavic 2 | Yes | Yes, plaintext (proven by open tools) |
| OcuSync 3 / 3+ | Mini 3 / 3 Pro, Air 2S, Avata, Mavic 3 | Yes | Mostly yes (one vendor's tests); Inspire 3 is encrypted |
| O4 / O4+ | Mini 4 Pro, Air 3, Avata 2, Neo, Flip, Mavic 4 | Yes, plus a per-session ID | **No.** Encrypted; the key is in DJI's hardware and is not public |

**The approach.** Every OcuSync drone sends a short, LTE-style DroneID burst (about 9 MHz wide,
0.57–0.64 ms long, every ~640 ms) on a handful of fixed frequencies in 2.4 and 5.8 GHz, but
only while its motors are running. A dedicated C5 reads its own radio's raw samples, as C5VRX
does, watches one of those frequencies continuously, and recognises the burst by its built-in
reference pattern (Zadoff-Chu root 600). For plaintext bursts it then demodulates and decodes
the fields. No external SDR is needed. No ESP32 or Rust implementation exists yet, so this is new
ground.

1. **10a. Host decoder first, no hardware needed.** **DONE 2026-11-09:** `crates/droneid` +
   `tools/droneid-file`. Both public recordings decode, 13 of 13 bursts CRC-valid, on the cheap
   path. A simulated 4-bit C5 capture still decodes in clipping mode across more than 50 dB.
   Open: the receiver needs about 125 KiB of RAM plus about 30 KiB of stack, which is tight
   next to a 56 KiB capture ring on a C5 without PSRAM. Feet vs metres for altitude is
   unresolved. O4 parsing is untested on real signals.
   - New crate `crates/droneid`: resample to 15.36 MS/s, FFT, equalise from the reference
     symbols, demap, descramble, LTE turbo decode, CRC, parse fields. Fixed-point from the
     start so it later runs on a chip.
   - Tested on RUB-SysSec's public Mini 2 and Mavic Air 2 recordings (fetched by a script, not
     copied into the repo: they are AGPL), and cross-checked against anarkiwi's decoder.
   - Also recognises O4's encrypted records and reports their session ID.
2. **10b. C5 raw-capture layer in Rust** — **WRITTEN 2026-11-09, not yet run on a chip:**
   `crates/rawcap`, `firmware/node-sdr` (first-flash steps in its `BRINGUP.md`), `tools/rawcap-pull`.
   The decoder needs PSRAM (XIAO ESP32-C5); `cargo fw-lite` captures without decoding.
   (about 8–12 days; the riskiest part).
   - Shared with Phase 7's C5 FPV detector, so it is built once.
   - Calls the closed radio library we already ship (every needed function is present), routes
     the radio's debug sample bus to pins and back into memory through PARL_IO as a
     continuous 40 MS/s ring, with fixed gain.
   - esp-hal lacks pieces (no debug-bus signal entries, no circular PARL_IO DMA), so some raw
     register work.
3. **10c. Detection on the C5.** Cheap energy trigger on the ring, freeze about 1 ms, confirm
   with the reference pattern. Reports "DJI OcuSync, frequency, signal strength, plaintext or
   O4" to the hub as a new link transport.
4. **10d. Decoding on the C5.** Estimated 50–200 ms per burst, well inside the gap between
   bursts. Fallback: ship the ~40 KB capture to the S3 hub over SPI (about 8 ms).
   - Fallback for signal quality: the 10-bit snapshot capture at 20 MS/s, if the 4-bit ring proves
     too coarse.

**O4 / DroneID v3 status (2026-11-16, branch `droneid-v3`; details in docs/DRONEID-O4.md):**
built and tested on synthetic signals only. The published O4 coding chain matches our decoder bit
for bit (5 of the spec's 7 SHA-256 stage digests; the two floating-point stages are not
reproduced and the reason is unknown, no generator is published), and synthetic O4 packets
decode through the C5 capture model. O4 records now pass the spec's full structural checks
before they count, are shown as `DJI O4 <key id>` sessions with a model-dependent "GPS locked"
/ "probably flying" state, and travel to the hub with their raw logical object (link protocol 8).
node-sdr's DroneID plan adds 5816.5 MHz and follows O4 drones with the published seven-channel
schedule (`droneid::follow`), falling back to scanning. Needs a real O4 drone to confirm.

**Decisions:**
- **Its own C5.** Parked on one DroneID frequency it catches a share of bursts (O4 hops over 7
  frequencies, so roughly one every 4–5 s), enough for presence and tracking. Prefer a C5 with
  built-in PSRAM (XIAO ESP32-C5).
- **O4 stays detect-only, permanently.** No key handling of any kind: decrypting would lose the
  legal "readily accessible" safe harbour and could raise anti-circumvention issues.
- **Plaintext fields shown by default**, like Remote ID, because DroneID is a broadcast made for
  third-party receivers (AeroScope), with a setting to turn decoding off. This is our call, not
  legal advice; revisit before selling kits.
- **Never transmit DroneID**, not even on the bench.

**Testing without a DJI drone:** the public recordings cover 10a. Bench testing 10b–10d needs a
real OcuSync drone with props off and motors armed, or a field visit. A used Mini 2 (plaintext
OcuSync 2) is the cheapest reference.

### Phase 11: Other drone links (Autel, Skydio, digital FPV)
Decided 2026-11-09 (see RESEARCH.md "Autel, Skydio and digital FPV").

**What each system allows:**

| System | Best we can do | How |
|---|---|---|
| wfb-ng / OpenIPC, OpenHD, RubyFPV (open digital FPV) | **Identify system and link** | Their injected Wi-Fi frames carry a fixed cleartext address pattern; Node A/C promiscuous mode |
| Skydio 2/2+/X10 at 20 MHz | Remote ID + Wi-Fi headers | Standard Remote ID beacon (already decoded); 802.11 headers visible, payload encrypted |
| Skydio 5/10 MHz and X10 HE10/HE5 modes | Fingerprint | Raw-capture C5: Wi-Fi preamble timing scaled ×2/×4 |
| DJI FPV, O3, O4 air units | Fingerprint, plus DroneID if they send it | Raw-capture C5: LTE-style symbol timing, width, channel plan |
| Walksnail Avatar | Fingerprint | Same as DJI (Artosyn LTE-style chip); telling the two apart needs recordings |
| HDZero | Fingerprint | Fixed 37 MHz channel grid, 27 or 17 MHz wide, continuous |
| Autel SkyLink 2.4/5.x GHz | Fingerprint | TD-LTE based: 1.1/4.5/9/18 MHz wide on a 1 MHz raster |
| Autel 900 MHz, Skydio X10D 1.6–2.1 GHz | Not reachable | No ESP32 covers it; LR2021 wideband energy at best (1.5–2.5 GHz reaches part of X10D) |
| Video or telemetry content of any of them | Out of scope | Encrypted or proprietary, and legally off-limits |

1. **11a. Open digital FPV over Wi-Fi.** **BUILT 2026-11-09**, untested on radios: `crates/openlink`,
   link protocol v2 `Frame::OpenLink`, Node A/C data-frame path, tracker/phone/CSV/rid-view output.
   IDs hidden unless the hub is built with `show-link-ids` (rid-view: `--show-link-ids`). A classifier for wfb-ng/OpenIPC,
   OpenHD, RubyFPV (and DroneBridge if safely matchable), a new link frame, and tracker, phone
   page and log output.
   - Recognising the system is on by default.
   - Link IDs and vehicle IDs are identifiers, shown only when opted in.
   - Exception (owner decision 2026-11-15): the Phase 13 evidence log always keeps the raw frame
     bytes, IDs included, so a record is complete; display settings don't change it.
   - Bench checks needed: does ESP32 promiscuous mode deliver LDPC/STBC-coded frames
     (wfb-ng's default video profile), and does the C5 receive channel 165?
   - **2026-11-14:** channel 165 answered **yes**; the fake frames of all four systems pass on
     ch 6/149/161/165 (`--group openlink`, docs/BENCH.md). LDPC/STBC stays open: the ESP32 cannot
     transmit it, so it needs real OpenIPC hardware.
2. **11b. One fingerprint classifier on the raw-capture C5** (after 10b, sharing its capture
   layer). It measures, from a capture:
   - the cyclic-prefix autocorrelation lag (64 samples at 20 MS/s = Wi-Fi; about 1333 =
     LTE-style DJI/Autel/Walksnail/Herelink; ×2/×4 Wi-Fi = Skydio narrow modes);
   - occupied width;
   - duty cycle (Wi-Fi is bursty, video links near-continuous);
   - whether the centre matches a known channel plan.

   It reports a family ("LTE-style video link", "HDZero", "narrow Wi-Fi"), not a brand, until
   recordings let us separate brands. The DJI DroneID check (Phase 10) is what confirms "DJI".
   Build it on the host first against public recordings (RFUAV), then tune on our own.
   - **DJI link fingerprints: BUILT 2026-11-15, tested on synthetic signals only** (branch
     `dji-fingerprint`; numbers, sources and limits in docs/DJI-LINKS.md). New `crates/djilink`
     (no_std, fixed point, 12.6 KiB scratch) runs after fpvscan on the same capture: bursts, a
     78 kHz spectrum, and the cyclic prefix folded at the symbol period (finds a 0.45 ms, 1.1 MHz
     controller burst that fpvscan's whole-capture test cannot). Link protocol 7 adds `DjiVideo`,
     `DjiO4Video` (5.170-5.250 GHz, which only O4 DJI products use), `DjiFpvAirUnit` (whole-MHz
     DJI FPV channels) and `DjiController` (narrow 15 kHz OFDM bursts, confirmed on two hop
     centres); tracker, screen, JSON and CSV show them. Wired into node-sdr's FPV job; builds,
     not flashed.
   - **Untested:** everything on real signals. Known limits: "DJI" vs Autel (same LTE-like shape)
     and DJI FPV vs Walksnail (same channel plan) are not separable yet; O3 vs O4 only by the
     5.1 GHz band; the sweep does not visit 5.1 GHz yet (four extra hops listed in
     `djilink::plan::O4_BAND_SWEEP_MHZ`); 20 MHz signals are usually cut by the capture edge, so
     their centre (and the air-unit test) is unknown; on-chip run time unmeasured.
   - **To validate:** RFUAV `DJI MINI4 PRO.rar` (4.95 GB) first, then `DJI AVATA2.rar`,
     `DJI FPV COMBO.rar`, `DAUTEL EVO NANO.rar` (owner approval needed; list in DJI-LINKS.md),
     plus one bench session with an OcuSync drone armed (props off) and an O3/O4 air unit on a
     manual channel.
3. **11c. Channels above 5800 MHz** (HDZero R6–R8, DJI/Walksnail CH6–8) are outside the C5's
   measured raw-capture range. The Phase 7 RX5808 covers them as signal strength on known
   centres.

**Blocked on recordings.** No public dataset has Walksnail, HDZero, OpenIPC or Skydio. See
RESEARCH.md for the recording list and the hardware that unblocks it (about $250 for HDZero +
Walksnail + OpenIPC).

### Phase 12: Flagging flights that are likely breaking FAA rules (added 2026-11-15)
Start after the web-head work. Goal: the hub flags, logs and shows drones (and, from ADS-B, crewed
aircraft) whose reported position and altitude look like a rule violation, using an on-device
database of airspace and restrictions. Works offline from the SD card.

**Wording rule: "likely", never "illegal".** The hub cannot see waivers, LAANC authorizations,
public-safety (COA) operations or Part 107 vs recreational status, so every flag is "possible
violation: <rule>, <evidence>", with the data source and its date. Flags are a log for the owner,
never sent anywhere automatically.

**Checks, in order of value and data cost:**
1. **Height above ground over 400 ft** (Part 107.51(b), 44809(a)(6)). Remote ID gives geodetic
   altitude, often pressure altitude, and height above takeoff or ground. Ground height comes from
   an elevation tile (USGS 3DEP or SRTM, public domain) on the SD card. Allow the 107.51 exception
   (within 400 ft of a structure) only as a softer "high, check for structures" level, since the hub
   has no building data. Include a margin for Remote ID's own altitude accuracy class.
2. **Controlled airspace without a known authorization** (Class B, C, D, E surface): FAA airspace
   shapes from NASR / the FAA ADDS open data (public domain), plus **UAS Facility Map** grid
   ceilings (the LAANC altitude limits). Flag "above the facility-map ceiling" more strongly than
   "inside controlled airspace" (which is often authorized).
3. **Restricted areas:** Special Use Airspace (prohibited/restricted), the DC SFRA/FRZ, stadium and
   national-security UAS flight restrictions, and **TFRs**. TFRs change daily and need a network
   (web head's station mode or a laptop sync); show the database date so stale TFR data is obvious.
4. **Remote ID problems:** a DroneID/Wi-Fi drone seen with no Remote ID, operator position missing,
   or a Remote ID serial that changes mid-flight.
5. **Crewed aircraft (ADS-B):** below the 91.119 minimum safe altitudes (1000 ft over congested
   areas, 500 ft elsewhere) away from airports, using the same elevation tiles. Lower priority; many
   legitimate exceptions (helicopters, approach paths).
6. **Beyond visual line of sight (107.31, 44809(a)(7)), inferred.** Standard Remote ID carries the
   operator's position and its type (live GNSS, takeoff point, or fixed). The rule has no distance
   in it, so this is graded, not a yes/no:
   - drone-to-operator distance against how far a drone of that size can be seen (a small drone is
     hard to see beyond roughly 400–800 m; thresholds are settings, defaults to be justified from
     published visibility studies);
   - **terrain in the way:** the elevation tiles say whether a hill blocks the straight line between
     the operator and the drone (buildings are not in the data, so a city flag stays softer);
   - confidence is lower when the operator position is only the takeoff point (broadcast modules
     often send that) or is missing.
7. **Night and twilight (107.29), inferred.** Sun elevation from the hub's GPS time and position:
   night is between the end of evening civil twilight (sun 6° below the horizon) and the start of
   morning civil twilight. Night flight is **legal** with anti-collision lighting visible for 3
   statute miles (and, for Part 107, the updated training), which radio can't see. So night alone
   is only the lowest level; it raises other flags' levels instead (e.g. far from the operator at
   night). Sun position is computed on the device; no data download.
8. Out of scope: flying over people (no data on people), lighting itself.

**Alert levels.** Each flag has a level that rises as evidence builds up, so a single noisy reading
never shouts:

| Level | Meaning | Example |
|---|---|---|
| 0 Note | context, not a deviation | flying at night; inside Class D (often authorized) |
| 1 Watch | one rule possibly exceeded, briefly or within error margins | 420 ft above ground for 3 s; 600 m from operator |
| 2 Warning | clearly exceeded, sustained | over 450 ft above ground for 30 s; above the facility-map ceiling |
| 3 Alert | strong, combined evidence | over 600 ft above ground and 1.5 km from the operator at night; inside a TFR or prohibited area |

Confidence rises with: how far over the limit (beyond the reported accuracy class), how long and how
often, two independent sources agreeing (Remote ID and DJI DroneID for the same serial), and combined
factors (night raises distance flags one level). Thresholds live in one settings table, documented
with the rule each one comes from, so the owner can tune them. Only the levels change what the screen
and phone show; the log keeps every flag with its raw numbers.

**Data:** a host tool (`tools/airspace-pack`) downloads the public FAA and USGS datasets, cuts a
region (e.g. 100 km around the owner), simplifies shapes, and writes a compact versioned pack to the
SD card; the hub never parses shapefiles. Pack holds: airspace polygons with floor/ceiling, facility
map grid, SUA, a terrain grid, and a dated TFR list. Shared crate `crates/airspace` does point-in-
polygon and height lookups with host tests on known airports (PHL, PNE).

**Outputs:** a flag column and reason in the SD CSV, a "flags" screen page and phone page section,
a CoT remark, and a per-flight summary (worst flag, max height above ground, time inside each area).

**Done when:** test-beacon tracks placed (by fake positions) at known spots around PHL and PNE get
the right flags against the public data, the pack fits on the SD card and lookups take under a few
milliseconds on the P4.

**Status (2026-11-15, branch `faa-flags`):** built and run on the P4. `crates/airspace` (pack
format, lookups, sun, line of sight), `crates/faarules` (checks 1-7, levels, the settings table
with citations in `settings.rs`), `tools/airspace-pack` (FAA ADDS, USGS 3DEP, NGS GEOID18, Census
TIGERweb; a 100 km Philadelphia pack is 662 KB). On the P4 the pack loads from
`ROTORSCP/AIRSPACE.PAK` in ~9 s; one drone's evaluation takes ~3 ms (area lookup 0.3 ms).
Real-data tests: `AIRSPACE_PAK=... cargo test -p airspace-pack --test philly`. Open: TFRs (tfr.faa.gov
refuses the tool's HTTP client; save the files by hand), the VLOS distance defaults rest on two
studies seen only as search snippets, the S3 web-head side is untested on hardware.

### Phase 13: Uploading detections, with evidence-grade integrity (added 2026-11-15)
Goal: the unit uploads drone (and aircraft) detections to a web server, either one on the local
network or a hosted/cloud one, and every record can be shown later to be exactly what the device
received, when, and with an unbroken chain of custody, so it stands up as evidence. Whether a
court admits it is the court's call; the aim is to make authentication (US Federal Rules of
Evidence 901, and 902(13)/(14), self-authentication by a certified process and hash verification)
straightforward and the method open to challenge. Get a lawyer's review before relying on it.

**1. Device identity that can't be copied.**
- Each hub gets its own signing key, generated on the chip and held by the hardware signing unit
  (the P4's ECDSA peripheral with its key in eFuse; the S3/C5 digital-signature peripheral), so
  the key is never readable by software or from the flash. Public key + a fingerprint are printed
  at setup and registered with the server.
- Secure boot and flash encryption so nobody can swap in firmware that forges records. Every
  record carries the firmware's hash. **Burning eFuses is permanent:** do it only on kit units,
  only with the owner's explicit go-ahead per unit, never on the bench boards.

**2. A tamper-evident evidence log, separate from the convenience CSV.**
- Append-only binary log on the SD card. Each record holds: the **raw received bytes** (the Remote
  ID or DroneID message exactly as heard, so anyone can re-decode it), our decode, node ID, band,
  channel, signal strength, the hub's own GPS position and fix quality, time and its source (GPS+PPS,
  GPS, or free-running clock), firmware hash, and a sequence number.
- **Hash chain:** each record includes the SHA-256 of the previous one, so deleting, reordering or
  editing any record breaks every later link.
- **Signed checkpoints** every minute and at shutdown: the device signs (chain head, record count,
  time). A gap in sequence numbers or a missing checkpoint is itself recorded and visible.
- Everything that could matter is logged into the same chain: boot, settings changes, clock
  corrections, GPS loss or jumps (possible spoofing), firmware updates, SD card insert/remove,
  node joins/leaves, pack (airspace data) version, and operator notes.

**3. Time you can defend.** GPS time locked to PPS, with the fix quality in every record; a
free-running clock is marked as such. When online, the chain head is also timestamped by an outside
party (an RFC 3161 time-stamp authority, and/or OpenTimestamps), proving the records existed by
then and weren't written later.

**4. Upload.**
- Offline-first: records queue on the SD card and upload whenever a network exists (the web head's
  station mode, or a laptop). Nothing is lost while offline; the chain proves completeness.
- TLS to the server, with the device authenticating by its hardware key (mutual TLS or signed
  requests), so the server knows which unit sent what.
- The server is our own Rust program (same code for a local box or the cloud). It verifies every
  signature and chain link on arrival, rejects or flags breaks, and stores records **write-once**
  (append-only storage; object-lock in the cloud). It keeps its own access audit log: who viewed,
  exported or annotated what, and when.
- A map and list view of drone locations, and an export button.

**5. Chain of custody and exports.**
- An **evidence export bundle**: the records, the checkpoints, the device's public key and
  registration, firmware hash and version, the outside timestamps, the server's audit trail for
  those records, and a SHA-256 of the whole bundle.
- An **open-source verifier** (command-line, Rust) that anyone, including the other side's expert,
  can run to check every signature, link and timestamp and re-decode the raw messages
  independently of our code.
- A custody record: who held the unit and the SD card, when, signed handovers; and a printable
  summary sheet for a declaration (the 902(13)/(14) route).
- A **method document**: how detection works, its known error rates (from our bench and field
  tests), the limits ("a detection shows a broadcast was received, not who was flying"), and the
  versions used. Reliability arguments (Daubert) rest on this.

**6. Privacy and conduct.** Remote ID includes the pilot's location: personal data. Retention
limits as a setting, access control on the server, and the legal line from Phase 9 still applies
(broadcast data only; no decoding of link contents). Uploads are off until the owner turns them on.

**Done when:** a detection made on the bench travels device → server; the verifier passes on the
export; editing, deleting or reordering any record in the log, the upload or the bundle makes the
verifier fail and say where; and a cold-start with no GPS is visibly marked in the records.

**Status 2026-11-15 (branch `evidence`): the done-when is met on the bench with a development key.**
`crates/evidence` (format, chain, checkpoints, verifier core), the P4 evidence log
(`firmware/hub-p4/src/evlog.rs`, `ROTORSCP/EVIDENCE/`), `tools/evidence-verify`,
`tools/evidence-server` (+ `evidence-upload`). Records reach the server through a laptop (USB
console or the SD card). Still to do: the hardware key and secure boot (eFuse; owner's per-unit
go-ahead), automatic outside time-stamps, upload through the web head, retention settings, real
server accounts. See docs/EVIDENCE.md (plain language) and docs/EVIDENCE-FORMAT.md (byte layout).

### Phase 14: Our own aircraft-signal decoding: ADS-B, Mode S, UAT, and FLARM-family (added 2026-11-15)
Goal: decode crewed-aircraft location signals ourselves instead of trusting the ADSBee's summary,
and add the light-aircraft/glider systems (FLARM and its open relatives) that ADS-B misses.

**14a. Mode S / ADS-B decoder in Rust (`crates/modes`), no new hardware.**
**BUILT 2026-11-15 (branch `modes`):** `crates/modes` (47 host tests, published example
frames from the 1090 MHz Riddle and pyModeS), `tools/modes-live`, P4 feature `modes` (off by
default) and `modesloop`. Live on the laptop against the ADSBee's own decode (15 min): callsign,
squawk and category 100%, altitude 99.7%, position 98.1%; every disagreement checked was the
ADSBee's CSBee value being stale. P4 loop-back with recorded frames matches the host decode.
Not yet: the real ADSBee wired to the P4. Details in docs/ADSBEE.md. 14b/14c/14d research:
docs/AIRCRAFT-SIGNALS.md (UAT first; FLARM content only under licence, presence by default).
- Feed: the ADSBee's raw frame output (raw hex or Beast binary, whichever its firmware offers)
  instead of its CSBee summary. Our code then does everything above the radio: CRC-24 parity and
  address recovery, 1-2 bit error correction on DF17/18, CPR global and local position, velocity,
  identification, Mode C/Gillham altitude, squawk (DF5/21), comm-B (DF20/21, the common BDS
  registers), all-call (DF11), TIS-B and ADS-R (DF18).
- Cross-check against the ADSBee's own decode on the same frames; a mismatch is logged.
- Host tests against hand-made and published example frames; then live on the laptop with the
  ADSBee, then on the P4 (ADSBee UART, Phase 6 wiring).
- Mode A/C replies (no address) are only visible to a raw demodulator: see 14c.

**14b. UAT 978 MHz** (US general aviation below 18,000 ft): same decoder approach (Reed-Solomon
FEC, basic and long ADS-B messages). Needs a 978 receiver: the ADSBee 1090U variant, or 14c. Ours
has no working 978 radio.

**14c. Raw demodulation ourselves (optional, later).** An RTL-SDR on the P4's USB 2.0 host port at
2-2.4 MS/s, with our own 1090 pulse demodulator (and 978 for UAT). Large job (a Rust USB host
driver for the RTL2832U on the P4). Laptop first with the same code, the P4 only if it earns it.

**14d. FLARM family on 868 / 902-928 MHz.**
- **FLARM**: gliders, light aircraft and many helicopters in Europe; used in the US too (US band
  902-928 MHz). Its air protocol is proprietary and obfuscated or encrypted depending on the
  version; FLARM Technology has objected to third-party decoding. **Legal review before decoding
  FLARM content**; presence/energy detection is the safe default (same line as Phase 9).
- **Open relatives, decode freely:** OGN tracker protocol, FANET (LoRa), and ADS-L (EASA's open
  standard for light aircraft and drones, SRD860 band). Check which have any US use.
- Radio: the Phase 9 LR2021 (GFSK and LoRa, 150-960 MHz) can receive all of these, one channel
  plan at a time, so a FLARM-family job is another role for that node; an SX1262 board can too.
- **Done when:** a decoded FLARM-family position from a real transmitter, or recordings, reaches
  the tracker, and the legal decision is written down.

Outputs for all of 14: aircraft on the screen, SD log, USB JSON, phone page and CoT, with the source
(ADS-B, Mode S, UAT, FLARM, OGN, FANET, ADS-L); Phase 12's low-flying check uses them too.

### Later ideas
- DJI legacy beacons (D10, decoder already in `rid::dji`) and drone Wi-Fi fingerprinting (D11) can be added to Nodes A and C at any time.
  - This also catches Wi-Fi toy drones (Tello and similar) by network name and maker address, with no new hardware. Try this before buying an nRF24.
- **nRF24L01+** ($1–8) for the 55 toy-drone protocols it carries (SymaX, Bayang, MJX…), if Wi-Fi matching proves insufficient.
- ~~**SX1280**, only if ELRS racing (FLRC) mode matters.~~ Not needed: the LR2021 (Phase 9) does FLRC.
- RTL-SDR as an optional laptop add-on for a waterfall next to `rid-view`.
- **Skip:** CC1101, CC2500, A7105 and CYRF6936. They duplicate the LR2021 or cover declining protocols.
- Time sync across nodes using the GPS PPS pulse.

### Node chip (2026-11-12)
- **Every node can be a XIAO ESP32-C5.** On the bench, a C5 passed every node role:
  - 2.4 GHz and 5 GHz Wi-Fi Remote ID, beacon and NAN (`node-wifi`, `cargo fw24` for 2.4 GHz);
  - BT4, BT5 extended and Long Range (`node-ble`);
  - raw capture for DroneID (`node-sdr`).
- **One chip per job still applies:** each chip has one radio and can listen to one band at a time.
- **Gains:** one board type for all nodes, one stock-Rust toolchain for them, and 5 GHz plus raw capture that the S3 lacks.
- **Costs:** a younger Rust stack. Two esp-radio C5 bugs were fixed in this repo (BLE transmit clock; receive DC calibration).

### Hub chip (decided 2026-11-09)
- **ESP32-S3 N16R8** (8 MB PSRAM): same toolchain and code as today.
  - PSRAM support in esp-hal is still marked unstable.
  - Octal PSRAM reserves GPIO35–37, so re-check the hub pin map.
- **Not the ESP32-P4.**
  - It has no radio; Wi-Fi comes from a companion C6.
  - Rust can't drive that Wi-Fi.
  - Its PSRAM support in esp-hal is a stub.
  - Its video encoders are C-only.
- **Not the Pico 2 W.** It needs a second toolchain and flasher for kit buyers, and has 2.4 GHz only. Its PIO serial ports are its one real advantage.
- **Watch the ESP32-S31**: Espressif's RISC-V successor to the S3.
  - Hardware: 2× 320 MHz cores with FPU, 4 UARTs, 60 GPIO, high-speed USB, a JPEG encoder, and 8–32 MB in-package PSRAM. Wi-Fi is 2.4 GHz only.
  - Availability: mass production since 2026-07-27. Modules exist (WROOM-1 N16R8V and others), but only AliExpress-class dev boards.
  - Rust (2026-11-12): esp-hal lists it as "early stages"; DMA, PSRAM, USB and JPEG are unstable; esp-radio Wi-Fi is beta. ESP-IDF has open early-silicon issues (PSRAM init, Wi-Fi scan).
  - Decided 2026-11-12 (owner): stay on the S3 for the hub, no parallel S31 prototype for now. Revisit when esp-hal and esp-radio mark the S31 stable, or a XIAO/DevKitC-class board ships.

---

## Rough parts list (Remote ID unit, before ADSBee)

| Part | Qty | Approx. |
|---|---|---|
| ESP32-S3 board (XIAO-class) for Nodes A, B | 2 | $16 |
| ESP32-S3 board **with PSRAM** for the hub (N16R8-class) | 1 | $12 |
| ESP32-C5 board (Seeed XIAO ESP32-C5) for Node C | 1 | $9 |
| Dual-band 2.4/5 GHz antennas (the XIAO C5's stock antenna is 2.4 only) | 3 | $12 |
| MAX-M10S GPS breakout with PPS pin (SparkFun $46; cheaper M10 modules may lack PPS) | 1 | $20–46 |
| 2"-class SPI colour screen | 1 | $10 |
| microSD socket, buzzer, buttons | — | $5 |
| LiPo or 18650 cell + power-path charger | 1 | $18 |
| Carrier PCB + case (3D printed) | 1 | $15 |
| **Total** | | **≈ $117–143** |

The upper end is slightly over the $120 target, mostly because of the GPS board. Choosing a
cheaper M10 module that still exposes PPS closes the gap. The FPV node adds about $9 (a C5) or
$6 (an RX5808). The ADSBee tier's cost depends on ADSBee pricing, which isn't posted now. The
m1090 solder-down module is the integration option for a PCB.

**To order now for Phases 0–3:** one more ESP32-S3 (test transmitter / Node B), two XIAO
ESP32-C5s (Node C, plus a 5 GHz test source or the later FPV node), and dual-band antennas.

---

## Risks to watch

1. **Rust sniffer receiving NAN frames** (Phase 1). This is the earliest proof point. If it
   fails, fall back to Rust on ESP-IDF for that node.
2. **Bluetooth Long Range from Rust** (Phase 2). Neither chip has been demonstrated doing it yet.
3. **Kit firmware updates across 4–5 chips** (Phase 5). The update path has to be designed in,
   not bolted on.
4. **Licensing (decided 2026-11-09: GPL-3.0-or-later).** This matches the FPV code (C5VRX)
   and the ADSBee firmware, so we may port or reuse their code directly, keeping their
   copyright notices. Third-party code must be GPL-3.0-compatible: Apache-2.0, MIT and BSD are
   fine, GPL-2.0-only is not.
5. **Regulation of the test transmitter.** Keep it low-power, use an obviously fake ID, and use
   it indoors on the bench.

## Test signal sources (decided 2026-11-09: the owner has no drone or module)

- **Our own `firmware/rid-beacon`** replaces ArduRemoteID.
  - It is Rust and sends every transport, including 5 GHz on a C5.
  - Research found **no commercial module confirmed to transmit on 5 GHz**, so the C5 build is
    the only 5 GHz source short of a DJI drone.
- **One certified module for ground truth (optional, about $60–90):**
  - Dronetag BS, for BT4 plus BT5 Long Range.
  - BlueMark db120, if still stocked, for Bluetooth plus 2.4 GHz Wi-Fi.
  - Most need a GPS fix before they transmit, so test near a window or outdoors.
- **Free independent receiver:** an Android phone with the "Drone Scanner" app. On iPhone it
  only sees Bluetooth.
- **Free real-world check:** take a unit to a local RC flying field or a popular park. Newer
  DJI models broadcast Wi-Fi beacon Remote ID, and DRONK saw an Avata on 5 GHz channel 149.

## Open questions for you

1. ~~Drone or module on hand?~~ None. See "Test signal sources" above.
2. ~~License?~~ GPL-3.0-or-later (2026-11-09). See risk 4.
3. ~~Other ESP32s on hand?~~ More S3s and C5s arrive the weekend of 2026-11-14.
4. ~~A project name?~~ Rotorscope.
