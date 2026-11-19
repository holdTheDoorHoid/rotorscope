# Build and flash

**Level: 🟡 Intermediate** · From the repository on your computer to the right firmware on the
gateway, every node, the P4 hub and the test transmitter.

Run every command from the top folder of the repository unless a step says otherwise. If you
did [Step 1](step-1-demo.md) you already have the repository and Rust.

> [!IMPORTANT]
> Flash one board at a time, and plug in only the board you are working on until it is
> registered and labelled. The bench boards are told apart by name, and a wrong name sends
> firmware to the wrong board.

## 1. The toolchains

Most firmware folders carry a `rust-toolchain.toml` that names the Rust it needs, so `rustup`
picks the right one when you build there. The exceptions are node-ble and rid-beacon: their
toolchain file names Espressif's `esp` toolchain (for the S3), so their C5 builds add `+stable`
and need the C5 target added once ([Step 2](step-2-one-board.md#what-you-need-to-install));
`tools/flash-node.sh` already builds node-ble that way.

| Firmware | Chip | Rust | Target |
|---|---|---|---|
| Gateway, node-wifi (C5), node-ble (C5), node-sdr, rid-beacon (C5) | ESP32-C5 (RISC-V) | stock **stable** | `riscv32imac-unknown-none-elf` |
| P4 hub | ESP32-P4 (RISC-V) | stock **stable** | `riscv32imafc-unknown-none-elf` |
| Web head and other S3 builds | ESP32-S3 (Xtensa) | Espressif's `esp` toolchain from `espup` | `xtensa-esp32s3-none-elf` |

You need the `esp` toolchain only for the web head. Install it once with `espup install`
(see [Set up your machine](../dev/setup.md)), and in each new terminal before an S3 build run:

```bash
. ~/export-esp.sh
```

## 2. Make the kit's link key

The gateway and every ESP-NOW node share one secret **link key**. With it, each message
between them is signed, and forged or replayed messages are dropped and reported as an attack
([ESP-NOW in depth](../ESPNOW.md#securing-the-link)). Make it once per kit:

```bash
cargo run -p link-key -- new
```

It writes `tools/link.key` (readable only by you, and kept out of git) and prints the key's
**ID**, eight hex digits that name the key without revealing it. Write the ID down; the gateway
and every ESP-NOW node print it at boot. Back the file up somewhere private. To see the ID again later:

```bash
cargo run -p link-key -- id
```

`new` refuses to overwrite an existing key. Replacing a key on purpose (`new --force`) means
reflashing the gateway and every node, because boards with the old key go deaf to the kit.

**A firmware build with no key stops**, with a message saying how to make one. The P4 hub and
the test transmitter do not use ESP-NOW and need no key. Host builds, such as `cargo test`,
never need one.

## 3. Register your boards

`tools/dev-board.sh` flashes a board and reads its output while holding a per-board lock, so two
programs never flash the same board at once. It knows boards by name. Plug in **one** C5 and run:

```bash
tools/dev-board.sh --register
```

It asks the board what chip it is, gives it the next free name (`c5a`, `c5b`, `c5c`, ...) and
saves the name in `tools/boards.local`, a file that stays on your machine. Put a sticky label
with that name on the board, unplug it, and repeat for each C5. Then, with all of them plugged
in:

```bash
tools/dev-board.sh --list
```

Every board should say `attached`. The examples below use the bench's names: `c5a` and `c5b`
for nodes, `c5c` for the gateway. Use your own.

`--register` only finds boards that appear as Espressif USB devices. The P4 hub's USB port is a
different chip (a CH340), so the P4 is not registered and is flashed separately (step 6).

## 4. The gateway

Build it:

```bash
(cd firmware/gateway && cargo fw)
```

While compiling it prints `ESP-NOW link key <ID>`. Flash it and watch it for 60 seconds:

```bash
tools/dev-board.sh firmware/gateway/target/riscv32imac-unknown-none-elf/release/gateway 60 c5c
```

`dev-board.sh` prints the key ID of the image, and a **WARNING** if the image has no key or a
different one from `tools/link.key`. The gateway's `"t":"hello"` line contains
`"key":"link-key:<ID>"`.

## 5. The listener nodes

A node holds several firmware images side by side, and switches between them when the hub
gives it a different job. `tools/flash-node.sh` builds the images and writes each into its own
slot:

| Slot | Image | Notes |
|---|---|---|
| `ota_0` | node-wifi | The node boots this one first, as a 2.4 GHz Wi-Fi listener, until the hub says otherwise |
| `ota_1` | node-ble | Bluetooth Remote ID |
| `ota_2` | node-sdr | DroneID and FPV. **Only if you built it**: the script never builds node-sdr itself, and erases the slot when there is no image |
| `rsjob` | (data) | The job the node switched images for; erased by the script, so the node starts fresh |

Each image recognises the others by the name in their header, so the slot order is a
convention, not a rule.

**With the DroneID and FPV image** (recommended), build node-sdr's ESP-NOW version first:

```bash
(cd firmware/node-sdr/c5 && cargo fw-now)
```

then flash the node, pointing the script at that image:

```bash
NODE_SDR_ELF=firmware/node-sdr/target-espnow/riscv32imac-unknown-none-elf/release/node-sdr tools/flash-node.sh c5a
```

**Without it**, the node offers only Wi-Fi and Bluetooth:

```bash
tools/flash-node.sh c5a
```

Repeat for each node. The script builds node-wifi and node-ble with the link key, flashes all
the slots and reads the board for 20 seconds (give a second number after the board name for
longer). On the bench, flashing takes 15 to 20 seconds. It **refuses** to flash when:

- there is no `tools/link.key` (it tells you how to make one);
- the images carry different link keys, for example a node-sdr image built before you made the
  key: rebuild it and pass it again in `NODE_SDR_ELF`;
- an image is a bench build and `ROTORSCOPE_BENCH=1` is not set.

| Option | What it does |
|---|---|
| `--open` (before the board name) | Builds an **open** kit with no link key, on purpose. Anyone nearby could then inject fake nodes and fake jobs. The gateway must be built open too (`ROTORSCOPE_LINK_KEY_FILE=none`), and the hub then shows `OPEN LINK`. |
| `NODE_WIRED=1` | Builds every image for a single point-to-point wire to the hub instead of ESP-NOW. No key needed. |
| `NODE_BUS=1` | Builds every image for the wired bus ([The wired bus in depth](../WIREBUS.md)); simulated only, never wired. Every image on a bus node must be a bus build, and the script checks. |

## 6. The P4 hub

The P4 board's chip is an early revision (v1.3) that the official Rust support for the P4
(`esp-hal`) does not cover; it supports v3.0 and newer only. A community patch adds the older
revision. The repository keeps that patch with its provenance in
`firmware/patches/esp-hal-p4-pre-v3/`, plus one fix of the project's own: on this revision the
cache sits where the patch put the program's stack, and a memory test overwrote it. Prepare the
patched copy once:

```bash
tools/p4-prepare.sh
```

It downloads one pinned esp-hal commit into `firmware/patches/esp-hal-p4-pre-v3/upstream/`
(kept out of git), checks the patch's SHA-256 against the recorded value, and applies the patch
and the project's own fix. It does not run the community kit's own tool; it does the same two
steps in the open. Run again, it says `already prepared`.

Then build the hub:

```bash
(cd firmware/hub-p4 && cargo fw)
```

The image lands in `firmware/hub-p4/target/rev103/riscv32imafc-unknown-none-elf/release/hub-p4`.
The `rev103` folder is deliberate: images for this chip revision and for newer ones are not
interchangeable.

**Flashing the P4.** `tools/dev-board.sh` does not flash the P4. The bench flashes it with
`espflash` directly, at 460800 baud (reads at 921600 fail on this board's CH340 USB chip with
"Corrupt data"), and with **Elecrow's own bootloader and partition table** rather than the ones
`espflash` would supply. The two `elecrow-*.bin` files are cut from a backup of the board's
factory flash (the bootloader at 0x2000 to 0x8000, the partition table at 0x8000 to 0x8C00).
They are **not in the repository**, and the repository does not yet give the steps for making
that backup or cutting those files from it.

> [!WARNING]
> **If you do not already have a backup of your P4's factory flash and the two files cut from
> it, stop here.** Flashing writes over the board's factory bootloader and partition table.
> The bench backed up the whole factory flash first, and flashes only the bootloader and
> partition table taken from that backup. Do not run the command below without both files,
> and do not drop the `--bootloader` and `--partition-table` options: espflash would then
> write its own P4 bootloader, which the bench has never used on this chip revision. Without a
> backup you have no known-good way back to the factory state.

This is the bench's command. The port is the bench board's CH340 name; check yours with
`ls /dev/serial/by-id/`:

```bash
espflash flash --chip esp32p4 --port /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0 -B 460800 --bootloader elecrow-bootloader.bin --partition-table elecrow-partitions.bin firmware/hub-p4/target/rev103/riscv32imafc-unknown-none-elf/release/hub-p4
```

On the bench the P4 started by itself straight after flashing; it may still need a press of
its reset button on some runs. Because `dev-board.sh`'s lock does not cover the P4, make sure
nothing else has its port open while you flash.

## 7. The test transmitter

`rid-beacon` sends a fake drone, `TESTDRONE0000000001`, at the lowest power the radio allows.
It is for the bench only: read [Law, safety and ethics](../start/law-and-ethics.md) first.
Build the C5 version:

```bash
(cd firmware/rid-beacon && cargo +stable build --release --target riscv32imac-unknown-none-elf --no-default-features --features esp32c5)
```

You flash it in [First power-up](kit-first-run.md#prove-it-with-the-test-transmitter), once
the rest of the kit is running.

## 8. The web head (optional, does not link yet)

The web head is the S3 hub firmware (`firmware/hub`) built with its `webhead` feature. As of
2026-11-18 the S3 build does not link: it is about 18 KB over the chip's static RAM. There is
nothing to flash, and no phone page, until that is fixed. The build commands are in
[WEBHEAD.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub-p4/WEBHEAD.md)
for whoever takes that work on.

## Bench builds

Some features exist only to test the kit on the bench (made-up drones, loop-back tests, a
fixed test position). Every one of them stops the build unless `ROTORSCOPE_BENCH=1` is set,
and a board running one says so in its `hello` line; the P4 also puts `BENCH BUILD` on its
screen (not yet checked by eye). Never leave a bench
build on a kit you rely on. The list is in [Bench tests](../BENCH.md#bench-builds-2026-11-18-compile-checked-only).

## Where this lives in the code

- `tools/link-key/src/main.rs`, `crates/airlink/build.rs`, `crates/airlink/src/keysrc.rs`: the key
- `tools/dev-board.sh`: registering, locking, flashing, reading
- `tools/flash-node.sh`, `tools/partitions-node.csv`, `crates/nodeimage/`: multi-image nodes
- `tools/p4-prepare.sh`, `firmware/patches/esp-hal-p4-pre-v3/`: the P4's patched esp-hal
- `firmware/hub-p4/.cargo/config.toml`: the P4's chip revision and cache settings
- `firmware/hub-p4/BRINGUP.md`, "Build and flash"

**Next:** [First power-up](kit-first-run.md)
