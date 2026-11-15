# Node B: Bluetooth Remote ID listener

Bare-metal Rust (esp-hal 1.2, esp-radio 1.0.0-beta.1, trouble-host 0.8, embassy).
It listens for ASTM F3411 Remote ID on Bluetooth: BT4 legacy adverts, BT5
extended adverts, and BT5 Long Range (coded PHY). It never transmits.

## Build

The same source tree builds for both chips. Pick the chip with a feature.

**ESP32-S3** (Xtensa, needs the `esp` toolchain from `espup`; this is the default):

```sh
. ~/export-esp.sh
cargo build --release
# -> target/xtensa-esp32s3-none-elf/release/node-ble
```

**ESP32-C5** (RISC-V, builds on stable Rust):

```sh
cargo +stable build --release --no-default-features --features esp32c5 \
      --target riscv32imac-unknown-none-elf
# -> target/riscv32imac-unknown-none-elf/release/node-ble
```

`rust-toolchain.toml` selects `esp`, so the C5 command overrides it with `+stable`.
The `[unstable] build-std` line in `.cargo/config.toml` only applies to the S3 build.
Stable cargo ignores it.

Flash and watch through the shared lock. Never call espflash directly:

```sh
tools/dev-board.sh firmware/node-ble/target/xtensa-esp32s3-none-elf/release/node-ble 30
```

## What it does

- **Scanning:** an extended scan on the 1M and coded PHYs at the same time
  (`Scanner::scan_ext`, `PhySet::M1Coded`).
  - Passive.
  - Duplicate filtering off, because repeated adverts carry fresh positions.
  - The scan window equals the interval (100 ms, `SCAN_INTERVAL_MS`). The
    controller alternates the two PHYs, one interval each.
- **Legacy adverts:** legacy BT4 PDUs arrive as extended reports with the legacy
  bit set. On the S3 this was verified on hardware: the `old` counter (old-style
  LE Advertising Report events) stays at 0 while `legacy` counts every ambient
  advert.
- **Filtering:** the report handler runs inside the BLE host's receive loop.
  - It counts every advert.
  - It runs a cheap check for Service Data UUID 0xFFFA, app code 0x0D.
  - It copies only matching reports into a 16-entry queue. When the queue is
    full it counts a drop, so HCI is never blocked.
  - It reassembles chained extended adverts, for packs that do not fit in one
    HCI event.
- **Decoding:** a separate task uses `rid::ble::extract`, then
  `rid::message::raw_messages`, then `rid::message::decode`.
- **Transport classification:**
  - legacy PDU → `BleLegacy`
  - extended on 1M → `BleExtended`
  - coded on either leg → `BleLongRange`

## Outputs

**USB** (USB-Serial-JTAG, the board's USB-C port) carries JSON lines. Every line is
a JSON object. Log records are wrapped as `{"t":"log",...}` too.

| `t` | When | Fields |
|---|---|---|
| `hello` | boot | `node`, `fw`, `chip`, `proto`, `scan{phys,interval_ms,window_ms,active,dup_filter}`, `uart_baud` |
| `rid` | each Remote ID advert | `tr`, `ch` (0: HCI does not report the channel), `rssi`, `mac`, `ctr`, `up_us`, `msgs[]` (decoded; an undecodable message shows as `{"error":..,"raw":hex}`), `trunc` only when data was lost |
| `rid_err` | 0xFFFA advert whose payload `rid` rejects | `tr`, `rssi`, `mac`, `up_us`, `error`, `len`, `data` (hex) |
| `stats` | 1 s | per-second counts: `adv` (all adverts), split into `legacy`/`ext1m`/`coded`; `old`; `rid`/`rid_ok`/`rid_err`; `trunc`; `drops` (queue); `usb_drops`/`uart_drops`; `hci_err`; `scan_err`; plus `heap_free` and running totals `tot_adv`/`tot_rid`/`tot_drops` |
| `log` | warn and above | `lvl`, `target`, `msg`. The level is set by `ESP_LOG` in `.cargo/esp-config.toml` |

Output to USB goes through the async driver and an 8 KB pipe. If no host is
reading, lines are dropped and counted in `usb_drops`. The radio and the hub link
are never blocked.

**UART to the hub** carries `link::Frame`, as postcard + COBS via `link::encode`,
at 921 600 baud, 8N1, TX only for now.

| Chip | Node TX (to hub RX) | Node RX (hub commands) |
|---|---|---|
| ESP32-S3 | GPIO17 | GPIO18 |
| ESP32-C5 | GPIO11 (XIAO D6) | GPIO12 (XIAO D7) |

The C5 pins moved from GPIO10/9 to the same pins node-wifi and the gateway
use, so every image on a multi-image board talks to the same carrier wiring.
The S3 pins are placeholders until the carrier board fixes them.

| Frame | When |
|---|---|
| `Hello` | at boot and every 5 s |
| `Detection` | per Remote ID advert |
| `Stats` | 1 s, with **running totals**: `frames_seen` = adverts, `rid_found`, and `dropped` = queue drops + UART drops |

## Roles and hub commands (protocol 4)

The node sends `Frame::Status { role: Ble, can, plan }` with every Hello and
when it changes; `can` covers every image in the board's flash (see
node-wifi's README). `Assign(Ble)` only sets the plan id it echoes (Bluetooth
has no channel plan). Any other role switches image (`firmware/node-common`).

## ESP-NOW (feature `espnow`, C5)

```sh
cargo +stable build --release --no-default-features --features esp32c5,espnow \
      --target riscv32imac-unknown-none-elf
```

Hub frames go to the ESP-NOW gateway instead of the UART (`src/uplink.rs`),
and the hub's commands come back the same way. Bluetooth and Wi-Fi share the
C5's one radio. With both simply running (esp-radio `coex`), Wi-Fi heard
**nothing** on the bench: 0 gateway beacons in 45 s, with the scan window at
100% and at 80% of the interval, and with the coexistence preference set to
Wi-Fi. So the node time-shares: the scan runs 800 ms (then the host
disables it: a controller-timed stop crashed the controller under Long Range
traffic, see docs/BENCH.md "End-to-end through the P4"), stays off for 200 ms (`SCAN_ON_MS`,
`WIFI_GAP_MS`), and batches are sent only in that gap, when the gateway's
beacons and commands are heard too. The node also takes the gateway's
address from its command packets (they get through the gaps more often than
its broadcast beacons) and keeps it for 30 s after the last beacon, command or
ACKed send.

Bench, 2026-11-15, c5a, gateway c5c ~1 m away, same spot, ambient adverts only:

| Build | Adverts heard | Uplink |
|---|---|---|
| wired (no ESP-NOW), 114 s | 29.4 / s | n/a |
| `espnow`, 87 s in the Bluetooth role | 24.1 / s (**-18%**) | 80 gaps, 77 sends tried, 77 delivered, 0 failed, 1 batch dropped (at boot, before the gateway was known); 40 beacons and 14 hub commands heard |

So ESP-NOW costs the Bluetooth scan about a fifth of its listening time,
which is the 200 ms gap per second. Earlier settings that did not work: both
stacks at once (0 beacons in 45 s, also with the scan window at 80% of the
interval or the coexistence preference set to Wi-Fi), and 900/100 with the
host cancelling the scan (often a minute or more with nothing heard).

## Panics

After a panic the backtrace is printed to USB and the chip reboots
(`custom_halt`). An unattended node should not sit halted.

## Bench test without a drone

A Linux laptop's BlueZ can send a BT4 legacy Basic ID. The data is 27 bytes
(app code, counter, then a 25-byte message), which together with the service
data header fills the 31-byte legacy advert exactly. So name, tx-power and
appearance must be off.

```
bluetoothctl
menu advertise
name off
tx-power off
appearance off
discoverable off
service 0xfffa 0x0d 0x01 0x02 0x12 0x54 0x45 0x53 0x54 0x42 0x4c 0x45 0x30 0x30 0x30 0x30 0x30 0x30 0x30 0x30 0x30 0x30 0x30 0x30 0x30 0x00 0x00 0x00
back
advertise broadcast
...
advertise off
```

This sends Basic ID, serial number, multirotor, `TESTBLE0000000000000`. Coded-PHY
and extended-advert reception need the Rust test transmitter (`firmware/rid-beacon`)
on a second board.


## Bench tests

Two-board tests that flash and judge this firmware: `tools/bench/run.py --list` (see `docs/BENCH.md`).
