# Bench tests: checklist

Two boards, one command, PASS or FAIL. Run everything from the project folder
(`~/Desktop/rotorscope`).

Nothing here transmits except `rid-beacon`, the test beacon. It uses a fake drone called
`TESTDRONE0000000001` at the lowest power. Keep the two boards on the desk, a metre or so apart.

## Before you start (once)

1. Plug in **one** board with its USB-C cable (the port that shows up as a serial device).
2. Run `tools/dev-board.sh --register`. It prints a name (`s3`, `s3b`, `c5a`, `c5b`, ...), the chip
   and the MAC address, and saves them in `tools/boards.local` (this file stays on your machine).
3. **Put a sticky label on the board with that name.** Unplug it.
4. Repeat for each board. Plug in one at a time, so you always know which is which.
5. When done, plug all in and run `tools/dev-board.sh --list`. Every board should say `attached`.

Names matter: `s3` and `s3b` are the two ESP32-S3 boards, `c5a` and `c5b` the two ESP32-C5 boards.
If you move a board to a different USB port nothing changes, because it is found by its MAC.

## Which board does what

| Test group | Command | Needs | Transmitter | Listener |
|---|---|---|---|---|
| Wi-Fi beacon, Wi-Fi NAN, Bluetooth 4, 5 and Long Range (2.4 GHz) | `tools/bench/run.py --group s3` | `s3`, `s3b` | `s3` | `s3b` |
| Same, 5 GHz Wi-Fi, and C5 Bluetooth | `tools/bench/run.py --group c5` | `c5a`, `c5b` | `c5a` | `c5b` |
| Mixed chips (C5 sends, S3 listens; S3 sends, C5 listens) | `tools/bench/run.py --group cross` | `s3`, `c5a`, `c5b` | see `--list` | |
| A C5 as the 2.4 GHz Wi-Fi node (`node-wifi` built `cargo fw24`) | `tools/bench/run.py c5-to-c5-wifi24-beacon c5-to-c5-wifi24-nan s3-to-c5-wifi24-beacon s3-to-c5-wifi24-nan` | `c5a`, `c5b`, `s3` | `c5a` or `s3` | `c5b` |
| Fake open FPV links (wfb-ng, OpenHD, RubyFPV, DroneBridge) on ch 149, 161, 165 and 6 | `tools/bench/run.py --group openlink` | `c5a`, `c5b` | `c5b` | `c5a` |
| Node to hub over wires | `tools/bench/run.py --group hub` | see "Hub wiring" | node | hub (`s3`) |

One test at a time: `tools/bench/run.py --list` shows every test name, then for example

```
tools/bench/run.py s3-to-s3-wifi-beacon
tools/bench/run.py s3-to-s3-ble-lr
```

Add `--dry-run` to see what a test would do without touching anything. A full test takes about
two minutes (the first build takes longer). Do not unplug a board while a test runs.

## Reading the result

The last lines are a table. For each test:

* **PASS**: the listener decoded at least 3 messages from the test beacon, with the right
  transport (and the right band for Wi-Fi). Evidence is shown: channel, signal strength, sender address.
* **FAIL**: the boards ran, but the listener did not hear the beacon the way it should. The reason is
  printed on the same line, for example "heard it as WifiBeacon, not WifiNan".
* **ERROR**: the test could not run (board not registered, not plugged in, build broke, board stuck).
  This says nothing about the radio.

The exit code is 0 only if everything passed.

First results (2026-11-12): all five `--group c5` tests and all eight `--group cross` tests PASS,
including Bluetooth Long Range in both directions and 5 GHz NAN. A C5 built as the 2.4 GHz Wi-Fi
node passes all four of its tests (beacon and NAN, from a C5 and from the S3). A C5 drops off USB for about a second as its app starts; the tools wait for it.

## Open FPV links (2026-11-14)

`--group openlink` is 16 tests: each of the four systems on each of channels 149, 161, 165 and 6.
The beacon (`c5b`) types `x`, a channel key and the system's key (for example `x9f` = channel 165,
fake wfb-ng). The listener (`c5a`) runs `node-wifi`: the 5 GHz build for 149/161/165, the 2.4 GHz
build (`cargo fw24`) for channel 6. PASS = at least 3 `"t":"open"` lines with that system, its
fake ID and that channel. The fake frames and IDs are described in `firmware/rid-beacon/README.md`.

**Result: 16 of 16 PASS** on the first full run (`bench-logs/20261114-181014`), boards about a
metre apart, beacon at 2 dBm, 0 failed sends.

| Channel | Systems | Reports (each test) | RSSI | Frames counted in 30 s |
|---|---|---|---|---|
| 149 | all four | 26-27 | −43 to −41 dBm | ~430 (149 is the listener's home channel) |
| 161 | all four | 26 | −43 to −42 dBm | ~35 (listener visits for 50 ms in 800) |
| 165 | all four | 24-27 | −45 to −44 dBm | ~34 |
| 6 | all four | 25-26 | −41 to −38 dBm | ~420 |

The two open questions from PLAN 11a:
- **Does the C5 receive channel 165 in normal Wi-Fi mode? Yes.** All four systems passed there,
  with frames received at the same rate as on 161.
- **Does it receive wfb-ng's default LDPC/STBC-coded frames? Still open.** Our transmitter cannot
  send them: the ESP32 raw-TX API sets only the PHY mode and rate, with no LDPC or STBC switch. So
  these tests send plain coded frames. Answering this needs real wfb-ng/OpenIPC hardware with its
  default video settings.

## Hub wiring (two boards)

The node's TX pin goes to the hub's RX pin, and the two grounds are joined. Three wires or fewer.
Both boards are 3.3 V; never use 5 V pins. The pins are placeholders from the code
(`firmware/hub/src/pins.rs`, `firmware/node-*/src/`). Check them against your boards' printed labels.

| Test | Node | Node TX pin | Hub RX pin (S3 hub) | Also |
|---|---|---|---|---|
| `hub-uart-s3-wifi` | `s3b` running node-wifi | GPIO43 (labelled TX / D6) | GPIO4 | GND to GND |
| `hub-uart-s3-ble` | `s3b` running node-ble | GPIO17 | GPIO6 | GND to GND |
| `hub-uart-c5-wifi` | `c5a` running node-wifi | GPIO11 (labelled TX) | GPIO15 | GND to GND |
| `hub-uart-c5-ble` | `c5a` running node-ble | GPIO10 | GPIO6 | GND to GND |

The hub (`s3`) and the node each stay plugged into the computer by USB. Power the hub and the node
from USB; do not also power them from the wire. Change the wires between tests.
The C5 pins have not been checked against a real board's labels.

PASS means the hub counted at least 10 good messages from the node on that link. If frames show up on
a different link than expected, the wire is on the wrong hub pin.

## When something fails

1. Read the reason on the FAIL line. It usually says what to check.
2. Open the raw logs. Each run saves them in `bench-logs/<date-time>/<test>/`:
   `tx.log` is the transmitter, `rx.log` the listener (`node.log` and `hub.log` for the hub tests),
   `result.json` the verdict and numbers. Look at the last few lines:
   `tail -20 bench-logs/*/s3-to-s3-ble-lr/rx.log`
3. "Listener printed nothing" or "never printed hello": check the cable (data cable, not charge-only),
   then run the test again. Boards sometimes need to be unplugged and replugged.
4. "Transmitter did not send ..." or "isolation keys not applied": the beacon itself is the problem.
5. "listener alive but heard no ...": both boards work but the radio signal did not arrive. Move the
   boards closer, away from the laptop, and try again. Wi-Fi 5 GHz and Bluetooth Long Range are the
   least tried; a real FAIL there is a finding, so save the `bench-logs/` folder.
6. "busy" or long waits: another run is using the board. `tools/dev-board.sh --list` shows who holds
   it. Wait, or if it is a crashed run, ask before forcing anything.
7. Do not run `espflash` or `cargo run` directly on these boards: they skip the lock.

## How the lock works (for the curious)

Each board has its own lock file under `~/.local/state/rotorscope/locks/`, named after the board's
MAC. Anything that flashes or reads a board takes that board's lock first, so two runs cannot flash
the same board at once, while the two boards of one test can run at the same time.
The old lock lived in `/tmp`. A sandboxed program can have its own private `/tmp`, so two programs
could each hold "the" lock without seeing each other. That is the most likely cause of the earlier
report that the lock did not serialize (not proven; it could not be reproduced without hardware).

## End-to-end through the P4 (2026-11-15)

Every signal we may legally send, through the whole chain: test transmitter (`c5b`, rid-beacon,
2 dBm Wi-Fi, -15 dBm BLE, about a metre away) -> listener node `c5a` (multi-image, ESP-NOW) ->
gateway `c5c` -> wire -> P4 hub. **PASS means the hub's own tracker table had it**, read from the
hub's console with a bench-only build (below), not from the node.

### How it was run

* **One role at a time.** With one listener node the coordinator always gives it Wi-Fi 2.4. A bench
  build switch makes the node offer only one role, so the hub assigns that role:
  `ROTORSCOPE_BENCH_CAN=wifi24|wifi5|ble tools/flash-node.sh c5a` (`node_common::bench_can_mask`;
  both images get the same mask; any other value stops the build; unset = no restriction). Never
  for a deployed node.
* **Hub evidence.** `cargo build --release --features benchlog` in `firmware/hub-p4`
  (`src/bench.rs`): a `bench new-slot` / `new-link` / `new-rf` / `new-wifidrone` line the moment the
  tracker gets a new drone slot (transport and band), open-link stream, RF emitter or Wi-Fi drone,
  a table summary every 5 s, and at boot a count by kind of every row in the previous SD log file.
  It prints only our test IDs (`TESTDRONE...`); link IDs only as `fake` / `none` / `other`.
* Per role: session A = hub reboot, beacon on, reflash `c5a` with the mask (role switch timing);
  session B = hub reboot, then one transmission at a time (45 s each for Remote ID, 30 s for each
  fake link, 2 s of silence between). Each hub boot writes its own SD log file; the next boot counts it.
* "Time" = from typing the key on the beacon to the hub's `new-` line. It includes the beacon's
  own restart (opening its console resets it, about 1 s), so the chain itself is faster.

### 1. Remote ID through the hub

One track all along: `TESTDRONE0000000001`, `drones=1`, `decode_err=0`, location present.

| Signal | Node role (hub plan) | Hub slot | Time | Heard by the hub (segment) | RSSI | Result |
|---|---|---|---|---|---|---|
| Wi-Fi beacon, ch 6 | Wi-Fi 2.4 (`0x1503`) | WifiBeacon / 2.4, ch 6 | 2.4 s | 26 in 45 s | -38 | PASS |
| Wi-Fi NAN, ch 6 | Wi-Fi 2.4 | WifiNan / 2.4, ch 6 | 1.6 s | 33 in 45 s | -40 | PASS |
| Wi-Fi beacon, ch 149 | Wi-Fi 5 (`0x8a0c`) | WifiBeacon / 5, ch 149 | 2.8 s | 27 in 45 s | -41 | PASS |
| Wi-Fi NAN, ch 149 | Wi-Fi 5 | WifiNan / 5, ch 149 | 2.8 s | 30 in 45 s | -41 | PASS |
| Bluetooth 4 legacy | Bluetooth (`0x6749`) | BleLegacy | 1.8 s | 158 in 45 s | -58 | PASS |
| Bluetooth 5 extended | Bluetooth | BleExtended | 2.5 s | 76 in 45 s | -57 | PASS |
| Bluetooth 5 Long Range | Bluetooth | BleLongRange | 2.6 s | 55 in 45 s | -57 | PASS |

The first legacy advert carries one message, not always the Basic ID, so its slot appears a moment
before the ID does; the next adverts fill it in on the same track. No transmission showed up in
another transport's slot.

### 2. Fake open FPV links through the hub's open-link table

All 16 PASS: each system appeared within 1.5-3.2 s of its key, with the fake ID recognised
(`id=fake`; OpenHD has no clear ID: `none`).

| Channel | Node role | wfb-ng | OpenHD | RubyFPV | DroneBridge | RSSI |
|---|---|---|---|---|---|---|
| 6 | Wi-Fi 2.4 | 1.5 s, 23 reports | 1.7 s, 25 | 1.7 s, 24 | 1.7 s, 28 | -39 |
| 149 | Wi-Fi 5 | 2.2 s, 22 | 1.8 s, 24 | 2.4 s, 22 | 2.8 s, 24 | -41 |
| 161 | Wi-Fi 5 | 3.0 s, ~19 | 3.2 s, ~18 | 1.8 s, ~21 | 2.4 s, ~19 | -43 |
| 165 | Wi-Fi 5 | 2.8 s, ~18 | 3.2 s, ~14 | 2.0 s, ~17 | 2.2 s, ~18 | -45 |

("reports" = the node's once-a-second sightings the hub received in that 30 s; the same link on a
second channel is the same table entry, so 161/165 are the increase.) One extra stream: the ch 6
wfb-ng link was also reported on **ch 1** once, while the node visited 1: at a metre the 2 dBm
signal is decoded 25 MHz off its channel, and the node labels a frame with the channel it is tuned
to. A real effect of the bench distance, not a fault; it would not happen at range.

### 3. Role switching under the hub

| Role | Flash done -> hub assigns | Node applies | First detection on the hub |
|---|---|---|---|
| Wi-Fi 2.4 (already running it) | 4.6 s (plan echoed; same image) | at once | detections never stopped |
| Wi-Fi 5 (same image) | 1.0 s | at once (0 band or channel errors) | 2.2 s after boot |
| Bluetooth (image switch) | 2.0 s | node-wifi switches image; node-ble up 1.0 s later | 4.0 s after the first boot |

Flashing itself takes 15-20 s before that. Every assignment was `applied=true` on the hub after.

### 4. DroneID (chip only)

`tools/droneid-hil` on `c5b` (node-sdr `cargo fw-now`): the 13 public-recording bursts at two
levels. 200 codes (9.4 dB): 13/13 detected, 13/13 CRC-valid, median decode 151 ms (max 158).
150 codes (6.9 dB): 13/13, 13/13, median 144 ms (max 151). Same as 2026-11-14: no regression.
**Chip only by design:** HIL sightings are never sent to the hub, so a recording can never appear
as a live drone. A real DroneID on the hub needs a real DJI drone.

### 5. FPV scanner, 2 minutes, through the hub

`c5b` pinned to FPV (`role fpv`), reporting through the gateway (32 uplink windows, 32 ACKed).
58 sweeps of the 31 reachable channels; 84 loud dwells analysed: 77 `wifi`, 7 `energy`,
**0 `analog_video`**, 0 `fpv_carrier`, 0 `hdzero`, 0 `lte_style`, 0 `wifi_narrow`. The hub's RF
table stayed empty (`rf=0` in every summary): Wi-Fi and plain energy are never sent, as designed.
PASS as a sanity check. (The hub kept asking for DroneID, `applied=false`, because of the pin.)

### 6. SD card

Each hub boot counted the previous file on the card (read back on the device, every row):

| File | Session | Rows | Test drone rows | Open-link rows | Kinds |
|---|---|---|---|---|---|
| LOG00010 | Wi-Fi 2.4, A | 138 | 137 | 0 | WifiBeacon, WifiBeacon\|WifiNan |
| LOG00011 | Wi-Fi 2.4, B | 508 | 149 | 358 | + WfbNg, OpenHd, RubyFpv, DroneBridge |
| LOG00012 | Wi-Fi 5, A | 114 | 113 | 0 | WifiNan5G, WifiBeacon5G\|WifiNan5G |
| LOG00013 | Wi-Fi 5, B | 1205 | 150 | 1054 | + WfbNg5G, OpenHd5G, RubyFpv5G, DroneBridge5G |
| LOG00014 | Bluetooth, A | 113 | 112 | 0 | BleLegacy\|BleExtended\|BleLongRange |
| LOG00016 / 17 | Bluetooth (after the fix) | 139 / 200 | 137 / 198 | 0 | BleLegacy, ...\|BleExtended\|BleLongRange |

(Each count includes the header line. The one "other drone" row in 16 and 17 is the Bluetooth
track's first second, before its Basic ID arrived: an empty ID, not a foreign drone.) Every
flush read-back OK, 0 errors, 0 dropped rows. Nothing deleted or formatted.

### 7. Health over the whole run

Ten hub sessions, about 34 minutes logged, 11,228 frames from the gateway: **2 corrupt frames,
8 UART errors** (5 in one Wi-Fi 2.4 session, 1, 2), **0 ring_full**, 0 lcd DMA errors. Loop
max_gap 6.8-7.2 ms normally (the screen band); worst 17.0 ms, at SD flushes (BRINGUP: 18.3 ms).
Node uplinks: 0 failed sends.

### Bug found and fixed: Bluetooth node reset under Long Range traffic

`node-ble` with ESP-NOW reset itself in the controller,
`ASSERT r_ble_lll_mmgmt_block_cnt_check:669 0 1`, while the beacon sent all three Bluetooth
transports: 4 resets in 120 s (first run: 3 in 100 s). Isolated on `c5a`:

| Build, beacon | Resets |
|---|---|
| ESP-NOW, controller-timed 800 ms scans: all three / legacy only / extended only / Long Range only | 4 in 120 s / 0 in 90 / 0 in 90 / 1 in 90 |
| ESP-NOW, bigger HCI event buffers (24 x 257 B) | 5 in 150 s (no help) |
| Wired (no ESP-NOW, one endless scan), all three | 0 in 150 s |
| ESP-NOW, one endless scan, all three | 0 in 150 s |
| **ESP-NOW, host stops each 800 ms scan (the fix)**, all three | **0 in 180 s** (and 0 in the 100 s hub run) |

So the trigger is the controller ending a scan by its duration while Long Range (coded PHY) reports
are in flight. Fix (`firmware/node-ble/src/bin/main.rs`): no controller duration; the host drops
the scan session after `SCAN_ON_MS`, which disables the scan. Same 800/200 split. Uplink with the
fix, 180 s: 178 gaps, 220 sends, 0 failed, 44 gateway beacons and 30 hub commands heard (fewer
beacons than the 0.4/s with the controller stop, but commands still arrive). Each reset cost about
2 s of blindness; the hub track survived them.

### Not testable with our hardware

| Signal | Needs |
|---|---|
| Real DJI OcuSync / O4 DroneID on the hub | a real DJI drone (we never transmit DroneID) |
| DJI legacy Wi-Fi DroneID | a Wi-Fi DJI (Spark, Mavic Air, Mini SE); we never transmit it |
| Analog FPV video | a 5.8 GHz VTX and an amateur radio licence |
| HDZero, Walksnail | an HDZero VTX / Walksnail kit (and HDZero scanning is not built) |
| ExpressLRS | an LR1121 or SX1280 receiver (not built) |
| Wi-Fi drone makers (Tello, Parrot, ...) | a real one; we never imitate a maker's network |
| China GB 46750 Remote ID | a test-beacon mode or a DJI on 2026 firmware |
| wfb-ng's real LDPC/STBC video | OpenIPC hardware |
