# Build, test and flash

**Level: 🟡 Intermediate** · How to run the tests, build each firmware, put it on a board safely,
watch what the board says, and run the bench tests.

All commands run from the repository folder unless a step says `cd` somewhere. Set your machine up
first ([Set up your machine](setup.md)).

## Run the tests

```bash
cargo test
```

This builds and tests every member of the root workspace: the 34 crates under `crates/` and the 13
Rust programs under `tools/`. It never builds firmware (each firmware folder is its own project).
As of commit `6651ecd` (2026-11-18) the source holds 1,467 `#[test]` markers in files under
`crates/` and `tools/` (counted with `git grep`, not by running the tests). Four are marked
`#[ignore]`: two need an input file, one only prints a simulation table, and one is a known
mismatch with published figures (their messages say which). What the tests do, broadly:

- compare our Remote ID decoder with the official C library, field for field
  (`crates/odid-ref`, `crates/rid/tests/`);
- push random and mutated bytes through every decoder: nothing may panic or hang
  (`crates/rid/tests/robustness.rs` runs 2,000,000 iterations per test by default; set
  `RID_FUZZ_ITERS` to change that);
- run detectors on synthetic attacks, floods, jamming and flights;
- simulate hardware on the host: an SD card at clock-edge level (`crates/sdnative`), the Sonoff
  dongle's bootloader (`tools/cc26xx-flash/tests/sim_flows.rs`), many nodes on one wire
  (`crates/wirebus`);
- draw every screen into PNG files (`crates/screen/tests/snapshots.rs`). This test **rewrites**
  `crates/screen/snapshots/*.png`: if you changed the screen, the new pictures are part of your
  change.

Some DSP tests push millions of samples, so the root `Cargo.toml` builds `jamwatch`, `fpvscan`,
`rawcap`, `wirebus` and `ctrlwatch` optimised even in test builds.

### Tests that need recordings

The DroneID tests in `crates/droneid/tests/recordings.rs`, `crates/rawcap/tests/pipeline.rs` and
`tools/rawcap-pull/tests/roundtrip.rs` decode two public DJI recordings. Without them they print
`SKIPPED: ... run tools/fetch-droneid-samples.sh` and pass. To run them for real:

```bash
tools/fetch-droneid-samples.sh
```

```bash
cargo test --release -p droneid -p rawcap -p rawcap-pull
```

`DRONEID_TESTDATA=/some/folder` points the tests at the recordings somewhere other than
`testdata/droneid/`.

> [!IMPORTANT]
> The recordings come from RUB-SysSec/DroneSecurity under AGPL-3.0. They are downloaded on demand,
> checked by SHA-256 and never committed (`testdata/` is ignored). They also carry a real
> aircraft's serial number: the tests check format and CRC only and never print it.

### Other test suites

- The bench judge: `python3 -m unittest discover -s tools/bench` (its sample logs are hand-written,
  not captured).
- FLARM with real keys: `FLARM_KEYS_FILE=tools/flarm.keys cargo test -p flarm`. Without the
  variable that test passes with a note.
- The documentation's links: `tools/docs-build.sh check` ([Writing these docs](docs-style.md)).

## Run a laptop tool

Every Rust tool is a workspace member, so `cargo run -p <name>` runs it. For example, the viewer's
demo, which needs no hardware:

```bash
cargo run --release -p rid-view -- --demo
```

Everything after `--` goes to the tool. Each tool's options are in the [tools reference](tools.md).

## Build the firmware

Each firmware folder has short build aliases in its `.cargo/config.toml`, mostly `cargo fw` for a
release build. Where one source tree serves two chips, you choose the chip by the folder you build
from (`s3/` or `c5/`), or by a feature on the command line. Every feature and build switch is in
the [firmware reference](firmware.md); the common builds are:

| Firmware | Build from | Command | Image |
|---|---|---|---|
| `node-wifi`, S3 | `firmware/node-wifi/s3` | `cargo fw` (after `. ~/export-esp.sh`) | `firmware/node-wifi/target/xtensa-esp32s3-none-elf/release/node-wifi` |
| `node-wifi`, C5 5 GHz | `firmware/node-wifi/c5` | `cargo fw` | `.../target/riscv32imac-unknown-none-elf/release/node-wifi` |
| `node-wifi`, C5 2.4 GHz | `firmware/node-wifi/c5` | `cargo fw24`, `cargo fw24-now` (ESP-NOW), `cargo fw24-bus` (wired bus) | under `target-band24/`, `target-espnow/`, `target-bus/` |
| `node-ble`, S3 | `firmware/node-ble` | `cargo build --release` (after `. ~/export-esp.sh`) | `.../target/xtensa-esp32s3-none-elf/release/node-ble` |
| `node-ble`, C5 | `firmware/node-ble` | `cargo +stable build --release --no-default-features --features esp32c5 --target riscv32imac-unknown-none-elf` | `.../target/riscv32imac-unknown-none-elf/release/node-ble` |
| `node-sdr` | `firmware/node-sdr/c5` | `cargo fw`, `cargo fw-lite` (no PSRAM decoder), `cargo fw-now`, `cargo fw-bus` | under `firmware/node-sdr/target*/` |
| `node-lr` | `firmware/node-lr` | `cargo fw`, `cargo fw-now`, `cargo fw-bus` | never flashed yet (no LR2021 hardware) |
| `gateway` | `firmware/gateway` | `cargo fw` | `.../target/riscv32imac-unknown-none-elf/release/gateway` |
| `hub-p4` | `firmware/hub-p4` | `cargo fw` (after `tools/p4-prepare.sh` once) | under `firmware/hub-p4/target/rev103/` |
| `hub` (S3, and web head) | `firmware/hub/s3` | `cargo fw`; `cargo fw --features webhead` for the web head | `.../target/xtensa-esp32s3-none-elf/release/hub` |
| `rid-beacon`, S3 | `firmware/rid-beacon` | `cargo build --release` (after `. ~/export-esp.sh`) | `.../target/xtensa-esp32s3-none-elf/release/rid-beacon` |
| `rid-beacon`, C5 | `firmware/rid-beacon` | `cargo +stable build --release --target riscv32imac-unknown-none-elf --no-default-features --features esp32c5` | `.../target/riscv32imac-unknown-none-elf/release/rid-beacon` |

> [!WARNING]
> As of 2026-11-18 the S3 hub firmware (`firmware/hub`, also the P4's web head) **does not link**:
> `firmware/hub/README.md` records its static RAM 18,088 bytes over budget. That note is about
> the S3 build only.

### The link key

No firmware build needs or reads a key. Since the cut-over of 2026-11-18 every ESP-NOW node has a
key of its own, kept in a small flash partition (`rskey`) that firmware updates never touch, and
the P4 hub holds the kit master and roster in its `rskit` partition; the gateway holds no key at
all. Images are the same for every kit. The keys are written to the boards with `link-key`, after
flashing or before, as described in
[ESP-NOW in depth, "Kits ship open; sealing yours"](../ESPNOW.md#kits-ship-open-sealing-yours) and
the [Tools reference](tools.md#link-key).

Two things to keep the keys in place when you flash: flash the hub with
`tools/partitions-hub.csv` and single-image ESP-NOW nodes with `tools/partitions-single.csv`
(`tools/dev-board.sh --pt <csv> <elf>`); multi-image nodes get `tools/partitions-node.csv` from
`tools/flash-node.sh`. `dev-board.sh` never erases a key partition.

Before 2026-11-18 one kit key was compiled into every ESP-NOW image from `tools/link.key`
(`ROTORSCOPE_LINK_KEY_FILE`, `link-key new`). That is gone: nothing reads the file or the variable
any more, and a board still on that firmware is refused by the hub as `old_firmware`. Never commit,
paste or print `tools/kit.key` (or an old `tools/link.key`).

### Bench builds and `ROTORSCOPE_BENCH=1`

Some features exist only for the bench: they fake drones or a GPS fix, loop a board's pins back,
or open debug commands. Every firmware's `build.rs` except the test transmitter's
(`rid-beacon`, itself a bench tool) refuses them unless the environment has
`ROTORSCOPE_BENCH=1`, and an image built with one says so at boot (the P4's screen is coded to show **BENCH
BUILD**, not yet checked by eye). For a bench session, set it in that shell only:

```bash
export ROTORSCOPE_BENCH=1
```

The list of bench features per firmware is in [Bench tests](../BENCH.md#bench-builds-2026-11-18-compile-checked-only).
Never leave a bench build on a unit you use for real.

## Flashing a board

Never run `espflash` (or `cargo run`, whose runner calls espflash) directly on the shared bench
boards. Use `tools/dev-board.sh`: it holds a per-board lock so two programs never flash one board
at once, finds the board by its USB by-id path, and checks the image before writing it.

### Name your boards once

Plug in **one** board, then:

```bash
tools/dev-board.sh --register
```

It finds Espressif boards under `/dev/serial/by-id/usb-Espressif_*`, asks each new one its chip
(this resets it), names it (`s3`, `s3b`, ... for ESP32-S3; `c5a`, `c5b`, ... for ESP32-C5) and
appends it to `tools/boards.local`, a per-machine file Git ignores. Put a label with the name on the
board, unplug it, and repeat for the next. `--register --no-probe` skips the chip question (the
board is then not registered: edit the file by hand). To see what is attached and who holds it:

```bash
tools/dev-board.sh --list
```

```text
NAME   CHIP     STATE     PORT
c5a    esp32c5  attached  /dev/serial/by-id/usb-Espressif_..._aa:bb:cc:dd:ee:01-if00
s3     esp32s3  BUSY      /dev/serial/by-id/usb-Espressif_..._aa:bb:cc:dd:ee:02-if00
         held by: pid=12345 since=10:42:07 cmd=node-wifi s3
```

`ABSENT` means registered but not plugged in. `tools/boards.local` is plain text, one board per
line (`name  by-id-path  chip   # mac`); you may rename boards in it, but keep the by-id paths.

### Flash one image and watch it

```bash
tools/dev-board.sh firmware/gateway/target/riscv32imac-unknown-none-elf/release/gateway 60 c5a
```

The arguments are the ELF, how many seconds to read the console afterwards (default 15) and the
board name (default `s3`). The script:

1. checks the image: a bench image is refused without `ROTORSCOPE_BENCH=1` (images carry no link
   key, so there is none to check);
2. waits up to 10 minutes for the board's lock (`~/.local/state/rotorscope/locks/`, or
   `ROTORSCOPE_LOCK_DIR`), and also takes the old `/tmp` lock so an older copy of the script still
   excludes it;
3. flashes with espflash, resets the board, and prints its console at 115200 baud for the
   seconds you asked.

Useful options (they go before the ELF): `--dry-run` shows what would happen and touches nothing;
`--send KEYS` types characters on the console after the reset (8 s later, or `--send-after SECS`);
`--log [seconds] [board]` resets and reads without flashing; `--hold SECONDS [board]` takes the
lock and sits on it; `--run CMD` runs a command with the lock held and `BOARD_PORT` set to the
board's port, for tools that talk to the board themselves. The [tools reference](tools.md#dev-boardsh)
lists them all.

### Flash a multi-image node

A listener node (XIAO ESP32-C5) holds several firmware images in one flash and switches between
them when the hub assigns it a job. `tools/flash-node.sh` builds and flashes them all:

```bash
tools/flash-node.sh c5a
```

It builds `node-wifi` into slot `ota_0` and `node-ble` into `ota_1` (with ESP-NOW), adds
`node-sdr` to `ota_2` only if its ELF already exists (it never builds node-sdr; point
`NODE_SDR_ELF` at another one), erases the boot choice so the node starts `node-wifi`, leaves the
node's key record (`rskey`) alone, and reads the console for 20 seconds. It calls the flash good
only if the node's boot line says it is keyed (`"auth":"kit:..."`), or with `--open` has an open
record. It refuses to go on if an image does not fit the node or is a bench build without
`ROTORSCOPE_BENCH=1`. `NODE_WIRED=1` builds wired images instead, and `NODE_BUS=1` builds
for the wired bus (where every image must be a bus build). The slot layout is
`tools/partitions-node.csv`.

### Flash the P4 hub

The P4 board (Elecrow CrowPanel Advanced 5") is not flashed through `dev-board.sh`: it uses
Elecrow's own second-stage bootloader and partition table, which are known to work on its v1.3
chip. `firmware/hub-p4/BRINGUP.md` gives the command and where the two files sit in flash
(bootloader 0x2000..0x8000, table 0x8000..0x8C00). They come from a full backup of the board's
factory flash; **they are not in the repository**, and that command overwrites the bootloader.
Do not write anything to a P4 board until you hold your own full backup of its flash. Use 460800 baud: 921600 fails on this board's
CH340 USB chip.

## Watching a console

`tools/dev-board.sh --log 60 c5a` resets the board and prints 60 seconds of what it says. Nodes
print one JSON object per line (`{"t":"hello",...}`, `{"t":"stats",...}`), so you can save the
output to a file and feed it to `rid-view` or `rawcap-pull` later.

Two behaviours to expect, both recorded in the repository:

- **An ESP32-C5 drops off USB for about a second** as its firmware starts. `dev-board.sh` waits up
  to 10 seconds and reopens the port; you will see `USB re-enumerated (1), reopened`.
- **Opening the P4's console resets the P4, every time**, even with DTR and RTS held low
  (`firmware/hub-p4/BRINGUP.md`). The first lines you see are always a fresh boot.

## Bench tests

`tools/bench/run.py` runs two-board tests: it builds the test transmitter and a listener, flashes
both through `dev-board.sh`, has the transmitter send one kind of signal with its fake identity,
and passes the test only if the listener decoded it the right way. The `hub` tests instead pair a
node with the S3 hub and count the node frames the hub receives.

```bash
tools/bench/run.py --list
```

```bash
tools/bench/run.py --group c5
```

Groups: `s3`, `c5`, `cross`, `wifi`, `ble`, `openlink`, `hub`; or name single tests, or `--all`.
`--dry-run` prints the plan and touches nothing; `--no-build` reuses built images; `--seconds`,
`--min-hits` and `--min-frames` change the capture length and pass marks; `--tx-board` and
`--rx-board` swap boards. Logs go to `bench-logs/<date-time>/<test>/`. The exit code is 0 only if
every test passed, 1 if any failed and 2 if any could not run. The runner sets
`ROTORSCOPE_BENCH=1` for its own builds and builds only wired images, which never use a link key.

The `hub` group builds the S3 hub, which does not link as of 2026-11-18 (see the warning above).
How to read results and what each group needs: [Bench tests](../BENCH.md).

## DroneID on real hardware, without a drone

Rotorscope never transmits DroneID, not even on the bench. To test the C5's DroneID receiver on the
chip itself, `tools/droneid-hil` turns each burst of the two public recordings into exactly the
bytes the C5's capture would hold and sends them over USB into the board's live decoding path.

1. Fetch the recordings once: `tools/fetch-droneid-samples.sh`.
2. Build a `node-sdr` bench image with the bring-up console (its `@F` and `hil` commands are
   refused otherwise):

   ```bash
   export ROTORSCOPE_BENCH=1
   ```

   ```bash
   (cd firmware/node-sdr/c5 && cargo fw --features bringup)
   ```

3. Flash it with `dev-board.sh` as above, then run the test with the board's lock held:

   ```bash
   tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT"' 600 c5b
   ```

The tool prints counts and record types only, never identities. Results and the hostile-signal
scenarios (`--train`, `--forge`) are in [Bench tests](../BENCH.md#node-sdr-under-hostile-signals-2026-11-18-compile-checked-only).
Status: DJI DroneID is **proven on recordings**, on the laptop and through the chip's own capture
path; it has not been heard from a real drone.

**Next:** [Crate reference](crates.md)

## Where this lives in the code

- `Cargo.toml`: workspace members and the optimised test profiles
- `firmware/*/.cargo/config.toml`: the `cargo fw` aliases
- `tools/link-key/src/main.rs`, `tools/partitions-*.csv`: the link keys and where they live on a board
- `crates/link/src/benchfence.rs`: the bench fence every firmware build uses
- `tools/dev-board.sh`, `tools/flash-node.sh`, `tools/partitions-node.csv`: flashing
- `tools/bench/run.py`, `tools/bench/judge.py`: the bench runner
- `tools/droneid-hil/src/main.rs`, `tools/fetch-droneid-samples.sh`: DroneID on the chip
