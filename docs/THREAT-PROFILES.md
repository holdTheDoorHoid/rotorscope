# Threat profiles: alerting on what a drone is doing, not only that it is there

Status: **design accepted 2026-11-18, branch `threat-profiles`; slices 16a to 16e built
(host tests and P4 firmware compile; first bench pass 2026-11-19 over the air with the test
beacon's scripted paths, see "Bench pass" at the end; no real drone yet; 16c's sound half waits
for the acoustic branch). See "Slice 16a as built", "Slice 16b as built", "Slice 16d as built",
"Slice 16c as built" and "Slice 16e as built" at the end.**

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
| 16c | rules 12-13 (signal and sound trends, dark drone) | 16a; rule 13 needs the acoustic branch (built 2026-11-19 with a plain sound input; see below) |
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
  silenced-alarms and switches lines share one. On the P4's 800x480 layout (merged from main
  2026-11-19) it is a filled bar at the top of the strip above the keys, over the fault bars and
  notes lines (red from Warning up, caution-coloured at Caution; an outline in Red Night), cut to
  the panel's width (the line is at most 64 characters and fits). Simulated only
  (`wide_threat_bar_above_the_keys` and the `threat_strip_*` snapshots); not yet seen on the panel.
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
  switch is an evidence `Setting` event (code 4) with the data byte 7 (5 until the merge of main's
  site-file size limit, which took 5); **no new evidence code**.

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

## Slice 16c as built (2026-11-19)

Rules 12 and 13, for what has **no position**: FPV video, a control link's downlink, drone Wi-Fi,
open FPV links, and rotor sound; and rule 14 for a striker without a position. Code:
`crates/tracker/src/behaviour/trend.rs` (the level history, the rules, the place choice, the words),
wired into `Tracker::tick_behaviour` after the drones; tests in `tests_trend.rs`. Each signal gets
its **own** threat entry (`SignalThreat`, with the same `TrackThreat` level a drone has, moved by
the same policy: rises at once, comes down 30 s after, snoozable), because these signals are not
drone tracks and cannot be tied to one. Like the other slices it runs only on hubs that keep path
history (the P4); the S3 hub and the laptop viewer keep no signal history.

Where the levels come from (recorded as they arrive, per node, so two nodes hearing one emitter keep
two histories even though the emitter table keeps only the latest node):

| Signal | Level used | Which ones |
|---|---|---|
| RF emitter | dB above the node's noise floor | analog video, HDZero, LTE-style, narrow Wi-Fi, DJI video, DJI O4 video, DJI FPV air unit. **Not** the DJI controller (the pilot's), an unconfirmed FPV carrier (a Wi-Fi access point looks the same) or plain energy |
| control link | RSSI (dBm) | only the drone's own packets (downlink, or direction unknown) of a link at least "likely"; the uplink is the pilot's radio, which does not move with the drone. Recorded from `ControlTable::ingest`, which the hub keeps outside the tracker |
| drone Wi-Fi | RSSI | every Wi-Fi drone (the transmitter may be the drone or its controller) |
| open FPV link | RSSI | air-to-ground and unknown-direction streams |
| rotor sound | loudness (dBFS) | handed in once a second (`Tracker::hear_sound`), see below |

The numbers in use (all in `trend.rs`, to tune on the bench):

| What | Value | Why |
|---|---|---|
| history kept | 32 s of one-second levels (the loudest each second), 3 nodes per signal | 12 s for the trend, 20 s before it for "louder than before" |
| the trend is judged over | the last 12 s; at least 5 seconds with a sample, spanning at least 5 s, the newest at most 2 s old | sparse band scans (one sample every 2 s) still work |
| a rise is | at least 6 dB between the median of the first three samples and the median of the last three | 6 dB is halving the distance in free space (research §4); medians of three ignore one noisy sample |
| steadily | Kendall's tau at least 0.5 (most pairs of samples agree it went up) | banking swings of 8 dB every few seconds fail it, a real rise with them on top passes |
| not a step | no step between consecutive medians over two thirds of the rise | a video transmitter leaving pit mode (+14 dB in one second) is not an approach |
| a new high | the new level above every sample of the 20 s before the window | a drone coming out of a null or a bank is back where it was, not closer |
| not a crossing | no other node of the same signal falling steadily by 6 dB at the same time | research §4: up on one, down on another is a crossing track |
| Warning | rising on two or more nodes at once, or the microphone agreeing (rotor sound getting louder, a dark drone, or a dark drone's level still held while the sound is heard) | rule 12's "Warning with 13" |
| rule 13 | rotor sound heard for 5 s, not a low hum, not explained by a crewed aircraft, and no drone heard by radio from 15 s before the sound began until now | the acoustic branch's 5 s and 15 s |
| "a drone heard by radio" | a confirmed drone track (but not one whose position is over 600 m from the hub: rotor sound carries 50-200 m), an open FPV link, an emitter of a kind that alerts, a Wi-Fi drone, a control link | a forged Remote ID far away cannot hide a dark drone; one forged near the hub can (and is itself in the bubble or the area) |

Choices made here, and why (open to change):

- **Which place a signal closes on.** It has no position, so the place is that of the node(s)
  hearing it rise. The hub's own receiver (node 0), the microphone, and every node whose position
  the site file does not give count as **at the hub**: the bubble when it is on, else the area(s)
  the hub stands in or is nearest to (within the area's own size, at least 50 m, as 16d's "at or
  near"), else the only area if there is exactly one. A node placed with a `node` line gives the
  area(s) it stands in or is nearest to, else the bubble when the node is inside it. When nothing
  fits, the rule still fires and the words name no place ("FPV video signal getting louder fast on
  1 node (+8 dB in 7 s)"; JSON `place` null). A rise seen on two nodes in different areas names the
  first; the striker check tries each.
- **Rule 12 levels**: Caution on one node; Warning on two at once or with the microphone. Rotor
  sound getting louder is rule 12 too: Caution, Warning with a radio signal getting louder at the
  same time; dark and getting louder is a Warning (12 with 13), not Critical: lawn machinery and
  traffic also get louder as they come.
- **Rule 14 without a position**: a radio signal getting louder (not a sound: the spotter's own
  rotors may be what gets louder) calls `Across::striker_if` with the place, as 16d left the hook.
  Critical at a fixed area; Warning at the moving bubble (as 16d's slow striker there: the signal
  has no speed). A spotter that is itself still heard flying in (its own beeline or fast inbound)
  is not paired with it: the signal may be its own video. The spotter is marked as in 16d and its
  reason names the signal: `spotter: a signal getting louder (S3) is closing on house` (JSON
  `sig`, the signal's entry id, in place of `other`).
- **Friends** cannot cover these signals: a `friend` line names a serial, and none of them carries
  one. Nothing is lowered.
- **Words say it is a trend**: `FPV video signal getting louder fast on 2 nodes (+8 dB in 7 s),
  nearing house`, `..., rotor sound too`, `rotor sound getting louder (+7 dB in 6 s), nearing you`,
  `rotor sound, no radio heard, for 29 s, near house`; the evidence and console text start with what
  it is and "(no position)": `Warning: Analog video 5740.0 MHz (no position): ...`. Screen strip:
  `CAUTION FPV video louder fast, house`, `WARNING FPV video louder fast, 2 nodes, house`, `WARNING
  rotor sound, no radio heard`, `CRITICAL FPV video after spotter to house`. The strip shows a
  signal only when it outranks the most serious drone (on a tie the drone's, judged from its
  position, wins). The screen's Snooze snoozes signals at Warning or above too (one `Setting`
  event each).
- **Outputs**: a `{"t":"signal_threat"}` line per signal with a level (P4 console, once a second),
  with `basis` (`"signal_trend"` or `"sound"`: never a position), `src` and `src_id` (the table and
  entry it came from), `kind`, `what` (from the firmware's own tables, never over-the-air text),
  `nodes` (rising now) and `threat` as a drone's; reasons for rules 12 and 13 have no `m` and gain
  `db`, `nodes`, `kind`, `agreed` (`json.rs`). `/api/state` (`state_full`) gains
  `"signal_threats":[...]` after the Wi-Fi drones. Evidence code 28 carries rule numbers 12 and 13,
  and flag bit 4 ("no position": the id is the signal's entry, not a drone track).
  [EVIDENCE-FORMAT.md](EVIDENCE-FORMAT.md).
- **No CoT event for a signal.** CoT is a point on a map; a signal without a position has none, and
  putting one at the hub or the area's middle is exactly the "reads as a position" mistake the
  design forbids. (No hub that sends CoT runs the levels yet either.) The spotter's CoT remarks name
  the signal (`spotter: a signal getting louder (S3) is closing on house`).

**Expected false alarms** (THREAT-TACTICS.md §6):

- 12: **the unit walking or driving towards a transmitter** (a handheld unit, or a node carried
  towards a pilot standing still): nothing here can tell that from the drone approaching. A pilot
  walking towards a node with Wi-Fi goggles or a DJI Wi-Fi drone's phone. Two co-located nodes
  (the gateway and a relayed node on the same desk) give "2 nodes" for one view of the drone: the
  Warning for two nodes assumes they stand apart. A video transmitter that ramps its power slowly
  (not in one step). Mitigated, with tests: banking swings, a null that recovers, switching on at
  full strength (one weak first sample included), pit mode, a still pilot, one spike, a crossing.
- 13: a drone whose radio the hub does not hear (a protocol or band no node covers, a link out of
  range of the nodes but the drone within earshot), a crewed aircraft the hub does not track, lawn
  and garden machines, traffic tones above 140 Hz. Low hums (below 140 Hz) and aircraft-explained
  sounds never count.
- 14 without a position: a hobbyist's FPV arriving after another drone hovered; the spotter's own
  video getting louder as it drifts in slowly (only the spotter flying a beeline is excluded).

**What is not done**:

- **Nothing feeds the microphone yet.** The `acoustic` branch is not merged; on this branch the
  sound half (rule 13 and rotor sound in rule 12) is host-tested only. **What the acoustic merge
  must wire** (a few lines in the hub's sound loop, `firmware/hub-p4/src/ear.rs` on that branch):
  after `tracker.ingest_sound(&report, now)` returns `Some(id)`, find the contact in
  `tracker.sounds()` and call
  `tracker.hear_sound(&RotorSound { id: c.id.0, first_ms: c.first_seen_ms, last_ms: c.last_seen_ms,
  level_db: c.level_dbfs, low_hum: c.kind == SoundKind::LowHum, aircraft: c.kind ==
  SoundKind::Aircraft }, now)`. The acoustic branch's `SoundTable` lives in the tracker without
  path-history parameters (`impl Tracker<N, A>`); its `Tracker<N, A>` impls need the third
  parameter `P` added on merge. Do **not** use its `radio_drone_recent` for rule 13:
  `Tracker::last_radio_drone_ms` (this branch) is the rule's check (it adds control links and
  leaves out far-away tracks); the two can be unified when it merges. The acoustic evidence codes
  23 and 24 are untouched.
- The sound trend uses the detector's loudness only; the research's other cue (blade-pass pitch
  rising, throttle or Doppler) is not used.
- Not on the bench: every number above is from scripted levels, not real FPV or sound recordings.
  The first bench job is a walk-in and a fly-in with one FPV quad and two nodes, to see the real
  slope, swings and spikes.
- **Measured**: the P4 tracker is 416,464 bytes (16d: 393,904; the signal slots 22,560, 352 bytes
  each). The S3 tracker (`Tracker<64, 32>`) grew 32 bytes (87,760). The widest `signal_threat` line
  is 730 bytes (buffer 1,024). The P4 firmware compiles with no warnings; the main stack frame is
  68,128 bytes (16d: 68,112); the once-a-second housekeeping frame is 3,632 bytes (16d: 3,712), and
  rules 12-14 for signals run in their own 816-byte frame below it; the rise announcement for
  signals has its own 896-byte frame. Host tests: 1,569 passed, 5 ignored (16d: 1,547); tracker 324
  (16d: 304).
- **Hub wiring** (`firmware/hub-p4`): rises are announced with the drones' (`app.rs`,
  `announce_signal_levels`), the console prints the `signal_threat` lines (`report.rs`), the
  screen's Snooze covers signals (`site.rs`). Emitters, Wi-Fi drones and open links reach the
  tracker as before; control links through the control table, which now hands each packet's level
  to the tracker. No link protocol change; `crates/link` untouched.

## Slice 16e as built (2026-11-19)

Profiles, a host false-alarm matrix, the places in `/api/state`, and the phone page showing the
levels and drawing areas. Code: `crates/tracker/src/behaviour/profile.rs` (the table and the
floors), `places.rs` (`Places::profile`, `crowd_set`, `crowd()`), `site.rs` (the `profile` and
`crowd auto` lines, the change text), `json.rs` (`places_json`, `Signals::places`), the matrix in
`behaviour/tests_profiles.rs`; `web/index.html`; `firmware/hub-p4/src/site.rs` (the evidence tag);
`tools/rid-view` (`--mock-state`, fixtures in `web/mock/`). Site lines: [SITE.md](SITE.md),
"Profiles".

**How a profile is applied.** A `profile NAME` line picks one row of `profile::TUNINGS`. The rules
read their numbers from that row (`Places::tuning()`), and the level weights are applied in one
place, `behaviour::judge`, to every hit before the "two signs" pair is counted. The rules across
drones (16d) run on the unweighted levels, so a weight never hides a spotter from rule 14; a repeat
visitor's raise is worked out again on the weighted levels. There are no `if profile == ..` checks
in the rules.

| | default | home | facility | event | patrol |
|---|---|---|---|---|---|
| fast inbound (6): closing faster than | 25 m/s | 25 | 25 | 25 | **18** |
| fast inbound (6): arriving sooner than | 30 s | 30 | 30 | 30 | **45** |
| pilot "at the fence" (2): within | 30 m | 30 | **100** | 30 | 30 |
| loitering (7): a Warning when low after | 5 min | **3 min** | 5 | 5 | 5 |
| low hover (9): under | 15 m | **20 m** | 15 | 15 | 15 |
| low hover (9): for | 10 s | **6 s** | 10 | 10 | 10 |
| following (11): the unit travelled | 200 m | 200 | 200 | 200 | **120 m** |
| repeat visitor (17): days in two weeks | 3 | **2** | 3 | 3 | 3 |
| several at once (15): the count of drones | on | on | on | **off** | on |
| level weights (one level, never above Warning, never below Notice) | | | circling **+1** | pilot near, probing **-1** | |

How the design's intent maps to it: home weights repeat visitor and low hover up (and long low
loitering, the "casing" case); facility weights the pilot outside (a pilot standing near the
fence counts out to 100 m, so drone in plus pilot near is two signs) and circling up; event turns
several-at-once by count off and lowers the "pilot nearby" and "coming back" signs that every show
drone gives, approach rules untouched; patrol makes fast inbound and following sooner.

**Safety floors no profile can loosen**, held in code, not by convention: `Tuning::fast_mps()`
never returns more than 25 m/s and `soon_s()` never less than 30 s; `Tuning::weigh` never lowers
beeline (5), fast inbound (6), the striker and the spotter's mark (14, 21), identity change (19),
the lost-track hold (20), or several-at-once during jamming or GPS interference (the alarm half of
15); a Notice (a friend's lowered place rule) is never raised. A test checks every row
(`no_profile_loosens_a_safety_floor`), including a deliberately bad row the accessors must hold.
Friends are never lowered on 5, 6, 14 or 19 under any profile (that is not a profile matter).

**Precedence (decided here).** A `crowd` line is an explicit setting and **wins over the profile
whatever the order of the lines**: the profile is a preset of many numbers, a line naming one
setting is a deliberate choice about it, and switching profiles must not silently undo it.
`crowd auto` hands the count back to the profile. `crowd` is the only overlap: areas, the bubble,
friends and `visits` are never set by a profile. Profile lines among themselves: later wins.

**The false-alarm matrix** (`tests_profiles::false_alarm_matrix`; every flight under every profile,
one report a second through the whole tracker; the worst level seen, and for the hostile flights
the second it was first reached; run with `--nocapture` to print it). Ordinary flights must stay at
or below the level here, hostile ones must reach it; the test asserts each cell.

| | flight | place | default | home | facility | event | patrol |
|---|---|---|---|---|---|---|---|
| ordinary | estate photographer next door (a 60 m circle 20 m from our fence, pilot 80 m out) | house | Notice | Notice | Caution | Notice | Notice |
| ordinary | roof or solar inspector, 8 m up for a minute | house | Warning | Warning | Warning | Warning | Warning |
| ordinary | delivery drone passing straight, 400 m off | house | Notice | Notice | Notice | Notice | Notice |
| ordinary | park hobbyist 120 m from a handheld user, 7 min | bubble | Caution | Caution | Caution | Caution | Caution |
| ordinary | lawnmower survey over the house, surveyor 50 m out | house | Warning | Warning | Warning | Warning | Warning |
| ordinary | landed drone in the area | house | Warning | Warning | Warning | Warning | Warning |
| ordinary | two hobbyists in a park | bubble | Caution | Caution | Caution | Caution | Caution |
| ordinary | neighbour's drone over its own garden, 4 days | house | Notice | Notice | Warning | Notice | Notice |
| ordinary | club meet: 4 drones over the area, pilots inside | house | Warning | Warning | Warning | Caution | Warning |
| hostile | beeline at 12 m/s | house | Critical @59 s | Critical @59 s | Critical @59 s | Critical @59 s | Critical @44 s |
| hostile | FPV dive at 40 m/s | house | Critical @6 s | Critical @6 s | Critical @6 s | Critical @6 s | Critical @6 s |
| hostile | spotter then striker | house | Critical @350 s | Critical @350 s | Critical @350 s | Critical @350 s | Critical @350 s |
| hostile | recon circling over the house | house | Warning @95 s | Warning @95 s | Warning @95 s | Warning @95 s | Warning @95 s |
| hostile | payload drop | house | Warning @60 s | Warning @60 s | Warning @60 s | Warning @60 s | Warning @60 s |
| hostile | following a walker | bubble | Warning @144 s | Warning @144 s | Warning @144 s | Warning @144 s | Warning @87 s |
| hostile | camera drone at 20 m/s at a walker | bubble | Critical @32 s | Critical @32 s | Critical @32 s | Critical @32 s | Critical @6 s |
| hostile | window peek, 18 m for 10 s | house | Warning @66 s | Warning @57 s | Warning @66 s | Warning @66 s | Warning @66 s |

What it says, and what was tuned:

- **Tuned: the event profile no longer weights circling down.** The first table had circling -1
  under event (camera drones orbit a stage); recon circling over the area then fell to Caution.
  A hostile pattern dropping below Warning is worse than an orbiting camera drone at a Warning, so
  circling is untouched under event (a `friend` line covers your own camera drone).
- **Accepted, and the same under every profile** (no profile can soften them without also
  hiding the hostile case they look like): the roof inspector and the landed drone are low hover
  (the window-peeping case); the survey is drone-in-pilot-out plus passes heading for the middle
  (a contractor: a `friend` line). These are the 16b and 16d expected false alarms.
- **Facility is louder by design**: a pilot standing 80 m from the fence (the photographer, the
  neighbour) is "at the fence"; with the neighbour back on a third day it is a Warning.
- **Event** takes the club meet from Warning to Caution (no crowd count, pilots inside no longer a
  second sign) and leaves every hostile flight where it was.
- **Patrol** reaches Critical sooner (the walker rushed at 20 m/s: 6 s instead of 32 s; the beeline
  44 s instead of 59 s) and warns of following sooner (87 s instead of 144 s).
- **Home** sees the window peek 9 s sooner, as low hover while it is still at the window; every
  profile sees it later as a drop (down 22 m, a pause, up) when it climbs away.
- The two hobbyists stayed at Caution here: the second one's short passes never made a beeline
  long and straight enough. Another geometry can make the 16d "two friends in a park" Warning.

**`/api/state`** (`json::state_full`) gains `"places":{...}` when the hub gives its places
(`Signals::places`): `profile`, `crowd` (null: not counted by number), `visits`, `friends` (a
count; the serials are never written: they would tell anyone on the hotspot which drone the owner
flies and has lowered), `bubble_m`, and each area with an outline: `name`, `ceiling_m`, and
`circle {lat, lon, r_m}` or `poly [[lat, lon], ...]`. Widest object 3,978 bytes
(`json::MAX_PLACES_JSON` 4,608). Hubs that judge no levels (the S3 hub, rid-view) leave it out.
Schema in `crates/tracker/README.md`.

**The phone page** (`web/index.html`):

- Every drone with a level shows it as a symbol, the word and a colour (NOTICE dotted, CAUTION
  outlined, WARNING filled, CRITICAL filled with a double edge), with its first reason in the list
  and every reason in the drone sheet (worded as the hub's own text), and "friend", "held", "two
  signs at once" and "snoozed on the hub" when they apply. The list sorts by level after
  standing. A banner shows the most serious drone or signal at Warning or above whose alert the
  hub has not snoozed.
- Signals with a level (rules 12-13) are their own rows, "Getting louder (no position)", "judged
  from how loud a signal or rotor sound is, not from a position", and are never drawn on the radar
  (the radar note counts them).
- The radar draws each area's outline (circle or polygon) with its name and ceiling, and the bubble
  as a dashed ring; drones at Caution or above get a ring whose dash and weight differ by level,
  and the level in their label. Legend entries appear when the hub sends places.
- **Draw an area**: circle (tap the middle, then the edge, or type the radius; "Centre on the hub"
  and a typed radius need no pointer) or polygon (3 to 16 corners); a name and an optional ceiling;
  the exact `area ... circle|poly ...` and `area ... ceiling ...` lines to copy (7 decimals, as the
  hub writes them; a test parses lines copied from the page), with a note on SITE.TXT and the
  console's 120 characters. Points are kept as latitude and longitude from the radar's own
  reference (the hub's position, `own`), so zooming does not move them; the zoom is held while
  drawing. Without a hub position the page says so and does not offer drawing.
- **Profile picker** (Settings, "Threat profile"): the profile in use (from `places`), what each
  does, and the `profile NAME` line to copy.
- **Nothing is sent to the hub**: no new endpoint, no POST (a write surface on the hotspot that
  can switch protection off is the owner's decision). The page's only requests are still
  `/api/state`, `/api/flags` and `POST /api/own-pos`.
- A hub that judges no levels sends no `threat`, `signal_threats` or `places`: the page shows
  nothing new except "Draw an area" (it needs only the position) and "This hub does not report a
  profile". Checked with `web/mock/no-levels-state.json`.
- Light and dark checked, and a 375 px phone width with no sideways scrolling. Size: 64,376 ->
  90,983 bytes, **21,458 -> 29,847 gzipped** (the budget test allows 40 KB).
- Preview without hardware: `rid-view --http 127.0.0.1:8080 --mock-state
  web/mock/threat-state.json` (tools/rid-view/README.md). The fixture was written by the tracker
  from a scripted run (test coordinates only).

**Evidence.** A profile change, from a card load or the console, is a `Setting` event (code 4)
with data byte **6** ([EVIDENCE-FORMAT.md](EVIDENCE-FORMAT.md)) whose text names the profile and
what it does (and that a `crowd` line still wins, when one is in force). The load summary ends with
`, profile NAME`. No new evidence code.

**Not done, or blocked:**

- **Bench and field tuning.** Every number in the profile table is from scripted flights. There are
  no recorded flight paths in the repo. *Update 2026-11-19: the first bench pass flew five of the
  hostile and ordinary shapes under four profiles ("Bench pass" below); nothing needed retuning.
  Field tuning with a real drone is still to do.*
- **The P4 screen strip does not show the profile.** Its caveat lines are full at 52 columns
  ("ON: control IDs, control content, audio clips" is 45); a profile there needs a layout change,
  not a few characters. The console, the evidence log and the phone page show it.
- **Writing areas or the profile from the phone** (a POST): not built, owner decision.
- **The S3 web head running the behaviour layer**: the S3 tracker keeps no path history (`P` = 0)
  and the S3 web-head build does not link yet, so a phone on a P4 kit's hotspot sees no levels,
  places or signals yet; the page degrades cleanly. The P4 itself does not serve the page.
- The S3 hub's default build (`firmware/hub/s3`, `cargo fw`) compiles but **does not link, before
  or after this slice**: its RAM is over by 48,240 bytes at 6574429 and 48,248 after (`stack.x:
  cannot move location counter backwards`, from 0x3fce7370, now 0x3fce7378, to 0x3fcdb700): the 8
  bytes are the two new fields in its tracker's `Places`. The page adds 8.4 KB gzipped to its flash.

**Measured** (P4 firmware compiles, no warnings): `HubTracker` on the target 412,904 bytes
(6574429: 412,896; earlier slices quoted the host figure, which is unchanged at 416,464); main
stack frame 68,128 bytes and once-a-second housekeeping frame 3,632 bytes, both unchanged. S3
`Tracker<64, 32>` on the host 87,760 bytes, unchanged. Host tests: 1,582 passed, 5 ignored
(16c: 1,569); tracker 335 (16c: 324).

## Bench pass (2026-11-19)

The full record is in [BENCH.md](BENCH.md), "Bench results, 2026-11-19 (threat profiles)".

- **What ran.** The P4 hub judged `rid-beacon`'s fake drone (`TESTDRONE0000000001`, lowest power)
  heard over the air through a listener node and the gateway, against `area bench circle ... 150`
  round the beacon's centre. The beacon's new bench-only `scripted` build flies the paths of the
  host flights (`firmware/rid-beacon/src/script.rs`: beeline, fast inbound, a pass 300 m off, low
  hover, a drop, and its old circle), and `behaviour/tests_bench.rs` flies the same file through the
  tracker on the host. The hub's GPS was the bench emulator (`gpsloop`), standing at the centre.
- **Proven on the air** (made-up positions, real radio, the real hub): rules 1, 2, 5, 6, 8, 9, 10,
  friends lowering place rules, the lost-track hold, profiles `home`, `patrol` and `event` (their
  predicted differences, and the beeline still Critical under each), the bubble round a hub that
  stands still (emulated fix), the evidence records (code 28, verified, also from the card), and
  the visit history's file, wipe and `visits off` on a real card. Every level and reason matched
  the host prediction; every time was within 2.5 s of it (the beeline: Warning 11.3 s, Critical
  25.4 s; host 11 and 26).
- **Fixed on the way:** a drone lost mid-beeline kept its level for 30 s but not as "last seen
  inbound" (the path keeps one point a second, so the approach rules lapsed one judgement before
  the position went stale and the remembered approach was cleared): `756774b`, with a host test at
  uneven report times. And the P4 firmware did not compile after the merge of main: `67ec35e`.
- **Not tried:** the threat bar on the panel (nobody could see the screen; still simulated only),
  Snooze (screen only), a moving bubble and following, rules 3, 4, 7, 11 and 12 to 19, spotter then
  striker (one fake ID), a real drone.
