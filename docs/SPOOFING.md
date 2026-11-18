# Spoofing checks and identity continuity

Remote ID (ASTM F3411) is unauthenticated broadcast. Anyone can put a
convincing drone on the air with a cheap Wi-Fi or BLE chip, and DJI's
DroneID can be forged with an SDR. Rotorscope cannot prove that a broadcast
is genuine. It can notice when a broadcast disagrees with physics, with
itself, or with what else the unit hears. This document explains how the
hub's tracker does that (`crates/tracker/src/spoof.rs`) and how it keeps
following a drone that changes identity (`continuity.rs`).

Everything here is receive-only and runs on the hub. Nothing is looked up
online. The output only ever shows what the broadcast itself said, or a
number worked out from it. The DJI pilot account ID that some O4 records
carry is never taken in (see `dji.rs`).

## What attackers have

These are all public and cheap. They are the threat model.

- **ESP8266 and ESP32 sketches** that broadcast Remote ID Wi-Fi beacons for
  invented drones. Examples are jjshoots/RemoteIDSpoofer and its forks
  (dealyllama/RemoteIDSpoofer), which put many fake drones on the air from
  one board, and ANG13T/drone-swarmer, which adds GPS so the "ghost" drones
  sit at chosen coordinates. flyandi's SQUIDRID runs on most ESP32 boards.
  [1][2][3][4]
- **A PC or Raspberry Pi tool** from the Swiss Cyber-Defence Campus that
  crafts F3411 Wi-Fi beacon and BLE broadcasts. It was a ZHAW thesis
  project. [5]
- **DJI legacy Wi-Fi DroneID from an ESP8266**: the
  DJISDKUser/ESP8266_DJI_DroneID_Throwie project. Hackaday covered it in
  March 2024. [6]
- **DJI OcuSync DroneID**: the NDSS 2023 paper by Schiller et al. decoded it
  with cheap SDR hardware. It also showed the transmission can be disabled,
  and that the pilot position it reports can be spoofed with an
  off-the-shelf GPS-spoofing phone app. [7]
- **A buggy honest module**: CVE-2024-22520 lets an attacker make a
  Dronetag unit impersonate other Remote ID signals. [8]

All of these send whatever their author chooses. The checks below look
for what is hard to fake consistently: physics (signal strength against
distance, speed against positions), the radio layer (the transmitter
address, the message counter, timing), and agreement between independent
sources (Remote ID against DroneID, against the hub's own GPS time, and
against the RF scanner).

## How flags combine

Each check that fires leaves a **flag** on the track. A flag has a check
name, a confidence from 0 to 100, a `detail` number and a reason sentence.
Flags last for the life of the track, and their confidence only ever rises.
A track keeps at most 4 flags. If a fifth fires, it replaces the weakest one,
but only if it is stronger.

- **Weak** checks describe things that plenty of honest drones also do,
  such as using an ESP32 chip or a serial number in the wrong format.
  They are set below 30.
- **The score** treats each flag as independent evidence:
  `100 * (1 - Π(1 - c/100))`. If every flag is weak, the score is capped at
  29. So weak checks never raise an alarm on their own, however many there
  are.
- **Reporting.** `Tracker::take_spoof_flag` reports a flag once it reaches
  30 (`REPORT_MIN`). It reports it again each time the confidence rises by
  20 more. The hub logs each report as evidence event `SpoofSuspect` (code
  20). The data is the check code and the confidence. The text comes from
  `spoof::event_text`, for example:
  `track 7 SerialNumber 1ZZZ9FAKE00001: position jumped 2140 m, faster than any drone flies (confidence 90, score 97)`.

## The checks

The thresholds are deliberately loose. Each check is aimed at the cheap
spoofers above, not at a careful attacker (see "What still gets through").
For each check, "Code" is the evidence-log byte, and the units of `detail`
are given in brackets.

### Kinematics

| Code | Check | Fires when | Confidence |
|---|---|---|---|
| 1 | `position_jump` (m) | One transmitter jumps: two positions for the same ID, at most 120 s apart, further apart than 60 m + 2 x declared accuracy (default 30 m, max 150 m) + 100 m/s x (time between them + 2 s), **and** the track followed the jumped claim (nothing on the old path for 10 s while it continued), or reports keep jumping with nothing on the old path for 10 s. Since WP-10 a jump while the old path continues is not this flag: it is a rival claim, and a dispute when it persists (see "Impersonation and disputes") | each such jump one step stronger: 25, 50, 75, 90 |
| 2 | `motion_mismatch` (m/s, or minus degrees) | Over windows of 5 to 30 s, the distance actually covered, less 2 x accuracy + 10 m, gives a speed above 1.5 x the highest reported speed + 8 m/s. Or, above 5 m/s, the straight line between the window's ends points more than 100 degrees away from both the starting and the ending reported direction | Needs 2 bad windows and more than a third of all windows bad: 40; 4 bad windows: 60 |
| 3 | `altitude_mismatch` (m) | In the same windows, the change in geodetic altitude and the change in height differ by more than 40 m + 30 % of the height change | 2 windows 30, 4 windows 50 |
| 4 | `implausible_value` (kind) | In 3 frames: speed over 120 m/s (1); geodetic altitude outside -500 to 9000 m (2); height above **ground** below -50 m (3); geodetic and pressure altitude more than 800 m apart (4) | 35 |
| 5 | `operator_on_drone` | In 5 frames, the operator position is within 0.5 m of the drone while the drone moves faster than 2 m/s | 35 |
| 6 | `operator_far` (km) | In 3 frames, the operator is more than 30 km from the drone | 30 |
| 7 | `operator_jump` (m) | With the same location type, the operator position moves more than 100 m + 70 m/s x (time + 2 s) | 2 jumps 35, 4 jumps 55 |

Why these numbers:

- **Speed limits.** FAA Part 107 caps speed at 44 m/s. Racing quads and
  fixed-wing UAS can go faster, so the tracker allows 100 m/s, plus 2 s
  for a report that is stale when it arrives.
- **Jitter.** The jitter allowance covers GPS noise and declared accuracy.
  It is also why a single jump stays weak: a GPS receiver re-acquiring a
  fix can jump once.
- **Speed and direction.** Speed is checked one way only. A drone flying
  an orbit covers little straight-line distance, so "reports a high speed
  but barely moves" is normal. "Moves fast but reports zero" is not, and it
  is what a sketch that only moves its coordinates sends.
- **Direction.** Remote ID's direction is track over ground, not where the
  nose points, so a multirotor flying sideways still matches. For any turn
  under about 200 degrees, the straight line between two points lies
  between the starting and ending direction.
- **Altitude.** This check compares *changes*, so a fixed offset cancels
  out. That covers HAE against MSL (up to about 106 m of geoid height),
  takeoff elevation, and the HAE-or-MSL inconsistency SkySafe found in some
  DJI firmware [9]. The geodetic-against-pressure limit of 800 m allows for
  standard-atmosphere error of about ±500 m between 950 and 1050 hPa,
  plus the geoid.
- **Negative height.** Only "above ground" is checked. SkySafe reports
  Autel firmware sending negative height-above-takeoff values [9], so that
  is not flagged.
- **Operator.** An operator moving at 70 m/s covers a pilot in a vehicle.
  An operator *exactly* on a moving drone is what a sketch that copies one
  coordinate into both fields produces. A Takeoff-type operator location
  stays put, so it never matches a moving drone.

### Radio physics

| Code | Check | Fires when | Confidence |
|---|---|---|---|
| 8 | `rssi_too_strong` (dB over) | For 3 frames, RSSI exceeds `52 dB - FSPL_2.4GHz(d)`, with at least a quarter of all frames over the limit. `d` is the distance from the node: exact if `set_node_position` was called, otherwise the distance from the hub minus 300 m. Skipped when `d` is under 200 m or there is no own fix | Largest excess under 10 dB: 45; under 20 dB: 65; otherwise 85 |

The limit is a ceiling, not an estimate. It assumes 30 dBm EIRP (above
what drone Remote ID and DroneID transmitters use), a 10 dBi receive
antenna, and 12 dB more for two-ray constructive interference and RSSI
calibration error. It also uses 2.4 GHz free-space loss for every band.
5.8 GHz loses 7.7 dB more, so the limit is looser there.

At 5 km the limit works out to about -62 dBm, and at 1 km about -48 dBm.
A "drone 5 km away" heard at -35 dBm is 26 dB over. The check is one-way
only. A weak signal from a near drone is normal (walls, terrain, antenna
nulls), and multipath only ever subtracts or adds a few dB around free
space.

Comparing RSSI ordering across several nodes would need each node's RSSI
for the same frame. The tracker keeps only the latest node per slot, so
that is not done (see "Known gaps").

### Identity

| Code | Check | Fires when | Confidence |
|---|---|---|---|
| 9 | `shared_address` (tracks sharing) | After 3 conflicts on one transmitter address: tracks with different IDs, each heard within 5 s, using the same address | 2 sharers: 25, or 45 when 80 % of at least 5 conflicts arrive within 2 dB of each other on the same node; 3-4 sharers 55; 5 or more 80 |
| 10 | `address_alternation` (switches) | A track's address in one slot changes while the previous address was heard less than 2 s earlier | 4 switches 45, 10 switches 70 |
| 11 | `spoofer_swarm` (group size) | At least 4 new tracks born within 10 s on one node and slot, with first RSSI within 3 dB of each other | 4-5 tracks 20, 6 or more 25; 65 once two members claim distances from the unit that differ by 4x or more (12 dB of path loss), with the farther one at least 400 m away |
| 12 | `serial_format` (fault) | A Remote ID serial number fails ANSI/CTA-2063-A (see below) | 20 (weak) |
| 13 | `vendor_mismatch` (kind) | A DJI serial (maker code 1581) from a globally administered Wi-Fi address outside DJI's blocks (1, conf 25); from an Espressif block (2, conf 45); a valid non-DJI serial from a DJI block (3, conf 30); DJI legacy Wi-Fi DroneID from an Espressif address (4, conf 45) or another non-DJI global address (5, conf 20) | as listed |
| 14 | `drone_id_disagrees` (m) | A plaintext DroneID position lies more than 100 m + 2 x accuracy + 30 m/s x age from a Remote ID position under 3 s old on the same track, but inside the `position_jump` limit (further than that it is a rival claim and a dispute, not a flag). Needs at least 2 such readings, and more disagreements than agreements | 55; 5 or more 75 |

Why these work, and the honest cases they allow for:

- **Shared address.** A sketch that broadcasts several drones from one
  board cycles through them on one MAC. Each frame with a different ID
  "conflicts" with the track that last used the address. SkySafe found that
  Autel drones broadcast with **the same fixed MAC on every unit** [9], so
  two Autels flying together look exactly like this. That is why two
  sharers stay weak unless their frames arrive at identical strength, which
  two separate aircraft rarely do. Three or more sharers at once is a
  multi-drone transmitter.
- **Alternation.** A transmitter whose ID arrives from addresses that
  alternate within 2 s. A BLE address rotates every few minutes, which is
  one switch, not a pattern. Wi-Fi beacon and NAN share one MAC. Since
  WP-10 a clone of a real drone's ID that claims a position off the
  drone's path does not reach this check (its frames are a rival claim,
  not applied); only a clone inside the path limit still alternates here.
- **Swarm.** A spoofer swarm is born together and, coming from one
  antenna, at one strength. Drones in a light show also power up together.
  But they sit close together, so their claimed distances are similar and
  the check stays at 20-25. The geometry upgrade needs an own fix.
- **Vendor mismatch.** The ESP32 and ESP8266 that spoofing sketches run on
  use Espressif addresses by default. DJI aircraft broadcast from DJI's
  blocks (the list is in `crates/wifidrone`). Locally administered
  (randomised) addresses are never judged. An honest add-on module has its
  own serial, not a DJI one, so a DJI serial from another maker's chip is
  wrong. A third-party module configured to send a DJI serial would also
  trigger this, which is why plain "non-DJI address" is only 25.
- **DroneID disagrees.** DroneID needs an SDR to forge and Remote ID needs
  an ESP8266, so a cheap clone of a DJI drone's Remote ID sits somewhere the
  real drone's DroneID does not. DroneID records carry 16-byte serials, and
  the tracker matches them to Remote ID serials as `dji.rs` describes.

**ANSI/CTA-2063-A** [10][11][12]: a 4-character manufacturer code, one
length character, then the serial. The length character is `1`-`9` or
`A`-`F` for 1-15. Parrot's `1588E040452P3001126` has `E` = 14, and a
Watts example `1785C000000002000` has `C` = 12. The allowed characters are
digits and upper-case letters **except O and I**, so at most 20 characters
in all.

Fault codes: 1 too short, 2 bad maker code, 3 bad length character,
4 length mismatch, 5 forbidden character.

This check is weak on purpose. SkySafe's 30-day survey [9] found real Autel
drones sending 12-character factory serials, and serials containing the
letter O. It found 43 % of Parrot drones sending legacy French `PI040...`
identifiers.

### Timing and replay

| Code | Check | Fires when | Confidence |
|---|---|---|---|
| 15 | `clock_skew` (s) | In 5 frames, and in more frames than not, the Location timestamp (tenths of a second past the hour) is more than 30 s from the hub's UTC. Skipped until the hub calls `set_utc` | 30; 40 above 300 s |
| 16 | `time_inconsistent` (s) | Between two Location messages under 60 s apart, the timestamp advanced more than 10 s differently from the time between their arrivals | 3 times 45, 8 times 70 |
| 17 | `replay` (repeats) | An identical Location (position, altitudes, speeds, direction and timestamp) is heard again more than 3 s after it was first heard. The tracker keeps 3 recent fingerprints per track | 3 repeats 50, 10 repeats 75 |
| 18 | `frozen_counter` (% frozen) | Over at least 20 frames on one node and slot, 50 ms to 5 s apart, at least 90 % carry the same F3411 counter as the frame before. BLE 4 legacy is skipped, because it keeps one counter per message type | 20 (weak) |
| 19 | `incomplete_message_set` (kind) | After 15 s and 20 Location frames: no Basic ID at all (2), or no System message and no DJI source (1) | 20 (weak) |

Notes:

- **What Remote ID must send.** Standard Remote ID must broadcast its
  identity, position, altitude, velocity, control-station position and a
  time mark, at least once a second (14 CFR 89.305 and 89.310). A broadcast
  module must send the takeoff location (89.315 and 89.320) [13].
- **Repeats are normal at first.** A drone repeats the same Location
  between updates, and several nodes hear each frame, so an identical
  report within 3 s is normal.
- **Replays.** A replayed recording repeats old reports seconds later.
  Its timestamps also run backwards whenever the loop restarts, which is
  what `time_inconsistent` catches.
- **Clock skew needs a trusted clock.** It assumes the hub's own UTC is
  right. If the hub's GPS is being spoofed, that is reported separately (by
  the GNSS integrity work), and this check may then accuse honest drones.
- **What is not checked.** Missing timestamps are fine: some modules send
  "unknown". The per-message rate is not checked, because a node that hops
  channels hears only part of the traffic.

### Corroboration and other weak signs

| Code | Check | Fires when | Confidence |
|---|---|---|---|
| 20 | `no_dji_rf` | A DJI drone (DJI source or a 1581 serial) has been flying for 2 min, a raw-capture node (Fpv or DroneId role) has reported within 10 s, and no DJI video, O4 video, FPV air unit or controller emitter is in the table | 15 (weak) |
| 21 | `dev_board` (0 BLE, 1 Wi-Fi) | The transmitter address is in an Espressif block (355 blocks, sorted table) [14]. BLE random static addresses (top bits 11) are skipped | 15 (weak) |
| 22 | `auth_malformed` (count) | At least 2 malformed authentication pages: a page-0 header whose length the announced pages cannot hold (it does not decode), a page beyond page 0's last-page index, or a reserved auth type | 30 |
| 23 | `identity_changed` (previous track id) | The track continues one that was flying under another ID (see Continuity) | 55; 40 when both IDs are session IDs |

- **No DJI RF.** The RF node may simply have been scanning another band,
  so this check only adds weight to other evidence.
- **Dev board.** Honest ArduRemoteID add-on modules also run on ESP32.
- **Authentication.** Authentication pages are counted (`spoof.auth` in
  JSON) but cannot be verified, because the hub has no keys.

## Continuity: following a drone that changes identity

Tracks that share a Basic ID are already merged, so a drone that rotates
its MAC but keeps its serial stays one track. Continuity handles the case
where the **ID** changes. That happens legitimately with a new session ID
each flight. It happens suspiciously when a serial is swapped mid-flight.

When a track no more than 30 s old gets its first position, every other
track is a candidate. Up to 5 attempts are made. A candidate qualifies
only if all of these hold:

- it is not already continued;
- it was last heard no later than 1.5 s after the new track began, and
  began earlier;
- the gap between them is under 10 min;
- it shares no ID with the new track;
- its UA type and DJI product (if known) match;
- both have positions.

The candidate is then scored:

- **Mid-flight**, meaning the old track was airborne, under 60 s of gap,
  and had a position within 10 s of its end. The old position is
  dead-reckoned along its reported direction and speed (speed capped at
  30 m/s, time at 60 s). The new position must be within
  `min(50 m + 2 x accuracy + 20 m/s x elapsed, 2 km)`, and the geodetic
  altitudes within 30 m + 10 m/s x elapsed. Base score 50, plus up to 20
  for closeness.
- **Between flights.** The new track must start within 150 m of the old
  one's last position, home point or operator position. Base score 45.
- **Adjustments.** +15 if both operator positions are within 50 m, and the
  candidate is rejected if they are over 1 km apart. +10 if they were heard
  by shared nodes with latest RSSI within 10 dB. +5 if they used the same
  transports. +10 if they have the same DJI product.

The best candidate is linked if it scores at least 50, and if no other
candidate comes within 10 points of it (otherwise the link would be
ambiguous). The link shows in several places:

- **Track and JSON.** It is stored as `Track::continuity.previous`, and
  in JSON as `"cont":{"prev":id,"conf":n,"id_changed":bool}`.
- **CoT remarks.** It appears in the CoT remarks and in the `spoof::Summary`
  text, for example
  `probably the same drone as track 12, new identity mid-flight`.
- **Mid-flight changes.** If the link is mid-flight and both tracks had
  IDs, `id_changed` is true and the track gets the `identity_changed` flag.
  A change between flights is not flagged: per-flight session IDs and MAC
  randomisation are normal and legal.
- **Undoing.** If the old track is heard again more than 1.5 s after the
  new one began, the link is removed (it was two drones after all), along
  with the flag.

Memory is bounded. The candidates are only the tracks already in the
table, so once a track is removed (after 10 min by default), nothing can be
linked to it.

## RAM

Measured with `cargo test -p tracker spoof_memory -- --nocapture` on a
64-bit host:

| Item | Bytes |
|---|---|
| `SpoofState` per track | 128 (4 flags of 8 B, a kinematics window of 20 B, 3 replay fingerprints of 24 B, about 35 counters) |
| `Continuity` per track | 20 |
| `Track` | 832 (was 680) |
| `Tracker<64, 0>` | 57 232 (was 47 392; +9.8 KB) |
| Tracker-wide (`set_utc`, 8 node positions) | about 80 |
| Espressif block table | 1.4 KB of flash |

On the 32-bit hubs the per-track cost is slightly lower, because `heapless`
vector lengths are 4 bytes there instead of 8. A 32-track C5 hub pays about
4.8 KB. `json::MAX_LINE` grows from 2432 to 2816 bytes; the worst case is
now 2702 bytes, with 4 flags and a continuity link.

## Hub API

```rust
tracker.set_utc(utc_ms, now_ms);                 // whenever GPS time is known
tracker.set_node_position(node, lat, lon);       // optional, nodes 0-7
while let Some(ev) = tracker.take_spoof_flag(now_ms) {
    let text = tracker::spoof::event_text(&ev, tracker.get(ev.track));
    evidence.event(EventCode::SpoofSuspect, text.as_bytes(), &ev.data()); // [check code, confidence]
}
while let Some(d) = tracker.take_dispute(now_ms) {  // two transmitters, one ID (WP-10)
    let text = tracker::claims::event_text(&d, tracker.get(d.track));
    evidence.event(EventCode::IdDisputed, text.as_bytes(), &d.data()); // [track id u32, what, source]
}
track.claims.dispute(now_ms); track.claims.rival(now_ms);  // for screens
track.spoof.score(); track.spoof.flags();         // for screens
tracker::spoof::Summary(&track)                  // one-line text (Display)
track.continuity.previous / .identity_changed
```

## Floods: standing before trust

The checks above judge one drone at a time. A cheaper attack ignores them:
send hundreds of new, well-formed drones a second, each with a random
serial and address, each claiming to sit on the hub. One ESP32 does it.
Published work shows the same attack blinding a commercial receiver. Before
WP-09 every fake got a track, a "new drone" alert, a row at the top of the
screen (they claimed 0 m), a TAK marker and a rule-engine entry, and a full
table threw out the least recently heard drone, which between two of its
own frames was usually the real one.

Nothing here can tell a careful fake from a drone. What the tables can do
is make a new identity earn its standing before it can cost anything that
already has some. `crates/tracker/src/probation.rs` holds the rule and its
numbers.

### Drone tracks

| Rule | Value | Why |
|---|---|---|
| A track is **confirmed** when heard again at least this long after it was first heard (2 frames) | 1 s (`CONFIRM_SPAN_MS`) | Remote ID sends a Location at least once a second. A single forged frame never qualifies; the cheap flood sends exactly one per identity |
| Announce (buzzer, banner, page alert), TAK, the drone-near-aircraft check | confirmed tracks only | |
| Worst case from first frame to the alert, a drone sending at least once a second | 2 s (`WORST_CASE_ANNOUNCE_MS`) | 1 s plus one frame interval; most drones send several frames a second and make it in about 1 s. Before WP-09 a drone with an ID was announced on its first frame |
| An unconfirmed track not heard for this long is removed | 30 s (`UNCONFIRMED_REMOVE_MS`) | Confirmed ones keep 60 s stale / 10 min removal |
| A full table makes room from | a stale track; else the weakest unconfirmed one (fewest frames, then heard longest ago); else a confirmed one not heard for 15 s (`QUIET_EVICT_MS`) | Never a confirmed track heard in the last 15 s. Real drones are heard every second, so a flood never displaces one |
| When the table is full, a new identity first goes into a sightings table | 4 per track slot, 12 B each (256 for 64 tracks) | It gets a track only when heard again at least 0.5 s later (`SIGHTING_GAP_MS`). The sightings table forgets the least-heard, oldest entry first, so a real drone's first frame survives a single-frame flood of up to 256 new IDs a second for the second it needs |
| Admission into a busy table | a token bucket of 4, refilled 4 a second | With `k` tokens left an identity needs `2 + (4 - k)` sightings: an attacker who repeats each fake once spends the first tokens, after which a real drone that keeps transmitting outranks fakes that are not kept alive as long |
| Tracks made, worst case | 64 per 30 s (free slots, refilled only as unconfirmed tracks expire) + 4 a second | |

A track's id and history (first heard, frame counts, spoof flags,
continuity, FAA timers) are kept for as long as it is in the table, which a
confirmed, transmitting drone never leaves during a flood.

The screen and the phone page list confirmed drones first, then unconfirmed
ones, each group nearest first. Unconfirmed rows are faint, numbered `?` on
the screen and marked "unconfirmed" on the page; the big drone count leaves
them out ("+58 unconf."). JSON track lines carry `"confirmed"`. CoT is sent
for confirmed tracks only (`FormatError::Unconfirmed`). The SD CSV is
unchanged and logs every track.

### The flood alarm

New identities (a frame whose drone matches no track and no remembered
sighting) are counted per second. At **20 a second averaged over 5 s**
(`FLOOD_NEW_PER_S`, `FLOOD_WINDOW_S`) the tracker raises a hub threat,
`id_flood` ("Flood of new drone IDs"), refreshed once a second with the
rate (`detail` low 16 bits; JSON `table` and `per_s`), and ended 10 s after
the rate falls. It goes through the ordinary threat path: one evidence
`Threat` event with a sentence (`probation::describe`), the console, the
threat count on the screen. It can only be snoozed (5 min), never muted, and
a snooze does not clear the bar (WP-11b). The screen's bar shows `ID FLOOD`;
`/api/state` carries `"flood": {new_per_s, refused, since_ms}` and the page
shows a banner.

The same alarm covers ADS-B (`table` `aircraft`: unproven aircraft still
being reported thrown out for newer ones, 5 a second) and control links
(`table` `control`: unproven links replaced, 2 a second).

**False alarms.** A race of 40 drones, all switched on within seconds,
makes 40 new IDs once and never reaches 20 a second for 5 s (tested). A
light show that powers up hundreds of drones at once can: a show whose
drones all fit the table (64) or the sightings table behind it (about 320
in all) raises it at most briefly while they switch on; a bigger one keeps
it raised, which is also true (the hub can no longer follow every drone).
A busy airport does not raise the aircraft alarm: real aircraft are heard
every second and are never replaced (tested with 50 against a 32-entry
table).

### The slow rules (2026-11-18)

The 20-a-second line has a blind side. A cheap Wi-Fi or Bluetooth flooder
that paces itself at, say, 3 new IDs a second stays under node-wifi's
new-source budget (8 a second, so nothing is shed and the node-shed alarm
below never sees it) and under the fast line: 180 fake drones a minute and
no alarm. If it sends each fake twice, a second apart, they are even
confirmed and announced. A DJI DroneID flood cannot reach the fast line at
all: one node-sdr decodes at most about 2.8 valid bursts a second. So the
same alarm has two slower rules, evaluated once a second over the same
count of new drone identities (`probation::SlowWatch`, 96 bytes):

| Rule (`flood_rule`) | Raised when | Says |
|---|---|---|
| `rate` (as before) | 20 new a second averaged over 5 s (`FLOOD_NEW_PER_S`) | "25 new a second over 5 s (limit 20)" |
| `minute` | **60 new drone identities within the last 60 whole seconds** (`FLOOD_NEW_PER_MIN`, `FLOOD_MINUTE_S`): one a second averaged over a minute | "64 new drone IDs in the last minute (limit 60)" |
| `active` | **32 drone identities heard within the last 10 s at once**, confirmed or not (`FLOOD_ACTIVE_IDS`, `ACTIVE_HEARD_MS`) | "34 drone IDs active at once (limit 32)" |

The fast rule, when it holds, is the one reported; else the minute rule;
else the population rule. All three are the one hub-wide `id_flood` entry
of table `drones` (so one snooze covers it, as before), and the threat's
`detail` names the rule in bits 16-23 (0 = rate, the only value before, so
old lines read as they did; 1 = minute, 2 = active) with the rule's number
in bits 0-15. The console and the evidence `Threat` event carry the
sentence of whichever rule raised it first (`probation::describe`, for
example "Flood of new drone IDs: 64 new drone IDs in the last minute (limit
60); unconfirmed ones are kept out of alerts"); the JSON threat line adds
`flood_rule`, `ids` (for the slow rules, in place of `per_s`) and `says`;
`/api/state`'s `"flood"` adds `rule`, `minute` and `active`, and the phone
banner reads the rule's number in words. The screen shows `ID FLOOD` as for
the fast rule. Tracks are shown exactly as before: the alarm blocks nothing.
It ends 10 s after its rule stops holding, like the other flood alarms.

**What counts.** A *new identity* is the same thing the fast rule counts: a
frame whose drone matches no track (by any of its IDs, then its address)
and no remembered sighting. A drone that changes its address, or is heard
on Wi-Fi and Bluetooth at once, matches its own track by ID and is counted
once; frames never count, identities do. The population rule counts
**tracks** heard in the last 10 s, and a track is one drone however many
addresses or media it uses. (The one place an address still makes an
identity is a frame without any ID, a Bluetooth 4 advert carrying one
message, until its ID arrives on the same address and the two are merged.)

**Why these lines, and why 10 s for "at once".** A real site's drones are
each new once, so the minute rule measures arrivals: 60 drones switching on
within a minute is a light show powering up, a race meet's whole field
on the grid at once, or a flood. Remote ID sends at least once a second, so
a transmitting drone is heard about ten times in 10 s; one that has landed
or left stops counting within 10 s, and an alarm raised by kept-alive fakes
ends about 20 s after they stop (10 s to stop counting, 10 s hold). 32 at
once is half the P4's table, the whole table of a C5 hub, and the number
PLAN.md D7 takes as the most a site needs followed.

**When a real site raises it.** A drone show (dozens to hundreds of
broadcasting drones within radio range) raises the population rule as long
as it flies, and the minute rule while it powers up. A large meet where
more than 32 broadcasting drones are powered on at once (pilots' drones
idling in the pits count: standard Remote ID transmits from power-up) raises
the population rule; the host test of a 40-drone race shows it, with the
fast and minute rules quiet. A typical race (4 to 8 in the air) or a busy
park does not: the host test of an hour with a new drone every 4 minutes,
each flying 20 minutes on two media with its address changing every 30 s
(about 6 at once), raises nothing, and 30 drones switched on within 3 s
raise nothing. In every case the alarm is information for the operator:
"more drone IDs than a real sky usually holds; unconfirmed ones are
probably fake". Nothing is refused or hidden because of it.

**Timings (host tests).** 3 new one-frame IDs a second: raised 21 s after
the start (the 60th new ID; the old rules never raise it), ended 51 s after
the flood stopped (the minute's count falls under 60 about 40 s later, then
the 10 s hold). The same with each fake repeated a second later (confirmed
fakes): raised at 12 s, by the population rule (30 fakes heard in the last
10 s plus the real drone). One new fake every 2 s, every fake kept alive
(sent again every 5 s): under both rate lines, raised at 62 s when 31 fakes
and the real drone are active at once, ended 18 s after the fakes stopped.

**On the bench (2026-11-18, P4 + one ESP-NOW node-wifi `floodtest` +
gateway).** Quiet 3 minutes with the test drone: no alarm. `flood rid 3`
(3 new IDs a second, nothing shed by the node): raised 12 s after the start,
first by the population rule ("32 drone IDs active at once": 30 one-frame
fakes heard in the last 10 s plus the test drone's two tracks, Remote ID and
DroneID), then read by the minute rule (124 new IDs in the last minute);
ended 50 s after the flood stopped. The test drone kept its two confirmed
tracks throughout. `flood rid 200`: the hub-wide drone alarm within 2 s
(read as the minute rule) and the node-shed alarm 5 s after the start, as two entries (`table` `drones` and
`node`); the node-shed one ended 15 s after the flood stopped, the minute one
61 s after. `flood crowd 40` (40 fakes kept alive through one node) did not
reach the population line: the node's 32-slot source table and its 8 a
second new-source budget let about 28 of them through, so the hub held 30
drone tracks. One Wi-Fi node cannot show the hub more than about 30
kept-alive sources; the population rule is the catch-all for floods spread
over several nodes or media, and is host-tested.

**Cost.** One byte per second for 64 seconds plus the latest count: 96
bytes in the tracker (75 848 bytes for `Tracker<64, 32>` in all), one pass
over 60 bytes and one over the drone table once a second.

### One node shedding a flood

The hub-wide fast line above cannot be reached through one Wi-Fi node (the
minute rule can: 8 new IDs a second is 60 in under 8 s, so a shedding node
now raises both alarms). Under a
frame flood node-wifi lets at most **8 Remote ID frames a second from new
sources** through (`wifiwatch::shed`, docs/BENCH.md "Frame flood") and sheds
the rest, so a fake-drone flood heard by one node reaches the hub as a steady
trickle of 8 new, never-confirmed drones a second: no alarm, and the
node's shedding visible only in its `Stats.dropped` total. The per-kind shed
counts stay on the node's USB line; no link format changed.

So the hub also watches each node (`probation::ShedWatch`) with what
already crosses the link: the node's `dropped` total, once a second, and the
new drone IDs it delivers, with how many of them confirm. A node is
**shedding a flood** when over 5 s (`SHED_WINDOW_S`) both hold:

| Condition | Value | Why |
|---|---|---|
| It discarded frames (`Stats.dropped` rising) | at least 8 a second (`SHED_DROP_PER_S`) | one whole new-source budget thrown away |
| New drone IDs it delivered that did not confirm | at least 4 a second (`SHED_NEW_PER_S`) | half of the node's 8 a second; counted 3 s back (`SHED_LAG_S`) so each ID had the 2 s a 1 Hz drone needs to confirm |

The hub then raises `id_flood` with `table` **`node`** ("Node shedding a
flood of drone IDs"): `by_hub`, `node` set to the shedding node, `per_s` the
unconfirmed new IDs a second and `dropped_per_s` the frames discarded a
second (`detail`: table in bits 24-31, unconfirmed a second in 16-23, drops
a second in 0-15). It is the same hub-only kind (`link::origin`: a node
cannot send `id_flood`; it can only make the hub raise it by actually
flooding), refreshed once a second, ended 10 s after either condition stops,
announced through the ordinary threat path (evidence `Threat` event with
`probation::describe`'s sentence, console, screen count). It is
safety-critical: snooze only (5 min), never muted, never from the site file.
A snooze names the node (`key_of(IdFlood)` is table and node), so silencing
node 2's alarm does not silence node 3's. The phone's `"flood"` banner stays
the hub-wide one.

A flood delivered at the node's budget is raised **within 10 s**
(`SHED_RAISE_MS`; 7 s in the host test at 200 a second). The checks run on the
P4 (the link reader keeps each node's latest `Stats` apart from the frame
queue, so a full queue cannot cost the report, and hands it to the tracker
each loop) and on the S3 web head (it gets every node's `Stats` forwarded).

**What does not raise it** (host tests in `crates/tracker/src/tests_flood.rs`):

- A quiet site: no drops.
- A node booting: its first `Stats` is a baseline, not a rate (whatever it
  lost while starting is not counted); a reboot (uptime or total going back)
  or a gap of more than 5 s between reports starts a new baseline. The
  drones it delivers all at once confirm.
- A busy Wi-Fi venue: the receive pool overflowing steadily (tested at 40
  and at 5 a second) with a stray one-frame Remote ID now and then.
- The 40-drone race through a node that also drops (venue overflow plus
  first frames over budget as the drones switch on): every new ID confirms.

**The hub-wide line is not lowered.** It counts every new ID, confirmed or
not, so lowering it to what one node can deliver (8) would make a race
start raise it; the per-node check is the scaled-down version, and it asks
whether the IDs confirm.

**Limits.** A flood at or just above the node's budget (up to about 15 a
second through one node: 8 delivered, under 8 shed) does not raise it (the
drop line, tested), and neither does one under 4 new IDs a second. Fakes each
sent twice, a second apart, confirm and do not count as unconfirmed (but
then they are the "confirmed tier" case below). A flood of other kinds (fake
drone networks, DJI elements) makes the node discard frames without new
drone IDs reaching the tracker's drone table, so it is not this alarm; those
kinds have their own budgets and alarms on the node. The table follows 16
nodes (about 60 bytes each).

### The other tables

| Table | Before | Now |
|---|---|---|
| Threats (16) | full: drop an ended one, else the oldest; floods keyed per target | flood kinds (deauth, disassociation, beacon, login and probe floods, BLE spam, channel switch) are one entry per kind and **reporting node** (latest target shown), unless a report matches a mute rule; floods hold at most 10 entries; an active jamming, Blinded, AreaGnss or ControlContested threat is never replaced by another kind, and the last live jammer never, so `partly_blind` cannot go false through eviction |
| ADS-B in the tracker (32) and in the 1090 decoder (32) | keep by distance; the decoder refused new addresses while 32 were heard within 5 s | an aircraft is **established** after 3 plausible position updates over 2 s (decoder: or 10 verified messages over 5 s). Unproven ones hold at most half the table, established ones at most three quarters; a newcomer replaces only a stale or quiet (1.5 s) unproven one; claimed distance ranks only established aircraft. The drone-near-aircraft warning pairs only confirmed drones with established aircraft |
| Control links (was 4, now 8) | least recently heard goes | established after 3 reports over 2 s; a new link replaces a stale one, else the unproven one with fewest reports, else an established one quiet for 15 s |
| FAA rule engine (P4: was 32 entries in internal RAM) | `entry()` for every track every second, least recently touched evicted | entries only for tracks with fresh data, confirmed drones and established aircraft first; a full engine replaces only an entry of lower standing or one idle for 5 s; the P4 sizes it for 64 drones + 32 aircraft (~84 KB, in PSRAM at 5 MB), so it never replaces anything |
| Distances | Vincenty, up to 200 iterations, for every shown drone and operator four times a second | points over 10 degrees apart use the sphere; Vincenty stops at 20 iterations (under 10 in practice); the screen's drone-to-operator distance uses the sphere |

### What an attacker can still do

- **Delay new real drones.** Single-frame fakes faster than about 256 new
  IDs a second (4 x the track capacity) push a real drone's first sighting
  out before its second frame, so its admission becomes a matter of luck.
  An attacker who repeats every fake several times, kept alive over
  seconds, competes for the 4 admissions a second and can keep a new real
  drone waiting for as long as that lasts. Confirmed drones are unaffected.
- **Fill the confirmed tier.** Fakes that transmit like real drones (every
  ID at least once a second, for as long as the attack lasts) become
  confirmed, are announced, reach TAK and fill the table. Once 64 are
  confirmed and transmitting, a new real drone gets no track. This costs
  the attacker one stream per fake, forever; the flood alarm fires while
  they are being created.
- **Forged established aircraft.** A forger that sends three plausible
  positions over two seconds per address gets established entries, and
  nearer ones push out the farthest real ones (as nearer real aircraft
  always did).
- **Muted floods.** A mute rule for one victim keeps that victim's floods
  apart; the screen's mute of a node's aggregate entry silences that node's
  only, for two hours (WP-11b: there is no rule for the whole kind any
  more, and the ID flood alarm can only be snoozed, docs/SITE.md).
- **Provoke a mute.** Raising nuisance alarms to get the operator to press
  Mute now costs the attacker exactly those alarms: the press takes only
  alarms already on the screen, never a jamming, blinded, area-GPS, ID-flood
  or forged-traffic one, and the mute ends after two hours.
- **Stay under the node-shed lines.** Through one Wi-Fi node, a flood of
  up to about 15 new IDs a second (or one paced at under 4 a second) raises
  no node-shed alarm; the probation rules above still keep every fake
  unconfirmed and out of alerts. Since the slow rules, any such flood of one
  new ID a second or more raises the hub-wide alarm within a minute.
- **Stay under the slow lines.** One-frame fakes slower than one a second
  are each removed 30 s after their frame, so fewer than 30 are ever in the
  table and neither slow rule is reached: a trickle of under 60 unconfirmed
  fakes a minute raises no alarm. Kept alive, they reach the population line
  at 32. Under that they are a few faint, unconfirmed rows (or, sent like
  real drones, the "confirmed tier" case above).
- **Slow forged-aircraft and control-link floods.** The aircraft (5
  replaced a second) and control-link (2 a second) flood lines have the
  same blind side as the drone one had: a forger pacing itself under them
  raises nothing. It also gains nothing: established aircraft and links are
  never replaced by unproven ones, and the unproven ones are never paired
  into the drone-near-aircraft warning. No slow rule was added there.
- **Unconfirmed clutter.** The list and the page still show the unconfirmed
  fakes, faint and last. They are never announced, never sent to TAK, and
  never ranked above a real drone.

## Impersonation and disputes

Floods make new identities. Impersonation reuses a real one: anyone in
range can transmit well-formed frames carrying a real drone's ID (serials
are broadcast in clear) with any position, operator position, Self-ID or
operator ID. Nothing in Remote ID or DroneID is authenticated: CRCs are
public, and a decrypted DJI O4 record proves only that its sender had the
receiver's public key or a recording. Before WP-10 the tracker treated each
frame as an instruction to the track its ID named:

- one frame with a victim's serial moved the victim's position, operator
  position, Self-ID and operator ID, and the real drone's next frame moved
  it back; two such jumps gave the **real drone** a `position_jump` flag at
  50 (three, 75), for the life of the track, and a "SPOOF?" evidence event
  against its serial. A legacy DJI Wi-Fi element or a plaintext OcuSync
  record with the victim's serial did the same while Remote ID was quiet
  for 3 s;
- one frame naming two drones' IDs (a serial and another drone's session
  ID) merged them for good; one frame with A's serial from B's address
  folded B's address-only track into A; one frame with two serials renamed
  A;
- a decrypted O4 record merged its session into the serial's track and
  moved it; a repeated CRYP cost a curve operation every time;
- an undecodable frame from a drone's address kept a departed drone
  "active";
- CoT markers (`drone-<UAS ID>`) moved with every forged frame, and nothing
  said that any of it was a claim.

`crates/tracker/src/claims.rs` holds the rule and its numbers. A frame is
now evidence about a transmitter, not a command to the track.

### The rule

| Rule | Value | Why |
|---|---|---|
| A track has an **established path** when it is confirmed and has a position under 120 s old | `claims::ESTABLISHED_MAX_AGE_MS` | Before confirmation (1 s, `probation`) there is nothing to protect yet |
| A report fits the path when it is within the `position_jump` limit of the shown position | 60 m + 2 x worse declared accuracy (30 m default, 150 m max) + 100 m/s x (time between + 2 s) | The existing check's maths, unchanged: any honest drone fits |
| A report that does not fit is a **rival claim**: not applied, nor anything else in its frame (operator position, texts, IDs), and its address does not join the track | one rival per track: position, first and last heard, reports along its path, source, address | A frame with no position from the rival's own address is the rival's too |
| **Disputed** when the rival forms its own path while the original continues | 3 rival reports over at least 2 s, each within the path limit of the last, and 4 switches between the two paths | Text fields that differed in the rival's frames join the dispute. A glitch that recovers switches twice |
| The dispute ends | 15 s after the rival was last heard (`RIVAL_TTL_MS`) | `ever` stays set in JSON |
| The track **follows** the rival | when nothing has been heard on the old path for 10 s and the rival has its own path (`FOLLOW_AFTER_MS`) | See below |
| A changed Self-ID, operator ID, session ID (or a third ID type) | adopted after 3 frames over 5 s with the old value not heard between (`ADOPT_MIN_FRAMES`, `ADOPT_MS`); old and new alternating 3 times is a dispute over that field | One shared pending slot per track: two fields changing at once are adopted one after the other |
| DJI flight purpose (Wi-Fi DroneID, O4 INFP-11) | fills an empty Self-ID, never replaces one | Remote ID's and DJI's texts are different fields of the same drone |
| Two tracks with their own IDs merge | after 3 frames over 2 s say they are one drone (both IDs, or one's ID from the other's address), positions within 60 m + 2 x accuracy + 30 m/s x (time between + 1 s), neither disputed (`MERGE_MIN_FRAMES`, `MERGE_SPAN_MS`) | A frame naming two tracks is applied to the one whose path it fits, and an ID another track holds is never added to it. 4 pending merges, tracker-wide |
| An ID-less track (Bluetooth 4: one message per advert) joins an ID's track | on the first frame carrying that ID from its address, if its position fits | Address rotation and Location-before-ID keep working |
| Several different IDs of one type in one frame | counted (`claims.contradictory`, `Tracker::contradictory_frames`), applied to nothing | F3411 allows one per type; no honest drone sends two |
| A frame with no position, by address only | goes to the track on that address whose path its position fits when there are several | One forged frame from B's address does not pull B's later frames onto A |
| An undecodable frame | counts a decode error, refreshes nothing | It can come from anyone |

**Why 10 s to follow.** A real drone's GPS can glitch (re-acquisition,
multipath) for a second or two; a real drone can also stop transmitting
(landed, out of range, switched off) while a forger carries on. Following
at once is the old defect; never following leaves a frozen marker over a
drone that has really moved (or a teleporting single transmitter never
shown where it is). Remote ID repeats its position at least once a second,
so 10 s is ten missed reports on the old path while the other continues:
longer than the glitches the tests use, short enough that an operator does
not watch a stale marker for long. Following during a dispute is said
(`followed`: "now showing the claim that continued"); following without
one is a single transmitter that really jumped, and only that earns
`position_jump`. Reports that keep jumping with nothing on the old path for
10 s also step it up.

### DJI sources

Plaintext OcuSync DroneID, legacy Wi-Fi DroneID, decrypted O4 and Remote ID
are separate claims about one serial. A DJI position is judged against the
track's path like any report: one that fits agrees (Remote ID's is still
preferred while under 3 s old, and DJI's is shown when Remote ID is quiet);
one that does not is a rival claim with its source (`DroneId`, `DjiWifi`,
`DroneIdO4`), never applied, a dispute if both continue. A DJI record's
serial finds the most established track holding it and never merges
tracks. A decrypted O4 record whose serial another track holds is judged
against that track: off its path, a rival claim on it (source `DroneIdO4`);
on it, evidence for the merge rule above (3 records over 2 s with
compatible positions), and the session keeps its own track until then.
`drone_id_disagrees` still covers DroneID within the path limit but more
than 100 m off.

"Decrypted" is shown as "decrypted, not authenticated" everywhere.
`djicrypt` refuses what it can (tested):

| Rule | Value |
|---|---|
| A CRYP whose key ID is already filed | decrypts with the stored key: no curve operation (a repeat cost one each before) |
| New unwraps (curve operations) | a bucket of 4, refilled 2 a second (`UNWRAP_BURST`, `UNWRAP_PER_S`); over it, `Error::RateLimited` |
| INFP-10 sequence number or GPS time not newer than the session's last accepted one, or an altered copy of the last one (same sequence, other content) | `Error::Replay`, counted (`stats.replayed`); an identical repeat (a second node) is fine |
| INFP-10 further ahead than the time since the last allows (sequence: 50 a second; GPS time: hub time + 10 s) | `Error::TooFarAhead`, counted, and the session does not move on, so a forged "future" record cannot lock the real ones out |

### What the operator sees

Tensions, and what was chosen (the owner delegated these):

- **Every track is a claim, but a label on every row is noise.** The list
  says it once ("IDs and positions are what drones broadcast; none is
  verified."); each detail page and screen detail has one quiet line
  ("broadcast, not verified"). JSON carries nothing extra for a quiet track
  (`"claims":null`). CoT, which leaves the hub and is read without context,
  says `Claim: unverified broadcast` in every remark.
- **A dispute must be visible without accusing anyone.** The word is
  DISPUTED, never "spoof" or "fake": two transmitters claim one ID, and
  either could be the real drone. Screen list: "!" before the ID. Radar
  readout and detail page: a DISPUTED box with where the other claim is;
  the radar draws the other claim as a hollow "?" diamond joined to the
  shown position. Phone page: a dashed DISPUTED tag (not the alert colour)
  and a section with both positions, how far apart, the other's source and
  what else differs. Evidence: event 25 `id_disputed`, text `track 7
  SerialNumber 1ZZZ9FAKE00001: two transmitters claim this ID; another
  Remote ID claim 2163 m from the shown one. Neither is verified.` No
  buzzer: a dispute does not mean a new drone.
- **Which position to show.** The established one (heard first), with the
  other beside it. Showing both as equals would let one forged frame put a
  second marker on every map; showing only the newer is the old defect.
- **CoT uid.** Kept as `drone-<UAS ID>` (other sensors and tools key the
  same drone on it). Because the track's position no longer follows forged
  frames, a forger moves nothing on TAK; the dispute and the other
  claimed position are in the remarks.
- **Authentication.** The hub verifies no Remote ID signature. Where an
  auth message was heard, the most anything says is "auth message present,
  not checked" (page, screen detail, CoT).

### Cost

| Item | Before | After |
|---|---|---|
| `Claims` per track | | 48 B: the rival claim (position, first and last heard, folded address, reports, source: 21 B), the dispute's times (6 B), a pending text/ID change (16 B), flags and counters |
| `Track` | 848 B | 896 B |
| `Tracker<64,32>` (P4, and the S3 web head's tracker) | 70 208 B | 73 368 B (+3 160: 64 x 48, a 4-entry merge table, a counter) |
| `Tracker<32,0>` (C5) | 37 436 B (computed: 32 x 48 + 92 less) | 39 064 B |
| `json::MAX_LINE` | 2816 | 3008 (worst case 2897; `"claims":null` adds 14 B to every line, a full object up to 195) |

Per frame: one equirectangular distance against the established fix per
track the frame names (two while a rival is alive), a pass over the table
for IDs held elsewhere and for another track on the address (both O(table),
a compare each), an FNV hash of each text field, and a 4-entry merge
table. A known CRYP now costs one AES-CTR pass instead of a scalar
multiplication.

### What an attacker can still do

- **Forge first and be the original.** Whoever establishes a track first
  (before it is confirmed, or before the real drone comes into range) is
  the original; the real drone then appears as the rival claim, and a
  dispute. The dispute is still raised, and says neither is verified.
- **Stay inside the path limit.** Reports within 60 m + 2 x accuracy + 100
  m/s x (gap + 2 s) of the real drone are applied: at 2 Hz that is about
  320 m, and a forger that declares the worst accuracy widens it to about
  610 m. The shown position then wanders within that, and `motion_mismatch`
  or `address_alternation` may fall on the real drone. A tighter limit
  would flag honest fast drones.
- **Take over.** Jam or outlast the real drone: 10 s after its last report
  the track follows the forger. A dispute at the time is said; without one
  it is a `position_jump` at 25.
- **Clone the address too.** Position-less frames from the real drone's own
  address cannot be told apart; their text fields still go through the
  adoption rule, and alternating ones become a dispute.
- **Two forgers.** Rivals at two different places reset the single rival
  slot, so neither forms a path and no dispute is raised; nothing they send
  is applied either (`claims.rejected` counts it).
- **Merge two drones that fly together.** Frames naming both IDs, repeated
  for 2 s, merge two real drones within about 100 m of each other.
- **Raise disputes at will.** Any drone can be put into a dispute by
  repeating its ID elsewhere. That is the truth (two transmitters claim
  it), and it costs the attacker a sustained transmission.
- **O4.** Mint records with the receiver's public key, replay a recorded
  session after it has been unused for 30 min (the hub has forgotten it), or
  flip position bits in a new record: all decrypt. They are claims, judged
  like any other.
- **Fake an area GPS spoof.** Rival claims and disputes never count as
  drone jumps for the area check (WP-11b, docs/DRONE-ATTACKS.md "GPS
  interference in the area"): impersonating two real drones and moving
  them together raises nothing while the real ones keep transmitting. What
  still works is confirmed fakes of the attacker's own, at least three,
  heard as separate transmitters (different addresses, nodes or 6 dB of
  RSSI, born 2 s apart), moved together: AREA FAKE at confidence 60.

## Known false-alarm cases

- **Two Autels sharing a fixed MAC.** `shared_address` at 25 (weak), or 45
  if their frames happen to arrive at the same strength for most conflicts.
- **A GPS re-acquisition jump.** Not shown and not flagged while it lasts
  under 10 s (the reports are held as a rival claim; tested). A glitch that
  lasts longer is followed, which is one `position_jump` at 25 (weak).
- **A dispute on an honest drone.** Two GPS glitches to the same offset
  within 15 s, or a receiver that keeps flicking between two solutions,
  make the reports switch between two paths: a dispute. Two honest
  transmitters configured with the same serial (bench test modules) are a
  true dispute.
- **Non-conforming serials** (Autel, Parrot `PI040`). `serial_format` at 20
  (weak).
- **ESP32-based add-on modules.** `dev_board` at 15 (weak).
- **A light show powering up.** `spoofer_swarm` at 20-25 (weak) while the
  drones are close together. If a show spans a wide area at very different
  distances with equal RSSI, it can reach 65.
- **A remote ESP-NOW node over 300 m from the hub, with no
  `set_node_position`.** `rssi_too_strong` can fire on honest drones near
  that node. Set the node positions if nodes are spread out.
- **A hub whose own GPS clock is spoofed.** `clock_skew` fires on everyone.
- **A different drone taking off where another landed.** It can be linked
  as `previous` (not flagged) when the operator positions agree or are
  missing.
- **A third-party module configured to send a DJI serial.**
  `vendor_mismatch` 25.

## What still gets through

A careful attacker can beat every check here.

- **Consistent fakes from a moving transmitter.** A spoofer can move along
  a smooth path, keep its claimed position near its real one, use
  plausible speeds and altitudes, and pick a realistic power. That defeats
  the kinematics and signal-strength checks.
- **Valid serials and random addresses.** A spoofer can use a valid
  CTA-2063-A serial from a real maker code, with a locally administered
  random MAC instead of the ESP default. That defeats the format and vendor
  checks.
- **Spreading out.** A swarm sent from several boards at different powers,
  or started gradually, avoids `spoofer_swarm`. Using one MAC per fake
  drone avoids `shared_address`.
- **Long replay loops.** A replay loop longer than 3 Location updates,
  re-timestamped to the current time, avoids `replay` and the timing
  checks. A transmitter that simply re-broadcasts live, re-timestamped
  positions is indistinguishable from the drone, except by RSSI, if it sits
  somewhere else.
- **Cloning by taking over.** Cloning a real drone works best when the
  real drone is out of range. With nothing to alternate against, there is
  no dispute; the track follows the clone after 10 s (see "Impersonation
  and disputes").
- **DroneID forgery.** An SDR DroneID forger that sends matching Remote ID
  defeats `drone_id_disagrees`.
- **Slow identity changes.** An identity change made during a gap of more
  than 60 s, or more than 2 km from the dead-reckoned position, is not
  linked.
- **Authentication.** No signature is verified. Only malformed pages are
  noticed.

## Known gaps

- **Multi-node RSSI ordering** (for example, louder on a node farther from
  the claimed position). This needs per-node RSSI for one frame, which the
  tracker does not keep. It would cost 8 bytes per track plus node
  positions.
- **Remote ID's vertical speed** is not compared with the altitude change.
- **O4 encrypted (presence-only) tracks** and tracks without a position are
  never linked by continuity.
- **The Espressif table** is a snapshot (2026-11-17). DJI's blocks are
  `crates/wifidrone`'s.

## Sources

1. jjshoots/RemoteIDSpoofer, https://github.com/jjshoots/RemoteIDSpoofer, and dealyllama/RemoteIDSpoofer, https://github.com/dealyllama/RemoteIDSpoofer (ESP8266/ESP32 Remote ID spoofers).
2. ANG13T/drone-swarmer (ESP8266 ghost-drone swarm with GPS), https://github.com/ANG13T/drone-swarmer
3. "How to spoof Remote ID – SQUIDRID", sUAS News, July 2023, https://suasnews.com/2023/07/how-to-spoof-remote-id-squidrid
4. Jared Watkins, open-source drone detection and spoofer survey, https://www.jaredwatkins.com/research/drone-detection/open-source/
5. S. Brunner, F. Müller, bachelor thesis, ZHAW School of Engineering, 2023 (Cyber-Defence Campus Remote ID spoofer and receiver), https://srv-clst-301-data66.zhaw.ch/bitstream/11475/29937/1/2023_Brunner-Sebastian_Mueller-Fabia_BA_SoE.pdf, and https://github.com/cyber-defence-campus/RemoteIDReceiver
6. "Can't disable DJI Drone ID? Spoof it with an ESP", Hackaday, 2024-03-04, https://hackaday.com/2024/03/04/cant-disable-dji-drone-id-spoof-it-with-an-esp ; DJISDKUser/ESP8266_DJI_DroneID_Throwie, https://github.com/DJISDKUser/ESP8266_DJI_DroneID_Throwie
7. N. Schiller et al., "Drone Security and the Mysterious Case of DJI's DroneID", NDSS 2023, https://www.ndss-symposium.org/wp-content/uploads/2023-217-paper.pdf
8. CVE-2024-22520, Dronetag v1.5.2, https://github.com/Drone-Lab/Dronetag-vulnerability
9. SkySafe, "Drone manufacturers fail FAA Remote ID requirements", https://blog.skysafe.io/drone-manufacturers-fail-faa-remote-id-requirements
10. ANSI/CTA-2063-A, Small Unmanned Aerial Systems Serial Numbers, https://www.cta.tech/standards/ansicta-2063-a/ ; required by 14 CFR 89.505, https://www.ecfr.gov/current/title-14/chapter-I/subchapter-F/part-89/subpart-F/section-89.505
11. IETF draft-wiethuechter-drip-uas-sn-dns (quotes CTA-2063-A's character set: digits and upper-case letters except O and I; 4-character manufacturer code), https://datatracker.ietf.org/doc/html/draft-wiethuechter-drip-uas-sn-dns
12. Length-code examples: Parrot developer forum, "ANAFI Ai serial number DRI breakdown", https://forum.developer.parrot.com/t/anafi-ai-serial-number-dri-breakdown/22704 ; Watts Innovations knowledge base, "Serial Numbers & Labeling", https://wattsinnovations-1.gitbook.io/wattsinnovations-knowledge-base/aircraft/prism-sky/remote-id/serial-numbers-and-labeling
13. 14 CFR Part 89 Subpart F (Remote ID design and production), https://www.ecfr.gov/current/title-14/chapter-I/subchapter-F/part-89/subpart-F
14. Espressif Inc. MAC blocks, as listed by maclookup.app, https://maclookup.app/vendors/espressif-inc (snapshot 2026-11-17)
15. Related detection work, for comparison: RSSI-based position checking against broadcast positions, IMAV 2024, https://www.imavs.org/papers/2024/27.pdf ; "Remote ID Spoofing-Aware Trajectory Planning", https://arxiv.org/pdf/2607.19650 ; IETF DRIP authentication, RFC 9575.
