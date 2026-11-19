# Firmware reference

**Level: 🔴 Expert** · One section per firmware project in `firmware/`: chip and target, toolchain,
cargo aliases and features, build-time inputs, task layout, memory, crash behaviour, console and
proof level.

Each firmware is its own Cargo workspace (it has a `[workspace]` table of its own), so it never
joins the host workspace at the repository root and builds for its own target. Everything below is
read from each project's `Cargo.toml`, `.cargo/config.toml`, `rust-toolchain.toml`, `build.rs` and
sources at commit `6651ecd` (2026-11-18). For how to set up a machine and flash, see
[Set up your machine](setup.md) and [Build, test and flash](build-test-flash.md). The proof words
are defined in [Writing these docs](docs-style.md#say-what-is-proven-every-time).

## Shared rules

### Versions

Every firmware except `hub-p4` uses the crates.io Espressif stack: `esp-hal ~1.2.2`,
`esp-rtos 0.4.0` (with embassy), `esp-bootloader-esp-idf 0.6.0`, `esp-alloc 0.11.0`,
`esp-backtrace 0.20.0`, `esp-println 0.18.0`, and **`esp-radio =1.0.0-beta.1`**, pinned exactly
because beta.2 was yanked and replaced in every radio firmware by the patched copy in
`firmware/patches/esp-radio` (see [patches](#patches); `node-lr` takes esp-radio only in its
`espnow` build). `hub-p4` takes
`esp-hal`, `esp-backtrace`, `esp-bootloader-esp-idf` and `esp-println` from a patched esp-hal
checkout (see [patches](#patches)) and uses no esp-rtos, esp-alloc or esp-radio. Release profiles are `opt-level = "s"`, `lto = "fat"`,
`codegen-units = 1` with debug info, except `node-sdr`, which uses `opt-level = 3` for its
per-sample loops.

### The bench fence (`ROTORSCOPE_BENCH=1`)

Some features fake data, open debug doors or loop a board's pins back on themselves. Every
firmware's `build.rs` except `rid-beacon`'s calls `crates/link/src/benchfence.rs`, which **stops
the build** if any bench-only feature or bench build switch is on and the environment does not say
`ROTORSCOPE_BENCH=1` exactly. A bench build then says so: a cargo warning, the list in its hello
line (`ROTORSCOPE_BENCH_JSON`), and in every image except `hub-p4` the tag
`bench-build:<list>` in the ELF, which `tools/flash-node.sh` and `tools/dev-board.sh` look for
and refuse without `ROTORSCOPE_BENCH=1`. The P4 shows `BENCH BUILD` on its screen and writes the
list into the evidence boot event instead. Details: [Bench tests](../BENCH.md), "Bench builds".

### The link key (`tools/link.key`)

The heading names the file the old firmware used. Since the cut-over of 2026-11-18 (link protocol
14) **no firmware build reads a key**: not `tools/link.key`, not `ROTORSCOPE_LINK_KEY_FILE`, not
any other file or variable, and no image carries a `link-key:` tag. Every image is the same for
every kit. Instead:

| Board | Key material | Where |
|---|---|---|
| ESP-NOW node (`espnow` builds of `node-wifi`, `node-ble`, `node-sdr`, `node-lr`) | its own key record | `rskey` partition, read at boot (`firmware/node-common/src/keystore.rs`) |
| P4 hub | the kit master and roster; the boot counter | `rskit` and `rsboot` partitions (`firmware/hub-p4/src/linkauth.rs`) |
| Gateway | none: the roster's MAC addresses only, from the hub, in RAM | |

`link-key` writes the partitions ([Tools reference](tools.md#link-key)); the procedure is
[ESP-NOW in depth](../ESPNOW.md), "Kits ship open; sealing yours". A node prints its kit id, slot,
generation and key check at boot (`{"t":"uplink","auth":"kit:<id> slot:<n> gen:<g> check:<c>",...}`),
or `"auth":"open"` / `"auth":"none"` with the reason; `tools/flash-node.sh` judges a flash by that
line. Wired, wired-bus and S3 hub builds use no key. A board still on the old firmware is refused
as `old_firmware`, and the old `tools/link.key` (still git-ignored) can be deleted once every board
is reflashed.

### Crash recovery

Every firmware is built with esp-backtrace's `custom-halt`: after a panic the backtrace is printed
and the chip resets about 0.5 s later (on `hub-p4`, 5 s if the panic came within 10 s of boot).
Every firmware except `rid-beacon` also arms the chip's RTC watchdog after boot. The boot line of
every firmware except `hub` and `rid-beacon` says why the chip last reset. Bench results are in
[Bench tests](../BENCH.md), "Crash recovery".

| Firmware | Watchdog fed by | Timeout |
|---|---|---|
| `gateway` | the relay loop, only while the beacon loop has gone round in the last 5 s | 10 s |
| `hub-p4` | the main loop, every pass | 30 s |
| `hub` | the 1 Hz tick | 10 s |
| `node-wifi` | the channel hopper, every hop | 30 s (a hub plan may dwell 10 s) |
| `node-ble` | the stats task, once a second | 10 s |
| `node-sdr` | the capture loop, every pass | 10 s |
| `node-lr` | the stats loop in `main`, once a second | 10 s |
| `rid-beacon` | no watchdog | |

## gateway

**What it is.** The ESP-NOW gateway: one XIAO ESP32-C5 that receives every ESP-NOW node and passes
each packet to the hub over one wire, untouched and still sealed, as `link::gw` messages (link
protocol 14; never link frames, so no `Via`). It **holds no key**: the hub verifies every node
packet and seals the beacons, commands and time syncs, which the gateway only puts on the air
(beacons as the hub sends them, twice a second on 2.4 GHz channel 6; commands unicast until the
radio acknowledges them or the hub's time limit runs out). A captured gateway can drop, delay or
repeat traffic, never forge it. It transmits. See [ESP-NOW in depth](../ESPNOW.md) and
[Wire protocol reference](protocol.md#the-gateway-wire-linkgw).

| Item | Value |
|---|---|
| Chip and target | ESP32-C5, `riscv32imac-unknown-none-elf` |
| Toolchain | `stable`, with `rust-src` (`rust-toolchain.toml`); `rust-version = "1.88"` |
| Alias | `cargo fw` = `build --release` |
| Rustflags | `-C force-frame-pointers`, `-C link-arg=-Tlinkall.x` |

**Features.**

| Feature | Meaning |
|---|---|
| `faulttest` | **Bench only.** Type `fault panic` or `fault hang` on USB to prove the reboot after a panic and the hang watchdog |
| `gwforge` | **Bench only.** `gwforge ...` typed on USB makes the gateway behave as a captured one (made-up and replayed packets, a flipped byte, a held time sync, dropped packets); the hub shows `BENCH GATEWAY`. Not with `faulttest` |

**Build-time inputs.** `ROTORSCOPE_BENCH`. No key.

**Tasks.** `main` sets up the hub UART (UART1, 921600: TX GPIO11, RX GPIO12; received in the UART
interrupt into a 4 KB ring), Wi-Fi in 2.4 GHz with country "US" and Espressif Long Range added to
the station's protocols, ESP-NOW on channel 6, then spawns `air_tx` (beacons and transmits),
`hub_in` (the hub's `GwDown` messages), `hub_out` (`GwUp` messages to the hub), `stats` (the
once-a-second USB line) and, with `faulttest` or `gwforge`, their console; `main` then runs the
receive loop. Every packet heard goes through `airlink::relay`'s keyless filters (roster, 40
packets and 16 KB a second per address, 50 KB a second in total, byte-identical retries dropped,
round-robin per address); what it refuses is reported to the hub as `GwUp::Refused`, and the hub
raises the `forged_link` threats.

**Memory.** 64 KB heap in the RAM the bootloader frees (`heap_allocator!(#[ram(reclaimed)] 65_536)`).

**Console.** USB-Serial-JTAG: JSON lines out (`hello` with `"key":"none"`, `roster` when the
hub's first roster arrives, a `gw` summary once a second, `refused`, and `warn`). No commands except
`fault ...` in a `faulttest` build and `gwforge ...` in a `gwforge` build.

**Proof.** Before the 2026-11-18 cut-over the version 2 gateway was **proven on the air**: it
relayed two ESP-NOW nodes to the P4 with 0 corrupt frames and delivered the hub's commands (21 of
21 acknowledged first try) on 2026-11-14, and crash recovery was proven on the air on 2026-11-18.
The keyless protocol 14 gateway is **simulated only** (tested on a computer, built for the board); its bench pass is
docs/BENCH.md, "Link keys: bench checklist".

## hub-p4

**What it is.** The hub on the Elecrow CrowPanel Advanced 5" (ESP32-P4, silicon v1.3): tracker,
touch screen, SD log, GPS, ADSBee, coverage (telling nodes what to listen to), evidence log, FAA
rule flags, aero and control-link tables, and the web-head feed. Bare-metal Rust with no async
executor: one main loop.

| Item | Value |
|---|---|
| Chip and target | ESP32-P4 v1.3, `riscv32imafc-unknown-none-elf` (with the single-precision FPU) |
| Toolchain | `stable` (`rust-toolchain.toml`) |
| Alias | `cargo fw` = `build --release` |
| Target directory | `target/rev103` (pre-v3 and v3+ images are not interchangeable) |
| Rustflags | `-C link-arg=-Tlinkall.x`, `-C force-frame-pointers` |
| `[env]` in `.cargo/config.toml` | `ESP_HAL_CONFIG_MIN_CHIP_REVISION = "103"` (unset, esp-hal assumes v3.0 and the image traps on its first lock); `ESP_HAL_CONFIG_L2_CACHE_SIZE = "256KB"` (must match the L2 cache size the bootloader sets); `ESP_LOG = "info"` |
| esp-* crates | by path from `firmware/patches/esp-hal-p4-pre-v3/upstream/`, made once by `tools/p4-prepare.sh` |
| Output | `firmware/hub-p4/target/rev103/riscv32imafc-unknown-none-elf/release/hub-p4` |

Run `tools/p4-prepare.sh` once before the first build. Flash with Elecrow's own bootloader from
the backup of the factory image (espflash's bundled P4 bootloader is not used) and, since the
2026-11-18 cut-over, the partition table `tools/partitions-hub.csv`: Elecrow's own four rows
unchanged, plus `rskit` (kit master and roster) and `rsboot` (boot counter) for the keyed ESP-NOW
link, which `link-key hub` writes. The command and the reason are in
[BRINGUP.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub-p4/BRINGUP.md),
"Build and flash". The CH340 console fails at 921600 for flashing; 460800 works.

**Features.** `default = ["wirebus"]`.

| Feature | Bench only | Meaning |
|---|---|---|
| `wirebus` | no (default) | Poll many nodes on the node link as a wired bus ([The wired bus in depth](../WIREBUS.md)). An ESP-NOW gateway or one point-to-point node is still recognised at run time and read as before. `--no-default-features` never polls |
| `show-link-ids` | no | Show open FPV links' identifiers on every output (screen, console JSON, SD log). Off by default by project policy |
| `rskit-readonly` | no | Bring-up: read the kit records and the boot counter, never write them. The keyed link then stays closed (`NO BOOT COUNTER` on the screen) |
| `modes` | no | Decode the ADSBee's raw Beast frames with `crates/modes` instead of trusting its CSBee summary (`AT+PROTOCOL_OUT=COMMS_UART,BEAST`). CSBee lines are still accepted. Off by default until run with a real ADSBee on this hub |
| `selftest` | yes | UART2 sends sample link frames onto the node-link RX pad inside the chip, so the link reader is tested with no wire. Leave the UART1 header unconnected |
| `fakenode` | yes | Once a second a made-up ESP-NOW node (`02:52:53:00:00:01`) reports through the link decoder, so the coverage coordinator assigns it and real commands go to the gateway. Nothing is driven onto any pin |
| `fakedrone` | yes | Once a second a made-up Remote ID drone (`RSTEST-SD-0001`) flying over PHL is decoded as if relayed, so the tracker, SD log and FAA flags have data. Nothing is driven onto any pin |
| `benchlog` | yes | Print the tracker's tables (a line per new slot, a summary every 5 s) and count the previous SD log file's rows at mount |
| `gpsloop` | yes | On-chip loop-back of the GPS and ADS-B inputs: UART2 plays a Quectel LC29H(AA) on the GPS pads, the PPS pad is pulsed from inside, UART4 sends CSBee onto its own RX pad. Excludes `selftest` |
| `gpsloop-m10` | yes | As `gpsloop`, but UART2 plays a u-blox MAX-M10S as shipped (NMEA at 9600), exercising the baud hunt and UBX configuration |
| `webheadloop` | yes | On-chip loop-back of the web-head feed: UART4 RX moves to the feed's TX pad and decodes what UART2 sends; NMEA and CSBee are injected from inside |
| `embedpack` | yes | Build the airspace pack named by `AIRSPACE_PAK` into the firmware and write it to the card as `ROTORSCP/AIRSPACE.PAK` when the card has none or a different one |
| `modesloop` | yes | Implies `modes`. UART4 replays recorded Beast frames (real aircraft, recorded from the owner's ADSBee on the laptop on 2026-11-15) onto its own RX pad |
| `faulttest` | yes | Console line `fault panic` or `fault hang` (start with `f`, end with Enter) |
| `testpoint` | yes | Console `p` sets the hub's position to a fixed test point near Philadelphia City Hall, for a bench with no GPS |

The firmware refuses to compile `selftest` with `gpsloop`, `modesloop` with `gpsloop` or
`webheadloop`, and `webheadloop` with `selftest` or `gpsloop` (they share UARTs).

**Build-time inputs** (`build.rs`, `src/evlog.rs`, `src/gpsloop.rs`, `src/sd.rs`):

| Variable | Effect |
|---|---|
| `ROTORSCOPE_BENCH` | The bench fence (above) |
| `ROTORSCOPE_DEV_KEY` | The evidence log's development signing key, 64 hex digits. Set but malformed stops the build; unset, the firmware signs with the published test key and says so on screen. Never printed |
| `SOURCE_DATE_EPOCH` | The build date as Unix seconds (else the time `build.rs` ran), baked in as `ROTORSCOPE_BUILD_UNIX_S`: GPS time before it is impossible |
| `GPSLOOP_SCENARIO` | `gpsloop` only: which scripted GPS scenario the emulated receiver plays |
| `AIRSPACE_PAK` | `embedpack` only: the pack file to embed |

**Main loop.** `main` brings up, in order: the console on UART0, a check that the stack is below
the L2 cache (it asserts `sp < 0x4FF80000`), the bit-banged board I2C (touch and backlight), a full
PSRAM test, the RGB panel streaming from a PSRAM framebuffer, the node link (UART3, interrupt into
an 8 KB ring), the web-head feed (UART2), the GPS (UART1 ring) and PPS interrupt, the ADSBee
(UART4 ring), the SD logger, the FAA engine, the evidence log and the tracker. Then it arms the
watchdog and loops forever. Each pass:

1. feeds the watchdog, pumps the console, drains the node link (`gw.poll`), hands frames to the
   tracker (`app.ingest`), runs the downlink (coverage commands), GPS and ADSBee;
2. handles one console byte;
3. once a second: housekeeping, `TimeSync` to nodes from GPS time, aero and control-link reports,
   site settings, evidence watch, GPS memory, FAA rules, the web-head feed's own frames, and the
   JSON lines (`report::tick`);
4. reads touch, applies screen requests (hub moved, accept home, mute, snooze, unmute, nodes at
   hub);
5. draws the screen one band at a time, polling the link and console between bands;
6. advances the SD logger by one small step;
7. bench features, if built.

The screen band (about 7 ms) is the longest normal step; a normal build with nothing on the GPS
and ADSBee pins ran about 600 passes a second (BRINGUP.md step 17). Everything slow is cut into
steps so the loop never waits.

**Memory** (`src/main.rs`, `src/evlog.rs`, `src/faa.rs`, `src/aero.rs`, BRINGUP.md):

| Region | What lives there |
|---|---|
| Internal RAM 0x4FF40000..0x4FF80000 (256 KB) | Statics, then the main stack (whatever the statics leave). Rings: console 16 KB, node link 8 KB, web-head queue 8 KB, GPS 2 KB, ADSBee 4 KB. Panel DMA descriptors |
| L2 cache 0x4FF80000..0x4FFC0000 | Not RAM: the project's memory patch keeps RAM and stack out of it |
| PSRAM offset 0 | Framebuffer, 800x480 RGB565 (768 KB) |
| PSRAM 2 MB | The tracker (about 64 KB). Moved out of internal RAM on 2026-11-17: `main`'s frame is about 65 KB, and the SD mount had overrun the stack guard on every boot with a card in |
| PSRAM 4 MB | Evidence queue, 256 KB (about 9 minutes of one detection a second); an 8 KB internal fallback exists |
| PSRAM 5 MB | The FAA rule engine (about 84 KB) |
| PSRAM 6 MB | Aero store: aircraft frame queue, the 1090 table, control-link and area-GPS tables |
| PSRAM 8 MB | The airspace pack, up to 8 MB |

Compile-time asserts check that these regions do not overlap. **The hub cannot run without PSRAM**:
if the PSRAM test fails, it panics with a message ("no PSRAM: the tracker lives there; cannot
start"), and so reboots in a slow loop.

**Watchdog and panic** (`src/fault.rs`). The RTC watchdog (LP_WDT) through the patched HAL, 30 s.
It is armed only if the slow-clock calibration looks plausible and the timeout read back from the
chip is within a factor of two of 30 s; otherwise the boot line says `watchdog LEFT OFF`. After a
panic: backtrace, then 500 ms (5 s if the panic came within 10 s of boot, so a start-up panic loops
slowly), then a software reset.

**Console** (`src/console.rs`, `src/main.rs`). The USB-C port (CH340 on UART0, 115200 baud) is the
only serial port: the P4's own USB-Serial-JTAG pins carry the microphone. Log lines start
`hub-p4:`, JSON lines start `{`, evidence lines start `@EV1`. Output never blocks the loop: a JSON
line that does not fit the 16 KB ring is dropped whole and counted (`console_drops`). Opening the
port resets the P4 every time.

Commands are one character, no Enter, except where a line is typed:

| Key | Does |
|---|---|
| `p` | `testpoint` builds only: set the hub's own position to the test point. In a normal build it only says it is bench-only |
| `c` | Clear the manual own position |
| `b` | Time the airspace lookups (prints a benchmark) |
| `a` | Cycle the aero plan: auto, or one mode for every aero node (1090, UAT, FLARM, OGN, control links) |
| `m` | Hub moved: forget the GPS home the spoofing checks learned |
| `H` | Accept the new GPS home the hub found (after a "NOT adopted" line) |
| `T` | Accept GPS time as it is now (after a time jump or time-floor alarm) |
| `L` | Type one site line, then Enter (Esc cancels): `node <address> <lat> <lon>`, `node <address> clear`, `node all here`, `home <lat> <lon> [height]`, `home clear`, `mute <kind> [source=..] [target=..] [mhz=..] [ssid=..]`, `unmute all`, `control ids on\|off`, `control content on\|off`. See [Site settings](../SITE.md) |
| `n` | Type an operator note, then Enter (Esc cancels): it goes into the evidence log |
| `k` | Evidence checkpoint now |
| `r` | Replay the evidence file over the console |
| `u` | Evidence stream on the console on or off |
| `e` | Evidence shutdown checkpoint; the card is safe to remove once the next flush line shows `queued=0` |
| `h`, `?` | Help |
| `fault panic`, `fault hang` | `faulttest` builds only; start with `f`, end with Enter |

JSON lines once a second (`src/report.rs`): `new`, `track`, `link`, `aircraft`, `control`,
`control_energy`, `threat`, `conflict`, `area_gnss`, `flags`, and the P4-specific `hub` status
line with its companions `bus`, `aero` and `control_stats`. Field names match the S3 hub's USB
stream where both have them.

**Proof (as of 2026-11-18).** Screen, touch, SD log, node link, downlink and coverage: **proven on
the air** (they worked on the running bench kit). The test drone from `rid-beacon` reached the track
table through a node and the gateway: **proven on the air** (2026-11-14). GPS, ADSBee and web-head
feed: **simulated only** (on-chip loop-back). Wired bus: **simulated only** (never wired). Crash
recovery: **proven on the air** (2026-11-18). Nobody could see the screen during the 2026-11-18
pass.

## hub

**What it is.** The first hub, on an ESP32-S3 (with a C5 build kept compiling). It reads up to three
wired nodes, merges detections into tracks, runs a Wi-Fi hotspot on channel 11 with the phone page,
sends TAK CoT, streams JSON on USB, can join a network as a sensor (station mode), and reads a GPS,
a 320x240 screen, three buttons, a buzzer, an SD card and a battery. With `webhead` it is the P4's
web head ([WEBHEAD.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub-p4/WEBHEAD.md)).

| | `s3/` | `c5/` |
|---|---|---|
| Target | `xtensa-esp32s3-none-elf` | `riscv32imac-unknown-none-elf` |
| Toolchain | `esp` (Espressif's fork, from `espup`; run `. ~/export-esp.sh` first) | `stable` with `rust-src` |
| Alias | `cargo fw` = `build --release --features esp32s3` | `cargo fw` = `build --release --features esp32c5` |
| Other | `build-std = ["alloc", "core"]`; `-C link-arg=-nostartfiles`; `ESP_LOG = "warn"` | `-C force-frame-pointers`; `ESP_LOG = "warn"` |

Build from inside `s3/` or `c5/`; `build.rs` stops a build whose chip feature does not match the
target, and an Xtensa build whose linker is not on `PATH`. `rust-version = "1.88"`.

**Features.**

| Feature | Bench only | Meaning |
|---|---|---|
| `esp32s3`, `esp32c5` | | Exactly one: the chip |
| `demo` | yes | Two synthetic, obviously fake drones circling the hub (and a synthetic helicopter with `adsbee`); a made-up own position and the build machine's clock |
| `open-hotspot` | yes | The hotspot with no password |
| `ili9341` | | An ILI9341 (2.8" class) screen instead of the default ST7789 (2.0" class) |
| `gps-uart` | | The GPS on UART2 instead of I2C; costs node link 2 |
| `show-link-ids` | | Show open FPV links' identifiers on every output; off by default by project policy |
| `adsbee` | | An ADSBee 1090's CSBee output on UART2 (S3 only; costs node link 2; excludes `gps-uart`) |
| `webhead` | | Adds the aircraft table for the P4's forwarded ADS-B; every build already understands the P4's feed |

**Build-time inputs** (`build.rs`). `ROTORSCOPE_BENCH`. **Station mode** is compiled in only when
the gitignored file `firmware/hub/wifi.local.toml` exists, with `ssid` (1-32 bytes), `psk` (empty
for an open network, else 8-63 characters) and optional `allow_writes` (let the station network
change the own position and the CoT switch). After creating the file, touch `build.rs` or build once
with `HUB_WIFI_RESCAN=1`. `demo` builds bake in the build time as a stand-in clock.

**Tasks.** Core 0 runs a thread-mode embassy executor: ingest into the tracker, the 1 Hz tick and
USB stream, USB commands, the network side (hotspot, DHCP, DNS, HTTP tasks, CoT, station mode)
and the GPS task. A priority-2 interrupt executor runs the node UART readers, the ADSBee reader and
the PPS task, so a slow phone page cannot overflow a UART. On the S3 the second core runs its own
executor for the screen, SD card, buzzer and battery, whose drivers block; on the C5 they share
the one core. The tracker is touched only from core 0's thread-mode executor.

**Memory.** No PSRAM on the bench S3. Heap: 73,744 bytes reclaimed from the bootloader plus 64 KB
(56 KB in `adsbee` and `webhead` builds); C5: 65,536 plus 60 KB. Core 1's stack, 12 KB, comes from
the heap. Measured with the demo on 2026-11-09: at least 55.6 KB of heap free while running, about
29 KB left for core 0's stack (19.2 KB in an `adsbee` build). **As of 2026-11-18 the S3 build does
not link**: static RAM is over by 18,088 bytes
([firmware/hub/README.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub/README.md),
"Security checks still to run on hardware", item 14).

**Console.** USB-Serial-JTAG. Keys: `p` set the own position to a test point, `c` clear it, `m`
CoT on or off, `s` print the state body and a check, `h` hotspot details and help (with **BOOT**
held, also the hotspot password), `R` with BOOT held: a new hotspot password and a restart.

**Proof.** **Simulated only**: the S3 hub ran on a bare S3 with the made-up `demo` drones (every
peripheral reported absent). GPS, screen, SD card, buttons, buzzer, battery and ADSBee have never
run against real parts: **built, not yet tried**. The web head: **built, not yet tried**. The
current build does not link, so nothing in `firmware/hub` ran in the 2026-11-18 bench pass.

## node-ble

**What it is.** Node B, the Bluetooth Remote ID listener: Bluetooth 4 legacy adverts, Bluetooth 5
extended adverts and Long Range (coded PHY), passive extended scan on 1M and coded PHYs with
duplicate filtering off. Also reports Bluetooth pairing pop-up spam (`wifiwatch`). Never transmits,
except over ESP-NOW in an `espnow` build.

| Item | ESP32-S3 (default) | ESP32-C5 |
|---|---|---|
| Target | `xtensa-esp32s3-none-elf` | `riscv32imac-unknown-none-elf` |
| Toolchain | `esp` (`rust-toolchain.toml`) | `cargo +stable` overrides it |
| Build | `cargo build --release` | `cargo +stable build --release --no-default-features --features esp32c5 --target riscv32imac-unknown-none-elf` |

There is no cargo alias and no chip subdirectory. `rust-version = "1.95"`. `.cargo/esp-config.toml`
sets `ESP_LOG="warn"`; log records become JSON lines.

**Features.** `default = ["esp32s3"]`.

| Feature | Bench only | Meaning |
|---|---|---|
| `esp32s3`, `esp32c5` | | Exactly one: the chip |
| `espnow` | | Send hub frames over ESP-NOW to the gateway and take its commands. Wi-Fi sits on channel 6 and the scan stops for 100 ms of every second; needs esp-radio's coexistence support. The node then transmits |
| `wirebus` | | Report on the wired bus (TX open-drain D9 = GPIO9, RX D7 = GPIO12). The scan never pauses. C5 only; not with `espnow` |
| `faulttest` | yes | `fault panic` / `fault hang` on USB |

**Build-time inputs.** `ROTORSCOPE_BENCH`; bench switches `ROTORSCOPE_BENCH_CAN` (report only one
role: `ble`, `wifi5` or `wifi24`) and `ROTORSCOPE_BENCH_FLIP` (leave Bluetooth and Wi-Fi 2.4 out of
`can`, so the hub flips the node between images). No key: with `espnow` the node reads its key
record from `rskey` at boot.

**Tasks.** The BLE host's receive loop runs the report handler, which counts every advert, checks
cheaply for Remote ID (service data UUID 0xFFFA, app code 0x0D), reassembles chained adverts and
queues matches (16 entries; a full queue is counted, never waited on). `decode_task` decodes with
`crates/rid`; `stats_task` prints once a second and feeds the watchdog; `role::control` applies hub
assignments and switches images; `usb_task` writes JSON through an 8 KB pipe. The hub link is
`uart_task` + `uart_rx_task` (wired), `bus_task` on a priority-2 interrupt executor (`wirebus`), or
the ESP-NOW `uplink` task.

**Memory.** Heap: 73,744 bytes reclaimed (S3) or 65,536 (C5), plus 32 KB of headroom for queued HCI
events, plus 48 KB for the Wi-Fi driver in `espnow` builds.

**Console.** USB-Serial-JTAG, output only (JSON lines such as `hello`, `rid`, `rid_err`, `stats`,
`threat`, `log`). USB input is read only by `faulttest`.

**Proof.** **Proven on the air**: on a C5, Bluetooth 4 legacy, 5 extended and Long Range Remote ID
from a C5 `rid-beacon` (2026-11-12); on an S3, a Bluetooth 4 broadcast from a laptop, and all three
from a C5 `rid-beacon` in the mixed-chip bench tests (2026-11-12). No real drone.

## node-common

**What it is.** A library, not a firmware: the flash and fault glue shared by the nodes and the
gateway. It builds inside each firmware's workspace, for that firmware's chip.

- `lib.rs`: which images the board holds (app slots `ota_0`..`ota_2`, recognised by the project
  name in each app descriptor), the job a node switched images for (the 4 KB `rsjob` sector), and
  switching: write the job, point `otadata` at the target slot, reset. Layout in
  `tools/partitions-node.csv`. Also `bench_can_mask` for `ROTORSCOPE_BENCH_CAN`.
- `fault.rs`: `arm` / `feed` for the RTC watchdog, `reboot_after_panic`, the reset reason as one
  word, and the bench `fault` console.
- `bus.rs` (`wirebus` feature): the wired-bus node end, including the open-drain TX set-up.
- `keystore.rs`, `linkauth.rs` (`espnow` feature): read the node's key record from `rskey` at
  boot (read only; `wrong_board` if made for another board, `missing` if the table has none), and
  the chip side of the version 3 link: the hardware random source, the boot line
  (`{"t":"uplink","auth":...}`), the `link_auth` refusal counts and the status LED. The decisions
  are in `crates/nodeimage` (`uplink`).

| Feature | Meaning |
|---|---|
| `esp32s3`, `esp32c5`, `esp32c3` | The chip (`esp32c3` for the aero node, which holds a single image) |
| `wirebus` | The wired-bus node (`src/bus.rs`), used by node-wifi, node-ble, node-sdr and node-lr through their own `wirebus` feature |
| `espnow` | The ESP-NOW link glue (`src/keystore.rs`, `src/linkauth.rs`), used through each node's own `espnow` feature |
| `faulttest` | Bench only, enabled only through a firmware's own `faulttest`: the `fault panic` / `fault hang` console |

No toolchain file, aliases or build script of its own.

## node-lr

**What it is.** The aero node: a receive-only Semtech LR2021 on SPI, attached to an ESP32-C3,
hearing 1090 MHz extended squitter, 978 MHz UAT, FLARM and OGN trackers (following their hops),
and drone control links (ExpressLRS, mLRS, SiK, OpenLRSng). There is no transmit path to the
LR2021 (`crates/lr2021cfg` has no transmit command). See [The aero node](../AERO-NODE.md) and
[Control links](../CONTROL-LINKS.md).

| Item | Value |
|---|---|
| Chip and target | ESP32-C3, `riscv32imc-unknown-none-elf` (no atomics: counters use `portable-atomic`) |
| Toolchain | `stable` |
| Aliases | `cargo fw` = `build --release` (wired UART); `cargo fw-now` = `build --release --features espnow --target-dir target-espnow`; `cargo fw-bus` = `build --release --features wirebus --target-dir target-bus` |
| Rustflags | `-C force-frame-pointers` |

**Features.** `default = []`.

| Feature | Bench only | Meaning |
|---|---|---|
| `espnow` | | Report through the ESP-NOW gateway, sealed with the node's own key from `rskey` (flash it with `tools/partitions-single.csv`); Wi-Fi sits on channel 6 |
| `wirebus` | | The wired bus, TX open-drain on GPIO0. Not with `espnow` |
| `faulttest` | yes | `fault panic` / `fault hang` on USB |

**Build-time inputs.** `ROTORSCOPE_BENCH`. No key (see [The link key](#the-link-key-toolslinkkey)).

**Tasks.** `radio::run` drives the LR2021 (configurations and checks from `lr2021cfg`, hops from
`aerohop`, control-link scans from `control.rs`); `usb_task`; the hub link (`uart_tx_task` +
`uart_rx_task`, `bus_task` on an interrupt executor, or the ESP-NOW `uplink`). `main` sends
`Hello` and `Status`, then once a second sends `Stats`, prints `stats` and `ctrl_stats` JSON lines
and feeds the watchdog. The watchdog is deliberately not fed from the radio task, which may stay
inside one plan entry for its whole length.

**Memory.** A 72 KB heap, for the Wi-Fi driver's buffers in `espnow` builds; the rest of the node
allocates nothing.

**Console.** USB-Serial-JTAG, output only; input only with `faulttest`.

**Proof.** **Built, not yet tried.** No LR2021 hardware has been in hand; "Never flashed by any
script in this repository yet" (`.cargo/config.toml`).

## node-sdr

**What it is.** A XIAO ESP32-C5 used as a raw radio sampler. The radio's internal debug bus is
looped through eight pads into PARL_IO as a continuous 40 MS/s ring, and the dump engine takes
10-bit snapshots. Two jobs, chosen by the hub: **DroneID** (walk the DJI DroneID frequencies and
decode bursts with `crates/droneid`, following O4 drones' hops) and **FPV** (walk 15 analog video
channels at 2.4 GHz and 31 at 5.8 GHz and classify each with `crates/fpvscan` and `crates/djilink`).
Every dwell is also judged for jamming (`crates/jamwatch`). Receive-only while capturing; the
`espnow` uplink transmits only in short windows between captures. See
[README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/node-sdr/README.md),
[DJI DroneID](../detect/dji-droneid.md) and [O4 DroneID in depth](../DRONEID-O4.md).

| Item | Value |
|---|---|
| Chip and target | ESP32-C5 only, `riscv32imac-unknown-none-elf`; `build.rs` stops any other target |
| Toolchain | `stable` with `rust-src` (`c5/rust-toolchain.toml`); build from `c5/` |
| Aliases | `cargo fw` = `build --release`; `cargo fw-lite` = `build --release --no-default-features` (no PSRAM decoder); `cargo fw-now` = `build --release --features espnow --target-dir ../target-espnow`; `cargo fw-bus` = `build --release --features wirebus --target-dir ../target-bus` |
| `[env]` | `ESP_HAL_CONFIG_ENSURE_MAIN_STACK_MINIMUM = "40000"` (the decoder runs about 30 KiB deep) |
| Linker | `ld/sdr_bank.x` (inserted before `linkall.x`), `--wrap=phy_chip_set_chan_ana`, and a map file `node-sdr.map` next to the ELF |
| Profile | `opt-level = 3` |

**Features.** `default = ["decode"]`.

| Feature | Bench only | Meaning |
|---|---|---|
| `decode` | | Build the DroneID receiver (about 125 KiB) into PSRAM and decode every triggered burst. Needs a C5 with PSRAM (the XIAO's C5HR8). Without it the node captures, triggers and dumps only |
| `espnow` | | Report through the gateway: between captures the node pauses, retunes to channel 6, sends, listens briefly for a command, then re-locks the receiver. The only way this firmware transmits |
| `wirebus` | | The wired bus (TX open-drain D9 = GPIO9, RX D7 = GPIO12). Nothing transmitted on the air; capture never pauses to send. Not with `espnow` |
| `faulttest` | yes | Console `fault panic` / `fault hang` |
| `bringup` | yes | The bring-up and test console commands (below) |

**Build-time inputs** (`build.rs`):

| Input | Effect |
|---|---|
| The `libphy.a` in `esp-wifi-sys-esp32c5 =0.3.0` | Its SHA-256 must be `dbf33c41…04fffb`, the blob whose private ABI `src/phy.rs` was checked against (ESP-IDF 6.0.2's C5 PHY, version `edb400d6`). Any other blob stops the build. Found under `CARGO_HOME` (or `~/.cargo`); if not found, a warning, and the run-time version check still applies |
| `NODE_SDR_ALLOW_UNVERIFIED_PHY` | Bench switch: build with another blob anyway, loudly |
| `NODE_SDR_BOOT` | Bench switch: console commands baked in to run at boot, such as `"level;grab;dump"` |
| `ROTORSCOPE_BENCH` | The bench fence |
| Link key | none: `espnow` builds read the node's key record from `rskey` at run time |

At boot `phy::abi_ok()` checks the blob's version string again; on a mismatch the firmware refuses
every private PHY call and says so.

**Tasks.** `main` starts Wi-Fi (only to power and calibrate the PHY; promiscuous with an empty
filter, transmit queues stopped and read back), builds the DroneID receiver in PSRAM, boots the
radio (PHY locked receive-only, dump engine free-running, debug bus routed, ring running), starts
the `wirebus` task on a priority-2 interrupt executor if built, spawns the `console` task and the
`capture_loop` task, then sleeps. The capture loop runs as its own task so its state is static:
inline in `main`, its future took 20 KiB of stack and the decoder overflowed about 12 KiB into the
capture bank (found by the HIL test, 2026-11-14). The capture loop feeds the watchdog every pass.

**Memory** (README "Memory", from `cargo fw`). Internal SRAM 0x40800000..0x4085e5a0 (378 KiB
counting the 64 KiB the bootloader frees):

| Region | Size | Holds |
|---|---|---|
| Block 0 (128 KiB) | 75.8 KiB | code in RAM, Wi-Fi code, `.data`, `.bss` |
| | 52.2 KiB | heap (the gap below the bank) |
| Block 1 (128 KiB) | 56 KiB | Path B ring at 0x40820000 (`sdr_bank.x` fails the link if it is anywhere else) |
| | 8 KiB | 16 DMA descriptors |
| | 64 KiB | capture buffer: Path A target, or the decoder's input |
| Block 2 to 0x4084e5a0 | 57.4 KiB | main stack |
| dram2 | 64 KiB | heap (64 KiB reclaimed from the bootloader) |
| PSRAM | 120.8 KiB | the DroneID receiver |

Block 1 belongs to the radio: the dump engine can only own it. Never-touched stack after decodes
was about 6.9 KiB on 2026-11-14 and 4.7 to 5.5 KiB in `bringup` builds on 2026-11-18. **Nothing
catches a stack overflow at run time** here: the deepest decoder frame steps over esp-hal's guard
word, and below the stack is block 1. FPV analysis scratch (about 13.6 KiB) and DJI-link scratch
(about 12.6 KiB) come from the heap.

**Console** (`src/console.rs`, `crates/rawcap/src/cmd.rs`). USB-Serial-JTAG, one command per line.
JSON lines out, plus checksummed dump lines (`@H`, `@D`, `@E`). Serials, UUIDs and O4 key IDs are
redacted unless `reveal on`.

| Build | Commands |
|---|---|
| Every build | `help`, `stats`, `phy`, `plan`, `jam`, `uplink [now]`, `fpv verbose on\|off` |
| `faulttest` | `fault panic`, `fault hang` |
| `bringup` only | `freq <MHz>`, `offset <kHz>`, `gain <idx>`, `lpf <0-63>\|off`, `dcal [ref \| auto on\|off]`, `win clip\|fold5\|fold`, `edge rise\|fall`, `pins xiao\|alt`, `swap on\|off`, `trig <dB> [min_us max_us]`, `mode detect\|hold\|idle`, `snap`, `grab`, `level`, `decode`, `dump [raw\|c16] [b64\|hex]`, `resend <first> [last]`, `reveal on\|off`, `resume`, `bootloader`, `role droneid\|fpv\|auto`, `hop on\|off`, `edge on\|off` (the FPV off-plan channel), `assign <role> <id> [f:ms ...]`, `@F <offset> <base64>`, `hil run <len> <crc32>`, `hil train ...`, `hil forge [<secs> [<case>]]` |

`freq` is refused outside 2200-2700 and 5150-5950 MHz.

**Proof (as of 2026-11-18).** Both jobs run on air on hub assignment on two C5 boards
([BRINGUP-RESULTS.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/node-sdr/BRINGUP-RESULTS.md)).
DroneID decoding: **proven on recordings** (public recordings, also through the chip's live path
with `tools/droneid-hil`); no real DJI drone has been heard. FPV classification: **simulated only**
on synthetic signals; never heard from a real video transmitter. DJI link fingerprints: **proven on
recordings** on a computer only, "not yet run on a chip" (README). Jamming watch: **simulated
only**, not yet run on a chip.

## node-wifi

**What it is.** The Wi-Fi Remote ID listener: promiscuous mode, Remote ID beacons and NAN action
frames, DJI legacy Wi-Fi DroneID elements, drone makers' networks, open digital FPV links (wfb-ng,
OpenHD, RubyFPV, DroneBridge, by header only) and Wi-Fi attacks (deauth floods, fake networks,
evil twins and more; [Wi-Fi and Bluetooth attacks](../WIFI-ATTACKS.md)). Node A is 2.4 GHz, Node C
is 5 GHz; one C5 image does both, the hub picks the band.

| | `s3/` (Node A) | `c5/` (Node C, or Node A with `band24`) |
|---|---|---|
| Target | `xtensa-esp32s3-none-elf` | `riscv32imac-unknown-none-elf` |
| Toolchain | `esp` (needs `. ~/export-esp.sh`) | `stable` with `rust-src` |
| Aliases | `cargo fw` = `build --release --features esp32s3` | `cargo fw` = `build --release --features esp32c5`; `cargo fw24` = `build --release --features esp32c5,band24 --target-dir ../target-band24`; `cargo fw24-now` = `build --release --features esp32c5,band24,espnow --target-dir ../target-espnow`; `cargo fw24-bus` = `build --release --features esp32c5,band24,wirebus --target-dir ../target-bus` |
| Other | `build-std = ["alloc", "core"]`, `-nostartfiles` | `-C force-frame-pointers` |

`build.rs` stops a build whose chip feature does not match the target. `rust-version = "1.88"`.
`tools/flash-node.sh` puts the `fw24` family into slot `ota_0` of a multi-image node.

**Features.**

| Feature | Bench only | Meaning |
|---|---|---|
| `esp32s3`, `esp32c5` | | Exactly one: the chip |
| `band24` | | With `esp32c5`: boot as a 2.4 GHz node (channels 6/1/11, role Wifi24). Only the boot default: the hub's assignment picks the band |
| `espnow` | | Send hub frames over ESP-NOW to the gateway; 2.4 GHz only. A 5 GHz plan gets 40 ms visits to channel 6 at least once a second. The node then transmits |
| `wirebus` | | The wired bus, TX open-drain D9 = GPIO9, RX D7 = GPIO12. Receive-only on the air. C5 only; not with `espnow` |
| `faulttest` | yes | `fault panic` / `fault hang` on USB |
| `forgetest` | yes | `forge hubfix\|csbee\|flags\|via\|hello\|threat\|join` on USB sends a frame a node must never originate, to see the hub refuse it (before protocol 14 the gateway refused it first) |
| `floodtest` | yes | `flood <kind> <rate>` (`rid`, `tool`, `drone`, `dji`, `wfb`, `spoof`, `retry`, `dup`, `hijack`), `flood real on\|off`, `flood crowd <n>`, `flood off`: synthetic frames pushed into the node's own receive path. Transmits nothing |

`faulttest`, `forgetest` and `floodtest` all read the USB console, so the firmware allows only one
of them at a time.

**Build-time inputs.** `ROTORSCOPE_BENCH`; bench switches `ROTORSCOPE_BENCH_CAN` (as for
node-ble) and `ROTORSCOPE_BENCH_FLIP` (here: leave Wi-Fi 2.4 out of `can`). No key: with `espnow` the
node reads its key record from `rskey` at boot.

**Tasks.** The Wi-Fi driver's promiscuous callback copies management frames into 32 static slots
(a full queue is counted, never waited on), classifies data frames on the spot for open links and
counts header-only frames (deauths, probes) for flood detection. `drain` decodes Remote ID and runs
the beacon checks; `stats`, `threats`, `open_links`, `mavlink` (hostile MAVLink on open Wi-Fi, only
with the hub's opt-in) and `role::control` run alongside; `usb::writer` never blocks. The hub link
is `report::hub_writer` + `hub_reader` (wired), `bus_task` on a priority-2 interrupt executor
(`wirebus`), or `uplink` (`espnow`). `main` ends as the channel hopper, feeding the watchdog at
every hop and scouting one off-plan channel every 3 s.

**Memory.** Heap reclaimed from the bootloader: 73,744 bytes (S3) or 65,536 (C5).

**Console.** USB-Serial-JTAG, output only (lines such as `hello`, `stats`, `rid`, `open`, `dji`,
`wifidrone`, `threat`, `err`). USB input is read only by the bench features above.

**Proof.** C5: **proven on the air** from a C5 `rid-beacon`: 5 GHz beacon and NAN on channel 149
(2026-11-12), and fake open FPV links on channels 6, 149, 161 and 165 (2026-11-14). S3: **proven on
the air**, 2.4 GHz beacon and NAN from a C5 `rid-beacon` in the mixed-chip bench tests
(2026-11-12). DJI Wi-Fi and makers' drone networks: **simulated only**, never seen from a real
drone. Attack detection: **simulated only**. Wired bus: **simulated only** (never wired).

## rid-beacon

**What it is.** The bench test transmitter: the known signal every receiver is tested against. It
sends Remote ID by Wi-Fi beacon, Wi-Fi NAN, Bluetooth 4 legacy, Bluetooth 5 extended and Long
Range, with an obviously fake identity (`TESTDRONE0000000001`), the lowest power the libraries
accept, and a simulated 50 m circle. It can also send fake open FPV link frames (IDs starting
`7E57`), off at boot, and never anything DJI. See
[README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/rid-beacon/README.md).

| Item | ESP32-S3 (default) | ESP32-C5 |
|---|---|---|
| Target | `xtensa-esp32s3-none-elf` | `riscv32imac-unknown-none-elf` |
| Toolchain | `esp` | `cargo +stable` overrides it |
| Build | `cargo build --release` | `cargo +stable build --release --target riscv32imac-unknown-none-elf --no-default-features --features esp32c5` |
| Wi-Fi channel | 2.4 GHz channel 6 | 5 GHz channel 149 by default |

No cargo alias. `rust-version = "1.95"`. `.cargo/esp-config.toml` sets `ESP_LOG="info"`. It
**patches esp-radio** with `firmware/patches/esp-radio` through `[patch.crates-io]`.

**Features.** `default = ["esp32s3"]`; `esp32s3` and `esp32c5` choose the chip. No other features,
and no bench fence (the whole firmware is a bench tool).

**Build-time inputs** (`build.rs`, read with `env!`/`option_env!`):

| Variable | Default | Meaning |
|---|---|---|
| `RID_TEST_LAT`, `RID_TEST_LON` | 39.9526, -75.1652 (Philadelphia City Hall) | Centre of the simulated circle and the operator position |
| `RID_BEACON_HZ` | 1 | Beacons a second: 1, 2, 5 or 10 (use 10 to see it from a laptop scan) |
| `RID_BLE_DISCOVERABLE` | unset | `1` adds a plain legacy advert so BlueZ shows the device; a verification aid only |

**Tasks.** `wifi_task` (raw 802.11 frames through the sniffer interface: beacon, NAN, fake open
links), `console_task`, `summary_task` (one `tx` JSON line a second), and Bluetooth advertising
(three advertising sets through trouble-host) in `main`. `src/hci_fix.rs` works around a bt-hci
0.10.1 encoding bug that breaks extended advertising data.

**Memory.** Heap: 73,744 (S3) or 65,536 (C5) bytes reclaimed, plus 64 KB.

**Watchdog and panic.** Reboots 0.5 s after a panic; no hang watchdog.

**Console** (`src/control.rs`). USB-Serial-JTAG, single characters, no Enter, case ignored:
`w` beacon, `n` NAN, `l` BLE 4, `e` BLE 5 extended, `c` BLE Long Range (each toggles), `a` all on,
`x` all off (fake links too), `b` C5 only: 149 or channel 6, `f` / `o` / `r` / `d` fake wfb-ng /
OpenHD / RubyFPV / DroneBridge link on or off, `6` channel 6, `7` / `8` / `9` C5 only: channel
149 / 161 / 165, `?` / `h` help.

**Proof.** It transmits; what matters is that receivers hear it. A second C5 heard all five C5
transmitters (2026-11-12); the laptop heard the S3's Wi-Fi beacon and Bluetooth 4, and a C5 heard
the S3's Bluetooth 4, 5 extended and Long Range, and (built `cargo fw24`) its 2.4 GHz Wi-Fi beacon
and NAN (2026-11-12).

## patches

`firmware/patches/` holds third-party code the firmware needs changed. See its
[README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/patches/README.md).

### `esp-hal-p4-pre-v3/`: esp-hal for the P4's v1.3 silicon

**Why.** Upstream esp-hal supports ESP32-P4 silicon v3.0 and newer only; the CrowPanel's chip is
v1.3, with a different interrupt controller (no `mintthresh` CSR, a memory-mapped threshold), clocks
and ROM.

**What is there.**

| File | What it is |
|---|---|
| `manifest.json` | The upstream esp-hal repository and pinned revision `e02f3613e9f9ba1ce00070eb387e3bf4fde2267b`, the patch's SHA-256, the crates it covers (`esp-hal`, `esp-backtrace`, `esp-bootloader-esp-idf`, `esp-println`), and the SHA-256 of each `rotorscope-*.patch` (`local_patches`) |
| `esp32p4-pre-v3.patch` | The community compatibility patch from naoNao89's esp32p4-pre-v3-kit 0.1.3 (about 1,700 lines). Among other things it derives a `esp32p4_rev_lt_v3` cfg from the configured minimum chip revision, since the wrong CSR access would trap before the revision can be read, and adds the pre-v3 ROM linker tables |
| `rotorscope-pre-v3-memory.patch` | The project's own fix (20 lines) to `esp-hal/ld/esp32p4/memory.x`: RAM starts at 0x4FF40000 with length 0x80000 minus the L2 cache size, ESP-IDF 5.4.2's pre-v3 split. The kit's layout had put RAM and the stack inside the L2 cache, which sits at the top of L2 memory on pre-v3 chips; a 32 MB PSRAM sweep then overwrote the stack (found 2026-11-14). BRINGUP.md notes it should go upstream to the kit, after asking the owner |
| `PROVENANCE.md` | Where the ROM tables come from: copied from ESP-IDF (two pinned commits) with SHA-256 for each, plus one local adaptation (`additional.ld`) and the esp-rs boilerplate files |
| `LICENSE-MIT`, `LICENSE-APACHE` | The kit's licences (MIT/Apache-2.0). The ROM tables are Apache-2.0, as in ESP-IDF |

**How it is applied.** `tools/p4-prepare.sh` checks the kit's patch and every
`rotorscope-*.patch` against the SHA-256 sums in `manifest.json` (`local_patches` lists the
project's own, in order), fetches that one esp-hal commit by its full id into
`firmware/patches/esp-hal-p4-pre-v3/upstream/` (git-ignored), checks the checkout is that commit,
applies the patches and verifies the result. The kit's own tool is not run; the script does the
same steps in the open. `tools/p4-prepare.sh --verify` checks a prepared tree offline;
`--update-sums` records a changed patch's checksum ([tools](tools.md#p4-preparesh)). `hub-p4/Cargo.toml` takes its esp-* crates from that `upstream/` directory by path.

### `esp-radio/`: ESP-NOW receive length check and the C5 RF clock fix

**What it is.** A verbatim copy of esp-radio 1.0.0-beta.1 from crates.io with two files changed:
`src/esp_now/mod.rs` and `src/radio_clocks/clocks_ll/esp32c5.rs`. Licence as upstream esp-radio
(MIT or Apache-2.0). Carried by every radio firmware: `gateway`, `node-wifi`, `node-ble`,
`node-sdr`, `node-lr`, `hub` (the S3 web head) and `rid-beacon`. `hub-p4` has no radio.

**Why: the ESP-NOW receive length.** Upstream turns the radio library's received length into a
slice length with no check. The underlying bug (CVE-2025-52471) is fixed in the Espressif radio
libraries this repository pins, which `tools/check-wifi-blobs.sh` checks by their hashes; the patch
is a second line that drops a zero, negative or oversized length (or a null pointer) before it
becomes a slice, and counts it (`esp_radio::esp_now::bad_rx_len_drops()`, which should stay 0).
Compile-checked on every radio firmware; not testable on the bench without sending malformed
frames.

**Why: the C5 RF clocks.** On the ESP32-C5 the RF front-end clocks are shared by Wi-Fi, Bluetooth
and 802.15.4. ESP-IDF reference-counts them; upstream esp-radio's `enable_wifi(false)` clears them
unconditionally, and the Wi-Fi blob calls it whenever Wi-Fi goes idle. With Wi-Fi started, the
Bluetooth controller then accepted every command and reported advertising as running, but nothing
was transmitted. The fix keeps a three-bit "who needs the front end" mask and turns the clocks off
only when no user is left. Evidence (2026-11-12, a XIAO ESP32-C5): the clock register read back
with the front-end bits clear while Wi-Fi ran; with the patch the bits stay set and the laptop
decodes the beacon's Bluetooth while Wi-Fi beacons run.

**How it is applied.** `[patch.crates-io] esp-radio = { path = "../patches/esp-radio" }` in each
radio firmware's `Cargo.toml`. `tools/check-wifi-blobs.sh` fails if the copy differs from the
crates.io release anywhere else, or if a lockfile does not take it. When to remove each change is
in the patches README.

## Where this lives in the code

- `firmware/*/Cargo.toml`: features, with a comment on each
- `firmware/*/.cargo/config.toml` and `firmware/*/{s3,c5}/.cargo/config.toml`: aliases, targets, `[env]`
- `firmware/*/rust-toolchain.toml` and `firmware/*/{s3,c5}/rust-toolchain.toml`
- `firmware/*/build.rs`; `crates/link/src/benchfence.rs`
- `firmware/node-common/src/keystore.rs`, `linkauth.rs` (a node's key record and link);
  `firmware/hub-p4/src/linkauth.rs`, `links.rs` (the hub's); `tools/partitions-*.csv`
- `firmware/node-common/src/fault.rs`; `firmware/hub-p4/src/fault.rs`
- `firmware/hub-p4/src/main.rs` (the main loop and console commands), `src/console.rs`, `src/report.rs`
- `firmware/node-sdr/README.md` "Memory"; `firmware/hub/README.md` "Design notes"
- `firmware/patches/`, `tools/p4-prepare.sh`, `tools/flash-node.sh`

**Next:** [Tools reference](tools.md)
