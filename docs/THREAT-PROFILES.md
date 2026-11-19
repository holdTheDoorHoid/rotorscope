# Threat profiles: alerting on what a drone is doing, not only that it is there

Status: **design accepted 2026-11-18, branch `threat-profiles`; slices 16a, 16b and 16d built
(host tests and P4 firmware compile; not yet on the bench). See "Slice 16a as built", "Slice 16b as
built" and "Slice 16d as built" at the end.**

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

## Slice 16d as built (2026-11-18)

Rules 14-19 (across drones and days), for drones that report their position, confirmed tracks
only, on hubs that keep path history (the P4; the S3 hub and the laptop viewer keep none of it).
Code: `crates/tracker/src/behaviour/across.rs` (rules 14-19 and what they keep between seconds),
`visits.rs` (the repeat-visitor history and its file), `watch.rs` (per drone: the pilot's trail
and the serials), wired into `evaluate` in `mod.rs`; tests in `tests_across.rs` (the 16a
simulation gained UTC, a way to jump days, moving pilots, a serial change and a second ID). On the
hub: `firmware/hub-p4/src/visits.rs` (the file on the card and the console wipe); `sd.rs` only
gained an accessor for the folder. Site lines `crowd` and `visits`: [SITE.md](SITE.md).

The numbers in use (in `across.rs`, `visits.rs`, `watch.rs`; to tune on the bench):

| Rule | What | Value |
|---|---|---|
| all | "at or near a place" | inside it, or beyond its edge by at most its own size (a circle's radius, a polygon's farthest corner from its middle), and at least 50 m |
| 14 Spotter then striker | the spotter | lingering (rule 7 or 8, at Caution or above, not a friend) at a place |
| | the striker | a *different* drone (another track, another ID, not the continuation of the spotter) on a beeline or fast inbound (rules 5, 6) to the same place |
| | within | 10 min of the spotter's last lingering; Critical on the striker, whose reason names the spotter |
| | the spotter is marked | Warning, while a striker it led was seen in the last 5 s (then the usual 30 s hold) |
| 15 Several at once | a crowd | 3 drones at or near the places at once (`crowd N`, 2-20; `crowd off`), friends not counted; counted the second before |
| | or an alarm | any drone at or near a place while jamming (broadband, tone, sweep, pulsed), a blinded node or GPS interference in the area is active; muted alarms count (the jamming is still there). `crowd off` leaves this on |
| 16 Probing | left an area, went | at least 50 m beyond its edge |
| | and came back after | at least 30 s and at most 30 min; shown while it stays in |
| 17 Repeat visitor | seen at or near a place on | 3 different UTC days within 14 (today included) |
| | the same pilot spot | 25 m squares; spots up to two squares apart match (any two within 50 m do) |
| | kept | 30 days; 4 drone IDs and 2 pilot spots per path ring (256 and 128), the least recently seen one makes room |
| 18 Pilot on the move | the pilot's trail | up to 10 broadcast pilot positions at least 4.9 s apart, the newest under 15 s old, spanning at least 30 s |
| | moving | the average of its newest third at least 5 m/s from its oldest third, each half at least 3 m/s, and two thirds of the single steps at 2.5 m/s or more (one jump of a bad position is not a drive) |
| 19 Identity changes | (a) | the track continues an earlier one under another ID mid-flight (`continuity.rs` sets that, read only here) |
| | (b) | its Remote ID serial or DJI serial changed within the track |
| | counts for | 10 min after the change, while at or near a place |

How the new rules combine and what they give (decisions made here, open to change):

- **Probing and "in the area" are two signs**, as recommended: re-entry after leaving is the "test
  the response" pattern CISA lists, so a drone back over the area is a Warning.
- **Pilot on the move and "in the area" are two signs** too (a drone over the house flown from a
  moving car is the Orem stalking case): Warning. Near but outside, it is Caution.
- **Repeat visitor** raises the drone's other reasons by one level, never above Warning: a Caution
  becomes a Warning; Warning and Critical stay as they are; on its own it is a reason at Notice
  ("3rd day in 2 weeks at house (same drone)"). It is never a Critical.
- **Friends**: a friend is never a spotter, never counted in a crowd and never recorded as a visitor
  (its ID is not stored at all). Several at once, probing and pilot on the move give a friend only
  Notice (as the place rules). The striker's reason (14) is never lowered, like rules 5 and 6.
  **Identity changes are never lowered either**: switching to a friend's serial mid-flight is
  exactly how a faked friend would look.
- Policy as before: rises at once, comes down 30 s after the rule stops, reasons stay shown while it
  is held; snooze as in 16a.

The repeat-visitor history (owner decision: 30 days, wipeable):

- **What is kept**: for a drone, a 64-bit fingerprint of its ID (its serial, a registration, or
  its DJI serial; not the ID itself), and for a pilot spot the 25 m square, each with the UTC days
  it was seen at a place as bits. No times of day, no paths. Session and UTM IDs are new every
  flight and are not kept. Days older than 30 are dropped every day and when the file is read.
  Without GPS time nothing is recorded.
- **On the card**: `ROTORSCP/VISITS.DAT`, version 1: a 16-byte header, one 16-byte record per
  entry, an FNV-1a checksum; at most about 4.8 KB. Read once per card mount, after the site file.
  Written when it changed, at most every 5 min, and at once on a new UTC day, a wipe, `visits off`
  or after a bad file. A file with a wrong checksum, length or version is discarded whole and
  logged; it never crashes the hub.
- **Wiping**: console `W` twice within 5 s; `visits off` in the site file also wipes. There is no
  screen action: the Settings page's six rows fill the screen. Every load, discard, wipe and
  switch is an evidence `Setting` event (code 4) with the data byte 5; **no new evidence code**.

Where it differs from the design, or needs a word:

- **Probing is for fixed areas only.** The bubble moves with the unit, so drones go in and out of
  it as the carrier walks.
- **Spotter marking** happens only while the spotter is still heard. A spotter long gone is named
  in the striker's reason (and the JSON's `other`) only.
- **Several at once, "appearing"**: any drone at or near a place while an alarm is active counts,
  not only one that first appeared during it.
- **Identity change (c)** (Remote ID stops while the drone's radio link carries on) is not built:
  slice 16a's lost-track hold already keeps an inbound drone's level for 30 s, and tying a
  Remote ID track to a control link needs the link work. Future work.
- **A serial changing on the same transmitter** usually becomes a new track (the tracker keys
  drones by ID), which continuity links to the old one: rule 19 (a). (b) catches the case where
  the transmitter keeps another ID (a registration) and the track takes the new serial.
- **16c hook**: `Across::striker(track, key, previous, place, now)` is what a striker without a
  position (an FPV signal getting louder fast, rule 12) will call with the place it closes on; it
  gets the same answer and marks the spotter the same way.
- **Words and outputs**: reasons like `follows spotter drone SPOT-1 (lingered at house 3 min ago)`,
  `spotter: drone #15 is rushing to house`, `one of 4 drones at house at once`,
  `at house while jamming is heard`, `back over house after 5 min away (probing)`,
  `4th day in 2 weeks at house (same pilot spot)`, `pilot on the move at 15 m/s while it flies at
  house`, `new identity mid-flight at house (was drone IDA-1, 40 s ago)`. The console and evidence
  text name the other drone by its ID; CoT remarks by its track number. JSON reasons gain `other`
  (a track id), `n`, `alarm` (`"jamming"`, `"gps"`), `days`, `by` (`"drone"`, `"pilot_spot"`), and
  `ago_s`, `for_s`, `mps` for the new rules (see `json.rs`). Evidence code 28 carries the rule
  numbers 14-19 and 21 ("spotter for a striker"); [EVIDENCE-FORMAT.md](EVIDENCE-FORMAT.md).
- **Expected false alarms** (THREAT-TACTICS.md §6):
  - 14: a second hobbyist heading for the place within 10 min of another's hover (two friends in
    a park; with the bubble on, drones playing near you). `friend` lines for your own. Tuned after
    review: at the moving bubble a *slow* striker (a beeline under the fast-inbound speeds) makes
    rule 14 a Warning, not Critical; a fast striker, or any striker at a fixed area, is Critical.
  - 15: events, light shows, racing meets (`crowd off`); a jamming alarm that is itself false
    makes every drone near a place a Warning; a big bubble in a busy park.
  - 16: hobbyists who do not know the boundary; a survey drone whose passes cross the fence and go
    50 m beyond it more than 30 s apart.
  - 17: a neighbour who flies every day; delivery routes; a popular take-off field near the site
    (different pilots, the same spot).
  - 18: follow-me from a vehicle, boats, a pilot on a bus or train.
  - 19: a drone whose Remote ID restarts mid-flight with a new session ID; two drones taking turns
    on the same path (a relay), which is arguably worth a warning anyway.
- **Measured**: the P4 tracker is 393,904 bytes (16b: 371,864; the cross-drone state 10,768, of
  which the visit table 6,152; the per-drone pilot trail and serials 176 bytes a path ring). The S3
  tracker (`Tracker<64, 32>`) grew 24 bytes (87,728). The P4 firmware compiles; the main stack frame
  is 68,112 bytes (16b: 68,080); the once-a-second housekeeping frame grew from 3,008 to 3,712
  bytes (the rules are judged inside it, as in 16a and 16b; taking the judging out of line made the
  sum larger), and rules 14-19 run in their own 544-byte frame below it. The card file's reading
  runs in a 720-byte frame from the main loop, its writing in 608. The worst-case JSON line is 3,443
  bytes (the buffer is 3,456, unchanged). Host tests: 1,547 passed, 5 ignored (16b: 1,523).
