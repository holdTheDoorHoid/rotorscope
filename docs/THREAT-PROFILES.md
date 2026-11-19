# Threat profiles: alerting on what a drone is doing, not only that it is there

Status: **design accepted 2026-11-18, branch `threat-profiles`; slices 16a and 16b built (host
tests and P4 firmware compile; not yet on the bench). See "Slice 16a as built" and "Slice 16b as
built" at the end.**

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
   drones 40+ m/s), or under 30 s to arrival while closing at 10 m/s or more. Critical.

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
  flying near you with the pilot also near you was therefore a Warning; **slice 16b changed this**
  (see below): in the bubble a nearby pilot no longer counts towards the pair.
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

## Slice 16b as built (2026-11-18)

Rules 7-11 (lingering, circling, low hover, drop, following), for drones that report their
position, confirmed tracks only, from the path history 16a keeps (so on the P4 hub only). Code:
`crates/tracker/src/behaviour/linger.rs` (rules 7-10), `follow.rs` (rule 11), wired into
`evaluate` in `mod.rs`; new scripted flights in `scenario.rs` (a drop, a lawnmower survey) and the
tests in `tests_linger.rs`. The hub's own trail (for rule 11) is in `path.rs`.

The numbers in use (all in `linger.rs` and `follow.rs`, to tune on the bench), and why:

| Rule | What | Value | Why |
|---|---|---|---|
| 7 Loitering | every position within | 50 m of where it is now | a Remote ID position is good to about 30 m, so a still drone shows as a cloud about that wide; 50 m still means "one spot" |
| | Caution after | 3 min | a passing drone or one photo is over within a minute or two |
| | Warning after | 5 min, **and** under 40 m above the unit | the design's own example ("hovering low over the yard for 5 min"); 40 m is about where a camera picks out faces and windows. Lower than 15 m is rule 9 |
| | counts in an area or within | 50 m outside it | watching from just beyond the fence |
| 8 Circling | turns added up to | 360 degrees, mostly one way (all the turning at most 1.5 times the net turn) | a real circle turns one way; jittery positions turn both ways |
| | the path followed in steps of at least | 15 m (positions first averaged over 5, one a second) | wobble of a hovering drone's position makes no turns |
| | a step turning more than | 100 degrees ends the search | jumps of the position turn every which way |
| | at least | 8 turns, at least 1 m/s along the path | the smallest circle counted is about 20 m in radius; tighter is within the error of the positions |
| | circle's radius under | 150 m | wider is not "round one place" |
| | within the last | 5 min (first the last 2 min at one point a second, then 5 min at one every 5 s, averaged in threes, for slow wide circles) | |
| | counts when | the circle's middle is in the place, or the circle reaches into it | the estate photographer next door does not count |
| | the circle must close: the drone back within | one radius (at least 30 m) of where the turn began | a drone that circled and flew off stops counting about one radius later |
| 9 Low hover | inside a place, under | 15 m above the unit | window height (the design and the research) |
| | nearly still (within 15 m of where it is now) for | 10 s | |
| 10 Drop | heights averaged over | 5 reports | GPS heights wobble a few metres |
| | down at least | 10 m | |
| | a pause near the bottom (within 3 m) of | 3 s | a drop needs a moment to release |
| | then up at least | 8 m | a drone that lands and stays down is not a drop |
| | all within | 60 s, the bottom inside the place (or within 10 m of its edge) | |
| 11 Following | the unit travelled at least | 200 m (straight line, so pacing a street does not add up) | |
| | the drone within, the whole way | the bubble's radius, at least 100 m (one wild position forgiven) | |
| | and the drone itself moved | at least half as far, within 45 degrees of the unit's direction | keeps out a drone you walk past or one hovering where you walk |

Policy, as 16a (rises at once, comes down 30 s after the rule stops; the reasons stay shown
while it is held). How the new rules combine:

- **Loitering and "in the area" are one sign**, not two: loitering *is* being there, for longer.
  So a drone loitering over the area is Caution (and Warning only when long and low), not a
  Warning after 3 minutes. Loitering just outside the fence is Caution on its own.
- **Circling and "in the area" are two signs**: a drone circling over the area is a Warning
  (the brief's "orbit over the area"). A circle that takes the area in without flying over it
  is Caution.
- Low hover, drop and following are Warning on their own.
- **Friends**: rules 7-11 give a friend only Notice (as rules 1-4). Following is lowered too,
  because a follow-me selfie drone is the classic friendly case. The approach rules (5, 6) are
  still never lowered.
- **The 16a tuning issue, fixed**: in the bubble, a pilot near you (rule 2) no longer counts
  towards the two-signs Warning (a pilot nearby is normal in a park). Both reasons are still
  shown. For fixed areas nothing changed: drone and pilot both inside is still a Warning.

Where it differs from the design above, or needs a word:

- **The bubble moves**, so for loitering only the time the unit itself stood about (within 50 m)
  counts: a drone hovering where you walk is not loitering at you (and one that keeps up is
  following).
- **Low hover beside its own pilot in the bubble** does not count (that is a take-off or a
  landing next to the person flying it). In a fixed area it counts.
- **Expected false alarms** (THREAT-TACTICS.md §6): a roof or gutter inspector hovering low is a
  Warning, and so is a drone that lands inside the area and sits there (rule 9). A photographer
  dropping for a shot and climbing again would look like a drop. These are the price of catching
  window-peeping and payload drops; the profiles (16e) are where to soften them.
- **Heights.** Rule 9 uses the height above the unit as 16a does (the drone's GPS height minus the
  hub's, else the height it broadcasts above take-off or ground); no height, no rule 9. Rule 10
  works on the GPS heights kept in the path, so a drone that broadcasts no GPS height makes no
  drop (a mix of the two kinds of height could fake a dip).
- **The unit's own trail**: one point every 5 s, four points for each drone path ring (256 on the
  P4 hub, about 21 minutes, 3 KB). Hubs without path rings keep none. A gap in the hub's fixes of
  over a minute starts it again. The P4 tracker grows by about 3 KB (still well under 1 MiB).
- **New numbers in the outputs**: JSON reasons may carry `for_s` (how long: loitering, circling,
  low hover, following) and `ago_s` now also for the drop; `h_m` for loitering, circling, low hover
  and drop. Evidence code 28 carries the rule numbers 7-11. Reason words, for example
  `lingering at house for 5 min, 30 m up`, `circling shed (100 m radius), a full turn in 79 s`,
  `hovering low over house, 8 m up, for 12 s`, `dropped 31 m over house (down to 20 m up) and
  climbed again, 4 s ago`, `following you for 420 m (5 min)`.
- **One hit per rule** per fixed area and per bubble is now kept while judging (the most serious,
  then the nearest), so many overlapping areas can no longer overflow the list of hits.
- **Measured**: the P4 tracker is 371,864 bytes (16a: 368,784; the own trail is 3,076). The P4
  firmware compiles; its main stack frame is unchanged (68,080 bytes); the once-a-second
  housekeeping frame grew from 2,736 to 3,008 bytes, and the new path checks run in their own
  frames below it (848 bytes for rules 7-10, 208 for rule 11). Host tests: 1,523 passed, 5 ignored
  (16a: 1,503).
- **Tuned after review (16a rule 6)**: a survey drone flying a pass straight over the middle of
  the area at walking pace was Critical (it "arrives within 30 s"). Now "under 30 s to arrival"
  makes Critical only when the drone is also closing at 10 m/s or more; slower passes stay at
  Warning (rule 5). Faster than 25 m/s is Critical at any distance, as before.
