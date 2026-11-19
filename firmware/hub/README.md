# hub: merges the nodes' detections and serves them

Bare-metal Rust (esp-hal 1.2, esp-radio 1.0.0-beta.1, embassy, embassy-net) on an ESP32-S3.
It works **without PSRAM** (the board on the bench has none, and 4 MB flash).

The hub does these things:
- It reads up to three radio nodes over UART.
- It merges their Remote ID detections into tracks (`crates/tracker`).
- It lists open digital FPV / telemetry links the Wi-Fi nodes recognise (wfb-ng/OpenIPC, OpenHD,
  RubyFPV, DroneBridge), separately from drones, with their identifiers hidden by default.
- It runs a Wi-Fi hotspot with a phone page, and sends TAK CoT multicast.
- It streams JSON lines on USB.
- It can also join an existing network as a fixed sensor.
- It reads a u-blox MAX-M10S GPS (own position, UTC, PPS), drives a 320x240 screen with three
  buttons and a buzzer, logs CSV to an SD card, and measures the battery.

Every one of the last group is optional hardware. With none of it wired (the bench board today) the
hub reports GPS `absent`, SD `no_card`, power `usb`, draws to a panel that is not there, and
everything else works exactly as before.

## Build

The chip is chosen by the directory you build from, as with the nodes.

```sh
# ESP32-S3 (Xtensa: Espressif's `esp` toolchain from espup)
. ~/export-esp.sh
cd firmware/hub/s3
cargo fw                              # the real hub
# Bench builds (refused unless ROTORSCOPE_BENCH=1, see below):
ROTORSCOPE_BENCH=1 cargo fw --features demo              # + two synthetic drones, no nodes needed
ROTORSCOPE_BENCH=1 cargo fw --features demo,open-hotspot # + hotspot with no password
#   -> firmware/hub/target/xtensa-esp32s3-none-elf/release/hub

# ESP32-C5 (RISC-V: stable Rust). Kept compiling; two node links only.
cd firmware/hub/c5 && cargo fw
#   -> firmware/hub/target/riscv32imac-unknown-none-elf/release/hub
```

`cargo fw` is `cargo build --release --features esp32s3` (or `esp32c5`).

**Bench builds are fenced off.** `demo` (fake drones, a made-up own position, the build machine's
clock) and `open-hotspot` (no password) make a unit unfit for use, so `build.rs` stops any build with
either of them unless the environment has `ROTORSCOPE_BENCH=1` (the shared fence every Rotorscope
firmware uses, `crates/link/src/benchfence.rs`). Such a build says so: a cargo warning,
`"bench":["demo",...]` in the USB boot line followed by a
`{"t":"warn","what":"bench_build","tag":"bench-build:demo",...}` line, `"bench"` in `/api/state`,
which the phone page shows as a striped **BENCH BUILD, NOT FOR USE** banner at the top of every
view, and the `bench-build:` tag in the ELF, for which `tools/dev-board.sh` refuses to flash it
without `ROTORSCOPE_BENCH=1`.

Flash and watch through the shared lock. Never call espflash directly:

```sh
tools/dev-board.sh firmware/hub/target/xtensa-esp32s3-none-elf/release/hub 60
```

As the P4 hub's **web head** (hotspot, phone page, CoT and station mode for the P4, which has no
radio): `cargo fw --features webhead`, with P4 GPIO31 wired to node A RX (GPIO4) plus ground. See
`firmware/hub-p4/WEBHEAD.md`. Every build understands a relayed link (a P4 hub; an ESP-NOW gateway
only up to link protocol 13: a protocol 14 gateway speaks `link::gw`, which only the P4 reads):
`Via` markers keep up to 8 nodes apart (`{"t":"relayed"}` USB lines, phone node list), the
P4's GPS (`HubFix`) becomes the own position and UTC when this board has no GPS of its own, and
protocol 4 frames reach the tracker; `webhead` adds the aircraft table for forwarded ADS-B lines.

**Web head trust.** Each link reader sorts its frames by the one table of who may send what
(`link::origin`, docs/ESPNOW.md "Who may send what"), as the P4 does. `HubFix` (the
kit's own position and clock), `Csbee` (aircraft) and `Flags` (FAA flags) describe the P4 itself,
so they are applied only when they come from the relay itself, never when a `Via` marker says the
relay passed them on for one of its nodes. A relayed frame must be one a node may send (a relayed
gateway `Hello` or `ForgedLink` report, or a bus-control frame, is refused), and a `Via` straight
after a `Via` is refused with the one it follows. In a `webhead` build the P4's frames are taken
only on link 0 (node A RX), so a node wired straight to link 1 or 2 cannot send them; a build
without `webhead` takes them on any link. Refused frames count in the `hub` line's `relay_refused`.

Other features: `ili9341` (an ILI9341 panel instead of the default ST7789), `gps-uart` (GPS on
UART2 instead of I2C; costs node link 2), `adsbee` (an ADSBee 1090 on UART2; costs node link 2;
see "ADS-B" below), and `show-link-ids` (below). A rebuild with nothing changed takes well under a second:
`build.rs` no longer watches a file that does not exist (see "Station mode").

## Pins (placeholders until the PCB exists)

All pins are defined in `src/pins.rs`. Change them there and nowhere else.

| Function | S3 GPIO | Notes |
|---|---|---|
| Node 0 RX / TX | 4 / 5 | UART0, Wi-Fi 2.4 GHz node (A). TX is reserved; nodes only talk to the hub today |
| Node 1 RX / TX | 6 / 7 | UART1, Bluetooth node (B) |
| Node 2 RX / TX | 15 / 16 | UART2, Wi-Fi 5 GHz node (C). Not used in a `gps-uart` or `adsbee` build |
| ADSBee RX | 41 | `adsbee` builds only: UART2, from the ADSBee 1090's COMMS_UART TX. Free on N16R8 modules and the DevKitC header |
| GPS I2C SDA / SCL | 8 / 9 | MAX-M10S at 0x42. In a `gps-uart` build: 8 = hub RX, 9 = hub TX |
| GPS PPS | 2 | TIMEPULSE input, pulled down |
| SPI SCK / MOSI / MISO | 12 / 11 / 13 | Shared by the screen and the SD card |
| Screen CS / DC / backlight | 10 / 17 / 18 | Backlight is LEDC PWM |
| SD CS | 14 | |
| Buttons Up / Down / Select | 38 / 39 / 40 | Active low, internal pull-ups |
| Buzzer | 21 | LEDC tone, 2.7 kHz |
| Battery sense | 1 | ADC1. ADC2 can't be used while Wi-Fi runs |

Wiring a node: node TX goes to hub RX, plus a shared ground. Node A (`node-wifi`) transmits on GPIO43,
and Node B (`node-ble`) on GPIO17. All links run at 921600 baud.

The pin rules, so the choices also suit a PSRAM hub module (N16R8-class):
- Avoid 26-37 (flash and octal PSRAM).
- Avoid 0, 3, 45 and 46 (strapping pins).
- Avoid 19 and 20 (USB).
- Avoid 43 and 44: the ROM bootloader prints there at reset, and that output would go to a node.

The C5 has only two HP UARTs, so a C5 hub has links 0 and 1 only (and no `gps-uart`). esp-hal 1.2
has no LEDC driver for the C5, so there the backlight is only on or off and the buzzer must be an
active one; and with one core, the screen and SD share it with everything else. Its buttons sit on strapping pins
(see `pins.rs`).

The console is USB-Serial-JTAG (the USB-C port), so all three UARTs are free for nodes.

## Wiring the optional parts

Everything below is 3.3 V logic; share ground with the hub. Pin numbers are the S3 placeholders above.

| Part | Its pin | Hub pin | Notes |
|---|---|---|---|
| **GPS** (u-blox MAX-M10S breakout) | SDA | GPIO8 | I2C at 100 kHz. Pull-ups to 3V3 (most breakouts have them; the hub's internal ones are weak) |
| | SCL | GPIO9 | |
| | TIMEPULSE (PPS) | GPIO2 | Optional but wanted: sub-millisecond UTC |
| | D_SEL | GND | Selects I2C on the MAX-M10S (open = UART). Check the breakout's silkscreen |
| | VCC / GND | 3V3 / GND | A backup battery (V_BCKP) keeps the configuration and speeds up the next fix |
| **GPS on UART** (`gps-uart`) | TXD / RXD | GPIO8 / GPIO9 | Hub RX / hub TX. Takes node link 2 |
| **ADSBee 1090** (`adsbee`) | TX (COMMS_UART) | GPIO41 | 115200 8N1, 3.3 V. Plus GND. Its RX stays unconnected: it takes no commands on this port. Takes node link 2. Setup: `docs/ADSBEE.md` |
| **Screen** (ST7789 2.0" or ILI9341 2.8", 320x240 SPI) | SCK / SDA (MOSI) | GPIO12 / GPIO11 | 40 MHz |
| | CS / DC | GPIO10 / GPIO17 | |
| | RST | 3V3 (or RC to 3V3) | Not driven: the hub sends a software reset |
| | BL / LED | GPIO18 | PWM. Through a transistor if the module draws more than ~20 mA |
| | VCC / GND | 3V3 / GND | |
| **SD card** (SPI microSD socket) | SCK / MOSI / MISO | GPIO12 / GPIO11 / GPIO13 | Shares the screen's bus. 400 kHz to start, then 20 MHz |
| | CS | GPIO14 | |
| | VCC / GND | 3V3 / GND | FAT16/FAT32, MBR partition 1 |
| **Buttons** | one side | GPIO38 / 39 / 40 | Up, Down, Select (hold Select = Back). Other side to GND |
| **Buzzer** | + | GPIO21 | A *passive* piezo (driven with a tone). Through a transistor for a louder magnetic one |
| **Battery** | divider mid-point | GPIO1 | Divider from the cell (+) to GND; ratio is `panel::BATTERY_DIVIDER` (placeholder 2.0, two equal resistors, 100k+100k) |

Two things the PCB should add: a **card-detect** switch on the SD socket (the hub has to probe the
bus today), and a **VBUS sense** pin (today "on USB" is only inferred from the battery voltage).

## What each status means

All of these are in the `{"t":"hub"}` USB line; `gps` and `rec` are also in `/api/state`, and the
screen's status bar and Health page show them.

| Status | Values | Meaning |
|---|---|---|
| `gps.state` | `absent` | Nothing answered at I2C 0x42. `gps.why` says how: `nack` (no device) or `bus` (bus error or timeout: wiring, missing pull-ups). Checked again every 3 s, so a GPS plugged in later is found. Screen: inverse **NO GPS** |
| | `searching` | Receiver present, no fix yet (or the fix is not trustworthy). Screen: **NO FIX** |
| | `2d` / `3d` | Fix, with `sats` used. Only a fix that `gnss::Fix::is_usable()` accepts becomes the own position |
| `gps.cfg` | `none` / `running` / `ok` / `nak` / `no_ack` | The boot configuration (NAV-PVT on, NMEA off, PPS on). `nak`/`no_ack` name the step in a `warn` line; NMEA still works without it |
| `gps.pps_n`, `pps_age_ms` | | PPS edges seen, and the age of the last. No edges = no GNSS time lock (the receiver only pulses when locked) |
| `own.source` | `gps` / `manual` | GPS wins; a manual position is used only while there is no usable fix younger than 10 s. Screen: `GPS 9`, or `POS MAN` for a typed position |
| `utc_src` | `gps_pps` / `gps` / `build` / null | UTC anchored to a PPS edge (sub-ms), to NAV-PVT arrival (up to a few hundred ms late), the demo build's clock, or none (CoT skipped) |
| `sd.state` | `starting` | Not looked yet (first second after boot) |
| | `no_card` | Nothing answered in the slot. Probed every 3 s. Screen: small "no SD"; `rec` false |
| | `recording` | File open, rows going in. `rec` true; screen **REC** |
| | `error` | A card answered but could not be used (not FAT, unreadable) or a write failed. Retried every 15 s; details in a `warn` line. Screen: inverse **SD!** |
| `sd.file` | `MMDDhhmm.CSV` / `LOGnnnnn.CSV` | This boot's file: named from GPS UTC when known at the time, else the next sequence number |
| `sd.rows`, `dropped` | | Rows queued for the card, and rows dropped because the card fell behind |
| `batt` | `src`: `usb` / `battery`, `pct`, `mv` | **Uncalibrated placeholder.** Below 2.5 V or above 4.3 V at the cell the hub assumes it is on USB with no cell |
| `screen` | `frames`, `render_us`, `flush_us`, `feed_us`, `core1_stack_free` | Frames drawn; the last frame's drawing time (all bands) and SPI time; time to build the view on core 0; untouched bytes of the second core's stack |

## Open FPV links and `show-link-ids`

Node A/C report open FPV / telemetry links as `link::Frame::OpenLink` sightings (one per stream per
second at most). The hub keeps them in the tracker's separate link table (8 links, see
`crates/tracker/README.md`), lists them in `/api/state` (`open_links`) and on the phone page's List,
sends a `{"t":"link",...}` USB line for each every second, and logs active ones to the SD card as
`L<n>` rows. They have no position: no CoT, no radar dot, no buzzer.

**Identifiers are hidden unless the firmware is built with `--features show-link-ids`.** That covers
the wfb-ng link ID, RubyFPV vehicle ID and DroneBridge comm ID, and those links' transmitter
addresses (wfb-ng's embeds the link ID), on every output. This is a project decision: seeing *that* a
wfb-ng link is nearby is for anyone holding the unit; *whose* it is takes the builder's opt-in. There
is no runtime settings store yet, so it is a build-time switch (`hub::SHOW_LINK_IDS`); the boot line
reports `"show_link_ids"`. Video and MAVLink content is never decoded.

## Hotspot

| | |
|---|---|
| SSID | `Rotorscope-XXXX`: the last two bytes of the chip's MAC, in hex (bench board: `Rotorscope-A1B2`) |
| Channel | **11**. Remote ID lives on channel 6, where Node A spends about 75% of its time, so the hub (the only transmitter in the box) stays as far from it as 2.4 GHz allows |
| Security | WPA2-PSK with the unit's own password (below). With `--features open-hotspot` (a bench build) it is open; that build prints a warning on USB at every boot and the phone page shows the bench banner |
| Address | `192.168.4.1/24`. DHCP hands out .50-.57 (8 addresses, 10-minute leases), with the hub as router and DNS |
| Clients | Up to 4 phones |
| TX power | esp-radio's default, 5 dBm. Low on purpose: the nodes' antennas are centimetres away |

### Password scheme

Each unit has its own password, for example `7k2m-q9xw-4thc`: 12 characters in 3 groups (60 bits),
lower-case Crockford base32 (no i, l, o or u, so nothing is misread as 1 or 0).

```
SHA-256("rotorscope hotspot password v2" || salt (16 bytes) || OPTIONAL_UNIQUE_ID eFuse (16 bytes) || base MAC (6 bytes))
  -> first 60 bits -> 12 symbols
```

The **salt** is what makes the password secret. It is 16 random bytes, made on the unit's first boot
from the chip's true random number generator (on the S3: the hardware RNG with the ADC as its
entropy source, before Wi-Fi starts; on the C5, which has no such source in esp-hal 1.2, the radio
is started first without the hotspot) and stored in flash. The MAC is broadcast in every beacon,
this code is public, and the unique-ID eFuse is blank (all zero) on some chips, so without the salt
anyone nearby could work the password out. The ID and MAC stay in the hash so that a whole flash
image copied to another board still gives that board a different password. Code and tests:
`crates/webui/src/hotspot.rs`.

**Existing units get a new password once.** Units flashed before this change used
`"…password v1" || ID || MAC`; their first boot on this firmware makes a salt, and from then on the
password is the new one. Read it as below.

**Where the salt lives.** The last 4 KB sector of the `nvs` partition: 0xE000 with the partition
table espflash writes by default (`nvs` 0x9000-0xEFFF, `phy_init` 0xF000, `factory` app from 0x10000
to the end of the flash, so there is no free space after the app). The hub finds the sector by
reading the partition table at boot, one entry at a time. Nothing in this firmware uses NVS:
esp-radio 1.0.0-beta.1 runs the Wi-Fi blobs with NVS disabled and its NVS hooks are stubs. A normal
reflash (`tools/dev-board.sh`, `espflash flash`) writes only the bootloader, table and app, so the
password survives it; `espflash erase-flash` (or erasing `nvs`) gives the unit a new one. The record
is magic, version, salt and an 8-byte SHA-256 check, so a half-written or foreign sector is never
taken for a salt.

**If the salt cannot be read or stored** (no `nvs` partition, a flash error), the hotspot still never
comes up with a guessable password: it uses a fresh random salt for that boot only. The
`hotspot` line then says `"salt":"this_boot_only"`, an `err` line says why, and the password changes
at every boot until the flash is fixed. That is meant to be noticed.

**Reading the password: hold BOOT, press `h`.** USB is the only place to read it (on a P4 kit the
screen belongs to the P4, which cannot know the web head's password). It is printed only while the
board's **BOOT** button (GPIO0) is held, so a cable alone is not enough: someone has to be at the box.
Press and hold BOOT *after* the hub has started (held during power-up or RESET, it starts the ROM
downloader instead), then type `h`:

```
{"t":"hotspot","ssid":"Rotorscope-A1B2","channel":11,"security":"wpa2","password":"…","salt":"stored",…}
```

Without the button the same line has no `password` field, and a `help` line says how to get it. The
boot output never contains the password (boot logs get pasted into issues). The C5 build, whose BOOT
pin drives the buzzer, uses the panel's Select button instead.

**A new password: hold BOOT, press `R`** (capital). The hub asks its next boot for a new salt (a
flag in RTC memory) and restarts at once; every phone is dropped and must join again with the new
password, which `h` (with BOOT held) then shows. Use it when the password has been shared too widely.
Each new password costs one erase of the salt sector.

`salt` in the `hotspot` line: `stored` (as last boot), `new` (first boot or after `R`; a `warn` line
says how to read it), `this_boot_only` (see above), `none` (`open-hotspot`).

## Station mode (fixed sensor, O5): a bench build for now

Station mode lets the hub join your own Wi-Fi network as well as running its hotspot. Today the
network name and password are compiled into the firmware image from a file, so anyone holding the
image, or a dump of the hub's flash, can read the password, and every site needs its own image.
Until the run-time record below exists, a build therefore takes the file only as a **bench build**
that says so, and refuses to build while the file is present without the bench switch.

1. Create `firmware/hub/wifi.local.toml` yourself (it is gitignored), with your network's name and
   password:

   ```toml
   ssid = "YourNetwork"
   psk  = "your-wifi-password"   # "" for an open network
   ```

2. Tell cargo the file is new: `touch firmware/hub/build.rs` (or build once with
   `HUB_WIFI_RESCAN=1`). Cargo cannot see a file appear by itself without recompiling the hub on
   every build; edits and deletion of an existing file are noticed automatically.
3. Build as a bench build, from `firmware/hub/s3`:

   ```bash
   ROTORSCOPE_BENCH=1 ROTORSCOPE_BENCH_STATION=1 cargo fw
   ```

   Cargo prints `warning: station mode ON (bench build: ...)`. The SSID and password are never
   printed. The image carries `bench-build:env:ROTORSCOPE_BENCH_STATION`, so `tools/dev-board.sh`
   flashes it only with `ROTORSCOPE_BENCH=1`, and the boot line lists it under `bench`.
4. Flash the hub. The boot line shows `"station":true`. Then `{"t":"sta","state":"up","ch":N}`
   appears once the hub has joined.
5. To turn station mode off, move the file out of `firmware/hub` and rebuild without the switch.
   While the file is there, a build without `ROTORSCOPE_BENCH_STATION=1` stops with an error, so
   a field image can never pick up the password by accident.

Do not share or field an image built this way: the password is inside it.

### Planned: the network set at run time (design, not built)

The image will carry no network details, so one image serves every site and an image on its own
gives nothing away:

- **Where the record lives.** One flash sector of the `nvs` partition, beside the hotspot salt
  (which uses the last sector). It holds a magic number, a version, the SSID (1-32 bytes), the
  passphrase (empty or 8-63 characters), `allow_writes`, and a CRC. No record means no station: the
  hub behaves as a build without station mode does today.
- **How it gets there.** Over USB only, with the **BOOT** button held, as for reading the hotspot
  password: a console command takes the SSID and the passphrase and writes the record, and another
  erases it. Nothing on the hotspot or the station network can set, read or change it. A host tool
  can also write the sector with `espflash write-bin`, as `tools/link-key` does for the nodes'
  `rskey`.
- **How the password is protected at rest.** Honestly: by the hub being in your hands. The record is
  in the hub's own flash, not in any image, log or SD card (the SD card leaves the box, so the
  card's `SITE.TXT` was ruled out). Anyone who can read that hub's flash over USB can read the
  password, as with any Wi-Fi device. Only Espressif's flash encryption (an eFuse setting that
  cannot be undone) protects against that; it is a separate decision for the kit. The USB console
  never prints the passphrase, even with BOOT held.
- **What the phone page shows.** Whether a station network is set, its SSID, joined or not, the
  channel, and the channel-6 warning. Never the passphrase, and no control to change it.
- **Boot line.** `"station":true` when a record is present, as now.

Not started. The current build-time path stays, as a bench build, until this exists and has been
tried on a board.

On the joined network the hub serves the same page and API on port 80 (one connection at a time). It
also sends CoT multicast there, and gets its address by DHCP.

**On someone else's network the hub is read-only by default.** Anyone on that network can open the
page and read `/api/state` (tracks, and the hub's own position), but `POST /api/own-pos` and
`POST /api/cot` answer `403 {"error":"changes are off on this network"}`. To let the LAN change them,
add `allow_writes = true` to `wifi.local.toml` (the build then warns
`station mode: the LAN may change ...`). The hotspot is unaffected. CoT multicast leaves with an IP
hop limit (TTL) of 1 on both networks, so a multicast router does not carry the positions further.

Not done (station mode): pinning the router's BSSID (another access point with the same name and
password would be joined just the same) and an access token for the API. Use station mode only on a
network you trust.

**AP+STA works**, through esp-radio's `Config::AccessPointStation`, and the hotspot stays up. A single
radio brings two limits:
- **The hotspot follows the router's channel.** If your router is on channel 6, the hub has to
  transmit on the Remote ID channel. The hub warns about this on USB. Put the router on channel 1 or
  11 if you can. (The S3 has no 5 GHz radio.)
- **Reconnecting disturbs the hotspot.** While the router is out of reach, each connect attempt scans
  every channel, and phones on the hotspot see short stalls. To limit this, retries happen only once
  every 30 s.

Station mode has not been run on hardware yet (**built, not yet tried**).

## Endpoints

| | |
|---|---|
| `GET /` | The phone page: `webui::INDEX_HTML_GZ` (about 16 KB), sent with `Content-Encoding: gzip`, an `ETag` and its Content-Security-Policy. If the phone's `If-None-Match` matches, the hub answers `304` |
| `GET /api/state` | The state object (schema in `crates/tracker/README.md`), sent with `Transfer-Encoding: chunked` and `Cache-Control: no-store`. This hub adds `"bench"` (bench features compiled in) and `"cot"` (the TAK switch) at the start of the object |
| `POST /api/own-pos` | Body `{"lat":..,"lon":..,"alt":..}` or `{"clear":true}`, checked with `json::parse_own_pos`. Answers `200 {"ok":true}`, or `400 {"error":"…"}` |
| `GET` / `POST /api/cot` | Read or set the CoT toggle: `{"on":true}` |
| anything else | `302` to `http://<the hub's address>/`. This catches every captive-portal probe (Android `/generate_204`, Apple `/hotspot-detect.html`, Windows, Firefox) |

**Which requests are answered** (`webui::request`, host-tested). A web page open on a joined phone
could otherwise switch the TAK feed off or move the position with a plain cross-site form POST, or
read `/api/state` (including the hub's own position) by pointing its own name at 192.168.4.1 (DNS
rebinding). So every route except the redirect checks:
- `Host` must be the hub's own address on that network: `192.168.4.1` (or `:80`), or the station
  address. Anything else gets `421 {"error":"wrong host"}`, except the page itself (`GET /`), which
  is redirected to `http://192.168.4.1/` like a captive-portal probe (it holds no data). Use the
  address, not a name.
- The two `POST`s also need `Content-Type: application/json` and an `Origin` that is absent or the
  hub's own (`http://192.168.4.1`); otherwise `403`. curl works:
  `curl -H 'Content-Type: application/json' -d '{"on":false}' http://192.168.4.1/api/cot`.
- On the station network, `POST`s are off unless `allow_writes = true` (above).

Refusals count in the `hub` line's `http_refused`. Every response carries
`X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`; the page also
`Content-Security-Policy: default-src 'none'; script-src 'sha256-…'; style-src 'sha256-…'; img-src data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`.
The two hashes are computed by `crates/webui/build.rs` from `web/index.html` itself, so they follow
every edit of the page; a hash policy also blocks inline `on...=` handlers and `style="..."`
attributes, and a `webui` test fails if the page gains one. A new connection must send its request
within 2 s (idle connections cannot hold the eight sockets for long).

**DHCP pool.** edge-dhcp 0.8 reuses an expired lease only once *every* address in its range is in
its lease table. The old range (.50-.200, 151 addresses) with an 8-entry table never got there: the
ninth different phone since boot was refused until a reboot. The range is now exactly as large as
the table (.50-.57) and leases last 10 minutes, so an address frees within 10 minutes of a phone
leaving. `crates/webui/src/dhcp.rs` drives the same server type past the table size in its tests.
**Captive portal.** DNS answers every name with 192.168.4.1, and probe URLs get the redirect above, so
phones pop the page open. DHCP also sends the RFC 8910 captive-portal URL.

**`/api/state` without PSRAM.** `json::state` builds the whole object in one buffer, and for 64
tracks that can be about 99 KB. That is more than the S3 has spare. So the hub streams the same bytes
in pieces, from `src/api.rs`:
1. the envelope, which is `json::state` with no tracks, minus its closing `]}\n`;
2. each `track_line`, comma-separated;
3. a final `]}\n`.

The hub checks that the streamed bytes equal `json::state` byte for byte:
- `demo` builds check once at boot;
- USB command `s` checks any time.

The result appears as `{"t":"state_check","identical":true,…}`.

**Own position.** A recent usable GPS fix wins (height above the WGS-84 ellipsoid, as Remote ID
uses). Otherwise the manual position (from the phone or USB `p`) is used, and it is re-applied every
second so the tracker's 10 s fix timeout doesn't drop it. It is not kept across reboots yet.

**Browsers only give a page the phone's location over HTTPS.** Over plain HTTP the page cannot read
`navigator.geolocation`, so you have to type the position in on the page.

## TAK Cursor-on-Target

Once a second, every track with a position becomes a drone event, and every track with an operator
position becomes an operator event (`tracker::cot`). They go to **239.2.3.1:6969**, ATAK's default
SA multicast, on the hotspot (and on the station network, if built).

CoT needs UTC. It comes from the GPS (`"utc_src":"gps_pps"` or `"gps"`), so every build sends CoT
once the receiver has time. Without a GPS, events are skipped and counted in `cot_no_utc`. Demo
builds fall back to the build machine's clock plus uptime, labelled `"utc_src":"build"`.

To toggle CoT, press `m` on USB or `POST /api/cot`. It is on by default. The switch is shared by
every phone, so the phone page shows a **TAK off** chip in its status strip (and the state under
Settings, Hub status) whenever it is off. Datagrams go out with an IP hop limit of 1 (same network
only).

## USB (USB-Serial-JTAG)

The hub sends JSON lines, one object each, with a `"t"` tag. Readers should skip lines that do not
start with `{` (bootloader text, panics).

| `t` | When |
|---|---|
| `boot` | Once: firmware, features, pins, `track_bytes` (static RAM of the track table) |
| `hotspot` | At boot, and on `h`: SSID, channel, security, `salt` state; the password only on `h` with BOOT held |
| `track` | Every second, one per track (`tracker::json::track_line`) |
| `link` | Every second, one per open FPV link (`tracker::json::link_line`) |
| `hub` | Every second: tracks, own position, per-link counters, heap, CoT/HTTP counts, timings (below), and `gps`, `sd`, `screen`, `batt`, `buzz` ("What each status means") |
| `gps`, `sd` | GPS found or lost, configured; SD card found, recording, lost |
| `relayed` | Every second, one per node behind a relay (gateway or P4 hub): address, link, RSSI, role, rates, drops |
| `new` | A drone seen for the first time (the buzzer's twin) |
| `state_check` | Streamed `/api/state` vs `json::state` |
| `sta` | Station up, down or failed |
| `warn`, `err`, `cmd`, `help`, `demo` | Messages |

The `hub` line also counts `relay_refused` (frames refused because their sender may not originate
them, such as the P4's own-position/clock, ADS-B and flag frames arriving relayed for a node: see
"Web head trust") and `http_refused` (requests
refused by the Host / Origin / Content-Type checks).

The `hub` line also counts open FPV links: `open_links` (in the table) and `open_active` (heard in the
last minute). The boot line has `max_open_links` and `show_link_ids`.

The `hub` line includes timing and memory fields:
- `heap_free` and `heap_min_free`;
- `tick_us`, the 1 Hz housekeeping plus the USB dump;
- `cot_us`, the longest CoT pass;
- `lag_max_us`, the worst lateness of the thread-mode executor in the last second.

Per link it reports:
- bytes and frames received;
- `corrupt`, `ovf`, `uart_err` and `qfull`;
- `seen_ps` and `rid_ps_node`, the node's own rates;
- `rid_ps_hub`, Remote ID detections that reached the hub per second;
- `drops`, the node's drops plus the hub's.

Commands are one character, with no Enter:

| Key | Does |
|---|---|
| `p` | Set own position to a test point (300 m south of Philadelphia City Hall) |
| `c` | Clear it |
| `m` | Toggle CoT |
| `s` | Print the state body plus `state_check` |
| `h` | Show the hotspot details and help. With **BOOT held**: also the hotspot password |
| `R` | With **BOOT held**: make a new hotspot password and restart (every phone must join again) |

## Design notes

- **Node UARTs run on a high-priority interrupt executor** (`links.rs`). At 921600 baud the 128-byte
  FIFO fills in 1.4 ms. Its interrupt fires at half full, and the reader only decodes COBS frames into
  a 24-slot queue. Tracker work runs on the thread-mode executor, so formatting tracks for a phone
  can't overflow a UART.
- **The tracker is only touched from the thread-mode executor**, so a `RefCell` is enough and no
  critical section blocks interrupts while formatting. It is never borrowed across an `.await`
  (HTTP, CoT and USB format one track, release it, then send).
- **Formatting is slow on Xtensa**, because f64 is soft-float. Measured on the S3:
  - about 3 ms per track line;
  - about 1.5 ms per CoT event.

  Each loop yields after every track. At 64 tracks, expect roughly 0.4 s of CPU per second for USB
  plus CoT.
- **Watchdog.** The 1 Hz tick feeds the RTC watchdog with a 10 s timeout. A panic prints a backtrace
  on USB and reboots after 0.5 s.
- **Two cores.** The screen, SD card, buttons, buzzer and battery run on the S3's second core with
  their own executor (`periph.rs`). Their drivers are blocking (an SD card can be busy for hundreds
  of ms; a frame takes ~120 ms), and on core 0 that would come out of USB, HTTP and CoT. They see
  core 0 only through critical-section statics: the screen view-model (`screen::UI_STATE`, an
  async mutex), the SD row pipe, and atomics. The C5 has one core, so there the same tasks share
  it and a slow card or frame delays everything.
- **Screen without a framebuffer.** 320x240 RGB565 is 150 KB. The frame is drawn in 10 bands of
  320x24 (15 KB, `screen::band`), each sent as soon as it is drawn, so the panel only ever gets
  finished pixels (no flicker). Measured on the S3 with the demo radar page: **~75 ms to draw all
  bands (up to ~100 ms), ~44 ms to send them at 40 MHz** (with no panel attached the SPI still
  clocks), about one frame a second. Core 0 builds the view-model in ~4-9 ms of wall time, yielding
  between tracks. While core 1 draws, the shared flash cache slows core 0 a little: the CoT pass
  goes from ~24.5 to ~27 ms in those seconds.
- **RAM (S3, no PSRAM)**, measured with the demo (2026-11-09):
  - heap at boot 132 KB free, at least **55.6 KB** free while running (was 90 KB before the screen
    and SD): the band buffer (15 KB), core 1's stack (12 KB, ~3.6 KB used) and the screen
    view-model (7 KB) live on the heap;
  - static `.bss` + `.data` is 267 KB, leaving **~31 KB** for core 0's stack (was ~48 KB).
  - The open-FPV-link table (2026-11-09) costs ~2.2 KB of that: the linker now leaves **29.1 KB**
    for core 0's stack (31.3 KB before). The C5 hub, whose stack was within 1.2 KB of the linker's
    8 KB floor, gives 4 KB of its extra heap to make room (`EXTRA_HEAP` in `main.rs`).

## ADS-B (feature `adsbee`, PLAN.md Phase 6)

An ADSBee 1090 shows crewed aircraft (medevac helicopters, crop dusters)
beside the drones. `src/adsbee.rs` reads its CSBee text on UART2 with the
`adsb` crate; the aircraft go into the tracker's own aircraft table and out
on every output: the phone page's list ("Aircraft (ADS-B)", plus an ADS-B chip
in the status strip), USB (`{"t":"aircraft",...}` lines and an `adsb` object
in the `hub` line), the SD log (`ICAO-<hex>` rows), CoT (`ICAO-<hex>`, civil
air types) and the screen (an ADS node and an AIRCRAFT line on the health
page). Aircraft **never** trigger the new-drone alert or the buzzer; one
within 5 km and 2000 ft is tagged LOW, quietly.

Why CSBee over the UART, not the ADSBee's other outputs: it is the only one
that is decoded *and* integrity-checked text (a CRC per line), carries
everything we show (callsign, squawk, category, both altitudes, signal), and
needs no Mode S decoder of our own. MAVLink and GDL90 are binary and drop the
signal level and statistics; raw/Beast would need a full Mode S and CPR
decoder on the hub; the Wi-Fi feeds would mean changing the ADSBee's network
settings, and the hub's hotspot is not a place to send them anyway.

The ADSBee only accepts AT commands on its USB console, so the hub cannot
configure it; the owner sets `AT+PROTOCOL_OUT=COMMS_UART,CSBEE` once (see
`docs/ADSBEE.md`). Memory (S3, measured from the linked image): the aircraft
table (32 x 120 B), the report queue and the extra tasks cost ~9.9 KB of
static RAM, leaving **19.2 KB** for core 0's stack in an `adsbee` build
(29.1 KB before). Builds without `adsbee` size the table to 0: the default S3
build lost 0.6 KB of stack (28.5 KB) and the C5 0.5 KB (10.9 KB), for the
always-compiled formatter code and status fields. `adsbee` is refused on the
C5 (no spare UART).

## Untested without hardware

Built and run on the bare S3 with nothing attached: every peripheral reports absent and nothing
stalls. These paths have **not** run against real parts yet:
- GPS: the I2C read loop with data, the configuration ACKs, fix and UTC publishing, PPS anchoring,
  and the whole `gps-uart` transport (builds only).
- Screen: whether the panel lights, its orientation and colour inversion (placeholders), and the
  real SPI timing on a panel.
- SD: card init, FAT open, file naming, row writes, flush, hot removal and re-insertion.
- Buttons, buzzer and battery: never pressed, heard or measured.
- ADSBee: the UART reader against a real ADSBee (only the parser and the demo's encoded lines
  have run), its real line rate in a busy sky, and GPIO41 on the actual module.

Not done:
- station mode: BSSID pinning and an API token (see "Station mode");
- WPA3 (esp-radio 1.0.0-beta.1 has no setting for it) and client isolation on the hotspot;
- SNTP time in station mode;
- keeping the manual position and screen settings across reboots;
- a hub-to-node command channel on the reserved TX pins;
- PPS to the node chips for cross-node time sync (P3): the hub timestamps PPS for its own UTC only.


## Security checks still to run on hardware (WP-05)

Everything in the hotspot / web-head security work is compile-checked for the S3 and C5 and
host-tested in `crates/webui` and `crates/link`; none of it has run on a board yet. On the S3 bench
board, in this order:

1. **First boot makes a salt.** Flash a build of this branch (`tools/dev-board.sh ... 60`). The
   `hotspot` line says `"salt":"new"` and a `warn` line says how to read the password; no line
   contains a `password` field. Reset: now `"salt":"stored"`.
2. **Password needs BOOT.** Type `h`: no `password`, plus the `help` hint. Hold BOOT (after the hub has
   started), type `h`: the line has `"password":"xxxx-xxxx-xxxx"`. A phone joins `Rotorscope-XXXX`
   with it.
3. **Reflash keeps it.** Flash again (no erase): `"salt":"stored"`, same password.
4. **New password.** Hold BOOT, type `R`: a `warn` line, the board restarts, `"salt":"new"`; BOOT +
   `h` shows a different password; the phone has to rejoin with it. Without BOOT, `R` only prints
   the hint.
5. **Erased salt.** `espflash erase-flash`, then flash again: `"salt":"new"` and a new password. (A
   flash read/write fault cannot be staged on the bench; it takes the `this_boot_only` branch, which
   only a code review has checked.)
6. **DHCP recovers.** With the hub up, join and leave with 9+ distinct clients (or one laptop whose
   MAC is changed between joins); after 10 minutes the next client still gets an address in
   .50-.57.
7. **Request checks.** From a joined laptop:
   `curl -i http://192.168.4.1/api/state` (200);
   `curl -i -H 'Host: evil.example' http://192.168.4.1/api/state` (421);
   `curl -i -H 'Host: evil.example' http://192.168.4.1/` (302 to `http://192.168.4.1/`);
   `curl -i -d '{"on":false}' http://192.168.4.1/api/cot` (403, not JSON);
   `curl -i -H 'Content-Type: application/json' -H 'Origin: http://evil.example' -d '{"on":false}' http://192.168.4.1/api/cot` (403);
   `curl -i -H 'Content-Type: application/json' -d '{"on":false}' http://192.168.4.1/api/cot` (200).
   `http_refused` in the `hub` line counts the refused ones.
8. **Captive portal still opens.** A phone joining the hotspot pops the page (Android and iOS);
   `curl -i -H 'Host: connectivitycheck.gstatic.com' http://192.168.4.1/generate_204` is a 302 to
   `http://192.168.4.1/`.
9. **The page works under its CSP.** On a phone, and in a desktop browser with the console open:
   no CSP violation reports; the radar arrows point the right way; the bench banner shows on a
   `ROTORSCOPE_BENCH=1 ... --features demo` build and not on a normal one; switching CoT off (USB `m`)
   shows the **TAK off** chip within a second.
10. **Headers.** `curl -i http://192.168.4.1/` shows `Content-Security-Policy`,
    `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`.
11. **Idle sockets.** Open 8 idle TCP connections to port 80 (`nc 192.168.4.1 80` x 8, send
    nothing): within about 2 s the page loads again for a phone.
12. **Station mode** (needs `wifi.local.toml`): from a LAN machine, `GET /api/state` works and
    `POST /api/cot` answers 403 "changes are off"; with `allow_writes = true` it works. A packet
    capture of the CoT multicast on the LAN shows TTL 1.
13. **Web head trust** (P4 kit): `relay_refused` stays 0 in normal operation, and the P4's GPS still
    becomes the own position (`"own"` in the `hub` line) on a web head without its own GPS.
14. **RAM.** `heap_min_free` in the `hub` line, and core 0's stack. (On 2026-11-18 the S3 build did
    not link before this change either: static RAM over by 17,344 bytes at `stack.x`. This change
    adds 712 bytes, measured from the same linker error (`0x3fcdfac0` -> `0x3fcdfd88`): the request
    check in each of the nine HTTP tasks' futures, and the flash routines the salt code needs, which
    must run from RAM. Sorting each link by the origin table adds 32 more: `0x3fcdfda8`, 18,088
    bytes over in all.)

## Bench tests

Two-board tests that flash and judge this firmware: `tools/bench/run.py --list` (see `docs/BENCH.md`).
