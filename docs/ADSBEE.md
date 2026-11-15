# ADSBee 1090: live test (owner's steps)

Rotorscope reads crewed aircraft from an ADSBee 1090 (Pants for Birds,
firmware 0.9.0 here) as **CSBee** text: one `#A:` line per aircraft and one
`#S:` statistics line, once a second, each with a CRC. The parser is
`crates/adsb`; it has only run against lines built from the format
definition, never against a real ADSBee. These steps check it on the real
unit. None of them save settings, touch the ADSBee's Wi-Fi, or update its
firmware.

**Settings are not saved.** `AT+PROTOCOL_OUT` and `AT+BAUD_RATE` change the
running settings only. Unplugging the ADSBee undoes them. Making them
permanent takes `AT+SETTINGS=SAVE`, which these steps never send; do it
yourself if you want the ADSBee to come up in CSBee mode every time.

**The hub cannot configure the ADSBee.** On firmware 0.9.0 it reads AT
commands only from its USB console (and its own Wi-Fi console), never from
the COMMS_UART header. So the one-time setup is always done over USB.

## 1. Laptop only: the USB console

Plug the ADSBee into the laptop by USB. It appears as an RP2040 CDC port.

```sh
ls -l /dev/serial/by-id/            # find the ADSBee; use its by-id path below
PORT=/dev/serial/by-id/usb-...      # <- paste it

stty -F "$PORT" 115200 raw -echo    # no echo back into the ADSBee
printf 'AT+PROTOCOL_OUT?\r\n' > "$PORT"
printf 'AT+PROTOCOL_OUT=CONSOLE,CSBEE\r\n' > "$PORT"

# Record 30 s of output (aircraft lines arrive once a second):
timeout 30 cat "$PORT" > ~/adsbee-csbee.txt
grep -c '^#A:' ~/adsbee-csbee.txt   # aircraft lines
grep '^#S:' ~/adsbee-csbee.txt | tail -1
```

Then from the Rotorscope checkout:

```sh
# The recording, once, as a table (your position for distance and bearing):
cargo run --release -p rid-view -- --once --own-pos 39.9526,-75.1652 --adsbee ~/adsbee-csbee.txt

# Live, with the phone page on the laptop too:
cargo run --release -p rid-view -- --own-pos 39.9526,-75.1652 --adsbee "$PORT" --http 127.0.0.1:8080
```

**What to look for**, in the `adsbee` line at the top of the table:

- `bad lines: N` absent (or 0). Any bad line means the CRC or the layout
  disagrees with the real unit: keep `~/adsbee-csbee.txt` and send it to
  whoever works on `crates/adsb`; real lines become test fixtures.
- `[csbee v2 ...]`: the `#S:` version. Anything other than 2 means a new
  layout; expect refusals.
- `other text` counts console log lines. Those are normal on the USB port.
- The `AIRCRAFT (ADS-B)` section lists what flightradar-type sites show
  overhead, with sensible distances and bearings from `--own-pos`.

To put the console back: `printf 'AT+PROTOCOL_OUT=CONSOLE,NONE\r\n' > "$PORT"`,
or unplug it.

## 2. On the hub (S3, feature `adsbee`)

The `adsbee` build takes the S3's third UART from node link 2 (the 5 GHz
node), so node C is not read in this build.

**Wiring** (3.3 V logic on both; no level shifter):

| ADSBee 1090 header pin | Hub (ESP32-S3) |
|---|---|
| TX (COMMS_UART, RP2040 GPIO4) | GPIO41 |
| GND | GND |
| RX | not connected (the ADSBee takes no commands on this port) |

Power the ADSBee from its own USB (or the hub's 5 V, if your carrier has it).

**On the P4 hub** (CrowPanel ESP32-P4) no feature is needed and no node link
is lost: the ADSBee's TX goes to **GPIO30** (a pin of the wireless-module
socket, free while no radio module is fitted; UART4 RX) plus GND. Status is
in the console's `{"t":"hub"}` line (`"adsb":{...}`) and the 5-second
`hub-p4: adsbee` log line. Built and loop-back tested only, not yet with a
real ADSBee (see `firmware/hub-p4/PARITY.md`).

**ADSBee setup** over its USB console, as in step 1:

```sh
printf 'AT+BAUD_RATE?\r\n' > "$PORT"                     # COMMS_UART must be 115200 (the default)
printf 'AT+PROTOCOL_OUT=COMMS_UART,CSBEE\r\n' > "$PORT"
printf 'AT+PROTOCOL_OUT?\r\n' > "$PORT"                  # check both interfaces
```

**Build and flash the hub** (`tools/dev-board.sh` holds the board lock):

```sh
cd firmware/hub/s3
. ~/export-esp.sh
cargo fw --features adsbee            # add ,demo for the demo drones and a demo helicopter too
cd ../../..
tools/dev-board.sh firmware/hub/target/xtensa-esp32s3-none-elf/release/hub 90
```

**What to look for** on the hub's USB output:

- the boot line: `"adsbee":true`, `"max_aircraft":32`, and `["adsbee_rx",41]` in `pins`;
- every second, the `hub` line's `adsb` object: `lines` climbing, `alive:true`,
  `errors:0`, `uart_err:0`, `qfull:0`, `csbee:2`;
- `{"t":"aircraft",...}` lines, one per aircraft;
- on the phone page (join the hub's hotspot): an `ADS-B ✓` chip in the strip and
  an "Aircraft (ADS-B)" section in the List tab. No new-drone alert or buzzer for
  any aircraft, ever.

If `lines` stays 0 with the ADSBee showing aircraft on its console: check
the TX/GPIO41 wire and ground first, then `AT+PROTOCOL_OUT?` on the ADSBee.
If `errors` climbs: send the recording from step 1.

## Result: laptop, real unit (2026-11-14)

Step 1 on the owner's ADSBee 1090 (firmware 0.9.0, indoors, centre city Philadelphia):
30 s gave **77 `#A:` aircraft lines, 0 bad lines**, `#S:` version 2. `rid-view --adsbee` listed
6 aircraft, among them two arrivals about 7.4 km south at 575-925 ft (the PHL approach), an
airliner at 12,000 ft 10 km NNE and one at 24,000 ft 22 km N; one aircraft had no position
yet. RSSI -71/-72 dBm. The parser (`crates/adsb`) agrees with the real CSBee layout. The console
output was set back to `CONSOLE,NONE` afterwards; nothing was saved. Not yet tried: the
ADSBee wired into a hub (the P4 has no `adsbee` input yet; the S3 build does).

## Raw frames and our own decoder (Phase 14a, 2026-11-15)

Besides CSBee, firmware 0.9.0 offers `AT+PROTOCOL_OUT=<iface>,<protocol>` with `NONE`, `RAW`,
`BEAST`, `BEAST_NO_UAT`, `BEAST_NO_UAT_UPLINK`, `CSBEE`, `MAVLINK1`, `MAVLINK2`, `GDL90` and
`AIRCRAFT_JSON` (`firmware/common/settings/settings_strs.cpp`). One protocol per interface.

- **BEAST** (used): binary, `0x1A`-framed, 12 MHz timestamp, a signal byte (the ADSBee maps
  -100..-40 dBm onto it), ~23 bytes per long frame. Parsed by `crates/modes::input::BeastReader`.
- **RAW**: text, `#MDS*<hex>;(source,dBm,SNR,48 MHz counter)`, ~70 bytes per frame. Parsed by
  `modes::input::parse_raw_line`, not used by default.
- Both carry only frames the ADSBee already **accepted**: it repairs single-bit errors itself and
  drops what it cannot validate (`adsbee.cc`, `IngestAndForwardPackets`; address/parity replies
  only for aircraft in its own table). So our error correction rarely fires on this feed; it
  is there for other raw sources (an SDR, 14c).

`crates/modes` decodes everything above the radio (CRC-24, address recovery, 1-2 bit repair,
CPR global and local, velocity, identification, Gillham and 25 ft altitudes, squawks, DF11,
Comm-B BDS 1,0/2,0/4,0/5,0/6,0, TIS-B/ADS-R) and reports a table once a second as the same
`adsb::Aircraft` records the CSBee parser gives.

### Laptop check against the ADSBee's own decode

`tools/modes-live` alternates the USB console between BEAST (4 s) and CSBEE (1.6 s), decodes the
Beast frames itself and compares every CSBee aircraft line with its own table at that moment.
It reads `AT+PROTOCOL_OUT?` first and restores it at the end (also on Ctrl-C); it never sends
`AT+SETTINGS=SAVE` and never touches Wi-Fi or COMMS_UART.

```sh
PORT=/dev/serial/by-id/usb-Raspberry_Pi_Pico_...-if00
cargo run --release -p modes-live -- live "$PORT" --secs 900 --own-pos 39.9526,-75.1652 \
    --record ~/modes.bin --log ~/modes-mismatch.txt
cargo run --release -p modes-live -- replay ~/modes.bin --own-pos 39.9526,-75.1652   # again, offline
```

**Result, 2026-11-15, centre city Philadelphia, indoors, 15 minutes** (`modes-live live`):
5229 Beast frames (DF0 2294, DF4 451, DF5 2, DF11 683, DF16 59, DF17 1737, DF20 3), 5209 used,
0 bad CRC, 5 address/parity replies dropped as unconfirmed, 511 global + 129 local positions,
0 refused. Against 404 CSBee lines whose frame counts said the ADSBee heard the aircraft that
second (27 aircraft):

| Field | Agree |
|---|---|
| aircraft also in our table | 401 / 404 (99.3%) |
| callsign | 306 / 306 |
| squawk | 19 / 19 |
| emitter category | 327 / 327 |
| barometric altitude, allowing for climb since the last update | 396 / 397 (99.7%); exactly equal 233 |
| position, allowing for motion since the last update | 362 / 369 (98.1%); median distance 1 m |

An earlier 15-minute recording the same afternoon gave 99.5% / 100% / 94% (squawk changes by
ATC caught at different moments) / 100% / 99.4% / 99.0% over 553 lines and 36 aircraft.

**Every disagreement we looked at was the ADSBee's value being stale**, not ours being wrong:
its CSBee line kept the same position or altitude for 17-113 s while the aircraft moved and
our decode of its own frames moved with it (e.g. UAL1392 climbing at ~1100 ft/min: CSBee held
28000 ft for 50 s, then jumped; our 28850 ft matched the climb). This may be an artifact of
switching protocols every few seconds; it has not been seen in CSBee-only use. Two more
observations, for whoever works on the CSBee path:

- On these lines the `UPDATED_*` flag bits (23-29) stayed set while the values repeated, so
  `adsb::Aircraft::heard()` could call a repeated line fresh. Fixed 2026-11-15: `heard()` now
  uses only the per-second frame counts, and `position_updated()` requires `heard()`.
- A Beast byte stream fed through `adsb::LineSplitter` produces `TooLong` errors (a stray `#`
  in binary); the hub's `modes` build counts those as "other", not errors.

### On the P4 hub

Build with `--features modes` and set the ADSBee once over USB with
`AT+PROTOCOL_OUT=COMMS_UART,BEAST` (CSBee lines are still accepted in that build). CSBee is
still the default build. On-chip loop-back test (`--features modesloop`: UART4 replays 300 s
of the recording above onto GPIO30): see `firmware/hub-p4/PARITY.md`.
