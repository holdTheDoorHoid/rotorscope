# node-sdr first flash: bring-up checklist

Work through the steps in order; each one assumes the ones before it passed. Every step says what
to run, what a good result looks like, and what a bad one most likely means. Nothing here
transmits. Keep the logs: they are the evidence for any fix.

## Conventions

```sh
ELF=firmware/node-sdr/target/riscv32imac-unknown-none-elf/release/node-sdr
B=c5b                     # the board name in tools/boards.local (see docs/BENCH.md)
```

**Bring-up builds.** Almost every command below (`freq`, `gain`, `trig`, `mode`, `snap`, `grab`,
`level`, `dump`, `resume`, `win`, `dcal`, `reveal`, `role`, `hop`, `assign`, `@F` / `hil run`,
`bootloader`, ...) exists only in a build with the bench feature `bringup`; a normal build
answers them with `"e":"bring-up command: needs a bringup build (BRINGUP.md)"` and keeps only
`help`, `stats`, `phy`, `plan`, `jam`, `uplink` and `fpv verbose`. Bench features (and
`NODE_SDR_BOOT`) stop the build unless `ROTORSCOPE_BENCH=1` (docs/BENCH.md "Bench builds"), so
for a bring-up session:

```sh
export ROTORSCOPE_BENCH=1          # this shell only
cd firmware/node-sdr/c5 && cargo fw --features bringup     # (or fw-lite / fw-now, same flag)
```

The `hello` line then says `"bench":["bringup"]` and a `bench_build` warning follows it. The ELF
lands where the normal one does: rebuild with plain `cargo fw` (in a shell without
`ROTORSCOPE_BENCH`) before the node goes anywhere; `tools/flash-node.sh` refuses a bench ELF
unless `ROTORSCOPE_BENCH=1`.

`tools/dev-board.sh` flashes and *reads* the port; it does not send anything. Two ways to give
console commands:

- **Baked in** (works with dev-board.sh alone): `NODE_SDR_BOOT="snap;grab;dump" cargo fw --features bringup` from
  `firmware/node-sdr/c5`; the commands run 2 s after boot, one every 500 ms, each echoed as a
  `boot_cmd` line. Most steps below are written this way.
- **Typed**: while `tools/dev-board.sh --log 120 $B | tee c5.log` runs, write to the same port
  from a second terminal: `printf 'snap\r\n' > "$PORT"` (`$PORT` = the c5 entry's by-id path in
  `tools/dev-board.sh`). Any serial terminal at any baud works too (USB-Serial-JTAG ignores baud).

Output is one JSON object per line. Ignore lines that do not start with `{` or `@` (the ROM and
second-stage bootloader print a few at reset).

## 0. Hardware

- XIAO ESP32-C5 (C5**HR8**: 8 MB PSRAM in the package; the decoder lives there). A C5 without
  PSRAM: use `cargo fw-lite` throughout and skip the decode checks.
- **No jumpers, no wiring.** The I/Q bits loop back inside the chip through eight pads: GPIO 1, 0,
  25 (header D0, D1, D2 on the XIAO), 27 (the user LED) and 2, 3, 4, 5. **Leave all of them
  unconnected**: they toggle at up to 40 MHz. The user LED glowing dimly is expected.
- Antenna fitted for the band you test (5.8 GHz for the default 5756 MHz).
- Useful, not required: a 50 Ω SMA/U.FL terminator (noise floor without signals), a 5 GHz Wi-Fi
  access point you control on channel 149 (step 8), and later a DJI drone with props off (step 11).

## 1. Host checks (no board)

```sh
cargo test --workspace                       # rawcap: unpacking, ring, trigger, dumps, pipeline
DRONEID_TESTDATA=$PWD/testdata/droneid cargo test --release -p rawcap -p rawcap-pull
(cd firmware/node-sdr/c5 && cargo fw)
```

Good: all pass; with the recordings present `path_b_chain_decodes_real_bursts` decodes every
burst in both windows. `cargo fw` ends `Finished`. Bad: `build.rs` panics with a SHA-256 mismatch
→ the PHY blob changed (cargo updated `esp-wifi-sys-esp32c5`?); do not override, re-check the ABI
(README.md "PHY ABI"). Link error "no longer fit SRAM block 0" → `.bss`/code grew past 128 KiB.

## 2. Flash and boot

```sh
tools/dev-board.sh $ELF 15 $B | tee boot.log
```

Good, in this order:

```text
{"t":"hello","fw":"node-sdr",...,"decode":true,"bank":"0x40820000","heap_gap":53496,"heap_reclaimed":65536,"stack":58784}
{"t":"psram","bytes":8388608,"receiver":~123700,"at":"0x4..."}      (exact size of droneid::rx::Receiver)
{"t":"phy",...,"mhz":5756,"gain_forced":true,"gain":83,"gain_max":7x-8x,"agc_off":true,"lpf":[12,12],"txq_off":true,"dc_cal":{"n":0,"mhz":0,...}}
{"t":"phy","version":"<n>,edb400d6,Mar 10 2026,...","abi_ok":true,"mhz":5756,"gain_forced":true,"gain":83,"gain_max":7x-8x,"agc_off":true,"lpf":[12,12],"txq_off":true,"dc_cal":{"n":1,"mhz":5756,"band":"5g","us":2xxxx,"valid":true,"iq_kept":true,"auto":true,"rf_dc":[[27x,26x],...]}}
{"t":"mem","heap_used":...,"heap_free":...}
{"t":"stats",...}   every second
```

| Symptom | Most likely |
|---|---|
| Nothing after the bootloader, or a reset loop | Panic before USB output: read the backtrace (esp-backtrace prints it). A stack-guard hit means the main stack is too small. |
| `"t":"err","what":"psram"` with `bytes:0` | PSRAM not found: wrong board (C5HF4?) or esp-hal's C5 PSRAM init failed. `cargo fw-lite` still exercises everything else. |
| `"what":"wifi_init"` | Heap too small for the Wi-Fi driver (`mem` line), or esp-radio failed; capture cannot start without it. |
| `"what":"phy_abi"`, `abi_ok:false` | The linked libphy is not `edb400d6`. The firmware refuses the private calls by design. |
| `gain_forced:false` or `gain` ≠ 83 | The forced-gain register write did not stick: `phy_force_rx_gain` ABI or the AGC re-enabling itself. |
| `agc_off:false` | `phy_disable_agc` did nothing; snapshots will show the gain index changing. |
| `lpf` not `[12,12]` | BBTOP block/host/reg wrong for this chip (esp-sdr says host 1, regs 6/7). Captures still work, just wider. |
| `txq_off:false` | MAC TX queues re-enabled: **stop and investigate** before anything else (receive-only is a hard rule). |
| First `phy` line's `dc_cal.rf_dc` all one value, or Q all 256 | Normal: that is the PHY's own boot DC calibration, which on 5 GHz comes out at or near the untrimmed default (see BRINGUP-RESULTS.md). The second line (`n` 1) is ours, at the tuned LO. |
| `dc_cal.n` 0 on the second line, `valid:false`, or `us` ≫ 30,000 | Our receive DC calibration (`phy::dc_recal`) did not run or did not finish; expect the DC offset to rail the ADC at gain ≥ 75 (step 6). |
| `dc_cal.iq_kept:false` | The PHY's IQ-calibrated flag is clear. Nothing we call runs the IQ step (which transmits a tone), but report it. |
| `gain_max` 0 or > 95 | Different PHY register map; the snapshot gain field (step 5) is the cross-check. |
| Board will not take the next flash | The modem domain keeps running across the USB reset (C5VRX saw this). In a `bringup` build, type or bake `bootloader` first; a normal build has no `bootloader` command: hold BOOT while plugging in. |

## 3. Is the PHY really ours?

```sh
NODE_SDR_BOOT="phy" cargo fw --features bringup && tools/dev-board.sh $ELF 10 $B
```

Good: the `phy` line as above, and a `bus` line:
`"pins":[1,0,25,2,27,5,3,4]`, `"out_sel":[112,113,114,115,122,123,124,125]` (106 + clip lanes 6,7,8,9
and 16..19), `"pad_samples"` four different bytes (the pads toggle at 40 MHz, so four reads in a row
should not all match), `"parl_rx_cnt"` non-zero, `"dma_node"` 0..15, `"produced"` growing.

Bad: `pad_samples` all identical (e.g. `[0,0,0,0]` or `[255,255,255,255]`) → the debug bus is not
carrying I/Q: the free-running engine did not start or the lane word is wrong (see step 4's table).

## 4. Path B ring running

Boot with `NODE_SDR_BOOT="trig 30"` and watch `stats` for 10 s. (Every trigger rise stops the
ring to inspect a window and restarts it, which resets `produced` and pauses `isrs`; with Wi-Fi
around that is 10–20 times a second, so a plain boot reads about 6 % low and `produced` jumps around.)

Good: `produced` rises by **40,000,000 ± 0.01 % per second**; `isrs` by about 11,160 per second
(one per 3584-byte node); `dma_err` 0; `overrun` false; `probes` about 19,500 per second; `floor`
settles; `rises` small in a quiet band.

| Symptom | Most likely |
|---|---|
| `produced` stays 0, `parl_rx_cnt` (step 3) also 0 | PARL_IO RX clock not running: PLL_F240M divider (`pll_240m_clk_en`), the PCR clock select/divider, or `rx_sw_en` mode. |
| `parl_rx_cnt` moves, `produced` 0, `isrs` 0 | GDMA side: peripheral id 9, descriptor address, interrupt not bound, or the SRAM window. |
| `produced` reaches 57,344 and stops | PARL_IO stops after its EOF (bit count). esp-sdr-zephyr ran on with EOF at the ring length; if not here, set `rx_eof_gen_sel` to 1 in `src/ring.rs` (EOF on the never-asserted enable signal). |
| Rate ≠ 40 MB/s (e.g. 80 or 20 MB/s) | Clock divider; `CLK_DIV` in `src/ring.rs` is the divider itself, the register takes divider − 1. |
| `dma_err` rising | Descriptor errors (owner check still on, a bad `next` link) or PARL_IO FIFO overflow (the DMA cannot keep up: bus contention, or the SRAM window registers). |
| `overrun` true, `skipped` rising | The loop falls a lap behind (a long command or decode); normal during `dump`/decode, not otherwise. |
| `gain_lost` rising | Something re-enables the AGC or drops the forced gain; it is re-forced each second, but find out what. |

## 5. Path A snapshot

```sh
NODE_SDR_BOOT="snap" cargo fw --features bringup && tools/dev-board.sh $ELF 10 $B
```

Good: `{"t":"snap","words":16380,"us":82x,...,"dc":[d,d],"rms":[r,r],"peak":p,"railed":0,...,"gain_word":[83,83],"advice":"..."}`.
`us` near 822 (16,380 samples at 20 MS/s). `gain_word` equal to the forced gain proves the forced
gain is what the ADC saw. DC within a few tens of codes at every gain, thanks to the DC
calibration at the tuned LO (without it, it grows with gain until the ADC rails; see step 6).

| Symptom | Most likely |
|---|---|
| `"e":"Timeout(0x...)"` | DONE never set. If the printed control word has bit 22 set, the engine signals done there on this chip (ESPARGOS's C5 probe waits on bit 22): change `CTRL_DONE`. Otherwise the clock forces or the source/rate fields are wrong. |
| `"e":"Short"` | The engine stopped early: count or rate field. |
| `us` ≈ 205 / 410 | Rate field ignored (80 or 40 MS/s): the rate index bits 23:21. |
| `rms` 0 and `dc` constant | RX chain off: `phy_pbus_xpd_rx_on` / `phy_set_rxclk_en`, or wrong source. |
| `gain_word` ≠ forced gain, or first ≠ last | AGC still active (step 2 `agc_off`). |
| `dc` beyond about ±60 codes, growing with gain | The DC calibration is stale (temperature) or did not run: type `dcal` and `level` again; compare `dcal ref` (the PHY's reference LOs, as at boot). |

The snapshot holds; `resume` (or the next boot) restarts the ring.

## 6. Set the gain

```sh
NODE_SDR_BOOT="gain 40;level;gain 55;level;gain 65;level;gain 75;level;gain 79;level;gain 83;level" cargo fw --features bringup
tools/dev-board.sh $ELF 20 $B
```

The receiver's DC offset is re-measured at the tuned LO at boot and on every `freq` change
(`dcal auto on`, the default; `dc_cal` in the `phy` line). `dcal` re-measures by hand (for example
after a large temperature change), `dcal ref` measures at the PHY's own reference LOs instead, as
its boot calibration does, for before/after comparisons. A gain change needs no re-measurement:
the calibration covers every gain index. A `level` whose `rms` is in the hundreds with `railed`
non-zero but `dc` near 0 caught a real burst (a nearby access point or bench transmitter), not a
DC problem: run `level` again.

Each `level` prints a `snap` line with an `advice`. For the default clip window you want the noise
RMS (antenna on, quiet band) around **48–80 codes** (about one 4-bit step), `railed` 0, `over256`
near 0: advice `clip: noise in range`. Note the index; it becomes `GAIN_DEFAULT` in `src/phy.rs`
once confirmed. Repeat with the terminator fitted: the RMS difference is your antenna's noise
contribution. If even the highest calibrated index (`gain_max`) leaves RMS under ~24 codes, the
clip window will be quantisation-limited: compare `win fold` in step 9.

## 7. Path B against Path A, and the sampling edge

Same scene, both paths, pulled to the laptop:

```sh
NODE_SDR_BOOT="gain <G>;snap;dump;resume;grab;dump" cargo fw --features bringup
tools/dev-board.sh $ELF 25 $B > c5.log
cargo run --release -p rawcap-pull -- c5.log --out caps --spectrum
```

Good: two dumps rebuild (`dump 1: w32 65520 bytes ...`, `dump 2: nib8 53760 bytes ...`). Their
spectra have the same shape over ±10 MHz: flat-ish noise inside the low-pass, rolling off towards
±10 MHz (code 12 is about 20 MHz full width), a DC spike, nothing mirrored between the two. The
`grab` line shows `at_rail` small for the clip window.

| Symptom | Most likely |
|---|---|
| `rawcap-pull` reports missing lines | USB dropped output (esp-println gives up when the host stalls). Type/bake `resend a b` as printed; append the new log; run again. |
| Path B spectrum is white/flat to ±20 MHz, or full of spurs, while Path A looks right | PARL_IO samples on the debug-bus transitions: **`edge fall`**, grab again. (The bus updates at 80 MHz; we take every other sample.) If neither edge is clean, try `pins alt` (lower load) and drive strength (`DRIVE` in `src/route.rs`). |
| Path B nibbles stuck (spectrum all at DC, `at_rail` 0 and lvl very low) | Wrong lanes or gain far too low for the window; check `out_sel` in step 3. |
| Path B mirrored relative to Path A | The two paths disagree on which field is I: a lane-order bug in `rawcap::iq::Window::diag_lanes`. |

## 8. I/Q orientation (passive, no transmitter needed)

Use any 5 GHz access point on channel 149 (centre 5745 MHz, 20 MHz wide; a phone Wi-Fi analyser
shows the channel). Tune 5 MHz below it so its band sits on the positive side:

```sh
NODE_SDR_BOOT="freq 5740;gain <G>;snap;dump;resume;grab;dump" cargo fw --features bringup
```

`rawcap-pull --spectrum`: the access point's bursts should put clearly more energy at **0..+10
MHz** than at −10..0. Grab a few times if the AP is idle (beacons are short). If the energy is on
the negative side in both paths, I and Q are swapped: `swap on` and repeat; if that fixes it, make
`swap_iq: true` the default in `src/radio.rs` (Settings). A mirrored capture is not cosmetic:
DroneID's Zadoff-Chu references do not correlate mirrored, and nothing would ever decode.

## 9. Window comparison

```sh
NODE_SDR_BOOT="gain <G>;grab;dump;win fold;resume;grab;dump;win fold5;resume;grab;dump;win clip" cargo fw --features bringup
```

Fold windows show finer noise but fold strong signals back (spurs in the spectrum when a strong
AP is near). The research recommends clip; keep it unless step 6 showed the noise far below one
clip step at maximum gain.

## 10. Trigger in your RF environment

Plain boot at 5756 MHz for 60 s (`tools/dev-board.sh $ELF 60 $B`).

At gain 83 every Wi-Fi frame is a `rise`, so expect tens to hundreds a second in a busy band. The
trigger follows each hot run and drops the ones whose length cannot be DroneID right away
(`gated`), without stopping the ring; only the rest get a full look (`rejected` + `accepted`, and
`ev` lines for rejects, at most five a second). Good: `accepted` 0 without a DJI drone, `lapped`
near 0, and the ring capturing most of the time: `isrs` × 3584 / 40,000,000 per second ≥ 0.95
(each full look stops the ring for about 1 ms). Tune with `trig <dB> [min_us max_us]` (default 6 dB,
450–800 µs; the same limits drive the early length gate). On 2.4 GHz (`freq 2414`, DroneID 2414.5)
expect many more `ev` lines; spurs at 2400/2440/2448/2480/2496 MHz are board-made.

## 11. A real DroneID burst

A DJI OcuSync drone with props off and motors armed (bursts only while motors run), a few metres
away. Its DroneID frequencies (RESEARCH.md): 5756.5 / 5776.5 / 5796.5 and 2399.5 … 2459.5 MHz; the
node tunes the whole MHz below (`freq 5756` = default, `freq 5776`, …).

Good: a `burst` line about every 0.6–3 s with `dur_us` 570–650 and `"dec":[{"crc":true,...}]`,
serial redacted, `ms` (decode time on the chip, PSRAM) well under the burst interval. O4 drones
show `"type":"o4_encrypted"` only.

To keep one for the laptop: `mode hold` → the next accepted burst is held (`held` line) →
`dump` (raw `nib8`) and `dump c16` (decoder input) → `resume`. Then:

```sh
cargo run --release -p rawcap-pull -- c5.log --out caps
cargo run --release -p droneid-file -- caps/c5-N.cs16 --rate 40000000 --format ci16 --redact --failures
```

| Symptom | Most likely |
|---|---|
| `burst` lines, `dec` spans fail to sync (`"err":...`) | Mirrored I/Q (step 8), or the sampling edge (step 7). Compare the held `c16` in droneid-file. |
| `burst`, `crc:false` with good `snr_db` | Level: clipping/folding (try lower gain), or the 4-bit path is too coarse here: try the same burst through `snap` (10-bit) timing it by hand, and report both dumps. |
| No `rises` while the drone flies | Wrong frequency, gain too low (floor at the bottom of the window), or the burst falls between hops (O4 hops; expect one every 4–5 s per frequency). |
| `ms` larger than ~600 | PSRAM decode too slow for every burst; fine for 10b, note it for 10d. |

## 12. Soak

Ten minutes of plain detect mode. Good: `overrun` false, `lapped`/`skipped` only around dumps,
`dma_err` 0, `gain_lost` 0, `heap_free` constant, no resets (`up_us` monotonic). Run it on the
**normal** build too (`cargo fw`, no `bringup`): that is what a fielded node runs. Before
reflashing a `bringup` build: `bootloader` (or bake it as the last boot command) so the ROM loader
answers; a normal build has no `bootloader`: hold BOOT while plugging in if the flash will not take.

## Report back

For anything that fails, keep: the boot log, the `phy`/`bus` lines, the `snap` line, any dumps
(`rawcap-pull` output files), the git hash, and which step. A Path A and a Path B dump of the same
scene are the single most useful pair.
