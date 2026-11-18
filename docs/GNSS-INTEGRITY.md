# GNSS integrity: is the hub's own GPS being jammed or spoofed?

Status 2026-11-17: built and host-tested (`crates/gnss/src/integrity.rs`,
`tests_integrity.rs`), built into the P4 hub. **Not yet run against a real
receiver, a real jammer or a real spoofer.** Every threshold below is a
first estimate to be checked on the bench and outdoors.

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

**Holding** (`firmware/hub-p4/src/gps.rs`):

- Position: the learned *home* (below) if there is one, else the last fix
  from an epoch in which no spoofing check fired at all. The held fix has
  `held = true`, counts as fresh for range and FAA checks (the hub does not
  move), and keeps the time of the trusted fix in `at_ms`, so the evidence
  log's *fix age* keeps growing and shows the position is old.
- Clock: the last time anchor from such an epoch; UTC then runs on from the
  hub's own crystal (`time_held`). After 2.5 s the evidence context says
  `free_running`, the same as with no GPS at all.
- The receiver status (fix type, satellites) still shows what the receiver
  claims.
- Both are released when the verdict returns to `ok`.

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
| | | | | 7 | UTC jump vs hub clock | ms (signed) |
| | | | | 8 | receiver clock bias step | ns |
| | | | | 9 | receiver clock drift step | ns/s |
| | | | | 10 | impossible satellites | count |

For the screen and web page: `Gps::integrity()` returns the verdict, the
check behind it (`cause`), and its figures (`metrics`: C/N0 mean, spread,
baseline, AGC, CW, receiver flags, distance from home, time step).
`Gps::integrity_forget_home()` is for when the hub is moved on purpose.

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
| UTC vs hub clock | all | GPS UTC minus the hub's millisecond clock moves by > 2 s + 100 ppm of the time since the reference = suspect | one-shot |
| Clock bias step | u-blox (NAV-CLOCK) | bias off (previous bias + drift x dt) by > 1 us after removing whole-ms jumps; corroborating only | one-shot |
| Clock drift step | u-blox (NAV-CLOCK) | drift changes > 100 ns/s between two 1 s epochs with a fix; corroborating only | one-shot |
| Impossible satellites | all | a satellite number its constellation cannot have, elevation outside +-90, azimuth outside 0-359; corroborating only | 3 |

Accuracy is the receiver's estimate (u-blox `hAcc`/`vAcc`); NMEA has none,
so HDOP x 6 m is used (vertical: 1.5 x horizontal).

**Home.** The hub is normally static. After `HOME_LEARN_EPOCHS` = 900
usable fixes (15 min at 1 Hz) that all stay within max(40 m, 3 x accuracy)
of their running mean, and none with an accuracy worse than 50 m, the mean
becomes home. Learning only happens while the verdict is `ok` and no
spoofing check fired that epoch. Home is not stored: every boot learns it
again. **If the hub is moved on purpose while running**, the position
check will (correctly) call it spoofing until `integrity_forget_home()` is
called; the screen integration should offer that.

**Time.** The reference is GPS UTC minus the hub's own millisecond clock,
re-taken every trusted epoch (catches jumps) and once an hour (catches a
steady drag faster than about 0.6 s per hour). NMEA time counts when RMC
says valid; u-blox time only when NAV-PVT says fully resolved. The
allowance (2 s, plus 100 ppm for the crystal) covers NMEA arrival jitter and
a leap second. A time drag slower than that is not caught by this check.

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

- **A spoofer present from power-up** is learned as home. The u-blox
  detector has the same limit: it needs a transition from real signals
  (integration manual 3.5.2.2). The C/N0 checks still apply.
- **A slow, careful spoofer** that copies realistic C/N0 and drags the
  position is caught only at 75 m (suspect) and 300 m (strong) from home,
  and only after home is learned (15 min after boot). Until then, only jumps
  over 300 m are caught.
- **Held position during a drag** is home, so the drag never reaches the
  hub's position once home exists; before that, the last clean fix.
- **Time drag** slower than ~0.6 s/hour is not caught by the time check.
  PPS is not used as a separate check: it comes from the same receiver.
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
5. An hour on the bench: no `gnss_*` events in the evidence log.
6. A shielded test with a GNSS simulator, if one is available (not on air:
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
