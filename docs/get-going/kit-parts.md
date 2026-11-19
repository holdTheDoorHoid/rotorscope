# Parts and tools

**Level: 🟡 Intermediate** · Everything the bench kit uses, how many of each, which parts you
can leave out, and the software your computer needs.

> [!NOTE]
> This list is taken from the developer's bench as of 2026-11-18, not from a finished design.
> There is no kit bill of materials yet, and no prices are given here except the plan's own
> early estimates, quoted at the end.

## The boards and modules

| Part | What it does | How many | Required? |
|---|---|---|---|
| **Seeed XIAO ESP32-C5** as a listener node | Listens for Remote ID on Wi-Fi 2.4 GHz, Wi-Fi 5 GHz or Bluetooth, or samples the radio for DJI DroneID and FPV video, whichever job the hub gives it. Each node holds up to three firmware images; the third (DroneID and FPV) only if you build it. | 1 or more (the bench runs 2) | Required: at least one |
| **Seeed XIAO ESP32-C5** as the gateway | Receives the nodes' ESP-NOW messages and passes them to the hub over a wire. It never listens for drones itself. | 1 | Required for the ESP-NOW kit |
| **Seeed XIAO ESP32-C5** as the test transmitter | Runs `rid-beacon`, the bench test signal with a fake identity, so you can prove the kit works without a drone. Any spare C5 will do, and it can be reflashed as a node afterwards. | 1 | Strongly recommended |
| **Elecrow CrowPanel Advanced 5"** (ESP32-P4) | The hub: 800x480 touch screen, microSD slot, the track table, the log. | 1 | Required |
| **microSD card** | The hub's log, evidence files, airspace pack and site settings (see below). | 1 | Optional to run, needed for logs and FAA flags |
| **GPS module**: u-blox MAX-M10S *or* Quectel LC29H(AA), with its PPS (pulse-per-second) pin brought out | The hub's own position and time. The hub finds out which one is fitted by itself. | 1 | Optional. Untested on the P4 with a real module |
| **ADSBee 1090** | Hears crewed aircraft (ADS-B on 1090 MHz) and sends them to the hub as text. The bench unit runs ADSBee firmware 0.9.0. | 1 | Optional. Untested on the P4 with a real unit |
| **ESP32-S3 board** as the web head | Would run the phone hotspot and phone page. The bench uses the earlier S3 hub board. | 1 | Optional. Its firmware does not currently link (2026-11-18) |

Notes on these parts, all from the repository:

- **Which C5.** The DroneID decoder needs a C5 with PSRAM: the XIAO ESP32-C5 has 8 MB of PSRAM
  and 8 MB of flash. A node's flash is laid out for 8 MB (`tools/partitions-node.csv`).
- **Antennas.** The plan lists dual-band 2.4/5 GHz antennas, because the XIAO C5's supplied
  antenna is for 2.4 GHz only. The bench tests, at about a metre, used the antenna supplied
  with the board. Range has not been measured with either.
- **The P4 board's chip.** The bench board carries ESP32-P4 silicon revision v1.3, with 16 MB
  of flash and 32 MB of PSRAM. The firmware is built for exactly that revision (see
  [Build and flash](kit-flash.md)). A board with a newer chip revision has not been tried.
- **The P4 board's own radio.** It has an ESP32-C6 for Wi-Fi. Rotorscope leaves it alone: its
  factory firmware has no ESP-NOW, and the C6 has no pins of its own for reflashing, so a bad
  replacement image could leave the board without a working radio. Nobody has tried. That is
  why the kit needs a separate gateway and web head.
- **The GPS.** The plan asks for a module with a PPS pin, and notes that cheaper M10 modules
  may not bring one out. The hub reads the MAX-M10S at 9600 baud as shipped and the LC29H(AA)
  at 115200, and sets either one up itself for that power-on only; nothing is saved to the
  module's own flash.
- **The ADSBee.** It must be set once, over its own USB, to send its "CSBee" text on its
  serial header. The hub cannot do this for it. Steps are in [ADSBee 1090](../ADSBEE.md).

## The SD card

The hub talks to the card itself, one data line at a time (the board wires only one), at
about 115 kB/s. That is plenty for its once-a-second log.

| Requirement | Why |
|---|---|
| **FAT32 (or FAT16)** | The file system library the hub uses reads only these. A card formatted as exFAT (most cards over 32 GB are sold that way) is reported and left alone, and nothing is logged. |
| **Formatted on your computer** | The hub never formats, never deletes and never writes outside its own folder, `ROTORSCP`. |
| **Size** | The only card tried on the bench is a 4 GB-class SDHC card (3728 MiB). Older small SDSC cards are handled in the code but have only been tested against a simulated card. Larger cards formatted as FAT32 have not been tried. |

What goes on the card, and what the hub writes there, is on
[First power-up](kit-first-run.md#prepare-the-sd-card) and [SD card and logs](../use/sd-card.md).

## Cables and small parts

| Part | Used for | How many |
|---|---|---|
| USB-C **data** cables (not charge-only) | Flashing and powering every C5, and the P4's flashing and console port | One per board you flash at once |
| A USB power supply or powered USB hub | Powering the nodes and gateway, each from its own USB port. The wired-bus guide gives up to about 0.25 A for each C5, so size the supply for all of them. | 1 |
| Jumper wires, 3.3 V logic | Gateway to hub (3 wires), GPS (5), ADSBee (2), web head (2) | About a dozen |
| A lead with a **PH2.0** 4-pin plug | The P4's UART1 header, where the gateway connects | 1 |
| A lead with an **XH2.54** plug | The P4's UART3-IN header, where the GPS connects | 1 (only with a GPS) |
| A multimeter | Checking which hole of the P4's wireless-module socket is which pin before you wire GPS PPS, the ADSBee or the web head. The socket pin numbers come from Elecrow's wiki, and the repository says to check them with a meter before wiring. | 1 |

**Only for the wired bus** (simulated only, never wired; see [The wired bus in depth](../WIREBUS.md)):
one 470 ohm resistor, one small Schottky diode per node (1N5817, 1N5819, BAT43 or BAT85, not a
1N4148), and optionally one 1 kohm resistor per node.

## Software on your computer

The scripts are written for **Linux**: they find boards under `/dev/serial/by-id/`, take locks
with `flock` and read serial ports with Python. If you did [Step 1](step-1-demo.md) and
[Step 2](step-2-one-board.md) you already have most of this.

| Tool | What for | Needed for |
|---|---|---|
| `git` | Getting the repository; `tools/p4-prepare.sh` also uses it to fetch one pinned version of esp-hal | Everything |
| Rust through `rustup`, **stable** channel | Every C5 firmware and the P4 hub. The gateway, node-wifi, node-sdr and P4 folders have a `rust-toolchain.toml` naming stable and the chip target (`riscv32imac-unknown-none-elf` for the C5, `riscv32imafc-unknown-none-elf` for the P4). node-ble and rid-beacon name Espressif's toolchain and are built for the C5 with `cargo +stable`, which needs the C5 target added once ([Step 2](step-2-one-board.md#what-you-need-to-install)) | Everything |
| `espflash` | Writing firmware to the boards. `tools/dev-board.sh` calls it for you | Flashing |
| Python 3 with the `serial` module (pyserial) | `tools/dev-board.sh` reads each board's output with it; `tools/p4-prepare.sh` reads a JSON file with it | Flashing, the P4 build |
| `sha256sum` | `tools/p4-prepare.sh` checks the P4 patch against its recorded checksum | The P4 build |
| `espup` and Espressif's `esp` Rust toolchain | The ESP32-S3 is a different processor family (Xtensa) that stock Rust cannot build for | Only the web head and S3 boards |
| An internet connection | The first builds download Rust libraries; `tools/p4-prepare.sh` downloads esp-hal; `airspace-pack` downloads public FAA, USGS, NOAA and Census data | First builds, the airspace pack |

How to install each is on [Set up your machine](../dev/setup.md).

## What it might cost

The repository gives no price list for this kit. The plan
([PLAN.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/PLAN.md), "Physical and
cost" and "Rough parts list") sets a **target** of $120 or less in parts for a Remote ID unit
and $200 or less with the ADSBee, and its rough parts list adds up to about $117 to $143.
Both were written for the **earlier ESP32-S3 hub design** with a small 2-inch screen, before
the move to the P4 touch panel, so they do not price the kit described on these pages. Treat
them as the plan's early estimates, not as a quote.

## Where this lives in the code

- `PLAN.md`: "Node chip", "Hub chip", "Rough parts list"
- `firmware/hub-p4/BRINGUP.md` and `PARITY.md`: the P4 board, its pins and the two GPS modules
- `firmware/hub-p4/src/sd.rs`, `src/sdbb.rs`, `crates/sdnative/`: the SD card
- `docs/ADSBEE.md`: the ADSBee
- `tools/partitions-node.csv`: a node's flash layout

**Next:** [Wiring](kit-wiring.md)
