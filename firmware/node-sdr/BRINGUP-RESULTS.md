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

## Step 6 again, after the DC fix (branch `fix/node-sdr-dcoc`)

**Root cause.** Every entry of the PHY's 90-entry RX gain memory (`phy_write_gain_mem`, registers
0x600a0844–0x600a0850) carries analog DC trims for its gain index, built by
`phy_set_rx_gain_table` from RX DC tables in `phy_param` (5 GHz: RF trims at +0x180, seven
reference LOs × 9 RF gain steps × (I, Q); baseband at +0x390). The PHY measures those tables once,
in `phy_bb_init` at boot (`phy_set_rx_gain_cal_dc`, flag 0x80 in `phy_param+0x94`); esp-radio runs no
re-calibration task (nothing links `phy_param_track_tot`). On our boards the boot 5 GHz table is
unusable: read straight after boot it is one value repeated, near the DAC mid-code and different
every boot, e.g. c5b `[366,182]`×9, `[362,204]`×9, `[259..268, 256]`, c5c `[350,373]`… (Q stuck at 256 =
untrimmed). Re-measured later in the same boot it is a proper per-step curve (c5b
`[273,260] … [282,236]`). A forced gain selects an entry with those bad trims, and nothing
re-measures them, so the offset grew with gain until the ADC railed. The AGC is not involved: the
fix keeps it off.

**Fix.** `phy::dc_recal` re-runs `phy_set_rx_gain_cal_dc` at boot and on every `freq` change, with
`phy_chip_set_chan_ana` wrapped (`--wrap`) so all seven bins are measured at the tuned LO, then
`phy_set_rx_gain_table` and our usual `apply` (gain forced, AGC off). The sequence is ESPARGOS
esp-sdr's `rx_recalibrate_locked` for the C5; every offset was re-read from our blob's
`phy_bb_init` and `phy_cal_param_track`. Receive-only: its callees are pbus test forces, the DCO
DAC search, the DC estimator and the digital TX clock gate (no TX chain power-up, no tone); the
loopback IQ step (`phy_set_rx_gain_cal_iq`, which sends a tone) is reached only from `phy_bb_init`
and `phy_cal_param_track`, neither of which we call. Takes 21–25 ms on 5 GHz, 4–5 ms on 2.4 GHz;
`txq_off` stayed true throughout. New console command `dcal [ref | auto on|off]`; the `phy` line
gained `dc_cal`.

5756 MHz, clip window, antenna on, same boot each (firmware built with `dcal auto` off for the
"before" half, then `dcal`). Two `level`s per gain; `dc` is the mean, I/Q.

| gain | c5b before: DC | c5b after: DC | RMS after | c5c before: DC | c5c after: DC | RMS after |
|---|---|---|---|---|---|---|
| 40 | −167, 177 | 2, 0 | 0.9 | −1, 0 | 0, 2 | 0.9 |
| 55 | −224, 205 | 3, 0 | 2.4 | −3, −4 | 0, 2 | 2.0 |
| 65 | −38, −230 | 4…7, 0 | 6.6–7.1 | −7, −14 | −1…2, 6 | 5.9 |
| 75 | 395, **−512 (railed)** | 5…10, 0…4 | 20 | −16, −38 | −5, 15 | 25 |
| 83 | **511, −512 (both railed)** | 0…4, −5…−13 | 48–50 | −52, −95 | −19…−7, 31…33 | 43–44 |

c5c happened to boot with a usable table that time (its next boot did not: `[350,373]`). The final
firmware (`dcal auto` on, default) on both boards, including retunes:

| board | 5756 g40 | g55 | g65 | g75 | g79 | g83 | 5776 g83 | 2414 g55 / g65 / g75 / g83 |
|---|---|---|---|---|---|---|---|---|
| c5b DC | 1, 0 | 3, 0 | 4, −5 | 11, −12 | 22, −27 | 41, −46 / 18, −20 | 9, 6 | 3,0 / −1,6 / 0,8 / −7,13 |
| c5b RMS | 0.9 | 2.5 | 7.0 | 20 | 33 | 50 | 47 | 3.5 / 17 / – / 64 |
| c5c DC | 1, 0 | 1, 0 | 1, −1 | 7, 0 | 8, −1 | 15, 1 / 11, −5 | 11, 44 | −1,1 / 0,2 / 2,7 / 16,10 |
| c5c RMS | 0.9 | 3.1 | 6.2 | 18 | 29 | 44 | 43 | 3.4 / – / 33 / 84 |

No railing from DC at any gain. A `–` is a `level` that caught a real burst (RMS in the hundreds,
`railed` > 0, `dc` still near 0): c5a/s3 were transmitting on the same bench. `gain_max` is 83 on
5 GHz, 81 on 2.4 GHz. Gain steps are about 1 dB. **`GAIN_DEFAULT` is now 83**: noise 43–51 codes
RMS, the nearest any index gets to the clip window's 48–80.

Path A + Path B pair (`gain G;snap;dump;resume;grab;dump`, `rawcap-pull --spectrum`): the centre
bins against the in-band median (1–8 MHz from DC) of the averaged 1024-point spectrum.

| dump | before (c5b, gain 65) | c5b after, g65 | c5b after, g83 | c5c after, g65 | c5c after, g83 |
|---|---|---|---|---|---|
| Path A (w32, 20 MS/s) | DC 4, −149; spike +41.0 dB | DC 3, 3; +14.4 dB | DC 15, −10; +12.7 dB | DC 1, −2; +11.0 dB | DC 15, −12; +10.9 dB |
| Path B (nib8 clip, 40 MS/s) | +29.6 dB | +8.6 dB | +10.8 dB | +7.6 dB | +8.4 dB |

What is left is a few codes of offset (a 3-code DC already makes a 10–14 dB bin over 7-code noise
in a 1024-point FFT); `math::remove_dc` takes it out before decoding. Logs and dumps:
`firmware/node-sdr/bench-logs/dcoc/` (local, gitignored).

Still open: the DC drift with temperature over hours (no re-measurement except on retune or
`dcal`; esp-sdr also re-measures when the PHY's own tracking replaces the tables, which cannot
happen here); why the boot calibration fails on 5 GHz (it runs in `phy_bb_init` before the first
5 GHz channel setup; not needed for the fix); the gain-83 headroom against strong nearby bursts in
the field.

Next for node-sdr, in order:

1. Steps 7–9 at gain 83 with an access point on channel 149, then step 10–12.
2. Fix the step-4 expectation in BRINGUP.md: `produced` and `isrs` restart at every trigger
   inspection (each rise stops and restarts the ring), so measure the rate with `trig 30`.
