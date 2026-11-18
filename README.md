# Rotorscope

An affordable, receive-only detector for drones that broadcast Remote ID, built from several
ESP32 chips with firmware written in Rust. See [PLAN.md](PLAN.md) for requirements and phases,
and [RESEARCH.md](RESEARCH.md) for the survey behind them.

## Status (2026-11-15)

**How it fits together today:**

```
C5 listener nodes --ESP-NOW--> C5 gateway --wire--> P4 hub (5" touch screen, SD, GPS, ADSBee)
                                                      |  one wire
                                                      v
                                            web head (ESP32-S3): hotspot, phone page, TAK CoT
```

The hub now runs on an ESP32-P4 (Elecrow CrowPanel Advanced 5"). The earlier ESP32-S3 hub still
builds and runs, and the same S3 firmware is reused as the P4's web head. Wiring the nodes to the
hub over one shared serial line, instead of ESP-NOW, is the next big step (PLAN.md Phase 5a).

| Piece | Where | State |
|---|---|---|
| **P4 hub** | `firmware/hub-p4` | Runs on the bench with two C5 nodes through the gateway, 0 corrupt frames: touch screen (radar, list, signals, health, settings, FAA flags), SD log, coverage planning, USB JSON, GPS (u-blox MAX-M10S or Quectel LC29H, found automatically, with PPS time) and ADSBee input. GPS and ADSBee are tested only by the chip feeding its own pins, not yet with the real modules. Feature-by-feature comparison with the S3 hub: [PARITY.md](firmware/hub-p4/PARITY.md). |
| ESP-NOW gateway and coverage | `firmware/gateway`, `crates/coverage`, `crates/link` | Working on the bench: one C5 relays every wireless node to the P4 over a wire, and the hub tells each node which job to do (Wi-Fi 2.4/5 GHz, Bluetooth, DroneID, FPV), switching between firmware images stored on each node. |
| Phone page (web head) | `firmware/hub` (feature `webhead`), [WEBHEAD.md](firmware/hub-p4/WEBHEAD.md) | Built and tested on the laptop: an S3 fed by one wire from the P4 serves the hotspot, phone page and TAK CoT. Not yet wired to the P4 on the bench. |
| DJI DroneID and analog FPV node | `firmware/node-sdr`, `crates/droneid`, `crates/fpvscan` | Bring-up on two C5 boards: both jobs run on air on hub assignment ([BRINGUP-RESULTS.md](firmware/node-sdr/BRINGUP-RESULTS.md)). Not yet heard from a real DJI drone or video transmitter. |
| FAA rule flags | `crates/airspace`, `crates/faarules`, `tools/airspace-pack` | Built and running on the P4 (Phase 12): flags *possible* violations (height above ground, controlled airspace, TFRs, out of the pilot's sight, night) at four levels, from an offline map pack on the SD card (a 100 km Philadelphia pack is 662 KB). Never says "illegal". |
| Attack and jamming detection | `crates/wifiwatch`, `crates/jamwatch`, `crates/tracker` (threats) | Built 2026-11-17, simulated attacks only: Wi-Fi deauth/disassoc floods, fake-network floods, evil twins, Karma, forced channel switches, login/probe floods, known attack tools, Bluetooth pop-up spam ([WIFI-ATTACKS.md](docs/WIFI-ATTACKS.md)); wideband, tone, sweep and pulsed jamming on 2.4/5.8 GHz and deaf nodes ([JAMMING.md](docs/JAMMING.md)). The hub shows JAMMED / ATTACK and logs each as evidence; false alarms can be muted ([SITE.md](docs/SITE.md)). |
| Spoofed drones | `crates/tracker` (spoof, continuity) | Built 2026-11-17: 23 consistency checks on Remote ID and DroneID with confidence scores, and linking a drone across ID or address changes ([SPOOFING.md](docs/SPOOFING.md)). Node positions sharpen the signal-strength check ([SITE.md](docs/SITE.md)). |
| Signed node link | `crates/airlink`, `tools/link-key` | Running on the bench: every ESP-NOW packet signed with the kit's own key, forged or replayed ones dropped and reported ([ESPNOW.md](docs/ESPNOW.md) "Securing the link"). |
| Hub GPS integrity | `crates/gnss` (integrity) | Built 2026-11-17, not yet run with a real receiver: jamming and spoofing of the hub's own GPS; holds the last trusted position and time; "Hub moved" reset ([GNSS-INTEGRITY.md](docs/GNSS-INTEGRITY.md)). |
| Evidence-grade log | `crates/evidence`, `tools/evidence-verify`, `tools/evidence-server` | Works on the bench with a development key (Phase 13): every detection hash-chained and signed each minute, a verifier that names the exact spot of any tampering, and an upload server with an audit log. Hardware key, secure boot and automatic upload still to do. Plain-language guide: [docs/EVIDENCE.md](docs/EVIDENCE.md). |
| Crewed aircraft (ADS-B via ADSBee 1090) | `crates/adsb`, `crates/tracker`, both hubs, `tools/rid-view --adsbee` | Run against a real ADSBee from the laptop; a separate aircraft table on every output, never an alert. See `docs/ADSBEE.md`. |
| Our own ADS-B / Mode S decoder | `crates/modes`, `tools/modes-live` | Built (Phase 14a): agreed with the ADSBee's own decode on 98-100% of checks over 15 minutes of live traffic. Off by default on the P4 until the ADSBee is wired to it. UAT and FLARM-family are researched in [docs/AIRCRAFT-SIGNALS.md](docs/AIRCRAFT-SIGNALS.md). |
| Remote ID decoder and encoder | `crates/rid` | Done. Matches the official opendroneid-core-c field for field across hundreds of thousands of random cases, and survives millions of garbage frames. Also decodes DJI legacy Wi-Fi DroneID and GB 46750 (China). |
| Node-to-hub wire format | `crates/link` | Done (postcard + COBS over UART). |
| Hub track logic | `crates/tracker` | Done: merges by UAS ID across radios (2.4 and 5 GHz Wi-Fi kept apart), Vincenty range/bearing, JSON / TAK CoT / CSV output. Runs on the hub. |
| Wi-Fi listener (2.4 GHz S3, 5 GHz C5) | `firmware/node-wifi` | S3 verified on hardware: sniffs and hops channels for 60 s with no drops. C5 verified on air (2026-11-12): decodes 5 GHz beacon and NAN Remote ID from a C5 `rid-beacon` on channel 149. |
| Bluetooth listener (legacy + Long Range) | `firmware/node-ble` | S3 verified: decoded a BT4 Remote ID broadcast from a laptop. C5 verified on air (2026-11-12): BT4 legacy, BT5 extended and Long Range all decoded from a C5 `rid-beacon` (bench `--group c5`). |
| Test transmitter | `firmware/rid-beacon` | S3 verified: the laptop received the Wi-Fi beacon and BT4 adverts; BT4, BT5 extended and Long Range heard by a C5 (2026-11-12). S3 NAN still to be heard. C5 verified on air (2026-11-12): all five transmitters (5 GHz beacon and NAN, BT4, BT5 extended, Long Range) heard by a second C5; BT4 also decoded by the laptop. Needs the esp-radio clock fix in `firmware/patches`. |
| Open FPV link detection | `crates/openlink`, `firmware/node-wifi`, `crates/tracker` | Built and unit-tested (2026-11-09): recognises wfb-ng/OpenIPC, OpenHD, RubyFPV and DroneBridge frames by their headers, lists them apart from drones, identifiers hidden unless opted in. **Heard on air 2026-11-14** from the test beacon's fake frames: all four systems on channels 6, 149, 161 and 165 (16/16 bench tests PASS, C5 to C5). Not yet heard from real FPV hardware, and wfb-ng's default LDPC/STBC coding is untested. |
| Laptop viewer | `tools/rid-view` | Done. Reads node USB output, files or stdin; can send TAK CoT; has `--demo`. |
| WarDragon / DragonSync sensor output | `tools/rid-view --dragonsync` | Built; tested on the laptop with a real ZeroMQ subscriber and DragonSync's own parser functions, not yet against a running DragonSync or a WarDragon. Publishes drones and signal alerts in the JSON that DragonSync subscribes to. See `tools/rid-view/README.md`. |
| Laptop Bluetooth sensor (BT4 only) | `tools/rid-view --ble hci1`, `tools/rid-ble-beacon` | Built and verified laptop to laptop (2026-11-11). Any BlueZ adapter, such as the internal Intel or the Sena UD100, listens for Bluetooth Remote ID with no root (`rid-view --ble hci1`). It hears legacy adverts only, not BT5 extended or Long Range. `rid-ble-beacon hci0` is a bench-only BT4 test transmitter with `rid-beacon`'s fake identity. See `tools/rid-view/README.md`. |
| Bluetooth 5 sensor on a Sonoff dongle | `tools/cc26xx-flash`, `tools/rid-view --sniffle` | Flashed and verified on hardware (2026-11-11). A Sonoff Zigbee 3.0 USB Dongle Plus (CC2652P) is backed up and flashed with NCC Group's Sniffle by our own CC26x2 bootloader tool (CCFG lock-out check, CRC-verified read and write), then `rid-view --sniffle` reads legacy, extended and Long Range Remote ID with RSSI, passively. Decoded the laptop's BT4 test beacon at about 7 adverts/s; on 2026-11-12 also BT5 extended (`--sniffle-mode 1m`, -55 dBm) and Long Range (`coded`, -56 dBm) from a C5 `rid-beacon`. Zigbee restore tested only in simulation. See `docs/SONOFF.md`. |
| DJI OcuSync DroneID decoder | `crates/droneid`, `tools/droneid-file` | Host side done (Phase 10a): `no_std`, fixed point, builds for the C5. Decodes every burst in both public recordings (fetch them with `tools/fetch-droneid-samples.sh`). Recognises O4's encrypted records but never decrypts; that part is untested on real signals. |
| S3 hub (first hub) | `firmware/hub` | S3 runs on the bench: node links, hotspot + phone page, CoT, USB, plus GPS (I2C, PPS), banded 320x240 screen, buttons, buzzer, SD CSV log and battery on the second core. With nothing attached each reports absent and the rest runs; untested against a real GPS, panel or card. |

**Verified end to end:** every signal we may legally send, from the test transmitter on the air →
C5 listener node → C5 gateway → wire → the P4 hub's own track table ([docs/BENCH.md](docs/BENCH.md)).

**Next:** wired nodes on one shared serial line (Phase 5a), the web head and the real GPS and
ADSBee on the P4, and the LR2021 control-link node (Phase 9). Everything planned is in
[PLAN.md](PLAN.md).

## Building

Host crates and tools:

```bash
cargo test
```

```bash
cargo run --release -p rid-view -- --demo
```

Firmware: each `firmware/*` folder is its own project with a README. The S3 is an Xtensa
chip, so it needs Espressif's toolchain (`espup install`, then `. ~/export-esp.sh`). The C5
builds on stable Rust.

Flash and watch a board with `tools/dev-board.sh <elf> [seconds] [board]`. It holds a per-board
lock so that parallel agents take turns. Boards are named in the gitignored `tools/boards.local`
(`tools/dev-board.sh --register` fills it in). Two-board bench tests, one command each:
`tools/bench/run.py --list`; see [docs/BENCH.md](docs/BENCH.md).

## Conduct

This project only listens. `rid-beacon` exists purely to test the receivers on the bench. It
uses an obviously fake identity and the lowest transmit power, and must not be used to
impersonate an aircraft.

The hub also has a microphone, for drones that transmit nothing (PLAN.md Phase 15,
docs/ACOUSTIC.md, on branch `acoustic`). It keeps only numbers (pitch, loudness) unless it
believes it hears a drone; then it saves a short sound clip to its own SD card. A microphone
outdoors also hears people. `audio clips off` in the site file stops all storing of sound.
Recording conversations needs consent in many places (everyone's, in Pennsylvania): post a
notice where the device listens, or turn clips off.

## Acknowledgements

The C5 raw-radio work builds on findings from C5VRX by Twotoz and the C5VRX contributors
(https://github.com/Twotoz/C5VRX, https://twotoz.github.io/C5VRX/), ESPARGOS/esp-sdr,
cyber-murmel/esp-sdr-zephyr and CapnBry/widscan58. The DJI DroneID work builds on RUB-SysSec's
DroneSecurity research, proto17/dji_droneid and anarkiwi/samples2djidroneid, and on the O4 record
and PHY description in EdwardBlair/dji-droneid-spec. `crates/droneid` uses these for protocol
facts only (layouts, constants, the published scrambler example); it contains no code from any
of them. The two test recordings come from RUB-SysSec/DroneSecurity (AGPL-3.0) and are
downloaded on demand, never committed.

## License

GPL-3.0-or-later (see `LICENSE`). Anyone may use, modify and sell Rotorscope, including as
kits, but whoever ships a changed version must publish their changes under the same license.

`third_party/opendroneid-core-c` keeps its own Apache-2.0 license and is used only by host-side
tests.
