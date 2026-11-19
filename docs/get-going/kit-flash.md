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

**No build needs a key.** Since 2026-11-18 every ESP-NOW node has a key of its own, kept in a
small part of its flash that firmware updates never touch, and the P4 hub holds the kit's
**master** key; the gateway holds nothing secret. Firmware images carry no key, so you can build
and flash everything first. Until you seal it, the kit runs **open**: it works, but nothing on the
radio link is checked, and the hub's screen says `OPEN LINK`.

You can make the kit master now, once per kit:

```bash
cargo run -p link-key -- kit new
```

It writes `tools/kit.key` (readable only by you, and kept out of git) and an empty
`tools/kit.roster`, and prints the **kit id**, eight hex digits that name the kit without
revealing the key. Back `kit.key` up somewhere private and never paste it anywhere. Writing the
keys to the boards comes after flashing: [step 9](#9-seal-the-kits-link).

The old commands `link-key new` and `link-key id` and the file `tools/link.key` belonged to the
firmware before 2026-11-18, which compiled one key into every image. Nothing reads them any more.

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

Flash it and watch it for 60 seconds:

```bash
tools/dev-board.sh firmware/gateway/target/riscv32imac-unknown-none-elf/release/gateway 60 c5c
```

The gateway holds no key, so there is nothing to check here: its `"t":"hello"` line always says
`"key":"none"` and "relay only". Once the hub is running it adds a `"t":"roster"` line; before
that it passes no sealed node packet on.

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
| `rskey` | (data) | The node's own link key record, written by `link-key` (step 9). The script never erases or writes it, so reflashing keeps the key |

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

Repeat for each node. The script builds node-wifi and node-ble with ESP-NOW, flashes all the
slots and reads the board for 20 seconds (give a second number after the board name for longer).
On the bench, flashing takes 15 to 20 seconds. It **refuses** to flash when:

- an image does not fit the node, for example a node-sdr image built before 2026-11-18 or a wired
  one in an ESP-NOW node: rebuild it and pass it again in `NODE_SDR_ELF`;
- an image is a bench build and `ROTORSCOPE_BENCH=1` is not set.

After flashing it reads the node's boot line and calls the flash good only when the node says it
holds a key (`"auth":"kit:..."`). A node that has not been sealed yet reports an open record or
none: the script says so and exits with an error, which is expected before step 9. Pass `--open`
while you are still running open.

| Option | What it does |
|---|---|
| `--open` (before the board name) | Accepts a node with an **open** key record as a good flash, on purpose. Anyone nearby could then inject fake nodes and fake jobs, and the hub shows `OPEN LINK`. |
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
that backup or cutting those files from it. Since 2026-11-18 the hub is flashed with
`tools/partitions-hub.csv` instead of `elecrow-partitions.bin`: Elecrow's own four rows,
unchanged, plus two small areas after them for the kit's keys (`rskit`) and the hub's boot counter
(`rsboot`). Without them the hub's screen says `NO KIT KEYS` once the kit is sealed.

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
espflash flash --chip esp32p4 --port /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0 -B 460800 --bootloader elecrow-bootloader.bin --partition-table tools/partitions-hub.csv firmware/hub-p4/target/rev103/riscv32imafc-unknown-none-elf/release/hub-p4
```

On the bench the P4 started by itself straight after flashing; it may still need a press of
its reset button on some runs.

`dev-board.sh` does not build or flash the P4 itself, but it can hold the hub's lock while that
command runs, so two sessions cannot flash it at once. Add the hub to `tools/boards.local` by
hand once (step 9 shows the line), then run the same command through `--run`, with
`"$BOARD_PORT"` in place of the port:

```bash
tools/dev-board.sh --run 'espflash flash --chip esp32p4 --port "$BOARD_PORT" -B 460800 --non-interactive --bootloader elecrow-bootloader.bin --partition-table tools/partitions-hub.csv firmware/hub-p4/target/rev103/riscv32imafc-unknown-none-elf/release/hub-p4' 300 p4
```

Run on the bench 2026-11-19. Without the `p4` line, make sure nothing else has the port open
while you flash.

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

## 9. Seal the kit's link

With the gateway, every node and the hub flashed, the kit works open. Sealing it writes the hub's
master and roster and each node's own key to their flash, with `link-key`, the boards on USB. The
steps, with `--dry-run` first, are in
[ESP-NOW in depth, "Kits ship open; sealing yours"](../ESPNOW.md#kits-ship-open-sealing-yours). In
short:

```bash
cargo run -p link-key -- hub p4
cargo run -p link-key -- node c5a --label "roof" --hub p4
```

The P4 is not registered by `--register`, so add a `p4` line for it to `tools/boards.local` by
hand first. A sealed node prints its kit id, slot and key check at boot, and the
hub's strip stops saying `OPEN LINK`. Never paste `tools/kit.key` anywhere.

## Bench builds

Some features exist only to test the kit on the bench (made-up drones, loop-back tests, a
fixed test position). Every one of them stops the build unless `ROTORSCOPE_BENCH=1` is set,
and a board running one says so in its `hello` line; the P4 also puts `BENCH BUILD` on its
screen (not yet checked by eye). Never leave a bench
build on a kit you rely on. The list is in [Bench tests](../BENCH.md#bench-builds-2026-11-18-compile-checked-only).

## Where this lives in the code

- `tools/link-key/src/main.rs`, `crates/kitkeys/`: the kit's keys and how they reach the boards
- `tools/partitions-hub.csv`, `tools/partitions-single.csv`: where the keys live on the hub and on single-image nodes
- `tools/dev-board.sh`: registering, locking, flashing, reading
- `tools/flash-node.sh`, `tools/partitions-node.csv`, `crates/nodeimage/`: multi-image nodes
- `tools/p4-prepare.sh`, `firmware/patches/esp-hal-p4-pre-v3/`: the P4's patched esp-hal
- `firmware/hub-p4/.cargo/config.toml`: the P4's chip revision and cache settings
- `firmware/hub-p4/BRINGUP.md`, "Build and flash"

**Next:** [First power-up](kit-first-run.md)
