# DJI radio links: what the public record says, and what Rotorscope fingerprints

PLAN.md Phase 11b. Written 2026-11-15 on branch `dji-fingerprint`. **Receive-only, presence
only**: nothing here demodulates video or control data, handles a key, or transmits. O4 stays
detect-only permanently (PLAN.md Phase 10 decision).

Code: `crates/djilink` (host-tested, `no_std`, fixed-point), wired into `firmware/node-sdr`
(`src/radio/dji.rs`), `crates/link` (protocol 7 `RfKind`s), `crates/tracker`, `crates/screen`.

**Status: verified only on synthetic signals** built from the numbers below and passed through a
model of the C5's 4-bit capture path. Nothing has been run against a real DJI transmitter or a real
recording. Every threshold is a first guess until recordings (end of this document) or a bench
session confirm it.

## How to read the evidence labels

- **measured**: someone captured the real signal and reported the number (paper, reverse
  engineering notes).
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
| Duty cycle in flight, TDD frame | not published | unknown | |
| O3/O4 aircraft numerology | assumed the same 15 kHz / normal CP family | **inferred** (O4's DroneID burst still uses it, P; O3 Mini 3 Pro frame matches, A2) | |
| O4 wide modes | O4 Air Unit (Pro): max 60 MHz; manual 60/40 MHz on 5794.5, 20/10 MHz on 5768.5, 5789.5, 5814.5 | filed | D1 |
| O4 Air Unit Pro filing | sections for 20/40/60/80 MHz at 5.8 GHz; 40 MHz 5745.5-5829.5, 60 MHz 5755.5-5819.5 | snippet | F3 |
| O4 subcarrier spacing in 40/60 MHz | not published (15 kHz with 2401+ subcarriers, or 30 kHz, are both possible) | **unknown** | |
| O3 Air Unit | max 40 MHz; manual 40 MHz on 5794.5, 20/10 MHz on 5768.5, 5804.5, 5839.5 | filed | D2 |
| 5.170-5.250 GHz | listed by O4 products (O4 Air Unit, Air 3, Mini 4 Pro); not by O3/O3+ (Mini 3 Pro, Mavic 3 Pro, O3 Air Unit) | filed | D1, D4 |
| Autel overlap | EVO Lite/Max also file 5.15-5.25 GHz with 1.4/10/20 MHz LTE-like modes of the same widths (RESEARCH.md) | filed | RESEARCH.md |

## 2. The controller uplink (controller to drone)

| Parameter | Value | Label | Source |
|---|---|---|---|
| Waveform | OFDM, 15 kHz, ZC sequences for timing; QPSK | measured | I, T |
| OcuSync 1/2 | 128-point FFT at 1.92 MS/s, ~73 subcarriers (~1.1 MHz), 6 OFDM symbols a frame (~0.43 ms), 576 bits | measured | I |
| O3 | 256-point FFT at 3.84 MS/s, 145 subcarriers (~2.2 MHz), 7 symbols (~0.5 ms), 1440 bits | measured | I |
| O4 | not published; assumed like O3 | **inferred** | |
| "1.4 MHz-CA" mode (the hopping uplink, per T) | 59 channels, 2 MHz raster at x.12 MHz: 5730.12-5846.12 MHz; 6 dB 1.136-1.152 MHz, 99 % 1.164-1.200 MHz | filed (OS2) | F1, T |
| 2.4 GHz uplink channels | Mini 2 1.4M-CA edges 2409.12 / 2467.12 MHz; Mavic 3E 1.4M-CA to 2471.12, 3M-CA from 2408.2 | snippet | F2 |
| Hop dwell, period, sequence | not published. Generic DroneRFa-era range for drone FHSS uplinks: 0.4-5 MHz wide, 0.5-5 ms bursts | unknown (generic: measured) | A1 |

## 3. DJI FPV air units

| System | Channels | Width | Label | Source |
|---|---|---|---|---|
| DJI FPV V1/V2 goggles, Air Unit, Caddx Vista, DJI FPV drone | 5660, 5695, 5735, 5770, 5805, 5878, 5914, 5839 (public ch 8); CE/SRRC 5735/5770/5805/5839; MIC 5660/5700/5745 | 20 MHz per channel at 25 Mbps; 50 Mbps takes about two channels | filed (FCC list via RESEARCH.md); widths secondary | D3 |
| O3 Air Unit | 5768.5, 5804.5, 5839.5 (10/20); 5794.5 (40) | 10/20/40 | filed | D2 |
| O4 Air Unit / Pro | 5768.5, 5789.5, 5814.5 (10/20); 5794.5 (40/60); also 5.170-5.250 GHz | 10/20/40/60 | filed | D1 |
| V1 PHY | not published; assumed LTE-like 15 kHz like OcuSync | **inferred** | |
| Walksnail Avatar | reportedly the same 8-channel plan, Artosyn LTE-style chip | secondary / inferred | RESEARCH.md |

## What each detector looks for

All need **DJI's numerology first**: 15 kHz OFDM with a normal (4.7 µs) cyclic prefix, found by
folding `x[n] conj(x[n+1333])` at the 71.4 µs symbol period so the prefixes of every symbol add
coherently. Wi-Fi (3.2 µs symbols), analog FM video, carriers and hobby FSK links do not have it.

| Rotorscope kind (screen / phone label) | What must hold | Confidence in the public basis |
|---|---|---|
| `DjiVideo` ("DJI video") | DJI numerology; >= 7 MHz wide at -10 dB and >= 5 MHz at -3 dB; not a lone DroneID-shaped burst | Good for OcuSync 2/3 (measured). **Autel's LTE-based links have the same shape**: "DJI" is the likely maker, not proven |
| `DjiO4Video` ("DJI O4 video") | the same, centred (or the LO, if wider than the capture) in 5.170-5.250 GHz | Good that only O4 DJI products use the band (filed); Autel EVO Lite/Max also file it |
| `DjiFpvAirUnit` ("DJI FPV air unit") | the same, whole signal inside the capture, centre within 200 kHz of a whole-MHz DJI FPV channel and off the x.5 raster | Channel plan filed; V1's numerology inferred; Walksnail may share the plan |
| `DjiController` ("DJI controller") | DJI numerology; a 0.2-1.3 ms burst 0.6-3 MHz wide (the burst's own width counts); confirmed only when bursts show on two centres >= 1 MHz apart within 3 s | Width and symbol count measured (I) and filed (F1); hop timing unknown, so the confirmation rule is deliberately loose |
| DroneID burst (reported as `LteStyle`) | 8-10.5 MHz, one whole 0.45-0.8 ms burst, dwell otherwise nearly silent | Measured (A1, P); the DroneID job decodes these properly |
| `LteStyle` (existing) | 15 kHz OFDM that fits none of the above, or an extended prefix | |

What is **not** separable from one 20 MHz capture, and why:

- **O3 vs O4 on 5.8 GHz**: same channel plans overlap (5768.5 and 5794.5 are on both; every manual
  channel is on the aircraft raster too), same assumed numerology. Only 5.1 GHz, or a width over
  40 MHz (60 MHz is O4-only), separates them; our capture is 20 MHz wide, so 40 vs 60 needs two
  adjacent captures (not built). The DroneID job's O4 record (encrypted, key ID only) is the
  reliable O4 marker; fusing the two in the tracker is a later step.
- **Drone vs O3/O4 air unit**: both use the x.5 raster; not separable.
- **DJI vs Autel**: same widths, same LTE-like numerology; separating them needs recordings of
  both (frame timing is the likely difference: Autel's EVO II test mode was on 4.05 of every
  5.01 ms; DJI's in-flight timing is unpublished).
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
capture saw a burst begin, the prefix search is limited to the timing that start predicts, which
is what makes a 0.45 ms, 1.1 MHz uplink burst detectable at all.

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
samples are small), skipping the extended/30 kHz folds when the normal fold is far from its
threshold.

**Wire format**: protocol 7 appends four `RfKind`s (older kinds keep their postcard indices;
`crates/link` test `protocol_7_rf_kinds_append_only` pins them). A protocol-6 hub counts an `Rf`
frame with a new kind as corrupt and carries on. The tracker treats every `DjiController` sighting
within 150 MHz as one emitter (it hops); the node rate-limits it as one emitter too.

## Test results (synthetic only)

`cargo test -p djilink`: 19 tests. Signals are built per symbol by inverse FFT at each system's
own rate (30.72 MS/s / 2048 for 20 MHz video, 15.36 / 1024 for 10 MHz video and DroneID,
1.92 / 128 and 3.84 / 256 for the uplinks) with the documented prefixes, interpolated to 40 MS/s,
plus the C5's receiver noise, quantised to 4 bits by the real `rawcap` window and half-banded to
20 MS/s by the real `rawcap` filter.

- 20 MHz and 10 MHz video, continuous and with gaps, several offsets: always `DjiVideo`; centre
  within 60 kHz on 10 MHz signals.
- On a V1 channel (5770.0) `DjiFpvAirUnit`; 1.5 MHz lower (5768.5, x.5 raster) `DjiVideo`; a
  clipped 20 MHz signal never gets an air-unit claim.
- 5.2 GHz band (10, 20 and a 15 kHz model of 40 MHz): `DjiO4Video`; the 40 MHz model on 5.8 GHz:
  `DjiVideo`.
- DroneID bursts: DroneID, not video (unless the dwell is busy).
- OcuSync 2 and O3 uplink bursts at four offsets, two levels, six timings: all controller bursts,
  burst centre within 150 kHz; two hops in one capture: two centres, confirmed.
- Carrier offsets -250..+240 kHz: no effect on detection; centre follows the offset.
- **Negatives** (never a DJI kind): 802.11a and 802.11ax Wi-Fi at three traffic levels (largest
  15 kHz metric 79 under its threshold), FM analog video (PAL/NTSC, three pictures), a carrier,
  an HDZero-like flat signal without prefix, silence over 40 captures (108 under), GFSK hopping
  bursts like a hobby RC link (127 under).
- LTE extended prefix: family `Ofdm15`, prefix measured >= 250 samples; 30 kHz OFDM: `Ofdm30`;
  DJI's prefix measured 64-128 samples (true 94).
- Sensitivity (total signal power over the noise in the 20 MHz capture): 20 MHz video detected
  4/4 down to -0.6 dB and 3/4 at -2.7 dB; O3 uplink down to +1.9 dB; OcuSync 2 uplink down to
  +4.4 dB (that is ~17 dB in its own 1.1 MHz). Weak signals give no verdict, never a wrong one.

Whole workspace: `cargo test --workspace` 617 passed (baseline 595). `cargo fw` and `cargo fw-lite`
(node-sdr), `cargo fw` (hub-p4, hub c5, hub s3) build. Not flashed.

## What would validate it

1. **Recordings** (owner approval needed before fetching; nothing downloaded):

   | File | URL | Size | Licence | Validates |
   |---|---|---|---|---|
   | `DJI MINI4 PRO.rar` | huggingface.co/datasets/kitofrank/RFUAV/resolve/main/DJI%20MINI4%20PRO.rar | 4.95 GB | Apache-2.0 per the dataset card's metadata (RESEARCH.md had it as unstated; check the card text before redistributing anything) | O4 drone video + its controller uplink: the O4 numerology question, hop timing |
   | `DJI AVATA2.rar` | .../resolve/main/DJI%20AVATA2.rar | 11.41 GB | same | O4 FPV link (goggles) |
   | `DJI FPV COMBO.rar` | .../resolve/main/DJI%20FPV%20COMBO.rar | 15.00 GB | same | DJI FPV (V1-generation) system: the air-unit raster and numerology |
   | `DJI MAVIC3 PRO.rar` | .../resolve/main/DJI%20MAVIC3%20PRO.rar | 5.16 GB | same | O3+ for comparison |
   | `DJI MINI3.rar` | .../resolve/main/DJI%20MINI3.rar | 6.48 GB | same | an earlier OcuSync generation for comparison |
   | `DAUTEL EVO NANO.rar` | .../resolve/main/DAUTEL%20EVO%20NANO.rar | 3.37 GB | same | Autel: the DJI-vs-Autel question |

   RFUAV is 100 MS/s float32 (RESEARCH.md). Start with `DJI MINI4 PRO.rar` (smallest O4). Using
   them needs a small host tool (not written yet): resample to 20 MS/s, model the C5's 4-bit
   capture as these tests do, cut 0.82 ms windows and print `djilink::features`. The
   ValidationSet_5Drones files (13.6-43.2 GB each) are not needed.

2. **Hardware**: one session with an OcuSync drone, props off, motors armed, near a C5 on the FPV
   job, would check the video detector, the measured centre against the drone's channel, and how
   the uplink actually hops (the HopTracker window and rule). An O3 or O4 Air Unit with goggles on
   a manual channel checks the air-unit raster rule. The 5.1 GHz band needs the four extra sweep
   hops in `djilink::plan::O4_BAND_SWEEP_MHZ` added to the FPV sweep (not done here: the sweep is
   `fpvscan::channels`, being changed in parallel).
