# Jamming detection (raw-capture node)

Status: **built and host-tested, not yet run on a chip** (2026-11-17). Code: `crates/jamwatch`
(logic, 27 tests on synthetic 4-bit scenes), `firmware/node-sdr/src/radio/jam.rs` (wiring),
`crates/link` `ThreatKind::{JamBroadband, JamTone, JamSweep, JamPulsed, Blinded}` (wire format).

The point is to **fail loud**. A detector that is being drowned out must not show "all clear": if the
node's receivers cannot hear a drone link any more, the hub is told so (`Blinded`), whatever the
cause.

## What a jammer looks like to this node

Commercial anti-drone guns and DIY jammers mostly barrage the 2.4 GHz and 5.8 GHz ISM bands (the
control and video links), and usually also 433/915 MHz (long-range control: ELRS, Crossfire),
1.2-1.3 GHz (long-range analog video) and GNSS L1 (1575.42 MHz) to force a drone into failsafe.
**This node cannot hear 433/915 MHz, 1.2 GHz or GNSS L1**: the ESP32-C5's receiver is measured at
LO 2200-2650 and 5180-5816 MHz only. A jammer that leaves 2.4/5.8 GHz alone is invisible here; GNSS
jamming belongs to the GNSS-integrity work, and sub-GHz needs other hardware.

Within reach, jammers come in four shapes, and each has a detector:

| Kind | Typical source | What we measure |
|---|---|---|
| Barrage (`jam_broadband`) | noise source + wideband amplifier per band | floor (10th-percentile probe level) of most hops raised, passband flat, no link structure, many hops / both bands |
| Tone (`jam_tone`) | an unmodulated oscillator, a cheap "spot" jammer | one bin persistent, concentrated, far above the rest |
| Sweep (`jam_sweep`) | VCO driven by a sawtooth (most cheap handheld jammers) | the spectrum peak moving monotonically from frame to frame; rate in MHz/s |
| Pulsed (`jam_pulsed`) | gated or reactive jammers | loud on/off runs of probes whose loud part is wideband noise |
| Blinded (`blinded`) | any of the above, or anything else loud enough | median floor rise of a band's hops ≥ 12 dB |

## How (per dwell, no extra air time)

The node already hops (DroneID job: 13 hops of 700 ms; FPV job: 46 channels of 60 ms). For each
dwell it already has:

1. **Every trigger probe's power** (256 of every 2048 bytes of the 40 MS/s 4-bit stream, one per
   51.2 µs: ~13,700 in a DroneID dwell, ~1,170 in an FPV dwell). `jamwatch::ProbeStats` keeps
   their half-dB histogram (`fpvscan::Meter`, reused) and runs of consecutive probes 12 dB over
   the hop's floor.
2. **The newest ~1.34 ms of the ring** (the ring is stopped for the retune anyway).
   `jamwatch::spectrum` runs 128 Hann-windowed 64-point integer FFTs (625 kHz bins): 64
   back-to-back at the newest end (102 µs, fast sweeps) and 64 spread over the rest (~21 µs
   apart, slow sweeps and pulse structure). About 2 ms of CPU per dwell. The passband used is
   ±1.25-8.75 MHz (the analog low-pass is about ±10 MHz; the bins next to DC hold the receiver's
   DC residual).

From those, per dwell (`Watch::assess`):

- **Floor rise** = 10th-percentile probe level minus the hop's learnt floor. The 10th percentile
  ignores bursty traffic (Wi-Fi, DroneID, controllers): only something that never pauses lifts it.
- **Occupancy**: percent of passband bins no more than 6 dB below the passband's mean bin. Flat
  noise fills it (~100 %); a narrow signal pulls the mean up and leaves most bins far below.
- **Clipping**: share of samples on the 4-bit rails. The clip window has only ~20 dB of headroom
  over the receiver noise, so a strong jammer pins the window; ≥ 1 % clipped on a dwell that was
  loud throughout (≥ 90 % of probes) counts as a rise of at least 20 dB.
- **Tone**: averaged-spectrum peak ≥ 15 dB over the median bin, ≥ 50 % of the passband power within
  ±2 bins, the per-frame peak within ±1 bin in ≥ 70 % of frames, and the floor up ≥ 6 dB.
- **Sweep**: a run of ≥ 5 "carrier" frames (peak ≥ 10 dB over the frame's median bin) moving
  monotonically (≤ 8 bins a step, one bin back allowed), spanning ≥ 6 bins (3.75 MHz), straight
  (RMS residual ≤ 1.5 bins). Rate from the least-squares slope. Measurable range: about
  2.7e3-2.3e5 MHz/s (spread frames) and 3.7e4-3.1e6 MHz/s (contiguous frames).
- **Pulsed**: floor not raised, 2-90 % of probes loud in ≥ 3 runs, and the loud frames' own spectrum
  ≥ 60 % occupied (or the whole window inside one clipping pulse).

**Link or noise?** When a dwell is suspicious (raised, tone, sweep, pulse, or blinding), the node
asks `fpvscan` (reused, not duplicated): cyclic-prefix autocorrelation at the Wi-Fi (3.2/12.8 µs),
half-clock Wi-Fi (6.4 µs) and LTE-style 15 kHz (66.7 µs: DJI, Walksnail, Autel) symbol lengths,
the analog-video line rate, and on 2.4 GHz the 802.11b Barker check. On 4096 half-banded samples
(~205 µs; ~25 ms of CPU, only on suspicious dwells), placed on a loud frame for a pulse; the FPV
job reuses its own analysis. A structured link is not jamming and does not count as blinding. A
link under a strong jammer loses its cyclic-prefix metric (it scales with the link's share of the
power): that is the right answer, it is drowned.

## Baselines that cannot learn a jammer

Each hop learns its own floor (the dwell's 10th percentile): **down at once, up at most 0.5 dB a
minute, and not at all while the dwell is ≥ 6 dB up** or judged raised/pulsed. A jammer that
appears is never learnt in; one that stays for an hour is still reported (test: 39 minutes).

A node that **boots into** a jammed band has nothing quiet to learn from, so a learnt floor is never
trusted above the lowest of:

- the receiver's expected floor + 8 dB (`REF_FLOOR_X10`: 34.5 dB at 2.4 GHz, 31.0 dB at 5.8 GHz in
  probe units, derived from the bring-up noise of 64-84 and 43-51 codes RMS at gain 83 through the
  4-bit clip window: **to be checked against real floors**, `jam` on the console prints them);
- the band's quietest hop + 6 dB;
- the other band's quietest hop, corrected by the expected 3.5 dB difference, + 4 dB.

So a jammer on 2.4 GHz only is seen against the 5.8 GHz band and vice versa; a jammer on both from
boot is seen against the receiver's own noise if it is more than ~16 dB up, or clips the 4-bit
window (tests cover both).

## From dwells to reports

Each hop keeps its latest verdict; verdicts older than two rounds of the plan (+2 s, at least 5 s)
are dropped. Per band:

- **Broadband**: ≥ 3 raised hops (rise ≥ 8 dB, occupancy ≥ 75 %, no link) spanning ≥ 50 MHz with at
  least half the band's recent hops raised; or ≥ 2 raised in each band with half of each raised.
  One drone link raises one or two neighbouring hops (a hop counts only if the signal fills ≥ 75 %
  of its passband, so a 40 MHz link raises hops within ~±12 MHz of its centre) and carries a
  cyclic prefix; a barrage raises everything and is noise.
- **Tone / Sweep / Pulsed**: at least 2 dwells in the window.
- **Blinded**: median floor rise of the band's recent non-link hops ≥ 12 dB (one hop is enough when
  parked). `detail` = percent of free-space detection range left, `100·10^(-rise/20)`: received
  power falls with the square of distance, so each 6 dB of extra noise halves the range at which a
  link still clears the same SNR. 12 dB → 25 %, 20 dB (clipping) → 10 %.

Each kind is sent at most once a second (alternating bands when both are hit) as `Frame::Threat`
and printed as a `jam` line. Notable dwells print `jamhop` (floor, rise, percentiles, duty, runs,
clipping, occupancy, link, tone, sweep, CPU µs). Field meanings are in `crates/link`
(`ThreatKind` docs). There is no "jamming ended" frame: the hub should expire a finding a few
seconds after the last one.

## False-alarm cases and how they are handled

- **Microwave ovens (2.45 GHz)**: a magnetron is on for half of each mains cycle (8.3 ms at 60 Hz,
  10 ms at 50 Hz), so it never lifts the 10th percentile. Its on-time looks like pulses, a slowly
  wandering carrier (tone-ish), or, close by, 10-20 MHz of noise. Gate: on a 2.4 GHz hop between
  2400 and 2500 MHz, 20-80 % loud in runs of 2-14 ms is an oven: no tone, sweep or pulsed verdict
  from that dwell (`oven` in `jamhop`). Inverter ovens run continuously but narrow (a few MHz), so
  they never fill the passband. Tested with both a carrier and a wide-noise oven. A pulsed jammer
  with a mains-like cadence inside 2400-2500 MHz would be missed: accepted.
- **Busy Wi-Fi**: bursty (10th percentile stays low) and OFDM with a cyclic prefix (link). 802.11b
  beacons have no prefix but are caught by the Barker check. A Wi-Fi channel saturated by several
  nearby access points can lift the floor on 2-3 hops: not enough for Broadband, and if its
  prefix shows it is a link. Tested with five loud channels at 30-45 % airtime.
- **A drone transmitting very close to the node**: its video clips the 4-bit window (rise ≥ 20 dB)
  on the hops it covers, but the cyclic prefix survives clipping (identical samples clip
  identically), so those hops are links: no Broadband, no Blinded. Tested at +32 dB. The node is
  genuinely deafened on those channels, which the hub already knows from the drone sighting.
- **HDZero and other flat, prefix-less links**: one transmitter fills one or two hops, never half a
  band spanning 50 MHz. A race with many HDZero quads could in principle add up; with each raising
  ~2 of 31 FPV hops it stays far under half.
- **Analog FM video**: its carrier moves with the picture (a gradient sweeps it steadily along each
  line); the line-rate check marks it a link, so no sweep verdict. Tested.
- **The receiver itself**: spurs and DC residual are excluded by the DC bins and learnt into the
  per-hop floor; a hop noisier than its band's quietest by more than 6 dB reads as raised but
  needs neighbours to make a report.

Known blind spots: a narrowband noise jammer on one channel looks exactly like an unknown wideband
link (FPV may report `fpv_carrier`/`energy`; jamwatch reports nothing unless it blinds the band);
a node parked on one frequency cannot report Broadband (no other hops to compare); sweeps slower
than ~2.7e3 MHz/s look like a tone in each dwell and are reported as `jam_tone`; sweeps faster than
~3e6 MHz/s smear into noise and are reported as `jam_broadband`; weak barrage (< ~8 dB over the
floor) is not reported, and from a jammed boot less than ~16 dB is not.

## Coverage against evasion (FPV job)

After every round of its plan an FPV node visits one channel off the main sweep, rotating
(`coverage::plans::FPV_EDGE`, node-sdr `EDGE_MHZ`, console `edge on|off`): 5181/5201/5221/5241
(the 5.170-5.250 GHz band only DJI O4 video uses; djilink's `DjiO4Video` verdict needs it; LO
inside the measured reach) and 5820-5945 (the upper 5.8 GHz FPV channels; **LO 5819-5944 MHz is
above the 5816 MHz verified on a chip: unverified**). Cost: 60 ms plus up to two DC
re-measurements per ~2.8 s round (~4 %); each edge channel comes round every ~30 s. 2.3 GHz is not
added: satellite radio (2320-2345, with terrestrial OFDM repeaters) and US WCS LTE (2305-2360) are
continuous OFDM that would read as drone links. The DroneID plan is unchanged.

## Needs a bench

1. **Floors.** `jam` on the console at rest: compare each hop's `floor` with `ref` (34.5/31.0). If
   real floors sit more than ~8 dB above `ref`, raise `REF_FLOOR_X10` or a booted node will think
   it is jammed.
2. **A known noisy source.** A broadband noise source (or a 2.4 GHz video sender / Wi-Fi card in
   continuous-transmit test mode, legally, into a dummy load or a shielded box with the node)
   stepped in level: `jamhop` should show `rise` following the level, `raised` once the passband
   fills, `jam_broadband` once it covers enough hops (a single 20 MHz source will NOT make
   broadband, by design: it should show `raised` on 1-2 hops only), and `blinded` above 12 dB.
   Never radiate a jammer: it is illegal almost everywhere and hits everyone's links.
3. **Ovens.** A microwave oven boiling water 1-3 m away for 2 minutes: expect `oven` in `jamhop`
   lines and no `jam` lines.
4. **Wi-Fi.** A laptop streaming next to the node: no `jam` lines.
5. **A real drone** at 1-3 m (clipping): its hops show `link:true`, no `jam` lines.
6. **Edge channels.** `role fpv`, `fpv verbose on`: `fpv` lines with `"edge":true` at LO 5819-5944
   should show floors like the other 5.8 GHz channels; garbage there means the PLL does not reach.
7. **CPU.** `jamhop` `us` (spectrogram + check): expected ~2 ms quiet, ~30 ms with a link check.

## Sources

- This repo: `firmware/node-sdr/BRINGUP-RESULTS.md` (noise levels, Wi-Fi levels on air, FPV
  analysis time), `docs/SIGNALS.md` (band plans, reach), `docs/DJI-LINKS.md` (O4 5.1 GHz band,
  LO 5806/5816 verified on c5b), `crates/fpvscan` (cyclic-prefix, line-rate, DSSS checks).
- General radio facts relied on: free-space path loss (inverse square, 6 dB per doubling of
  distance); magnetron ovens are half-wave driven at mains frequency in the 2.45 GHz ISM band;
  jammer sales and use are illegal in the US (FCC) and the EU, so this project only detects them.
