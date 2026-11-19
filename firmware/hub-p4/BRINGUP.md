# hub-p4 bring-up: Elecrow CrowPanel Advanced 5" (ESP32-P4)

The P4 hub is bare-metal Rust (owner decision 2026-11-14), on a patched esp-hal
because this board's chip is revision **v1.3** and upstream esp-hal supports
only v3.0+. The patch is the community pre-v3 kit (naoNao89/esp32p4-pre-v3-kit
0.1.3, MIT/Apache-2.0), kept in `firmware/patches/esp-hal-p4-pre-v3/` with its
provenance, applied by `tools/p4-prepare.sh` (we do not run the kit's tool).

## Board facts (Elecrow wiki + this unit)

| Item | Value |
|---|---|
| Chip | ESP32-P4 rev v1.3, 40 MHz crystal, 360 MHz, MAC aa:bb:cc:dd:ee:01 |
| Memory | 16 MB flash, 32 MB PSRAM (AP, 200 MHz) |
| Display | 800x480 IPS, **16-bit parallel RGB565** (not MIPI-DSI). DE 2, PCLK 3, HSYNC 40, VSYNC 41, B 8/7/6/5/4, G 14/13/12/11/10/9, R 19/18/17/16/15 |
| Backlight | STC8 MCU on I2C 0x2F, register 0x20 = 0..100 |
| Touch | GT911, I2C 0x5D, INT 42, RST 36 |
| Board I2C | SDA 45, SCL 46 (touch, STC8, PH2.0 header) |
| UART0 | GPIO37 TX / 38 RX via the CH340 USB-C port (flash + log) |
| UART1 header | 47 TX / 48 RX (shares the wireless-module socket SPI; DIP switch 1 must select UART1), 3.3 V out. **Node link** (since 2026-11-17; GPS before) |
| Wireless-module socket | GPIO26 CLK, 47/48 (shared with UART1), 29-32 (SX1262 BUSY/NSS/IRQ/NRST, nRF24 IRQ/CE/CS). With no module fitted: **GPIO29 = GPS PPS, GPIO30 = ADSBee TX in** (2026-11-15) |
| UART3 header | 27 TX / 28 RX (XH2.54, also a 5 V input). **GPS** (since 2026-11-17; node link before). Its power pin is 5 V |
| Web-head feed | UART2 TX on GPIO31 (wireless-module socket), 921600, to the web head's node-A RX; WEBHEAD.md (2026-11-15) |
| Radio | ESP32-C6 on SDIO slot 1 (CMD 54, CLK 53, D0-D3 52/51/50/49), factory esp-hosted firmware; no UART/boot pads |
| Factory image | Backed up outside the repository: `elecrow-p4-factory-16MB.bin`, SHA-256 32723c03..., bootloader and app image hashes verified |

## Build and flash

```
tools/p4-prepare.sh                 # once: patched esp-hal checkout
cd firmware/hub-p4 && cargo fw      # target/rev103/...
```

Flash with Elecrow's own bootloader and partition table (ESP-IDF 5.4.2, known
good on v1.3; espflash's bundled P4 bootloader is not used), from the backup:
`elecrow-bootloader.bin` = flash 0x2000..0x8000, `elecrow-partitions.bin` =
0x8000..0x8C00.

```
espflash flash --chip esp32p4 --port /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0 -B 460800 \
  --bootloader elecrow-bootloader.bin --partition-table elecrow-partitions.bin <elf>
```

921600 baud reads fail on this CH340 ("Corrupt data"); 460800 works.

After flashing, the P4 booted by itself straight away, both times (bench, 2026-11-18); it may
still need a reset on some runs. Opening its serial console (the CH340) resets it every time: the
ROM prints `POWERON`, even with DTR and RTS held low before the port is opened. So the first boot
log seen through a console is always a fresh boot, not a continuation.

## Results

*Update 2026-11-18: steps 7, 11, 13 and 15 give the pins from before the 2026-11-17 header swap.
Since then the gateway / node link is on the UART1 header (RX GPIO48, TX GPIO47) and the GPS on
the UART3-IN header (RX GPIO28, TX GPIO27); see the pin table above.*

| Step | What | Result (2026-11-14) |
|---|---|---|
| 1 | Boot, print | PASS: prints, chip revision reads 1.3; image has no CSR 0x347 |
| 2 | GPIO routing | PASS: bit-banged I2C on 45/46, lines idle high |
| 3 | I2C devices | PASS: 0x2F (STC8 backlight), 0x5D (GT911) both ACK |
| 4 | Backlight writes | PASS (ACKed); not yet confirmed by eye |
| - | esp-hal I2C controller | FAIL: every transfer times out, nothing starts (clock enabled, XTAL source, reset clear, `int_raw` only TXFIFO_WM). Not needed yet; bit-bang instead |
| 5 | PSRAM | PASS: 32 MiB at 80 MHz, full write/read pattern, 0 bad words (37 MB/s write, 28 MB/s read, uncached word loop). Earlier I2C trouble after PSRAM was the stack bug |
| 6 | RGB panel (`src/rgb.rs`) | PASS, confirmed by eye 2026-11-14 (owner: colour bars and border look right, so colour order, direction and polarity are correct): LCD_CAM + AXI-GDMA stream 42-43 frames/s (VSYNC and DMA EOF agree; 18 MHz PCLK predicts 42.54), 0 DMA errors, CPU loop unaffected. See "RGB panel" |
| - | Stack inside L2 cache | FOUND + FIXED (memory.x patch): the kit's RAM overlapped the pre-v3 L2 cache; PSRAM sweeps corrupted the stack. See "Stack and the L2 cache" |
| 7 | Gateway link on UART3 (RX GPIO28, 921600) | PASS on-chip self-test: UART2 TX drives the same pad; 50 frames/s, 0 corrupt, `Via` attribution right. Not yet with a real wire |
| 8 | C6 over SDIO | not started; see "C6 and ESP-NOW" |
| 9 | Hub app (`src/app.rs`) | RUNNING: tracker (64 tracks) fed by the gateway link; the shared `screen` UI drawn at 2x (640x480) in 24-row bands, 4 redraws/s; UP / SELECT / DOWN touch keys in the right 160 px (replaced 2026-11-18 by the native layout, see "Screen"). Self-test frame became a track; 0 corrupt, 0 bytes lost while drawing |
| 10 | Touch (GT911) | PASS 2026-11-14: 46 taps by the owner logged; coordinates match the panel (keys at x 697-759, rows at y 66-126 / 229-276 / 378-439; left-side taps at x 297-404), so no axis swap or mirror. No crash |
| 11 | Real ESP-NOW chain: nodes -> C5 gateway (c5c) -> wire -> P4 UART3 RX GPIO28 | PASS 2026-11-14: two ESP-NOW nodes (c5a, and c5b on its own power) relayed by the gateway; P4 decoded 520 frames, 0 corrupt, 0 UART errors, 0 ring overflow, nodes=2, while drawing at ~36 fps. Gateway: 0 lost, 0 duplicates, RSSI -47 to -55 dBm on the desk. No test beacon ran (no spare board), so no detections yet |
| 12 | Test drone end to end: c5b rid-beacon (Wi-Fi beacon, 2.4 GHz ch 6) -> node c5a -> ESP-NOW -> gateway -> wire -> P4 | PASS 2026-11-14: P4 track count went 0 -> 1 about 18 s after the beacon started and held for the rest of the run, 0 corrupt. c5b then returned to node firmware (gateway in sight, 0 failed sends) |
| 13 | Downlink: hub -> wire (P4 UART3 TX GPIO27 -> gateway c5c D7/GPIO12) -> ESP-NOW command packet -> node | PASS 2026-11-14 (branch `coverage`): wire present. Coverage coordinator ran every 2 s over the relayed node table and sent `To` + `Assign` (changes at once, all every 5 s). Gateway read 1439 bytes / 32 commands from the hub, 0 corrupt, 0 orphans. Two real ESP-NOW nodes (c5a, c5b, Wi-Fi 2.4 firmware without Status, so 2.4 only): one parked on 6, the other sweeping 1-13 with a 6 visit; 21 commands, **21 ACKed first try, 0 failed**. A made-up node injected in the P4 (`--features fakenode`, can 2.4 + 5 GHz) was given Wi-Fi 5 (18 hops incl. 6 visits); no board has its address, so 4 tries each then "failed", as designed. P4 meanwhile: 0 corrupt, 0 ring overflow, max band 6.8 ms, ~35 fps. Nodes do not apply assignments yet (node side not built), so "applied" stays 0 |
| 14 | microSD log (bit-banged SD native 1-bit, CLK 43 / CMD 44 / D0 39), owner's card | PASS 2026-11-14 (branch `p4-sd`): card initialises as SDHC, 3728 MiB (7,635,968 blocks), CSD v2, manufacturer 0x6f, product "CBADS", FAT32 (partition type 0x0b). Created `ROTORSCP\` and one `LOGnnnnn.CSV` per boot; nothing else on the card touched. With `--features fakedrone` (made-up Remote ID drone, 1 row/s) and the real gateway link live (2-3 nodes): 14 of 14 flush read-backs OK (bytes and line counts match), and the next boot counted the previous file back (9556 bytes, 105 lines). Bus clock ~0.94 MHz (~115 kB/s, limited by the GPIO register path). Cost to the loop: one block ~5.5 ms, the longest normal step (flush) 18.3 ms, a flush with read-back up to 37 ms; mount at boot 93-166 ms once. Throughout: 0 corrupt, 0 UART errors, 0 ring_full, 0 dropped rows, 0 card errors; loops/s and max_gap (~6.7-7.0 ms, the screen band) same as before SD. (max_gap is not the main loop's worst stall: its clock restarts after the SD mount. The worst step is `max_tick` on the `hub-p4: sd` line.) No-card path host-tested only (owner's card stayed in) |
| - | CPU clock | esp-hal defaults to 90 MHz on the P4; now 360 MHz (`CpuClock::max()`), 3.4x faster drawing |
| 15 | GPS + ADS-B loop-back, LC29H(AA) emulated (`--features gpsloop`, branch `p4-parity`) | PASS 2026-11-15: UART2 played the GPS on the UART1 pads (GPIO48/47) at 115200, PPS pulsed on GPIO29 from inside the chip, UART4 sent CSBee onto its own RX pad (GPIO30). Hub found it at 115200, detected `lc29h` by `$PQTMVERNO`, 7 config steps ACKed, 3D fix, 14 in view / 14 used, dual band; 55 PPS edges in 59 s, UTC anchored to PPS; own position in the tracker (test aircraft at 1401 m, 37.6 deg); ADS-B 116 lines, 0 errors; 0 checksum / UART errors; gateway 2 nodes, 0 corrupt meanwhile |
| 16 | Same, u-blox MAX-M10S emulated as shipped (`--features gpsloop-m10`) | PASS 2026-11-15: nothing valid at 115200, NMEA at 9600; detected `max-m10s` by MON-VER, 4 VALSETs ACKed, baud step to 115200 followed, NAV-PVT 3D fixes, PPS anchor; SD file named from GPS time (`11160000.CSV`, the emulator's 2026-11-16 00:00). 2 UART errors = the 9600 bytes heard at 115200 during the hunt |
| 17 | Normal build from `p4-parity`, nothing on UART1/GPIO29/GPIO30 | PASS 2026-11-15, 45 s: GPS hunts 115200/9600/38400 and reports `absent` (0 bytes, 0 UART errors: pull-ups hold the idle lines), ADSBee not alive (0 bytes), SD `LOG00023.CSV`, gateway 2 nodes, 0 corrupt, 0 UART errors, 0 ring overflow, ~600 loops/s, max gap < 0.8 ms, max band 7.1 ms |
| 18 | Web-head feed: UART2 TX GPIO31 (branch `webhead`, WEBHEAD.md) | PASS on-chip loop-back 2026-11-15 (`--features webheadloop`: UART4 RX on the GPIO31 pad decodes what UART2 sends; NMEA and CSBee fed from inside the chip; real gateway with 2 nodes): 8724 bytes sent = received, 306 frames, 0 corrupt, 0 orphan `Via`, 0 dropped, HubFix with the test point and UTC, every Csbee line CRC-good; with `fakedrone` detections forwarded too. Normal build: feed runs with nothing on GPIO31 (0 dropped), gateway 2 nodes, 0 corrupt. Not yet with the real web head or wire |
| - | Gateway UART | moved to the UART3 interrupt with an 8 KB ring (~90 ms at full rate): a screen band takes ~6.5 ms and a touch read ~3 ms, the FIFO only lasts 1.4 ms |

## PSRAM and the I2C pins (resolved: it was the stack)

Before the stack fix below, the bit-banged I2C on GPIO45/46 failed after
PSRAM init (lines read low, or could not pull low), and raising LDO VO2
ourselves first seemed to cure it. That was a red herring: the stack lived in
the L2 cache's RAM, and the 32 MB PSRAM test sweep overwrote it. With the
memory map fixed, the I2C works after PSRAM with no LDO step (two boots,
2026-11-14), so the workaround is gone. VO2 (PMU ext_ldo p1_0p1a, IDF unit
mapping {0,3,1,4}) is the PSRAM supply: lowering it after init hangs PSRAM.

## RGB panel

`src/rgb.rs` is a register-level port of ESP-IDF v5.4.2's RGB panel driver
(`esp_lcd_panel_rgb.c`, stream mode, one framebuffer, no bounce buffer) with
`lcd_ll.h` / `axi_dma_ll.h`. Register offsets come from IDF 5.4.2's
`soc/esp32p4/register/soc/*.h`, which is the pre-v3 map (Elecrow's factory
image was built with 5.4.2 on this chip); the esp32p4 PAC is not used.

| Item | Value |
|---|---|
| DMA | AXI-GDMA TX channel 0, peripheral id 0 (LCD_CAM), as IDF on P4 (`SOC_GDMA_TRIG_PERIPH_LCD0_BUS = AXI`). 240 descriptors of 3200 B (2 lines) in internal RAM, circular, EOF on the last; 64 B data burst, descriptor burst on, no owner check |
| Framebuffer | 800x480 RGB565 at PSRAM 0x4800_0000 (reuses the PSRAM test area), written through the cache, then `Cache_WriteBack_Addr` (ROM, L1D+L2) |
| Clock | PLL_F160M, HP_SYS_CLKRST fractional divider 4 + 4/9 = 36 MHz, LCD prescale 2 = **18 MHz PCLK** (IDF's own choice of divider for 18 MHz) |
| Timing | H 4/8/800/8 (total 820), V 4/16/480/16 (total 516) |
| Polarity | HSYNC and VSYNC idle high (active low), DE idle low (active high), PCLK idle low, `pclk_active_neg = true` (data changes on the falling edge). Constants at the top of `rgb.rs` |
| Pins | GPIO matrix, signals 149 PCLK, 151 DE, 152 HSYNC, 153 VSYNC, 154+n data n; data 0..15 = B0-B4 (8,7,6,5,4), G0-G5 (14..9), R0-R4 (19..15) |
| CPU cost | none per frame (`next_frame_en`); `Panel::poll` only counts flags |

Measured 2026-11-14, log once a second: `vsync=42..43 eof=42..43` per
1000 ms (18e6 / (820 x 516) = 42.54 Hz, so PCLK is 18 MHz as configured),
`dma_err=0x0` (descriptor error and every FIFO under/overflow bit), current
descriptor address different on every sample, main loop ~140k iterations/s,
longest gap between link polls ~0.26 ms outside the log prints.

**Unverified: nobody has looked at the screen yet.** A person must check it. *Update 2026-11-18:
checked: the owner confirmed the colour bars and border by eye on 2026-11-14 (step 6).*
Expected: eight vertical bars, left to right white, yellow, cyan, green,
magenta, red, blue, black, each 100 px wide, with a 1-pixel white frame
around the whole panel (visible against the black bar on the right). If
red and blue are swapped, the data pins are B/R reversed; mirrored left to
right means the panel scans the other way; a shimmering or one-pixel-shifted
image means PCLK edge (`PCLK_ACTIVE_NEG`); a rolling or offset image means
HSYNC/VSYNC/DE polarity or porches. Elecrow sources also quote 16 MHz PCLK.

## Stack and the L2 cache

The patched esp-hal's `memory.x` puts RAM at 0x4FF4_0000..0x4FFA_E000 with
the stack growing down from the top, which assumes the v3 L2MEM layout (L2
cache at the low end). On pre-v3 silicon the L2 cache sits at the **top**:
IDF 5.4.2 `esp32p4/memory.ld.in` has sram_high = 0x4FF4_0000, length
0x80000 - L2 cache size. This unit's bootloader sets 256 KB
(`CACHE_L2_CACHE_CACHESIZE_CONF` = 0x400), so the cache data RAM is
0x4FF8_0000..0x4FFC_0000 and the whole default stack was inside it.

Symptom: after an unrelated change moved a pointer onto the stack, the PSRAM
test faulted (load access fault, address 0) in its read loop: sweeping 32 MB
through the cache had overwritten the saved PSRAM base on the stack.
Code-only working sets touch few cache ways, which is why it hid until now.

Fix: our own patch on top of the kit's,
`firmware/patches/esp-hal-p4-pre-v3/rotorscope-pre-v3-memory.patch`
(applied by `tools/p4-prepare.sh`), sets RAM to IDF's pre-v3 split:
0x4FF4_0000, length 0x80000 - L2 cache size, so 0x4FF4_0000..0x4FF8_0000 with
the 256 KB cache (`ESP_HAL_CONFIG_L2_CACHE_SIZE = "256KB"` in
`.cargo/config.toml`, matching the bootloader). Below 0x4FF4_0000 is where the
2nd-stage bootloader runs while it loads the app; IDF leaves it out too. The
boot log prints the cache split and sp, and `main` asserts sp < 0x4FF8_0000.
Verified 2026-11-14: stack top 0x4FF8_0000, PSRAM test, I2C and the panel all
pass on two boots. (The first workaround, moving the stack into `.bss` from
`main`, is no longer needed and was removed.) This should go upstream to the
kit; ask the owner before offering it.

## C6 and ESP-NOW

The stock esp-hosted slave firmware has **no ESP-NOW support** (esp-hosted-mcu
issue #19, open since 2024-11; only a generic CustomRpc hook exists since
v2.8.1). Receiving ESP-NOW on the C6 therefore needs a different C6 image,
installed by esp-hosted's OTA over SDIO. The C6 has no UART or boot pins
brought out, so a bad image could leave the board without a working radio.
Not done without the owner's explicit go-ahead.

## microSD log

The card slot is wired for **SD native 1-bit mode on SDMMC slot 0**: CLK
GPIO43, CMD GPIO44, DAT0 GPIO39, no DAT1-3 and no chip select, so SPI mode is
impossible (Elecrow's IDF example: 1-bit, 10 MHz max, internal pull-ups, no
power switch). The patched esp-hal has no SDMMC driver for the P4, so the SD
bus protocol is bit-banged:

| Piece | What |
|---|---|
| `crates/sdnative` | The protocol over a `Pins` trait: CRC7 commands, R1/R1b/R2/R3/R6/R7, init (CMD0, CMD8, ACMD41 with HCS, CMD2, CMD3, CMD9, CMD7, ACMD6 1-bit, CMD16 for SDSC), single + multi-block read/write with CRC16, CRC status token, busy on DAT0. A write returns once the block is accepted; `is_busy()` (8 clocks) lets the loop skip while the card programs. Host tests: `cargo test -p sdnative` (CRC vectors from the spec, CSD/CID decode, and the driver against a clock-edge card simulator: init SDHC/SDSC v1/v2, empty slot, never-ready, single/multi read/write, data starting inside the response, bad CRC, rejected write, card pulled out) |
| `src/sdbb.rs` | The pins (esp-hal `Flex`, pull-ups on CMD/DAT0), calibrated half-period spins: <= 400 kHz identifying, <= 10 MHz after; the `embedded-sdmmc` `BlockDevice` |
| `src/sd.rs` | The S3 hub's CSV log (`tracker::csv`): `ROTORSCP\LOGnnnnn.CSV` (or `MMDDhhmm.CSV` with GPS time, since 2026-11-15), one file per boot, rows once a second for every active drone, open link, RF emitter, Wi-Fi drone and ADS-B aircraft. One step per loop pass: a probe, an ACMD41, the mount, one block-aligned chunk (<= 512 B) or one flush (every 5 s). Nothing while the card is busy. Read-back line count after each flush while the file is <= 2 KB, and of the previous boot's file at mount |

Never formats, never deletes, never writes outside its folder; exFAT (or any
non-FAT16/32 card) is reported and left alone (`embedded-sdmmc` reads FAT16/32
only). No card: a ~1 ms probe every 3 s. Bench: `--features fakedrone`
injects a made-up Remote ID drone once a second so the log has rows. Every
bench feature of this firmware (`fakedrone`, `fakenode`, `benchlog`,
`gpsloop`, `gpsloop-m10`, `webheadloop`, `modesloop`, `embedpack`,
`selftest`, `faulttest`, `testpoint`) needs `ROTORSCOPE_BENCH=1` in the
environment, or the build stops; such a build shows `BENCH BUILD` on the
screen and lists itself in the hello line and the evidence boot event
(docs/BENCH.md "Bench builds").

## Screen: native 800x480 layout and themes (2026-11-18, branch `p4-themes`)

**Not yet seen on the panel.** Built and host-tested only (snapshots in
`crates/screen/snapshots/wide/`).

| Piece | What |
|---|---|
| Layout | `screen::wide` (feature `wide`): status bar 0..38, page, caveats strip, and three context keys at y 436..474 whose labels say what UP / SELECT / DOWN do on the screen showing |
| Touch | `display::key_at` = `screen::wide::key_at`: y >= 416 (12 px above the strip, generous), x in thirds edge to edge (0..266 UP, 267..532 SELECT, 533..799 DOWN). Touches on the page are ignored. SELECT held 700 ms = Back, as before |
| Band | 800 x 48 RGB565 (75 KB) in **PSRAM** at 1 MB (`BAND_OFFSET`, between the framebuffer and the tracker), 10 bands per frame, each copied into the framebuffer, `between(gw)` after each. The old 15 KB internal-RAM band static is gone, so the stack grew |
| Stack | `_stack_start - _stack_end` = **185,672 B** (origin/main 6651ecd: 170,688 B): `.bss` 68,584 -> 53,224 B (the band left), `.data` +376 B |
| Fonts | Anti-aliased 4-bit glyphs (B612, B612 Mono, IBM Plex Sans Condensed; SIL OFL) in flash: 235 KB of bitmaps plus metrics. Image: `.rodata` 129 KB -> 367 KB, `.text` +12 KB (the 320x240 renderer and its mono fonts are no longer linked), about +250 KB of the 16 MB flash in all |
| Cost | Host estimate only: a full radar frame takes 2.8 ms on the laptop (release), about 0.3 ms per band. Expect several ms per band on the P4; `max_band` on the `link` line will say. Not measured on the chip |
| Themes | Settings: Theme Auto / Glass Cockpit / Field Day / Red Night; with Auto, Day theme, Night theme, Switch at. Saved as `theme`, `theme-day`, `theme-night`, `theme-switch` lines appended to `ROTORSCP/SITE.TXT` when an edit ends (one Setting evidence event per line); console `L theme red-night` sets one; the file is applied at mount |

## GPS, ADS-B and the console (2026-11-15, branch `p4-parity`)

What the S3 hub does and the P4 now does too, and what it cannot yet, is in
`PARITY.md`. Summary of the hardware side:

| Piece | What |
|---|---|
| `src/gps.rs` | GPS on the **UART3-IN header** (hub RX GPIO28, hub TX GPIO27; UART1 peripheral; header power is 5 V), PPS on **GPIO29** (socket pin, rising-edge GPIO interrupt). Hunts 115200 / 9600 / 38400, detects u-blox MAX-M10S (UBX MON-VER) or Quectel LC29H (`$PQTMVERNO`), configures it for this boot only, publishes fix, UTC (PPS-anchored) and the sky view. **Untested with a real module** |
| `src/adsbee.rs` | ADSBee CSBee on **UART4 RX GPIO30** (socket pin), 115200. **Untested with the real ADSBee** (not wired to the P4) |
| `src/uartring.rs` | UART1/UART4 receive by interrupt into RAM rings (2 KB / 4 KB), like the gateway link |
| `src/console.rs` | UART0 (the CH340 USB-C port) taken over from esp-println: log lines and JSON lines go through a 16 KB ring drained without waiting; JSON lines dropped whole when full. Commands (`main.rs`): `c` clear manual position, `b` time airspace lookups, `a` aero plan, `m` / `H` GPS home, `T` accept GPS time, `L` site line, `n` / `k` / `r` / `u` / `e` evidence, `h` or `?` help; `p` (test position) in `testpoint` builds and `f` fault lines in `faulttest` builds only. Opening this port resets the P4 every time (ROM `POWERON`), even with DTR/RTS held low |
| `src/report.rs` | 1 Hz JSON lines: `new`, `hub`, `track`, `link`, `aircraft` |
| `src/gpsloop.rs` | `--features gpsloop` / `gpsloop-m10`: the on-chip loop-back (steps 15, 16). Leave the UART3-IN header (the UART1 header before 2026-11-17) and GPIO29/30 unconnected while it runs |

Socket pin numbers are from Elecrow's wiki (SX1262 BUSY = 29, NSS = 30):
check which socket hole is which with a meter before wiring, and keep any
radio module out of the socket while GPS PPS / ADSBee use it.
