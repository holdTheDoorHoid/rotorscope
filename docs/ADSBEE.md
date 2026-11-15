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
