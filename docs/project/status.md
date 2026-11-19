# Status

**Level: 🟢 Beginner** · Exactly what works today, how far each piece has been proven, and what is
still missing, as of 2026-11-18 (commit `6651ecd`).

This page is the full status of the project. The README carries a short summary that points
here. Every row says how far a piece has been proven, using the six phrases from
[Writing these docs](../dev/docs-style.md#say-what-is-proven-every-time), and never rounds up.

## In five lines

1. Rotorscope is a working **bench prototype**, not a product. Nobody can buy one yet.
2. The kit on the bench is an ESP32-P4 touch-screen hub, a C5 gateway and C5 listener nodes.
   It decodes all seven ways Remote ID is sent over Wi-Fi and Bluetooth, from the project's own
   test transmitter.
3. DJI DroneID decoding works on public recordings of real DJI drones.
4. A great deal more is written and tested on a computer only: control links, aircraft radios,
   attack and jamming alarms, spoofing checks, GPS-spoofing checks.
5. The next big steps are wiring the nodes to the hub, the phone page on the P4 kit, real GPS and
   ADSBee modules on the P4, LR2021 radio hardware, and above all a real drone.

> [!IMPORTANT]
> **What has been heard for real:** crewed aircraft. An ADSBee 1090 receiver, read from a laptop,
> showed real aircraft, and Rotorscope's own ADS-B decoder agreed with it on live traffic.
>
> **What has not:** any real drone. As of 2026-11-18 Rotorscope has **never heard a real drone**.
> Every drone signal it has decoded came from the project's own test transmitter (`rid-beacon`),
> the laptop's own Bluetooth test advert, or public recordings of real DJI drones.

### How to read the proof column

| Words | Meaning |
|---|---|
| **Heard for real** | Received from a real device someone else made (a real aircraft, a real drone). |
| **Proven on the air** | Real boards received it over the air from our own test transmitter. |
| **Proven on recordings** | Decodes public recordings of the real signal; not yet heard live. |
| **Simulated only** | Tested with made-up data on a computer or on the chip itself. |
| **Built, not yet tried** | The code exists and compiles; nobody has run it against the real thing. |
| **Planned** | Not built. |

Some pieces do not receive radio themselves: the screen, the logs, the safety checks, the tools.
For those, **Proven on the air** means the piece has worked on the real boards of the running bench
kit, with real nodes linked; **Simulated only** means it has only run against made-up input.

## Hub

The hub is the board that merges what the nodes hear and shows it. See
[Nodes, gateway and hub](../how/nodes-gateway-hub.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| P4 hub (Elecrow CrowPanel Advanced 5", ESP32-P4) | `firmware/hub-p4` | **Proven on the air** | The hub in use since 2026-11-14. Test drone shown end to end through gateway and nodes, on bench builds that print the track table. Runs on a patched esp-hal because the board's chip is an early revision (v1.3). |
| Touch screen (radar, list, signals, health, settings, FAA flags) | `crates/screen`, `firmware/hub-p4/src/app.rs` | **Proven on the air** | Panel and touch confirmed by eye on 2026-11-14 (46 taps). Newer screen lines (GPS, SD, aircraft, caveats strip, alarm badges) not yet checked by eye. |
| microSD CSV log | `crates/sdnative`, `firmware/hub-p4` | **Proven on the air** | Every row read back on the device in the 2026-11-15 end-to-end run, 0 errors. |
| Coverage: the hub tells each node what to listen to | `crates/coverage`, `crates/nodeimage` | **Proven on the air** | Wi-Fi 2.4, Wi-Fi 5 and Bluetooth roles assigned and switched, including switching between firmware images on one node. |
| USB JSON output | `firmware/hub-p4/src/report.rs` | **Proven on the air** | On the USB-C console at 115200 baud; a busy table drops whole track lines rather than block. |
| GPS (u-blox MAX-M10S or Quectel LC29H, found automatically, PPS time) | `crates/gnss`, `firmware/hub-p4/src/gps.rs` | **Simulated only** | Tested by the chip pretending to be each GPS on its own pins. **No real GPS module has been wired to the P4.** |
| ADSBee input on the P4 | `firmware/hub-p4/src/adsbee.rs` | **Simulated only** | Loop-back of CSBee text on the chip. **The real ADSBee is not wired to the P4.** |
| Phone page, hotspot and TAK CoT through a "web head" | `firmware/hub` (feature `webhead`), [WEBHEAD.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub-p4/WEBHEAD.md) | **Built, not yet tried** (the S3 side does not link today) | The P4 has no radio of its own, so an ESP32-S3 fed by one wire serves the phone page. The P4's sending side passed a loop-back test. **The S3 firmware does not currently link** (see Known gaps), and the web head has never run on hardware with the P4. |
| S3 hub (the first hub) | `firmware/hub` | **Simulated only** (does not link today) | Ran on a bare S3 with made-up demo drones before 2026-11-18; GPS, screen, SD, buttons, buzzer and battery never tried with real parts. **Does not currently link.** Hotspot hardening of 2026-11-18 (password from flash, request checks) has not run on a board. |
| Hang watchdog and reboot after a crash | `firmware/hub-p4/src/fault.rs` | **Proven on the air** | 2026-11-18: a forced crash rebooted in 0.5 s, a forced hang in 30 s, and the reason was logged. |
| New-drone buzzer, battery level on the P4 | — | **Planned** | The board has a speaker amplifier rather than a buzzer, and no known battery-voltage pin. |
| GPS integrity (jamming and spoofing of the hub's own GPS) | `crates/gnss` (integrity), [GNSS-INTEGRITY.md](../GNSS-INTEGRITY.md) | **Simulated only** | Host tests and on-chip GPS emulation, including a spoofer, time steps and drift (bench, 2026-11-18). Holds the last trusted position and time. Thresholds are first estimates; never run with a real receiver. |
| Site settings file (`SITE.TXT`): node positions, mutes, opt-ins | `crates/tracker/src/site.rs`, [SITE.md](../SITE.md) | **Simulated only** | Host-tested; read on the P4. |

## Nodes and link

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Wi-Fi listener, 2.4 GHz (S3, or C5 built `band24`) | `firmware/node-wifi` | **Proven on the air** | Beacon and NAN Remote ID decoded from the test transmitter on both chips. |
| Wi-Fi listener, 5 GHz (C5) | `firmware/node-wifi` | **Proven on the air** | Beacon and NAN on channel 149, through the P4 hub's tracker. |
| Bluetooth listener (BT4 legacy, BT5 extended, Long Range) | `firmware/node-ble` | **Proven on the air** | C5: all three. S3: BT4 from a laptop, and all three in the mixed-chip bench. A controller reset under Long Range traffic was found and fixed on 2026-11-14. |
| Raw-capture node (C5 as a radio sampler) | `firmware/node-sdr`, `crates/rawcap` | **Proven on the air** | Captures and reports to the hub over the air ([BRINGUP-RESULTS.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/node-sdr/BRINGUP-RESULTS.md)). What it detects is listed under DJI and FPV below. |
| Aero / control-link node (ESP32-C3 + Semtech LR2021) | `firmware/node-lr`, `crates/lr2021cfg` | **Built, not yet tried** | No LR2021 board has been in hand. Receive-only by construction. |
| ESP-NOW gateway (one C5 relays wireless nodes to the hub over a wire) | `firmware/gateway`, `crates/airlink` | **Proven on the air** | Two nodes at once, 0 corrupt frames, with the firmware before the 2026-11-18 cut-over. The keyless protocol 14 gateway is simulated only so far. More than two nodes, and a range test, not yet done. |
| Signed node link (on a sealed kit every ESP-NOW packet signed with that node's own key; forged or replayed ones dropped and reported by the hub) | `crates/airlink`, `crates/kitkeys`, `tools/link-key`, [ESPNOW.md](../ESPNOW.md) | **Simulated only** for the per-node keys (2026-11-18: tested on a computer, built for the boards); **proven on the air** before that with one key for the whole kit | The bench kit ran keyed builds with one kit key until the 2026-11-18 cut-over. No build carries a key now; kits ship open and are sealed with `link-key`. |
| "Who may send what" (origin rules) | `crates/link/src/origin.rs` | **Proven on the air** | 2026-11-18, before the cut-over: seven kinds of forbidden frame sent by a test node were all refused at the gateway, which did this check then; since the cut-over the hub does it (simulated only). The other checks on the bench list have not been run. |
| Wired bus (many nodes on one hub serial line) | `crates/wirebus`, [WIREBUS.md](../WIREBUS.md) | **Simulated only** | Byte-level simulation of up to 8 nodes. Nobody has wired it yet. The hub listens for it by default; the bench kit still uses ESP-NOW. |
| Hang watchdogs on the nodes and gateway | `firmware/node-common` (fault module) | **Proven on the air** | Crash and hang recovery checked on 2026-11-18 for the gateway, node-wifi (C5) and node-sdr (C5). **Not yet checked** on node-ble, node-lr or any S3 board. `rid-beacon` has no watchdog. |
| Node-to-hub message format | `crates/link` | **Proven on the air** | postcard + COBS over UART. |

## Remote ID

See [Remote ID](../detect/remote-id.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Remote ID decoder and encoder | `crates/rid` | **Proven on the air** | Matches the official opendroneid-core-c decoder field for field on random cases and survives garbage input. |
| Wi-Fi beacon and NAN, 2.4 GHz and 5 GHz | `firmware/node-wifi` | **Proven on the air** | All four through the P4 hub on 2026-11-15, each in 1.6-2.8 s. |
| Bluetooth 4, 5 extended, 5 Long Range | `firmware/node-ble` | **Proven on the air** | All three through the P4 hub on 2026-11-15. |
| China's GB 46750 format | `crates/rid` (`cn`) | **Simulated only** | Checked against opendroneid-core-c's Chinese variant. No test-transmitter mode for it. |
| Range | — | **Planned** | No range has been measured. All tests were about a metre apart. |
| Test transmitter | `firmware/rid-beacon` | **Proven on the air** | C5: all five transports heard by a second C5. S3: Wi-Fi beacon and BT4 heard by the laptop; 2.4 GHz beacon and NAN, BT4, BT5 extended and Long Range heard by a C5 (2026-11-12, `s3-to-c5` bench tests). The S3 has no 5 GHz radio. Fake identity, lowest power. |

## DJI

See [DJI DroneID](../detect/dji-droneid.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| DroneID decoder (OcuSync 2/3 plaintext bursts) | `crates/droneid`, `tools/droneid-file` | **Proven on recordings** | Decodes all 13 bursts of the two public RUB-SysSec recordings, also on the C5 itself fed through its own capture path (`tools/droneid-hil`, median 151 ms a burst). |
| DroneID job on the raw-capture node | `firmware/node-sdr` | **Proven on recordings** | Runs on the air on hub assignment, but has never had a real DJI drone to hear. Recordings fed to the chip never reach the hub, by design. |
| O4 records (encrypted; detect-only without a key) | `crates/droneid`, `tools/rfuav-check`, [DJI-LINKS.md](../DJI-LINKS.md#validation-against-rfuav-recordings) | **Proven on recordings** | Matches the published O4 test vectors. On 2026-11-16, CRC-valid O4 records were decoded on a computer from public RFUAV recordings of a DJI Mini 4 Pro and an Avata 2, also through a model of the C5's capture path. Not yet fed through the chip itself. (DRONEID-O4.md's status line predates this.) |
| O4 session tracking and channel follower | `crates/droneid`, `crates/rawcap`, [DRONEID-O4.md](../DRONEID-O4.md) | **Simulated only** | Host tests and forged-packet scenarios; needs a real O4 drone. |
| O4 decryption with an owner-supplied key | `crates/djicrypt` | **Simulated only** | Tested with a key made up in the tests. No public key exists, so it has never decrypted a real record. Without a key, O4 is detect-only. |
| DJI radio-link fingerprints (video, controller, FPV air units) | `crates/djilink`, [DJI-LINKS.md](../DJI-LINKS.md) | **Proven on recordings** | Tuned against four RFUAV recordings on a computer. Not yet run on the chip against a real DJI signal. O4 is told from Autel by its signal shape; older DJI links (OcuSync 2/3, O3) cannot always be told from Autel, nor DJI FPV from Walksnail. |
| DJI legacy Wi-Fi DroneID beacons | `crates/rid` (`dji`), `firmware/node-wifi` | **Simulated only** | The node forwards them; never seen from a real Wi-Fi DJI. |
| Drone makers' own Wi-Fi networks (Tello, Parrot and others) | `crates/wifidrone` | **Simulated only** | Never seen from a real drone. |

## FPV and other links

See [FPV video links](../detect/fpv.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Open digital FPV links (wfb-ng/OpenIPC, OpenHD, RubyFPV, DroneBridge) | `crates/openlink`, `firmware/node-wifi` | **Proven on the air** | The test transmitter's fake frames, all four systems on channels 6, 149, 161 and 165, 16 of 16 through the hub. wfb-ng's default coded video (LDPC/STBC) is untested. Identifiers hidden unless opted in. |
| Analog video, 5.8 GHz | `crates/fpvscan`, `firmware/node-sdr` | **Simulated only** | Synthetic signals only. A 200-second run on the air showed no false video among ordinary Wi-Fi. No video transmitter has been tried (one needs an amateur radio licence). |
| Analog video, 2.4 GHz | `crates/fpvscan` | **Simulated only** | Synthetic signals only, including Wi-Fi, Bluetooth and microwave ovens. |
| Walksnail, HDZero, Skydio, Autel as named systems | — | **Planned** | Recognised at most as a family of signal; no recordings exist to tell them apart. The raw-capture scan does have an HDZero verdict (`crates/fpvscan`: a wide, flat, continuous signal on HDZero's channel grid), **simulated only**. |
| Channels above 5810 MHz (RX5808 receiver) | — | **Planned** | Outside the C5's measured capture range. |

## Control links

See [Control links](../CONTROL-LINKS.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Control-link recognisers (ExpressLRS, mLRS, SiK/RFD900, OpenLRSng) and energy map | `crates/ctrlwatch` | **Simulated only** | Thresholds tuned on synthetic data. Crossfire, Tracer, Ghost, FrSky and FlySky are energy candidates only. |
| Control-link scanning on the LR2021 node, table on the hub | `firmware/node-lr`, `firmware/hub-p4` | **Built, not yet tried** | Waiting for an LR2021 board. Identifiers and content are separate opt-ins in `SITE.TXT`. |
| Hostile MAVLink on a drone's telemetry | `crates/mavwatch` | **Simulated only** | On Wi-Fi nodes and the LR2021 node, only when content reading is switched on. |

## Aircraft

See [Crewed aircraft](../use/aircraft.md) and [Crewed-aircraft signals](../detect/aircraft-signals.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| ADS-B through an ADSBee 1090 | `crates/adsb`, `tools/rid-view --adsbee` | **Heard for real** | From the laptop on 2026-11-14: 77 CSBee lines, 0 bad, 6 aircraft. Aircraft are shown in their own table and never raise the drone alarm. |
| Our own Mode S / ADS-B decoder | `crates/modes`, `tools/modes-live` | **Heard for real** | On the laptop, 15 minutes of live traffic: agreed with the ADSBee on 98-100 % of checks. On the P4 only replayed recordings (feature `modes`, off by default). |
| UAT (978 MHz), OGN and FLARM decoders | `crates/uat`, `crates/flarm`, `crates/aerohop`, [AIRCRAFT-SIGNALS.md](../AIRCRAFT-SIGNALS.md) | **Simulated only** | No real radio frame has been through them. FLARM content needs a key file on the SD card. |
| Aero node on the hub (decoding its frames, time sync) | `firmware/hub-p4`, [AERO-NODE.md](../AERO-NODE.md) | **Built, not yet tried** | No LR2021 board. |
| Drone near a crewed aircraft warning | `crates/tracker/src/conflict.rs`, [AIRCRAFT-CONFLICT.md](../AIRCRAFT-CONFLICT.md) | **Simulated only** | Host tests. |

## Threat detection

See [Attacks and interference](../detect/threats.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Spoofed-drone checks and identity continuity | `crates/tracker` (spoof, continuity), [SPOOFING.md](../SPOOFING.md) | **Simulated only** | Host tests on synthetic flights. |
| New drones on probation; fake-drone flood alarm; ID disputes | `crates/tracker` (probation, claims) | **Simulated only** | Floods of made-up drones injected by a test node on the bench (2026-11-18): the alarms rose and ended as designed and the test drone stayed confirmed. |
| Wi-Fi and Bluetooth attacks (deauth floods, evil twins, Karma, attack tools, pop-up spam) | `crates/wifiwatch`, [WIFI-ATTACKS.md](../WIFI-ATTACKS.md) | **Simulated only** | Host tests; node flood handling run on the bench with injected frames. Never seen from a real attack tool. |
| Attacks on drones (Wi-Fi drone hijack, contested control link, hostile MAVLink) | `crates/wifiwatch`, `crates/ctrlwatch`, `crates/mavwatch`, [DRONE-ATTACKS.md](../DRONE-ATTACKS.md) | **Simulated only** | Host tests. |
| Jamming on 2.4 and 5.8 GHz, and "node blinded" | `crates/jamwatch`, `crates/tracker` (threats), [JAMMING.md](../JAMMING.md) | **Simulated only** | Host tests on synthetic scenes. The "node blinded" check was re-tested on the bench with a frame flood (2026-11-18). |
| GPS interference in the area (from other receivers' reports) | `crates/tracker/src/area_gnss.rs` | **Simulated only** | Host tests. |

## FAA flags

See [FAA rule flags](../use/faa-flags.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Possible-violation flags (height, controlled airspace, restricted areas, line of sight, night) | `crates/faarules`, `crates/airspace` | **Simulated only** | Runs on the P4 against made-up drone positions and real public airspace data (about 3 ms per drone). Never says "illegal". |
| Offline airspace pack | `tools/airspace-pack` | **Proven on the air** | Builds from public FAA, USGS, NGS and Census data; a 100 km Philadelphia pack is 662 KB and loads on the P4. TFRs must be saved by hand (the FAA site refuses the tool). |

## Evidence log

See [The evidence log](../EVIDENCE.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Hash-chained, signed evidence log on the SD card | `crates/evidence`, `firmware/hub-p4/src/evlog.rs` | **Proven on the air** | Works on the bench, signed with a development key (2026-11-15) or, on the 2026-11-18 run, the **published test key**: not evidence-grade either way. That 2026-11-18 log checked out record for record once the verifier was told to accept the test key. |
| Verifier | `tools/evidence-verify` | **Proven on the air** | Says VERIFIED only against a key supplied from outside the log; refuses the published test key unless told it is a test. |
| Upload server and uploader | `tools/evidence-server` | **Proven on the air** | On the bench (2026-11-15) records reached it from the hub through a laptop. Shared-token access, no real accounts; plain HTTP is for the bench only. |
| Hardware-held key, secure boot, flash encryption, automatic upload | — | **Planned** | See the [roadmap](roadmap.md). |

## Laptop tools

See [The laptop viewer](../use/rid-view.md) and the [Tools reference](../dev/tools.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Laptop viewer: tables, demo mode, TAK CoT, phone-style web page | `tools/rid-view` | **Proven on the air** | Reads node USB output, files or standard input; `--demo` needs no hardware. The table and the web page have shown drones heard on the air. The TAK CoT output has been sent, but nothing records it being checked in a TAK app. |
| Laptop Bluetooth sensor (any BlueZ adapter) | `tools/rid-view --ble` | **Proven on the air** | Hears BT4 legacy adverts only, not BT5. |
| Sonoff dongle as a Bluetooth 5 sensor (Sniffle firmware) | `tools/cc26xx-flash`, `tools/rid-view --sniffle`, [SONOFF.md](../SONOFF.md) | **Proven on the air** | Legacy Remote ID from the laptop's test transmitter (2026-11-11); extended and Long Range from a C5 test transmitter (2026-11-12, recorded in commit `8b30df8`; SONOFF.md's test section predates it). Restoring the Zigbee backup tested only in simulation. |
| WarDragon / DragonSync output | `tools/rid-view --dragonsync` | **Simulated only** | Tested with a real ZeroMQ subscriber and DragonSync's own parsing code, not against a running DragonSync or a WarDragon. |
| Laptop Bluetooth test transmitter | `tools/rid-ble-beacon` | **Proven on the air** | BT4 only, same fake identity as `rid-beacon`. Bench use only. |

## Test equipment

See [Bench tests](../BENCH.md).

| Piece | Where | Proof | Notes |
|---|---|---|---|
| Two-board bench tests (PASS / FAIL per signal) | `tools/bench/run.py` | **Proven on the air** | All C5 and mixed S3/C5 tests passed on 2026-11-12. |
| Board flashing with per-board locks | `tools/dev-board.sh`, `tools/flash-node.sh` | **Proven on the air** | Refuse keyless and bench images unless told otherwise (checked 2026-11-18). |
| DroneID recordings fed through the C5 | `tools/droneid-hil` | **Proven on recordings** | 13 of 13 bursts at two signal levels. |
| Bench-only builds that fake input (demo drones, floods, crashes, GPS emulation) | `ROTORSCOPE_BENCH=1` builds | **Proven on the air** | Refused without `ROTORSCOPE_BENCH=1` (checked 2026-11-18). A bench build names itself in the evidence log (seen 2026-11-18) and on screen (not yet checked by eye). |

## Known gaps

- **The S3 firmware (`firmware/hub`) does not link.** On 2026-11-18 its static RAM was 18,088 bytes
  over budget ([hub README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub/README.md),
  "Security checks still to run on hardware", item 14). That firmware is also the P4's web head, so
  **the P4 kit has no phone page, hotspot or TAK output today.** TAK output is available from the
  laptop through `rid-view`.
- **No real drone has been heard.** No Remote ID drone or module, no DJI drone, no FPV video
  transmitter and no control-link transmitter has been in front of Rotorscope.
- **No LR2021 hardware.** The aero node and control-link node exist only as code.
- **No real GPS and no ADSBee on the P4.** Both were tested only by the chip emulating them.
- **No range figures.** Every test was at about a metre.
- **The wired bus has not been wired.** The bench kit uses ESP-NOW through the gateway.
- **The evidence log is not evidence-grade yet.** The hub signs with a published test key unless
  a development key is built in, and it has no secure boot: at best the log shows records have
  not changed since they were signed, not which unit signed them.
- **No kit hardware**: no carrier board, case, battery or build guide yet.
- **Some older documents are behind the code.** The detailed write-ups were written alongside the
  work and several still describe an earlier state; the ones found during the 2026-11-18
  documentation pass now carry dated update notes (for example the "Hub chip" section of
  [PLAN.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/PLAN.md), [SONOFF.md](../SONOFF.md) and
  [DRONEID-O4.md](../DRONEID-O4.md)). Where an older document and this page disagree, this page
  is the newer.

Next: [Roadmap](roadmap.md)
