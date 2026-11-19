# Threat profiles: alerting on what a drone is doing, not only that it is there

Status: **design accepted 2026-11-18, branch `threat-profiles`; slice 16a built (host tests and
P4 firmware compile; not yet on the bench). See "Slice 16a as built" at the end.**

Owner decisions 2026-11-18: fixed areas and the moving bubble matter equally (both in 16a);
repeat-visitor history (drone IDs and pilot spots on the SD card) is kept **30 days**, wipeable
from the hub; build 16a next.
Research behind it: [THREAT-TACTICS.md](THREAT-TACTICS.md) (sourced catalogue of hostile and
criminal drone tactics, counter-drone products, and lightweight trajectory methods).

## The idea in one paragraph

Today the hub says "a drone is here" and raises named alarms for radio attacks (jamming,
deauth, spoofing...). This layer sits above that. It watches each drone's **behaviour over time**
against **places the owner cares about** (a fixed site, or a bubble that travels with a handheld
unit) and gives every drone **one threat level with plain reasons**: "Warning: heading straight
for you, 180 m away, arrives in about 14 s". It runs entirely on the hub, from data the tracker
already has, so it needs **no change to the radio link between boards** and does not collide with
the Phase 3 link-key work.

## What the hub can and cannot know (this shapes everything)

Behaviour rules need to know *where* the drone is. How much we know depends on what it transmits:

| What we hear | What we learn | Behaviour rules possible |
|---|---|---|
| Remote ID, DJI DroneID, MAVLink telemetry | position, height, speed, heading, often the pilot's position, about once a second, to about 30 m | all of them |
| FPV video, ELRS/SiK control link, drone Wi-Fi | "it exists" and how loud it is at each node | getting closer (loudness trend), how many, how long |
| Rotor sound (acoustic branch) | "it exists" and how loud | getting closer, and **a drone with no radio at all** |

So "an FPV drone coming straight at you" can be judged precisely only for drones that report their
position. A racing or attack FPV drone usually does not; for those the hub can only say "an FPV
signal appeared and is getting louder fast", which is a weaker, trend-based cue (research §4:
banking alone swings signal strength by over 10 dB, so it must be a sustained rise, ideally seen on
more than one node). A fibre-optic FPV drone transmits nothing; only the microphone can catch it.
The screen and phone page must say which kind of judgement an alert is.

## Places to protect

Written in `SITE.TXT` (same rules as today: appended, later lines win) and settable from the hub:

```text
area house  circle 39.95210 -75.16380 150            # name, centre, radius in metres
area yard   poly   39.9520 -75.1640 39.9523 -75.1636 39.9519 -75.1633
area house  ceiling 120                              # optional height band, metres above the unit
area here 200                                        # (screen/console) circle round the unit now
bubble 300                                           # a circle that moves with the unit's GPS
friend 1581F5FKD229400B4WW1 our own Mini             # a known drone (see "Friends" below)
profile patrol                                       # preset thresholds (see "Profiles")
```

The **bubble** follows the hub's own GPS, so a handheld unit protects its carrier while walking or
driving. That needs the hub's own speed and direction, which the tracker does not keep yet
(`OwnFix` has position only): an early job.

## The threat level

Every drone gets one level, the highest any rule gives it, plus the reasons:

| Level | Meaning | Example |
|---|---|---|
| **Notice** | a drone is around, nothing about it is unusual | transiting 800 m away |
| **Caution** | something worth watching | inside the protected area; circling; pilot standing at the fence |
| **Warning** | behaviour that matches a known hostile pattern | heading straight for you; hovering low over the yard for 5 min; same drone 4th day this week |
| **Critical** | something may arrive within seconds | fast drone inbound, under 30 s to arrival; a second drone rushing in after a spotter |

Rules about one drone rarely prove anything on their own (CISA's guidance says the same), so
**combinations raise the level**: circling *and* a repeat visitor, or a dark (radio-silent) drone
*and* getting closer, go up one level.

Design choices made here (human-factors calls, open to change):

- **Levels go up at once and come down slowly** (about 30 s of clear behaviour), so an alert does
  not flicker.
- **A drone that vanishes while inbound and close keeps its level** for 30 s ("last seen inbound,
  180 m, 10 s ago"). Losing a low drone behind trees is exactly when it matters (research §1.7).
- **Friends are lowered, never hidden.** Remote ID serials can be faked, so a `friend` line caps a
  drone at Notice for area and circling rules but never silences "fast inbound". A friend that
  behaves like an attacker still alerts.
- **Every rule can be snoozed, none can be switched off from the screen**, matching the existing
  mute rules (WP-11b).
- Alerts are written to the evidence log with the reasons, so a later review sees why the hub
  thought what it thought.

## The behaviour rules

Grouped by what they need. Numbers are starting points to tune with tests and a real drone.

**Places (need a position)**
1. **In the area**: inside a fixed area or the bubble. Caution.
2. **Pilot in or near the area**: the broadcast pilot position is inside, or at the fence. Caution.
3. **Drone in, pilot outside**: flying over the site from beyond the fence (the common prison,
   stalking and substation pattern). Caution.
4. **Above the legal ceiling** (120 m) inside the area. Caution.

**Approach (need a position; uses the closest-approach maths already in `conflict.rs`)**
5. **Beeline**: flying toward you, a fairly straight path, predicted to pass within the area within
   60 s. Warning.
6. **Fast inbound**: as 5, faster than 25 m/s (camera drones top out near 16-21 m/s; FPV attack
   drones 40+ m/s) or under 30 s to arrival. Critical.

**Lingering (need a position and a recent path)**
7. **Loitering**: staying within a small circle for minutes. Caution, Warning when long and low.
8. **Circling**: turning through a full circle or more around a point in or near the area. Caution.
9. **Low hover over the area**: under about 15 m for over 10 s. Warning.
10. **Drop pattern**: a quick descent, a pause, a climb, inside the area (contraband or payload).
    Warning.
11. **Following**: stays near a moving handheld unit while it travels over 200 m. Warning.

**No position (signal strength and sound)**
12. **Getting louder fast**: an FPV video or control signal rising steadily (about 6 dB in 5-10 s),
    on more than one node when possible. Caution, Warning with 13.
13. **Dark drone**: rotor sound with no Remote ID, DroneID or radio link heard. Warning.

**Across drones and days**
14. **Spotter then striker**: a lingering drone, then a new fast inbound one within 10 min.
    Critical. (The most common attack sequence in the research.)
15. **Several at once**: three or more drones at the same time, or drones appearing while the hub
    sees jamming or GPS spoofing. Warning.
16. **Probing**: entering, leaving and re-entering within 30 min. Caution.
17. **Repeat visitor**: the same drone or the same pilot spot on several days in two weeks. Raises
    others by one level. Needs a small history on the SD card (see open questions).
18. **Pilot on the move**: the pilot's position travelling over 5 m/s while the drone flies (launch
    from a vehicle). Caution.
19. **Identity changes**: Remote ID stops while the drone's radio link carries on, or the serial
    changes mid-flight. Ties into the existing spoofing checks. Warning.

Known false alarms are listed per rule in THREAT-TACTICS.md §6 (estate photographers circle,
inspectors hover low, delivery drones fly straight past). The closest-approach test keeps
"flying straight past" from counting as a beeline.

## Profiles

Presets that set the thresholds, so nobody tunes nineteen numbers by hand:

- **home**: fixed circle, repeat visitor and low hover weighted up (stalking, casing).
- **facility**: fixed polygon, pilot-outside and circling weighted up (infrastructure, prisons).
- **event**: many friendly drones expected; several-at-once off, approach rules on.
- **patrol**: handheld bubble, following and fast inbound weighted up.

## How it is built

- A new module `crates/tracker/src/behaviour/`, pure Rust and host-tested like the rest of the
  tracker. Hub-side only: **no link protocol change**.
- A short **path history per drone**: one point a second for 2 minutes, one every 5 s for
  20 minutes. About 4.4 KB per drone (12-byte points), about 280 KB for 64 drones, in PSRAM inside
  the tracker (the tracker must stay under 1 MiB; it is 369 KB).
- The hub's own speed and direction added to `OwnFix` for the bubble.
- Output: level and reasons on the screen, `/api/state` JSON, CoT (TAK colours), the SD log and the
  evidence log (a new evidence code).
- Tests come in two kinds. **Scripted flights** run on the host: beeline, fast dive, orbit, a
  photographer's orbit (must stay low), a delivery drone passing (must stay Notice), a drop, a
  follow. Then **the bench**: `rid-beacon` flies the same scripted paths as Remote ID (fake IDs,
  lowest power, bench only, as already allowed), so the real hub sees them over the air. A real
  drone flight comes last, when one is available.

## Slices

| Slice | Contents | Depends on |
|---|---|---|
| 16a | path history, areas, bubble, own speed, threat level and reasons, friends, rules 1-6, lost-track hold, screen/JSON/evidence | nothing |
| 16b | rules 7-11 (lingering, circling, low hover, drop, following) | 16a |
| 16c | rules 12-13 (signal and sound trends, dark drone) | 16a; rule 13 needs the acoustic branch |
| 16d | rules 14-19 (across drones and days, repeat-visitor history on SD) | 16a-b |
| 16e | profiles, tuning from bench and field runs, drawing areas on the phone map | web head linking |

## Slice 16a as built (2026-11-18)

Code: `crates/tracker/src/behaviour/` (rules and level policy in `mod.rs`, `path.rs`,
`places.rs`, scripted flights in `scenario.rs`, tests in `tests.rs`); site lines in
`crates/tracker/src/site.rs` (see [SITE.md](SITE.md)); outputs in `json.rs` (`"threat"` on every
track line), `cot.rs` (remarks, and orange at Warning, red at Critical), the screen's bottom strip,
and the evidence log (code 28 `threat_level`, [EVIDENCE-FORMAT.md](EVIDENCE-FORMAT.md)).

The numbers in use (all in `behaviour/mod.rs`, to tune on the bench):

| What | Value |
|---|---|
| a position is judged while it is younger than | 5 s |
| area checks use the average of the last | 5 positions, moved on along the drone's velocity |
| a drone counted inside stays inside until it is beyond the edge by | 10 m |
| "fairly straight": the path of the last 20 s (at least 10 s of it, 5 s above 15 m/s), its two halves within | 25 degrees of each other, each half over 15 m, and within 30 degrees of the reported direction |
| an approach needs a speed towards the place of at least | 3 m/s |
| "heading for it" when already inside: passing the middle within | a quarter of the area's size, at least 40 m |
| pilot "at the fence" | within 30 m outside |
| a level comes down after | 30 s without the rule that raised it |

Where it differs from the design above, and why:

- **Path history only on the P4 hub.** The tracker has a third size, `P`, for the drones with a
  history. The P4 hub uses one per track; the S3 hub and the laptop viewer use none (their
  tracker stays the size it was, apart from about 170 bytes a track for the level itself). With no
  history only rules 1-4 could fire, on the latest position alone; the S3 hub does not run the
  levels at all yet.
- **Two signs.** Rules 2, 3 and 4 all need the drone (or its pilot) in or at an area, and rule 1 is
  "in the area", so a drone over an area with its pilot outside (1 + 3), with its pilot inside
  (1 + 2) or too high (1 + 4) is a **Warning**, as the brief asks. With the bubble on, a hobbyist
  flying near you with the pilot also near you is therefore a Warning. Worth watching on the bench
  and in the field; the profiles (16e) are the place to soften it, for example by not counting
  rule 2 together with rule 1 in the bubble.
- **Rule 4 measures height above the unit** (the drone's GPS height minus the hub's), else the
  height the drone broadcasts above its take-off point or the ground. It applies to the bubble
  too, with the default 120 m.
- **Lost-track hold.** The 30 s are counted from when the drone was last heard (a position up to
  5 s old is still judged, so counting from the last judgement would hold it 35 s).
- **Snooze.** There is no per-drone button on the screen yet: the Settings "Alarms" Snooze snoozes
  every drone at Warning or above for 5 minutes (the tracker can snooze one drone; the screen
  does not offer it yet). A snoozed drone leaves the strip; its level is still in the JSON and the
  evidence log.
- **Evidence.** One `threat_level` event per rise to Caution or above (not for Notice); a fall is
  not logged.
- **Screen.** One red (Warning, Critical) or yellow (Caution) line at the top of the bottom strip,
  for the most serious drone. The strip keeps its four lines: when all five would show, the
  silenced-alarms and switches lines share one.
- **Friends** match any of the drone's IDs, the DJI serial included.
- Only confirmed tracks (WP-09 probation) are judged, so one forged frame raises nothing.
- On the console a site line is at most 120 characters, so a polygon of more than about five
  corners goes in the file (file lines may now be 384 characters).
