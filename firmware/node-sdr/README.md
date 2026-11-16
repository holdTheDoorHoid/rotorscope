# node-sdr: the ESP32-C5 as a raw radio sampler

PLAN.md Phase 10b (DroneID), Phase 7 option 2 and 11b (FPV / RF scan). A dedicated ESP32-C5 reads
its own radio's raw I/Q and does one of two jobs, chosen by the hub: **DroneID** (walk the DJI
DroneID frequencies, decode every burst with `crates/droneid`) or **FPV** (walk the analog FPV
channels, classify what is on each with `crates/fpvscan`). **Receive-only while capturing**: the
Wi-Fi driver runs only to power and calibrate the PHY; the MAC's transmit queues are stopped and
checked, and the capture path cannot send a frame or a tone. The one exception is the optional
ESP-NOW hub uplink (`cargo fw-now`, see "Hub link" below), which transmits only in short, separate
windows between captures.

**Status: builds (`cargo fw` and `cargo fw-lite`), clippy-clean, host logic tested; bring-up on two
XIAO ESP32-C5 boards under way ([BRINGUP-RESULTS.md](BRINGUP-RESULTS.md)).** [BRINGUP.md](BRINGUP.md)
is the first-flash checklist.

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
cargo fw-now    # cargo fw + the ESP-NOW hub uplink (TRANSMITS between captures)
#   -> firmware/node-sdr/target-espnow/riscv32imac-unknown-none-elf/release/node-sdr
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

`stats`, `phy`, `freq <MHz>`, `offset <kHz>`, `gain <idx>`, `lpf <0-63>|off`, `dcal [ref | auto on|off]`,
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

## Jobs, plans and the hub (2026-11-14)

`hub.rs` speaks `crates/link` protocol 4. The node says `Hello` + `Status` (role, `can` =
DroneId|Fpv, plan id) at boot, every 5 s and on every change; a `Frame::DroneId` for every burst that
matched the DroneID reference pattern (`Flight` for plaintext flight records, `Encrypted{key_id}`
for O4, `Detected` when the pattern matched but the block did not decode or is another record
type); and a `Frame::Rf` per FPV verdict worth reporting, at most once per channel per 10 s unless
the kind changes. `rssi` in a DroneID sighting is the burst level in dB below the 4-bit window's
full scale (this receiver has no dBm calibration); `snr_db` is the decoder's per-carrier SNR.

The hub's `Command::Assign` (through the gateway, or the `assign` console command, which takes the
same fields) sets role and plan. Idempotent: the same role and plan id change nothing. Plans are
`(centre in 100 kHz units, dwell ms)`. Empty plan = built-in: DroneID 5756.5 / 5776.5 / 5796.5 /
2414.5 / 2429.5 / 2444.5 / 2459.5 / 2399.5 MHz, 700 ms each (the LO sits on the whole MHz below,
+0.5 MHz off the DC spike); FPV every reachable channel of both bands (15 at 2.4 GHz, then 31 at
5.8 GHz), 60 ms each. Entries outside the C5's reach
are dropped. Roles this image cannot do (Wi-Fi, BLE) are refused and the node keeps its job;
switching images is the multi-image layout's business. Console: `role droneid|fpv` (holds that job
against hub assignments until `role auto`, for bench work), `hop on|off`
(`freq` parks the node), `plan`, `assign <role> <id> [f:ms ...]`, `fpv verbose on|off`, `uplink [now]`.

### FPV / RF scanner

Each dwell: every trigger probe goes into a level meter (half-dB histogram); the noise floor is
the quietest channel's 10th percentile across the sweep (a video transmitter never pauses, so its
own channel cannot show the floor). The trigger runs ungated and the first rise freezes one window
(else the newest window is taken at the end), decimated to 20 MS/s. The receiver DC offset is
re-measured whenever the LO moves more than 8 MHz from the last measurement: a residual offset
inflates the probe level (the meter sees raw 4-bit samples) and made flat noise look like a loud,
wide, continuous signal at 37 MHz from the last calibration. A channel loud by 8 dB or more
is analysed (`crates/fpvscan`, scratch in internal RAM: about 13.6 KiB of heap): FM line rate (AnalogVideo), cyclic prefix at 66.7 µs (LteStyle),
3.2 / 12.8 µs (Wi-Fi: printed, never sent to the hub), 6.4 µs (WifiNarrow), wide flat continuous
on the HDZero grid (Hdzero), else continuous = FpvCarrier, bursty = Energy (printed, not sent: on 5.8 GHz it is nearly always Wi-Fi that missed the capture). 2.4 GHz channels: see below. Console: one `fpv` line
per non-quiet channel, a `sweep` line per sweep.

**DJI link fingerprints** (`crates/djilink`, `src/radio/dji.rs`, docs/DJI-LINKS.md; written
2026-11-15, **not yet run on a chip**). The same capture then goes through `djilink` unless
`fpvscan` found analog video or HDZero: bursts, a 78 kHz spectrum and the cyclic prefix folded
at the symbol period. Verdicts: DjiVideo, DjiO4Video (5.170-5.250 GHz), DjiFpvAirUnit (a
whole-MHz DJI FPV channel), DjiController (narrow 15 kHz OFDM bursts, reported only once bursts
have shown on two centres within 3 s, rate-limited as one emitter), else LteStyle. A dwell
whose loudest 1 % of probes are 8 dB up is analysed even when its 90th percentile is not (short
controller bursts). Each probe's level also goes into a 1.5 KiB per-dwell timeline (burst
length, period, duty). Scratch 12.6 KiB of heap, internal RAM; check the boot `mem` line on
first flash. The `fpv` line gains `dji`, `dji_cp` [metric, threshold, prefix samples], `dji_w`
[-3/-10/-20 dB widths kHz], `dji_c` (centre kHz from the LO), `dji_clip`, `dji_bursts`,
`dji_on`, `dji_long_us`, `tl` [bursts, burst µs, period µs, duty %] and `dji_us` (time taken).

Reach: carriers 5181–5810 MHz (LO 5180–5800, a channel above 5801 sits up to +10 MHz in the
capture): 31 of the 47 distinct channels. **Skipped** (above reach; an RX5808 covers them):
5820 F5, 5825 A3, 5828 B6, 5840 F6, 5843 R6, 5845 A2, 5847 B7, 5860 F7, 5865 A1, 5866 B8,
5880 F8/R7, 5885 E5, 5905 E6, 5917 R8, 5925 E7, 5945 E8.

**2.4 GHz** (2026-11-15, `fpvscan::band`, docs/SIGNALS.md 3b): all 15 analog video channels
(2370–2510 MHz, FuriousFPV-style bands `2.4A` and `2.4B`) are inside the measured 2.4 GHz reach (LO
2200–2650), tuned the same way (LO = carrier − 1 MHz). Nothing in the capture path is 5 GHz-only:
`phy_set_chanfreq` takes any whole MHz, the DC re-measurement already handles the 2.4 GHz tables
(and re-runs on every band change; 4–5 ms there), and the BBTOP low-pass code is the same analog
branch. Differences handled: one noise floor per band (2.4 GHz noise is a few dB higher at the same
gain index), an 802.11b DSSS check and a line-shape check (`dsss`, `sub_line`, `half_r` in the
`fpv` line, 2.4 GHz only), and inside 2400–2500 MHz unconfirmed continuous energy is `energy`
(not sent), not `fpv_carrier`. `fpv` lines carry `"band":"2g4"|"5g8"`; `sweep` lines add
`floor_2g4`. **Not yet checked on air**: the gain stays at 83 on both bands (2.4 GHz `gain_max` is
81; DroneID already runs 2.4 GHz at 83 with DC within ±16 codes and noise 64–84 codes RMS, a
little above the clip window's sweet spot), and the extra 2.4 GHz analysis time (~3 ms DSSS fold
+ the line-shape lags) is unmeasured.

### Hub link: the ESP-NOW uplink (feature `espnow`, `cargo fw-now`)

The radio is the capture front end, so sending means stopping capture. At a hop boundary, when
frames are waiting and the last window was at least 1 s ago (keepalives make that every 5 s),
`uplink.rs` opens a window: ring and dump engine stopped; `esp_wifi_stop` + `esp_wifi_start`
hand the PHY back to the driver in its ordinary state (this undoes our forced gain, AGC-off and
forced-off TX chain through documented calls only); channel 6; gateway learnt from its beacon
(up to 1.2 s, first time only); one `airlink` batch sent unicast (radio ACK + retries); 40 ms of
listening for the gateway's command reply; then `Radio::resume` re-tunes, re-measures the DC
offset, forces gain and AGC off, powers the TX chain down, stops the MAC transmit queues and
**reads them back**. A bad read-back is counted (`relock_bad`) and retried once; a second failure
halts capture (mode idle) rather than capture with a transmit-capable MAC. What transmits: the
batch, and the MAC-level ACKs of the gateway's command packets. Nothing else, and never during
capture. A batch is retried in 3 windows, then dropped and counted. `{"t":"uplink"}` lines count
windows, sent, acked, failed, frames, beacons, command packets and window time.

Measured on c5b (gateway c5c, 2026-11-14): about 49 ms per window once the gateway is known
(0.4–0.5 s the first time, while it waits for a beacon), plus ~25 ms to re-lock (DC calibration);
one window per ~5 s, so about 1.5 % of capture time. See BRINGUP-RESULTS.md "Hub link, FPV scan, HIL".

**Stack.** The capture loop runs as its own embassy task (`capture_loop`) so its state is static:
the main stack (block 2) also carries the decoder and ends right above SRAM block 1. With the loop
inline in `main`, main's future took 20 KiB and the decoder overflowed ~12 KiB into the capture bank
(found by the HIL test). The `link` line reports `stack_free` (never-touched stack, painted at boot):
about 6.9 KiB after decodes.

### Hardware in the loop (DroneID without a drone)

`tools/droneid-hil` turns every burst of the public recordings into exactly the bytes Path B
would hold (+0.5 MHz, 20 MHz analog band, 40 MS/s, receiver noise, 4-bit clip window), sends one
53,760-byte block at a time as `@F <offset> <base64>` console lines into a PSRAM staging buffer,
and `hil run <len> <crc32>` checks it and runs it through the live path (`Radio::hil_run`: copy
into the stopped ring, a fresh trigger, `window`: length gate, half-band, DC removal, decoder,
redacted `burst` line, hub sighting). HIL sightings are encoded and round-tripped but **never
queued for the hub**: a recording must not appear on the hub as a live drone.

```sh
tools/fetch-droneid-samples.sh
tools/dev-board.sh --run 'target/release/droneid-hil --rms 200,150,125' 800 c5b
```

## How it works

1. **PHY** (`src/phy.rs`). Wi-Fi started in station mode, never connected, power save off, an
   empty promiscuous filter; then `lmac_stop_hw_txq` plus the five MAC TX-queue enable bits
   cleared and read back; `phy_wifi_fbw_sel(0)` and `phy_set_chanfreq(mhz, 0)` (a full channel
   setup at any whole MHz, the 20 MHz analog branch); TX chain powered down, RX on;
   `phy_disable_agc` + `phy_rfagc_disable`; `phy_force_rx_gain`; the BBTOP low-pass capacitor
   code 12 (about 20 MHz full width, so neither 40 MS/s nor 20 MS/s aliases). Gain and AGC are
   re-checked every second and re-forced if lost (`gain_lost`).
   **Receive DC offset.** Each of the PHY's 90 gain-memory entries carries analog DC trims, taken
   from tables the PHY measures once at boot (on 5 GHz at seven reference LOs, 5210–5855 MHz). On
   our boards that boot measurement comes out at or near the untrimmed default, different every
   boot, and with a forced gain nothing re-measures it, so the DC offset grew with gain until the
   ADC railed at index ≥ 75. `phy::dc_recal` therefore re-runs the PHY's own RX DC measurement
   (`phy_set_rx_gain_cal_dc`) at boot and on every `freq` change, with `phy_chip_set_chan_ana`
   wrapped (`--wrap`, build.rs) so all seven table bins are measured at *our* LO, then rebuilds the
   gain memory (ESPARGOS esp-sdr does the same on the C5). About 22 ms on 5 GHz, 4–5 ms on 2.4 GHz.
   The measurement is receive-only (its callees are pbus test forces, the DCO DAC search and the DC
   estimator; it gates the digital TX clock but never powers the TX chain), and the PHY's loopback
   IQ step, which does send a tone, is not reached. Result: DC within about ±50 codes at every gain
   up to 83 (BRINGUP-RESULTS.md).
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

`src/phy.rs` calls 16 private entry points of the closed `libphy.a` / `libpp.a`, reads and writes
documented offsets of its `phy_param` state (band byte, calibration flags, RX DC tables), and
wraps one (`phy_chip_set_chan_ana`). Each declaration
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
