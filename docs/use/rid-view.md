# The laptop viewer, rid-view

**Level: 🟡 Intermediate** · The complete guide to `rid-view`: every input, every output and
every option, with worked examples.

`rid-view` is a program for a laptop. It reads detections from Rotorscope's listener nodes, from
recordings, from a laptop's own Bluetooth adapter, from a Bluetooth 5 dongle or from an ADSBee,
and shows a live table of drones. It runs the **same tracker code as the hub**, so what the
laptop shows is what a hub would show. It can also write the hub's CSV log, serve the hub's phone
page, send TAK CoT and feed DragonSync.

It is the quickest way to see Rotorscope work: with `--demo` it needs no hardware at all (see
[Step 1: The demo, no hardware](../get-going/step-1-demo.md)).

## Running it

From the Rotorscope folder, after [setting up your machine](../dev/setup.md):

```bash
cargo run --release -p rid-view -- --demo
```

Everything after the lone `--` goes to `rid-view`. The full option list:

```bash
cargo run --release -p rid-view -- --help
```

The examples below use Philadelphia City Hall (`39.9526,-75.1652`) as "your position"; put your
own there.

## Inputs

You can combine any of these in one run. Each input counts as one node.

| Input | How to give it | What it reads | Proof |
|---|---|---|---|
| A node's USB serial port | its path, e.g. `/dev/ttyACM0` | The JSON lines a Wi-Fi or Bluetooth listener node (`node-wifi`, `node-ble`) prints on USB | Nodes **proven on the air** with the project's test transmitter |
| A recorded file | its path | The same JSON lines, saved earlier | |
| Standard input | `-` | The same lines piped in. Used when you give no input at all | |
| Simulated data | `--demo` | Two drones circling your position (or City Hall), an FPV video link, three aircraft, and one of each newer signal type | **Simulated only** |
| A laptop Bluetooth adapter | `--ble ADAPTER`, e.g. `--ble hci1` | Bluetooth 4 Remote ID adverts, through Linux's BlueZ, no root needed | **Proven on the air**, between two adapters in one laptop with `rid-ble-beacon` (2026-11-11) |
| A Sniffle dongle | `--sniffle PORT` | Bluetooth 4, Bluetooth 5 extended and Long Range Remote ID, with signal strength | **Proven on the air** from the test transmitters (2026-11-11 and 2026-11-12) |
| An ADSBee 1090 | `--adsbee PORT` | Crewed aircraft as CSBee text, from its USB console, a USB-serial adapter on its output pins, or a recording | **Heard for real** (2026-11-14) |

Up to 200 node, Bluetooth and Sniffle inputs in all. A serial port is opened in raw mode (no echo
back into the board) at 115,200 baud; USB boards ignore the speed.

**Bluetooth with `--ble`.** This is Linux only. A Bluetooth 4 adapter, such as most laptops'
built-in one, hears only the older "legacy" adverts, not Bluetooth 5 extended or Long Range
adverts. Repeat `--ble` for several adapters. BlueZ discovery sends scan requests to devices that
accept them, but Remote ID adverts do not, so a drone never receives one; nothing is connected or
paired. Details:
[`tools/rid-view/README.md`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/tools/rid-view/README.md).

**Bluetooth 5 with `--sniffle`.** A Sonoff Zigbee 3.0 USB Dongle Plus, flashed with NCC Group's
Sniffle firmware, hears what a laptop adapter cannot. It listens passively and never transmits.
How to flash it, and how to put it back: [A Sonoff dongle as a Bluetooth 5 receiver](../SONOFF.md).
Linux only. One dongle listens for one kind of Bluetooth at a time; `--sniffle-mode` picks which.

**Aircraft with `--adsbee`.** Set the ADSBee's output first; see [ADSBee 1090](../ADSBEE.md).
Aircraft are listed apart from drones and never raise an alert.

## Outputs

| Output | Option | What it gives |
|---|---|---|
| Live table | (default) | Redrawn four times a second on a terminal; once a second when piped |
| JSON lines | `--json` | The hub's JSON lines instead of the table |
| CSV log | `--log FILE.csv` | The hub's SD card CSV columns |
| Phone page | `--http ADDR` | The hub's phone page and its data, in a browser |
| TAK CoT | `--cot udp://HOST:PORT` | Cursor-on-Target, once a second |
| DragonSync | `--dragonsync`, `--dragonsync-signals` | ZeroMQ JSON for WarDragon's DragonSync |

### The table

The top line shows the time, how many drones are active and in total, and your position (or
"own position unknown (no --own-pos): no range or bearing"). Then one line per input with its
counts, and a line for the ADSBee if there is one.

The drone table has these columns: `UAS ID`, `TYPE`, `HEARD` (which radios), `RSSI` (signal
strength), `AGE`, `DIST`, `BRG` (bearing), `ALT`, `SPD`, `OPERATOR` and `SELF-ID` (the
operator's own text). Active drones come first, nearest first. A drone marked `?` is
**unconfirmed**: it has not yet been heard again at least a second after it was first heard, so
it is not announced and may be forged. What the rest means: [Reading a detection](reading-detections.md).

When there is something to show, further sections follow: `OPEN FPV LINKS`, `RADIO SIGNALS (raw
capture; no position)`, `WI-FI DRONES`, and `AIRCRAFT (ADS-B)` with `CALL`, `ICAO`, `TYPE`,
`SQK` (squawk code), `ALT`, `KT` (speed in knots), `DIST`, `BRG`, `RSSI`, `AGE` and `NOTE`.

A **new drone** rings the terminal bell (the laptop's version of the hub's buzzer) and adds a
line such as `new drone: ...` with the time.

### JSON lines

`--json` prints, once a second, one line per drone (`track`), open FPV link, radio signal, Wi-Fi
drone and aircraft, in the same format as the hub's USB output. With `--once`, it prints them
once at the end. The format is documented in
[`crates/tracker/README.md`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/crates/tracker/README.md).

### CSV log

`--log` appends a row for each drone, open link, radio signal, Wi-Fi drone and aircraft heard
since its last row, at most once a second, with the same columns as the hub's SD card log (see
[SD card and logs](sd-card.md)). The header is written only when the file is new or empty, so
you can keep appending to one file. The P4 hub's two extra FAA flag columns are not included:
`rid-view` does not run the FAA rules.

### Phone page

`--http 127.0.0.1:8080` serves the same page a hub serves over its hotspot (see
[Phone page and TAK](phone-and-tak.md)), at `http://127.0.0.1:8080/`. Port `0` picks a free
port, and `rid-view` prints the address it chose. The page can set your position, which replaces
`--own-pos`.

> [!WARNING]
> Keep `--http` on `127.0.0.1`. The page shows where drones and their pilots are and accepts a
> position; bound to any other address, anyone on that network can read and change it.
> `rid-view` prints a warning when you do.

### TAK CoT

`--cot udp://239.2.3.1:6969` sends a drone event and a pilot event for each active drone, and an
event for each active aircraft, once a second, to ATAK's default multicast address. Only
`udp://` is supported. Multicast leaves with a hop limit of 1, so it stays on your local network.
CoT needs the time; the laptop's clock provides it.

### DragonSync (WarDragon) output

`--dragonsync BIND` publishes drones, and `--dragonsync-signals BIND` publishes radio-signal
alerts (FPV video, DJI links, open FPV links), as ZeroMQ JSON for
[DragonSync](https://github.com/alphafox02/DragonSync). `BIND` is `tcp://HOST:PORT`. There are no
default ports, the two must differ, and neither can be used with `--once`. Use `127.0.0.1` when
DragonSync runs on the same laptop; avoid `0.0.0.0`, which lets anyone on the network read the
drone stream. In DragonSync's `config.ini`, set `zmq_host` and `zmq_port` to the drone stream and,
with `fpv_enabled = true`, `fpv_zmq_host` and `fpv_zmq_port` to the signal stream.

Crewed aircraft, Wi-Fi drones known only by maker or network name, plain radio energy and
tracks with no ID are not sent. At most one message per track per second, and only when it was
heard again. Signal alerts carry `--own-pos` as their location, so give it. The full field list
and DragonSync's settings are in
[`tools/rid-view/README.md`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/tools/rid-view/README.md).
**Simulated only**: checked with DragonSync's own parsing functions, not yet with a running
DragonSync or a WarDragon.

## Every option

| Option | Value | Default | Does |
|---|---|---|---|
| `INPUT...` | paths, or `-` | `-` (stdin) when nothing else is given | Node serial ports, recorded files, or stdin. Each is one node |
| `--own-pos` | `LAT,LON` or `LAT,LON,ALT` | none | Your position, for distance and bearing. `ALT` is metres above the WGS-84 ellipsoid (GPS height) |
| `--log` | `FILE.csv` | off | Append CSV rows, same columns as the hub's SD log |
| `--cot` | `udp://HOST:PORT` | off | Send TAK Cursor-on-Target once a second, e.g. `udp://239.2.3.1:6969` |
| `--dragonsync` | `tcp://HOST:PORT` | off | Publish drones to DragonSync over ZeroMQ |
| `--dragonsync-signals` | `tcp://HOST:PORT` | off | Publish signal alerts on a second port; must differ from `--dragonsync` |
| `--adsbee` | port or file | off | Read crewed aircraft from an ADSBee 1090's CSBee text |
| `--ble` | adapter, e.g. `hci1` | off | Scan for Bluetooth 4 Remote ID through BlueZ. Repeat for several adapters. Linux only |
| `--sniffle` | serial port | off | Scan for Bluetooth 4 and 5 Remote ID with a Sniffle dongle. Repeatable. Linux only |
| `--sniffle-mode` | `1m`, `coded` or `alt` | `1m` | `1m`: legacy and extended adverts; `coded`: Long Range only; `alt`: switch every 3 s, hearing each half the time |
| `--duration` | seconds | none; 10 with `--once` | Stop the live Bluetooth inputs (`--ble`, `--sniffle`) after this long |
| `--demo` | | off | Add simulated drones, an FPV link, aircraft and signals around `--own-pos` (or City Hall) |
| `--demo-secs` | seconds | 10 | How many simulated seconds `--demo --once` runs |
| `--show-link-ids` | | off | Show open FPV links' identifiers and transmitter addresses, and Wi-Fi drones' addresses. Off because they identify someone's equipment |
| `--hide-droneid-ids` | | off | Keep identity decoded from DJI DroneID (serial, O4 session key ID, flight purpose) out of every output. Positions stay, and a serial also sent in Remote ID stays |
| `--once` | | off | Read the inputs to the end, print the table once, and exit |
| `--json` | | off | Print JSON lines instead of the table |
| `--realtime` | | off | Play recorded files back at their captured speed |
| `--http` | `ADDR`, e.g. `127.0.0.1:8080` | off | Serve the phone page and its data |
| `-h`, `--help` | | | Print the option list |

Some combinations are refused: `--http`, `--dragonsync` and `--dragonsync-signals` are live
outputs and cannot be used with `--once`; `--dragonsync` and `--dragonsync-signals` cannot share
an address; `--sniffle-mode` must be one of the three names; `--own-pos` must be a real latitude
and longitude; and more than 200 node, Bluetooth and Sniffle inputs are refused.

## Timing: live, replay and `--realtime`

`rid-view` treats an input as **live** or as a **recording**:

- **Live**: a serial port, a Bluetooth adapter, a Sniffle dongle, or a terminal on stdin. These
  run on the laptop's clock.
- **Recording**: a file, or stdin from a pipe. Without `--realtime` the whole file is read at
  once, but each line is timed by the **node's own clock** (the `up_us` field in its lines; an
  ADSBee recording moves on one second per statistics line). The tracker therefore sees the run
  as it happened: a drone heard over several seconds is confirmed and announced; one heard in a
  single instant stays unconfirmed (`?`).
- With `--realtime`, a recording plays back at the speed it was captured, waiting between lines.

Several recordings at once are each timed from their own first line, so their drones are not
lined up in time with one another.

## `--once` and `--duration`

`--once` reads every input to its end, prints the table (or JSON) once and exits. It is meant for
**files**. A serial port never ends, so do not combine `--once` with one.

- With `--demo`, `--once` simulates `--demo-secs` seconds (10 by default) instantly.
- With `--ble` or `--sniffle`, `--once` listens for `--duration` seconds (10 by default), then
  prints.
- Without `--once`, `--duration` still stops the Bluetooth inputs after that long; the program
  keeps running for any other inputs.

## `--own-pos`

A laptop has no GPS, so `rid-view` only knows where you are if you say. Without `--own-pos` the
table has no distance or bearing, and CoT and DragonSync signal alerts have no location for the
sensor. `ALT` is optional. With `--http`, the page's **Settings** tab can change or clear the
position while it runs; the table notes each change.

## Worked examples

**See the demo, no hardware:**

```bash
cargo run --release -p rid-view -- --demo --own-pos 39.9526,-75.1652
```

**One table from 20 simulated seconds, then exit:**

```bash
cargo run --release -p rid-view -- --demo --once --demo-secs 20
```

**Two nodes plugged in by USB, logging to a file:**

```bash
cargo run --release -p rid-view -- /dev/ttyACM0 /dev/ttyACM1 --own-pos 39.9526,-75.1652 --log drones.csv
```

Serial port names change when boards are re-plugged; the stable names under
`/dev/serial/by-id/` are safer.

**Replay a recording as one table** (a file of a node's USB lines, saved earlier):

```bash
cargo run --release -p rid-view -- --once --own-pos 39.9526,-75.1652 recording.txt
```

**Listen with the laptop's Bluetooth for 20 seconds:**

```bash
cargo run --release -p rid-view -- --ble hci1 --once --duration 20
```

**Bluetooth 5 Long Range with a Sniffle dongle:**

```bash
cargo run --release -p rid-view -- --sniffle /dev/serial/by-id/usb-ITead_Sonoff_Zigbee_3.0_USB_Dongle_Plus_...-if00-port0 --sniffle-mode coded
```

**Crewed aircraft from an ADSBee, with the phone page:**

```bash
cargo run --release -p rid-view -- --own-pos 39.9526,-75.1652 --adsbee /dev/serial/by-id/usb-... --http 127.0.0.1:8080
```

**Feed ATAK on the local network:**

```bash
cargo run --release -p rid-view -- /dev/ttyACM0 --own-pos 39.9526,-75.1652 --cot udp://239.2.3.1:6969
```

**Feed DragonSync, checked first with the demo:**

```bash
cargo run --release -p rid-view -- --demo --own-pos 39.9526,-75.1652 --dragonsync tcp://127.0.0.1:4230 --dragonsync-signals tcp://127.0.0.1:4231
```

In the `...` paths, put your own device's name from `/dev/serial/by-id/`.

## Limits

- A laptop Bluetooth 4 adapter misses Bluetooth 5 Remote ID entirely; use a Sniffle dongle or a
  `node-ble` board for that.
- `rid-view` does not show the FAA rule flags, the drone-near-aircraft warning or the evidence
  log; those are on the P4 hub.
- With no `--own-pos`, nothing has a distance.

## Where this lives in the code

- `tools/rid-view/src/main.rs`: options, the main loop, CoT and CSV output.
- `tools/rid-view/src/input.rs`: serial ports, files, stdin, the ADSBee, replay timing.
- `tools/rid-view/src/view.rs`: the table.
- `tools/rid-view/src/http.rs`: the phone page server.
- `tools/rid-view/src/bluez.rs`, `ble.rs`, `sniffle.rs`: Bluetooth inputs.
- `tools/rid-view/src/dragonsync.rs`: DragonSync output.
- `tools/rid-view/src/demo.rs`: the simulated drones and signals.
- `crates/tracker`: the tracker, JSON, CSV and CoT shared with the hubs.

**Next:** [SD card and logs](sd-card.md)
