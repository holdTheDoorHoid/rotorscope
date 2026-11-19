# Set up your machine

**Level: 🟡 Intermediate** · What to install to build, test and flash Rotorscope, chip by chip,
and which parts you can skip.

You do not need all of this. To run the host tests and the laptop tools, Rust and a C compiler
are enough. The firmware toolchains, the flasher and the serial-port setup only matter once you
have boards.

## Which operating system

Rotorscope is developed on **Linux**, and every script in `tools/` is written for it: boards are
found under Linux's `/dev/serial/by-id/`, the board lock uses `flock`, and the laptop Bluetooth
tools talk to BlueZ (`rid-ble-beacon` stops with "needs Linux and BlueZ" anywhere else).

macOS and Windows are **untried**, as of 2026-11-18. The host crates and most Rust tools have
nothing Linux-specific in them and may well build there; the flashing and bench scripts will not
work as written. If you try, please report what happened (see [Contributing](contributing.md)).

## What each job needs

| You want to... | You need |
|---|---|
| Run `cargo test`, the demo, the laptop tools | Rust (stable), a C compiler |
| Build firmware for the ESP32-C5, ESP32-C3 or ESP32-P4 | The above, plus the RISC-V targets |
| Build firmware for the ESP32-S3 | The above, plus Espressif's Xtensa toolchain from `espup` |
| Build the P4 hub | The above, plus `git`, `python3`, and the patched esp-hal from `tools/p4-prepare.sh` |
| Flash and watch boards | `espflash`, `python3` with the `serial` module (pySerial), `flock`, serial-port access |
| Run the two-board bench tests | All of the flashing list |
| Build these documents | mdBook 0.5.4, `curl`, `sha256sum`, `python3` |

## Rust

Install Rust with [rustup](https://rustup.rs). The workspace uses Rust edition 2024
(`Cargo.toml`), so use a current stable toolchain.

A **C compiler** is also needed for host builds: `crates/odid-ref` compiles the C reference
library in `third_party/opendroneid-core-c` (through the `cc` crate) so the tests can compare our
Remote ID decoder against it. Your distribution's standard build tools package provides one.

Then, from the repository folder, check that the host side builds and passes:

```bash
cargo test
```

## Firmware toolchains, by chip

Each firmware folder pins its own toolchain in a `rust-toolchain.toml`, and rustup follows it
automatically whenever you run `cargo` inside that folder. What they ask for:

| Chip | Firmware | Toolchain (`rust-toolchain.toml`) | Target |
|---|---|---|---|
| ESP32-C5 (RISC-V) | `gateway`, `node-wifi/c5`, `node-sdr/c5`, `hub/c5` | `stable`, with `rust-src` | `riscv32imac-unknown-none-elf` |
| ESP32-C5, from a shared folder | `node-ble`, `rid-beacon` | the folder says `esp`; the C5 build overrides it with `cargo +stable` | `riscv32imac-unknown-none-elf` |
| ESP32-C3 (RISC-V, no atomics) | `node-lr` | `stable` | `riscv32imc-unknown-none-elf` |
| ESP32-P4 (RISC-V with floats) | `hub-p4` | `stable` | `riscv32imafc-unknown-none-elf` |
| ESP32-S3 (Xtensa) | `node-wifi/s3`, `node-ble`, `rid-beacon`, `hub/s3` | `esp` (Espressif's fork) | `xtensa-esp32s3-none-elf` |

The RISC-V chips build on **stock stable Rust**. If a stable build complains that the target is
missing, add it with rustup (`rustup target add` and the target name from the table).

### The ESP32-S3 needs Espressif's toolchain

The S3 is an Xtensa chip, which stock Rust does not support. Install
[espup](https://github.com/esp-rs/espup), then:

```bash
espup install
```

Before every S3 build, in the same shell, load its environment:

```bash
. ~/export-esp.sh
```

If you forget, the S3 firmware's `build.rs` stops with a message telling you to run
`. ~/export-esp.sh` (and `espup install` if that file does not exist yet).

### The ESP32-P4 needs a patched esp-hal

The P4 board Rotorscope uses carries chip revision v1.3, and upstream esp-hal supports only v3.0
and newer. `tools/p4-prepare.sh` checks the community compatibility patch and the project's own patches
against their recorded SHA-256 sums, clones one pinned esp-hal commit, and applies them. Run it once, from the repository folder:

```bash
tools/p4-prepare.sh
```

It writes `firmware/patches/esp-hal-p4-pre-v3/upstream/` (ignored by Git). It needs `git`,
`python3` and `sha256sum`, and network access to GitHub. Running it again on a prepared tree only
checks it; `tools/p4-prepare.sh --verify` does the same without changing anything. More in [Hard-won lessons](gotchas.md#esp32-p4).

## Flashing tools

**espflash** writes firmware to the boards. Install it from
[esp-rs/espflash](https://github.com/esp-rs/espflash). The repository does not pin a version;
its scripts use the `flash`, `erase-parts` and `board-info` commands with `--non-interactive`,
`--partition-table` and `--target-app-partition`, so use a release that has those.

You will rarely call espflash yourself. The project's flashing script, `tools/dev-board.sh`,
wraps it with a lock so two programs never flash one board at once. That script also needs:

- `python3` with the **pySerial** module (`import serial`): it reads the board's console with it;
- `flock`, from util-linux, for the board lock;
- `timeout` and `stty` (coreutils), which `--run` and `tools/droneid-hil` use.

## Serial-port permissions

Boards appear as serial devices. If opening one fails with "Permission denied", your user is not
allowed to use serial ports yet. On most Linux distributions that means joining the group that
owns them (commonly `dialout` on Debian and Ubuntu, `uucp` on Arch), then logging out and back
in. The repository's own notes do not cover this step, because it depends on your distribution.

Always address a board by its `/dev/serial/by-id/...` path, never `/dev/ttyACM0` or
`/dev/ttyUSB0`: those numbers change when you plug things in, and the by-id path names one exact
board. `tools/dev-board.sh --register` records the by-id paths for you
([Build, test and flash](build-test-flash.md#flashing-a-board)).

## Laptop radio tools: what they need from the system

The host tools were chosen to avoid native libraries where possible. From their `Cargo.toml`
files:

| Tool | Native needs |
|---|---|
| `rid-view --ble`, `rid-ble-beacon` | A running **BlueZ** (Linux Bluetooth service). They talk to it over D-Bus with `zbus`, which is pure Rust, so no libdbus. No root needed. |
| `rid-view --dragonsync` | Nothing: the `zeromq` crate is pure Rust, no libzmq. |
| `cc26xx-flash`, `evidence-upload` | Nothing: `serialport` is built without its `libudev` feature. |
| `airspace-pack`, `evidence-server` | Nothing: HTTPS through rustls, no OpenSSL. (The `ring` crypto library they pull in compiles some C, so the C compiler above covers it.) |

## Python

Python 3 is used only by scripts, never by the firmware or the Rust tools:

- `tools/dev-board.sh` (inline, with pySerial) and `tools/p4-prepare.sh`;
- the bench runner `tools/bench/run.py` and its judge, standard library only;
- the documentation link checker `tools/docs-check.py`, standard library only.

## Test recordings (optional)

Some DroneID tests use two public recordings. They are not in the repository; fetch them when you
want those tests to run (they skip with a message otherwise). This needs `curl` and `sha256sum`:

```bash
tools/fetch-droneid-samples.sh
```

They land in `testdata/droneid/`, which Git ignores. The recordings are AGPL-3.0 and must never
be committed.

## The documentation site

To build these pages you need mdBook, the exact version the site is built with:

```bash
cargo install mdbook --version 0.5.4 --locked
```

Then follow [Writing these docs](docs-style.md). The first build downloads the diagram library
(checked against a fixed checksum), so it needs `curl` and `sha256sum`.

## Build cache and disk space

The repository configures no shared compiler cache (there is no `sccache` setting), and states no
disk-space figure. Expect each firmware folder to build into its own `target` directory, and some
into extra ones (`target-band24`, `target-espnow`, `target-bus`), so several firmware builds take
several times the space of one.

**Next:** [Build, test and flash](build-test-flash.md)

## Where this lives in the code

- `firmware/*/rust-toolchain.toml` and `firmware/*/.cargo/config.toml` (also under `c5/` and `s3/`): toolchains, targets and build aliases
- `firmware/hub/build.rs`, `firmware/node-wifi/build.rs`, `firmware/node-ble/build.rs`, `firmware/rid-beacon/build.rs`: the `export-esp.sh` check
- `tools/p4-prepare.sh`, `firmware/patches/esp-hal-p4-pre-v3/manifest.json`: the P4 esp-hal
- `crates/odid-ref/build.rs`: why a C compiler is needed
- `tools/*/Cargo.toml`: the host tools' dependencies
