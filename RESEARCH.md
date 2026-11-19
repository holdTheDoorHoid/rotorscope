# Research notes (2026-11-09)

The evidence behind [PLAN.md](PLAN.md). Three web surveys produced it. Items marked
*(unverified)* were not confirmed from a primary source. Nothing here has been tested on our
hardware yet.

## Attached hardware

The board on `/dev/ttyACM0` (Espressif USB JTAG, MAC `aa:bb:cc:dd:ee:01`) is an **ESP32-S3**,
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

**Reuse under our GPL-3.0-or-later license (decided 2026-11-09):** GPL-3.0 projects above
(orecchino-esp32, C5VRX, ESP-SDR, ADSBee) can be ported or copied with their notices kept.
ArduRemoteID (GPL-2.0) and andylee77/RID_Scanner (no license) still may not be copied.

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
  - DJI OcuSync/O4 can't be decoded by any of these LoRa-class chips (see "DJI OcuSync and DroneID" for the C5 route).
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

## DJI OcuSync and DroneID (2026-11-09, third round)

Two research agents; primary sources read where noted, *unverified* marks the rest.

### Open projects
| Project | License, language | Receiver | Tested | Output |
|---|---|---|---|---|
| [RUB-SysSec/DroneSecurity](https://github.com/RUB-SysSec/DroneSecurity) (NDSS 2023) | AGPL-3.0, Python | USRP B205-mini, 50 MS/s | Mini 2, Mavic Air 2, Mavic 2 (OcuSync 2) | Full fields. **Public recordings in `samples/`** (`mini2_sm` 5.8 MB, `mavic_air_2` 1.8 MB) |
| [proto17/dji_droneid](https://github.com/proto17/dji_droneid) | MIT, MATLAB + C++ | B205-mini, 30.72 MS/s | Mini 2 | Turbo decode; deframer unfinished. Recordings withheld |
| [anarkiwi/samples2djidroneid](https://github.com/anarkiwi/samples2djidroneid) | Apache-2.0, Python | float32 IQ at 15.36 or 30.72 MS/s | OcuSync 2 only | Full fields. Good test oracle |
| [alphafox02/antsdr_dji_droneid](https://github.com/alphafox02/antsdr_dji_droneid) → dragonsdr_dji_droneid | Unstated; vendor firmware | AntSDR / DragonSDR (AD9361) | Mini 2, Mini 3 Pro, Air 2S, Mavic 3 plaintext; Mini 5 encrypted | JSON over ZMQ, Kismet source. Paid closed O4 decryption add-on |
| [luyii-code-1/dji-ocusync-droneid-research](https://github.com/luyii-code-1/dji-ocusync-droneid-research) | GPL-3.0, Python + C | HackRF int8, 15.36 MS/s | O2 chain; Mini 5 Pro (O4), Inspire 3 (encrypted O3) | O4 records recovered, CRC-valid; decrypts only with a supplied key |
| [EdwardBlair/dji-droneid-spec](https://github.com/EdwardBlair/dji-droneid-spec) | No license; prose spec | — | O4/O4+ | PHY, coding, record formats and crypto |

No Rust and no ESP32 implementation exists. Aaronia sells a commercial OcuSync 1–3+/Lightbridge option (*unverified* marketing).

### The burst
- LTE-like OFDM: 15.36 MS/s, 1024-point FFT, 15 kHz spacing, 600 QPSK data carriers plus DC, about 9 MHz occupied. Same waveform on 2.4 and 5.8 GHz.
- 9 symbols (≈643 µs; CP 80/72…/80) or 8 symbols (≈572 µs) on older OcuSync and on O4.
- Zadoff-Chu reference symbols, roots 600 and 147. Root 600 alone is reportedly enough to detect a burst.
- Coding: LTE Gold scrambler (c_init 0x12345678), 173-byte input + CRC-24A → K = 1408, LTE turbo (QPP f1 = 43, f2 = 88), rate match to 7200 bits, inner DJI CRC-16. No pilots, so phase is tracked from the ZC symbols (proto17's notes).
- Payload (O2, NDSS Fig. 13): serial (16 chars), drone lat/lon/altitude/height, velocities, yaw, GPS time, pilot (controller) lat/lon, home lat/lon, product type, UUID.
- Interval about 640 ms (0.6–3 s seen). Bursts only while the motors run.
- Frequencies: O2 2399.5, 2414.5, 2429.5, 2444.5, 2459.5 / 5756.5, 5776.5, 5796.5 MHz (proto17). O4 hops per packet over 2429.5, 2444.5, 5756.5, 5776.5, 5796.5, 5816.5, 2414.5 (EdwardBlair). Bursts appear on 2.4 GHz even with video forced to 5 GHz. No 5.1 GHz DroneID channel documented.

### Generations and encryption
- Wi-Fi drones (Spark, Mavic Air, Mini SE, Tello): the 802.11 vendor element we already decode (`rid::dji`).
- OcuSync 2: plaintext (NDSS, primary).
- O3/O3+: plaintext on Mini 3 Pro, Air 2S, Mavic 3 (alphafox02 README only); Inspire 3 encrypted (luyii); O3 Air Unit, Avata, DJI FPV *unverified*.
- O4/O4+: encrypted. A CRYP record wraps a random AES-128 session key to an AeroScope SM2 public key; INFP records carry the telemetry in AES-128-CTR. Cleartext: record type, a 4-byte key ID that links one session's packets, IV, lengths, CRCs. The private key stays in the AeroScope module's TEE. No public key or method.
- Model-to-generation mapping comes mostly from retailer listings.

### Detecting the link itself
- Video downlink OFDM 10/20/40 MHz (O3 Air Unit up to 40 MHz); O4 Air Unit up to 60 MHz and uses 5.170–5.250 GHz. 60 MHz or 5.1 GHz activity points to O4.
- Uplink control: narrowband hopping, 0.4–5 MHz, 0.5–5 ms bursts (arXiv 2505.18167).
- Pitfall: an energy detector "confirmed" a link in half the scans where only the controller was on (arXiv 2609.25294). The ZC match is the reliable test.
- Datasets: DroneRF, DroneDetect, DroneRFa, RFUAV (Apache-2.0).
- Other links (Autel SkyLink, Skydio, Walksnail, HDZero): no open decoders; energy only.

### Can a C5 do it?
- **Two capture paths**, both driven through the closed radio library we already ship:
  - **Debug bus → GPIO → PARL_IO:** continuous 40 MS/s, 4-bit I + 4-bit Q (PARL_IO is only 8 bits wide), no anti-alias filter (set the low-pass ≤ 20 MHz), fixed gain. 56 KiB ring ≈ 1.43 ms. esp-sdr-zephyr ran 40 MB/s for 40 s with no gaps. CPU budget about 6 cycles per byte.
  - **Dump engine (esp-sdr's method):** 10-bit I/Q + gain index, 16,380-sample snapshots: 822 µs at 20 MS/s, enough for a whole burst; 412 µs at 40 MS/s is not. The CPU loses 128 KiB of SRAM while it runs.
- Neither runs at 15.36 MS/s; resample 20 or 40 MS/s by 96/125.
- Tuning: any whole MHz (`phy_set_chanfreq`); shift the half-MHz digitally, which also moves the burst off the DC spike. Retune 0.23–0.7 ms. C5 receives 2200–2650 and 5180–5800 MHz (measured).
- AGC is built for Wi-Fi and oscillated above 5.3 GHz (esp-sdr issue #5); use manual gain.
- 4-bit I/Q gives about 20 dB quantisation SNR, enough for turbo-coded QPSK, but only 20–24 dB between noise and clipping, so gain must suit the scene.
- OFDM has been decoded on these paths (esp-sdr-zephyr, S3, MER 27–31 dB over coax). Nobody has done LTE, Zadoff-Chu or DroneID on an ESP32.
- 2.4 GHz board spurs at 2400, 2440, 2448, 2480, 2496 MHz.
- Coexistence: one LO. *Untested idea:* parked on channel 149 (5745 MHz), DroneID at 5741.5 is in-band, so promiscuous Remote ID might still work alongside. Plan a dedicated C5.
- RAM 384 KB, no FPU (use int16). C5HR8 (XIAO C5) has 8 MB PSRAM in-package; C5HF4 has none.
- **Compute:** a C5 int16 1024-FFT ≈ 0.8 ms; S3 esp-dsp ≈ 65 µs. Full-rate correlation doesn't fit; energy trigger then frequency-domain ZC check ≈ 5–10 ms per candidate. Full decode ≈ 50–200 ms on a C5, 20–50 ms on an S3 (estimates). A capture is 32–41 KB: ≈ 8 ms over 40 MHz SPI, ≈ 70 ms over 5 Mbaud UART.
- **Rust gaps:** esp-hal's C5 signal table stops at 105, so the debug-bus outputs (106+) need raw `GPIO_FUNC_OUT_SEL` writes; circular PARL_IO DMA needs raw work; no BitScrambler (not needed here); `librftest.a` (the dump engine) isn't shipped, but its registers are documented by esp-sdr-zephyr. The radio-library ABI must match its version (widscan58's `check_phy_abi.sh` idea). No no_std turbo decoder exists; `pccc` and `comnum` (std) serve as host oracles.
- **Alternative:** an S3 can capture 2.4 GHz gaplessly at 16 MS/s, 10-bit (esp-sdr `docs/iq-stream.md`). The ESP32-S31 gives 8-bit I/Q at 40 MS/s, but only 2.15–2.85 GHz.

### Sources (C5 side)
[ESPARGOS/esp-sdr](https://github.com/ESPARGOS/esp-sdr) (GPL-3.0-or-later; it says the capture mechanism itself isn't copyrightable),
[cyber-murmel/esp-sdr-zephyr](https://github.com/cyber-murmel/esp-sdr-zephyr) `doc/hardware-quirks.md`,
[Twotoz/C5VRX](https://github.com/Twotoz/C5VRX) (GPL-3.0-only; asks for credit in README when code or findings are reused),
[CapnBry/widscan58](https://github.com/CapnBry/widscan58), esp-dsp benchmarks, ESP-IDF `soc_caps.h`.

### Legal (not legal advice)
- 2020 interagency advisory: collecting identifiers/telemetry may implicate the Pen/Trap statute; RF-energy sensing is safest.
- 18 U.S.C. §2510(16): encrypted transmissions aren't "readily accessible to the general public", so decrypting O4 loses that safe harbour; plaintext O2 is a closer call. Using a key pulled from DJI hardware could raise DMCA §1201 issues.

### Findings from building the decoder (10a, 2026-11-09)
Measured on the two public recordings and on synthetic bursts:
- **Results:** both recordings decode, 13 of 13 bursts CRC-valid (NDSS reported 7 of 10 for
  `mini2_sm`), all on the systematic-bits path with 0 raw bit errors. The turbo path gives
  identical blocks and gains about 8 dB on synthetic bursts.
- **Correction: root 600 alone does not fix the frequency offset.** A whole-carrier offset of
  d looks like a timing shift of about 1.7·d samples. Root 147 resolves it.
- **Correction: 4-bit capture.** In clipping (top-bits) mode it decodes from 2 dB to beyond
  54 dB per-carrier SNR, because the turbo path recovers hard-clipped bursts. Only the
  wrap-around window needs gain held within about 16 dB. Any truncating capture needs DC
  removal; without it the half-LSB bias costs 4.5 dB.
- **10-bit at 20 MS/s** (822 µs snapshot) decodes across the whole range on the cheap path.
- **Sensitivity is limited by detection, not decoding:** at 0 dB, 11 of 13 bursts decode when
  given the true timing, but the energy trigger finds none.
- **Layout:** in the 9-symbol layout the first symbol carries no coded data; data is in symbols
  1, 2, 4, 6, 7, 8. Standard LTE QPSK mapping works once phase comes from the reference
  symbols.
- **Records:** OcuSync 2 also sends a plaintext 0x11 record (141 bytes: serial plus unknown
  fields). The O2 flight record uses the same length-byte + CRC-16 envelope as O4's spec.
- **Altitude units:** feet vs metres is unresolved. raw ÷ 3.281 reproduces the paper's output.
- **The recordings** are concatenated detector output (bursts about 90 µs apart, at +2.0,
  −12.47 and +9.61 MHz), so they say nothing about the real burst interval.
- **Cost:** host per burst is about 2 ms on the cheap path, about 4 ms worst case
  (19 FFTs, about 600k multiply-accumulates resampling, about 113k operations per turbo
  half-iteration). Scaled C5 estimate 40–150 ms (not measured).

## Autel, Skydio and digital FPV (2026-11-09, fourth round)

Two research agents. FCC test reports were read directly where cited; *unverified* marks the rest.

### Autel SkyLink
- **TD-LTE based.** The FCC module manual (FCC ID 2AGNTMTBL, module M240958L) says "developed
  based on the 4G TD LTE technology platform". It names MCU **LC1881** (Leadcore lineage,
  *unverified*). It advertises "encrypted transmission" and one band at a time with adaptive
  channel choice. Fast hopping is *unverified*.
- **FCC reports:**

  | Model (FCC ID) | Bands | Modes and measured bandwidth |
  |---|---|---|
  | EVO II (2AGNTMC2409A) | 909–921, 2407.5–2471.5 MHz | OFDM 5/10/20 MHz modes, 6 dB widths 4.54/9.08/18.07 MHz, 1 MHz raster |
  | EVO Lite/Lite+ (2AGNTMDXM2409A) | 2.4, 5154–5246, 5728–5847 MHz | 1.4/10/20 MHz modes, 1.10/8.98/18.04 MHz; also a real 802.11n/ac radio |
  | EVO Max (2AGNTMDX240958A) | 904–926, 2.4, 5.2, 5.8 GHz | 1.4/10/20 MHz modes, QPSK/16QAM; also Wi-Fi and a 24 GHz radar |

- **The widths match LTE at 15 kHz spacing** (72/300/600/1200 subcarriers = 1.08/4.5/9/18 MHz).
  So: 66.67 µs symbols, normal CP ≈ 4.69 µs, FFT 128/1024/2048. *Inferred, not measured from I/Q.*
- EVO II test mode was on about 4.05 ms per 5.01 ms (TDD-like).
- RFUAV saw the EVO Nano as a 20 MHz video signal at 5.77 GHz plus a 4 MHz hopping signal with
  4 ms bursts.
- **No DroneID-like broadcast** found; no open detector or decoder. Commercial counter-drone
  vendors claim Autel identification (*unverified*).
- Remote ID: yes on EVO II V3, Max and Lite/Lite+ (firmware updates), reportedly over Bluetooth
  only (*unverified*).
- 900 MHz modes are unreachable on ESP32.

### Skydio
- **Skydio 2/2+** (2ATQRSDRC2V1): 802.11 at 2.4/5.2/5.8 GHz, 20/40 MHz, plus 5 and 10 MHz modes
  ("Skydio Link") added by a 2020 FCC change. The controller module (2ATQRSMO5GV1) is 2×2.
- **X10, SkydioLink Gen 3** (2ATQRSMODBV3): modified 802.11ax with HE20, HE10 (half clock) and
  HE5 (quarter clock), 2.4 and 5.18–5.845 GHz, AES-256.
- **X10D / X2D "Connect MH"**: 1625–2500 MHz in several bands, 4 MHz channels on X2D; vendor not
  named (Doodle Labs is a guess). X2D is not Remote ID compliant.
- **Remote ID:** sent as a standard Wi-Fi beacon (patent US 2023/0294848: the radio steps off
  its link channel to beacon). 2/2+/X2E got it in June 2023; on by default on X10. Rotorscope
  should already see it.
- HE20 headers are readable by ESP32 promiscuous mode; HE10/HE5 aren't, but in raw I/Q the
  short-training-field period scales 0.8 → 1.6 → 3.2 µs, a cheap autocorrelation test.

### Digital FPV
- **Open systems send ordinary 802.11 data frames** (frame control `08 01`, broadcast receiver):
  - **wfb-ng / OpenIPC:**
    - Source address `57:42:LL:LL:LL:PP` ("WB" + 3-byte link ID + stream number).
    - Link ID = SHA-1(link_domain)[0..3]; the default "default" gives `75:05:d6` (checked).
    - Streams 0–127 are drone→ground (video 0–15, MAVLink 16–31, tunnel 32–47); 128–255 are uplink.
    - Payload encrypted (ChaCha20-Poly1305).
    - Default channel 165 (5825 MHz), MCS1 with STBC + LDPC on video; OpenIPC often uses 161.
  - **OpenHD:**
    - addr2/addr3 = `[01 air | 02 ground][nonce…][port]`, first and last bytes equal.
    - Default 5745 MHz, MCS2; default bind phrase "openhd".
  - **RubyFPV:**
    - addr2 = addr3 = `13:12:34:56:78:90`; addr1 broadcast with an odd port byte.
    - Its header carries cleartext vehicle IDs while encryption is off; the README says
      encryption is "temporarily disabled".
  - **DroneBridge:** a cleartext 10-byte header after a data or RTS frame.
- **DJI FPV / Vista channels:** 5660, 5695, 5735, 5770, 5805, 5878, 5914, 5839 MHz (FCC; CE only
  CH3–5 and 8).
  - O3: 5.725–5.850 GHz, 10/20/40 MHz.
  - O4: also 5.17–5.25 GHz, with 60/40 MHz at 5794.5 and 10/20 MHz at 5768.5/5789.5/5814.5.
  - LTE-derived baseband; video proprietary.
  - Whether air units send DroneID is unknown: they have no GPS. Point the Phase 10 receiver at
    one to settle it.
- **Walksnail Avatar:**
  - Artosyn AR9201/AR8211 with a Ceva LTE baseband (FPVWiki, secondary).
  - 8 channels; the plan is inferred to match DJI's.
  - 20 MHz, or 40 MHz in the 50 Mbps mode.
  - No public PHY parameters.
- **HDZero** (open VTX firmware, `hd-zero/hdzero-vtx` `src/common.h`, `dm6300.c`):
  - R1–R8 at 5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917 MHz (37 MHz spacing).
  - Also E1 5705, F1 5740, F2 5760, F4 5800, and a low band 5362–5621 MHz.
  - 27 MHz "wide" by default, 17 MHz narrow mode.
  - Modulation undocumented. No pairing; continuous outside pit mode.
  - FCC 2A2O6HDZ3100.
- **Shared chip families:**
  - Leadcore-lineage LTE: DJI OcuSync 1/2 (LC1860C), Herelink (Pinecone S1), Autel (LC1881,
    *unverified*).
  - Artosyn: Walksnail, and others.
  - One 15 kHz-spacing cyclic-prefix detector flags the whole LTE-style family.
- **No open decoder exists** for Walksnail, HDZero, DJI FPV video, Autel or Skydio links.
  Commercial detectors claim some (*unverified*).

### Coverage gaps
- C5 raw capture measured to 5800 MHz.
  - Above that: wfb-ng channel 165, DJI/Walksnail CH6–8, HDZero R6–R8. Normal Wi-Fi mode on
    channel 165 needs a bench check.
  - The RX5808 covers the rest as signal strength.
- 900 MHz (Autel) and 1.6–2.1 GHz (Skydio X10D) need other radios. The owner's Airspy R2 (to
  1.8 GHz, 10 MS/s) can record Autel's 5/10 MHz modes at 900 MHz for study.

### Datasets
- **RFUAV:** Hugging Face `kitofrank/RFUAV`, 100 MS/s float32.
  - Includes Autel EVO Nano, Herelink HX4, DJI FPV Combo, Avata 2 and Parrot.
  - Code Apache-2.0; data licence unstated, so local validation only.
- **CageDroneRF:** Autel X-Star and EVO II at 20 MS/s. Access by request form; CC BY-NC-SA.
- **No Autel/Skydio/digital FPV:** DroneRFa, DroneDetect and DRFF-R2 are DJI- or Parrot-only.
  Nothing anywhere for Walksnail, HDZero, OpenIPC or Skydio.

### What would unblock the rest
- **Recordings** (C5 40 MS/s continuous plus 20 MS/s snapshots; one SDR reference capture ≥ 30 MS/s):
  - HDZero on R4 and F4, both widths, armed and pit mode.
  - Walksnail CH3/CH4, 20/40 MHz, linked and unlinked.
  - DJI O3/O4 at 10/20/40 MHz, plus O4 at 5.1 GHz.
  - OpenIPC/wfb-ng with LDPC/STBC on and off.
  - Autel and Skydio: powered/unpaired, idle, armed, hovering near and far, controller only.
- **Hardware, rough used prices:**
  - HDZero VTX $40–70; it transmits without pairing, so it's the cheapest first test.
  - Walksnail 1S kit $70–100.
  - OpenIPC camera + BL-M8812EU2 $70–120, plus an RTL8812AU dongle $30–40.
  - DJI O3 Air Unit about $150 (needs goggles to link).
  - Autel EVO Nano $300–450.
  - Skydio 2+ $600–1000.
  - X10: borrow from a public-safety user.
- **FCC test reports** with occupied-bandwidth plots for EVO II V2/V3, Alpha, Nano and HDZero.
  The HDZero PDF returned 403 to automated fetches.
