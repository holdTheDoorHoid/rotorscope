# DJI radio links: what the public record says, and what Rotorscope fingerprints

PLAN.md Phase 11b. Written 2026-11-15 on branch `dji-fingerprint`. **Receive-only, presence
only**: nothing here demodulates video or control data, handles a key, or transmits. O4 stays
detect-only permanently (PLAN.md Phase 10 decision).

Code: `crates/djilink` (host-tested, `no_std`, fixed-point), wired into `firmware/node-sdr`
(`src/radio/dji.rs`), `crates/link` (protocol 7 `RfKind`s), `crates/tracker`, `crates/screen`.

**Status (2026-11-16): checked against real recordings** (RFUAV: DJI Mini 4 Pro, DJI Avata 2,
DJI FPV Combo, Autel EVO Nano; section "Validation against RFUAV recordings") passed through a
model of the C5's 4-bit capture path, and tuned where they disagreed. Not yet run on the chip
against a live transmitter. The recordings changed two big assumptions: **O4 uses 30 kHz
subcarriers** (not 15 kHz), and **Autel uses DJI's 15 kHz numerology**.

## How to read the evidence labels

- **measured**: someone captured the real signal and reported the number (paper, reverse
  engineering notes). **measured (RFUAV <file>)**: measured here, by `tools/rfuav-check`, on that
  RFUAV recording (section "Validation against RFUAV recordings").
- **filed**: from an FCC test report or a DJI spec page / manual. Test-lab numbers are measured
  too, but in the manufacturer's engineering mode, not in normal flight.
- **snippet**: seen only in a search-engine excerpt of a filing; the full document was not readable
  from here (fcc.report and fccid.io refuse automated fetches). Treat as provisional.
- **inferred**: our reasoning from the above; no source states it.
- **unverified**: a claim from a source we do not trust for numbers (vendor blogs).

## Sources

| Key | Source | What it is |
|---|---|---|
| T | tmbinc, `random/dji/ocusync2/README.md` (github.com/tmbinc/random) | Reverse engineering of OcuSync 2.0 from captures |
| F1 | FCC ID SS3-MT2PD2007, SRD test report RDG200810010-00E (Bay Area Compliance Labs Dongguan, tested 2020-08-31), fccid.io/SS3-MT2PD2007/Test-Report/Test-Report-SRD-4941236.pdf | DJI OcuSync 2 aircraft (model MT2PD), 5.8 GHz, every SDR mode |
| F2 | FCC IDs SS3-MT2WD2007 (Mini 2), SS3-M3E2206 (Mavic 3E) test reports on fcc.report | 2.4 GHz uplink channel edges (snippet) |
| F3 | FCC ID SS3-DF3P290424 (O4 Air Unit Pro), Part 15E test report, fcc.report/FCC-ID/SS3-DF3P290424/7880296.pdf | O4 bandwidth modes at 5.8 GHz (snippet) |
| I | H. Lee, J. Jeong, I. Sim, S. Yun, J. Seo, H. Nam, "Real-Time OcuSync Demodulator: A Software-Defined Radio Based Implementation", ICTC 2025, pp. 1857-1859 | Uplink (controller) numerology for OcuSync 1/2/3, from USRP captures |
| A1 | arXiv 2505.18167, "Dim and Small Target Detection for Drone Broadcast Frames Based on Time-Frequency Analysis" | DroneRFa-based; DroneID timing and generic uplink ranges |
| A2 | arXiv 2504.02844, "Drone Remote Identification Based on Zadoff-Chu Sequences and Time-Frequency Images" | DroneRFa-based; Mini 3 Pro downlink frame |
| D1 | dji.com/o4-air-unit/specs | O4 Air Unit / Pro channels, bands, widths |
| D2 | DJI O3 Air Unit support page and user manual ("Operating channel") | O3 Air Unit channels |
| D3 | DJI FPV system: Vistek and RunCam listings, oscarliang.com DJI FPV FAQ; RESEARCH.md (2026-11-09) for the FCC list | V1 / Vista channels |
| D4 | DJI spec pages: Air 3, Mini 4 Pro (O4); Mini 3 Pro, Mavic 3 Pro (O3/O3+) | Which products use 5.1 GHz |
| P | proto17/dji_droneid; EdwardBlair/dji-droneid-spec (via RESEARCH.md) | DroneID frequencies |
| R | Environment. Technology. Resources (Rezekne) 2025 proceedings, doi 10.17770/etr2025vol5.8486: HackRF analysis of Air 3 and Phantom 4 | Qualitative only (no timing numbers) |
| V | counteruavhub.pages.dev (vendor blog) | **Not used for any number**: its figures contradict each other |

## 1. The video downlink (drone or air unit to controller / goggles)

| Parameter | Value | Label | Source |
|---|---|---|---|
| Waveform | OFDM, LTE-like but not LTE; ZC reference symbols, no obvious pilots | measured (OcuSync 2) | T, A2 |
| Subcarrier spacing | 15 kHz (useful symbol 66.67 µs) | measured (OcuSync 2) | T |
| 20 MHz mode | 2048-point FFT at 30.72 MS/s, 1201 subcarriers (incl. constant DC) = ~18 MHz | measured (OS2; Mini 3 Pro in A2) | T, A2 |
| 20 MHz mode occupied bandwidth | 99 %: 18.00-18.08 MHz; 6 dB: 18.08-18.16 MHz | filed | F1 |
| 10 MHz mode | 601 subcarriers; 99 %: 9.32-9.40 MHz; 6 dB: 9.04-9.12 MHz | measured / filed | T, F1 |
| Cyclic prefix | 144 samples at 30.72 MS/s = 4.69 µs (LTE normal); 160 (5.21 µs) on the first, middle and last symbols | measured (OS2) | T |
| Packet | 15 symbols, RS0, 6 data, RS1, 6 data, RS0: ~1.07 ms | measured (OS2; Mini 3 Pro "15 symbols, ~1 ms") | T, A2 |
| Other modes | 1.4 MHz, 3 MHz (2.2-2.4 MHz OBW), and "1.4 MHz-CA" | filed | F1 |
| Channel raster (aircraft) | 1 MHz steps at x.5 MHz: 10 MHz mode 5732.5-5844.5 (113 ch), 20 MHz mode 5735.5-5839.5 (105 ch) | filed (OS2) | F1 |
| Duty cycle | 100 % in the lab's test mode | filed, **not representative** (engineering mode) | F1 |
| Duty cycle in flight, TDD frame | O4: a **2.5 ms frame** (2.498-2.499 ms start to start); one video burst of 0.45-0.6 ms per frame (~20 % duty on the video channel) and one uplink burst 0.49 ms before it; the drones' flight state during recording is not documented. OcuSync 2/3, O3: not measured | measured (RFUAV Mini 4 Pro, Avata 2) | |
| O3 aircraft numerology | assumed the same 15 kHz / normal CP family | **inferred** (O3 Mini 3 Pro frame matches, A2) | |
| O4 numerology | **30 kHz subcarriers**: useful symbol 33.33 µs, period 35.7 µs (prefix ~2.4 µs), video and uplink both, in the 10, 20, 40 and 60 MHz modes, at 2.4 and 5.8 GHz. Bursts open with 1-3 symbols off that grid (gaps of 33, 44, 47 µs). O4's DroneID burst stays 15 kHz (decoded) | measured (RFUAV Mini 4 Pro, Avata 2) | |
| O4 occupied width (C5 model, -10 dB) | 10 MHz mode 9.0-9.1 MHz (about 301 subcarriers at 30 kHz); 20 MHz mode ~17 MHz; 40/60 MHz modes wider than the capture | measured (RFUAV Mini 4 Pro, Avata 2) | |
| O4 wide modes | O4 Air Unit (Pro): max 60 MHz; manual 60/40 MHz on 5794.5, 20/10 MHz on 5768.5, 5789.5, 5814.5 | filed | D1 |
| O4 Air Unit Pro filing | sections for 20/40/60/80 MHz at 5.8 GHz; 40 MHz 5745.5-5829.5, 60 MHz 5755.5-5819.5 | snippet | F3 |
| O4 subcarrier spacing in 40/60 MHz | **30 kHz** (lag peak at 33.32 µs, 68x its median in the 40 MHz pack) | measured (RFUAV Avata 2 VTSBW=40, VTSBW=60) | |
| O4 video channels seen | Mini 4 Pro 2450.5 MHz (both modes); Avata 2 5763.5 (10 MHz), ~5803 (20), ~5788 (40), ~5776.6 (60) | measured (RFUAV) | |
| O3 Air Unit | max 40 MHz; manual 40 MHz on 5794.5, 20/10 MHz on 5768.5, 5804.5, 5839.5 | filed | D2 |
| 5.170-5.250 GHz | listed by O4 products (O4 Air Unit, Air 3, Mini 4 Pro); not by O3/O3+ (Mini 3 Pro, Mavic 3 Pro, O3 Air Unit) | filed | D1, D4 |
| Autel overlap | EVO Lite/Max also file 5.15-5.25 GHz with 1.4/10/20 MHz LTE-like modes of the same widths (RESEARCH.md) | filed | RESEARCH.md |
| Autel numerology and shapes | EVO Nano: **15 kHz, period 71.4 µs, prefix ~4.7 µs: DJI OcuSync's numerology**. ~10 MHz bursts of 0.5-2.4 ms stepping over ten centres (5733.1, 5742.0, 5751.3, 5760.1, 5769.0, 5777.8, 5786.8, 5796.0, 5805.0, 5813.6; ~8.85 MHz apart), one centre per 40 ms; ~1.7 MHz bursts of 1.31 ms (with a 0.33 ms burst 0.41 ms before) and of 0.29 ms on whole-MHz centres, in 25 ms slots | measured (RFUAV DAUTEL EVO NANO) | |

## 2. The controller uplink (controller to drone)

| Parameter | Value | Label | Source |
|---|---|---|---|
| Waveform | OFDM, 15 kHz, ZC sequences for timing; QPSK | measured | I, T |
| OcuSync 1/2 | 128-point FFT at 1.92 MS/s, ~73 subcarriers (~1.1 MHz), 6 OFDM symbols a frame (~0.43 ms), 576 bits | measured | I |
| O3 | 256-point FFT at 3.84 MS/s, 145 subcarriers (~2.2 MHz), 7 symbols (~0.5 ms), 1440 bits | measured | I |
| O4 | **30 kHz** (useful 33.30 µs, period 35.7 µs), ~4.4 MHz at -10 dB in the C5 model (5.2 MHz mean span at full rate; 6.3-6.7 MHz in the Avata 2's 40 MHz mode): about O3's 145 subcarriers at twice the spacing. 0.40-0.45 ms bursts (11-12 symbols plus an odd head), one per 2.5 ms frame, 0.49 ms before the frame's video burst | measured (RFUAV Mini 4 Pro, Avata 2) | |
| "1.4 MHz-CA" mode (the hopping uplink, per T) | 59 channels, 2 MHz raster at x.12 MHz: 5730.12-5846.12 MHz; 6 dB 1.136-1.152 MHz, 99 % 1.164-1.200 MHz | filed (OS2) | F1, T |
| 2.4 GHz uplink channels | Mini 2 1.4M-CA edges 2409.12 / 2467.12 MHz; Mavic 3E 1.4M-CA to 2471.12, 3M-CA from 2408.2 | snippet | F2 |
| Hop dwell, period, sequence | OcuSync 2/O3: not published (generic DroneRFa-era range 0.4-5 MHz wide, 0.5-5 ms bursts, A1). **O4**: one burst per 2.5 ms frame; the centre changes frame to frame. Mini 4 Pro (2.4 GHz) within a 5-6 MHz set for hundreds of ms (10 MHz mode: 2435.75, 2438.6, 2439.5, 2440.3; 20 MHz mode: 2459.1-2465.2 first, later 2404.6-2438.7); Avata 2 (5.8 GHz) on four centres 20 MHz apart (10 MHz mode 5742.7/5762.7/5782.7/5802.4; 40 MHz mode on x.5: 5732.5-5802.5) | measured (RFUAV Mini 4 Pro, Avata 2) | A1 |

## 3. DJI FPV air units

| System | Channels | Width | Label | Source |
|---|---|---|---|---|
| DJI FPV V1/V2 goggles, Air Unit, Caddx Vista, DJI FPV drone | 5660, 5695, 5735, 5770, 5805, 5878, 5914, 5839 (public ch 8); CE/SRRC 5735/5770/5805/5839; MIC 5660/5700/5745 | 20 MHz per channel at 25 Mbps; 50 Mbps takes about two channels | filed (FCC list via RESEARCH.md); widths secondary. **The DJI FPV drone measured on 5770.50 (10/20 MHz) and 5745.50 (40 MHz)**: the listed whole-MHz values look rounded (RFUAV DJI FPV COMBO) | D3 |
| O3 Air Unit | 5768.5, 5804.5, 5839.5 (10/20); 5794.5 (40) | 10/20/40 | filed | D2 |
| O4 Air Unit / Pro | 5768.5, 5789.5, 5814.5 (10/20); 5794.5 (40/60); also 5.170-5.250 GHz | 10/20/40/60 | filed | D1 |
| DJI FPV drone (FPV Combo) channel, measured | its video sat on **5770.50 MHz** in the 10 and 20 MHz modes (full-rate -6 dB edges 5766.10-5774.89 and 5761.61-5779.39): the x.5 raster, not the whole-MHz "5770" of the channel list, which is probably rounded. Vista / Air Unit V1 not recorded | measured (RFUAV DJI FPV COMBO) | |
| DJI FPV drone PHY | **15 kHz, normal prefix** (lag 66.68 µs; DJI's OcuSync numerology). Video bursts 0.16-3.5 ms (median 2.1-3.1 ms), ~70 % duty on the channel. Uplink O3-like: 15 kHz, 2.2-2.3 MHz at -10 dB (C5 model), 0.53 ms (7 symbols), two bursts per 10 ms (gaps alternating 4.0 and 6.0 ms) cycling over a few centres (5729.4, 5731.4, 5735.5, 5745.3 MHz in one pack). DroneID: unencrypted flight-info records (product "DJI FPV"), decoded at 5796.5 | measured (RFUAV DJI FPV COMBO) | |
| Walksnail Avatar | reportedly the same 8-channel plan, Artosyn LTE-style chip | secondary / inferred | RESEARCH.md |

## What each detector looks for

All need **a DJI numerology first**: 15 kHz OFDM with a normal (4.7 µs) cyclic prefix (OcuSync
2/3, O3, DroneID), found by folding `x[n] conj(x[n+1333])` at the 71.4 µs symbol period so the
prefixes of every symbol add coherently, or **30 kHz with a normal prefix (O4, measured)**, the
same fold at 33.3/35.7 µs. Wi-Fi (3.2 µs symbols), analog FM video, carriers and hobby FSK links
have neither. The fold searches the prefix where a seen burst start predicts it and, failing that,
everywhere (higher threshold): real O4 bursts start off their own grid.

| Rotorscope kind (screen / phone label) | What must hold | Confidence in the public basis |
|---|---|---|
| `DjiVideo` ("DJI video") | 15 kHz; >= 7 MHz wide at -10 dB and >= 5 MHz at -3 dB; not a lone DroneID-shaped burst; a measured centre on the x.5 raster (or unknown: clipped) | Good for OcuSync 2/3 (measured). **Autel uses the same numerology (measured, EVO Nano)**: off-raster centres now go to `LteStyle`; 2 of the Nano's 10 centres and clipped captures still read DJI |
| `DjiO4Video` ("DJI O4 video") | **30 kHz**, >= 7 MHz at -10 dB and >= 5.5 MHz at -3 dB (or a burst of its own >= 8.8 MHz), any band; or the 15 kHz shape centred in 5.170-5.250 GHz | 30 kHz: measured on every O4 recording, not seen from Autel or anything else in them. 5.1 GHz 15 kHz rule: filed only |
| `DjiFpvAirUnit` ("DJI FPV air unit") | 15 kHz video 7-12 MHz wide, whole signal inside the capture, centre within 200 kHz of a whole-MHz DJI FPV channel and off the x.5 raster | **Not confirmed**: the one DJI FPV system recorded (FPV Combo) sat on 5770.50, so it reads `DjiVideo`; in the recordings only Autel (5805.0) and a mis-measured 20 MHz centre ever met this rule (the latter now excluded). Kept for Vista / Air Unit V1, unrecorded |
| `DjiController` ("DJI controller") | a whole 0.2-0.8 ms burst (cut ones do not count), 0.6-3 MHz wide at 15 kHz or 2.0-8.7 MHz at 30 kHz (the burst's own width counts), not continued on its channel by a cut or longer burst, and no video-width burst in the capture; every such burst (whatever the capture's verdict) feeds the hop tracker, which confirms on two centres >= 1 MHz apart within 6 s | O4: measured (RFUAV). OcuSync 2/O3: width and symbol count measured (I) and filed (F1), timing unmeasured. Autel EVO Nano's 0.29 ms 1.1 MHz 15 kHz bursts still pass (residual, see validation) |
| DroneID burst (reported as `LteStyle`) | 8-10.5 MHz, one whole 0.45-0.8 ms burst, dwell otherwise nearly silent | Measured (A1, P); the DroneID job decodes these properly |
| `LteStyle` (existing) | 15 kHz OFDM that fits none of the above, or an extended prefix | |

What is **not** separable from one 20 MHz capture, and why:

- **O3 vs O4 on 5.8 GHz**: now separable by numerology (O4 measured 30 kHz; O3 assumed 15 kHz,
  not yet measured: no O3 recording was used). The DroneID job's O4 record (encrypted, key ID
  only) remains a second O4 marker.
- **Drone vs O3/O4 air unit**: both use the x.5 raster; not separable.
- **DJI vs Autel**: O4 vs Autel is separable (30 vs 15 kHz, measured). OcuSync 2/3 and O3 vs
  Autel share the numerology (measured); what separates them is shape and raster (Autel's 1.31 ms
  narrow bursts and its stepping ~10 MHz bursts off the x.5 raster), which the rules now use, with
  the residuals listed under the EVO Nano results.
- **DJI FPV V1 vs Walksnail**: same channel plan (reportedly). Needs recordings.
- **The C5 sweep and 20 MHz modes**: the sweep puts the LO 1 MHz under each analog channel, so an
  18 MHz signal is usually cut at one edge of the capture. Then the centre is unknown and the
  channel-plan tests (V1 raster) do not run; the 10 MHz modes fit.

## Implementation notes

**Why a new crate (`crates/djilink`) and not more of `crates/fpvscan`.** fpvscan is the analog
scanner and is being changed in parallel (2.4 GHz channels); djilink makes zero edits to it, so
the two merge cleanly. djilink needs its own working memory anyway: a 4x finer spectrum (78 vs
312 kHz groups, to see a 1.1 MHz uplink and place an edge to tens of kHz), burst segmentation and
the folded prefix sums. It depends only on `droneid` (FFT, `C16`), like fpvscan, and runs after
fpvscan on the same capture.

**Per capture** (`djilink::features`): a 1.6 µs envelope split into bursts (each burst's own
centre and width from one FFT); 16 FFTs over the bursts averaged into 256 groups (widths at -3,
-10, -20 dB; -6 dB edges interpolated in dB; centre); the prefix fold for 15 kHz normal,
15 kHz extended and 30 kHz normal prefixes, each against two control lags 1.85 µs away and a
threshold scaled by the signal's degrees of freedom (bandwidth x prefix samples). When the
capture saw a burst begin, the prefix search is first limited to the timing that start predicts
(lower threshold), which is what makes a 0.45 ms, 1.1 MHz uplink burst detectable at all; if that
fails, the best window anywhere is tried against the higher threshold (real O4 bursts start off
their own grid). Both reuse the same three folds; no extra memory.

**Per dwell** (`djilink::Timeline`): every trigger probe's level (one byte, half-dB steps), split
at the end of the dwell against the sweep's floor: burst count, median length and period, duty,
and the level of the loudest 1 % (so a short-burst controller dwell gets analysed even though its
90th-percentile level, which fpvscan judges by, is the floor).

**Memory (state RAM)**: `Scratch` 12,928 B (heap, internal RAM, allocated once); `Timeline`
1,538 B and `HopTracker` 80 B inside the radio state; about 2 KiB of stack during analysis
(a sorted copy of the spectrum). No allocation in the analysis. Check the boot `mem` line on the
first flash: the node's heap also carries fpvscan's 26 KiB scratch and the Wi-Fi driver.

**Time**: not measured on a chip. About 20 FFTs of 1024 points plus nine complex
multiply-accumulate passes over the capture with 64-bit sums. fpvscan's analysis took ~100 ms on
the bench, dominated by its 64-bit sums; expect the same order here (the `fpv` line logs
`dji_us`). The ring is stopped meanwhile. Optimisation candidates: 32-bit products (4-bit-derived
samples are small), skipping the extended fold when the normal folds are far from their
thresholds. (The 30 kHz fold can no longer be skipped: it is O4's numerology.)

**Wire format**: protocol 7 appends four `RfKind`s (older kinds keep their postcard indices;
`crates/link` test `protocol_7_rf_kinds_append_only` pins them). A protocol-6 hub counts an `Rf`
frame with a new kind as corrupt and carries on. The tracker treats every `DjiController` sighting
within 150 MHz as one emitter (it hops); the node rate-limits it as one emitter too. `DjiO4Video`
now also means 30 kHz video anywhere, not only on 5.1 GHz; no wire change.

## Validation against RFUAV recordings

Done 2026-11-16 on branch `rfuav-validate` with `tools/rfuav-check`. Four RFUAV recordings
(Apache-2.0; huggingface.co/datasets/kitofrank/RFUAV) were downloaded one at a time, streamed
out of the archive and deleted; only derived feature files were kept, locally (`rfuav-results/`,
gitignored). Nothing from the recordings is committed: the regression tests use synthetic
signals shaped to the measured numbers.

### The RFUAV format

- One `.rar` per aircraft; inside, a folder per video bandwidth setting (`VTSBW=10`, `=20`,
  `=40`, `=60`), each with `packN.xml` and one-second files `packN_<a>-<b>s.iq`.
- `.iq`: interleaved little-endian `f32` I, Q ("Complex Float" in the XML), **100 MS/s**,
  100,000,000 samples (800,000,000 bytes) per file; recorded with a USRP X310
  (`DeviceType`), `IFBandwidth` 100 MHz. The XML gives `CenterFrequency`, `SampleRate`,
  `ReferenceSNRLevel` (dB) and a `ScaleFactor` (unused here). The RFUAV repository's
  `graphic/RawDataProcessor.py` reads them the same way (`np.fromfile(..., float32)`,
  `data[::2] + 1j * data[1::2]`).
- The archive stores members in name order (`0-1s`, `1-2s`, `10-11s`, `2-3s` ...): stream them in
  time order. Some packs have gaps (Avata 2 `VTSBW=10` has no `4-5s`).
- Centres used: Mini 4 Pro 2450 MHz (both packs: 2.4 GHz only), EVO Nano 5770, Avata 2
  5760/5765/5760/5760 (10/20/40/60), FPV Combo 5760 (10/20/40).

### Method (`tools/rfuav-check`)

- `run`: per 60 ms (one FPV dwell) of input, (a) a **full-rate survey**: spectrogram cells
  (1024-point FFTs, 41 µs x 195 kHz) over the recording's noise joined into time-frequency tracks,
  one CSV row per burst (start, length, centre, width, level) plus, per burst, the
  autocorrelation over 2-80 µs of lag (the symbol length without assuming one); (b) the **C5
  model** per LO: every 2.4/5.8 GHz sweep channel the recording covers (LO = channel - 1 MHz) and
  the DroneID LOs, the 20 MHz slice resampled to 40 MS/s, scaled to ADC codes, the C5's receiver
  noise added (48 codes RMS), quantised by `rawcap`'s clip window, then the node's own dwell logic
  (trigger probes, `fpvscan` meter and floor, timeline, one capture at the first rise, `fpvscan`,
  `djilink`, exactly as `radio.rs` / `radio/dji.rs`), plus `djilink` on every 8th or 4th
  capture-length window of the dwell (4-bit path and a clean path without noise or
  quantisation); (c) **DroneID** decoding at the node's DroneID LOs on both paths (serials,
  UUIDs and positions are never written, only CRC status, record type and model).
- Gain: the recording's own noise is put 6 dB under the C5's, capped so the loudest LO's
  90th percentile is at most 25 dB over it (the C5 runs at a fixed gain). A first version put the
  loudest LO at +15 dB; on the EVO Nano that lifted the USRP's noise 9 dB over the C5's, which the
  model saw as a flat wideband signal (`fpvscan` called it HDZero and an FPV carrier).
- `summary`: burst classes, raster, timing, lag peaks; per-LO verdicts; a **sweep simulation**:
  the node's built-in 46-channel sweep, one 60 ms dwell per channel, started at each of the 46
  phases, through the real `HopTracker`.
- `probe`: one burst's symbol structure at full rate (best lag in 0.01 µs steps, then the prefix
  peaks at that lag: their spacing is the symbol period, their width the prefix).
- `replay`: the dwell logic again over saved 4-bit dwells (to re-check a rule change).
- Limits: a model of the C5, not the chip; the recordings carry their own noise (low-SNR packs
  stay low), nearby Wi-Fi and Bluetooth (2.4 GHz), and in one case other drones' signals; the
  aircraft's state while recorded (linked, flying, streaming) is not documented.

### DJI Mini 4 Pro (O4), 2.4 GHz (`DJI MINI4 PRO.rar`, 2 x 7 s)

- **Video**: 2450.5 MHz in both modes (x.5 raster; C5 centre 2450.485). **30 kHz OFDM**: lag
  peak 33.33 µs (25-37x its median at full rate); `probe`: regular symbols every 35.4-36.3 µs
  (mean 35.7), prefix ~2.4 µs, and 1-3 head symbols off that grid (gaps 33, 44, 47 µs). 10 MHz
  mode 9.06 MHz at -10 dB in the C5 model; 20 MHz mode ~17 MHz. One 0.45-0.5 ms burst per
  2.498 ms frame (C5 timeline duty 16-23 %).
- **Controller uplink**: 30 kHz (33.30 µs), 4.4 MHz at -10 dB (C5), 0.40-0.45 ms, one per
  frame, 0.49 ms before the video burst; centres change frame to frame within a few MHz (see
  section 2).
- **DroneID**: O4 encrypted records (INFP-10/11), CRC-valid, at 2414.5, 2429.5 and 2444.5 MHz:
  6 and 4 bursts in the two 7 s packs, the C5 4-bit path decoding exactly what the clean path did.
- **Detectors before tuning**: video 0 of 117 dwells on its LOs (djilink said `Ofdm30` on a
  quarter, nothing reportable; `fpvscan` "energy", unreported in the ISM band); controller never
  confirmed. **After** (final rules, 3 s replays of the same 4-bit dwells): O4 video on 46/50
  and 47/50 dwells (10 MHz mode), 50/50 and 50/50 (20 MHz); controller confirmed in 18 and 7 of
  46 sweep phases within the 3 s replayed (about one sweep), median 1.4 s.

### Autel EVO Nano, 5.8 GHz (`DAUTEL EVO NANO.rar`, 12 s)

- 15 kHz numerology (useful 66.67 µs, period 71.4 µs, prefix ~4.7 µs): **the same as DJI's
  OcuSync** (`probe`). Shapes: see section 1 ("Autel numerology and shapes"). No DroneID
  (nothing CRC-valid). The wide bursts also show a lag peak at 11.08 µs (36x) that DJI's do not;
  not used.
- **Detectors before tuning** (full 12 s, sweep simulation over 46 phases): a "DJI controller"
  confirmed in **46 of 46** phases (median 2 s), 190 "DJI video" and 6 "DJI FPV air unit"
  reports. **After** (full 12 s, rules of commit `bea5aad` with the 500 kHz same-channel test):
  controller confirmed in 6 of 46 phases (after 5-10 s), 114 "DJI video", 6 "DJI FPV air unit",
  1,250 `LteStyle`. Final rules, 3 s replay: controller never confirmed, 20 "DJI video" against
  240 `LteStyle` reports.
- Residuals (Autel still called DJI): standalone 0.29 ms, 1.1 MHz, 15 kHz bursts are exactly an
  OcuSync 2 uplink burst's shape (only a raster rule could separate them: DJI's filed 1.4M-CA
  raster is x.12 on 2 MHz, Autel's centres were whole MHz; not adopted without a DJI 15 kHz uplink
  recording to confirm it); ~10 MHz bursts on 5805.0 (a DJI FPV V1 channel) and 5813.6 (x.5 raster)
  or clipped by the capture's edge.

### DJI Avata 2 (O4), 5.8 GHz (`DJI AVATA2.rar`, 4 packs, 4-5 s each)

- Video 30 kHz in all four modes (10/20/40/60 MHz): 5763.5 (10), ~5803 (20), ~5788 (40),
  ~5776.6 (60). Uplink 30 kHz, 0.45 ms, one per 2.499 ms frame, on four centres 20 MHz apart
  (10 MHz mode 5742.7, 5762.7, 5782.7, 5802.4; 40 MHz mode on x.5 MHz: 5732.5 ... 5802.5,
  6.3-6.7 MHz wide at full rate, up to 8.5 MHz at -10 dB in the C5 model: the reason for the
  2.0-8.7 MHz O4 controller width).
- DroneID: O4 encrypted records (INFP-10/11), CRC-valid on both paths, at 5741.5, 5751.5, 5756.5,
  5761.5, 5771.5, 5776.5, 5791.5 and 5796.5 MHz: **a 5 MHz raster**, wider than the centres listed
  in `plan::DRONEID_KHZ` (5741.5/5756.5/5776.5/5796.5, +5816.5). The node's DroneID LOs still
  decoded the others at +-5 MHz offsets inside their 20 MHz captures. 15 kHz DroneID-shaped bursts
  at 5746.2 did not decode.
- Three of the four packs are weak (reference SNR 9, 2 and 1 dB): through the C5 model their video
  stands ~5 dB over the receiver's noise, under the node's 8 dB analysis gate, so most dwells are
  not analysed. Where analysed, the verdicts are right. The 40 MHz pack (SNR 23, final rules,
  3 s replay): O4 video on 49 of 50 dwells at LOs 5779-5789; at LO 5770, where the video's lower
  edge and the 5772.5 uplink share the capture, a controller burst on 50 of 50; controller
  confirmed in 46 of 46 sweep phases (median 1.0 s).

### DJI FPV Combo, 5.8 GHz (`DJI FPV COMBO.rar`)

Three packs: `VTSBW=10` (pack1, 13 s), `VTSBW=20` (pack3), `VTSBW=40` (pack2); 5 s of each used.
All centred 5760 MHz.

- **Video: 15 kHz** (lag peak 66.68 µs, 32-56x its median), on **5770.50 MHz** in the 10 and
  20 MHz modes (full-rate -6 dB edges 5766.10-5774.89 and 5761.61-5779.39; 8.8 and 17.8 MHz).
  So the DJI FPV system is not on a whole-MHz channel: "5770" is 5770.5, on the OcuSync x.5
  raster, and it reads `DjiVideo`. Bursts are long (0.16-3.5 ms, median 2.1-3.1 ms) and fill
  ~70 % of the channel's time.
- **Controller uplink: O3-like**, 15 kHz, 2.2-2.3 MHz at -10 dB in the C5 model (ICTC's O3:
  145 subcarriers, 2.2 MHz), 0.53 ms (7 symbols), two bursts per 10 ms (gaps alternating 4.0 and
  6.0 ms), cycling over a few centres (5729.4, 5731.4, 5735.5, 5745.3 in pack1; 5729.2, 5730.8,
  5735.5, 5738.2, 5793.3 among others in pack3).
- **DroneID**: unencrypted flight-info records, product "DJI FPV", CRC-valid on both paths, at
  5796.5 MHz (2 in 5 s of pack1). Their contents (serial, positions) were never written out.
- **Detectors** (after tuning; 5 s, sweep simulation over 46 phases): 10 MHz mode: `DjiVideo` on
  83/84 dwells at LOs 5768 and 5770 (half-clipped LOs: `LteStyle`); controller confirmed in 46
  of 46 phases (median 1.3 s). 20 MHz mode: `DjiVideo` on 80-84/84 dwells at four LOs;
  controller confirmed in 37 of 46 (median 2.1 s). Before the raster-width gate the 20 MHz video
  at LO 5770 was called a DJI FPV air unit on 29 dwells (its centre read 5770.1-5770.4).
- **40 MHz mode** (pack2): still 15 kHz, 35.7 MHz wide at -6 dB, centred **5745.50 MHz** (x.5
  again). Through the 20 MHz capture it is always clipped: `DjiVideo` on 63-80 of 84 dwells at the
  seven LOs inside it; controller confirmed in 38 of 46 phases (median 2.3 s); 5 CRC-valid
  DroneID flight-info records at 5796.5 MHz in 5 s.

### What changed in `crates/djilink`, and why

| Change | Why (measured) |
|---|---|
| 30 kHz normal-prefix OFDM is an O4 numerology: of video width `DjiO4Video` on any band, of uplink width (2.0-8.7 MHz, whole 0.2-0.8 ms burst) a controller burst | every O4 signal recorded is 30 kHz; before, O4 video and uplink were never reported |
| The prefix fold also searches every window (higher threshold) when the window the burst start predicts fails | O4 bursts open with symbols off the regular grid; whole bursts measured 100-250 where the free search finds 600-900 |
| O4 video needs >= 5.5 MHz at -3 dB, or a burst of its own >= 8.8 MHz | strong 40 MHz-mode uplink bursts read up to 8.5 MHz at -10 dB |
| A capture holding a video-width burst is video; controller-shaped bursts feed the hop tracker whatever the verdict (`classify::controller_bursts`, `radio/dji.rs`) | O4 uplink and video share a 2.5 ms frame, so one capture often holds both; before, the video burst's centre was fed to the tracker as a "hop" |
| Controller bursts must be whole and at most 0.8 ms (was 1.3 ms, cut allowed), and not continued on their channel by a cut or longer burst (`same_channel_continues`, 500 kHz) | Autel's 1.31 ms narrow bursts, and its 0.33 + 1.31 ms pairs |
| 15 kHz video with a measured centre on neither DJI raster is `Ofdm15` (LTE-style), not DJI | Autel's stepping ~10 MHz bursts: 8 of 10 centres off both rasters |
| The raster tests use a measured centre only for signals up to 12 MHz wide | the FPV Combo's 20 MHz video (true 5770.50) measured 5768.4-5770.4 in the C5 model, its edges in the half-band's roll-off, and was called an FPV air unit |
| A burst ending (starting) within a merge gap of the capture's end (start) counts as cut | a 1.7 MHz burst's envelope dips at the edge; one running past the end read as whole |
| Hop tracker window 3 s -> 6 s | the 46-channel sweep takes ~2.8 s plus retuning; the O4 uplink's next centre is often seen only on the next visit |

## Test results (synthetic signals)

`cargo test -p djilink`: 22 tests (19 before the RFUAV validation; three regression tests added
from its measurements, and five existing tests updated where the recordings proved their
assumption wrong, as noted). Signals are built per symbol by inverse FFT at each system's
own rate (30.72 MS/s / 2048 for 20 MHz video, 15.36 / 1024 for 10 MHz video and DroneID,
1.92 / 128 and 3.84 / 256 for the uplinks) with the documented prefixes, interpolated to 40 MS/s,
plus the C5's receiver noise, quantised to 4 bits by the real `rawcap` window and half-banded to
20 MS/s by the real `rawcap` filter.

- 20 MHz and 10 MHz video, continuous and with gaps, several offsets: always `DjiVideo`; centre
  within 60 kHz on 10 MHz signals. (Updated: the 10 MHz centres are now on the x.5 raster, and
  the same signal on a whole MHz or x.25 is `Ofdm15`: Autel's video-width bursts sit there.)
- On a V1 channel (5770.0) `DjiFpvAirUnit`; 1.5 MHz lower (5768.5, x.5 raster) `DjiVideo`; a
  clipped 20 MHz signal never gets an air-unit claim. (Updated: a 20 MHz signal centred on 5735.0
  inside the capture is now `DjiVideo` too: an 18 MHz signal's measured centre is not trusted for
  the raster.)
- 5.2 GHz band (10, 20 and a 15 kHz model of 40 MHz): `DjiO4Video`; the 40 MHz model on 5.8 GHz:
  `DjiVideo`.
- DroneID bursts: DroneID, not video (unless the dwell is busy). (Updated: the second burst's
  centre moved from 5755.0 to 5754.5, on the x.5 raster like DroneID's own.)
- OcuSync 2 and O3 uplink bursts at four offsets, two levels, six timings: all controller bursts,
  burst centre within 150 kHz; two hops in one capture: two centres, confirmed. (Updated: every
  burst wholly inside the capture; a cut burst no longer counts, because Autel's 1.31 ms bursts
  cut by the capture looked like one.)
- **From RFUAV** (`o4_video_measured_shape`, `o4_uplink_measured_shape`,
  `autel_evo_nano_measured_shapes_are_not_dji`): O4 video (30 kHz, 301 subcarriers, 0.6 ms burst
  opened by two symbols off the grid) at 2449 and 5770 MHz: `DjiO4Video`, centre within 100 kHz,
  16 of 16 found only by the free prefix search; frames at random phase never another DJI kind.
  O4 uplink (30 kHz, 145 subcarriers, 0.43 ms) at three offsets: controller burst, centre within
  300 kHz; uplink + the frame's video burst 0.49 ms later in one capture: `DjiO4Video`, the uplink
  still handed to the hop tracker. Autel-shaped narrow pairs (0.33 ms + 1.31 ms, 1.7 MHz,
  15 kHz), captured from the first burst or mid-burst: never a controller burst; Autel-shaped
  ~10 MHz bursts at eight measured centres: `Ofdm15`.
- Carrier offsets -250..+240 kHz: no effect on detection; centre follows the offset.
- **Negatives** (never a DJI kind): 802.11a and 802.11ax Wi-Fi at three traffic levels (largest
  15 kHz metric 79 under its threshold), FM analog video (PAL/NTSC, three pictures), a carrier,
  an HDZero-like flat signal without prefix, silence over 40 captures (108 under), GFSK hopping
  bursts like a hobby RC link (127 under).
- LTE extended prefix: family `Ofdm15`, prefix measured >= 250 samples; DJI's prefix measured
  64-128 samples (true 94). 30 kHz OFDM of video width: `DjiO4Video` (updated from `Ofdm30`: the
  recordings show it is O4).
- Sensitivity (total signal power over the noise in the 20 MHz capture): 20 MHz video detected
  4/4 down to -0.6 dB and 3/4 at -2.7 dB; O3 uplink down to +1.9 dB; OcuSync 2 uplink down to
  +4.4 dB (that is ~17 dB in its own 1.1 MHz). Weak signals give no verdict, never a wrong one.

Whole workspace: `cargo test --workspace` 634 passed after the RFUAV validation (629 before it:
three new djilink tests, two in `tools/rfuav-check`). `cargo fw`, `cargo fw-lite`, `cargo fw-now`
(node-sdr) and `cargo fw` (hub-p4) build. Not flashed.

## What would validate it further

1. **Recordings.** Done: Mini 4 Pro, Avata 2, FPV Combo, EVO Nano (section "Validation against
   RFUAV recordings"; all four deleted after use). Still useful, from the same dataset (the owner
   approves each download):

   | File | URL | Size | Validates |
   |---|---|---|---|
   | `DJI MAVIC3 PRO.rar` | huggingface.co/datasets/kitofrank/RFUAV/resolve/main/DJI%20MAVIC3%20PRO.rar | 5.16 GB | O3+: whether O3 is really 15 kHz (assumed) and its uplink raster (would settle the OcuSync-2-shaped Autel residual) |
   | `DJI MINI3.rar` | .../resolve/main/DJI%20MINI3.rar | 6.48 GB | an earlier OcuSync generation: 15 kHz video and uplink timing |

   To repeat a run: `unrar p -inul <rar> "<folder>/packN_<i>-<i+1>s.iq"` for i = 0, 1, ... into
   `rfuav-check run - --fc-mhz <XML CenterFrequency> --out <dir> --tag <name>`, then
   `rfuav-check summary <dir> <name>`. Keep the archive and outputs out of the repository.

2. **Hardware**: one session with an OcuSync drone, props off, motors armed, near a C5 on the FPV
   job, would check the video detector, the measured centre against the drone's channel, and how
   the uplink actually hops (the HopTracker window and rule). An O3 or O4 Air Unit with goggles on
   a manual channel checks the air-unit raster rule. The 5.1 GHz band needs the four extra sweep
   hops in `djilink::plan::O4_BAND_SWEEP_MHZ` added to the FPV sweep (not done here: the sweep is
   `fpvscan::channels`, being changed in parallel). The real node's analysis gate (8 dB over the
   floor) decides whether weak links are judged at all: the low-SNR Avata 2 packs mostly were not.
