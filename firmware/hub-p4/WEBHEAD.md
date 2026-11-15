# Web head: the phone page for the P4 hub

The P4 has no radio of its own (the onboard ESP32-C6 is left alone; see
PARITY.md "Phone page: the routes"). So a second, small board does the
networking: the **web head**. It runs the S3 hub's own firmware
(`firmware/hub`, feature `webhead`), which already has the hotspot on
channel 11, the phone page, TAK CoT and station mode. One wire from the P4
feeds it everything the P4 hears.

```
nodes --ESP-NOW--> C5 gateway --wire--> P4 hub (screen, SD, GPS, ADSBee, coverage)
                                           |
                                           | one wire: P4 GPIO31 -> web head node-A RX
                                           v
                                  web head (S3 today): hotspot ch 11, phone page, CoT, station mode
```

## Wiring

| From (P4, CrowPanel Advanced 5") | To (web head, ESP32-S3 hub board) | |
|---|---|---|
| **GPIO31** (wireless-module socket; SX1262 IRQ / nRF24 CE position) | **GPIO4** (node A RX, UART0) | 921600 baud, 3.3 V logic both sides |
| **GND** (any: UART headers, socket) | **GND** | Needed: the two boards must share ground |

That is all: one signal wire and ground. Nothing goes back from the web head
to the P4. Power each board from its own USB (or both from one supply).

- The socket pin numbers are from Elecrow's wiki: **check with a meter which
  socket hole is GPIO31 before wiring**, as for GPIO29/30 (PARITY.md). Keep
  any radio module out of the socket while it is used.
- GPIO32 (the next socket pin) is reserved for a return wire if one is ever
  needed (see "One way" below). Nothing uses it today.
- A C5 web head (later): node A RX is GPIO3 on the C5 hub layout
  (`firmware/hub/src/pins.rs`).

## What goes down the wire

The same `link` frames the nodes speak (postcard + COBS, `crates/link`):

| Frame | From | How often |
|---|---|---|
| Everything the gateway sends (`Via` markers, nodes' `Hello`, `Status`, `Stats`, `Detection`, `OpenLink`, `DroneId`, `Rf`, `DjiWifi`, `WifiDrone`) | relayed unchanged | as it arrives |
| `Hello` (role `Gateway`, firmware `hub-p4 x.y.z`) | the P4 itself | every 5 s |
| `Stats` (gateway frames, detections, frames the P4 lost) | the P4 itself | every 1 s |
| **`HubFix`** (protocol 5): GPS state, satellites, latest usable fix (1e-7 deg, mm), UTC, PPS flag | the P4's GPS | every 1 s |
| **`Csbee`** (protocol 5): one ADSBee line, CRC included | the P4's ADSBee | as it arrives (about 1 s) |
| **`Flags`** (protocol 6): possible FAA rule violations on one track (who, worst level, up to 6 rule/level/value items, airspace pack date) | the P4's rule engine (`src/faa.rs`) | every 1 s per flagged track |

Protocol 5 appends `HubFix` and `Csbee` after every older variant, so old
nodes, gateways and hubs are unaffected (an old hub would count them as
corrupt and carry on). Nodes never send them. Protocol 6 appends `Flags`
the same way. The web head keeps the 4 worst flagged tracks (its RAM is
nearly full), shows them on the phone page (`GET /api/flags`, "Possible
violations" on the list tab, a WATCH/WARNING/ALERT tag on the drone row)
and adds them to the drone's CoT remark. Tracks are matched by UAS ID (or
transmitter address), since the two boards number tracks independently.
`webheadloop` counts `Flags` frames too (tested 2026-11-15: 68 frames, 0
corrupt); the S3 side is built but not yet run on hardware.

`crates/link/src/forward.rs` (`Forwarder`) is the P4's send queue (8 KB,
about 90 ms of the wire). Two rules: whole frames only (a frame that does
not fit is dropped and counted, never cut), and a `Via` is queued together
with the frame it names or dropped with it, so a lost frame can never move a
node's name onto another node's frame. Host-tested (`cargo test -p link`).

## On the web head

Every hub build understands the feed; `--features webhead` only adds the
aircraft table for forwarded ADS-B (an `adsbee` build has one anyway).

- **Relayed nodes.** The S3 hub now pairs each `Via` with the next frame
  (`links.rs`) and keeps up to 8 relayed nodes apart (`hub.rs`
  `Relayed`): each gets its own health (role, rates, drops, `can`/`plan`)
  on the phone page's node list, the screen's Health page and a USB
  `{"t":"relayed"}` line. The P4 itself shows as a `Gateway` node on link 0.
  This also lets an S3 hub take a C5 ESP-NOW gateway directly.
- **Drones and signals.** All data frames go through
  `Tracker::ingest_frame`, the same entry the P4 uses, so protocol 4
  (DroneID, RF, DJI Wi-Fi, Wi-Fi drones) now reaches the S3's tracker too.
  Tracks are numbered independently on the two boards.
- **Own position and time.** `HubFix` is the "upstream" GPS (`gnss.rs`).
  Rules: the newer of this board's own fix and the P4's; this board's own
  UTC first, else the P4's; the P4's receiver state while this board has
  none. CoT and the phone clock work from the P4's GPS time; `utc_src`
  reads `hub_gps_pps` or `hub_gps`. The P4's time is a few milliseconds
  late on arrival (queue + wire), which CoT does not notice.
- **ADS-B.** `Csbee` lines go through `adsb::parse` and `Hub::on_adsb`,
  exactly as lines from an ADSBee on a UART. The phone page shows the
  aircraft and the ADSBee's health.
- **Hotspot.** Unchanged: channel 11 (never 6), WPA2 with the unit's own
  derived password (printed on USB at boot), `open-hotspot` only for bench
  builds. Station mode and CoT unchanged.

### One way

The web head sends nothing back. Coverage (telling nodes what to listen to)
stays on the P4 alone, so there is never a second commander. The phone page
form for a manual position sets the web head's own fallback position only
(used when neither board has a GPS fix); the P4's console `p` sets the P4's.
`show-link-ids` is a build-time choice on each board; build both the same.
If a phone-to-P4 path is ever wanted, GPIO32 is the spare pin for it.

### Memory on the S3 (no PSRAM)

The aircraft table plus the relayed-node table did not fit in static RAM
next to the 64 KB extra heap, and the `adsbee` build had already stopped
linking (by ~0.5 KB) on `p4-hub` before this work. `adsbee` and `webhead`
builds now use 56 KB of extra heap (137 -> 129 KB total heap; the S3 showed
>= 55 KB unused). **Not yet measured on the board in these builds**: check
`heap_min_free` in the `hub` line with the phone page open.

## Build

```sh
cd firmware/hub-p4 && cargo fw          # the P4: feed always on (UART2 TX GPIO31)
. ~/export-esp.sh
cd firmware/hub/s3 && cargo fw --features webhead
tools/dev-board.sh firmware/hub/target/xtensa-esp32s3-none-elf/release/hub 60
```

The P4's bench self-tests `selftest` and `gpsloop` need UART2, so those
builds have no feed.

## Tested

| Test | Result |
|---|---|
| `cargo test -p link` (host) | Forwarder: stream unchanged byte-for-byte through odd-sized writes, `Via` + frame dropped together when the queue is full, own frames never steal a held `Via`, orphan `Via` dropped; protocol 5 tags 10/11, round trip, worst case fits `MAX_FRAME` |
| `--features webheadloop` on the P4, 2026-11-15 (UART4 RX moved onto the GPIO31 pad, decoding what UART2 sends; NMEA and CSBee fed from inside the chip; real gateway with 2 nodes connected) | 45 s: 8724 bytes sent = 8724 received, 306 frames sent = 306 decoded (69 Via, 23 Hello, 79 Stats, 15 Status, 40 HubFix, 80 Csbee), **0 corrupt, 0 orphan Via, 0 dropped**, every Csbee line passed `adsb::parse` (CRC), HubFix carried the test point (39.9526, -75.1652, 12.5 m) and UTC. Gateway meanwhile: 2 nodes, 0 corrupt, 0 UART errors |
| `--features webheadloop,fakedrone` on the P4 | Detections forwarded too (33 in 40 s), 0 corrupt |
| S3 hub builds | `webhead`, stock, `adsbee`, `webhead,adsbee`, `demo,webhead`, `webhead,open-hotspot`; C5 hub with `webhead` |
| Normal P4 build | Feed running with nothing on GPIO31; see BRINGUP.md step 18 |

**Not tested:** the web head itself on hardware (the S3 board was unplugged)
and the real wire. That needs the owner: plug in the S3, flash it with
`--features webhead`, run one wire GPIO31 -> GPIO4 plus ground. What to look
for: the S3's USB `hub` line shows link 0 `role":"Gateway"`, `alive:true`,
`corrupt:0`; `relayed` = the number of nodes the P4 sees, with matching
`{"t":"relayed"}` lines; `gps.up_frames` rising once a second; and the phone
page (join `Rotorscope-XXXX` on channel 11) listing the same nodes, drones
and aircraft as the P4 screen.

## A C5 as the web head (later)

The hub firmware already builds for the C5 (`firmware/hub/c5`, `cargo fw
--features webhead`): two node links, UART0 RX on GPIO3. The C5 has one core
and 32 tracks (not 64), and its Wi-Fi radio does 2.4 and 5 GHz; the hotspot
stays on 2.4 GHz channel 11. What it would take: flash a spare C5 (not
c5a/b/c, which are nodes and the gateway), wire P4 GPIO31 -> C5 GPIO3 +
ground, and measure RAM and heap on the board (never run as a hub yet).
Its USB-Serial-JTAG console works the same.
