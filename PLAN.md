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

---

## Rust plan: what's feasible, and the exceptions

The research found **no hard blocker** for Rust on any Remote ID role. Espressif's Rust HAL
reached 1.0 in October 2025, and the ESP32-C5 is supported in it, including 5 GHz.

| Part | Stack | Confidence | Main risk |
|---|---|---|---|
| Node A: 2.4 GHz Wi-Fi RID (S3) | `esp-hal` 1.2 + `esp-radio` sniffer | High | No one has yet shown NAN frames arriving through the Rust sniffer. |
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
- Fixed-sensor extras: TAK CoT, MQTT.
- Check whether the hub's hotspot hurts the sniffers.
- **Done when:** a breadboard unit shows a test drone on the screen and phone, with distance and
  direction, and logs it.

### Phase 5: Kit hardware
- A carrier PCB with the chips as modules or castellated boards, antenna connectors, battery and
  power path, and a case.
- Decide the SPI-bus versus Wi-Fi-link question here.
- Browser flasher that updates every chip, building on the earlier Orbic web-flasher work.
- Build guide.
- **Done when:** someone other than us builds one from the guide.

### Phase 6: ADSBee
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

### Phase 8: LoRa mesh backhaul (several units sharing detections)
- Use a **stock Meshtastic or MeshCore board** (SX1262; Heltec V3/V4, XIAO + Wio-SX1262), wired to the hub's serial port as a modem.
- No mesh code of our own: both are C++, and no Rust stack is usable yet.
- Send two message types:
  - plain-text lines compatible with drone-mesh-mapper, so that ecosystem can read us;
  - a compact binary message for other Rotorscope hubs.
- Airtime is scarce. Send only on a new or significantly changed track, plus a heartbeat every 30–60 s, on a private channel using a 500 kHz preset. Those presets are the ones that clearly meet FCC 15.247.
- **Never share one LoRa radio between backhaul and detection.** A mesh node must keep listening to relay packets, and it is deaf to drone links whenever it transmits.

### Phase 9: Control-link detection node (LR1121)
- One **LR1121** radio covers both bands drone control links use: 900 MHz and 2.4 GHz.
  - Cheapest board: RadioMaster XR1, an ESP32-C3 + LR1121 at about $12–17. Whether it can be reflashed freely is unverified.
  - Alternative: LilyGo T3-S3 LR1121, about $24–31.
- Rust driver: `lora-phy`'s LR11xx driver.
- Detects ExpressLRS (the most common FPV control link) on both bands **without knowing the pilot's binding phrase**:
  - It parks on ELRS's fixed sync channel and tries each rate setting.
  - It also builds energy maps of both bands.
- Also catches mLRS, and the hopping energy pattern of SiK/RFD900 autopilot telemetry.
- It cannot decode Crossfire, Tracer or Ghost; those show up as energy only. It misses ELRS's high-rate racing mode (FLRC), which needs an SX1280.
- **Legal line.** A 2020 DOJ/FAA/FCC/DHS advisory warns that monitoring drone control links can fall under wiretap and pen-register law.
  - Presence and energy detection are the default.
  - Reading a link's identifier (ELRS UID bytes) is **opt-in**.
  - Decoding link *content* is out of scope.
- The hub flags any detection made while its own LoRa backhaul was transmitting.

### Later ideas
- DJI legacy beacons (D10, decoder already in `rid::dji`) and drone Wi-Fi fingerprinting (D11) can be added to Nodes A and C at any time.
  - This also catches Wi-Fi toy drones (Tello and similar) by network name and maker address, with no new hardware. Try this before buying an nRF24.
- **nRF24L01+** ($1–8) for the 55 toy-drone protocols it carries (SymaX, Bayang, MJX…), if Wi-Fi matching proves insufficient.
- **SX1280**, only if ELRS racing (FLRC) mode matters.
- RTL-SDR as an optional laptop add-on for a waterfall next to `rid-view`.
- **Skip:** CC1101, CC2500, A7105 and CYRF6936. They duplicate the LR1121 or cover declining protocols.
- Time sync across nodes using the GPS PPS pulse.

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
- **Watch the ESP32-S31**: Espressif's RISC-V successor to the S3, with hardware JPEG and stable Rust. It entered mass production in July 2026, but no boards were found yet.

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
