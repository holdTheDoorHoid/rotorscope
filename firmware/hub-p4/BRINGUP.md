# hub-p4 bring-up: Elecrow CrowPanel Advanced 5" (ESP32-P4)

The P4 hub is bare-metal Rust (owner decision 2026-11-14), on a patched esp-hal
because this board's chip is revision **v1.3** and upstream esp-hal supports
only v3.0+. The patch is the community pre-v3 kit (naoNao89/esp32p4-pre-v3-kit
0.1.3, MIT/Apache-2.0), kept in `firmware/patches/esp-hal-p4-pre-v3/` with its
provenance, applied by `tools/p4-prepare.sh` (we do not run the kit's tool).

## Board facts (Elecrow wiki + this unit)

| Item | Value |
|---|---|
| Chip | ESP32-P4 rev v1.3, 40 MHz crystal, 360 MHz, MAC e8:f6:0a:e3:f6:de |
| Memory | 16 MB flash, 32 MB PSRAM (AP, 200 MHz) |
| Display | 800x480 IPS, **16-bit parallel RGB565** (not MIPI-DSI). DE 2, PCLK 3, HSYNC 40, VSYNC 41, B 8/7/6/5/4, G 14/13/12/11/10/9, R 19/18/17/16/15 |
| Backlight | STC8 MCU on I2C 0x2F, register 0x20 = 0..100 |
| Touch | GT911, I2C 0x5D, INT 42, RST 36 |
| Board I2C | SDA 45, SCL 46 (touch, STC8, PH2.0 header) |
| UART0 | GPIO37 TX / 38 RX via the CH340 USB-C port (flash + log) |
| UART1 header | 47 TX / 48 RX (shares the wireless-module socket SPI; DIP switch) |
| UART3 header | 27 TX / 28 RX (XH2.54, also a 5 V input) |
| Radio | ESP32-C6 on SDIO slot 1 (CMD 54, CLK 53, D0-D3 52/51/50/49), factory esp-hosted firmware; no UART/boot pads |
| Factory image | Backed up: `~/Desktop/rotorscope-backups/elecrow-p4-factory-16MB.bin`, SHA-256 32723c03..., bootloader and app image hashes verified |

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

## Results

| Step | What | Result (2026-11-14) |
|---|---|---|
| 1 | Boot, print | PASS: prints, chip revision reads 1.3; image has no CSR 0x347 |
| 2 | GPIO routing | PASS: bit-banged I2C on 45/46, lines idle high |
| 3 | I2C devices | PASS: 0x2F (STC8 backlight), 0x5D (GT911) both ACK |
| 4 | Backlight writes | PASS (ACKed); not yet confirmed by eye |
| - | esp-hal I2C controller | FAIL: every transfer times out, nothing starts (clock enabled, XTAL source, reset clear, `int_raw` only TXFIFO_WM). Not needed yet; bit-bang instead |
| 5 | PSRAM framebuffer | not started (kit: unvalidated) |
| 6 | RGB panel | not started (esp-hal has no P4 RGB driver; LCD_CAM to write) |
| 7 | Node UARTs (UART1, UART3) | not started |
| 8 | C6 over SDIO | not started; see "C6 and ESP-NOW" |

## C6 and ESP-NOW

The stock esp-hosted slave firmware has **no ESP-NOW support** (esp-hosted-mcu
issue #19, open since 2024-11; only a generic CustomRpc hook exists since
v2.8.1). Receiving ESP-NOW on the C6 therefore needs a different C6 image,
installed by esp-hosted's OTA over SDIO. The C6 has no UART or boot pins
brought out, so a bad image could leave the board without a working radio.
Not done without the owner's explicit go-ahead.
