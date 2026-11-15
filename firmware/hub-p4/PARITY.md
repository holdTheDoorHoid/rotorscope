# P4 hub vs S3 hub: what the P4 does today

The older hub runs on an ESP32-S3 (`firmware/hub`). The new one runs on the
Elecrow CrowPanel Advanced 5" with an ESP32-P4 (`firmware/hub-p4`). This page
lists every S3 hub feature and where the P4 stands. Status words:

- **done**: worked on the P4 before this task.
- **done this task**: added on branch `p4-parity` (2026-11-15).
- **blocked**: cannot be done yet; the reason is given.
- **not ported**: could be done, was not, and why.

"Tested" says how. **No GPS module and no ADSBee are wired to the P4**: those
parts were tested with host tests and an on-chip loop-back (the chip pretends
to be the GPS / ADSBee on its own pins), never with the real modules.

## Feature table

| Feature (S3 hub) | P4 status | Notes and how it was tested |
|---|---|---|
| Drone tracks from radio nodes | done | Through the ESP-NOW gateway (C5) on UART3. The S3 reads up to three wired nodes; the P4 reads one gateway that relays many nodes (and still accepts a wired node: `direct` frames). The P4 also handles protocol 4 (DroneID, RF, Wi-Fi drones), which the S3 does not |
| Open FPV links table | done | Shared tracker. Identifiers hidden by default; `--features show-link-ids` now exists on the P4 too (**done this task**) |
| Telling nodes what to listen to (coverage) | done | P4 only (BRINGUP step 13) |
| **GPS position (own position)** | **done this task** | u-blox MAX-M10S *or* Quectel LC29H(AA) on the UART1 header, found automatically. Loop-back tested with both (below). **Untested with a real module** |
| **GPS time (UTC)** | **done this task** | From RMC (LC29H) or NAV-PVT (M10). Loop-back: `utc_src` `gps_pps` |
| **GPS PPS (sub-millisecond time)** | **done this task** | GPIO29 edge interrupt; UTC anchored to the edge as on the S3. Loop-back: edges counted, anchor used |
| **Distance and bearing to drones** | **done this task** | Own position now goes into the tracker once a second (GPS first, manual second, same rule as the S3). Loop-back: the test aircraft showed `dist_m 1401.1, brg 37.6` in the console JSON. Screen not checked by eye |
| Manual own position | **done this task** | Console `p` (test point) / `c` (clear), as the S3's USB commands. The S3's phone-page form needs the web page (blocked, below) |
| Old tracks expire | **done this task** | The P4 never called `tracker.expire()`; it now does at 1 Hz like the S3 |
| **SD card CSV log** | done | Bit-banged SD, `ROTORSCP\` folder (BRINGUP step 14) |
| SD: GPS time in rows, dated file names, FAT dates | **done this task** | `utc` column filled from GPS; file `MMDDhhmm.CSV` when time is known (waits up to 90 s after boot for it, rows kept in RAM); FAT timestamps from GPS. Loop-back M10 run: file `11160000.CSV`, rows 36, 0 dropped |
| SD: aircraft rows | **done this task** | `csv::aircraft_row` for active aircraft |
| **USB JSON output** | **done this task** | On the USB-C (CH340) console, 115200 baud, shared with the log: JSON lines start with `{`. Same `track`, `link`, `aircraft`, `new` lines as the S3 (same formatter code); a P4-specific `hub` line. Never blocks the loop: a 16 KB ring, whole lines dropped when full (`console_drops`). The P4's own USB-Serial-JTAG pins carry the microphone, so there is no second USB serial port. At 115200 a busy table (dozens of tracks) will drop track lines; the `hub` line goes first |
| USB `s` (whole state JSON in one line) | not ported | The phone page's `/api/state` body; it belongs with the web page. Per-track lines carry the same data |
| **ADS-B from an ADSBee** | **done this task** | UART4 RX on GPIO30, always built (the P4 has a spare UART; the S3 had to give up a node link). Aircraft table, screen line, ADSBee shown as a node, SD rows, JSON lines. Loop-back tested (UART4 sends CSBee onto its own pin): 0 errors. **Not tested with the real ADSBee** (not wired to the P4) |
| ADS-B decoded in-house (Phase 14a) | **done (2026-11-15), off by default** | `--features modes`: the ADSBee sends Beast (`AT+PROTOCOL_OUT=COMMS_UART,BEAST`) and `crates/modes` decodes it; aircraft go to the same table, screen, SD, JSON and web-head `Csbee` frames (no link change, no decoder on the S3). `hub` line gains `adsb.modes`. Loop-back tested with recorded real frames; **not tested with the real ADSBee on this hub** |
| Screen pages (Radar, List, Signals, Health, Settings) | done | Shared `screen` crate at 2x with touch keys (BRINGUP 9/10) |
| Screen: GPS state, SD state, aircraft line, ADSBee node, power | **done this task** | Were blank or "no GPS" before. Not yet checked by eye |
| New-drone alert banner | done | Shared UI |
| New-drone buzzer | not ported | The board has no buzzer, only an I2S amplifier and speaker (GPIO21-23). Needs an I2S driver on the pre-v3 chip; a separate task |
| Battery level | not ported | The BAT connector's voltage is not wired to a GPIO we know of; the screen says USB power (as a bench S3 does) |
| **Phone web page over the hub's hotspot** | **built (web head), wire untested** | Route 3 below, branch `webhead` (2026-11-15): a second board (the old S3 hub, `--features webhead`) serves it, fed by the P4 over one wire (P4 GPIO31 -> S3 GPIO4 + GND). See WEBHEAD.md. P4 side loop-back tested; the S3 and the wire still need the owner |
| TAK CoT multicast | built (web head) | From the web head, stamped with the P4's GPS time (`HubFix`) |
| Station mode (join a Wi-Fi network) | built (web head) | The S3 hub's own station mode, unchanged |
| Captive portal, DHCP, DNS | built (web head) | Part of the web head's hotspot (channel 11) |
| Watchdog (reboot if the loop hangs) | not ported | The S3 uses the RTC watchdog; it has not been checked on this pre-v3 chip and esp-hal patch. A wrong setting means reboot loops, so it needs its own bench test |
| Reboot after a panic (`custom-halt`) | not ported | The P4 halts and prints the panic; same reason as the watchdog |
| Demo mode (made-up drones) | done (different) | `--features fakedrone` / `fakenode`; the S3's two-circle demo is not ported |

## Pins chosen (all on connectors the board already has)

| What | P4 pin | Where on the board |
|---|---|---|
| GPS TX -> hub RX | GPIO48 | UART1 header (PH2.0-4P) "RX". DIP switch 1 must select UART1, not the wireless module |
| hub TX -> GPS RX | GPIO47 | UART1 header "TX" |
| GPS power | 3V3, GND | UART1 header (3.3 V out, 1 A max) |
| GPS PPS | GPIO29 | Wireless-module socket (the SX1262 BUSY / nRF24 IRQ pin). Free while no radio module is fitted |
| ADSBee TX -> hub | GPIO30 | Wireless-module socket (the SX1262 NSS pin). Plus GND |
| Gateway link | GPIO27 / 28 | UART3-IN header, unchanged |
| Web head feed (P4 TX -> web head RX) | GPIO31 | Wireless-module socket (SX1262 IRQ / nRF24 CE). Plus GND. WEBHEAD.md |

UARTs: UART0 console, UART1 GPS, UART2 web-head feed (TX GPIO31; the bench
self-tests `selftest`/`gpsloop` take it instead), UART3 gateway, UART4 ADSBee. The socket pin numbers are from Elecrow's wiki;
**check with a meter which socket hole is GPIO29 and GPIO30 before wiring**,
and do not fit a radio module in the socket while they are used.

## GPS: the two modules

Both are found automatically at boot: the hub listens at 115200, then 9600,
then 38400 baud; when it hears valid sentences it sends two read-only
questions (u-blox "MON-VER" and Quectel "PQTMVERNO"). Whichever module
answers is configured for this boot only (nothing is saved to the module's
own flash). A receiver that answers neither is read as plain NMEA.

| | u-blox MAX-M10S | Quectel LC29H(AA) |
|---|---|---|
| Bands | L1 | L1 + L5 (dual band) |
| Speed as shipped | 9600 | 115200 |
| What the hub sets | binary NAV-PVT once a second, text off, portable model, PPS 100 ms after lock, 115200 baud | GGA, RMC, GSA, GSV on; GLL, VTG off; PPS 100 ms, active high, every second, **only with a 3D fix** |
| Satellites per constellation / band | not shown (binary output) | shown: in view, used, L1 and L5 counts per system (`in_view`, `used`, `dual_band` in the `hub` line) |

LC29H facts and their sources are in `crates/gnss/README.md` ("Quectel
LC29H(AA)"). Things the manual did not settle, so the hub does not rely on
them: the factory PPS mode (the hub sets it), whether PPS is aligned to UTC
or GPS seconds (the hub rounds to the UTC second named by RMC, as with the
M10), and the serial frame format (8N1 assumed, as every NMEA module).

## What was tested, and how

| Test | Result |
|---|---|
| `cargo test -p gnss` (host) | 59 tests pass: the 43 earlier ones plus 16 new LC29H ones (dual-band NMEA 4.10 epoch from the protocol manual's examples, sky view per constellation and band, QZSS under GP, 2D from GSA, every talker, PAIR/PQTM replies and errors, the command builder against the manual's own sentences, the boot sequence and its confirmations, module detection, random chunking) |
| `--features gpsloop` on the P4 (LC29H emulated on the chip's own pins) | Found at 115200, detected as `lc29h`, 7 configuration steps ACKed, 3D fix, 14 in view / 14 used, dual band, PPS 55 edges in 59 s, `utc_src` `gps_pps`, own position in the tracker, 0 checksum errors, 0 UART errors. ADS-B: 116 lines, 0 errors, aircraft with distance and bearing. Gateway link at the same time: 2 nodes, 0 corrupt |
| `--features gpsloop-m10` on the P4 (M10 as shipped, 9600) | 115200 heard nothing valid, 9600 did; detected as `max-m10s`, configured, switched to 115200, NAV-PVT fixes, PPS anchor, SD file named `11160000.CSV` from GPS time. 2 UART errors (the 9600 bytes heard at 115200 during the hunt, expected) |
| `--features modesloop` on the P4 (2026-11-15: UART4 replays 300 s / 1996 recorded Beast frames, 9 real aircraft, onto GPIO30) | After one pass: 1993 frames, 1977 used, 0 bad CRC, 8 unconfirmed, 199 global + 17 local positions, 0 refused, `uart_err` 0, `ring_full` 0; the host's `modes-live check-export` on the same file: 1996 / 1980 / 8 / 199 + 18 (the last frames of the pass were still in flight), and the same final positions to 5 decimals. Gateway at the same time: 2 nodes, 0 corrupt; web head 0 dropped |
| Normal build on the P4 | See BRINGUP.md step 17 |

## Phone page: the routes

The S3 hub runs its own Wi-Fi hotspot and serves the phone page. The P4 has
no radio; Wi-Fi on this board is an ESP32-C6 on SDIO (CMD 54, CLK 53, D0-D3
52-49). Four routes were considered. **Nothing here touched the C6.**

**1. The onboard C6 via esp-hosted (the right end state).**
- What the C6 runs is not known for sure. Elecrow's wiki says only that its
  firmware is factory-programmed and "cannot be directly reprogrammed via
  P4"; it does not name it. Elecrow's own factory app on the P4 (our backup)
  contains no esp-hosted or Wi-Fi code at all, so it gives no hint either.
  If it is Espressif's esp-hosted co-processor firmware (the usual P4+C6
  pairing), current versions (esp_hosted 3.x) do run a soft access point:
  Espressif ships an "apsta" example for exactly a P4 host with a C6
  co-processor over SDIO. So softAP is *probably* there, not verified.
- What a Rust host driver needs, in order: (a) an SDIO host on the P4: the
  P4's SDMMC controller has no driver in our patched esp-hal (the SD card is
  bit-banged for the same reason), so either a register-level driver like
  `rgb.rs` or a bit-banged 1-bit SDIO (~1 MHz, ~100 kB/s: enough for a phone
  page); (b) the ESP SDIO slave protocol (CMD5/CMD52/CMD53, the slave's
  interrupt and buffer-credit registers); (c) esp-hosted's packet header and
  its protobuf control messages (start Wi-Fi, set AP mode, SSID, channel 11,
  password), hand-written since we take no new dependencies lightly; (d) a
  network stack on the P4 (the S3's embassy-net + picoserve + DHCP/DNS code,
  which means moving the P4 loop to embassy or polling smoltcp). Estimate:
  4-8 weeks of focused work, with the risk that the factory C6 firmware
  turns out to be an old version or not esp-hosted at all.
- First safe step, if the owner agrees: a **read-only** SDIO probe that
  enumerates the C6 and reads the esp-hosted start-up message (firmware
  version, capabilities). It writes nothing to the C6 and changes nothing,
  but it does talk to the C6, so it was not done without the owner's OK.

**2. The C5 gateway also runs the hotspot: not recommended.** The gateway's
ESP-NOW works on 2.4 GHz channel 6 (docs/ESPNOW.md). One radio cannot keep
an access point on channel 11 and ESP-NOW on channel 6 at once, so the
hotspot would have to sit on channel 6, where Remote ID lives. That breaks
PLAN.md's rule (hub transmissions stay off channel 6).

**3. A separate "web head" board on a wire (recommended for now).** One more
C5 or the old S3 hub board runs the S3 hub's hotspot + phone page + CoT
(already written and tested, `firmware/hub` hotspot.rs/http.rs/api.rs/cot.rs)
on channel 11, and gets the track table from the P4 over a UART (UART2 is
free; GPIO31/32 on the socket are free pins). The P4 side is a feeder of the
same JSON the console already prints. Cost: a board and 3 wires; about a
week of work; the web-head firmware is mostly the S3 code with the node links
replaced by one P4 link. It transmits (a hotspot on channel 11, never 6) and
must not be deployed without the owner's go-ahead.

**4. USB to a laptop (works today, no radio).** The console's JSON lines carry
every track, link and aircraft once a second. A laptop can read them now; a
phone with USB-OTG serial could too. This is not the phone page, but it is
the zero-risk fallback.

**Built (2026-11-15, branch `webhead`): route 3**, with the old S3 hub as
the web head; see WEBHEAD.md. The P4 forwards the gateway's frames as they
came plus its GPS (`HubFix`) and ADSBee lines (`Csbee`), link protocol 5,
on UART2 TX GPIO31.

**Recommendation (before):** route 3 now (it reuses tested code and keeps channel 6
clean), and route 1 as the long-term goal, starting with the read-only C6
probe once the owner agrees. Built in this task for the routes: the JSON
feed (route 4, and the data route 3 needs). No hotspot was built or run.
