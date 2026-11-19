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
| [`dev-board.sh`](#dev-boardsh) | Shell | Flashes and reads boards under a per-board lock |
| [`docs-build.sh`, `docs-check.py`](#docs-buildsh-and-docs-checkpy) | Shell, Python | Builds and checks this documentation site |
| [`droneid-file`](#droneid-file) | Rust | Decodes DJI DroneID from an I/Q recording |
| [`droneid-hil`](#droneid-hil) | Rust | Feeds the public DroneID recordings into a real C5 |
| [`evidence-server`](#evidence-server-and-evidence-upload) | Rust | Stores and exports hubs' signed evidence logs; `evidence-upload` feeds it |
| [`evidence-verify`](#evidence-verify) | Rust | Checks an evidence log or export bundle |
| [`fetch-droneid-samples.sh`](#fetch-droneid-samplessh) | Shell | Downloads the two public DroneID recordings |
| [`flarm-keys`](#flarm-keys) | Rust | Makes the private FLARM key file from a SoftRF checkout |
| [`flash-node.sh`](#flash-nodesh) | Shell | Builds and flashes a multi-image listener node |
| [`link-key`](#link-key) | Rust | Makes the kit's ESP-NOW link key |
| [`modes-live`](#modes-live) | Rust | Checks our ADS-B decoder against an ADSBee |
| [`p4-prepare.sh`](#p4-preparesh) | Shell | Prepares the patched esp-hal the P4 hub needs |
| [`partitions-node.csv`](#partitions-nodecsv) | Data | Flash layout of a multi-image node |
| [`rawcap-pull`](#rawcap-pull) | Rust | Rebuilds node-sdr captures from a console log |
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
```

| Option (before the positional arguments) | Meaning |
|---|---|
| `--send KEYS` | After the reset, type these characters on the console, one at a time (e.g. `rid-beacon`'s one-key commands) |
| `--send-after SECS` | When to start typing (default 8 s after the reset) |
| `--run CMD` | Run `CMD` (with `bash -c`) under the lock, with `BOARD_PORT` set, for at most `seconds`. No reset. Give an ELF too to flash first. |
| `--dry-run` | Print what would happen; touch no hardware, take no lock |
| `--pt`, `--app`, `--erase`, `--multi` | Multi-image flash: write the partition table, erase `otadata` and `rsjob` (plus any `--erase` label), write each ELF into its slot |

```bash
tools/dev-board.sh --list
```

```bash
tools/dev-board.sh --send xw firmware/rid-beacon/target/xtensa-esp32s3-none-elf/release/rid-beacon 40 s3
```

**Reads:** `tools/boards.local` (or `$ROTORSCOPE_BOARDS`), each ELF's `link-key:` and `bench-build:`
tags, the kit key's ID (through `link-key id`). **Writes:** the board; lock files in
`$ROTORSCOPE_LOCK_DIR` (default `~/.local/state/rotorscope/locks/`), plus the legacy
`/tmp/rotorscope-board-<name>.lock`; `--register` appends to `tools/boards.local`.

How it behaves:

- Boards are addressed by USB by-id path (it contains the MAC), never `/dev/ttyACMn`, never a
  serial number. Two names for one board share one lock. `--register` only finds boards under
  `/dev/serial/by-id/usb-Espressif_*`, and resets each new one to read its chip type.
- The lock lives outside `/tmp` because a sandboxed program can have a private `/tmp`, giving each
  program its own "shared" lock. Waiting is capped at 10 minutes. A held lock records the PID,
  time and command, which `--list` shows.
- Before flashing it prints each ESP-NOW image's link-key ID and **warns** if an image has no key or
  a different one from the kit key. It **refuses** a bench image unless `ROTORSCOPE_BENCH=1`
  (and warns when it is set).
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
**Reads:** the recording. **Writes:** stdout only. The file's doc comment also lists
`--llr-dump <file>`; the argument parser does not accept it as of 2026-11-18.

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
| `--open` | Build and flash an **open** kit on purpose (no link authentication) |
| `NODE_WIRED=1` | Build wired (UART) images, no ESP-NOW |
| `NODE_BUS=1` | Build wired-bus images; every image must be a bus build |
| `NODE_SDR_ELF` | The node-sdr image for slot `ota_2` (default its normal, or with `NODE_BUS=1` its bus, build path) |
| `ROTORSCOPE_LINK_KEY_FILE` | Key file (default `tools/link.key`); `none` needs `--open` |
| `ROTORSCOPE_BENCH=1` | Allow bench images |

```bash
tools/flash-node.sh c5a
```

It builds `node-wifi` (2.4 GHz build, into `ota_0`) and `node-ble` (into `ota_1`), adds `node-sdr`
to `ota_2` only if that ELF exists (it never builds it; otherwise the slot is erased and the node
offers no DroneID or FPV), erases `otadata` and `rsjob` so the node starts `node-wifi`, and reads
the console. It stops if the board is not an ESP32-C5, if there is no link key (without `--open`),
if the images carry different link keys, if a bus node has a non-bus image, or if an image is a
bench build without `ROTORSCOPE_BENCH=1`.

Source: `tools/flash-node.sh`.

## link-key

**Purpose:** make the kit's ESP-NOW link key, one random 32-byte key shared by the gateway and every
ESP-NOW node of one kit.

```text
link-key new [--force] [FILE]   # write tools/link.key (refuses to overwrite)
link-key id [FILE]              # print the key's ID (default tools/link.key)
```

```bash
cargo run -p link-key -- new
```

```bash
cargo run -p link-key -- id
```

**Writes:** 64 hex digits from `/dev/urandom`, owner-only permissions. **Prints:** only the key's ID
(8 hex digits, derived from the key, safe to show); boards print the same ID at boot. `new --force`
replaces the key: then rebuild and reflash the gateway and **every** ESP-NOW node, or they stop
talking to each other.

> [!IMPORTANT]
> The repository is public. `tools/link.key` and `*.linkkey` are ignored by Git; never commit,
> paste or print the file. Back it up somewhere private.

Source: `tools/link-key/src/main.rs`. Background: [ESP-NOW in depth](../ESPNOW.md#securing-the-link).

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

It reads the pinned esp-hal revision and the patch's SHA-256 from
`firmware/patches/esp-hal-p4-pre-v3/manifest.json`, refuses a patch whose checksum differs, clones
that one commit, applies the community pre-v3 patch, then every `rotorscope-*.patch` in the same
folder. On an already prepared tree it applies only missing project patches; a tree at another
commit is refused ("remove it and rerun"). **Writes:** `firmware/patches/esp-hal-p4-pre-v3/upstream/`
(ignored by Git). Needs `git`, `python3`, `sha256sum` and network access.

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

The running image recognises the others by the project name in their app descriptor
(`crates/nodeimage`), so the slot order is a convention, not a rule.

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
- `tools/dev-board.sh`, `tools/flash-node.sh`, `tools/p4-prepare.sh`, `tools/fetch-droneid-samples.sh`, `tools/docs-build.sh`: the scripts, documented in their header comments
- `tools/bench/run.py`, `tools/bench/judge.py`: the bench runner
- `Cargo.toml`: which tools are workspace members
