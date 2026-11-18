# Attacks on drones (link protocol 13)

Protocol 13 added six threat kinds for attacks *on* drones rather than on the
detector: `DroneDeauth`, `DroneTakeover`, `DroneNetClone`, `ControlContested`,
`MavlinkAttack` and `AreaGnss` (`crates/link`, `ThreatKind`). Each has its own
section here. They are threats like the Wi-Fi attacks and jamming the hub already
lists: kept in the hub's threat table, logged as evidence, shown on screen (the
"ATTACK" box in the status bar, muteable from Settings > Alarms) and in the JSON
feed as `{"t":"threat",...}` lines.

## Second transmitter on a control link

`ThreatKind::ControlContested` (`control_contested`, screen label "Second
controller on a link"). Code: `crates/ctrlwatch/src/contested.rs` (the check),
`crates/lr2021cfg/src/ctrl.rs` `contested_threat` (the link frame),
`firmware/node-lr/src/control.rs` (wiring), `crates/tracker/src/threats.rs` (the
hub's table, mute rules, JSON). Branch `ctrl-contested`, 2026-11-17.

### In plain language

A drone's radio link has a fixed cast. An ExpressLRS link has **one** transmitter
(the pilot's radio). An mLRS link has one transmitter and one receiver. A SiK
telemetry link has exactly **two** radios, one on the drone and one at the ground
station, taking turns.

The known ways to hijack these links all add a radio to that cast:

- **SiK telemetry (SiKW00F, SiKening).** The attacker finds the link's network
  number (it is in every packet, and most people leave it at the factory value
  25), tunes a third SiK radio to it, falls into step with the pair's hopping, and
  slips its own MAVLink messages in: fake status text, parameter changes, commands.
  A bolder version puts two rogue radios in the middle, one talking to the drone
  and one to the ground station, sometimes after jamming the real link so both
  ends re-connect to the attacker.
- **ExpressLRS.** Anyone who learns the link's binding phrase (or its ID) can run
  a second transmitter on it; the drone's receiver follows whichever transmitter
  it locks on to. The attacker may jam first so the receiver loses the real pilot
  and re-locks.
- **mLRS and OpenLRSng**: the same idea with their own bind phrase.

So the question the detector asks is: **is this link being heard from more radios
than it can have?** When the answer is yes, the hub shows "Second controller on a
link", for example *"SiK/RFD900, 3 transmitters, two clocks in one window"* near
915 MHz. With the "control ids" setting off (the default) it does not say which
link (no NET ID or UID); with it on, the identifier is included.

**How it tells radios apart.** Not mainly by signal strength, because signal
strength changes all the time when a drone flies. Instead it uses the link's own
bookkeeping, which two radios cannot share by accident:

- An ExpressLRS transmitter numbers its packets (0 to 255, over and over) on a
  strict clock. Its occasional "sync" packets say which number it is at. Two
  transmitters on one link are at different numbers at the same moment: their
  clocks disagree, and the node sees syncs from the two clocks taking turns.
- An mLRS transmitter sends on a strict frame clock too; a second one has its own.
- SiK radios take turns in time slots, and every packet says when its sender's
  slot ends. Two packets in the same slot that disagree about when the slot ends
  came from two different clocks: a third radio.
- With content decoding on, SiK packets carry MAVLink messages that each component
  numbers in order. Two interleaved number sequences from the same "component"
  inside one time slot means two senders, even if the intruder keeps perfect time.

**What it cannot do.** An attacker who copies the real transmitter's timing
exactly and never says anything contradictory is indistinguishable by timing. The
main case: a SiKW00F radio that is properly in step with the pair is only caught
when content decoding is on (the MAVLink numbering check). And two people at a
field who use the same ExpressLRS binding phrase, or two drones left on SiK's
default network number 25, really are "more transmitters on one link"; that is
reported too (and is a real problem for them: their links interfere). Mute it if
it is expected.

### Rules and thresholds

Every packet the node recognises (`ctrlwatch::recognise`) also goes to
`contested::Watch::observe`. Packets are grouped per link by **system, region and
identifier** (ExpressLRS UID bytes and Model Match value, SiK NET ID, mLRS sync
word). The identifier is used inside the node for this grouping only; it leaves
the node only when `read_ids` is on at the moment a report is sent.

The node is a scanner: it listens on one channel per system for a few seconds at
a time (ExpressLRS: the sync channel, 1-7 s; mLRS and SiK: one channel, 3-6 s),
then moves on, and comes back on the next cycle. So the check keeps state across
visits: up to 8 links, 12 observations each, kept 10 minutes.

**ExpressLRS: interleaved timing (`RULE_PHASE`).**

- Uses only CRC-checked SYNC packets (Confirmed). A 3.x sync that only checks with
  Model Match (Likely) is not used.
- Each sync gives a *phase*: `t - nonce x interval`, modulo `256 x interval` (0.5
  to 10 s depending on the rate). One transmitter keeps one phase.
- Two syncs are the same transmitter if their phases are within
  `TIME_TOL_US` (2 ms) plus `DRIFT_PPM` (50 ppm) of the time between them. 2 ms
  covers the node stamping a packet late (interrupt wake-up, SPI); 50 ppm covers
  the ESP32 crystal (±10 ppm) plus the transmitter's (TCXO ±2.5 ppm to crystal
  ±20 ppm) with margin. When the tolerance grows past a quarter of the cycle the
  two are "too far apart to tell" and never compared.
- Syncs are grouped (consistent pairs chained together). A group with at least
  `MIN_EACH` = 3 syncs is a transmitter.
- **Contested** when two transmitters on one link **alternate**: at least
  `MIN_SWITCHES` = 3 changes from one to the other (A B A B), each change between
  two syncs close enough in time to compare. One change (A A A B B B) is a
  restart, not a contest. New evidence is counted at each further change.
- A random second transmitter lands inside the tolerance of the first with odds of
  about 2 x 2 ms / cycle: 0.8 % at 500 Hz (cycle 512 ms), 0.3 % at 200 Hz.
- A packet-rate change gives a different cycle: a different group, never compared,
  never a contest.

**mLRS: interleaved timing (`RULE_PHASE`).**

- Uses CRC-checked frames. Phase = arrival time modulo the frame interval (20, 32
  or 53 ms), kept separately for uplink frames (transmitter) and downlink frames
  (receiver).
- Same tolerance, grouping and alternation rule. The transmitter count reported
  adds the receiver if it was heard (a contested mLRS link usually reads "3
  transmitters": two uplink clocks and the receiver).
- Weaker than ExpressLRS: the cycle is short, so a second transmitter can land
  inside the tolerance by chance (8 % at 53 ms, 20 % at 20 ms per comparison). The
  alternation rule needs several comparisons, which keeps a miss from turning into
  a false alarm, but a lucky intruder can go unnoticed.

**SiK: two clocks in one window (`RULE_CLOCKS`).**

- Every SiK packet ends with a TDM trailer whose `window` field is the number of
  16 µs ticks left in the sender's transmit window after this packet. So a packet
  received at `t` claims "my window ends at `t + window x 16 µs`". Statistics
  packets (`window` = 0) are skipped.
- Two consecutive packets of one link, at most `SIK_PAIR_US` (150 ms, longer than
  the largest window of 131 ms) apart, with the second one arriving before the end
  the first claimed, are in the same window. Their claimed ends must agree within
  `SIK_END_TOL_US` (3 ms: timestamp jitter plus the sender's flight-time estimate,
  rounded to bytes). The other radio's "bonus" packets on yielded ticks carry the
  same end (it is synchronised to the owner), so they agree too.
- A disagreement is a contradicting window (several within 150 ms count once).
  **Contested** after `SIK_MIN_EVENTS` = 3 contradicting windows in the last 10
  minutes, reported as at least **3 transmitters** (the pair plus one).
- The trailer is the last 2 bytes on most builds, but AES-capable builds
  (RFD900-class) put a 2-byte CRC after it. Both readings are tried on every
  packet; the one whose ends agree more often is used. A reading is only trusted
  once it has `SIK_MIN_AGREE` = 8 agreeing pairs and agreeing pairs are at least a
  quarter of all compared pairs (`SIK_AGREE_RATIO` = 3): the wrong reading is
  noise and agrees about 5 % of the time; an intruder slipping one packet into a
  window of five still leaves half the pairs agreeing.

**SiK, with content decoding on: two sequence counts (`RULE_SEQUENCE`).**

- Runs only with `decode_content`. MAVLink frames in the packets (CRC-checked,
  known messages) carry a sequence number that each sending component (system id,
  component id) raises by one per frame.
- Inside one window (150 ms), a frame continues a count if it is 0 to
  `MAV_MAX_STEP` = 16 ahead of it (0: SiK resends; up to 16: lost packets).
  Otherwise it starts a second count. When a frame continues a count after
  *another* count for the same component has moved (A 100, 101, B 7, A 102), two
  senders are interleaving. One piece of evidence per window; reported as at
  least 3 transmitters.
- This is what catches a SiKW00F injector that is in step with the pair: it posts
  as the ground station (system 255) in the ground station's window, with its own
  sequence count. Nothing it reads leaves the node.

**Forced re-sync (`RULE_HANDOVER`, ExpressLRS and mLRS).**

- A transmitter (3+ observations) goes silent and a new one (3+ observations,
  inconsistent phase) starts within `HANDOVER_GAP_US` = 30 s, and the two differ in
  signature: ExpressLRS switch mode, telemetry ratio or link mode, or mean signal
  strength `HANDOVER_RSSI_DB` = 15 dB apart. One report per new transmitter,
  2 transmitters.
- A pilot power-cycling the radio also gives a new phase; that is why a signature
  change is required, and why this is reported as its own, weakest, rule.

**Reports.** New evidence goes to the hub at most once a second; a standing
finding is repeated every 5 s for 60 s after its last evidence (so the hub keeps it
"active" between scan visits), then stops.

### What the hub receives

A `Frame::Threat` (`ThreatSighting`) with `kind = ControlContested`:

| Field | Meaning |
|---|---|
| `detail` | bits 0-7 system code (`ctrlwatch::System` discriminant: 0 ExpressLRS, 1 mLRS, 2 SiK), bits 8-11 transmitters (at least; max 15), bits 12-15 rule bits (1 interleaved timing, 2 two clocks in one window, 4 two sequence counts, 8 new transmitter after silence), bits 16-23 region code (`Region::ALL` index). `ctrlwatch::contested::decode_detail` reads it. |
| `freq` | where the node heard it, 100 kHz units |
| `level` | strongest packet on the link, dBm |
| `count` | pieces of evidence so far |
| `window_ms` | first to latest evidence |
| `source` | the node's own handle for the link in the last byte (1-255). Not an identifier of anything; it keeps two contested links on one node apart. |
| `target` | the link identifier, right-aligned: SiK NET ID or mLRS sync word in the last two bytes; ExpressLRS UID byte 3 (or 0), 4, 5 as sent, 5 as checked in the last four. **All zero unless `control ids` is on.** |
| `ssid`, `channel`, `width_khz` | empty / 0 |

On the hub: one table entry per node, link handle, identifier and system
(`Threat::same`). A mute rule made from it (`Mute::for_threat`) keys on the
identifier and the band (900 MHz or 2.4 GHz), not the handle; with ids off that
silences contested-link alarms in that band from any link. The JSON threat line
adds `"ctrl_system"`, `"transmitters"` and `"rule"`; `src`/`target` appear only
with `show_link_ids`. The console and evidence text read, for example,
`Second controller on a link (node 3, 915.5 MHz, ExpressLRS, 2 transmitters,
interleaved timing, 4 events, -61 dBm)`.

### False alarms and missed attacks

- **Same binding phrase / default NET ID.** Two pilots sharing an ExpressLRS
  phrase with Model Match off, or two SiK pairs left on NET ID 25 near each other.
  Real second transmitters; reported. Mute (by band, or by identifier with ids on).
- **One GCS radio carrying two ground-station programs with the same system and
  component id** (MAVProxy forwarding QGroundControl and Mission Planner both as
  255/190): two sequence counts on one radio. Rare; mute.
- **SiK radios finding each other** after power-up are briefly out of step; that
  can give contradicting windows for a few seconds. Three windows in ten minutes
  plus the agreement ratio are meant to absorb it; unmeasured.
- **Forced re-sync** fires on a power cycle combined with a settings change, or a
  restart at a very different signal level. It does not use jamming evidence (the
  hub has jamming threats from other nodes; tying the two together is not done).
- **Missed**: an intruder in perfect timing step with nothing contradictory to
  say (see "What it cannot do"); a lucky mLRS phase; anything on a channel or rate
  the node is not listening to at that moment (it is a scanner); SiK links whose
  channel plan the node's one listening channel misses; ExpressLRS FLRC/FSK rates
  and mLRS FLRC/FSK modes (not receivable without the phrase); OpenLRSng (no
  counter readable without the bind data, and its packet interval depends on it);
  closed systems (energy only).
- **Signal strength alone is never used to raise an alarm.** It only adds to the
  forced re-sync rule, together with a new phase.

### Sources

- SiKW00F: github.com/nicholasaleks/sikw00f (README, `core/*.py`): a third SiK
  radio with modified firmware (NET ID header filter bypass, NET ID overwrite,
  "state machine synchronization") that scans for NET IDs, copies the target's
  parameters and follows its hopping; injection via MavSploit is listed as coming.
- SiKening: github.com/MAVProxyUser/SiKening (brute-force the NET ID, sync to the
  hopping sequence). Listed in github.com/nicholasaleks/Awesome-Drone-Hacking.
- The infected-drones write-up (local copy reviewed) on SiK injection versus
  full MITM with a rogue pair, optionally after jamming to force re-association.
- SiK firmware: github.com/ArduPilot/SiK `Firmware/radio/tdm.c` (the TDM states,
  the trailer: `window:13, command, bonus, resend`, plus a CRC with AES;
  `sync_tx_windows`), `freq_hopping.c` (channel changes at the start and end of
  each transmit window).
- ExpressLRS: the nonce (+1 per packet interval) and FHSS index in SYNC packets,
  as ported in `crates/ctrlwatch/src/elrs.rs` (sources listed there and in
  docs/CONTROL-LINKS.md).
- mLRS (`mLRS/Common`): fixed frame period per mode, frame sync word, X.25 CRC.

### Not verified without hardware

We have no LR2021 board. Everything above is tested against packets built from the
projects' source by test encoders (`crates/ctrlwatch/src/contested/tests.rs`: 17
tests: two ExpressLRS transmitters on one UID, mLRS second uplink, SiK unsynced
third radio, SiK with AES trailer, SiKW00F-style synced injection with and without
content, ExpressLRS takeover after silence; and quiet cases: one transmitter with
jitter, 30 ppm drift, 40 dB signal swings and minutes of loss; restarts; rate
changes; separate links with different IDs and Model Match; mLRS over scan visits
with 40 ppm drift; SiK pair with bonus, statistics packets and loss; resends).

Not verified:

- The node's real timestamp jitter (the 2 ms tolerance assumes a few hundred µs).
- That ExpressLRS SYNC nonces progress exactly one per interval on air across sync
  channel visits, including through telemetry slots (from the source, not
  measured).
- SiK: the trailer's bit layout on air (taken from `struct tdm_trailer`, little
  endian), that `t + window x 16 µs` agrees within 3 ms between packets of one
  window on real radios, the 4-byte AES-build trailer, and how long a pair is out
  of step at power-up.
- What a real SiKW00F injector's timing looks like (whether it is in step, and how
  well); no injector has been observed.
- mLRS frame timing jitter on real transmitters.
- The LR2021 does not report a per-packet frequency error in the packet status the
  node reads (`GetLoraPacketStatus`, `GetFskPacketStatus`), so transmitter crystal
  offsets, the best physical fingerprint, are not used. If a later chip revision
  or an undocumented register gives it, it would separate in-step intruders.
