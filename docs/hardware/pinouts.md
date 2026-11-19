# Pins and wiring reference

**Level: 🔴 Expert** · Every pin every Rotorscope firmware uses, read from the source, with baud
rates, directions, voltage warnings and how far each connection has been proven.

Each table names the source file it was read from. Where a document disagrees with the source,
this page follows the source. Pin numbers are ESP32 GPIO numbers; XIAO header names (D6, D7, D9)
are given where the code names them. Facts as of 2026-11-18 (commit `6651ecd`).

> [!WARNING]
> Every signal on every board here is **3.3 V logic**. Never connect a 5 V pin to a signal pin.
> On the P4 board the **UART3-IN header's power pin is 5 V**: do not power a 3.3 V-only GPS from it
> (use the UART1 header's 3.3 V output or the I2C header). The P4 header's 5 V is also not sized to
> power several C5 nodes: power nodes separately. Share ground between every pair of boards you
> wire together.

The proof words in the "Proven" columns are defined in
[Writing these docs](../dev/docs-style.md#say-what-is-proven-every-time). For parts that do not
receive radio themselves (a console, a screen, a wire), **proven on the air** means they worked on
the real boards of the running bench kit, as on the [Status](../project/status.md) page.

## P4 hub (`hub-p4`, Elecrow CrowPanel Advanced 5")

### Serial ports

The P4 has five UART peripherals. Note that the **peripheral** numbers and the board's **header**
names do not match: the node link uses the UART3 peripheral on the header labelled UART1, and the
GPS uses the UART1 peripheral on the header labelled UART3-IN. The two were swapped on
2026-11-17.

| Peripheral | Job | Pins | Baud | Direction | Board connector | Proven | Source |
|---|---|---|---|---|---|---|---|
| UART0 | Console: log lines, JSON lines, one-letter commands | TX GPIO37, RX GPIO38 | 115200 | both | USB-C port, through the CH340 | Proven on the air | `src/console.rs` |
| UART3 | Node link: the wired bus (default), an ESP-NOW gateway or one point-to-point node | RX **GPIO48**, TX **GPIO47** | 921600 | frames in on RX; commands and bus polls out on TX | **UART1 header** (PH2.0-4P). DIP switch 1 must select UART1, not the wireless module | Gateway link on these pins: proven on the air (2026-11-18 bench pass). Wired bus: simulated only (nobody has wired it) | `src/links.rs`, `src/bus.rs`, `src/main.rs` |
| UART1 | GPS | RX **GPIO28** (from GPS TX), TX **GPIO27** (to GPS RX) | hunts 115200, then 9600, then 38400 | both (the hub sends probe and configuration commands) | **UART3-IN header** (XH2.54). Its power pin is **5 V** | Simulated only (on-chip loop-back) | `src/gps.rs` (`BAUDS`), `src/main.rs` |
| UART4 | ADSBee 1090 (CSBee text, or Beast with `modes`) | RX **GPIO30** only | 115200 | in only; the ADSBee's RX is not wired | Wireless-module socket (the SX1262 NSS position) | Simulated only (on-chip loop-back with recorded real frames) | `src/adsbee.rs`, `src/main.rs` |
| UART2 | Web-head feed (the same `link` frames the nodes use) | TX **GPIO31** only | 921600 | out only; nothing comes back | Wireless-module socket (the SX1262 IRQ / nRF24 CE position) | Simulated only (on-chip loop-back); the real wire is built, not yet tried | `src/webhead.rs` (`TX_PIN`, `BAUD`) |

UART1 and UART4 receive by interrupt into RAM rings (2 KB for the GPS, 4 KB for the ADSBee); UART3
into an 8 KB ring (`src/uartring.rs`, `src/links.rs`). Bench builds borrow some of these pins:
see [Bench builds that drive the hub's own pins](#bench-builds-that-drive-the-hubs-own-pins).

### Other hub pins

| Function | Pins | Notes | Proven | Source |
|---|---|---|---|---|
| GPS PPS | **GPIO29** | Input, pull-down, rising-edge interrupt. Wireless-module socket (the SX1262 BUSY / nRF24 IRQ position) | Simulated only | `src/gps.rs` (`pps_init`) |
| Board I2C (touch and backlight) | SDA **GPIO45**, SCL **GPIO46** | Bit-banged, open-drain with pull-ups. The esp-hal I2C controller starts no transfer on this chip. Also on the board's PH2.0 I2C header | Proven on the air | `src/main.rs`, `src/bb.rs` |
| Touch controller GT911 | I2C address 0x5D (0x14 also probed) | Polled over I2C. Its INT (GPIO42) and RST (GPIO36), listed in BRINGUP.md, are not driven by the firmware | Proven on the air (46 taps logged, 2026-11-14) | `src/touch.rs`, `src/main.rs` |
| Backlight controller | I2C address 0x2F, register 0x20 = brightness 0..100 | An STC8 helper chip on the board | Proven on the air | `src/main.rs` |
| RGB panel control | DE **GPIO2**, PCLK **GPIO3**, HSYNC **GPIO40**, VSYNC **GPIO41** | 800x480 RGB565, 18 MHz pixel clock, about 42.5 frames a second, from a framebuffer in PSRAM | Proven on the air (colour bars checked by eye, 2026-11-14) | `src/main.rs`, `src/rgb.rs` |
| RGB panel data | Blue 0-4: GPIO8, 7, 6, 5, 4. Green 0-5: GPIO14, 13, 12, 11, 10, 9. Red 0-4: GPIO19, 18, 17, 16, 15 | Data lines 0 to 15 in that order | Proven on the air | `src/main.rs` |
| microSD | CLK **GPIO43**, CMD **GPIO44**, DAT0 **GPIO39** | SD native 1-bit mode only (no DAT1-3, no chip select, so SPI mode is impossible). Bit-banged; at most 400 kHz while identifying, at most 10 MHz after; measured bus clock about 0.94 MHz. Pull-ups on CMD and DAT0 | Proven on the air (owner's card, 2026-11-14) | `src/main.rs`, `src/sdbb.rs` |

### Pins the board has that the firmware does not drive

| Pins | What is on them | Source |
|---|---|---|
| GPIO21, 22, 23 | The board's I2S amplifier and speaker. No driver on `main`; the P4 hub has no buzzer | `PARITY.md` |
| GPIO24, 25 | The microphone, on the pins the P4's own USB-Serial-JTAG would otherwise use. No driver on `main` | `src/console.rs` (module comment) |
| GPIO49-54 | The onboard ESP32-C6 on SDIO (CMD 54, CLK 53, D0-D3 52, 51, 50, 49). Never touched | `BRINGUP.md`, `PARITY.md` |
| GPIO26 | Wireless-module socket clock. Unused | `BRINGUP.md` |
| GPIO32 | Wireless-module socket. Reserved for a web-head return wire if one is ever needed; unused today | `WEBHEAD.md` |

The repository does not record a complete list of free P4 pins.

> [!IMPORTANT]
> GPIO29, 30 and 31 are wireless-module socket pins, and their positions come from Elecrow's wiki,
> not from a measurement. **Check with a meter which socket hole is which before wiring**, and keep
> any radio module out of the socket while these pins are used (`PARITY.md`, `WEBHEAD.md`).

### Wiring the node link to the P4

| Setup | Hub pin | Other end | Extra parts |
|---|---|---|---|
| ESP-NOW gateway or one point-to-point node | GPIO48 (RX) | gateway or node TX, XIAO D6 = GPIO11 | ground |
| | GPIO47 (TX) | gateway or node RX, XIAO D7 = GPIO12 | |
| Wired bus (up to 8 nodes in the wiring guide) | GPIO47 (TX), the command wire | every node's RX, XIAO D7 = GPIO12 (optionally through 1 kohm each) | ground to every node |
| | GPIO48 (RX), the return wire | every node's TX, XIAO **D9 = GPIO9**, open-drain, each through a Schottky diode with the **stripe at D9** | **one 470 ohm pull-up to 3.3 V** at the hub; 1N5817, 1N5819, BAT43 or BAT85 diodes, not a 1N4148 |

The wired bus is **simulated only**: nobody has wired it yet. Full guide: [The wired bus in depth](../WIREBUS.md);
the source is `firmware/hub-p4/src/bus.rs` and `firmware/node-common/src/bus.rs`.

### Bench builds that drive the hub's own pins

These features (all need `ROTORSCOPE_BENCH=1`) loop a UART back onto the hub's own pads, so the
listed pins must be left unconnected while they run. Read from `src/main.rs`, `src/gpsloop.rs`,
`src/webheadloop.rs`, `src/modesloop.rs`.

| Feature | What drives what | Leave unconnected |
|---|---|---|
| `selftest` | UART2 TX drives GPIO48 (the node-link RX pad) with sample link frames; UART2 RX listens to GPIO47 | the UART1 header |
| `gpsloop`, `gpsloop-m10` | UART2 TX drives GPIO28 and its RX listens to GPIO27 (plays the GPS); GPIO29 pulsed from inside; UART4 TX drives GPIO30 with CSBee | the UART3-IN header, GPIO29, GPIO30 |
| `webheadloop` | UART4 RX moved from GPIO30 to GPIO31 to read back what UART2 sends | GPIO30, GPIO31 |
| `modesloop` | UART4 TX drives GPIO30 with recorded Beast frames | GPIO30 |

`selftest` and `gpsloop` take UART2, so those builds have no web-head feed. The firmware refuses
to compile incompatible combinations (`src/main.rs`).

## ESP-NOW gateway (`gateway`, XIAO ESP32-C5)

Source: `firmware/gateway/src/main.rs`.

| Function | Pin | Baud | Direction | Proven |
|---|---|---|---|---|
| Hub link TX (UART1) | **GPIO11** (XIAO D6) | 921600 | out: relayed frames, each after a `Via` marker | Proven on the air, with the P4 |
| Hub link RX (UART1) | **GPIO12** (XIAO D7) | 921600 | in: the hub's `To` + `Assign` and `TimeSync` commands | Proven on the air, with the P4 |
| Console | USB-Serial-JTAG (USB-C) | | JSON lines out; `fault ...` in with `faulttest` | |

The gateway transmits ESP-NOW on 2.4 GHz channel 6. It has no wired-bus mode.

## Wi-Fi node (`node-wifi`)

Source: `firmware/node-wifi/src/chip.rs` (`hub_tx_pin!`, `hub_rx_pin!`, `HUB_BAUD`) and
`src/main.rs` (wired bus).

| Build | Hub TX (node to hub) | Hub RX (commands from hub) | Baud | Proven |
|---|---|---|---|---|
| ESP32-S3, point to point | **GPIO43** (labelled TX, D6) | **GPIO44** (labelled RX, D7) | 921600 | (see below) |
| ESP32-C5, point to point | **GPIO11** (XIAO D6) | **GPIO12** (XIAO D7) | 921600 | (see below) |
| ESP32-C5, `wirebus` | **GPIO9** (XIAO D9), open-drain | **GPIO12** (XIAO D7) | 921600 | Simulated only (never wired) |
| `espnow` | no UART: frames go over ESP-NOW | | | Proven on the air |

The repository records no bench result for the point-to-point wired builds of the nodes (the
two-board wiring tests are listed in [Bench tests](../BENCH.md) without results), so this page
makes no claim for them.

On the S3, GPIO43/44 are UART0's default pins, so the ROM boot banner also appears on GPIO43 for a
moment after reset; the hub's decoder discards it as a corrupt frame. The `wirebus` build is C5
only (the firmware refuses to compile it for the S3). The USB-C port carries the JSON output.

## Bluetooth node (`node-ble`)

Source: `firmware/node-ble/src/bin/main.rs` and `src/lib.rs` (`hub::BAUD`).

| Build | Hub TX (node to hub) | Hub RX (commands from hub) | Baud | Proven |
|---|---|---|---|---|
| ESP32-S3, point to point | **GPIO17** | **GPIO18** | 921600 | (see below) |
| ESP32-C5, point to point | **GPIO11** (XIAO D6) | **GPIO12** (XIAO D7) | 921600 | (see below) |
| ESP32-C5, `wirebus` | **GPIO9** (XIAO D9), open-drain | **GPIO12** (XIAO D7) | 921600 | Simulated only (never wired) |
| `espnow` | no UART | | | Proven on the air |

As for `node-wifi`, no bench result is recorded for the point-to-point wired builds.

The C5 pins moved from GPIO10/9 to GPIO11/12 so that every image on a multi-image node uses the
same carrier wiring. The S3 pins are placeholders until a carrier board fixes them.

## Raw-radio node (`node-sdr`, XIAO ESP32-C5 only)

Sources: `firmware/node-sdr/src/route.rs` (`PINS_XIAO`, `PINS_ALT`), `src/main.rs`.

| Function | Pins | Notes |
|---|---|---|
| Capture pads, default set | GPIO **1, 0, 25, 2** (Q nibble), **27, 5, 3, 4** (I nibble) | The radio's debug bus is looped out of these pads and straight back into PARL_IO inside the chip. On a XIAO: D1 (GPIO0), D0 (GPIO1), D2 (GPIO25), the user LED (GPIO27), and GPIO2-5, which are not on the headers |
| Capture pads, `pins alt` set (console command, `bringup` builds only) | GPIO 0, 1, 3, 4, 5, 7, 10, 23 | esp-sdr-zephyr's set, for boards where the default clashes |
| Wired bus (`wirebus`) | TX **GPIO9** (D9), open-drain; RX **GPIO12** (D7); 921600 | Simulated only (never wired) |
| `espnow` | no UART | The uplink transmits only in short windows between captures |
| Console | USB-Serial-JTAG | JSON lines and dump lines out, text commands in |

> [!WARNING]
> **Nothing may be connected to the capture pads.** They really toggle at up to 40 MHz while the
> node runs. The XIAO's user LED on GPIO27 glows; that is expected. The pads avoid the
> PSRAM and flash pads (15-22), USB (13, 14), the UART0 header pins (11, 12) and GPIO26
> (`src/route.rs`).

`node-sdr` has no point-to-point UART build: without `espnow` or `wirebus` its `hub` line reports
the transport as `none` and it only prints on USB.

## Aero node (`node-lr`, ESP32-C3 + LR2021)

Sources: `firmware/node-lr/src/board.rs` (the table and `SPI_MHZ`, `HUB_BAUD`) and
`src/main.rs` (the pins actually handed to the drivers; they agree).

| Signal | ESP32-C3 pin | Other end | Notes |
|---|---|---|---|
| SPI SCK | **GPIO4** | LR2021 SCK | SPI2 at 8 MHz ("gentle on hand wiring") |
| SPI MISO | **GPIO5** | LR2021 MISO | |
| SPI MOSI | **GPIO6** | LR2021 MOSI | |
| SPI NSS (chip select) | **GPIO7** | LR2021 NSS | output, idle high |
| BUSY | **GPIO3** | LR2021 BUSY | input, no pull |
| IRQ | **GPIO10** | LR2021 DIO9 | input, pull-down. DIO9 is from SoftRF's "Nano MK2" settings: **unverified**, check the module's schematic |
| RESET | **GPIO1** | LR2021 NRESET | output, idle high |
| Hub TX (point to point) | **GPIO21** | hub RX | UART1, 921600 |
| Hub RX | **GPIO20** | hub TX | UART1, 921600; also the bus RX |
| Bus TX (`wirebus`) | **GPIO0** | shared return wire | open-drain. Not GPIO21: that is UART0's TX, where the ROM prints its boot banner |
| Power | 3V3, GND | LR2021 VCC, GND | |

Pins avoided on purpose: GPIO2, 8, 9 (strapping pins: a module pulling them the wrong way stops
the board booting), GPIO18/19 (USB, the console), GPIO11-17 (flash). The RF switch table (DIO5-8),
TCXO setting (3.3 V, about 5 ms start-up) and IRQ line are SoftRF's settings for an ExpressLRS-style
module; `board.rs` marks the TCXO voltage and IRQ line unverified, and a module with a different RF
switch needs its own table. An ExpressLRS C3 + LR2021 receiver has its own fixed wiring: read
it from that receiver's ExpressLRS hardware target and change `board.rs` only.

**All of this is built, not yet tried**: no LR2021 hardware has been in hand. See
[The aero node](../AERO-NODE.md).

## S3 hub and web head (`hub`)

Source: `firmware/hub/src/pins.rs` (`TABLE` and `take!`), with speeds from `src/bus.rs`,
`src/gnss.rs` and `src/periph.rs`. The code calls every pin a placeholder until a carrier PCB
exists. The current S3 build does not link (static RAM over by 18,088 bytes, 2026-11-18), so none of
these pins has been exercised by today's code.

### ESP32-S3

| Function | GPIO | Bus and speed | Notes | Proven |
|---|---|---|---|---|
| Node 0 RX / TX (Wi-Fi 2.4 GHz node, A) | **4** / 5 | UART0, 921600 | Only RX is used; TX is reserved. In a `webhead` build GPIO4 is the RX for the P4's feed (P4 GPIO31) | Web-head wire: built, not yet tried |
| Node 1 RX / TX (Bluetooth node, B) | **6** / 7 | UART1, 921600 | TX reserved | |
| Node 2 RX / TX (Wi-Fi 5 GHz node, C) | **15** / 16 | UART2, 921600 | Not used in a `gps-uart` or `adsbee` build | |
| ADSBee RX (`adsbee` only) | **41** | UART2, 115200 | From the ADSBee's COMMS_UART TX; its RX stays unconnected | Built, not yet tried |
| GPS I2C SDA / SCL | **8** / **9** | I2C, 100 kHz, MAX-M10S at 0x42 | Needs pull-ups to 3V3 (the internal ones are weak). Tie the MAX-M10S's D_SEL to GND to select I2C. With `gps-uart`: 8 = hub RX, 9 = hub TX, hunting 115200 / 9600 / 38400 | Built, not yet tried |
| GPS PPS | **2** | input, pull-down | TIMEPULSE | Built, not yet tried |
| SPI SCK / MOSI / MISO | **12** / **11** / **13** | shared by screen and SD | panel 40 MHz; SD 400 kHz to start, then 20 MHz | Built, not yet tried |
| Screen CS / DC / backlight | **10** / **17** / **18** | backlight: LEDC PWM at 5 kHz | ST7789 by default, ILI9341 with `ili9341`. RST not driven (tie to 3V3). Use a transistor if the backlight draws more than about 20 mA | Built, not yet tried |
| SD CS | **14** | | FAT16/FAT32 | Built, not yet tried |
| Buttons Up / Down / Select | **38** / **39** / **40** | active low, internal pull-ups | Hold Select = Back | Built, not yet tried |
| Buzzer | **21** | LEDC tone, 2.7 kHz | A passive piezo; a transistor for a louder magnetic one | Built, not yet tried |
| Battery sense | **1** | ADC1 | ADC2 cannot be used while Wi-Fi runs. Divider ratio `panel::BATTERY_DIVIDER`, a placeholder 2.0 (100k + 100k) | Built, not yet tried |
| BOOT button | **0** | input | Held while typing `h` or `R` on USB: proof someone is at the box before the hotspot password is shown or changed | |
| Console | USB-Serial-JTAG (GPIO19/20) | | All three UARTs stay free for nodes | |

S3 pin rules (`pins.rs`): avoid GPIO26-37 (flash and octal PSRAM on N16R8 modules), 0, 3, 45, 46
(strapping), 19 and 20 (USB), and 43 and 44 (the ROM bootloader prints there at reset).

### ESP32-C5 (kept compiling, never run as a hub)

| Function | GPIO |
|---|---|
| Node 0 RX / TX | 3 / 4 (also the web-head RX) |
| Node 1 RX / TX | 5 / 6 |
| GPS SDA / SCL, PPS | 23 / 24, 0 |
| SPI SCK / MOSI / MISO | 9 / 10 / 8 |
| Screen CS / DC / backlight | 11 / 12 / 2 |
| SD CS | 15 |
| Buttons | 25 / 26 / 27 (strapping pins: do not hold a button during reset) |
| Buzzer | 28 (strapping pin; must be an active buzzer, as there is no LEDC driver for the C5 in esp-hal 1.2) |
| Battery sense | 1 |

Two node links only (the C5 has two full UARTs), no `gps-uart`, no `adsbee`.

## Test transmitter (`rid-beacon`)

`rid-beacon` uses **no wired pins**. Its only connection is the USB-C port (USB-Serial-JTAG), which
carries the JSON status lines out and single-key commands in
(`firmware/rid-beacon/src/control.rs`). It transmits at the lowest power the libraries accept,
with an obviously fake identity, for bench tests only.

## Where this lives in the code

- `firmware/hub-p4/src/main.rs`: every P4 pin handed to a driver
- `firmware/hub-p4/src/links.rs`, `bus.rs`, `gps.rs`, `adsbee.rs`, `webhead.rs`, `console.rs`, `sdbb.rs`, `rgb.rs`, `touch.rs`
- `firmware/gateway/src/main.rs`
- `firmware/node-wifi/src/chip.rs`; `firmware/node-ble/src/bin/main.rs`
- `firmware/node-common/src/bus.rs`: the open-drain bus pin set-up shared by every wired-bus node
- `firmware/node-sdr/src/route.rs`
- `firmware/node-lr/src/board.rs`
- `firmware/hub/src/pins.rs`
- `docs/WIREBUS.md` "Pins" and "Wiring guide"; `firmware/hub-p4/PARITY.md` "Pins chosen"

**Next:** [Wiring](../get-going/kit-wiring.md)
