# Sonoff Zigbee dongle as a Bluetooth 5 Remote ID receiver

The **Sonoff Zigbee 3.0 USB Dongle Plus** (the "-P" model: a TI CC2652P chip
behind a CP2102N USB bridge, about $25) is sold as a Zigbee stick. Its radio
also does Bluetooth 5. Loaded with NCC Group's free **Sniffle** firmware, it
becomes a Bluetooth listener that hears all three ways a drone can send
Bluetooth Remote ID:

| Remote ID over Bluetooth | Laptop Bluetooth (`--ble`) | Sonoff + Sniffle (`--sniffle`) |
|---|---|---|
| BT4 "legacy" adverts | yes | yes |
| BT5 "extended" adverts | no | yes |
| BT5 Long Range (coded PHY) | no | yes, in `coded` mode |

It only listens. Rotorscope never asks it to transmit anything: no scanning
requests, no connections, no advertising.

You can turn it back into a Zigbee stick at any time with the backup you make
in step 2.

## What you need

- The dongle: **Sonoff Zigbee 3.0 USB Dongle Plus**, model ZBDongle-P (CC2652P).
  The ZBDongle-E (Silicon Labs EFR32) is a different chip and will not work.
- A Linux computer with this repository and Rust installed.
- The Sniffle firmware file `sniffle_cc1352p1_cc2652p1.hex` from the
  [Sniffle v1.11.0 release](https://github.com/nccgroup/Sniffle/releases/tag/v1.11.0)
  (GPL-3.0). The one tested here has SHA-256
  `5a1332ae1df3a69ea68087b01d3411ccdb182c8a127f1879e23973c22462daf1`.
  Use the file **without** `_1M` in its name for this dongle.

## Find your dongle's name

Plug it in, then:

```sh
ls /dev/serial/by-id/
```

Look for `usb-ITead_Sonoff_Zigbee_3.0_USB_Dongle_Plus_...-if00-port0`. Use that
whole path everywhere below. It names this exact dongle, unlike `/dev/ttyUSB0`,
which can point at a different device after a replug.

```sh
PORT=/dev/serial/by-id/usb-ITead_Sonoff_Zigbee_3.0_USB_Dongle_Plus_...-if00-port0   # paste yours
```

Close anything else using the dongle first (Home Assistant, Zigbee2MQTT).

## Step 1: check the dongle

```sh
cargo run --release -p cc26xx-flash -- --port "$PORT" info
```

It resets the dongle into the chip's built-in loader, reads a few facts, then
restarts it. Changes nothing. You should see `wafer 0xBB41 = CC13x2/CC26x2`,
`flash 352 KB`, and `keeps the bootloader reachable through DIO15`.

## Step 2: back up the Zigbee firmware

```sh
mkdir -p ~/rotorscope-backups
cargo run --release -p cc26xx-flash -- --port "$PORT" read ~/rotorscope-backups/sonoff-zigbee.bin
sha256sum ~/rotorscope-backups/sonoff-zigbee.bin
```

This takes under a minute. The tool reads every byte, then asks the chip to
compute a checksum (CRC32) of its own flash, and only saves the file if the
two agree. The backup includes the Zigbee network settings stored on the
dongle. Keep it somewhere safe; it is your way back.

## Step 3: put Sniffle on it

```sh
# Check the file first, without touching the dongle:
cargo run --release -p cc26xx-flash -- --port "$PORT" flash sniffle_cc1352p1_cc2652p1.hex --dry-run
# Then for real (it asks you to type "yes"):
cargo run --release -p cc26xx-flash -- --port "$PORT" flash sniffle_cc1352p1_cc2652p1.hex
```

About ten seconds. The tool:

1. Checks the firmware file (every line's checksum) and refuses any file whose
   settings would switch off the chip's built-in loader or the button pin
   it uses. That is the one way to make this dongle unrecoverable over USB,
   so the tool will not do it.
2. Erases the chip, writes the file, and checks the chip's own checksum of
   what was written against the file.
3. Restarts the dongle into Sniffle.

**If it fails part-way**, it tries again by itself, up to three times. If it
still fails, just run the same command again. An erased or half-written chip
always starts in its built-in loader, so nothing is lost.

## Step 4: listen for drones

```sh
cargo run --release -p rid-view -- --sniffle "$PORT"
cargo run --release -p rid-view -- --sniffle "$PORT" --own-pos 39.9526,-75.1652   # with distance and bearing
```

The node line at the top shows `Sniffle 1.11.0` and counts: `adv` is every
Bluetooth advert heard in the last second, `rid` the Remote ID ones. In the
drone table, `HEARD` says how: `B4` legacy, `B5` extended, `LR` Long Range.

### Choosing what to listen for

One radio can listen in one way at a time, so pick a mode:

```sh
rid-view --sniffle "$PORT"                       # default: legacy + extended (most drones)
rid-view --sniffle "$PORT" --sniffle-mode coded  # Long Range only
rid-view --sniffle "$PORT" --sniffle-mode alt    # switch every 3 s between the two
```

- `1m` (default) hears BT4 legacy and BT5 extended adverts. That covers most
  drones with Bluetooth Remote ID.
- `coded` hears Long Range only. Use it when looking for a drone known to use
  Long Range, or for longest reach.
- `alt` hears both, each half the time. Remote ID repeats every message at
  least once a second, so nothing goes unseen for long, but rates halve.
  Add a second dongle (`--sniffle` twice, one per mode) to hear both all the
  time.

It listens on advertising channel 37. Remote ID is sent on all three
advertising channels, so one is enough, and for extended adverts Sniffle
follows the pointer to wherever the data is sent.

It combines with everything else `rid-view` reads: nodes, `--ble`, `--adsbee`,
`--demo`, `--http`, `--cot`.

## Going back to Zigbee

Flash your backup with the same tool:

```sh
cargo run --release -p cc26xx-flash -- --port "$PORT" flash ~/rotorscope-backups/sonoff-zigbee.bin
```

It erases Sniffle, writes the backup, checks it and restarts. The dongle comes
back exactly as it was, Zigbee network settings included.

## If something goes wrong

- **"could not reach the ROM bootloader"**: something else has the port open,
  or this is not the CP2102N dongle. Unplug, replug, close other programs, try
  `info` again.
- **"no Sniffle messages at 2000000 baud"** from `rid-view`: the dongle still
  runs Zigbee, or you flashed a `_1M` Sniffle build (that one talks at a
  different speed; use the file without `_1M`).
- **Older Sonoff dongles with a CP2102 (not CP2102N)** bridge cannot run at
  2 Mbaud. They need Sniffle's `_1M` build, which `rid-view` does not support
  yet. Check with `lsusb -v`: the CP2102N shows `ITead` as the maker.
- If all else fails, the dongle has a small BOOT button inside its case,
  wired to the same loader pin: hold it while plugging in, and the chip
  starts in its built-in loader whatever firmware it has.

## How it works (for the curious)

- `tools/cc26xx-flash` speaks the TI CC26x2 chip's built-in serial loader,
  written from TI's documentation (Technical Reference Manual SWCU185,
  "Bootloader" chapter). The dongle wires the USB chip's DTR and RTS lines to
  the CC2652P's reset and its loader pin (DIO15), so the tool can reset it into
  the loader with no button press.
- On this chip, a full erase deliberately keeps the loader switched on and
  debug access open (it rewrites a few bytes of the settings block). The tool
  allows for that.
- `rid-view --sniffle` speaks Sniffle's own serial protocol (base64 lines at
  2 Mbaud), configures a passive capture, and reads each packet's channel,
  PHY, type and signal strength. Remote ID service data (UUID 0xFFFA, app code
  0x0D) goes through the same parser the Bluetooth node firmware uses.

## What has been tested

Tested 2026-11-11 on one ZBDongle-P (CP2102N, CC2652P, Z-Stack 2.7.1 build
20210708 to start with):

- `info`, `read` (full 352 KB backup, CRC32 agreed with the chip) and `flash`
  of Sniffle 1.11.0 (CRC32 verified), then Sniffle started and answered.
- Live: the laptop's BT4 test transmitter (`rid-ble-beacon hci0`) was decoded
  at about 7 Remote ID adverts a second, RSSI around -37 dBm, every field
  correct.
- **Not yet tested on air:** BT5 extended and Long Range reception (no BT5
  transmitter was at hand; those paths are unit-tested with constructed
  packets), and restoring the Zigbee backup onto the dongle (tested only
  against the simulated chip).
