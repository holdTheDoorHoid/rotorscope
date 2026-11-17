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
| 1 | `position_jump` (m) | Two Remote ID positions from the same track, at most 120 s apart, are further apart than 60 m + 2 x declared accuracy (default 30 m, max 150 m) + 100 m/s x (time between them + 2 s) | 1 jump 25, 2 jumps 50, 3-5 jumps 75, 6 or more 90 |
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
| 14 | `drone_id_disagrees` (m) | A plaintext DroneID position lies more than 100 m + 2 x accuracy + 30 m/s x age from a Remote ID position under 3 s old on the same track. Needs at least 2 such readings, and more disagreements than agreements | 55; 5 or more 75 |

Why these work, and the honest cases they allow for:

- **Shared address.** A sketch that broadcasts several drones from one
  board cycles through them on one MAC. Each frame with a different ID
  "conflicts" with the track that last used the address. SkySafe found that
  Autel drones broadcast with **the same fixed MAC on every unit** [9], so
  two Autels flying together look exactly like this. That is why two
  sharers stay weak unless their frames arrive at identical strength, which
  two separate aircraft rarely do. Three or more sharers at once is a
  multi-drone transmitter.
- **Alternation.** If someone clones a real drone's ID, the tracker merges
  both transmitters onto one track, and their addresses alternate. A BLE
  address rotates every few minutes, which is one switch, not a pattern.
  Wi-Fi beacon and NAN share one MAC. In tests, a cloned ID also triggers
  `position_jump`.
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
track.spoof.score(); track.spoof.flags();         // for screens
tracker::spoof::Summary(&track)                  // one-line text (Display)
track.continuity.previous / .identity_changed
```

## Known false-alarm cases

- **Two Autels sharing a fixed MAC.** `shared_address` at 25 (weak), or 45
  if their frames happen to arrive at the same strength for most conflicts.
- **A GPS re-acquisition jump.** A single `position_jump` is 25 (weak).
  Repeated glitches are rare.
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
  no jump or alternation.
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
