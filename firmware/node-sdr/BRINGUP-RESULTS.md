# node-sdr bring-up: first results (2026-11-12)

Board `c5b`: XIAO ESP32-C5, chip revision v1.0, 48 MHz crystal, 8 MB flash, 8 MB PSRAM, MAC
10:BD:A3:CE:C7:D4. Firmware at commit 17892d7, default features (`cargo fw`). Antenna: the board as
supplied. Logs and dumps are in `bench-logs/node-sdr-20261112/` (local, gitignored).

| Step | Result | Evidence |
|---|---|---|
| 1 Host checks | PASS | rawcap and rawcap-pull tests pass with the real recordings; `cargo fw` builds |
| 2 Flash and boot | PASS | `psram` 8388608 B, receiver 123736 B; `phy` version `109,edb400d6`, `abi_ok`, `gain_forced`, gain 40, `gain_max` 83, `agc_off`, `lpf` [12,12], `txq_off` true; heap_free 82836 |
| 3 PHY / bus | PASS (one oddity) | `out_sel` [112..115,122..125] as designed; `pad_samples` read [255,255,255,255] (expected four different bytes). Path B data is clearly real (see step 7), so the GPIO input read is likely not seeing the debug-bus pads, rather than the bus being dead |
| 4 Path B ring | PASS | with the trigger quiet (`trig 30`): `produced` +40,001,024 B/s, `isrs` +11,154/s, `dma_err` 0, `overrun` false |
| 5 Path A snapshot | PASS | 16,380 words in 827–839 µs (20 MS/s), `gain_word` equals the forced gain |
| 6 Gain | **FINDING** | see below: DC offset grows with gain and rails the ADC before the noise fills the 4-bit window |
| 7 Path A vs B | PASS (shape) | four dumps rebuild (`w32` 65,520 B; three `nib8` 53,760 B for clip/fold/fold5); all spectra show the low-pass roll-off, symmetric about 0, no mirroring, a large DC spike (−7 dB / 0 dB in the two centre bins) |
| 8 I/Q orientation | not run | needs an access point on channel 149 actively sending |
| 9 Window comparison | partly | at gain 65: clip `lvl_db` −12.4, fold −17.2 (6 at rail), fold5 −7.4 |
| 10 Trigger | PASS (short) | at 5756 MHz: `rises` 10–18/s, `ev` lines ≈ 5/s for 80–90 µs Wi-Fi bursts at ~17 dB SNR, `accepted` 0 |
| 11 Real DroneID | not run | needs a DJI drone |
| 12 Soak | not run | |

## Step 6 in numbers (antenna on, 5756 MHz, clip window)

| gain | DC I | DC Q | RMS I | RMS Q | railed |
|---|---|---|---|---|---|
| 20 | −7 | −20 | 0.8 | 0.8 | 0 |
| 40 | −10 | −8 | 0.9 | 0.9 | 0 |
| 55 | −56 | 7 | 1.9 | 2.0 | 0 |
| 65 | −202 | 22 | 5.4 | 5.7 | 0 |
| 75 | −512 | 68 | 0.0 | 20.6 | all (I) |
| 83 | −512 | 156 | 0.0 | 43.5 | all (I) |

A second boot at gain 65 gave DC I 4, Q −149: the offset moves between boots and between I and Q.
The clip window wants noise RMS of 48–80 codes; we only get there at gain ≥ 83, where the DC offset
has already pinned one ADC at its rail. Because it is the ADC that rails, digital DC removal
(`math::remove_dc`) cannot rescue it: the fix has to be analog, in the receiver's own DC-offset
correction (DCOC), which normally tracks the AGC's gain and which we turn off by forcing the gain.

Next for node-sdr, in order:

1. Find how the PHY applies RX DC-offset calibration per gain index on the C5 (esp-sdr /
   esp-sdr-zephyr / C5VRX, and the `phy_*` symbols in the pinned libphy), and re-run it, or
   load the table entry, after `phy_force_rx_gain`. Success: DC within a few tens of codes at gain 75–83.
2. Then repeat step 6 to pick `GAIN_DEFAULT`, and steps 7–9 with an access point on channel 149.
3. Fix the step-4 expectation in BRINGUP.md: `produced` and `isrs` restart at every trigger
   inspection (each rise stops and restarts the ring), so measure the rate with `trig 30`.
