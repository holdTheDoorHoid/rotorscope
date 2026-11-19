# Tools reference

**Level: 🔴 Expert** · Every program and script in `tools/`: what it is for, every option its
argument parser accepts, what it reads and writes, and what can go wrong.

Rust tools are workspace members and run with `cargo run --release -p <name> -- <options>`
(everything after `--` goes to the tool). `evidence-upload` is a second program inside the
`evidence-server` package: `cargo run --release -p evidence-server --bin evidence-upload -- ...`,
or `target/release/evidence-upload` once built. Scripts run from the repository folder. Options are
as of commit `6651ecd` (2026-11-18); each section names the source file that is the final word.

| Entry | Kind | One line |
|---|---|---|
| [`airspace-pack`](#airspace-pack) | Rust | Builds the offline airspace map pack for the hub's SD card |
| [`bench/`](#bench) | Python | Two-board bench tests, one command each |
| [`cc26xx-flash`](#cc26xx-flash) | Rust | Backs up and flashes a Sonoff CC2652P dongle |
| [`check-hidden-chars.py`](#check-hidden-charspy) | Python | Fails on Unicode that can hide or reorder source text, in every tracked file |
| [`check-image-identity.sh`](#check-image-identitysh) | Shell | Shows that firmware images hold no key and no builder details: two independent checkouts in different folders, one with decoy keys, must build identical images |
| [`check-wifi-blobs.sh`](#check-wifi-blobssh) | Shell | Checks the Espressif radio libraries carry the CVE-2025-52471 fix, and the esp-radio patch |
| [`ci-lockfiles.sh`](#ci-lockfilessh) | Shell | The lockfile checks CI runs: every lockfile listed, current, and passed by cargo-deny |
| [`dev-board.sh`](#dev-boardsh) | Shell | Flashes and reads boards under a per-board lock |
| [`docs-build.sh`, `docs-check.py`](#docs-buildsh-and-docs-checkpy) | Shell, Python | Builds and checks this documentation site |
| [`droneid-file`](#droneid-file) | Rust | Decodes DJI DroneID from an I/Q recording |
| [`droneid-hil`](#droneid-hil) | Rust | Feeds the public DroneID recordings into a real C5 |
| [`evidence-server`](#evidence-server-and-evidence-upload) | Rust | Stores and exports hubs' signed evidence logs; `evidence-upload` feeds it |
| [`evidence-verify`](#evidence-verify) | Rust | Checks an evidence log or export bundle |
| [`fetch-droneid-samples.sh`](#fetch-droneid-samplessh) | Shell | Downloads the two public DroneID recordings |
| [`flarm-keys`](#flarm-keys) | Rust | Makes the private FLARM key file from a SoftRF checkout |
| [`flash-node.sh`](#flash-nodesh) | Shell | Builds and flashes a multi-image listener node |
| [`link-key`](#link-key) | Rust | Makes the kit master and writes each board's ESP-NOW key record |
| [`modes-live`](#modes-live) | Rust | Checks our ADS-B decoder against an ADSBee |
| [`p4-prepare.sh`](#p4-preparesh) | Shell | Prepares the patched esp-hal the P4 hub needs |
| [`partitions-node.csv`](#partitions-nodecsv) | Data | Flash layout of a multi-image node (and the hub and single-image tables) |
| [`rawcap-pull`](#rawcap-pull) | Rust | Rebuilds node-sdr captures from a console log |
| [`release-build.sh`, `release-parts.py`](#release-buildsh-and-release-partspy) | Shell, Python | Builds a release: the shipped images of one commit, reproducibly, with checksums and parts lists |
| [`repro-env.sh`](#repro-envsh) | Shell | Makes a firmware image independent of checkout path and build time |
| [`rfuav-check`](#rfuav-check) | Rust | Runs the C5's FPV and DroneID detectors over RFUAV recordings |
| [`rid-ble-beacon`](#rid-ble-beacon) | Rust | Bench-only Bluetooth Remote ID test transmitter (laptop) |
| [`rid-view`](#rid-view) | Rust | Laptop viewer: the hub's tracker on a laptop |

---

## airspace-pack

**Purpose:** download public US-government airspace, terrain and map data around a point and write
the single pack file the P4 hub reads for its FAA rule flags ([FAA rule flags](../use/faa-flags.md)).

```text
airspace-pack --lat LAT --lon LON [--radius-km 100] [--terrain-arcsec 15]
              [--out AIRSPACE.PAK] [--cache DIR] [--offline]
```

| Option | Meaning |
|---|---|
| `--lat`, `--lon` | Centre of the region. Required. |
| `--radius-km` | Radius, 1 to 400 km (default 100). |
| `--terrain-arcsec` | Terrain grid spacing in arc-seconds (default 15). |
| `--out` | Output file (default `AIRSPACE.PAK` in the current folder). |
| `--cache` | Download cache (default `tools/airspace-pack/cache`, ignored by Git). TFRs are cached per day. |
| `--offline` | Use only the cache; fail if something is missing. |
| `-h`, `--help` | Usage line. |

```bash
cargo run --release -p airspace-pack -- --lat 39.9526 --lon -75.1652 --radius-km 100
```

**Reads:** the FAA ADDS open-data layers (class airspace, UAS facility maps, special-use and
prohibited areas, national-security restrictions, stadiums, national defense airspace, FRIAs,
airports), tfr.faa.gov, the USGS 3DEP elevation service, the NOAA NGS GEOID18 API and US Census
TIGERweb urban areas, all public domain, with no key or login. tfr.faa.gov refuses this tool, so
TFRs are skipped unless you save that day's TFR files into the cache by hand in a browser (the tool
prints which files and names). **Writes:** the pack, and the cache.
Copy the pack to the hub's SD card as `ROTORSCP/AIRSPACE.PAK`. `PLAN.md` gives 662 KB
for a 100 km pack around Philadelphia.

Source: `tools/airspace-pack/src/main.rs`.

## bench

**Purpose:** two-board bench tests: build the test transmitter and a listener, flash both through
`dev-board.sh`, make the transmitter send one kind of signal, and judge what the listener decoded.
The `hub` tests instead flash a node and the S3 hub and count the node frames the hub receives.

```text
tools/bench/run.py [TEST...] [--all | --group G] [--list] [--dry-run] [--no-build]
                   [--seconds N] [--min-hits N] [--min-frames N]
                   [--tx-board NAME] [--rx-board NAME] [--logs DIR]
```

| Option | Meaning |
|---|---|
| `TEST...` | Test IDs, from `--list` |
| `--all` | Every test |
| `--group` | `s3`, `c5`, `cross`, `hub`, `ble`, `wifi` or `openlink` |
| `--list` | Print every test with its boards and a description |
| `--seconds`, `-n` | Listener capture length (default 30) |
| `--min-hits`, `-m` | Radio tests: decoded detections needed to pass (default 3) |
| `--min-frames` | Hub tests: node frames needed (default 10) |
| `--dry-run` | Print the plan; build, flash and lock nothing |
| `--no-build` | Reuse already-built images |
| `--tx-board`, `--rx-board` | Use other registered boards |
| `--logs` | Log folder (default `bench-logs/`) |

```bash
tools/bench/run.py --group c5
```

```bash
tools/bench/run.py s3-to-s3-wifi-beacon --dry-run
```

**Reads:** `tools/boards.local` through `dev-board.sh --resolve`. **Writes:** firmware builds,
and `bench-logs/<date-time>/<test>/` (`tx.log`, `rx.log` or `node.log` and `hub.log`,
`result.json`) plus `summary.txt`. Exit code 0 if all passed, 1 if any failed, 2 if any could not
run. It sets `ROTORSCOPE_BENCH=1` for its own runs.

**Safety:** it never opens a serial port or calls espflash itself; every board access goes through
`dev-board.sh`. The only transmissions are the existing `rid-beacon` firmware with its default
conduct settings. The judge (`judge.py`) is pure functions over captured text, tested by
`test_judge.py` against hand-written sample logs in `samples/`. The `hub` group builds the S3 hub
firmware, which does not link as of 2026-11-18 ([Build, test and flash](build-test-flash.md#build-the-firmware)).

Source: `tools/bench/run.py`, `tools/bench/judge.py`. Procedures and results:
[Bench tests](../BENCH.md).

## cc26xx-flash

**Purpose:** back up and flash a Sonoff Zigbee 3.0 USB Dongle Plus (TI CC2652P) through the chip's
built-in serial bootloader, so it can run NCC Group's Sniffle firmware as a Bluetooth 5 receiver.

```text
cc26xx-flash --port PATH [OPTIONS] info
cc26xx-flash --port PATH [OPTIONS] read OUT.bin [--len BYTES]
cc26xx-flash --port PATH [OPTIONS] flash IMAGE.hex|IMAGE.bin [--yes]
```

| Command | What it does |
|---|---|
| `info` | Enters the bootloader, prints the chip ID, flash size and the bootloader settings in the chip's configuration block (CCFG), then resets. Changes nothing. |
| `read OUT.bin` | Backs up flash (all of it by default), checked against the chip's own CRC32 of the same range. Refuses to overwrite an existing file. |
| `flash IMAGE` | Checks the image's CCFG keeps the bootloader reachable, erases, writes, verifies by CRC32, resets. Makes up to three attempts in all. |

| Option | Meaning |
|---|---|
| `--port PATH` | The dongle's serial port. Use its `/dev/serial/by-id/` path; any other path draws a warning. |
| `--baud N` | Bootloader baud rate (default 115200; the bootloader autobauds). |
| `--len BYTES` | For `read`: how much, e.g. `0x58000`. A multiple of 4, at most the flash size. |
| `--dry-run` | Do not open the port: check the image (`flash`) and say what would be done. |
| `--yes` | For `flash`: do not ask. Without it the tool asks you to type `yes`, and refuses if it is not run from a terminal. |
| `-h`, `--help` | Usage. |

```bash
cargo run --release -p cc26xx-flash -- --port "$PORT" read sonoff-zigbee-backup.bin
```

```bash
cargo run --release -p cc26xx-flash -- --port "$PORT" flash sniffle_cc1352p1_cc2652p1.hex --dry-run
```

**Reads:** an Intel HEX or raw binary image. **Writes:** the backup file (through a `.partial` file
renamed on success), and the dongle's flash.

> [!WARNING]
> A wrong image could make a dongle unrecoverable over USB; this tool is built to refuse one. What
> [SONOFF.md](../SONOFF.md) and the code say:
>
> - It **refuses any image whose CCFG would switch off the chip's built-in loader or the button pin
>   it uses** (DIO15, active low), disable bank or chip erase, or write-protect any sector. That is
>   the one way to make this dongle unrecoverable over USB, so the tool will not do it.
> - It refuses a chip whose flash is not the CC2652P's 352 KB (0x58000).
> - It is for the **ZBDongle-P (CC2652P, CP2102N bridge)** only. The ZBDongle-E is a different chip
>   and will not work. Use the Sniffle file **without** `_1M` in its name.
> - **Make the backup first** (`read`). It is your only way back to Zigbee, and it includes the
>   Zigbee network settings stored on the dongle.
> - If flashing fails part-way, run the same command again: an erased or half-written chip always
>   starts in its built-in loader. The dongle also has a BOOT button inside its case, wired to the
>   same pin.

Status: `info`, `read` and `flash` of Sniffle were run on one real dongle (2026-11-11); restoring
the Zigbee backup is **simulated only** (`tests/sim_flows.rs`). Source: `tools/cc26xx-flash/src/main.rs`,
`src/ccfg.rs`. Guide: [A Sonoff dongle as a Bluetooth 5 receiver](../SONOFF.md).

## check-hidden-chars.py

**Purpose:** fail on characters that can make text look different from what a compiler or reviewer
reads ("Trojan Source"): bidirectional controls (U+202A to U+202E, U+2066 to U+2069), zero-width
characters (U+200B to U+200D, U+2060, and U+FEFF anywhere but the first character of a file), and
every other control or format character, and U+2028 and U+2029, except tab, line feed and carriage
return.

```text
python3 -I tools/check-hidden-chars.py              every file Git tracks
python3 -I tools/check-hidden-chars.py FILE...      just these files
python3 -I tools/check-hidden-chars.py --self-test  checks of this script on temporary files
```

Each finding is printed as `path:line:column: U+XXXX NAME (rule)`, never the character itself.
Files with a NUL byte are skipped as binary; files that are not UTF-8 are skipped with a note.
Exceptions are lines of `path U+XXXX reason` in `tools/hidden-chars-allow.txt`
([Adding a hidden-character exception](build-test-flash.md#adding-a-hidden-character-exception)).

**Reads:** the files and the allow-list. Exit 0 when clean, 1 on a finding, 2 when the allow-list
has a malformed line. Standard library only.

Source: `tools/check-hidden-chars.py`.

## check-image-identity.sh

**Purpose:** show that firmware images hold no link key and nothing about the computer that built
them, and are the same for every kit and every builder: it builds the same commit from two
independent throwaway checkouts in **different folders**, one of them with decoy key files and the
retired key variable set, and compares the images.

```text
tools/check-image-identity.sh [--plain] [--paths] [node-wifi|gateway]...   default: node-wifi gateway
```

It compares the app image espflash would write (`espflash save-image`) and the ELF, byte for byte;
it fails if an image carries a `link-key:` tag, or if the image or ELF holds the home folder path,
user name, computer name or a build folder. Both builds go through `tools/repro-env.sh` and its fixed
build path (`/tmp/rotorscope-repro`, a link to each checkout in turn), which removes the build time,
host paths and the checkout's location from the image. `--plain` builds without it, to show what
differs then; `--paths` also builds the second checkout where it is, without the fixed path (on
2026-11-19: 407 bytes of the gateway's image and 4,666 of node-wifi's differ that way). Result on
2026-11-19: the node-wifi and gateway images and ELFs identical, nothing of the builder found. It
builds the committed tree (`git archive HEAD`), so commit first; it needs about 2.5 GB free while it
runs, removes everything afterwards and touches no board. Exit 0 when every image matches.

Source: `tools/check-image-identity.sh`. Background: [Releases](release.md#how-the-bytes-are-made-the-same-everywhere)
and docs/LINK-KEYS-DESIGN.md section 9.

## check-wifi-blobs.sh

**Purpose:** check, offline, that every radio firmware links Espressif radio libraries with the fix
for CVE-2025-52471, and the patched esp-radio.

```text
tools/check-wifi-blobs.sh [lockfile...]   default: every firmware/*/Cargo.lock
tools/check-wifi-blobs.sh --self-test     checks of this script on fake data (no registry)
```

1. For every `esp-wifi-sys-<chip>` crate a lockfile names, for the chips the firmware is built for
   (ESP32-C3, ESP32-C5, ESP32-S3), the git blob hashes of `libespnow.a`, `libnet80211.a` and
   `libpp.a` in the local cargo registry must equal the list in the script: the files of the
   Espressif Wi-Fi library commit that contains the fix. A dependency bump to a version with no
   entry fails until someone checks its provenance and records it.
2. Every lockfile that uses esp-radio must take the patched copy (`firmware/patches/esp-radio`),
   and that copy must differ from the crates.io release only in the two files
   `firmware/patches/README.md` names (the release is read from the registry cache after its
   SHA-256 is checked).

**Reads:** the lockfiles, `$CARGO_HOME` (default `~/.cargo`) and the repository; nothing from the
network. Exit 0 when all is good, 1 with what failed and why.

Source: `tools/check-wifi-blobs.sh`. Background: `firmware/patches/README.md`,
docs/LINK-KEYS-DESIGN.md section 8.

## ci-lockfiles.sh

**Purpose:** the lockfile checks CI runs, runnable on your machine
([What CI checks](build-test-flash.md#what-ci-checks)).

```text
tools/ci-lockfiles.sh list      every tracked Cargo.lock is in the script's list
tools/ci-lockfiles.sh resolve   each lockfile is current (cargo metadata --locked)
tools/ci-lockfiles.sh deny      cargo-deny with deny.toml on each lockfile
```

`resolve` and `deny` need `tools/p4-prepare.sh` to have run once (for `firmware/hub-p4`), and use
stock stable Rust (`RUSTUP_TOOLCHAIN=stable` unless you set it). `deny` needs cargo-deny 0.20.2.

**Reads:** the lockfiles and manifests; the first run downloads crates, and `deny` the RustSec
advisory database. Exit 0 when all pass, 1 naming each lockfile that failed, 2 on a usage error.

Source: `tools/ci-lockfiles.sh`, `deny.toml`.

## dev-board.sh

**Purpose:** flash an ELF to a bench board and capture its console, holding a per-board lock so
parallel programs (and the two halves of a bench test) take turns.

```text
tools/dev-board.sh [OPTIONS] <elf> [seconds=15] [board=s3]   flash, reset, read
tools/dev-board.sh [OPTIONS] --log [seconds=15] [board=s3]   reset and read, no flash
tools/dev-board.sh --register [--no-probe]                   find boards, name them, write tools/boards.local
tools/dev-board.sh --list                                    registered boards: attached? busy?
tools/dev-board.sh --resolve <board>                         print "<port> <chip>"
tools/dev-board.sh --hold <seconds> [board]                  take the lock and sit on it
tools/dev-board.sh --pt <csv> --app <label>=<elf> [--app ...] [--erase LABEL] --multi [seconds] [board]
tools/dev-board.sh --pt <csv> <elf> [seconds] [board]           single image with that partition table
tools/dev-board.sh --self-test                               dry-run checks of this script (no board)
```

| Option (before the positional arguments) | Meaning |
|---|---|
| `--send KEYS` | After the reset, type these characters on the console, one at a time (e.g. `rid-beacon`'s one-key commands) |
| `--send-after SECS` | When to start typing (default 8 s after the reset) |
| `--run CMD` | Run `CMD` (with `bash -c`) under the lock, with `BOARD_PORT` set, for at most `seconds`. No reset. Give an ELF too to flash first. |
| `--dry-run` | Print what would happen; touch no hardware, take no lock |
| `--pt`, `--app`, `--erase`, `--multi` | Multi-image flash: write the partition table, erase `otadata` and `rsjob` (plus any `--erase` label), write each ELF into its slot. `--pt` with one ELF flashes a single image with that table (single-image ESP-NOW nodes: `tools/partitions-single.csv`) |

```bash
tools/dev-board.sh --list
```

```bash
tools/dev-board.sh --send xw firmware/rid-beacon/target/xtensa-esp32s3-none-elf/release/rid-beacon 40 s3
```

**Reads:** `tools/boards.local` (or `$ROTORSCOPE_BOARDS`), each ELF's `bench-build:` tag. It looks
for no key: images carry none. **Writes:** the board; lock files in
`$ROTORSCOPE_LOCK_DIR` (default `~/.local/state/rotorscope/locks/`), plus the legacy
`/tmp/rotorscope-board-<name>.lock`; `--register` appends to `tools/boards.local`.

How it behaves:

- Boards are addressed by USB by-id path (it contains the MAC), never `/dev/ttyACMn`, never a
  serial number. Two names for one board share one lock. `--register` only finds boards under
  `/dev/serial/by-id/usb-Espressif_*`, and resets each new one to read its chip type.
- No board is built in. A name that is not in `tools/boards.local` (including the default `s3`) is
  refused with the advice to run `--register`.
- The lock lives outside `/tmp` because a sandboxed program can have a private `/tmp`, giving each
  program its own "shared" lock. Waiting is capped at 10 minutes. A held lock records the PID,
  time and command, which `--list` shows.
- It **refuses** a bench image unless `ROTORSCOPE_BENCH=1` (and announces it when it is set).
- It never erases the provisioning partitions `rskey` (a node's key record), `rskit` and `rsboot`
  (the hub's master, roster and boot counter): `--erase` refuses them, and an app flash with the
  same table leaves them where they were. Only `link-key` writes them, through `--run`.
- The console is read at 115200 baud after a USB-JTAG reset. It rides out an ESP32-C5 dropping off
  USB as its firmware starts, and keeps reading while it types, because Linux holds only 4 KB of
  unread serial input.

Source: `tools/dev-board.sh`. Background: [Bench tests](../BENCH.md#how-the-lock-works-for-the-curious).

## docs-build.sh and docs-check.py

**Purpose:** build, preview and check this documentation site.

```text
tools/docs-build.sh [build|serve|check]
```

`build` (default) writes `target/book`; `serve` runs a live preview (extra arguments go to
`mdbook serve`); `check` builds and then runs `tools/docs-check.py`, which reports broken relative
links and anchors, relative links that leave `docs/`, GitHub links to files not in the repository,
and pages missing from `docs/SUMMARY.md` (exit 1 if any). The first run downloads Mermaid 11.4.1
into `docs/assets/` and checks its SHA-256; every run copies `crates/screen/snapshots/*.png` into
`docs/images/screen/`. Needs mdBook 0.5.4. See [Writing these docs](docs-style.md).

## droneid-file

**Purpose:** decode DJI DroneID bursts from a raw I/Q recording and print one JSON line per burst.

```text
droneid-file <file> --rate <samples/s> [--format cf32|ci8|ci16] [--redact] [--no-turbo] [--failures]
```

| Option | Meaning |
|---|---|
| `--rate`, `-r` | Sample rate. Required. |
| `--format`, `-f` | `cf32` (float32 I/Q, default), `ci8` or `ci16` |
| `--redact` | Mask serial number and UUID (keep the first three characters). Use it whenever you share output: they identify a real aircraft and its owner. |
| `--no-turbo` | Do not use the turbo decoder |
| `--failures` | Also print a line, with the error, for each span that failed to decode |
| `-h`, `--help` | Usage |

```bash
cargo run --release -p droneid-file -- caps/c5-1.cs16 --rate 40000000 --format ci16 --redact --failures
```

Each line gives position, frequency offset, SNR, which decode path worked (`systematic` or
`turbo`), CRC status and the decoded fields; O4 records are reported as encrypted, not decrypted.
**Reads:** the recording. **Writes:** stdout only.

Source: `tools/droneid-file/src/main.rs`.

## droneid-hil

**Purpose:** hardware-in-the-loop test of `firmware/node-sdr`: turn each burst of the public DroneID
recordings into exactly the bytes the C5's capture would hold, send them over USB into the board,
and run them through the chip's live decoding path. Nothing is transmitted.

```text
droneid-hil [--port PORT] [--dir DIR] [--rms A,B,...] [--noise N] [--pre CMD]...
            [--train N,GAP_MS [--pulse burst|noise]] [--forge SECS[,start|flood|late]]
```

| Option | Meaning |
|---|---|
| `--port` | The board's port (default `$BOARD_PORT`, which `dev-board.sh --run` sets) |
| `--dir` | Recordings folder (default `testdata/droneid`) |
| `--rms` | Burst levels in ADC codes RMS (default `200,100,60`) |
| `--noise` | Receiver noise in codes (default 48, the board's measured noise at gain 83) |
| `--pre` | A console command to send first, e.g. `"trig 3"`; repeatable |
| `--train N,GAP_MS` | Hostile-signal scenario: replay one pulse N times, GAP_MS apart, with and without the decode budget |
| `--pulse` | For `--train`: `burst` (the first recorded burst, default) or `noise` (600 µs of white noise; needs no recording) |
| `--forge SECS[,CASE]` | Forged-session scenario on the live follower: `start` (default), `flood` or `late` |

`HIL_DEBUG` in the environment prints extra detail.

```bash
tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT"' 600 c5b
```

**Needs:** a node-sdr bench build with the `bringup` feature (built with `ROTORSCOPE_BENCH=1`; the
board refuses `@F`, `hil run`, `hop` and `trig` otherwise), and the recordings from
`fetch-droneid-samples.sh` (except `--pulse noise`). **Prints:** counts and record types only,
never identities; the board redacts its own lines and never sends HIL sightings to the hub.
Expected results: [Bench tests](../BENCH.md#node-sdr-under-hostile-signals-2026-11-18-compile-checked-only).

Source: `tools/droneid-hil/src/main.rs`.

## evidence-server and evidence-upload

**Purpose:** `evidence-server` stores the signed evidence logs hubs upload, keeps an audit log of
who viewed what, and exports bundles; `evidence-upload` is the laptop that carries a P4 hub's
records to it (the P4 has no network of its own). Plain-language guide: [The evidence log](../EVIDENCE.md).

```text
evidence-server serve --data DIR [--listen 127.0.0.1:8650] [--tls-cert PEM --tls-key PEM]
                [--token T] [--tsa URL] [--name NAME] [--open-registration]
evidence-server keygen --out FILE
```

| `serve` option | Meaning |
|---|---|
| `--data` | Data folder (default `evidence-data`, ignored by Git) |
| `--listen` | Address and port (default `127.0.0.1:8650`) |
| `--tls-cert`, `--tls-key` | Serve HTTPS; the two go together |
| `--token` | Bearer token (or `$EVIDENCE_TOKEN`). Every call except an upload needs it. |
| `--open-registration` | Allow registering units without a token: only on a bench laptop nobody else can reach |
| `--tsa` | An RFC 3161 time-stamping service. With it, `POST /api/devices/{id}/timestamp` has the newest checkpoint time-stamped (only a hash leaves the server). Off unless given. |
| `--name` | The name the server gives itself (default its URL) |

`keygen --out FILE` writes a **development** signing key (owner-only, refuses to overwrite) for a
P4 build (`ROTORSCOPE_DEV_KEY=$(cat FILE) cargo fw` in `firmware/hub-p4`). It is a software key,
not evidence-grade. The HTTP routes are under `/api/devices` (`tools/evidence-server/src/lib.rs`).

```text
evidence-upload --server URL --serial PORT [--replay] [--seconds N] [--save FILE] [--send TEXT]
evidence-upload --server URL --capture console.log
evidence-upload --server URL --card /media/<card>/ROTORSCP/EVIDENCE
common: [--register NAME] [--token T] [--actor NAME] [--allow-gap] [--ca cert.pem]
```

`--serial` reads the hub's USB console live (`--replay` first asks the hub to re-send its current
card file; `--send` types console commands; `--save` keeps the capture); `--capture` reads a saved
console log; `--card` reads the card's evidence files in a card reader. It uploads each boot's
records in batches that end on a signed checkpoint; the server checks the hub's signatures, so the
laptop vouches for nothing.

```bash
target/release/evidence-server serve --data ~/evidence-data
```

> [!WARNING]
> Plain HTTP is for one bench laptop only: anyone on the path can read or block uploads (they still
> cannot forge records). Without `--token` the server refuses to register units. Key files (`*.evkey`)
> and the data folder are ignored by Git; never commit them.

Status: **proven on the air** on the bench: records reached the server from the hub through a
laptop (2026-11-15), signed with a development key, so not evidence-grade. The hardware key,
automatic outside time-stamps and upload through the web head are **planned** (`PLAN.md`). Source: `tools/evidence-server/src/main.rs`, `src/bin/evidence-upload.rs`.

## evidence-verify

**Purpose:** check an evidence log or export bundle and say exactly how far it can be trusted.

```text
evidence-verify [--list] [--key HEX | --keys FILE] [--allow-gaps] [--allow-test-key] [--quiet] <input>...
```

Inputs: a card's `EVIDENCE/` folder, `EVnnnnnn.EVL` files, any file of records back to back, or an
export bundle (`bundle.json`) from `evidence-server`.

| Option | Meaning |
|---|---|
| `--list` | Print every record, one line each |
| `--key HEX` | The unit's public key (130 hex digits), confirmed outside the log |
| `--keys FILE` | Trusted keys, one per line: hex, then a label; `#` comments |
| `--allow-gaps` | Missing records or files are gaps, not failures |
| `--allow-test-key` | Accept the published test key (this project's own tests only) |
| `--quiet` | Verdict lines only |

Exit codes: 0 VERIFIED, 1 FAILED, 2 could not run, 3 SIGNER CHECKED WITH GAPS, 4 SELF-CONSISTENT,
SIGNER NOT CHECKED (no key supplied). No verdict proves the log is complete.

```bash
target/release/evidence-verify --key <130 hex digits> bundle.json
```

Source: `tools/evidence-verify/src/main.rs`. Format: [Evidence log format](../EVIDENCE-FORMAT.md).

## fetch-droneid-samples.sh

**Purpose:** download the two public DJI DroneID recordings (RUB-SysSec/DroneSecurity, NDSS 2023)
for the DroneID tests and `droneid-hil`, pinned to one commit and checked by SHA-256.

```text
tools/fetch-droneid-samples.sh            # download from GitHub
tools/fetch-droneid-samples.sh --from DIR # copy from a local clone's samples/
```

**Writes:** `testdata/droneid/mavic_air_2` and `testdata/droneid/mini2_sm` (50 MS/s complex float32).
A file already present with the right checksum is kept; a download with the wrong checksum is
deleted and the script fails. **Licence:** the recordings are AGPL-3.0 repository content: they are
downloaded, never committed (`testdata/` is ignored).

Source: `tools/fetch-droneid-samples.sh`.

## flarm-keys

**Purpose:** extract the FLARM legacy decryption keys from a SoftRF source checkout into the key file
the FLARM decoder reads.

```text
flarm-keys SOFTRF_DIR_OR_LEGACY_H [--out FILE] [--force]
```

It reads `LEGACY_KEY1` to `LEGACY_KEY5` from SoftRF's `Legacy.h` (or `Legacy.cpp` beside it),
checks the result parses with `flarm::Keys::parse`, and writes it (default `tools/flarm.keys`).
`--out` chooses another file; `--force` replaces an existing one.

```bash
cargo run -p flarm-keys -- ~/src/SoftRF
```

> [!IMPORTANT]
> The owner may use these keys (2026-11-17), but they must never be published. The tool never
> prints a key value, writes the file readable only by you, and **refuses to write anywhere inside
> a Git repository that Git would not ignore**. For the hub, copy the file to the SD card as
> `ROTORSCP/FLARMKEY.TXT`. Real-key tests: `FLARM_KEYS_FILE=tools/flarm.keys cargo test -p flarm`.

Source: `tools/flarm-keys/src/main.rs`. Background: [Beyond 1090 MHz: UAT and FLARM](../AIRCRAFT-SIGNALS.md).

## flash-node.sh

**Purpose:** build and flash a multi-image listener node (XIAO ESP32-C5) through the board lock.

```text
tools/flash-node.sh [--open] <board> [seconds=20]
```

| Setting | Meaning |
|---|---|
| `--open` | Accept a board with an **open** key record (no link authentication) as a good flash |
| `NODE_WIRED=1` | Build wired (UART) images, no ESP-NOW |
| `NODE_BUS=1` | Build wired-bus images; every image must be a bus build |
| `NODE_SDR_ELF` | The node-sdr image for slot `ota_2` (default its normal, or with `NODE_BUS=1` its bus, build path) |
| `ROTORSCOPE_BENCH=1` | Allow bench images |

```bash
tools/flash-node.sh c5a
```

It builds `node-wifi` (2.4 GHz build, into `ota_0`) and `node-ble` (into `ota_1`), adds `node-sdr`
to `ota_2` only if that ELF exists (it never builds it; otherwise the slot is erased and the node
offers no DroneID or FPV), erases `otadata` and `rsjob` so the node starts `node-wifi`, and reads
the console. It never erases or writes `rskey`, the node's key record, which `link-key node` put
there. It stops if the board is not an ESP32-C5, if an image does not fit the node (an ESP-NOW node
takes only images that run the version 3 link, `"link":"v3"`; a wired or bus node takes none of
those; a bus node needs bus images), or if an image is a bench build without `ROTORSCOPE_BENCH=1`.
Images carry no key. After an ESP-NOW flash it reads the boot line and calls the flash good (exit
0) only if the node says it is keyed (`{"t":"uplink","auth":"kit:<id> slot:<n> gen:<g>
check:<c>",...}`), or, with `--open`, has an open record; otherwise it exits 1 with what the board
reported (`missing`, `damaged`, `wrong_board`, `no_rng`, `open`, or no boot line).

Source: `tools/flash-node.sh`.

## link-key

**Purpose:** make the kit's master key and write each board's ESP-NOW key record: every node gets
a key of its own, worked out from the master and its roster slot, generation and MAC address; the
hub gets the master and the roster. The procedure is in
[ESP-NOW in depth](../ESPNOW.md#kits-ship-open-sealing-yours).

```text
link-key kit new [--force]                  new master in tools/kit.key + empty roster; prints the kit id
link-key hub <hub-board> [--keep-boot]      master + roster to the hub's rskit; seeds rsboot
link-key node <board> [--slot N] [--label TEXT] [--mac MAC] [--hub <hub-board>]
                                            provision a node (next free slot, or slot N at gen+1)
link-key hub-roster <hub-board>             roster only to the hub (never the master)
link-key revoke --slot N                    mark a slot revoked (then hub-roster)
link-key open <board> [--hub] [--keep-boot] write an open (unauthenticated) record
link-key show                               kit id and roster with key checks
```

| Option | Meaning |
|---|---|
| `--kit-dir DIR` | Where `kit.key` and `kit.roster` live (default `tools/`, or `$ROTORSCOPE_KIT_DIR`) |
| `--table CSV` | The board's partition table (default `tools/partitions-node.csv`, or `tools/partitions-hub.csv` for hub commands); single-image nodes need `tools/partitions-single.csv` |
| `--hub-table CSV` | The hub's table for `node --hub` |
| `--dry-run` | Print the `dev-board.sh` / `espflash` commands; touch no board and leave the roster alone (`node` then needs `--mac`). Every board-writing command takes it |
| `--keep-boot` | `hub`, `open --hub`: leave the boot counter alone |
| `--bench-corrupt` | `node`, bench only (`ROTORSCOPE_BENCH=1`): write a record whose CRCs are both bad |

```bash
cargo run -p link-key -- kit new
cargo run -p link-key -- hub p4 --dry-run
cargo run -p link-key -- node c5a --label "roof" --hub p4
```

**Writes:** `tools/kit.key` (the master, owner-only, git-ignored) and `tools/kit.roster` (slot,
generation, MAC, key check and label per node; no secrets, git-ignored because it is per kit); on a
board, the `rskey`, `rskit` or `rsboot` partition through `tools/dev-board.sh --run` and `espflash
write-bin` (`$ROTORSCOPE_DEV_BOARD` overrides the script, for tests). **Prints:** never a key; the
kit id and each node's key check (8 hex digits each), which boards print at boot too.

How it behaves:

- `hub` first reads the hub's boot counter and, when it still reads good, seeds the new one just
  above it, so the nodes follow a re-provisioned hub at once.
- A key image goes to a 0600 file in a private temporary directory and is overwritten and deleted
  as soon as it is written, also on Ctrl-C (the command then says "interrupted" and exits 130).
  The directory is made in your runtime directory (`XDG_RUNTIME_DIR`, held in memory and readable
  only by you) when the system provides one, so the image never reaches a disk; otherwise in the
  system temporary directory. Only `kill -9` in that second leaves `link-key-<pid>-<random>` there.
- `kit new --force` replaces the master: then the hub and every node must be provisioned again.
- `link-key new` and `link-key id` (the one-key-per-kit commands) are removed. A leftover
  `tools/link.key` is read by nothing; keep it only until every board runs current firmware.

> [!IMPORTANT]
> The repository is public. `tools/kit.key` and `tools/kit.roster` are ignored by Git; never commit,
> paste or print `kit.key`. Back it up somewhere private: it is the only way to add a node to the
> kit without provisioning everything again.

Source: `tools/link-key/src/main.rs`, `tools/link-key/src/lib.rs`. Background:
[ESP-NOW in depth](../ESPNOW.md#securing-the-link).

## modes-live

**Purpose:** check our own Mode S / ADS-B decoder (`crates/modes`) against an ADSBee 1090's own
decode of the same aircraft.

```text
modes-live live PORT [--secs 600] [--beast-s 4] [--csbee-s 1.6] [--own-pos LAT,LON] [--record FILE] [--log FILE]
modes-live replay FILE [--own-pos LAT,LON] [--log FILE]
modes-live export FILE OUT [SECS]
modes-live check-export OUT
```

`live` alternates the ADSBee's console between Beast (raw frames, decoded by us; `--beast-s`
seconds) and CSBee (the ADSBee's own summary; `--csbee-s` seconds) and compares address, callsign,
squawk, category, altitude and position. `--record` saves everything read; `replay` runs a
recording again offline; `export` writes Beast frames with timing for the P4's `modesloop` test;
`check-export` shows what our decoder makes of an export.

```bash
cargo run --release -p modes-live -- replay ~/modes.bin --own-pos 39.9526,-75.1652
```

**Changes on the ADSBee:** only the running console protocol (`AT+PROTOCOL_OUT=CONSOLE,...`). It
reads the setting first and restores it at the end, also on Ctrl-C; it never saves settings and
never touches Wi-Fi or COMMS_UART. Status: aircraft **heard for real** through a real ADSBee from
the laptop. Source: `tools/modes-live/src/main.rs`. Background: [ADSBee 1090](../ADSBEE.md).

## p4-prepare.sh

**Purpose:** prepare the patched esp-hal the P4 hub builds against (upstream esp-hal supports only
P4 chip revision v3.0 and newer; the board carries v1.3).

```bash
tools/p4-prepare.sh
```

It reads `firmware/patches/esp-hal-p4-pre-v3/manifest.json`: the esp-hal commit (a full 40-hex
commit id; a branch or tag name is refused), the SHA-256 of the community pre-v3 patch, and the
SHA-256 of each of the project's own `rotorscope-*.patch` files, in the order they are applied. It
stops before downloading anything if a checksum differs, a listed patch is missing, or a
`rotorscope-*.patch` in the folder is not listed. It then fetches that one commit over https from
the fixed address `https://github.com/esp-rs/esp-hal.git`, checks that the checkout is that
commit, applies the patches and checks the result with `--verify`. If anything fails, it removes the
half-made checkout. **Writes:** `firmware/patches/esp-hal-p4-pre-v3/upstream/` (ignored by Git).
Needs `git`, `python3`, `sha256sum` and network access.

An existing checkout is never patched further: it is checked with `--verify`, and if it is not
exactly right the script stops and asks you to remove it and run again.

To check a prepared checkout without the network:

```bash
tools/p4-prepare.sh --verify
```

```text
verified .../upstream: esp-hal e02f3613e9f9ba1ce00070eb387e3bf4fde2267b + esp32p4-pre-v3.patch + rotorscope-pre-v3-memory.patch, 1137 tracked files hashed, nothing else changed
```

`--verify` only reads. It checks that the checkout is at the pinned commit, that its Git objects
are intact (`git fsck`), that no file is hidden from Git, and that every file differs from the
pinned commit exactly as the patches say, byte for byte, with nothing else changed or added. Files
Git ignores, such as build output, are not checked. `--verify DIR` checks another folder.

When you change a `rotorscope-*.patch` on purpose, or add one, record its checksum:

```bash
tools/p4-prepare.sh --update-sums
```

It rewrites the `local_patches` list in `manifest.json` (existing order kept, new patches last) and
prints the sums. Review the change with `git diff` and commit it together with the patch.

Source: `tools/p4-prepare.sh`; provenance in `firmware/patches/esp-hal-p4-pre-v3/PROVENANCE.md`.

## partitions-node.csv

**Purpose:** the flash layout of a multi-image node (ESP32-C5, 8 MB), used by `flash-node.sh`.

| Name | Type | Offset | Size | Holds |
|---|---|---|---|---|
| `nvs` | data | 0x9000 | 0x4000 | settings |
| `otadata` | data | 0xd000 | 0x2000 | which slot boots |
| `phy_init` | data | 0xf000 | 0x1000 | radio calibration |
| `ota_0` | app | 0x10000 | 0x200000 | `node-wifi` by convention |
| `ota_1` | app | 0x210000 | 0x200000 | `node-ble` by convention |
| `ota_2` | app | 0x410000 | 0x200000 | `node-sdr` by convention |
| `rsjob` | data | 0x610000 | 0x1000 | the job a node switched images for |
| `rskey` | data | 0x611000 | 0x1000 | the node's key record, written only by `link-key`; every image on the board reads it |

The running image recognises the others by the project name in their app descriptor
(`crates/nodeimage`), so the slot order is a convention, not a rule.

Two more tables live beside it. `tools/partitions-single.csv` is for single-image ESP-NOW nodes
(ESP32-S3 node-wifi, ESP32-C3 node-lr): espflash's default 4 MB layout with the app one 64 KB block
shorter and `rskey` at 0x3f0000; flash those boards with it (`dev-board.sh --pt`), or the node
reports its key record `missing`. `tools/partitions-hub.csv` is the P4 hub's: Elecrow's own four
rows unchanged, then `rskit` (master and roster, 0xC10000) and `rsboot` (boot counter, 0xC12000).
Every provisioning offset is fixed for good, so a reflash with the same table keeps the keys.

## rawcap-pull

**Purpose:** rebuild `node-sdr` raw captures from a log of its USB console.

```text
rawcap-pull [<log>|-] [--out <dir>] [--spectrum]
```

It finds every `@H`/`@D`/`@E` dump in the log (other lines ignored, any order, repeats welcome) and
for each complete one writes, in `--out` (default `.`): `c5-<id>.<kind>` (the raw payload),
`c5-<id>.cs16` (interleaved little-endian `i16` I/Q at the capture's rate) and `c5-<id>.json` (the
header), and prints the matching `droneid-file` command. For an incomplete dump it prints the
`resend` commands to type on the node. `--spectrum` adds each capture's averaged power spectrum in
40 bins. `-` or no file reads stdin.

```bash
cargo run --release -p rawcap-pull -- c5.log --out caps --spectrum
```

It never opens a serial port itself; capture with `tools/dev-board.sh --log 60 c5a | tee c5.log`.
Source: `tools/rawcap-pull/src/main.rs`.

## release-build.sh and release-parts.py

**Purpose:** build a release: the shipped firmware images of one commit, made so that anyone with
the same tool versions builds the same bytes, with `SHA256SUMS`, a record of the build inputs and a
parts list per image. The whole recipe, and how to check a release, is in
[Releases: build, check, sign](release.md).

```text
tools/release-build.sh [--commit REV] [--out DIR] [--only NAME,...] [--toolchain TC] [--p4-upstream DIR]
tools/release-build.sh --self-test
python3 -I tools/release-parts.py --self-test
```

Images: `gateway`, `node-wifi`, `node-ble`, `node-sdr`, `rid-beacon-BENCH-ONLY` (the bench
transmitter, not for the field) and `hub-p4`. It builds from a fresh local clone of the commit,
offline, and refuses a working tree with changes, bench builds, key tags, builder details in an
output and cargo settings outside the repository that change the build. It signs nothing: it ends
with the signing step for the owner. `release-parts.py` (standard library only) turns `cargo
metadata` and `cargo tree` for one firmware into the parts list and a CycloneDX file.

Source: `tools/release-build.sh`, `tools/release-parts.py`.

## repro-env.sh

**Purpose:** make a firmware image independent of where and when it is built, so two builds of one
commit give the same image. Source it (bash) in the firmware folder before building:

```bash
cd firmware/gateway && . ../../tools/repro-env.sh riscv32imac-unknown-none-elf
cargo build --release "${REPRO_CARGO_ARGS[@]}"
```

It sets `SOURCE_DATE_EPOCH` to the commit's time unless already set (the app descriptor's build
time and date then come from the commit), and `REPRO_CARGO_ARGS` to a `--config` that adds
`--remap-path-prefix` for the home folder (to `/home`), the rustup home (`/rustup`), the cargo home
(`/cargo`) and this checkout (`/rotorscope`), so panic messages and debug info name no local paths,
and maps the toolchain's standard library sources back to `/rustc/<commit>` (otherwise the
toolchain folder's name, such as `stable-x86_64-unknown-linux-gnu`, ends up in the image when the
`rust-src` component is installed).
The argument is the target triple (default `riscv32imac-unknown-none-elf`).

Remapping alone does not make the image independent of the checkout's folder: cargo puts the
location of every `crates/*` crate into symbol names. For that, call `repro_link <checkout>` after
sourcing it: it points the fixed path `/tmp/rotorscope-repro` at the checkout (under a lock, so one
build uses it at a time) and adds `--manifest-path` through it to `REPRO_CARGO_ARGS`.
`release-build.sh` and `check-image-identity.sh` build that way; `flash-node.sh` uses the remapping
only. It also defines `repro_leaks <file>...`, the search for builder details both scripts use.

Source: `tools/repro-env.sh`.

## rfuav-check

**Purpose:** run the C5's FPV-job detectors and the DroneID decoder over RFUAV recordings, through a
model of the C5's 4-bit capture path, to validate them against real signals.

```text
rfuav-check run <file.iq|-> --fc-mhz <MHz> --out <dir> --tag <name>
            [--fs 100e6] [--lo all|MHz,MHz,...] [--rec-noise-db -6] [--level-db 25 | --scale S]
            [--stride 4] [--max-ms MS] [--t0-ms MS] [--no-survey]
            [--save DIR] [--save-ms 3000] [--no-droneid] [--droneid-lo MHz,...]
rfuav-check probe <file|-> --fc-mhz <rec centre> --f-mhz <burst centre> [--bw-mhz 10]
            [--skip-ms 0] [--len-ms 1] [--lag-us 20 80] [--win-us 1]
rfuav-check replay <nib dir> <tag> <out dir> [--stride N]
rfuav-check summary <dir> <tag>
```

`run` reads RFUAV `.iq` files (interleaved float32 I/Q, 100 MS/s by default, centred on
`--fc-mhz`; `-` reads stdin) and writes `<tag>.dwell.jsonl`, `<tag>.win.jsonl`,
`<tag>.droneid.jsonl`, `<tag>.survey.csv` and `<tag>.meta.json` in `--out`; `--save` keeps quantised
dwells (`<tag>.lo<LO>.nib`) that `replay` reruns after a detector change. `probe` measures one
burst's OFDM symbol structure. `summary` prints a report from a run's files.

```bash
cargo run --release -p rfuav-check -- summary rfuav-results avata2
```

**Privacy:** serial numbers, UUIDs and positions are never written, only that a burst was found, its
CRC status, record type and product model. **Licence:** RFUAV recordings are Apache-2.0 but are not
committed; results stay in `rfuav-results/` (ignored by Git). Status: the detectors were tuned on
four RFUAV recordings (2026-11-16); this is **proven on recordings**, not heard live. Source:
`tools/rfuav-check/src/main.rs`. Background: [DJI radio links in depth](../DJI-LINKS.md).

## rid-ble-beacon

**Purpose:** a **bench-only** Bluetooth 4 Remote ID test transmitter for a Linux laptop, so
`rid-view --ble`, `node-ble` and phone apps can be tested without a drone or a second board.

```text
rid-ble-beacon ADAPTER [--interval-ms N] [--secs N]
```

| Option | Meaning |
|---|---|
| `ADAPTER` | A BlueZ adapter name, such as `hci0` |
| `--interval-ms` | Change the message every N ms, 200 to 2000 (default 500); each of the five types repeats every 5 × N ms |
| `--secs` | Stop after N seconds |
| `RID_TEST_LAT`, `RID_TEST_LON` | Centre of the 50 m test circle (environment; default Philadelphia City Hall) |

```bash
cargo run --release -p rid-ble-beacon -- hci0 --secs 60
```

> [!WARNING]
> This tool **transmits**. It uses the deliberately fake identity `TESTDRONE0000000001` /
> `TEST-OPERATOR` with Self-ID "BENCH TEST-NOT AIRCRAFT", the same as the `rid-beacon` firmware (it
> compiles that firmware's `rid_tx.rs` and `flight.rs` unchanged, so the two cannot drift apart).
> Bench use only: indoors, metres not kilometres, only while testing. Never use it to imitate a
> real aircraft. Ctrl-C or `--secs` withdraws the advert.

It registers a non-connectable BlueZ advertisement over D-Bus (no root), exactly 31 bytes, and asks
for minimum transmit power only when BlueZ reports it can set it. Status: **proven on the air**: heard by
`rid-view --ble` through a second Bluetooth adapter on the same laptop (2026-11-11, commit `b48fd32`). Source: `tools/rid-ble-beacon/src/main.rs`, `src/bluez.rs`.

## rid-view

**Purpose:** the laptop viewer: a live table of drones (and crewed aircraft and signals) from node
USB output, recorded files, stdin, a laptop Bluetooth adapter (`--ble`), a Sniffle dongle
(`--sniffle`), an ADSBee (`--adsbee`) or a built-in demo (`--demo`). It runs the hub's own
`tracker` code, and can serve the phone page (`--http`), send TAK CoT (`--cot`), append a CSV log
(`--log`) and feed DragonSync (`--dragonsync`).

```bash
cargo run --release -p rid-view -- --demo
```

Its full option list is `rid-view --help` (`tools/rid-view/src/main.rs`), and its user guide is
[The laptop viewer, rid-view](../use/rid-view.md). Developer notes on the Bluetooth, Sniffle and
DragonSync inputs are in
[`tools/rid-view/README.md`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/tools/rid-view/README.md).

**Next:** [Wire protocol reference](protocol.md)

## Where this lives in the code

- `tools/*/src/main.rs`: each Rust tool's argument parser and usage text
- `tools/dev-board.sh`, `tools/flash-node.sh`, `tools/p4-prepare.sh`, `tools/fetch-droneid-samples.sh`, `tools/docs-build.sh`, `tools/check-wifi-blobs.sh`, `tools/check-image-identity.sh`, `tools/repro-env.sh`, `tools/ci-lockfiles.sh`, `tools/check-hidden-chars.py`, `tools/release-build.sh`, `tools/release-parts.py`: the scripts, documented in their header comments
- `tools/partitions-node.csv`, `tools/partitions-single.csv`, `tools/partitions-hub.csv`: flash layouts
- `tools/bench/run.py`, `tools/bench/judge.py`: the bench runner
- `Cargo.toml`: which tools are workspace members
