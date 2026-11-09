# Research notes (2026-11-09)

The evidence behind [PLAN.md](PLAN.md). Three web surveys produced it. Items marked
*(unverified)* were not confirmed from a primary source. Nothing here has been tested on our
hardware yet.

## Attached hardware

The board on `/dev/ttyACM0` (Espressif USB JTAG, MAC `e8:f6:0a:81:6d:f0`) is an **ESP32-S3**,
revision v0.2. It has 4 MB of embedded flash and 40 MHz crystal. Secure boot and flash
encryption are off. Identified with `espflash board-info`. It is not registered in `lab`, so
nothing locks it against other agents.

## Remote ID receivers that exist

| Project | Chip | What it hears | Notes |
|---|---|---|---|
| [opendroneid-core-c](https://github.com/opendroneid/opendroneid-core-c) | — | Decoder library | Apache-2.0. The reference decoder everyone uses; its test vectors are our ground truth. |
| [isaacbentley/orecchino-esp32](https://github.com/isaacbentley/orecchino-esp32) | C3/C6/S3 | Beacon, NAN, BLE legacy **and BLE5 coded PHY**, GB 46750 | GPL-3.0. The best software reference. Measured the Wi-Fi/BLE time-share penalty. |
| [probablytheguy/DRONK](https://github.com/probablytheguy/DRONK) | XIAO C5 | Beacon + NAN, 2.4 **and 5 GHz** | Apache-2.0. The only field-verified 5 GHz receiver found. Heard a DJI Avata on channel 149. Documents the C5 `sig_len` +4 bytes quirk. |
| [colonelpanichacks/drone-mesh-mapper](https://github.com/colonelpanichacks/drone-mesh-mapper) | XIAO S3 / C5 | Beacon, NAN, BLE **legacy only** | MIT, 244★. Meshtastic relay, Flask map, web flasher. C5 build hops 6 + 149–165 at 50 ms. |
| [andylee77/RID_Scanner](https://github.com/andylee77/RID_Scanner) | S3 | NAN, beacon, BLE legacy + coded config | No license. Rate-limits to ~1 Hz per drone. |
| [lnxgod/friendorfoe](https://github.com/lnxgod/friendorfoe) | 3× S3 | BLE + Wi-Fi | MIT. **Same one-chip-per-radio + display-hub shape as our plan.** |
| [alphafox02/DragonSync](https://github.com/alphafox02/DragonSync) (WarDragon) | Linux host | Wi-Fi 2.4/5 + BLE LR via Sniffle dongle | Apache-2.0. Outputs ZMQ → TAK/MQTT/Home Assistant. Accepts ESP32 JSON over UART. |
| [siercks/pocket-rid](https://github.com/siercks/pocket-rid) | S3 | Beacon only | **Rust**, Apache-2.0. Uses esp-hal 1.1 + esp-radio sniffer. Its `no_std` decoder was fuzzed 10⁶ times against core-c with zero mismatches. |
| [ArduPilot/ArduRemoteID](https://github.com/ArduPilot/ArduRemoteID) | S3/C3 | **Transmitter**: beacon, NAN, BLE4, BLE5 LR | GPL-2.0. Our bench test source. |

**Gaps nobody fills yet:**
- A coordinated multi-chip set covering NAN, beacon, BLE Long Range and 5 GHz together.
- A complete Rust receiver.
- Hardware-proven BLE coded-PHY reception on the S3.
- An ESP32 decoder for DJI's legacy Wi-Fi DroneID (OUI `26:37:12`; only Kismet parses it).
- Time sync across receivers.

## How Remote ID is caught

- **Wi-Fi NAN:** action frames to `51:6f:9a:01:00:00`. On 2.4 GHz they're always on channel 6.
- **Wi-Fi beacon:** vendor element with OUI `FA:0B:BC`, type `0x0D`. Beacons can be on any
  channel. On 5 GHz, channel 149 is the anchor.
- **Bluetooth:** service data UUID `0xFFFA`, app code `0x0D`. Extended scan on 1M + coded PHY
  covers both BLE4 and BLE5 Long Range.
  - Turn the duplicate filter **off**: repeated adverts carry fresh positions.
  - On ESP-IDF, start Bluetooth **before** Wi-Fi to leave the BT controller enough RAM.
- **Channel plans in use:**
  - Orecchino: 75% on channel 6.
  - DRONK: 250 ms dwell, with 6 and 149 taking 4 of every 6 visits.
  - droneid-go: `6,149`.
- **Receiver design:** keep the radio callback tiny (copy the frame into a queue), decode in a
  task, and rate-limit output to about 1 Hz per drone.
- **C5 quirks:**
  - One band at a time; set the band explicitly.
  - The country code limits which 5 GHz channels are allowed.
  - A forum report says the filter masks leak non-management frames, so parse the frame-control
    field yourself.
- **Format drift:** DJI's 2026 firmware may carry GB 46750-2025 inside the same Wi-Fi element,
  which core-c alone won't decode *(per Orecchino)*.

## Rust on ESP32 (checked against source, 2026-11-09)

- **Versions:**
  - `esp-hal` **1.2.2**. 1.0 has been stable since 2025-10-30. Radio and many peripheral APIs
    still need the `unstable` feature; pin `~1.2`.
  - `esp-radio` **1.0.0-beta.2**. It replaces `esp-wifi`.
  - `trouble-host` 0.8 for BLE. `bleps` is gone from crates.io.
- **Sniffer:** `WifiController::sniffer()` → `set_promiscuous_mode`, `set_receive_cb`,
  `set_channel`, and `set_band_mode(_5G)` on the C5.
  - Management frames are delivered.
  - NAN through this path is **untested** in Rust.
- **Channel 149:** esp-hal PR #6494 (merged 2026-10-15, unreleased) moves the default country to
  "01", which rejects channel 149 for access points. Always set `CountryInfo` to "US".
- **BLE:** `Scanner::scan_ext()` with `PhySet::M1Coded`.
  - The C5 adapter is configured for 1M + coded.
  - The C5 extended-scan assert was fixed in esp-radio 1.0.0-beta.0.
  - Coded-PHY scanning on the S3 is **unverified**.
- **Fallback:** `esp-idf-svc` 0.53 (now in the [esp-rs/esp-idf](https://github.com/esp-rs/esp-idf)
  monorepo). It is community-maintained, on ESP-IDF 5.5.5, and supports the C5. NimBLE extended
  scan works via `esp32-nimble` 0.13.
- **Toolchain:** the S3 (Xtensa) still needs `espup` and Espressif's rustc fork. The C5
  (RISC-V) builds on stock stable Rust.
- **Crates:**
  - Avoid `remote-id` 0.1.2: it is GPL, and Message Pack is `todo!()`, so it panics.
  - `opendroneid` 0.1.4 is FFI and not `no_std`.
  - Use `ublox` 0.10 (no_std; protocol 34 coverage for the M10 unverified) and `nmea` 0.8 (no_std).

## FPV and ESP32 raw-radio capture

- **[Twotoz/C5VRX](https://github.com/Twotoz/C5VRX):**
  - A XIAO ESP32-C5 plus seven resistors receives analog 5.8 GHz FPV video and outputs
    composite video.
  - It reads the Wi-Fi radio's diagnostic sample path at 40 MS/s, with FM demodulation in the
    C5's BitScrambler.
  - 48 channels (bands R/A/B/E/F/L), with a scan that finds the strongest.
  - C on ESP-IDF 6.0.2, GPL-3.0, experimental.
- **[ESPARGOS/esp-sdr](https://github.com/ESPARGOS/esp-sdr):**
  - Raw I/Q capture through an undocumented dump engine, on many ESP32 chips.
  - Covers 2.2–2.7 GHz on all of them, and **4.8–6.0 GHz only on the C5**.
  - Captures come in bursts of about 16k samples (≈205 µs at 80 MS/s).
  - C, GPL-3.0. Has a SoapySDR driver.
- **No Rust implementation** of either exists.
- **Takeaways:**
  - A dedicated C5 is a credible analog-FPV detector.
  - Digital FPV (DJI O3/O4, Walksnail) would only show as energy.
  - 900 MHz is unreachable on any ESP32.
- **Alternative:** an RX5808 analog receiver module (SPI tuning, analog signal-strength output),
  the part used in FPV lap timers.

## ADSBee

- **Repo:** [PantsForBirds/adsbee](https://github.com/PantsForBirds/adsbee), GPL-3.0, C++.
- **1090 family:** RP2040 + ESP32-S3 + CC1312 (978 MHz), about 1 W.
- **Integration option:** the m1090 solder-down module.
- **Prior art:** the vendor's "Winglet" was a Mode S + UAT + 2.4 GHz Remote ID receiver. Its
  crowdfunding failed at $21.6k of $87.5k.
- **Host feed:** `AT+PROTOCOL_OUT=COMMS_UART,CSBEE`, `AT+LOG_LEVEL=SILENT`, `AT+SETTINGS=SAVE`.
  The vendor calls CSBEE the easiest format to decode. The user's own ADSBee 1090 already
  outputs CSBEE.
- **GPS input:** the ADSBee has one, so a single GPS output wire can feed both it and the hub.
- **Regulation:** small drones are generally prohibited from ADS-B Out (14 CFR 107.53,
  91.225(i)). ADS-B here is for crewed-aircraft awareness.
- **Prices:** not currently posted *(unverified)*.

## GPS: u-blox MAX-M10S

- **Interfaces:** UART (9600 NMEA default) and I2C at 0x42. No SPI. Use UBX NAV-PVT.
- **Power:** under 25 mW tracking.
- **Boards:** the SparkFun Qwiic breakout ($45.95) exposes PPS. Cheap FPV M10 modules ($13–30)
  often don't *(unverified per model)*.
- **PPS:** wired to every chip, it gives sub-millisecond agreement between their timestamps.

## Commercial comparison

- **BlueMark DroneScout:** the ds230 sensor was about €950 ex VAT in 2022. Bridge units are
  about $79–139. A 2026 CVE covers missing TLS validation on firmware update.
- **WarDragon:** commercial kit hardware with the open-source DragonSync software.

## Expansion research (2026-11-09, second round)

### Hub chip
- **ESP32-S3 N16R8.**
  - esp-hal 1.2 `psram` is unstable; manual config is more reliable than auto-detect.
  - Register both an internal and a PSRAM heap region, since Wi-Fi DMA may need internal RAM *(unverified for esp-radio)*.
  - Octal PSRAM takes GPIO35–37.
- **ESP32-P4.**
  - Arrived in esp-hal 1.2.0-rc.0. Only GPIO/UART/I2C/SPI are stable; PSRAM is a stub.
  - No Wi-Fi in esp-radio, and no Rust host for esp-hosted over SDIO.
  - The H.264/JPEG encoders are C components (`esp_h264`).
  - esp-hal needs chip revision v3.0+.
  - Boards cost $14–90.
  - Not worth it for the hub.
- **Pico 2 W.**
  - `cyw43` 0.7 AP mode works, but it allows about 4 clients *(unverified)*.
  - PIO gives about 10 extra serial inputs.
  - Needs a second flash flow (UF2).
- **ESP32-S31.**
  - RISC-V, 320 MHz, hardware JPEG, stable Rust.
  - Mass production since 2026-07-30; no boards found yet.

### FPV: C5VRX internals and a Rust port
- **RF bring-up** uses the ESP-IDF Wi-Fi stack, then closed-library PHY calls (`phy_force_rx_gain`, `phy_set_freq`, `lmac_stop_hw_txq`…).
- **The sample path:**
  - The MODEM_DIAG debug bus is routed through the GPIO matrix onto 8 pins, as 4-bit I + 4-bit Q.
  - PARL_IO RX reads them at 40 MS/s into a 32 KiB DMA ring.
  - PARL_IO TX sends the ring through the **BitScrambler**, whose lookup-table state machine does the FM demodulation.
  - The result drives a 6-bit resistor DAC. No CPU touches the samples.
- **It has no Wi-Fi streaming of any kind**; its web controls work over Web Serial.
- **Rust fit:**
  - Our pinned `esp-wifi-sys-esp32c5` 0.3.0 libphy.a has the exact SHA C5VRX requires, and every symbol it uses.
  - **Blockers for a video port:**
    - no esp-hal BitScrambler driver (issue 5170);
    - no circular PARL_IO DMA;
    - no MODEM_DIAG signals in the GPIO table;
    - the demodulator table is GPL.
  - Video parity would take 35–60 engineer-days. Detection only would take 6–10.
- **Rebroadcast options:**
  - Time-slicing on the C5 gives at best a still every 1–2 s.
  - Tapping C5VRX's digital "Clock Pin D10" output (20 MS/s, 6-bit) with an S3's LCD_CAM, then software JPEG, is the realistic route.
  - The S3 ADC (about 83 kSPS) is far too slow for composite video.
- **Legal:** receiving unencrypted analog video is generally lawful in the US (18 U.S.C. §2511(2)(g)(i)). Recording or publishing pilots' feeds raises privacy concerns.

### Control links and LoRa
- **ExpressLRS on 900 MHz** (as of 4.1.0):
  - LoRa at 500 kHz, SF5–SF9 by packet rate, plus GFSK 1000 Hz on LR1121.
  - 40 hop channels from 903.5 to 926.9 MHz.
- **ExpressLRS on 2.4 GHz:**
  - LoRa at 812 kHz, SF5–SF8, plus FLRC on SX1280 only.
  - 80 channels.
- **The sync channel is fixed at the middle hop index** (about 915.5 and 2440.4 MHz).
  - Sync packets carry the rate, the telemetry ratio and UID4/UID5.
  - LoRa sync word 0x12. IQ inversion depends on a UID bit, so try both.
  - Presence detection without the binding phrase is realistic; no published detector does it yet.
- **SX1262 vs SX127x:** they reportedly interoperate at 500 kHz SF6–9 with implicit headers *(unverified on hardware)*.
- **Other links:**
  - mLRS is the same approach as ELRS.
  - Crossfire, Tracer and Ghost show as energy only.
  - SiK/RFD900 GFSK hopping is visible as a pattern.
  - DJI OcuSync/O4 can't be decoded by any of these chips.
- **Prior art:**
  - KonradIT/ELRS-Telemetry-Sniffer-Ground-Station (needs the binding phrase; GPL).
  - SkySweep32 (CC1101 + SX1281 + RX5808 energy; GPL; unvalidated).
  - NCC Group's 2022 ELRS advisory.
- **drone-mesh-mapper backhaul format:** Meshtastic serial TEXTMSG at 115200 baud:
  - `Drone: <mac> RSSI:<n> https://maps.google.com/?q=<lat>,<lon>`, then a matching `Pilot:` line.
  - At most one message per 5 s.
- **Airtime** for a 45-byte message: LongFast 600 ms, MediumFast 170 ms, ShortFast 50 ms.
  - US 15.247 compliance is clearest at 500 kHz bandwidth with under 0.4 s dwell. MeshCore issue #945 raises the concern.
- **Rust drivers:** `lora-phy` 3.x covers sx126x, sx127x and lr1110 (the LR11xx family); it has no SX1280 driver. There are no usable Rust Meshtastic or MeshCore stacks.
- **DOJ/FAA/FCC/DHS advisory (Aug 2020):** monitoring drone control links may implicate the Wiretap Act and the Pen Register statute. Keep identifier extraction opt-in, and keep content decoding out of scope.
