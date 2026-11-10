# node-sdr: the ESP32-C5 as a raw radio sampler

PLAN.md Phase 10b. A dedicated ESP32-C5 reads its own radio's raw I/Q, watches one frequency
for DJI DroneID bursts, and hands each one to `crates/droneid`. **Receive-only**: the Wi-Fi driver
runs only to power and calibrate the PHY; the MAC's transmit queues are stopped and checked, and
nothing here can send a frame or a tone.

**Status: builds (`cargo fw` and `cargo fw-lite`), clippy-clean, host logic tested. Never run on
hardware.** [BRINGUP.md](BRINGUP.md) is the first-flash checklist.

| Path | What | Rate | Bits | Length |
|---|---|---|---|---|
| B, continuous | modem debug bus → 8 pads → PARL_IO RX → circular GDMA ring | 40 MS/s | 4-bit window of I and Q | 57,344-byte ring, 1.43 ms |
| A, snapshot | dump engine → SRAM block 1 | 20 MS/s | 10-bit I/Q + gain index | 16,380 samples, 822 µs |

Path B runs all the time and feeds the trigger. Path A is for level setting (`snap`, `level`), for
checking Path B against full-resolution data, and as the 10-bit fallback PLAN.md lists for 10d.

## Build

```sh
cd firmware/node-sdr/c5
cargo fw        # with the DroneID decoder in PSRAM (XIAO ESP32-C5 = C5HR8)
cargo fw-lite   # capture/trigger/dump only, for a C5 without PSRAM
#   -> firmware/node-sdr/target/riscv32imac-unknown-none-elf/release/node-sdr (+ node-sdr.map)
NODE_SDR_BOOT="level;grab;dump" cargo fw   # bake console commands that run after boot
```

`build.rs` refuses to build against any `libphy.a` but the one the private ABI was checked against
(SHA-256 `dbf33c41…04fffb`); see "PHY ABI" below. Flash with `tools/dev-board.sh <elf> [secs] c5`.

## Hardware: no jumpers

The debug-bus bits are looped back **inside the chip**: each of eight pads is driven by one
`MODEM_DIAG` signal and read back by its own input buffer into `PARL_RX_DATA0..7` (C5VRX: "These
are GPIO-matrix loopbacks; no external IQ wiring is required"; esp-sdr-zephyr: "No wire; the pads
really toggle at up to 40 MHz"). **Nothing may be connected to those pads.** Default pads (C5VRX's
set, validated on a XIAO ESP32-C5): GPIO 1, 0, 25, 2 (Q nibble), 27, 5, 3, 4 (I nibble). GPIO27 is
the XIAO's user LED, which will glow; C5VRX documents the same. `pins alt` switches to
esp-sdr-zephyr's set (0, 1, 3, 4, 5, 7, 10, 23). The board needs its 5 GHz (and/or 2.4 GHz)
antenna fitted.

## Console and output

Everything is JSON lines on USB-Serial-JTAG (`"t"` tags: `hello psram mem phy stats burst ev snap
grab held decode bus err ok help boot_cmd`), plus dump lines (`@H/@D/@E`). Commands (one per line;
`help` lists them; grammar in `crates/rawcap/src/cmd.rs`):

`stats`, `phy`, `freq <MHz>`, `offset <kHz>`, `gain <idx>`, `lpf <0-63>|off`,
`win clip|fold5|fold`, `edge rise|fall`, `pins xiao|alt`, `swap on|off`,
`trig <dB> [min_us max_us]`, `mode detect|hold|idle`, `snap`, `grab`, `level`, `decode`,
`dump [raw|c16] [b64|hex]`, `resend <first> [last]`, `reveal on|off`, `resume`, `bootloader`
(stop the radio and reboot into the ROM download mode; use it before reflashing).

A `burst` line, one per accepted trigger:

```json
{"t":"burst","pos":123456,"mhz":5756,"dur_us":640,"lvl_db":-14.2,"snr_db":12.8,"gain":40,"win":"clip",
 "dec":[{"crc":true,"path":"systematic","layout":"9-symbol","snr_db":17.3,"cfo_khz":512.0,"zc":930,
 "crc16":true,"type":"flight_info","seq":812,"serial":"1ZN***********","product":"Mini 2",
 "lat":..,"lon":..,"alt_m":..,"height_m":..,"v_mps":[..],"yaw":..,"home":[..],"app":[..],"uuid":"123****"}],"ms":95}
```

Serials, UUIDs and O4 key IDs are **redacted by default** (`droneid-file --redact` style);
`reveal on` prints them. O4 bursts report `"type":"o4_encrypted"` and the record name only; there
is no key handling of any kind. A trigger whose energy has the wrong length for DroneID prints at
most five `ev` lines a second and is not decoded.

### Dumps: pulling real captures to the laptop

`dump` streams the held capture as checksummed text lines (`crates/rawcap/src/dump.rs`): an `@H`
JSON header, `@D <id> <seq> <crc32> <data>` lines of 48 bytes, and `@E`. Lost or corrupt lines are
named by `tools/rawcap-pull`, and `resend <first> [last]` sends just those. What can be held:

| Command | Held | `dump` sends |
|---|---|---|
| `grab` | the whole Path B ring now (53,760 bytes, 1.34 ms) | `nib8` at 40 MS/s |
| `mode hold` | the next accepted burst window (32,778 bytes) | `nib8`; `dump c16` = decoder input |
| `snap` | a Path A snapshot (16,380 words) | `w32` at 20 MS/s; after `decode`, only `c16` |
| (nothing) | the last decoded burst's decoder input | `dump c16` |

```sh
tools/dev-board.sh --log 30 c5 > c5.log      # while typing / baking `grab;dump`
cargo run --release -p rawcap-pull -- c5.log --out caps --spectrum
cargo run --release -p droneid-file -- caps/c5-1.cs16 --rate 40000000 --format ci16 --redact --failures
```

## How it works

1. **PHY** (`src/phy.rs`). Wi-Fi started in station mode, never connected, power save off, an
   empty promiscuous filter; then `lmac_stop_hw_txq` plus the five MAC TX-queue enable bits
   cleared and read back; `phy_wifi_fbw_sel(0)` and `phy_set_chanfreq(mhz, 0)` (a full channel
   setup at any whole MHz, the 20 MHz analog branch); TX chain powered down, RX on;
   `phy_disable_agc` + `phy_rfagc_disable`; `phy_force_rx_gain`; the BBTOP low-pass capacitor
   code 12 (about 20 MHz full width, so neither 40 MS/s nor 20 MS/s aliases). Gain and AGC are
   re-checked every second and re-forced if lost (`gain_lost`).
2. **Debug bus** (`src/engine.rs`). The dump engine is started free-running (raw source 15, rate
   index 0, endless count) and SRAM block 1 is handed straight back; the engine writes nothing
   more, but `MODEM_DIAG[19:0]` keeps carrying I (19:10) and Q (9:0).
3. **Routing** (`src/route.rs`). Eight `MODEM_DIAG` bits (signals 106+, beyond esp-hal's table)
   go to the pads with raw `GPIO_FUNCn_OUT_SEL_CFG` writes and back into `PARL_RX_DATAk`. The
   window picks which bits (`crates/rawcap/src/iq.rs`): **clip** = bits 9..6, a plain `>> 6` that
   saturates only with the ADC (default; the decoder research found it decodes from 2 dB to over
   54 dB per-carrier SNR); **fold** = C5VRX's bits 9,6,5,4 (12 dB finer, folds above 127 codes);
   **fold5** in between.
4. **Ring** (`src/ring.rs`). PARL_IO RX from PLL_F240M / 6 = 40 MHz, 8 bits, software start;
   GDMA channel 0 over 16 × 3584-byte descriptors in a circle, owner check off; an interrupt per
   node adds the written lengths to a 64-bit producer count.
5. **Trigger** (`crates/rawcap/src/trigger.rs`). 256 of every 2048 bytes are summed through a
   power table; a fast-down/slow-up floor; two probes 6 dB up make a rise. The loop waits until a
   0.82 ms window (from 102 µs before the rise) is complete, stops the ring, measures the burst
   length at full rate, and only for 450–800 µs half-bands it to 20 MS/s into the capture buffer,
   removes DC and runs `Receiver::decode_all`. Then the ring restarts.
6. **Snapshots** (`src/engine.rs`). Block 1 to the engine, rate index 2, 16,380 words, DONE, block
   back; sentinels at both ends catch short captures.

## Memory

From `cargo fw` (`llvm-size -A`, map in `target/.../node-sdr.map`). Internal SRAM is
0x40800000–0x4085e5a0 (378 KiB counting the 64 KiB the bootloader frees):

| Region | Size | Holds |
|---|---|---|
| Block 0 (128 KiB) | 75.8 KiB | `.trap` 2.7, `.rwtext` 7.1, `.rwtext.wifi` 33.7, `.data` 9.2, `.bss` 23.1 KiB |
| | 52.2 KiB | heap (the gap below the bank, `_sdr_gap_*`) |
| Block 1 (128 KiB) | 56 KiB | Path B ring at 0x40820000 |
| | 8 KiB | 16 descriptors (192 B) at 0x4082e000, rest spare |
| | 64 KiB | capture buffer at 0x40830000: Path A target / 16,384 `C16` decoder input |
| Block 2 to 0x4084e5a0 | 57.4 KiB | main stack (the decoder needs about 30 KiB; link check at 40,000 B) |
| dram2 | 64 KiB | heap |
| PSRAM | 120.8 KiB | `droneid::rx::Receiver` (exact size in the `psram` boot line) |

Block 1 has to be the radio's: the dump engine can only own it (block 2 holds ROM data), and while
it runs the CPU loses the whole block. `ld/sdr_bank.x` inserts a section after `.bss`, aligned to
the next 128 KiB boundary, and the link fails if that is not exactly 0x40820000.

**Without PSRAM the decoder does not fit.** Needed: code/data 76 + bank 128 + stack ≥ 40 +
receiver 121 = 365 KiB before the Wi-Fi driver's heap (the `mem` boot line measures it), out of
378 KiB; and no free region outside block 1 is contiguous enough for the receiver (the largest,
block 2 + dram2, is 121.4 KiB and is where the stack and half the heap live). Hence the default
build puts the receiver at the start of PSRAM (esp-hal 1.2's C5 PSRAM driver, `Psram::new`, maps
it; the XIAO's C5HR8 has 8 MB in-package) via `Receiver::init_in_place`, so it never passes
through the stack. A C5 without PSRAM runs `cargo fw-lite` (capture, trigger, dumps; decode on the
laptop). A later no-PSRAM option: let the receiver's 52 KiB burst buffer alias the ring, which is
stopped during every decode (needs a `droneid` change).

## PHY ABI

`src/phy.rs` calls 13 private entry points of the closed `libphy.a` / `libpp.a`. Each declaration
records which argument registers the disassembly of *our* blob reads, and the GPL callers (C5VRX,
ESPARGOS esp-sdr, esp-sdr-zephyr) that run on it. The blob is ESP-IDF 6.0.2's C5 PHY, version
`edb400d6` (built Mar 10 2026): the same file C5VRX pins by SHA-256. Two guards, after CapnBry's
widscan58 `check_phy_abi.sh`: `build.rs` hashes the `libphy.a` that `esp-wifi-sys-esp32c5 =0.3.0`
ships and stops on any other (`NODE_SDR_ALLOW_UNVERIFIED_PHY=1` overrides, loudly); at boot
`phy::abi_ok()` checks the version string and, if it differs, the firmware refuses every private
PHY call and says so.

## Licensing and credit

GPL-3.0-or-later, like the rest of Rotorscope. The register sequences and facts come from:

- [Twotoz/C5VRX](https://github.com/Twotoz/C5VRX) (GPL-3.0-only): the receive-only lock (TX
  queues), the PHY call order, the pinned libphy, the debug-bus lanes and the XIAO pad set, the
  fold window. **Read for facts only; no code is adapted from it**, so no file here is
  GPL-3.0-only. C5VRX asks for credit where its findings are reused: this paragraph is it.
- [cyber-murmel/esp-sdr-zephyr](https://github.com/cyber-murmel/esp-sdr-zephyr)
  (GPL-3.0-or-later): the C5 dump-engine registers, the free-running debug-bus method (after
  ESPsoup and C5VRX), the PARL_IO/GDMA ring set-up and its measurements.
- [ESPARGOS/esp-sdr](https://github.com/ESPARGOS/esp-sdr) (GPL-3.0-or-later; "the capture
  mechanism itself is not covered by copyright"): word layout, C5 low-pass table, block-1 reservation.

All of it is re-implemented in Rust from those facts.
