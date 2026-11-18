# Attacks on drones

Rotorscope only listens. This page collects what it can notice when
someone attacks the drones around it (or attacks everything around it in a
way that also hits drones), one section per attack family. Each section
starts in plain language, then gives the exact rules, thresholds and why,
the false alarms to expect, and sources. These detectors raise the threat
kinds added in link protocol 13 (`crates/link`, `ThreatKind`).

## GPS interference in the area

Status 2026-11-17: built and host-tested (`crates/tracker/src/area_gnss.rs`,
`tests_area_gnss.rs`), built into the P4 hub. **Not yet run against real
aircraft data from a jammed area, a real jammer or a real spoofer.** The
thresholds are first estimates.

### In plain language

Drones navigate by GPS. Two common attacks against them are **jamming**
(drowning out the satellite signals, so the drone no longer knows where it
is) and **spoofing** (sending fake satellite signals, so the drone believes
it is somewhere else and can be steered or made to land). Both are cheap
with a software radio. Both affect *every* GPS receiver within range, not
just the drone that was aimed at.

That is what the hub uses. It already hears many GPS receivers that are
not its own:

- **Aircraft.** Airliners and most light aircraft broadcast their GPS
  position (ADS-B), and with it two numbers saying how much they trust that
  position. When a jammer is on, aircraft nearby lose GPS and those numbers
  drop, several aircraft at the same time. The public map gpsjam.org shows
  GPS jamming around the world from exactly this.
- **Drones.** When a spoofer is on, every drone in range is pulled the same
  way at the same moment, so their reported positions all jump together.
  One drone jumping on its own is a broken or lying drone (the spoofing
  checks in docs/SPOOFING.md already flag it); two jumping together is the
  area.
- **The hub's own GPS.** It has its own jamming and spoofing checks
  (docs/GNSS-INTEGRITY.md). When it agrees with the aircraft or the drones,
  the alarm is more certain. It is never needed: the other receivers are
  enough on their own.

When the hub decides there is GPS interference in the area it raises an
alarm called **"GPS interference nearby"**: the status bar shows
**AREA JAM** or **AREA FAKE**, the health page has an **AREA GPS** line
(for example "JAMMED: 4/6 aircraft +hub", where "+hub" means the hub's own
GPS agrees), the alarm is written to the evidence log with a sentence
saying what was seen, and it goes out on the USB/console stream. Jamming
and spoofing are two separate alarms.

What it means for you: drone positions shown on the screen may be wrong,
and drones around you may behave strangely (drift, land, fly away). Someone
nearby may be running drone countermeasures, or a drone may carry a jammer.

### Rules

The hub looks once a second at the aircraft and drones it holds.

**Aircraft that count.** Only aircraft broadcasting their own position on
1090 MHz (ADS-B) or 978 MHz (UAT): not FLARM/OGN (no integrity figures),
not TIS-B (a ground station relaying radar tracks), not ground vehicles.
They must be **airborne** and **heard in the last 10 s**, and have reported
at least one integrity figure. Aircraft on the ground are left out: around
hangars and buildings their figures are poor for ordinary reasons.

**Degraded.** An aircraft is degraded when, for **5 s in a row**, any of:

- its **NIC** (integrity: the radius its position is guaranteed within)
  is below **7** (0.2 NM, 370 m), from a position heard in the last 10 s;
- its **NACp** (accuracy: 95 % error bound) is below **8** (0.05 NM, 93 m);
- its position has **stopped updating for 20 s** while it is still heard,
  and it was within **50 km** of the hub (many aircraft simply stop
  sending a position when they lose GPS; farther out, missing positions
  are just weak reception). Needs the hub's own position.

7 and 8 are the minimums the US ADS-B Out rule requires (14 CFR
91.227(c)(1)): an aircraft that meets the rule and has working GPS reports
at least these.

**Dropped.** A degraded aircraft that reported good figures (both at or
above the minimums) within the last **15 minutes**. This is the strong
evidence: some old transponders report low figures all the time, so a low
figure alone says little; the same aircraft *changing* from good to bad
says a lot.

**Jamming** is raised when either:

1. at least **2 aircraft dropped**, and `(degraded - 1) / counted` is at
   least **10 %**; or
2. at least **1 aircraft dropped** and the hub's own GPS says jamming
   (suspect or confirmed) at the same time.

The 10 % and the "minus one" come from gpsjam.org: it colours an area red
when more than 10 % of aircraft report low accuracy, and subtracts one bad
aircraft from each count "to limit possible false positives". Requiring
drops on top of that is ours. Aircraft that are low but never dropped are
counted (they show in the JSON) but never raise the alarm on their own.

**Spoofing** is raised when, within **10 s**:

1. **2 or more drones** jumped "alike"; or
2. **3 or more aircraft** jumped alike; or
3. **1 drone** jumped while the hub's own GPS says spoofing (two
   independent receivers moved at once).

A **drone jump** is the drone spoofing check's own position-jump test
(`spoof.rs`, `Check::PositionJump`: further than any drone flies between
two reports) firing on that track, with the position at least 60 m from
where it was a second earlier. An **aircraft jump** is a position more than
**2 km** off where the aircraft's previous position, speed and track put it
(positions at most 30 s apart). Two jumps are **alike** when their
displacements differ by at most 25 % of the larger one or a floor (75 m
for drones, 1 km for aircraft) — a spoofer that drags receivers moves them
all about the same way — *or* when they landed within 150 m (drones) / 3 km
(aircraft) of each other — a spoofer that broadcasts one fixed position
puts them all on the same spot.

**Confidence** (the alarm's `level`, 0-100): jamming from aircraft 50,
+10 per dropped aircraft beyond 2 (to 80); from one aircraft plus the hub
45; +20 when the hub's own GPS agrees. Spoofing from 2 drones 60 (+10 each
beyond, to 90), 3 aircraft 60, one drone plus the hub 50; +15 when drones
and aircraft both moved; +20 when the hub agrees. Capped at 95.

**How long.** An alarm stays raised for **60 s** after its evidence was
last seen, so a short episode stays on the screen and in the log.

### For the log and the stream

The alarm is an ordinary threat (`ThreatKind::AreaGnss`, raised by the hub:
`by_hub` true, node 0), so it appears in the `{"t":"threat"}` lines and the
evidence log's `Threat` event (data byte 0 = the kind). Its fields:

| Field | Meaning |
|---|---|
| `target[0]` | 1 jamming, 2 spoofing (two separate alarms) |
| `freq` | GPS L1, 1575.4 MHz |
| `level` | confidence 0-100 |
| `count` | receivers behind it (aircraft and drones, +1 for the hub's own) |
| `detail` bit 0 | jamming |
| `detail` bit 1 | spoofing |
| `detail` bit 2 | evidence: aircraft integrity (NIC, NACp, position lost) |
| `detail` bit 3 | evidence: aircraft positions jumped together |
| `detail` bit 4 | evidence: drone positions jumped together |
| `detail` bit 5 | the hub's own GPS agrees |
| `detail` bits 8-15 | jamming: aircraft degraded; spoofing: aircraft that jumped together |
| `detail` bits 16-23 | jamming: aircraft counted; spoofing: drones that jumped together |
| `detail` bits 24-31 | jamming: aircraft that dropped from good; spoofing: the jump in 100 m units |

The evidence text is a sentence, e.g. `GPS jamming in the area: 4 of 6
aircraft report degraded GPS (4 had been good); the hub's own GPS agrees
(confidence 70%)`.

Once a second, while anything is degraded, jumped or raised, the hub also
prints `{"t":"area_gnss","jamming":..,"spoofing":..,"jam_level":..,
"spoof_level":..,"jam_detail":..,"spoof_detail":..,"counted":..,
"degraded":..,"dropped":..,"air_jumpers":..,"drone_jumpers":..,"own":..}`.

### Where the numbers come from (NIC and NACp)

The ADSBee prints both in its `NICNAC` field (`crates/adsb`, module
`nicnac`): bits 15-14 SIL, 13-12 GVA, 11-8 NACp, 7-5 NACv, 4 NIC-baro,
3-0 NIC, as its `csbee_utils.hh` packs them. On 1090 MHz the NIC is not
sent as a number: it follows from the position message's type code and
the "NIC supplement" bits (DO-260B table 2-14); our own 1090 decoder
(`crates/modes`) now works it out the same way, and takes NACp from the
operational status message. On UAT (978 MHz) the NIC is in every state
vector and NACp in the mode status (`crates/uat`). A value of 0 means
"unknown" as well as "bad", and is also what is printed before anything
was received, so: NIC is only believed from a report that updated the
position; NACp 0 is ignored altogether (a jammed aircraft still shows up
through its NIC or its lost position).

### False alarms and limits

- **Old transponders** that always report low figures: never "dropped",
  so never an alarm on their own (tested).
- **Aircraft near the edge of reception**: positions missed for weak
  signal are not "lost" beyond 50 km, and a stale aircraft is not counted.
- **RAIM holes and receiver faults**: one aircraft at a time; two dropping
  together is required.
- **Military GPS testing** (published as NOTAMs) is real jamming and will
  be reported: that is correct, but not an attack on the drones near you.
- **Busy airport, few drops**: with many aircraft, two drops may stay under
  the 10 % share; real jamming hits every aircraft in range, so this
  mostly costs sensitivity for a very small, low-power jammer.
- **Drone track merges** can raise a drone's jump counter without a real
  jump; the 60 m minimum and the need for a second drone keep that from
  becoming an alarm.
- **A single aircraft whose ADS-B position is wrong** (spoofed alone, or
  broadcasting a false position) cannot be seen: that needs
  multilateration (timing one message at several receivers), which one
  hub cannot do.
- **Our 1090 decoder refuses positions that imply more than 1,200 kt**:
  a spoofer's big aircraft jumps may be refused there and never reach this
  check (they then show as "position lost" near the hub instead).
- **Drones losing GPS** (jamming) mostly stop sending a position; that is
  not used, because a drone landing looks the same.
- **No aircraft overhead, one drone**: nothing to compare; only the hub's
  own receiver can help.

### What needs checking with real data

1. Replay a CSBee log recorded where gpsjam.org shows jamming (or a log
   from an area during a published GPS test NOTAM): does it fire, and with
   what share?
2. A day of normal traffic around Philadelphia: no alarm, and how many
   aircraft are "always low"?
3. Does the ADSBee keep printing NIC/NACp for an aircraft that stops
   sending positions, and does its NIC go to 0 on TC 0?
4. A shielded spoofing test with two drones and a GNSS simulator, if one is
   available (not on air: transmitting on GPS frequencies is illegal
   without authorisation).

### Sources

- gpsjam.org FAQ (John Wiseman): aircraft reporting low navigation
  accuracy (NACp) per hex and day; green under 2 %, yellow 2-10 %, red over
  10 % of aircraft; one bad aircraft subtracted from each count.
- 14 CFR 91.227(c)(1): ADS-B Out performance — NACp < 0.05 NM (NACp 8),
  NIC < 0.2 NM (NIC 7).
- RTCA DO-260B, 2.2.3.2.7.2 and table 2-14 (NIC from type code and
  supplements); DO-282B (UAT state vector NIC, mode status NACp).
- ADSBee firmware (Pants for Birds, GPL-3.0),
  `firmware/common/comms/csbee/csbee_utils.hh` (NICNAC packing) and
  `firmware/common/adsb/aircraft_dictionary.cpp` (NIC from type code).
  Format followed, no code copied.
- Awesome-Drone-Hacking: "GPS Jamming Techniques for UAVs using Low-Cost
  SDR", "UAV Capture and Control via GPS Spoofing"; OWASP drone security
  cheat sheet (navigation consistency checks).
