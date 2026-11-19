# FPV video links

**Level: 🟢 Beginner, then 🟡 Intermediate** · What Rotorscope can notice about FPV drones, which
often send no Remote ID: analog video, open digital video systems, closed digital systems and
Wi-Fi camera drones. What each tells you, how far it is proven, and its limits.

## 🟢 In plain words

An **FPV** (first-person view) drone streams live video from its camera to the pilot's goggles.
Racing and freestyle FPV drones are often home-built and may send no Remote ID at all, so the
[Remote ID](remote-id.md) listeners never see them. But the video transmitter has to shout to be
seen in the goggles, and that signal is what Rotorscope listens for.

What Rotorscope can tell you from it:

- **That a video transmitter is on the air nearby**, which kind it is, which channel, how loud and
  how much of the time it is on.

What it cannot tell you:

- **Where the drone is.** A video signal carries no position. Rotorscope cannot put an FPV drone on
  the radar; FPV findings go in their own tables (the screen's SIGNALS page, the phone page), not
  the drone list.
- **Whose it is.** Analog video carries no identity. Some open digital systems carry a link ID;
  Rotorscope hides those unless the kit's builder opts in (below).
- **The picture.** Rotorscope never shows, records or decodes anyone's video. It only measures the
  signal's shape.

There are four families, each heard a different way:

| Family | Examples | How Rotorscope hears it | Proof, as of 2026-11-18 |
|---|---|---|---|
| Analog video | Most racing and freestyle quads; cheap 2.4 GHz cameras | A raw-capture C5 sweeps the video channels and confirms the TV line rate | **Simulated only** |
| Open digital video | wfb-ng / OpenIPC, OpenHD, RubyFPV, DroneBridge | The ordinary Wi-Fi listener recognises their frame headers | **Proven on the air** (fake frames from the test beacon) |
| Closed digital video | DJI FPV / O3 / O4 air units, HDZero, Walksnail | The raw-capture C5 fingerprints the signal's shape | DJI: **proven on recordings**; HDZero: **simulated only**; Walksnail: **planned** |
| Wi-Fi camera drones | DJI Tello and Spark, Parrot, toy "WIFI-UFO" quads | The Wi-Fi listener recognises the drone's own network | **Simulated only** |

Nothing in this family has yet come from a real FPV drone. Rotorscope only listens; testing with a
real video transmitter needs the right licence or a field visit.

## 🟡 Analog video

**How a scan recognises it** (`crates/fpvscan`, run by the `firmware/node-sdr` FPV job on an
ESP32-C5 used as a raw radio sampler, see [DJI DroneID](dji-droneid.md)):

1. The node steps through the analog channels, 60 ms on each. Every quick level probe goes into a
   meter, which gives the channel's loudness against the noise floor and its duty cycle (how much
   of the dwell it was on). The floor is the quietest channel's tenth percentile, because a video
   transmitter never pauses on its own channel.
2. A channel 8 dB or more above the floor gets one frozen capture analysed. The capture is
   FM-demodulated and checked for the **15.6 or 15.7 kHz line rate** of PAL or NTSC video. Wi-Fi
   has no such rhythm; analog video always does.
3. The same capture is checked for the signatures of other things: Wi-Fi's 3.2 µs (or 12.8 µs)
   cyclic prefix, half-clock Wi-Fi's 6.4 µs, the 66.7 µs symbol of LTE-style links (DJI, Autel,
   Walksnail), and a wide, flat, continuous signal on HDZero's channel grid.

The verdict is one of: analog video, FPV carrier (continuous and loud, line rate not seen),
HDZero, LTE-style, narrow Wi-Fi, Wi-Fi, energy or quiet. **Wi-Fi and plain bursty energy are
never sent to the hub**: on 5.8 GHz bursty energy is nearly always Wi-Fi whose frame missed the
capture.

**Channel tables** ([Signal catalogue](../SIGNALS.md), sections 3 and 3b):

| Band | Channels | What a C5 reaches |
|---|---|---|
| 5.8 GHz: bands A, B, E, F, R (Raceband) and L, eight channels each | 47 distinct, 5362-5945 MHz | 31 channels, carriers 5181-5810 MHz. The other 16 (5820 MHz and up) need an RX5808 receiver module, which is **planned** |
| 2.4 GHz: bands 2.4A (20 MHz grid) and 2.4B (18 MHz grid) | 15 distinct, 2370-2510 MHz | All 15 |

One FPV node sweeps all 46 reachable channels in about 2.8 s; two FPV nodes split them.

**Guards against look-alikes** (`crates/fpvscan/src/band.rs`, `dsss.rs`):

- **Wi-Fi OFDM** is recognised positively by its cyclic prefix, and can only become "video" by
  showing a line rate, which OFDM does not have.
- **Older 802.11b Wi-Fi** (DSSS) has no cyclic prefix, so a separate test looks for its
  1-microsecond Barker spreading.
- **Microwave ovens** can ripple at a rate that looks like a video line. A rhythm that also repeats
  at a half, third or quarter of a line, or a smooth ripple, is not video.
- **Inside 2400-2500 MHz**, only confirmed video (or a recognised digital family) is reported:
  loud continuous energy there is far more often an oven or streaming Wi-Fi than a drone. Each
  band keeps its own noise floor, and Bluetooth hops are never reported.

**Proof.** Detecting analog video is **simulated only**: PAL and NTSC test signals, with audio,
under Wi-Fi and Bluetooth, in host tests. The rejection side has met real signals: in 200 s on the
air next to real 5 GHz Wi-Fi, the scan analysed 130 loud dwells and called 124 Wi-Fi and 6 energy,
with **0 false video** (the largest line-rate score on any Wi-Fi capture was 41; video needs 150).
The 2.4 GHz sweep has not yet been checked on the air at all.

## 🟡 Open digital systems

wfb-ng (used by OpenIPC), OpenHD, RubyFPV and DroneBridge put an ordinary Wi-Fi card into a special
mode and send their video as home-made Wi-Fi frames. `crates/openlink` recognises them from one
frame by their fixed header patterns: for example wfb-ng's transmitter address always starts
`57:42`, and RubyFPV's is always `13:12:34:56:78:90`. The rules are built so that ordinary Wi-Fi
*cannot* match (the address shapes they rely on do not occur in normal traffic). Payloads are
encrypted or not looked at: video and telemetry content is never decoded.

The hub keeps these in their own table, apart from drones, with no position, no radar dot and no
buzzer.

**Identifiers are hidden unless opted in.** The wfb-ng link ID, RubyFPV vehicle ID, DroneBridge
comm ID and those links' transmitter addresses appear only on a hub built with
`--features show-link-ids` (in the laptop viewer, `rid-view --show-link-ids`). Seeing *that* a
link is nearby is for anyone holding the unit; *whose* it is takes the builder's opt-in. The
[evidence log](../EVIDENCE.md) always keeps the raw frames, identifiers included.

**Proof: proven on the air**, from fake frames sent by the project's own test beacon: all four
systems on channels 6, 149, 161 and 165, 16 of 16 bench tests, and again through the P4 hub's
link table (2026-11-14 and 2026-11-15, [bench tests](../BENCH.md)). Not yet heard from real FPV
hardware. Still unknown: whether the ESP32 receives wfb-ng's default video coding (LDPC and STBC),
which the test beacon cannot send; links that send their header in RTS frames, which the listener's
filter does not pass; and OpenHD's 40 MHz channels.

## 🟡 Closed digital systems

These have no open format to recognise, so the raw-capture C5 judges the signal's shape instead
(PLAN.md Phase 11).

- **DJI FPV, O3 and O4 air units, and DJI drones' own video and controller links.**
  `crates/djilink` reports "DJI video", "DJI O4 video", "DJI FPV air unit" and "DJI controller"
  from carrier spacing, width, timing and channel raster. Checked against public recordings (DJI
  FPV Combo, Avata 2, Mini 4 Pro) through a model of the C5's receiver: **proven on recordings**,
  not yet run on the chip against a live signal. A DJI drone and a DJI air unit on the same raster
  cannot be told apart. Detail: [DJI DroneID](dji-droneid.md) and
  [DJI radio links in depth](../DJI-LINKS.md).
- **HDZero.** A continuous, wide, flat signal on HDZero's channel grid is reported as HDZero.
  **Simulated only**: no HDZero transmitter has been near a node, and channels above 5810 MHz (R6
  to R8) need the planned RX5808.
- **Walksnail Avatar.** Shows only as an "LTE-style" link: it reportedly shares DJI's channel plan
  and a similar waveform. Telling it apart is **planned** and needs recordings, of which no public
  dataset has any.

## 🟡 Wi-Fi camera drones

Many cheap drones fly over plain Wi-Fi: the drone runs its own network and the pilot's phone joins
it. `crates/wifidrone` names the maker of such a network from two clues, the address prefix
registered to the maker and the default network name, across 13 makers (prefixes checked against
the IEEE registry on 2026-11-14). Because DJI's and Parrot's prefixes also appear on cameras, car
kits and headphones, most clues only count together: a prefix needs one of that maker's names, and
a name needs one of its prefixes. A randomised address never matches.

**Simulated only**: no real Wi-Fi drone has been near a node, and Rotorscope never imitates a
maker's network to test it. Attacks on these drones (someone kicking the pilot off and taking over)
are covered in [Attacks and interference](threats.md).

## Limits to keep in mind

- No position and no identity for any FPV signal, by design or by nature.
- A C5 hears one channel at a time, so a sweep takes about 2.8 s; a short transmission can be
  missed.
- Out of reach of any ESP32: analog video at 1.2-1.3 GHz, and anything at 900 MHz.
- A transmitter whose signal is too weak to stand 8 dB over the floor is not analysed at all.

## Where this lives in the code

- [`crates/fpvscan`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/fpvscan):
  analog scanner (`channels.rs`, `channels24.rs`, `analyse.rs`, `classify.rs`, `band.rs`,
  `dsss.rs`)
- [`crates/openlink`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/openlink):
  open digital systems
- [`crates/djilink`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/djilink): DJI
  link fingerprints
- [`crates/wifidrone`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/wifidrone):
  Wi-Fi drone makers
- [`firmware/node-sdr`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/node-sdr)
  (FPV job) and
  [`firmware/node-wifi`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/node-wifi)
- Deep dives: [Signal catalogue](../SIGNALS.md), [DJI radio links in depth](../DJI-LINKS.md),
  [Control links](../CONTROL-LINKS.md), [Attacks on drones](../DRONE-ATTACKS.md)

**Next:** [Control links](../CONTROL-LINKS.md)
