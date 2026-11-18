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
  `ROTORSCOPE_BENCH=1 ROTORSCOPE_BENCH_CAN=wifi24|wifi5|ble tools/flash-node.sh c5a` (`node_common::bench_can_mask`;
  both images get the same mask; any other value stops the build; unset = no restriction). Never
  for a deployed node.
* **Hub evidence.** `ROTORSCOPE_BENCH=1 cargo build --release --features benchlog` in `firmware/hub-p4`
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

## Crash recovery (2026-11-18, not yet run on hardware)

Every board now reboots after a panic, and every board except `rid-beacon` has a hang watchdog
(the chip's RTC watchdog) that resets it if its main work stops. After any restart the board's
`hello` line says why it last reset: `"reset":"power_on"`, `"software"` (a panic, or a restart the
firmware chose: image switch, BLE host restart, gateway epoch roll-over), `"watchdog"`, `"usb"`
(the flasher or a monitor reset it), `"brownout"`. The P4 hub says `last reset: ...` in its
`hub-p4: hello` line and in the evidence log's boot event.

| Board | Watchdog fed by | Timeout | Panic: back after | Hang: back after |
|---|---|---|---|---|
| `gateway` | relay loop, only while the beacon loop is alive (5 s) | 10 s | ~0.5 s + boot | ~10 s |
| `node-wifi` (S3, C5, `band24`, `espnow`, `wirebus`) | channel hopper, every hop | 30 s (a hub plan may dwell 10 s) | ~0.5 s + boot | ~30 s |
| `node-sdr` | capture loop, every pass, and every dump line | 10 s | ~0.5 s + boot | ~10 s |
| `node-ble` (S3, C5) | stats task, once a second | 10 s | ~0.5 s + boot | ~10 s |
| `node-lr` | stats loop in `main`, once a second | 10 s | ~0.5 s + boot | ~10 s |
| `hub-p4` | main loop, every pass | 30 s | 0.5 s (5 s within 10 s of boot) + boot | ~30 s |
| `rid-beacon` | (no watchdog) | | ~0.5 s + boot | |

The RTC watchdog runs from the chip's internal slow oscillator, so "10 s" may come out a few
percent either way. "Boot" is the board's normal start-up (a second or two for the nodes; longer for
the P4, which tests its PSRAM first).

### The `faulttest` build

To trigger the faults on purpose, build with the feature `faulttest`. It is in no `default` list, so
a normal build cannot be made to panic or hang from the console; its `hello` line says
`"faulttest":true` and `"bench":["faulttest"]` (P4: `FAULT TEST BUILD`, and `BENCH BUILD` on the
screen). It is a bench feature: `export ROTORSCOPE_BENCH=1` in the shell first, or the build
stops (see "Bench builds" below). Type a whole line and Enter:

* `fault panic`: the board panics (backtrace on USB) and must come back on its own.
* `fault hang`: the board stops feeding the watchdog (spins without yielding) and must come back
  after the timeout above.

Nodes and the gateway read it on their USB-Serial-JTAG port (the same port their JSON comes out
of); node-sdr through its normal console; the P4 hub on the USB-C (CH340) console, where `f`
starts the line (Esc cancels).

| Board | Build (from the firmware's folder, as usual, plus the feature; `ROTORSCOPE_BENCH=1` exported) |
|---|---|
| gateway | `cargo build --release --features faulttest` |
| node-wifi C5 | in `c5/`: `cargo build --release --features esp32c5,faulttest` (add `band24`, `espnow` as for `fw24` / `fw24-now`) |
| node-wifi S3 | in `s3/`: `cargo build --release --features esp32s3,faulttest` |
| node-sdr | in `c5/`: `cargo build --release --features faulttest` |
| node-ble S3 / C5 | `cargo build --release --features faulttest` / `cargo +stable build --release --no-default-features --features esp32c5,faulttest --target riscv32imac-unknown-none-elf` |
| node-lr | `cargo build --release --features faulttest` |
| hub-p4 | `cargo build --release --features faulttest` (flash as in firmware/hub-p4/BRINGUP.md) |

### The checklist (one board at a time)

Flash the `faulttest` build with the board's usual tool (`tools/dev-board.sh <elf> 90 <board>` for
the S3 and C5 boards; the P4 as in its BRINGUP.md). Note: opening the serial port resets the board,
so the first `hello` after flashing says `"usb"` or `"power_on"`, not a fault.

1. **First boot.** The `hello` line has `"faulttest":true` and a `reset` field. P4: the boot log
   also has `hub-p4: watchdog on: resets the chip if the main loop stops for N s`, with N close
   to 30 (between 15 and 60 is accepted; anything else prints `watchdog LEFT OFF` and is a FAIL to
   report with the numbers it shows).
2. **Normal running, 5 minutes.** No restart (`up_us` / uptime keeps growing, no second `hello`).
   This is the important one on the P4: run it with the SD card in, an airspace pack on the card
   (so the pack's load and check happen during the run) and the screen on. A restart here means
   the timeout is too short for something legitimate: record the last lines before it.
3. **Panic.** Type `fault panic` (for example
   `tools/dev-board.sh --log 40 c5b --send $'fault panic\r'`). Expect the `PANIC` banner with
   `faulttest: panic requested on the console`, a backtrace, then a fresh `hello` with
   `"reset":"software"` within the time in the table.
4. **Hang.** Type `fault hang` (`--send $'fault hang\r'`, reading for at least the timeout + 20 s).
   Expect the `faulttest ... hang` line, silence for about the timeout, then a fresh `hello` with
   `"reset":"watchdog"`. Time it: it should match the table within ~20 %.
5. **Hub view (nodes behind a hub).** During 3 and 4 the hub shows the node going quiet, then its
   Hello again; nothing else on the hub changes.
6. **P4 only: evidence.** After step 4, the evidence file's boot event for the new boot ends with
   `last reset: watchdog` (console `r` replays it, or read the card on the laptop).
7. **P4 only: panic at start-up.** Not testable from the console; it is covered by the 5 s wait in
   `fault.rs`. If you ever see a P4 boot-looping, the panic message repeats every ~5 s plus boot.
8. Finish by flashing the normal build again (no `faulttest`, in a shell without
   `ROTORSCOPE_BENCH`) and checking its `hello` has `"faulttest":false` and `"bench":[]`.

Under a Wi-Fi frame flood, `node-wifi` must not reset: the drain task now yields to the hopper
every 2 ms. If a busy bench (many access points, a `rid-beacon` at 10 Hz) ever shows
`"reset":"watchdog"` on a node-wifi without `fault hang`, that is a FAIL worth reporting.

## Bench builds (2026-11-18, compile-checked only)

Some cargo features and build switches exist only for the bench: they fake data (a made-up drone
whose frames are written to the **signed** evidence log, a made-up GPS fix), loop the board's own
pins back, or open debug doors on the console (panic on request, inject samples, un-redact serial
numbers, drop to the ROM loader). Nothing in an image used to tell such a build from a field build.
Now:

* **The build refuses** unless the environment has `ROTORSCOPE_BENCH=1`. Each firmware's `build.rs`
  checks its own list (`crates/link/src/benchfence.rs`) and stops with a message naming what is on.
  For a bench session: `export ROTORSCOPE_BENCH=1` in that shell only. `tools/bench/run.py` sets it
  for its own runs.
* **The image says so.** Every hello line has `"bench":[...]` (`[]` for a normal build), and a bench
  image prints `{"t":"warn","what":"bench_build","tag":"bench-build:<list>",...}` after it. The P4
  hub lists them in its `hub-p4: hello` line and in the evidence log's boot event
  (`...; bench: fakedrone,faulttest`, or `bench: none`), and shows **BENCH BUILD** on the screen
  for as long as it runs.
* **The flashing tools check.** `tools/dev-board.sh` and `tools/flash-node.sh` find the
  `bench-build:` tag in an ELF and refuse to flash it without `ROTORSCOPE_BENCH=1` (and warn with it).

| Firmware | Bench features | Bench build switches (environment at build time) |
|---|---|---|
| gateway | `faulttest` | |
| node-wifi | `faulttest`, `forgetest` (send frames a node may not originate: "Origin checks" below) | `ROTORSCOPE_BENCH_CAN`, `ROTORSCOPE_BENCH_FLIP` |
| node-ble | `faulttest` | `ROTORSCOPE_BENCH_CAN`, `ROTORSCOPE_BENCH_FLIP` |
| node-lr | `faulttest` | |
| node-sdr | `faulttest`, `bringup` (the bring-up console: BRINGUP.md "Bring-up builds") | `NODE_SDR_BOOT`, `NODE_SDR_ALLOW_UNVERIFIED_PHY` |
| hub (S3/C5 web head) | `demo` (fake drones, a made-up position, the build machine's clock), `open-hotspot` (no hotspot password) | |
| hub-p4 | `selftest`, `fakenode`, `fakedrone`, `benchlog`, `gpsloop`, `gpsloop-m10`, `webheadloop`, `embedpack`, `modesloop`, `faulttest`, `testpoint` (console `p`: own position to a fixed test point) | (`AIRSPACE_PAK` only with `embedpack`) |

Not fenced, on purpose: `rid-beacon` (it is the test transmitter; nothing fields it), hub-p4
`show-link-ids` (an operator privacy choice, not a bench aid), `modes` (a field feature), node-wifi
`ROTORSCOPE_ESPNOW_RATE` (a radio tuning). The S3 web head (`firmware/hub`) has `"bench"` in its
`boot` line (it has no `hello` line) and in `/api/state`, where the phone page shows a striped
**BENCH BUILD, NOT FOR USE** banner.

**Link key (since the same date).** A firmware build with ESP-NOW and no `tools/link.key` stops
(docs/ESPNOW.md "Securing the link"); `ROTORSCOPE_LINK_KEY_FILE=none` builds an open kit on purpose,
and `tools/flash-node.sh` then wants `--open`. The bench tests in `tools/bench/run.py` build only
wired images (no key needed).

### To check on hardware (not yet run)

Everything in this section, and the hub-p4 screen and evidence changes below, is compile-checked
only (2026-11-18, branch `sec/lane-a`). On the bench:

1. **Normal builds**: each board's hello has `"bench":[]` and no `bench_build` warning; the P4's
   hello says `evidence key: public_test (...)` (or `development_software`) and `bench: []`.
2. **A bench build** (e.g. node-wifi with `faulttest`): `"bench":["faulttest"]` plus the warning
   line; `tools/dev-board.sh <elf>` without `ROTORSCOPE_BENCH=1` refuses it before touching the
   board, with it flashes and warns.
3. **P4 screen, test key**: a strip at the bottom edge reads `TEST KEY: log not evidence` on every
   page (with a dev key: `DEV KEY: not evidence-grade`); a bench build adds `BENCH BUILD`. Check it
   is readable in both themes and does not hide anything needed (it covers the bottom 11 px).
4. **Open gateway**: flash the gateway with `ROTORSCOPE_LINK_KEY_FILE=none`; the P4 shows
   `OPEN LINK` within a few seconds of the gateway's Hello, and it goes away after the keyed gateway
   is back (its next Hello).
5. **Evidence not written**: (a) boot the P4 with no card: the strip shows
   `NO EVIDENCE (no SD): N waiting` with N growing; insert a card: the line goes away and REC shows.
   (b) On a spare card, create a *file* `ROTORSCP/EVIDENCE/EV000005.EVL` (any content) and a
   *folder* `ROTORSCP/EVIDENCE/EV000006.EVL`: the next boot writes `EV000007.EVL` (the folder's
   name is skipped) and links it to `EV000005.EVL`. Then add a *file* `EV999999.EVL`: the next
   mount uses the lowest free number (console: `using the lowest free number 1`) and evidence
   keeps recording.
   (c) Make `ROTORSCP/EVIDENCE` a *file* instead of a folder: the CSV still records, the status
   bar shows a boxed `NO EVID` instead of `REC`, the health page says `CSV only - NO EVIDENCE`,
   the strip says `NO EVIDENCE (file error): N waiting`, and the console prints a retry every
   15 s with the same `ev_errors` growing (no `TooManyOpenDirs` after many retries). Pull the
   card, delete the file on a laptop, put the card back: evidence records again (into the same
   new file chain, the queued records first).
6. **Console `p`** on a normal P4 build answers `p (test position) is in bench builds only`; a
   `testpoint` build sets the test position and logs the Setting event as before.
7. **node-sdr**: a normal build answers `freq 5756`, `reveal on`, `bootloader` and an `@F` line
   with `"e":"bring-up command: needs a bringup build (BRINGUP.md)"`; `stats`, `phy`, `plan` still
   work. A `bringup` build runs BRINGUP.md as before, and `tools/droneid-hil` passes on it.
   `@F fffffff8 AAAA` must be counted as a feed error (`hil` counters) and change nothing.
8. **Over-the-air text**: with `rid-beacon` or a fake open network whose SSID holds a control
   character (if the beacon can send one), the P4's `THREAT` console line and evidence text show
   `?` in its place.

## Origin checks (2026-11-18, compile-checked only)

Who may send which frame type (docs/ESPNOW.md "Who may send what") is checked by the gateway, the P4
hub, the P4's web-head feed and the wired bus. Host tests cover the rules (`cargo test -p link -p
wirebus`); the firmware changes are compile-checked only. On the bench, a node built with the bench
feature **`forgetest`** sends frames it must never originate, and both hops must refuse them.

### The `forgetest` build (node-wifi only)

`export ROTORSCOPE_BENCH=1` first (bench fence). Not together with `faulttest` (both read the USB
console). From `firmware/node-wifi/c5/`:

| Link under test | Build |
|---|---|
| ESP-NOW through the gateway | `cargo build --release --features esp32c5,band24,espnow,forgetest --target-dir ../target-espnow` |
| Wired bus | `cargo build --release --features esp32c5,band24,wirebus,forgetest --target-dir ../target-bus` |
| Point-to-point wire (the P4 in legacy mode) | `cargo build --release --features esp32c5,band24,forgetest` |

Flash it alone with `tools/dev-board.sh <elf> 90 <board>` (it refuses a bench ELF without
`ROTORSCOPE_BENCH=1`). Its `hello` has `"bench":["forgetest"]` and a `bench_build` warning, then
`{"t":"forge","note":"FORGE TEST BUILD: ..."}`. Type a whole line and Enter (for example
`tools/dev-board.sh --log 20 c5a --send $'forge hubfix\r'`); each answers `{"t":"forge","sent":"..."}`:

| Command | Sends | Must never |
|---|---|---|
| `forge hubfix` | a `HubFix` at 0 N 0 E, UTC in 2001 | move the web head's map or clock |
| `forge csbee` | an ADS-B line for a made-up aircraft | show an aircraft `FA4E00` |
| `forge flags` | an FAA flag report for `FORGED` | show a flag |
| `forge via` | `Via` naming 02:00:00:00:00:99, then a `Stats` | create node 02:00:00:00:00:99 on the hub |
| `forge hello` | `Hello` role Gateway, `gw key FAKE` | change the hub's `"gw"` field or `OPEN LINK` state |
| `forge threat` | `forged_link` report naming 02:00:00:00:00:99 | appear as the gateway's report |
| `forge join` | a `BusJoin` | reach the bus master or the tracker |

### To check on hardware (not yet run)

1. **Gateway (keyed kit; c5a forgetest node, c5c gateway, P4 hub).** After each `forge` command:
   the gateway's `"t":"gw"` line's `refused` goes up by one (for `forge via` the `Via`; the `Stats`
   after it is relayed under c5a's own address), and within a second a `{"t":"threat","kind":"forged_link",
   "src":"<c5a>","why":"origin",...}` line. On the P4: `hub-p4: THREAT Forged node traffic` naming
   c5a (detail 5); its `hub-p4: link` line's `refused=` does **not** move (the gateway already
   dropped the frames). Nothing in the "Must never" column happens on the P4 screen or the web head.
2. **Gateway, open kit.** The same with the gateway and node built `ROTORSCOPE_LINK_KEY_FILE=none`
   (`OPEN LINK` on the P4): the same refusals; the rules do not depend on the key.
3. **P4, wired bus (forgetest node on the bus).** After each command the `{"t":"hub"}` line's
   `"bus":{...,"refused":N}` goes up, and within a second `hub-p4: refused 1 frame(s) from <node
   MAC> ...` plus a `THREAT Forged node traffic` line. `forge via`: the `Stats` arrives under the
   node's own MAC; no node 02:00:00:00:00:99 appears in the node list.
4. **P4, point-to-point wire (legacy mode).** `forge hubfix`, `csbee`, `flags`, `join`: the
   `hub-p4: link` line's `refused=` goes up and the hub raises its own `forged_link` with an
   all-zero source (the link's own stream). `forge hello` and `forge threat` are **accepted** here
   on purpose: a frame with no `Via` on a point-to-point link is the gateway's own. `forge via`: the
   `Via` names the node's `Stats`, so node 02:00:00:00:00:99 appears: that is what a one-node wire
   can do, and why the bus is the default.
5. **Old gateway (defence in depth).** Flash the gateway built from a commit before this change
   (for example `git show 0d0bfe3:firmware/gateway/src/main.rs`; build it in a scratch checkout),
   keep the P4 on this change. `forge hubfix|csbee|flags|hello|threat|join` now reach the P4 after a
   `Via`: its `refused=` goes up for each, a hub `forged_link` names c5a, and nothing in the "Must
   never" column happens. `forge via`: one refusal (the `Via` after the gateway's `Via`); the
   `Stats` is c5a's, and the gateway's next `Hello` still sets `"gw"` (no lingering).
6. **Web head.** The `hub-p4: webhead` line's `refused=` moves with the `hub-p4: link` line's
   `refused=` (so not in 1 and 3, where the gateway or the bus already dropped the frames), and
   the web head shows none of the forged frames in any of 1-5.
7. **Node table.** With 16 relayed nodes known to the P4 (the bus and a gateway together, or
   `fakenode` plus real nodes), power one node off; after 60 s a new node takes its slot
   (`evicted=` on the `hub-p4: link` line goes up); before that the new node's frames count as
   `table_full=`. A keyed gateway with 16 nodes: a 17th node's packets count as `table_full` on the
   `"t":"gw"` line and arrive on the P4 as `forged_link` detail 6.
8. Finish by flashing the normal node build (no `forgetest`) and checking `"bench":[]`.
