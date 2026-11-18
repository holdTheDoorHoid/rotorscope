# GNSS integrity: is the hub's own GPS being jammed or spoofed?

Status 2026-11-18: built and host-tested (`crates/gnss/src/integrity.rs`,
`timeguard.rs`, `trust.rs`, `tests_integrity.rs`), built into the P4 hub.
WP-12 (2026-11-18) added the time guard, the trust gate, the remembered home
and time and the time floor. **Not yet run against a real receiver, a real
jammer or a real spoofer**; the hub's GPS path has only run against the
on-chip emulation (docs/BENCH.md). Every threshold below is a first estimate
to be checked on the bench and outdoors.

## Why

The hub uses its GPS for two things: **where it is** (distances to drones,
the map, FAA rule checks) and **what time it is** (evidence timestamps,
PPS). Anti-drone systems and some drone operators jam or spoof GPS. A
spoofed hub computes every distance from the wrong place and stamps every
record with the wrong time, and the evidence log loses its value. Jamming
nearby is itself worth recording: it usually means someone is running
drone countermeasures, or a drone carries a jammer.

## What the hub does

Every epoch (once a second) `gnss::Monitor` looks at what the receiver
reported and gives one of five verdicts:

| State | Meaning | What the hub does |
|---|---|---|
| `ok` | nothing wrong seen | normal |
| `suspect_jamming` | some sign of jamming | logs it; GPS still used |
| `jamming` | the receiver says so, or several signs agree | logs it; GPS still used (a jammed receiver simply has no fix) |
| `suspect_spoofing` | some sign of spoofing | logs it; **holds position and clock** |
| `spoofing` | the receiver says so, a strong sign, or several signs agree | logs it; **holds position and clock** |

**Trust and holding** (`crates/gnss/src/trust.rs`, applied by
`firmware/hub-p4/src/gps.rs`):

- An epoch is **trusted** when no spoofing check has fired, not even an
  unconfirmed one, for `TRUST_EPOCHS` = 10 epochs in a row, and the verdict is
  not a spoofing one. Only a trusted epoch's position and time are published
  (screen, tracker, aircraft decoding, FLARM, aero-node `TimeSync`, evidence,
  web head, file names) and only a trusted epoch anchors or steers the hub's
  clock. Attacker-facing: a spoofer's first epochs, before any check has
  sustained, are never published (before WP-12 they were, and were then
  held).
- Any other epoch publishes the **last trusted** position and time, held.
  Position: while spoofing is suspected, *home* (below) if there is one, else
  the last trusted fix; marked `held`, counted as fresh (the hub does not
  move), keeping the trusted fix's time in `at_ms` so the evidence log's *fix
  age* grows. Clock: the last trusted anchor, run on by the hub's own crystal
  (`time_held`; after 2.5 s the evidence says `free_running`).
- **Nothing trusted this boot** (a spoofer on since power-up): nothing is
  published. No position (unless a remembered or placed home exists, which is
  then held), no UTC: the screen's health page says `no trusted
  position/time`, the `{"t":"hub"}` line has `"untrusted_pos"` /
  `"untrusted_time"`, and evidence records carry time source `untrusted`
  with UTC 0. Every consumer already copes with no GPS: the tracker has no
  own position (no ranges), aircraft positions are decoded without a
  reference (global decoding only), FLARM is counted not decoded, no
  `TimeSync` is sent (aero nodes park after 10 s), the CSV log is numbered
  rather than named by time, the web head gets no hub position or time.
  With **no receiver at all** the flags stay false (`gps.state` `absent`,
  `untrusted_pos` / `untrusted_time` false, evidence time source `none`):
  "untrusted" means a receiver gave a position or time the hub refused, and
  with no receiver there is nothing to refuse.
- GPS time found before the build date or the remembered time also drops
  the held time (it was not trustworthy after all).
- The receiver status (fix type, satellites) still shows what the receiver
  claims.

**Evidence events** (`crates/evidence/src/format.rs`, codes 18 and 19), on
**changes** of the verdict only, never once a second:

| Change | Event | Data |
|---|---|---|
| into or within a jamming state | `GnssJamming` (18) | kind (u8), level (i16 LE) |
| into or within a spoofing state | `GnssSpoofing` (19) | kind (u8), level (i32 LE) |
| back to `ok` | the event of the category left | kind 0, level 0 |

The text is the plain-English reason, for example `possible GPS spoofing:
position 412 m from where the hub has stood (+1 more check); holding last
trusted position and time`. Kind and level:

| Kind | Jamming check | Level unit | | Kind | Spoofing check | Level unit |
|---|---|---|---|---|---|---|
| 1 | receiver jamming flag | state 2/3 | | 1 | u-blox spoofing detector | state 2/3 |
| 2 | u-blox CW jamming indicator | 0-255 | | 2 | uniform C/N0 | 0.1 dB (std dev) |
| 3 | AGC drop | % | | 3 | C/N0 too strong | dB-Hz |
| 4 | C/N0 collapse | 0.1 dB | | 4 | off learned home | m |
| | | | | 5 | position jump | m |
| | | | | 6 | altitude off home | m (signed) |
| | | | | 7 | UTC off the hub's clock | ms (signed) |
| | | | | 8 | receiver clock bias step | ns |
| | | | | 9 | receiver clock drift step | ns/s |
| | | | | 10 | impossible satellites | count |
| | | | | 11 | time before build date / remembered time | s (negative) |

For the screen and web page: `Gps::integrity()` returns the verdict, the
check behind it (`cause`), and its figures (`metrics`: C/N0 mean, spread,
baseline, AGC, CW, receiver flags, distance from home, time step).
Operator actions: **Hub moved** (screen Settings row, console `m`) forgets
home; **New home / Accept here** (the same row while a new place waits,
console `H`) adopts it; console `T` accepts GPS time as it now is. Each is an
evidence `Setting` event.

## The checks

Each check produces nothing, *suspect* or *strong* per epoch, and must hold
for a number of epochs in a row (*sustain*) before it counts. One-shot
checks (jumps, steps) stay counted for `LATCH_EPOCHS` = 60 epochs after they
fire. *Corroborating* checks never raise an alarm alone; they only add
weight. Per category, two counted checks at once raise the level one step
(suspect becomes strong; two corroborating ones become suspect). Spoofing
outranks jamming. Going up is immediate; coming down takes `CLEAR_EPOCHS`
= 30 epochs in a row at the lower level.

### Jamming

| Check | Receivers | Rule | Sustain |
|---|---|---|---|
| Receiver jamming flag | u-blox (MON-RF `jammingState`), LC29H (`$PAIRSPF`, `$PAIRSPF5`) | 2 warning = suspect, 3 critical = strong | 3 |
| CW jamming indicator | u-blox (MON-RF `cwSuppression`) | >= 100 of 255; corroborating only | 5 |
| AGC drop | u-blox (MON-RF `agcCnt`) | >= 30 % below its learned baseline = suspect | 5 |
| C/N0 collapse | all (NAV-SAT or GSV) | mean of the 4 strongest L1 satellites >= 10 dB below its learned baseline = suspect, **never strong alone** | 30 |

A C/N0 collapse alone is never more than *suspect*: a hand over the
antenna, a closed door or a car parked over the window does the same. Only
the receiver's own flags and the AGC see the raised noise floor that tells
jamming from blockage. A collapse that lasts `CN0_REBASE_EPOCHS` = 600
epochs (10 min) with no receiver flag or AGC drop is accepted as the new
normal (the hub was moved somewhere with a weaker sky) and the baseline is
learned again.

### Spoofing

| Check | Receivers | Rule | Sustain |
|---|---|---|---|
| Receiver spoofing detector | u-blox (NAV-STATUS `spoofDetState`) | 2 = suspect, 3 = strong | 2 |
| Uniform C/N0 | all | >= 6 L1 satellites, std dev < 1.5 dB, max-min <= 4 dB, mean >= 38 dB-Hz = suspect; mean >= 45 = strong | 5 |
| C/N0 too strong | all | >= 3 L1 satellites at >= 55 dB-Hz = suspect | 5 |
| Off home | all, once home is learned | > max(75 m, 4 x accuracy) = suspect; > max(300 m, 6 x accuracy) = strong | 10 |
| Altitude off home | all, once home is learned | > max(60 m, 4 x vertical accuracy) = suspect; > max(250 m, 6 x) = strong | 10 |
| Position jump | all | > 300 m and > 150 m/s between two usable fixes = suspect | one-shot |
| UTC vs hub clock | all | off the hub's own clock by > 150 ms (message) or > 5 ms (PPS edge) plus 20 ppm of the time since it was last steered (below) = suspect | 2 |
| Time floor | all | GPS UTC before the build date, or more than 2 s before the latest time the hub remembers trusting = suspect | 1 |
| Clock bias step | u-blox (NAV-CLOCK) | bias off (previous bias + drift x dt) by > 1 us after removing whole-ms jumps; corroborating only | one-shot |
| Clock drift step | u-blox (NAV-CLOCK) | drift changes > 100 ns/s between two 1 s epochs with a fix; corroborating only | one-shot |
| Impossible satellites | all | a satellite number its constellation cannot have, elevation outside +-90, azimuth outside 0-359; corroborating only | 3 |

Accuracy is the receiver's estimate (u-blox `hAcc`/`vAcc`); NMEA has none,
so HDOP x 6 m is used (vertical: 1.5 x horizontal).

**Home.** The hub is normally static. Three sources, strongest first:

1. **Placed**: a `home LAT LON [HEIGHT]` line in the site file
   (docs/SITE.md). Replaces any other home at every card mount; cannot be
   changed from the screen or console.
2. **Remembered**: learned on an earlier boot, kept in `ROTORSCP/GPSMEM.TXT`
   on the card and used from the moment the card is mounted, so a spoofer
   present from a later power-up is caught by the position checks at once.
3. **Learned**: after `HOME_LEARN_EPOCHS` = 900 usable fixes (15 min at 1
   Hz) that all stay within max(40 m, 3 x accuracy) of their running mean,
   none with an accuracy worse than 50 m, while the verdict is `ok` and no
   spoofing check fired. Written to the card at once (evidence `Setting`).

With a home, the hub keeps learning where it stands whenever the only
complaint is the position itself. A learned place more than
max(75 m, 4 x accuracy) from home (the off-home suspect level) is **not
adopted**: the off-home check keeps alarming, the hub logs `GPS: the hub
seems to stand at ... NOT adopted` (evidence `Setting`), and the screen's
Settings row changes to **New home / Accept here** (console `H`). Only the
operator's confirmation makes it home (and remembers it). Attacker-facing:
a spoofer cannot move home by being patient; it needs the operator to press
the button. **Moving a hub while it is switched off**: at the next boot it
alarms GPS FAKE and holds the remembered home; press Hub moved (relearn,
15 min) or wait 15 minutes and accept the new home.

**Time** (`crates/gnss/src/timeguard.rs`). The hub has its own free-running
clock (its uptime, from a 40 MHz crystal, +-10 ppm at 25 C and about as much
again over temperature: budget `DRIFT_PPM` = **20 ppm**). GPS time may steer
it only within that budget. Two observables, each compared with its own
reference offset (GPS UTC minus hub time):

- the **message**: UTC against the hub time the epoch was read. Arrival
  jitters with message length and the main loop (normally well under 50 ms);
  tolerance **150 ms**, a third of a 400 ms aero slot, under the 200 ms gap;
- the **PPS edge** (when wired): the whole second against the edge's hub
  time, taken in the interrupt; tolerance **5 ms**.

Before the hub trusts GPS time the reference follows the receiver (a step is
reported, then followed once two samples agree; a single late sample is not
followed). Once trusted, the reference is a **disciplined clock**: anchored
on the first trusted epoch, then steered towards each trusted epoch by an
eighth of the difference, never more than 20 ppm of the time since it was
last steered (after a gap, all of what the crystal could have drifted). It
is never re-anchored by GPS; only console `T` (accept GPS time) resets it.
The check fires while a sample is beyond tolerance + 20 ppm x (time since the
last steer), and counts after **2 epochs in a row** (a main-loop stall of up
to 381 ms at an SD mount is one late epoch and does not count). NMEA time
counts when RMC says valid; u-blox time only when NAV-PVT says fully
resolved.

Attacker-facing bounds:

| Spoofer does | Caught? |
|---|---|
| steps the time 0.3 s, 0.5 s, 1 s or more, either way | in 2 epochs, and for as long as the step lasts; never adopted |
| steps it 5-150 ms with PPS wired | in 2 epochs (PPS) |
| steps it under 150 ms without PPS | no (inside the arrival jitter) |
| walks it at `w` ppm, faster than 20 ppm + the crystal's own error `c` | after tolerance / (w - 20 - c): 2 ms/s is caught in about 76 s (message), 3 s (PPS) |
| walks it at most 20 ppm + `c` | **no**: at most **1.7 s a day** with a perfect crystal, **3.5 s a day** with one at its 20 ppm tolerance |

The walk limit is the price of not knowing the crystal better than its
datasheet. (Learning the crystal's actual rate while trusted and allowing a
few ppm around it would cut it to a fraction of a second a day; not done,
because without hardware the temperature behaviour of this crystal is
unknown and a false time alarm stops the aero nodes.)

**Time floor.** GPS time before this firmware's build date (`build.rs`) is
impossible. The hub also remembers the latest time it trusted (in
`GPSMEM.TXT`, written at the first trusted time after the card is mounted
and then **at most once an hour**): GPS time more than 2 s before it is
untrusted (check 11) until GPS time runs past it, or the operator accepts
GPS time (console `T`, which also forgets the remembered time). Neither can
see a jump **forward** at boot: a spoofer at the first fix after power-up
that claims a later time is believed (and then remembered). The remembered
time is read when the card is mounted, which is just after the first trusted
GPS time (the log file is named from it): a spoofer with an earlier time
active at that moment has its first seconds of time recorded, then the floor
fires and the time is dropped.

## Receiver by receiver

| | u-blox MAX-M10S (SPG 5.10) | Quectel LC29H(AA) | Other NMEA |
|---|---|---|---|
| Satellites, C/N0, elevation | UBX-NAV-SAT | GSV (L1 and L5 entries; L1 used) | GSV |
| Jamming flag | MON-RF `jammingState` (needs CFG-ITFM-ENABLE, switched on) | `$PAIRSPF` (needs `PAIR391,1`, switched on) | none |
| AGC / CW indicator | MON-RF `agcCnt`, `cwSuppression` | none | none |
| Spoofing flag | NAV-STATUS `spoofDetState` (always on) | **none** (no spoofing detector in the spec) | none |
| Clock bias / drift | NAV-CLOCK | none | none |
| Accuracy | `hAcc`, `vAcc` | HDOP only | HDOP only |

Configuration (`crates/gnss/src/config.rs`, `quectel.rs`), sent at boot,
RAM (and the M10's battery-backed RAM) only:

- u-blox: step `integrity-out` sets NAV-SAT, NAV-STATUS, NAV-CLOCK and
  MON-RF to once per epoch on UART1 and I2C (about 500 bytes/s with 30
  satellites, at 115200 baud), step `itfm` sets CFG-ITFM-ENABLE. Default
  ITFM thresholds are kept (`BBTHRESHOLD` 3, `CWTHRESHOLD` 15, dB above the
  reference spectrum).
- LC29H: step `jamming-detect` sends `PAIR391,1`.
- All three steps are **optional**: a NAK or no answer is logged
  (`gps optional config step ...`) and the boot configuration carries on,
  with fewer checks. Other users of the crate (the S3 hub) get none of this
  unless they ask (`ConfigSequence::with_integrity`).

## False alarms, and how they are kept down

- **Indoor bench by a window.** Weak signals (15-35 dB-Hz), satellites
  coming and going, fixes dropping, positions wandering tens of metres.
  The tests run an hour of that, with people passing the window (8 dB for
  15 s) and the window blocked (12 dB for 25 s), on three random seeds:
  no alarm. Why: the C/N0 baseline is learned in the same place, so it is
  relative; a collapse needs 10 dB for 30 s; a flat sky only counts when it
  is also strong (>= 38 dB-Hz), which an indoor sky is not; the home radius
  grows with the reported accuracy (HDOP x 6 m indoors is 15-35 m, so the
  thresholds are 60-140 m and 90-210 m).
- **Carrying the hub through a door.** A 20 s fix loss with everything
  15 dB down does not alarm (30 s sustain).
- **Moving the hub indoors for good.** Suspect jamming after 30 s, cleared
  10 minutes later when the new level is accepted.
- **Moving the hub to another place.** Spoofing (position) until
  `integrity_forget_home()`. Intended: the monitor cannot tell a careful
  spoofer from a person carrying the hub.
- **A real sky that happens to be flat.** Six or more satellites at the
  same strength, within 4 dB, at 38+ dB-Hz, for 5 s: unusual under a real
  sky, where strength follows elevation (Li et al. below report a few dB
  of spread across real satellites and under about 1.5 dB for spoofed ones).
  If it happens on a hilltop with a high-gain antenna, raise
  `UNIFORM_MIN_SATS` or lower `UNIFORM_MAX_STD_DB`.
- **u-blox clock housekeeping.** The receiver steps its clock bias by whole
  milliseconds; those are removed before the bias check, and clock steps
  are corroborating only.
- **Receiver reacquisition.** Clock checks run only between two epochs that
  both had a fix.

## Known limits

- **A spoofer present at the very first boot**, before any home or time is
  remembered and with no placed home, is learned as home after 15 min (if no
  check fires) and its time is trusted and remembered; a forward time jump
  at any boot is believed. The u-blox detector has the same limit: it needs
  a transition from real signals (integration manual 3.5.2.2). The C/N0
  checks still apply, and a spoofer they catch from power-up gets nothing
  published at all. Place the home in the site file at a fixed site.
- **A slow, careful spoofer** that copies realistic C/N0 and drags the
  position is caught only at 75 m (suspect) and 300 m (strong) from home,
  and only after home is learned (15 min after boot). Until then, only jumps
  over 300 m are caught.
- **Held position during a drag** is home, so the drag never reaches the
  hub's position once home exists; before that, the last clean fix.
- **Time walk** within the crystal budget (1.7-3.5 s a day, above) is not
  caught. PPS comes from the same receiver, so it is spoofed with it: it
  makes the hub's check sharper (5 ms), not independent.
- **Aero nodes** follow the hub's clock; a slow walk reaches them too. Their
  own slot timing (when they receive FLARM/OGN) is not yet used as a check
  (docs/AERO-NODE.md).
- **LC29H has no spoofing flag and no AGC**: jamming on it rests on
  `$PAIRSPF` (not yet seen on real hardware) and C/N0; spoofing on C/N0,
  position and time.
- **Single-antenna, single-receiver.** No angle-of-arrival, no second
  receiver of its own to compare with. Other receivers nearby (aircraft
  ADS-B integrity, drones' positions) are used as a separate check, which
  takes this verdict as corroboration: docs/DRONE-ATTACKS.md "GPS
  interference in the area".

## What needs hardware

1. Does the MAX-M10S ACK `integrity-out` and `itfm`, and output NAV-SAT,
   NAV-STATUS, NAV-CLOCK, MON-RF at 1 Hz? (`gps optional config step` lines
   on the console mean no.)
2. Does the LC29H(AA) firmware ACK `PAIR391,1` and print `$PAIRSPF`?
3. Real C/N0 on the bench and outdoors: are the bench numbers in the range
   assumed here, and does an open sky ever look flat?
4. MON-RF `agcCnt` and `cwSuppression` on a quiet bench: is 30 % and 100 a
   sensible distance from normal?
5. An hour on the bench: no `gnss_*` events in the evidence log, `trusted`
   true in the `{"t":"hub"}` line after the first 10 s, `time_step_ms`
   within +-150 (no PPS) / +-5 (PPS).
6. The real crystal's drift against GPS over a day and over temperature: is
   20 ppm a true bound? (If not, `DRIFT_PPM` too small means false time
   alarms; too large means a faster walk goes unseen.)
7. `GPSMEM.TXT` written and read back across a reboot (home, time), and
   the remembered-time floor at a real first fix.
8. A shielded test with a GNSS simulator, if one is available (not on air:
   transmitting on GPS frequencies is illegal without authorisation).

## Sources

- u-blox, *u-blox M10 SPG 5.10 Interface Description*, UBX-21035062 R03
  (protocol 34.10): UBX-NAV-SAT 3.15.13.1, UBX-NAV-STATUS 3.15.17.1
  (`spoofDetState` in `flags2` bits 4..3), UBX-NAV-CLOCK 3.15.2.1, UBX-MON-RF
  3.14.6.1 (`jammingState`, `agcCnt` 0..8191, `cwSuppression` 0..255),
  CFG-MSGOUT 4.9.11, CFG-ITFM 4.9.9, NMEA numbering 1.5.3 table 3. This
  firmware has no UBX-SEC-SIG and no UBX-MON-HW message.
- u-blox, *MIA-M10C Integration Manual*, UBX-23001616 R05 (same SPG 5.10
  firmware): 3.5.2.1 jamming/interference monitor (ITFM off by default,
  CW indicator always on), 3.5.2.2 spoofing detection (always on, needs
  several constellations, cannot see spoofing present at start-up).
- Quectel, *LC29H Series & LC79H(AL) GNSS Protocol Specification* V1.3
  (2024-03-12): 2.4.32 `PAIR391` and `$PAIRSPF`/`$PAIRSPF5`, 2.2.3 GSV,
  appendix B table 11 (satellite numbers). No spoofing message in V1.3.
- Android developers, *Detect GNSS jamming and spoofing*
  (developer.android.com/develop/sensors-and-location/sensors/gnss-spoof-jam):
  jamming lowers C/N0 and AGC; spoofing tends to raise C/N0 while AGC drops.
- Li et al., arXiv:2510.16229, section V: real-sky C/N0 varies several dB
  across satellites, spoofed signals about 0.5-1.5 dB.
- The absolute C/N0 figures used here (open sky roughly 30-50 dB-Hz at a
  patch antenna, 55 dB-Hz as "too strong", 38 dB-Hz as "strong for a flat
  sky") are engineering estimates, not taken from a cited document; they
  are among the first things to check on hardware.
