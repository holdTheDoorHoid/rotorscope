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
