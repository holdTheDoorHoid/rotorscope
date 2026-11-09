# hub: merges the nodes' detections and serves them

Bare-metal Rust (esp-hal 1.2, esp-radio 1.0.0-beta.1, embassy, embassy-net) on an ESP32-S3.
It works **without PSRAM** (the board on the bench has none, and 4 MB flash).

The hub does five things:
- It reads up to three radio nodes over UART.
- It merges their Remote ID detections into tracks (`crates/tracker`).
- It runs a Wi-Fi hotspot with a phone page, and sends TAK CoT multicast.
- It streams JSON lines on USB.
- It can also join an existing network as a fixed sensor.

## Build

The chip is chosen by the directory you build from, as with the nodes.

```sh
# ESP32-S3 (Xtensa: Espressif's `esp` toolchain from espup)
. ~/export-esp.sh
cd firmware/hub/s3
cargo fw                              # the real hub
cargo fw --features demo              # + two synthetic drones, no nodes needed
cargo fw --features demo,open-hotspot # bench only: hotspot with no password
#   -> firmware/hub/target/xtensa-esp32s3-none-elf/release/hub

# ESP32-C5 (RISC-V: stable Rust). Kept compiling; two node links only.
cd firmware/hub/c5 && cargo fw
#   -> firmware/hub/target/riscv32imac-unknown-none-elf/release/hub
```

`cargo fw` is `cargo build --release --features esp32s3` (or `esp32c5`).

Flash and watch through the shared lock. Never call espflash directly:

```sh
tools/dev-board.sh firmware/hub/target/xtensa-esp32s3-none-elf/release/hub 60
```

Builds always recompile the hub crate itself (about 15-25 s). This is because
`build.rs` watches for `wifi.local.toml`, and cargo treats a watched file that
does not exist as changed. The cost buys one thing: creating or deleting that
file always takes effect on the next build.

## Pins (placeholders until the PCB exists)

All pins are defined in `src/pins.rs`. Change them there and nowhere else.

| Function | S3 GPIO | Notes |
|---|---|---|
| Node 0 RX / TX | 4 / 5 | UART0, Wi-Fi 2.4 GHz node (A). TX is reserved; nodes only talk to the hub today |
| Node 1 RX / TX | 6 / 7 | UART1, Bluetooth node (B) |
| Node 2 RX / TX | 15 / 16 | UART2, Wi-Fi 5 GHz node (C) |
| GPS I2C SDA / SCL | 8 / 9 | MAX-M10S |
| GPS PPS | 2 | |
| SPI SCK / MOSI / MISO | 12 / 11 / 13 | Shared by the screen and the SD card |
| Screen CS / DC / backlight | 10 / 17 / 18 | |
| SD CS | 14 | |
| Buttons | 38, 39, 40 | Active low, internal pull-ups |
| Buzzer | 21 | |
| Battery sense | 1 | ADC1. ADC2 can't be used while Wi-Fi runs |

Wiring a node: node TX goes to hub RX, plus a shared ground. Node A (`node-wifi`) transmits on GPIO43,
and Node B (`node-ble`) on GPIO17. All links run at 921600 baud.

The pin rules, so the choices also suit a PSRAM hub module (N16R8-class):
- Avoid 26-37 (flash and octal PSRAM).
- Avoid 0, 3, 45 and 46 (strapping pins).
- Avoid 19 and 20 (USB).
- Avoid 43 and 44: the ROM bootloader prints there at reset, and that output would go to a node.

The C5 has only two HP UARTs, so a C5 hub has links 0 and 1 only. Its buttons sit on strapping pins
(see `pins.rs`).

The console is USB-Serial-JTAG (the USB-C port), so all three UARTs are free for nodes.

## Hotspot

| | |
|---|---|
| SSID | `Rotorscope-XXXX`: the last two bytes of the chip's MAC, in hex (bench board: `Rotorscope-6DF0`) |
| Channel | **11**. Remote ID lives on channel 6, where Node A spends about 75% of its time, so the hub (the only transmitter in the box) stays as far from it as 2.4 GHz allows |
| Security | WPA2-PSK. With `--features open-hotspot` it is open; that build prints a warning on USB at every boot |
| Address | `192.168.4.1/24`. DHCP hands out .50-.200, with the hub as router and DNS |
| Clients | Up to 4 phones |
| TX power | esp-radio's default, 5 dBm. Low on purpose: the nodes' antennas are centimetres away |

### Password scheme

Each unit gets its own default password, for example `tx2y-b8g4-b4xd`:
- It is 12 characters in 3 groups (60 bits).
- It uses lower-case Crockford base32. There is no i, l, o or u, so nothing can be misread as 1 or 0.

```
SHA-256("rotorscope hotspot password v1" || OPTIONAL_UNIQUE_ID eFuse (16 bytes) || base MAC (6 bytes))
  -> first 60 bits -> 12 symbols
```

**Why not hash only the MAC?** The MAC is broadcast in every beacon (as the BSSID), and this code is
public, so anyone nearby could compute the password. The chip's 128-bit `OPTIONAL_UNIQUE_ID` is
written at the factory, and it is never sent over the air. That makes the password impossible to work
out from the air, yet stable across reflashes.

If that eFuse is blank on some batch, the password falls back to the MAC alone. When that happens,
the hotspot line shows `"pw_secret":false` and a warning appears. The bench board reports
`"pw_secret":true`.

The password is printed on USB at boot:

```
{"t":"hotspot","ssid":"Rotorscope-6DF0","channel":11,"security":"wpa2","password":"…","pw_secret":true,…}
```

It is also handed to the screen seam (`UiState::ssid` and `password`), ready for a QR code. Changing
the password at runtime isn't supported yet.

## Station mode (fixed sensor, O5): owner's steps

Station mode is compiled in **only** if `firmware/hub/wifi.local.toml` exists when you build. The file
is gitignored, so it never gets committed. Without it, no station code is built.

1. Create `firmware/hub/wifi.local.toml` yourself, with your network's name and password:

   ```toml
   ssid = "YourNetwork"
   psk  = "your-wifi-password"   # "" for an open network
   ```

2. Build as usual (`cd firmware/hub/s3 && cargo fw`). Cargo prints
   `warning: station mode ON (wifi.local.toml found; ssid and psk compiled in)`. The SSID and password
   are never printed.
3. Flash the hub. The boot line shows `"station":true`. Then `{"t":"sta","state":"up","ch":N}`
   appears once the hub has joined.
4. To turn station mode off, delete the file and rebuild.

The password ends up inside the firmware image, so don't share images built with station mode on.

On the joined network the hub serves the same page and API on port 80 (one connection at a time). It
also sends CoT multicast there, and gets its address by DHCP.

**AP+STA works**, through esp-radio's `Config::AccessPointStation`, and the hotspot stays up. A single
radio brings two limits:
- **The hotspot follows the router's channel.** If your router is on channel 6, the hub has to
  transmit on the Remote ID channel. The hub warns about this on USB. Put the router on channel 1 or
  11 if you can. (The S3 has no 5 GHz radio.)
- **Reconnecting disturbs the hotspot.** While the router is out of reach, each connect attempt scans
  every channel, and phones on the hotspot see short stalls. To limit this, retries happen only once
  every 30 s.

Station mode has not been run on hardware yet. Nobody has created the file.

## Endpoints

| | |
|---|---|
| `GET /` | The phone page: `webui::INDEX_HTML_GZ` (about 16 KB), sent with `Content-Encoding: gzip` and an `ETag`. If the phone's `If-None-Match` matches, the hub answers `304` |
| `GET /api/state` | The state object (schema in `crates/tracker/README.md`), sent with `Transfer-Encoding: chunked` and `Cache-Control: no-store` |
| `POST /api/own-pos` | Body `{"lat":..,"lon":..,"alt":..}` or `{"clear":true}`, checked with `json::parse_own_pos`. Answers `200 {"ok":true}`, or `400 {"error":"…"}` |
| `GET` / `POST /api/cot` | Read or set the CoT toggle: `{"on":true}` |
| anything else | `302` to `/`. This catches every captive-portal probe (Android `/generate_204`, Apple `/hotspot-detect.html`, Windows, Firefox) |

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

**Own position.** A recent GPS fix wins. Otherwise the manual position (from the phone or USB `p`)
is used, and it is re-applied every second so the tracker's 10 s fix timeout doesn't drop it. It is
not kept across reboots yet.

**Browsers only give a page the phone's location over HTTPS.** Over plain HTTP the page cannot read
`navigator.geolocation`, so you have to type the position in on the page.

## TAK Cursor-on-Target

Once a second, every track with a position becomes a drone event, and every track with an operator
position becomes an operator event (`tracker::cot`). They go to **239.2.3.1:6969**, ATAK's default
SA multicast, on the hotspot (and on the station network, if built).

CoT needs UTC. Until the GPS is integrated there is none, so events are skipped and counted in
`cot_no_utc`. Demo builds use the build machine's clock plus uptime instead, labelled
`"utc_src":"build"`.

To toggle CoT, press `m` on USB or `POST /api/cot`. It is on by default.

## USB (USB-Serial-JTAG)

The hub sends JSON lines, one object each, with a `"t"` tag. Readers should skip lines that do not
start with `{` (bootloader text, panics).

| `t` | When |
|---|---|
| `boot` | Once: firmware, features, pins, `track_bytes` (static RAM of the track table) |
| `hotspot` | At boot, and on `h`: SSID, channel, security, password |
| `track` | Every second, one per track (`tracker::json::track_line`) |
| `hub` | Every second: tracks, own position, per-link counters, heap, CoT/HTTP counts, timings (below) |
| `new` | A drone seen for the first time (the buzzer's twin) |
| `state_check` | Streamed `/api/state` vs `json::state` |
| `sta` | Station up, down or failed |
| `warn`, `err`, `cmd`, `help`, `demo` | Messages |

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
| `h` | Show the hotspot details and help |

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
- **RAM (S3, no PSRAM)**, measured with the demo and HTTP clients:
  - heap is about 139 KB, with at least about 90.8 KB free;
  - static `.bss` is 225 KB. This includes the 64-track table (29.8 KB), 8 HTTP tasks (about 7 KB
    each) and the 64 KB heap block;
  - about 53 KB is left for the stack.

## Seams for later (no-ops today)

| Module | Integration needs |
|---|---|
| `gnss.rs` | A task on I2C0 (`gps_sda`/`gps_scl`, MAX-M10S at 0x42) using `crates/gnss`, which calls `publish()` with fix and time, and `publish_status()`. Plus a PPS edge interrupt for sub-ms UTC. Height must be ellipsoidal (`height`, not `hMSL`) |
| `screen.rs` | The shared SPI bus with `screen_cs`/`dc`/`backlight`, rendering `UiState` (already built every second) in its own task via a `Signal`. A 320x240 RGB565 framebuffer (150 KB) doesn't fit without PSRAM: draw in bands or straight to the panel |
| `sd.rs` | The shared SPI bus with `sd_cs` and `embedded-sdmmc`, writing `csv::HEADER` and then `csv::row` for every track heard since the last tick, through a pipe so the card never stalls the tick. `recording()` feeds `"rec"` |
| `panel.rs` | Debounced buttons, a buzzer on LEDC (`new_drone` is already called once per drone; rate-limit it), and the battery on ADC1 |

Not yet done:
- SNTP time in station mode;
- keeping the manual position across reboots;
- a hub-to-node command channel on the reserved TX pins.
