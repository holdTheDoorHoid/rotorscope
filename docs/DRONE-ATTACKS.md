# Attacks on drones (link protocol 13)

Protocol 13 added six threat kinds for attacks *on* drones rather than on the
detector: `DroneDeauth`, `DroneTakeover`, `DroneNetClone`, `ControlContested`,
`MavlinkAttack` and `AreaGnss` (`crates/link`, `ThreatKind`). Each has its own
section here. They are threats like the Wi-Fi attacks and jamming the hub already
lists: kept in the hub's threat table, logged as evidence, shown on screen (the
"ATTACK" box in the status bar, muteable for two hours from Settings > Alarms, one alarm at a time, see docs/SITE.md; `AreaGnss` is a safety kind and can only be snoozed, for five minutes) and in the JSON
feed as `{"t":"threat",...}` lines. Area GPS interference also has its own
AREA box and health-page line.

Sections:
- Second transmitter on a control link (`ControlContested`)
- Wi-Fi drone hijacking (`DroneDeauth`, `DroneTakeover`, `DroneNetClone`)
- GPS interference in the area (`AreaGnss`)
- Hostile MAVLink telemetry (`MavlinkAttack`)

Sources reviewed for this set: nicholasaleks/Awesome-Drone-Hacking,
nicholasaleks/infected-drones, and the OWASP Drone Security Cheat Sheet.

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

**History first (WP-11a).** These protocols carry no authentication, so a
forger can send syncs and SiK trailers that say anything. Before WP-11a
about six ExpressLRS syncs from **one** radio, alternating between two nonce
phases on a made-up UID, raised "link hijack" with no drone anywhere. Now a
link's contradictions count only once the link has been heard as **one**
consistent transmitter for `HISTORY_US` = 5 s before a second appeared (the
link is then *seasoned*, for as long as it is kept): ExpressLRS and mLRS, an
established group spanning 5 s with no other established group of its class
starting within those 5 s; SiK, window ends agreeing (8 pairs or more) for
5 s. The forced re-sync rule needs the same history of the old transmitter.
A real hijack happens to a link that was already flying; a link contested
from its first packet is counted (`unseasoned` on the `ctrl_pressure` line)
and not reported.

**New alarms are budgeted.** The first report of each contested link costs
a token: two at once, then one every 10 s for the whole node
(`NEW_FINDING_BURST`, `NEW_FINDING_REFILL_US`). A finding that waits is
counted (`held`) and goes out when a token comes, unless it lapsed (60 s
without new evidence) by then. Further reports of a link already reported
are not limited beyond the usual once a second.

**A full table.** 8 links are kept. A link with one observation is on
probation and makes room first; a link holding evidence (two or more
observations, SiK window counts, a finding) only after 120 s without a
packet (`QUIET_EVICT_US`); otherwise the newcomer is turned away (counted).
Before, the least recently heard link without a finding made room, so a
spray of syncs on made-up UIDs between the victim's packets emptied its
phase ring. A link's very first packet can still be pushed out by a spray
of 7 or more new UIDs before its second packet.

node-lr prints these counters on USB when they change (at most every 5 s):
`{"t":"ctrl_pressure","links":[dropped, dropped holding evidence, turned
away],"unseasoned":N,"held":N,"mav":[...]}` (the `mav` part is under
"Hostile MAVLink telemetry").

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
identifier and the band (900 MHz or 2.4 GHz), not the handle; with ids off
(identifier all zero) it also keys on the node, the handle and the system, so
it silences that one link, for two hours (WP-11b; before, it silenced every
contested link in the band). A site-file mute for good must name the
identifier, so needs ids on (docs/SITE.md). The JSON threat line
adds `"ctrl_system"`, `"transmitters"` and `"rule"`; `src`/`target` appear only
with `show_link_ids`. The console and evidence text read, for example,
`Second controller on a link (node 3, 915.5 MHz, ExpressLRS, 2 transmitters,
interleaved timing, 4 events, -61 dBm)`.

### False alarms and missed attacks

- **Same binding phrase / default NET ID.** Two pilots sharing an ExpressLRS
  phrase with Model Match off, or two SiK pairs left on NET ID 25 near each other.
  Real second transmitters; reported. Mute that link on the hub (2 h), or for good
  in the site file by identifier and band (needs ids on).
- **One GCS radio carrying two ground-station programs with the same system and
  component id** (MAVProxy forwarding QGroundControl and Mission Planner both as
  255/190): two sequence counts on one radio. Rare; mute.
- **SiK radios finding each other** after power-up are briefly out of step; that
  can give contradicting windows for a few seconds. Three windows in ten minutes
  plus the agreement ratio are meant to absorb it; unmeasured.
- **Forced re-sync** fires on a power cycle combined with a settings change, or a
  restart at a very different signal level. It does not use jamming evidence (the
  hub has jamming threats from other nodes; tying the two together is not done).
- **Missed (WP-11a)**: a hijack of a link the node first hears during the
  attack (no 5 s of history: counted as `unseasoned`, not reported); more new
  contested links than the budget lets out (they wait, and lapse after 60 s).
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

## Wi-Fi drone hijacking

### In plain words

Many cheap and toy drones (DJI Tello and Spark, Parrot AR.Drone and Bebop,
the unbranded "WIFI-UFO" quadcopters) are flown from a phone over ordinary
Wi-Fi: the drone makes its own little Wi-Fi network and the pilot's phone
joins it. Usually that network has no password, or a weak one.

The well-known hijacks all work the same way:

1. **Kick the pilot off.** The attacker sends forged "you are disconnected"
   messages (deauthentication frames) to the pilot's phone, pretending to be
   the drone. Any laptop with a cheap Wi-Fi adapter can do it.
2. **Take the pilot's place.** While the phone is reconnecting, the
   attacker's laptop joins the drone's network and starts sending flight
   commands. The drone obeys whoever is connected.

A variant sets up a **copy of the drone's network** (same name) so the
pilot's phone connects to the attacker instead (an "evil twin"), to steal
the password or feed the app false information.

Rotorscope's Wi-Fi nodes only listen. They recognise drone networks by name
and maker address, and then watch those networks for the three tell-tale
signs, each raised as its own alarm on the hub:

| Alarm (hub label) | Kind | What it means |
|---|---|---|
| "Drone kicked off its Wi-Fi" | `DroneDeauth` | Someone is sending disconnect messages at a drone's network. Even a few are suspicious: a toy drone has only one controller. |
| "Drone Wi-Fi taken over" | `DroneTakeover` | A new device joined a drone's network right after the pilot was kicked off, or while the pilot was still flying it. |
| "Copy of a drone's network" | `DroneNetClone` | A second transmitter is using a drone's network name, or a drone's name is coming from an address that does not belong to that drone maker. |

What it cannot do: it does not stop the attack (the nodes never transmit),
it cannot read the drone's commands (only addresses), and it only covers
drones that fly over plain Wi-Fi. Drones with their own radio links (most
DJI models, FPV, ExpressLRS) are covered elsewhere.

### How it works

Code: `crates/wifiwatch/src/drones.rs` (`DroneLinks`, `Clones`), tests in
`crates/wifiwatch/src/drones/tests.rs`. Node: `firmware/node-wifi`
(`rx.rs`, `main.rs`). The drone table is `crates/wifidrone` (13 makers).

- **Learning drones.** Every beacon and probe response the node queues goes
  through `wifiwatch::beacons::Beacons`, which asks `wifidrone::classify`
  whether it is a drone's network. If so it returns the drone (address,
  maker, name, channel, the beacon's timestamp) and the node's drain task
  hands it to `DroneLinks` (up to 3 drone networks followed at once; the
  stalest one not being reported makes room; forgotten after 2 minutes
  without a beacon).
- **What the node sees.** The promiscuous filter passes every management
  frame and, on node-wifi, data frames (`chip::OPEN_LINKS`). The receive
  callback passes deauth / disassoc, authentication, (re)association
  requests and responses, and the two addresses of each data frame, to
  `DroneLinks`. While no drone is known that costs one atomic load per
  frame. Association responses used to be queued and ignored; they are now
  read in the callback and not queued. Encrypted contents are never read.
- **The controller.** The station a drone is talking to (data frames either
  way, an accepted association, or the station a deauth is about) is its
  controller. Data frames keep it fresh: a phone flying a drone exchanges
  commands and video many times a second.
- **Restarts.** A beacon's timestamp is the drone's radio uptime. A beacon
  that puts the drone's start more than 2 s away from its clock is a
  **second timeline**. It is believed (the drone restarted: its controller
  and any newcomer are forgotten, so the phone rejoining, even with a new
  random address, is not a takeover) only after 3 beacons on it over at
  least 1 s, none on the old clock since, and the old clock silent for 1 s
  before it appeared (a restart takes seconds) or for 10 s since
  (`REBOOT_BEACONS`, `REBOOT_CONFIRM_MS`, `REBOOT_SILENCE_MS`,
  `REBOOT_ALONE_MS`). Before WP-11a one beacon with the drone's address and
  a timestamp of 1 was enough, and wiped the deauth count and the learned
  controller: interleaved between deauths it hid `DroneDeauth`, and the
  attacker's first frame after it became "the controller", so the pilot's
  phone reconnecting was reported as the takeover's newcomer. Both clocks
  heard in turn is a `DroneNetClone` (rule 4 below). A beacon with
  timestamp 0 is ignored once the drone's clock is known.
- **A full table.** 4 drone networks are followed (3 before WP-11a; each
  slot is about 190 bytes). A newcomer heard once may only take the slot of
  a network holding no evidence (no controller heard in the last 30 s, no
  recent deauths, no newcomer, no second clock) or not heard for 15 s.
  Otherwise it waits (4 remembered); once heard 3 times over 1 s it may
  displace a network holding evidence: one that has been "busy" (a deauth
  attack or takeover being reported) for more than 60 s first, then one not
  busy; a locally administered address first, then the network heard least
  since it was learned. A network busy for less than 60 s is never
  displaced, and displacing one holding evidence costs a token (one per
  30 s). Before, forged drone networks kept "taken" (a data frame from one
  made-up station and two authentication requests from another, every few
  seconds) held every slot for as long as they were refreshed, and the real
  drone was never followed; a spray of single beacons from made-up drone
  networks threw out a real one holding two deauths of three. The stats
  line's `dnet` is `[learned, displaced, made to wait]`.

### `DroneDeauth`

**Rule:** 3 deauth or disassoc frames that involve a drone's network (its
address as BSSID, sender or receiver) within 5 s, a retransmission (Retry
bit **and** the transmitter's previous sequence number) counted once. Until
WP-11a every Retry-bit frame was skipped; a receiver only discards a retry
that repeats the last frame it took, so an injector setting the bit on
frames with new numbers still disconnected the phone, unseen.
Reported at most once a second per drone while frames keep coming; over
when none has come for 5 s.

**Why 3:** the generic deauth detector (`Deauth`, docs/WIFI-ATTACKS.md)
needs 30 in a second, because busy networks kick stations all the time. A toy
drone has one phone: a phone leaving sends one deauth or disassoc (at most
one of each), and the drone dropping an idle phone sends one (its retries
are not counted). Every published hijack tool sends far more: SkyJack and
DroneJack run `aireplay-ng -0 3` (3 x 128 frames), Drone-Hacking-Tool
`-0 15`, `-0 35` or continuous, DroneSploit `-0 5`. Even a node that only
visits the drone's channel 12% of the time hears dozens.

**Ignored:** deauths with reason 6 or 7 ("frame from a station that is not
joined") sent by a drone whose beacon timestamp says it started less than a
minute ago (or whose restart is pending: a new clock after the old one fell
silent, not interrupted by it). That is a drone back from a battery swap telling the phone it
does not know it any more. Protected (802.11w) frames are counted (an
attempt, even if a protected client would ignore it) and flagged.

**Fields:** `target` = the drone's BSSID, `source` = claimed sender (latest),
`ssid` = drone network name, `count` = frames since the last report,
`window_ms` = the time they span, `level` = strongest RSSI, `channel` = where
heard. `detail`: low 16 bits the most common reason code; bit 16 some went
to broadcast; bit 17 all were protected (reason unreadable, reads 0); bit 18
some were disassociations; bit 19 the drone's known controller was hit;
bits 24-31 the maker (`wifidrone::MAKERS` index, shown by the hub as
`maker`).

### `DroneTakeover`

**Rule:** a station other than the drone's controller sends 2 frames that
show it joining or using the drone (authentication request, association
request, the drone accepting it, data either way) when any of these holds:

- a `DroneDeauth` happened on this drone in the last 30 s (flag
  `TAKE_AFTER_KICK`, bit 4);
- the controller got a single deauth or disassoc in the last 5 s (bit 5,
  `TAKE_AFTER_LEAVE`): a one-frame kick racing the phone's reconnect;
- the controller was heard in the last 10 s and has not left (bit 6,
  `TAKE_TWO_CONTROLLERS`): DroneJack joins first and kicks afterwards.

Otherwise (the controller left quietly long enough ago) the newcomer simply
becomes the controller: a pilot handing the drone to a friend, or coming
back with a new address.

Reported at most once a second per drone while the newcomer keeps sending;
over after 10 s of silence. `source` = the newcomer, `target` = the drone's
BSSID, `count` = its frames since the last report, `window_ms` = since its
first frame. `detail` bits 0-3 say what was seen of it (authentication,
association request, accepted by the drone, data); bits 4-6 why it is
suspicious (above); bits 24-31 the maker.

**Why 2 frames:** a real join is always at least an authentication and an
association request, and a flying controller sends data many times a
second, so 2 is reached at once; a single stray frame (corrupted, or a
phone that only started joining) is not enough.

### `DroneNetClone`

**Rules** (beacons and probe responses):

1. **Second address.** A drone's network name, heard from its original
   address at least 5 times in the last 30 s, now comes from a different
   address (2 frames). Open or not.
2. **Same address, other channel.** The drone's own address advertising the
   name on a second channel (by the DS Parameter / HT Operation element,
   not where the node heard it) while the original carries on, on its own
   channel (frames on both, interleaved, within 10 s): a copy made with
   `airbase-ng -a <drone>`.
3. **Wrong maker.** A name that matches a maker's precise default pattern
   (a fixed number of hex digits, for example DJI's `Spark-` plus 6) from an
   address outside that maker's registered blocks, 3 frames. No original
   needs to be heard (the drone may be out of the node's range).
4. **Same address, two clocks** (WP-11a, from the drone-link watch). The
   drone's own address, name and channel, but beacons whose timestamps put
   the drone's start somewhere else (more than 2 s away), heard in turn with
   the drone's own beacons (2 on the second clock, the drone's own clock
   heard between them, within 10 s). A copy made with the drone's address
   on its own channel keeps a clock of its own; so does a forged
   "restart" beacon. `source` = `target` = the drone's address, `count` =
   beacons on the second clock, detail bits 3 and 5.

`source` = the copy, `target` = the original (all zero if never heard),
`count` = frames from the copy, `ssid` = the name. `detail`: bit 0 the copy
is open; bit 1 its security differs from the original's; bit 2 its address
is not the maker's; bit 3 it uses the original's own address; bit 4 it says
it is on another channel; bit 5 (`CLONE_TWO_CLOCKS`, WP-11a) it keeps a
beacon clock of its own on the original's address; bits 24-31 the maker. Reported at most once a
second per copy.

**Not flagged:**

- A drone's other radio: a second address that differs from the original
  only in the locally administered bit of the first byte and by at most 8 in
  the last byte (how dual-band access points number their radios). The
  wifidrone notes say nothing about drones doing this; it is there in case
  a dual-band drone does.
- Names without a per-unit suffix (fewer than 4 hex digits at the end, such
  as `WiFi-720P`): two identical toys can share them.
- A drone that changes channel (its old channel falls silent), or is
  replaced by another unit with the same name after being gone 30 s.
- Weak names (`Typhoon`, `Bebop...` with no fixed digits) on other makers'
  addresses: only the precise patterns accuse an address.

### On the hub

`crates/tracker/src/threats.rs`: `DroneDeauth` and `DroneTakeover` are one
threat per drone network (the drone's BSSID) and node, whoever attacks it;
the entry's `source` follows the latest attacker. `DroneNetClone` is one
per name and copy, like an evil twin. Muting one (`Mute::for_threat`) mutes
that drone (deauth, takeover) or that name from that copy (clone). The
`{"t":"threat"}` line gains `"maker"` (for example `"DJI Tello"`) for these
three kinds; the line buffer grew from 512 to 576 bytes to hold the longest
one. The P4 hub's console line and evidence event end with the maker in
brackets. Labels: "Drone kicked off its Wi-Fi", "Drone Wi-Fi taken over",
"Copy of a drone's network".

### Attack-tool signatures

Added to `wifiwatch::TOOLS` (`AttackTool`, docs/WIFI-ATTACKS.md), each
checked in the tool's source:

| # | Tool | Signature | Source |
|---|---|---|---|
| 4 | RemoteIDSpoofer (ESP8266 fake Remote ID) | Setup network `ESP_RIDS`, up at boot before spoofing starts | jjshoots/RemoteIDSpoofer `RemoteIDSpoofer/frontend.cpp` line 8, commit c79ab77 |
| 5 | DroneSploit | A drone renamed to the module default `TELLO-abcdef` or `C-me-abcdef` | dhondta/dronesploit `.../command/dji/tello.py` line 28, `.../command/hobicco/cme.py` line 47, commit 6db6f12 |
| 6 | Wifiphisher | Rogue access point address `00:00:00:xx:xx:xx` (its `DEFAULT_OUI`, unless run with `--no-mac-randomization`) | wifiphisher/wifiphisher `common/constants.py` line 50, `common/interfaces.py` lines 932-943, `pywifiphisher.py` lines 526-529, commit 4ae3365 |

SkyJack (samyk/skyjack), DroneJack (brospars/wic-ter-dronejack),
Drone-Hacking-Tool (HKSSY) and DroneSploit's attack modules have no fixed
on-air signature: they drive `aireplay-ng` and the computer's own Wi-Fi
client, with the computer's own address. Their behaviour is what
`DroneDeauth` and `DroneTakeover` catch. RemoteIDSpoofer's fake drones use a
random 16-character name and random addresses; the Remote ID side
(docs/SPOOFING.md) is where they show.

### False alarms to expect

- **Testing your own drone** with a deauther: it is a real `DroneDeauth`.
  Mute it at the hub (the mute is per drone, for two hours).
- **Two phones** joined to one drone on purpose (a second viewer): reported
  as `DroneTakeover` with bit 6. Toy drones normally accept one phone.
- **A pilot's phone that changes its random address** while flying (rare;
  phones keep one per network) looks like a newcomer while the old address
  is still fresh.
- **A one-frame leave followed within 5 s by another phone joining**
  (unlikely by hand) is reported with bit 5.
- **Broadcast-only attacks** where the node never learned the controller:
  the first station to join after the attack is taken as the pilot; if that
  was the attacker, the pilot's reconnect is what gets reported (source is
  then the pilot). The alarm is still right; the address is the wrong one.
- **Copies seen before the original:** if the node hears the copy first, the
  copy is taken as the original and the real drone is reported as the
  copy (`source` and `target` swapped).
- **Wrong-maker:** a real drone whose address is in a block missing from
  `wifidrone` (the DJI list has 11 blocks). Only DJI's `Spark-` names are
  precise enough today; check the address against the IEEE registry and add
  the block to `wifidrone` if it is DJI's.

### Blind spots

- An attacker who sends one deauth and is slower than 5 s to join, while the
  node hears no data frames, is not seen (the pilot's phone usually wins the
  race anyway).
- With data frames off (`chip::OPEN_LINKS` false) a controller is known only
  from its joining, so "two controllers" is only seen within 10 s of it.
- A drone on a channel outside the node's plan is heard only on scouting
  visits (105 ms every 3 s per channel): beacons will be learned, but a
  short attack can be missed.
- A copy using the drone's own address on the drone's own channel is caught
  only by its beacon clock (rule 4): one that copies the drone's timestamps
  (adding the right offset to each) cannot be told apart.

### Sources

- Samy Kamkar, SkyJack (2013), github.com/samyk/skyjack `skyjack.pl`:
  Parrot address blocks, `aireplay-ng -0 3 -a <drone> -c <phone>`, then
  joins the drone's network.
- brospars/wic-ter-dronejack `attack/deauth-takeover/app.js`: joins first,
  then `aireplay-ng -0 3` at every other client.
- HKSSY/Drone-Hacking-Tool `drone_hacking_tool.py`: handshake capture,
  cracking, `aireplay-ng -0 15/35/0`, then joins (`nmcli`).
- dhondta/dronesploit `wifi/mixin.py`: `aireplay-ng -0 5`; Tello and C-me
  rename modules.
- Ordina-JTech/hack-a-drone: controlling a Wi-Fi toy (Cheerson CX-10) once
  joined.
- Pedro Cabrera, "All your bebop drones still belong to us" (Rooted CON
  2016); Drone Wars, BloomCon (Wi-Fi drone hijack contest).
- Wifiphisher (evil twin): github.com/wifiphisher/wifiphisher.
- Awesome-Drone-Hacking (nicholasaleks) and the OWASP drone security notes
  (802.11w recommended against forged deauths).

### What needs hardware

1. **Frames reach the callback.** With a Tello (or any `wifidrone` drone)
   flying from a phone near a node: `wdrone` in the stats line counts it;
   then a deauther board at the drone in a shielded box (your own drone
   only) must give `drone_deauth` threat lines with the Tello's name.
2. **Association responses and data frames** arrive with addresses intact
   on both chips (S3 and C5), so a second phone joining the drone gives
   `drone_takeover` with bits 2 and 3 set.
3. **Beacon timestamps** of real toy drones count from boot (the reboot rule
   depends on it): power-cycle the drone and check no alarm follows.
4. **Thresholds in real flying:** a day of normal flights, phone apps
   backgrounded and resumed, battery swaps, must give no drone alarms.
5. **Cost:** with a drone known, `drop` and `qmax` must stay low during a
   video stream (the callback takes a lock per data frame then).

## What a forged frame can make these alarms say

Like the Wi-Fi alarms (docs/WIFI-ATTACKS.md, same heading), every address
in these alarms is what a frame claimed (WP-11b):

- **Drone Wi-Fi hijacking** (`drone_deauth`, `drone_takeover`,
  `drone_net_clone`): the "attacker" address and the drone network's name
  are whatever the frames carry. An owner's own phone reconnecting can be
  made to look like a takeover by someone who sends a deauth in its name.
- **Contested control link** (`control_contested`): two transmitters were
  heard; which one is the owner is not known, and the link identifier (with
  `control ids on`) is copied from the air.
- **Hostile MAVLink** (`mavlink_attack`): the system and component IDs are
  set by the sender; "from the ground station 255/190" means the frame
  said so.
- **GPS interference in the area** (`area_gnss`): built from drones'
  broadcast positions and aircraft ADS-B, both unauthenticated. The rules
  below make an attacker need several independent transmitters, but the
  alarm still says what receivers *claim*.

The threat JSON line marks shown addresses and names with `"claimed":true`;
evidence and mute texts say `claimed`. A mute names one source or victim
and ends (docs/SITE.md), so framing an innocent address costs the attacker
that address, not every alarm of the kind.

### Cheapest forgery, and what still gets past (after WP-11a)

Raising an alarm on purpose (left) and hiding a real attack (right), per
kind, for someone in radio range who has read this source.

| Alarm | Cheapest forgery that raises it | Known evasions that remain |
|---|---|---|
| `drone_deauth` | Beacons of a made-up drone network (a maker's address block and name pattern, 10 a second) plus 3 deauths at it within 5 s. With a real drone in range: 3 deauths naming it. | Deauths on a channel the node is not on (it hops; a short burst on a visited channel can be missed). Protected (802.11w) deauths are counted but a protected phone ignores them anyway. A drone the node never learned (out of range of its beacons, or turned away from a full table: see the next row's evasions). |
| `drone_takeover` | A made-up drone network as above, a data frame from one made-up station, then two authentication requests from another within 10 s. The alarm names whatever addresses were used. | Kick the pilot with one deauth and join more than 5 s later, with no data frames heard (`chip::OPEN_LINKS` off). Make the node learn the attacker as the controller first: join before the pilot ever sends a frame the node hears, then the pilot's reconnect is the "newcomer". Keep the table full: 4 forged drone networks kept "busy" stop a newly arriving drone from being followed for up to a minute, and then cost one displacement per 30 s; forged networks with long history (beaconing for minutes, controller traffic) rank like a real drone. Forge a restart (beacons on a new, later clock) while the real drone's own beacons are not heard: it is believed after 3 beacons over 1 s if the drone was unheard for 1 s before (jamming it, or the node off its channel), or after 10 s of the forged clock alone; that wipes the deauth count and the controller as before. |
| `drone_net_clone` | Two beacons of a drone's name from a second address while the drone is heard; or 3 beacons of a precise maker name (`Spark-` + 6 hex) from a non-maker address with no drone around; or (WP-11a) 2 beacons with a drone's own address and a timestamp off its clock, between its own. | A copy on the drone's own address and channel that also copies its beacon timestamps. A copy heard before the original swaps the roles in the alarm. |
| `control_contested` | A made-up ExpressLRS UID: syncs from one transmitter for 5 s (history), then a second nonce phase alternating with the first, three syncs each (about a dozen CRC-valid syncs over 6 s). At most 2 such new alarms at once, then one per 10 s, per node. | Attack a link the node first hears during the attack (no history: counted, not reported). Follow the victim's nonce and timing exactly (no contradiction to see). Attack while the node scans other bands. Keep the node's 8 link slots full of links holding evidence (two packets each, refreshed every 2 minutes) so the victim's link is never admitted; its first packet can also be pushed out by 7 new UIDs before its second. |
| `mavlink_attack` | One checksum-valid frame of a one-message kind raises a **suspicion** (no alert); two different frames (or one frame with a claimed system ID that has 5 s of history, e.g. the real vehicle's ID 1) raise the alarm, with any system and component IDs the forger chooses. New alarms per link: 3 at once, then one per 10 s. | Encrypted links and signatures are not readable. A payload no check covers (see the kinds table). An attack sent while the new-alarm budget is spent by forgeries is reported late (it waits for a token) or, if it is one frame, may never be. A frame split across packets on a new stream while every stream slot holds half a frame for 5 s is seen only if whole in one packet. A ground station stricter than the C library about lengths is not modelled (kind 19 still flags the frame). |

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
  checks in docs/SPOOFING.md already flag it); several jumping together is
  the area. But a drone's position is only what its broadcast claims, and
  anyone can broadcast (WP-11b): only drones the hub has confirmed, whose
  own transmitter moved and stayed moved, count; and they must be
  separate transmitters, heard differently. On their own it takes three
  of them; two are enough when aircraft jump too; one when the hub's own
  GPS agrees.
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

1. **3 or more independent drones** made path jumps "alike"
   (`DRONE_MIN_JUMPERS_ALONE`); or
2. **2 or more independent drones** did, and at least one aircraft jumped
   in the same window (`DRONE_MIN_JUMPERS`); or
3. **3 or more aircraft** jumped alike; or
4. **1 drone** made a path jump while the hub's own GPS says spoofing (two
   receivers moved at once, and the attacker cannot reach the hub's own
   receiver with forged Remote ID frames).

A **drone path jump** (WP-11b, `claims.rs` `Claims::path_jumps`) is a
jump by *one consistent transmitter*: on a **confirmed** track (heard again
a second after it was first heard, docs/SPOOFING.md "Floods"), a report
further off the track's path than any drone flies is held as a rival claim
(docs/SPOOFING.md "Impersonation and disputes"); it becomes a path jump
when the moved claim comes from the track's own transmitter address, forms
its own path (3 reports over 2 s), the old path has not reported since,
and nothing about the track is disputed (or, for a DJI record with no
address, when the track follows the moved claim after 10 s). The position
must be at least 60 m from where the track was shown. Not counted, ever:
jumps of **unconfirmed** tracks (one cheap transmitter can make those by
the hundred), **rival claims** while the old path is still reported (an
impersonator), and **disputed** tracks.

Two drone jumpers are **independent** when they have different
transmitter addresses, were heard differently (different sets of
receiving nodes, or latest signal strengths at least **6 dB** apart), and
were first heard at least **2 s** apart. One radio sending several
identities fails at least one of these unless its operator varies the
power per identity and staggers their start, and then still needs three
identities, all confirmed and kept alive, to raise the alarm with nothing
to back it. An **aircraft jump** is a position more than
**2 km** off where the aircraft's previous position, speed and track put it
(positions at most 30 s apart). Two jumps are **alike** when their
displacements differ by at most 25 % of the larger one or a floor (75 m
for drones, 1 km for aircraft) — a spoofer that drags receivers moves them
all about the same way — *or* when they landed within 150 m (drones) / 3 km
(aircraft) of each other — a spoofer that broadcasts one fixed position
puts them all on the same spot.

**Confidence** (the alarm's `level`, 0-100): jamming from aircraft 50,
+10 per dropped aircraft beyond 2 (to 80); from one aircraft plus the hub
45; +20 when the hub's own GPS agrees. Spoofing from 3 independent drones
(or 2 with aircraft) 60 (+10 each beyond, to 90), 3 aircraft 60, one drone
plus the hub 50; +15 when drones and aircraft both moved; +20 when the hub
agrees. Capped at 95.

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
- **Drone track merges** add up the path-jump counts; the 60 m minimum and
  the need for independent drones keep that from becoming an alarm.
- **A hub that just booted** first hears every drone already flying within
  the same second or two, so they are not "first heard apart" and a real
  spoof in the first minutes after boot needs the hub's own GPS or aircraft
  to agree.
- **Fewer than three drones and no aircraft**: a real spoofer moving one
  or two drones is not raised as area spoofing unless the hub's own GPS
  sees it too (each drone still gets its own position-jump flag).
- **What an attacker can still do** (WP-11b): with three radios (or one
  radio, per-identity power control and patience: three identities first
  heard seconds apart, each confirmed, each heard 6 dB apart, then moved
  together and kept moving), raise AREA FAKE at confidence 60. The alarm
  says drones *claim* to have jumped together; nothing in Remote ID can
  prove it.
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

## Hostile MAVLink telemetry

`ThreatKind::MavlinkAttack`, detector `crates/mavwatch`, run on the LR2021 node
(`firmware/node-lr/src/control.rs`). Branch `mavwatch`, 2026-11-17. Also run by every Wi-Fi
node on MAVLink sent over open Wi-Fi (branch `wifi-mavlink`, 2026-11-17; see "Over Wi-Fi"
below).

**Status: written and host-tested (31 mavwatch tests, plus tests in ctrlwatch, tracker and
evidence); both firmware images build; never run against a real telemetry radio.**

### In plain language

Many drones (ArduPilot, PX4) talk to the pilot's laptop or tablet over a telemetry radio
(SiK/RFD900, or mLRS) using a language called MAVLink. Security researchers have shown in 2026
that the *drone side* of that conversation can attack the *laptop*: a drone, or anyone
transmitting on its radio link, can send messages that make common ground-station programs
(QGroundControl, Mission Planner, MAVSDK, mavros) overwrite files, open malicious links,
fill the disk or crash. Other published holes go the other way, letting someone on the link
open a command shell on the drone.

When the operator has turned on `control content on` (see docs/CONTROL-LINKS.md, "The two
settings"), the LR2021 node now reads the MAVLink on the telemetry links it hears and looks for
those attacks. It raises an alarm called **"Hostile drone telemetry"**, with a number saying
which attack it looks like (the table below) and a short excerpt (for example the poisoned web
address).

**What it can tell you**

- That someone on a drone's telemetry link sent bait for a known ground-station exploit (a
  booby-trapped file address, a fake web link, a poisoned file listing, impossible settings).
- That someone is trying to reach a drone's command shell.
- With `control ids on` as well: that a drone's link has a second, uninvited transmitter on it
  (two senders using one drone's identity, commands without the signature the real pilot uses,
  or old signed messages being played back).

**What it can't tell you**

- Whether the attack worked. It sees the bait, not whether a ground station took it.
- Anything on encrypted links (SiK with AES, mLRS with encryption on).
- Attacks carried over the internet rather than the radio (a normal-looking web address that
  serves a harmful file).
- Who is transmitting. MAVLink "system IDs" are claimed, not proven.

**How it avoids false alarms**

- Only messages that pass MAVLink's own checksum are looked at, so radio noise never counts.
- The single-message alarms look for things a working drone never sends, such as a camera
  description at a `file:///` address or a file listing entry named `../../x`.
- Things a fault could cause (a garbled status message, jumps in message numbering) need
  repeats within a time window before they count.
- A packet the radio repeats is counted once.
- The link-integrity checks only run when the node can tell links apart (`control ids on`):
  otherwise two honest drones flying nearby, both with the default ID 1, would look like an
  intruder.
- **Your own console counts.** If you use your ground station's MAVLink shell or "serial
  passthrough" over the telemetry radio, kind 13 fires. Mute it ("mute alarms" on the
  screen, or the site line `mute mavlink_attack source=00:00:00:00:ff:be` for a ground station
  with system ID 255, component 190): the rule silences that sender's MAVLink alarms.

### The setting it needs

**Off unless `control content on`.** The scan reads message content, so it obeys the same
opt-in as decoding positions and telemetry (docs/CONTROL-LINKS.md). Kinds 16-18 also need
`control ids on` on the LR2021 node (not over Wi-Fi: see "Over Wi-Fi"). Nothing about the scan
is transmitted; the nodes stay receive-only.

### Kinds (`detail` on the threat)

| detail | name | what it means | fires on | source |
|---|---|---|---|---|
| 1 | URI traversal | a camera definition / component metadata / video URI with `../`, `..\` or percent-encoded forms (`%2e%2e/`, `%252e`, `%c0%ae`) | one message | QGC-01, QGC-02, MAVSDK-02 |
| 2 | URI scheme | such a URI with a scheme off the field's list (`file://`, `qrc:`; allowed: `http`, `https`, `mftp`, `mavlinkftp`, for video also `rtsp(s)`, `udp`, `tcp`, `rtp` or a port number), a bare absolute or UNC path, or control characters | one message | QGC-02, MAVSDK-01 |
| 3 | camera name | CAMERA_INFORMATION vendor or model name containing `/`, `\`, `..` or control characters (QGC builds a file path from them) | one message | QGC-01 |
| 4 | video pipeline | VIDEO_STREAM_INFORMATION URI starting `gst:` or containing ` ! ` (GStreamer element syntax) | one message | MP-02 |
| 5 | status link | STATUSTEXT containing Mission Planner's `[link;target;label]` markup, also when split across two messages | one message | MP-03 |
| 6 | status control | STATUSTEXT with control characters (terminal escapes, etc.; tab/newline/CR allowed) | 2 messages within 60 s | DRONEKIT-01 (text handed to apps), defensive |
| 7 | FTP traversal | a MAVLink FTP directory listing whose entry name contains `/`, `\` or `..` (`.` and `..` as whole entries are normal) | one message | QGC-03, MP-01, MAVROS-02 PoC |
| 8 | FTP offset | a MAVLink FTP read acknowledgement at offset 0xF000_0000 (3.75 GiB) or more | one message | QGC-03 |
| 9 | FTP malformed | FTP size field over 239; open acknowledgement longer than 4 bytes; write acknowledgement claiming over 239 bytes written; request path of 200 bytes or more | one message | MAVROS-02; OWASP (bound FTP paths) |
| 10 | rally/fence count | PARAM_VALUE `RALLY_TOTAL` or `FENCE_TOTAL` not a whole number 0-255 (NaN, 3.5, 1e30, -1) | one message | MP-04 |
| 11 | parameter flood | about 3,100 distinct parameter names from one system within 10 minutes | threshold | MAVROS-01 |
| 12 | non-finite PARAM_SET | PARAM_SET of a float parameter to NaN or infinity | one message | OWASP (typed-payload validation) |
| 13 | serial access | SERIAL_CONTROL on the air at all (remote shell, PX4 NSH) | one message | CVE-2026-1579 |
| 14 | serial overflow | SERIAL_CONTROL whose `count` exceeds its 70-byte data field | one message | CVE-2026-38971 |
| 15 | signing key on air | SETUP_SIGNING over the radio: the link's secret key in the clear, or someone re-keying the drone. The key is never copied into the alarm | one message | MAVLink signing guide (keys go over USB) |
| 16 | signing downgrade | unsigned commands from a system that sent signed frames on the same link in the last 10 s | 2 within 30 s | CVE-2020-10283 |
| 17 | injected frames | two interleaved sequence-number streams for one system/component | 10 switches between two live streams within 10 s, 5+ frames each | injection (SiKW00F) |
| 18 | replayed signatures | a signed frame whose timestamp is not newer than one already seen on the same signing stream | 2 within 60 s | MAVLink signing spec |
| 19 | odd length (WP-11a) | a checksum-valid frame of a known message with a payload length no conforming sender uses: longer than the message (either version), a MAVLink 1 frame not exactly its length, an empty MAVLink 2 payload. The frame is also analysed as a lenient parser decodes it (below) | one message | defensive |

**Lengths (WP-11a).** The MAVLink C library built without
`MAVLINK_CHECK_MESSAGE_LENGTH` (its default) and pymavlink accept a frame of
any payload length whose checksum (with the message's CRC_EXTRA) is right,
and decode it by copying at most the message's length and zero-filling the
rest. mavwatch rejected such frames as not frames at all, so an attack
payload in an over-long or short frame reached the ground station unseen.
Now every length parses, the payload is analysed cut or zero-filled the way
those parsers do, and the odd length is itself kind 19. **Not verified per
ground station**: QGroundControl, Mission Planner, MAVSDK and MAVROS were
not tested; some may be stricter (then kind 19 flags a frame they drop, which
is still a hostile frame). A message that gains extension fields after the
table was built (mavlink/mavlink commit 2a8f8cf) would make a newer
autopilot's frames read as over-long: update the table.

**One frame is a suspicion (WP-11a).** Every field of a MAVLink frame is
the sender's claim and a checksum is not a signature, so one checksum-valid
frame used to raise a full alarm with whatever system and component the
forger wrote. Now the one-message kinds (all but 6, 11, 16-18) are reported
with `detail` bit 24 set (`mavwatch::analyse::DETAIL_SUSPECT`,
`link::MAVLINK_SUSPECT`; the kind stays in the low byte) unless backed:

- the same finding from the same claimed sender again, in another frame,
  within 60 s (`CONFIRM_HITS` = 2; a byte-identical resend is one frame);
- another finding on the same link, from another frame, within 60 s;
- or the claimed system has a history on the link: 5 frames over 5 s
  before (`HISTORY_FRAMES`, `HISTORY_MS`). A real vehicle or ground station
  sends the bait once, after its telemetry; that is a full alarm at once.

A suspicion that becomes backed is reported again at once, without the bit.
The hub lists a suspicion ("Possible hostile drone telemetry (one frame)"),
keeps it in the same table entry as the backed alarm, and does not alert on
it; a backed alarm is never demoted.

**New alarms are budgeted (WP-11a).** A new alarm (a kind from a claimed
sender not reported before) costs a token: per link 3 at once then one every
10 s, and for the whole scan 6 at once then one every 5 s
(`NEW_ALARM_BURST`, `NEW_ALARM_REFILL_MS`, `NEW_ALARM_BURST_ALL`,
`NEW_ALARM_REFILL_ALL_MS`). Before, a forger inventing system IDs got one
alarm per frame. Held-back alarms are counted.

**Full tables (WP-11a).** The scan's tables are small (12 senders, 2
parameter-name sets, 16 reports, 4 reassembly streams). A new entry is on
probation until it holds evidence (a sender heard twice, a name set of 8
names, a stream holding half a frame); a newcomer takes a probation slot,
else one holding evidence that has been quiet (30 s senders, 60 s name sets,
5 s streams), else none: the frame is still analysed without that table's
state (a stream's packet as whole frames only) and the refusal counted.
Report entries are made only for an alarm that got a token. Before, a spray
of made-up system IDs or stream keys between the victim's frames reset the
victim's sequence tracks, name count or half frame before any threshold.
Counters: node-wifi's stats `mavp` = `[alarms held back, entries holding
evidence given up, newcomers turned away, streams turned away]`; node-lr's
`ctrl_pressure` line `mav` = `[alarms held back, entries holding evidence
given up, newcomers turned away, streams turned away, streams replaced]`.

"Commands" for kind 16: SET_MODE, PARAM_SET, MISSION_COUNT, MANUAL_CONTROL,
RC_CHANNELS_OVERRIDE, MISSION_ITEM_INT, COMMAND_INT, COMMAND_LONG, FILE_TRANSFER_PROTOCOL,
SERIAL_CONTROL, SETUP_SIGNING.

### Thresholds and why

Every number is a constant in `crates/mavwatch/src/analyse.rs`, documented there.

- **One message is enough** for kinds 1-5, 7-10 and 12-15: each looks for a shape that a
  working autopilot, camera or companion computer does not send. What *is* normal was taken
  from the field documentation and from real firmware: PX4 publishes its component metadata as
  `mftp://[;comp=1]/etc/extras/component_general.json.xz` (an absolute path inside `mftp://`
  is fine), cameras use `http://` or `mftp://` definitions, ArduPilot's `RALLY_TOTAL` and
  `FENCE_TOTAL` are 8-bit parameters.
- **Status text with control characters: 2 in 60 s.** One garbled message from a buggy
  component is the "benign oddity"; a run of terminal escapes is deliberate.
- **FTP offset 0xF000_0000.** FAT32, the SD card format autopilots use, cannot hold a file that
  big; the published proof of concept uses 0xFFFF_FFF0.
- **FTP path 200 bytes.** Real autopilot paths are short (`/APM/LOGS/00000042.BIN`); ArduPilot's
  FTP data field is 239 bytes.
- **Parameter flood: ~3,100 names in 10 minutes** (2,600 bits set in an 8,192-bit set per
  system). ArduPilot has about 1,300-1,800 parameters and PX4 about 1,100; a re-download on
  reconnect repeats the same names and adds nothing.
- **Signing downgrade: 2 unsigned commands in 30 s, with signed frames in the last 10 s.** An
  operator who switches signing off stops sending signed frames; 10 s later their unsigned
  commands no longer count.
- **Injected frames: 10 switches in 10 s, 5 frames on each stream; a stream silent for 2 s is
  dropped.** A lossy link or a reboot causes one or two switches; an injector running alongside
  the real sender causes one per injected frame.
- **Replay: 2 in 60 s.** Byte-identical frames within 3 s are one frame heard twice (SiK resends
  unacknowledged packets), so a replay faster than 3 s is not seen.
- **Reports**: a finding that continues is reported again at most every 2 s (the hub's threat
  stays active for 10 s after a report).

### How it works

- `crates/mavwatch` (no_std, heapless, about 8 KB fixed): a CRC-checked MAVLink v1/v2 scanner
  for 27 messages (CRC_EXTRA computed from mavlink/mavlink `common.xml`, commit 2a8f8cf, and
  checked against pymavlink's published values), signed-frame parsing, reassembly of frames that
  span radio packets (`Watch`), and the analyser. `ctrlwatch::mavlink` now uses the same
  scanner.
- `ctrlwatch::serial_bytes`: the MAVLink bytes of one SiK packet (de-whitened, CRC-checked, TDM
  trailer and command packets removed) or one mLRS frame (unencrypted, with its direction).
- **Node, not hub.** The hub only receives a packet's first 96 bytes, at most 20 packets a
  second; most of what the scan needs (camera and video URIs, FTP, any frame split across two
  SiK packets) never reaches it whole. The node sees every packet in full. The scan runs inside
  the node's existing control scan, only with content decoding on.
- **Link IDs.** With `control ids on`, each SiK NET ID / mLRS sync word gets its own stream and
  the integrity kinds run. The ID stays in the node as a stream key; it is not sent with the
  alarm.
- **What reaches the hub**: a `Frame::Threat` (`MavlinkAttack`, `detail` = kind, plus bit 24
  for a suspicion (see "One frame is a suspicion"), `source` =
  `00:00:00:00:<sysid>:<compid>` of the sender, `target` the same for the addressee when the
  message names one, `ssid` = the excerpt, `freq`/`level` = the packet's frequency and signal),
  then a `Frame::MavlinkEvidence` with the MAVLink frame itself (up to 255 bytes), which the hub
  logs raw as evidence (frame kind `MAVLINK`, 10). The node also prints a
  `{"t":"mavlink_attack"}` line on its own console.
- **Hub**: the threat table keeps one alarm per sender and kind (`crates/tracker/src/
  threats.rs`); a mute silences that sender. No new hub code runs per packet, so the P4 stack is
  unchanged.
- **Builds (release, 2026-11-17)**: hub-p4 stack region 71,984 bytes before and after (.bss
  unchanged). node-lr: RAM +8.5 KB (the `Watch` sits in the control scan's static state; stack
  region 201,464 -> 192,960 bytes), code +17 KB.

### Over Wi-Fi

**In plain language.** Not every drone uses a telemetry radio. Cheap Wi-Fi drones, and many
home-built ones with a small ESP8266 or ESP32 "Wi-Fi telemetry bridge", carry the same MAVLink
conversation over Wi-Fi to a phone or laptop running QGroundControl. When that Wi-Fi network has
no password (very common for these bridges and toys), anyone nearby can read it, and anyone
nearby can join it and send their own messages: exactly the attacks in the table above, without
even needing a radio. With `control content on`, every Wi-Fi node now reads MAVLink it hears on
open Wi-Fi and raises the same alarm, shown as **"Hostile drone telemetry over Wi-Fi"** with the
Wi-Fi channel. Networks with a password are encrypted and are never looked into.

With the setting off (the default), the Wi-Fi nodes do not look inside anyone's Wi-Fi traffic
at all: each data frame costs one on/off check and is gone.

**How the setting reaches the Wi-Fi nodes.** The hub already sends each node its "plan" (which
channels to visit, `Command::Assign`) every 5 seconds through the gateway or the wired bus, and
the LR2021 nodes already get the two settings as an extra plan entry
(`AeroMode::ControlFlags`, code bit 29). Wi-Fi plans now end with the same entry, by the same
rule: only while a setting is on (`coverage::Coordinator::run`). Nothing new on the wire (link
protocol 13 notes in `crates/link`): no new command, no gateway or bus change for delivery, and
with both settings off every plan is byte-identical to before. Consequences:

- **A change goes out at once**: a different plan gets a different plan id, so the hub sends it
  in its next run (within 2 s) instead of waiting for the 5 s resend.
- **A node that restarts gets it again** within 5 s (the resend), and until then the scan is
  **off**: a node never takes the setting from a plan it stored before a restart
  (`role::init`), only from the hub.
- **Off is the default everywhere**: no entry = off; a plan the node rejects leaves the setting
  as it was before that plan.
- The node takes the entry out of the plan before hopping (`link::plan_control_flags`,
  `role::plan_from`); a plan holding only the entry is the built-in plan.
- A Wi-Fi node built before this would reject a plan carrying the entry ("bad channel") and keep
  its previous plan; protocol 13 is unreleased, so update Wi-Fi nodes with the hub.

**What the node looks at** (`crates/wifiwatch/src/mavwifi.rs`, host-tested):

1. *In the radio's receive callback*, only while the setting is on: the 802.11 header (data
   frames with a body; QoS or not; all four ToDS/FromDS address layouts; the HT Control field),
   then LLC/SNAP, IPv4, UDP or TCP. Passed over: protected (encrypted) frames, null frames,
   802.11 fragments, A-MSDUs, anything not IPv4, fragmented IPv4, anything not UDP or TCP.
2. Still in the callback, the "might be MAVLink" test. To or from a usual MAVLink port (UDP or
   TCP 14550, 14555, 14540, 14580, 5760): yes, whatever the bytes (a bridge that forwards raw
   serial chunks splits frames anywhere). Any other UDP port: only if the datagram starts with a
   plausible MAVLink header whose frame fits in it. Other TCP ports: no. The port is only a
   hint: what is queued is analysed only as far as it holds frames whose MAVLink checksum
   verifies, so a non-MAVLink datagram on port 14550 costs a little time and raises nothing.
3. **Rate limit**: a token bucket, on average **100 datagrams a second, bursts of 10**
   (`mavwifi::PER_S`, `BURST`). Then the payload (up to 560 bytes) is copied into one of **4
   slots of its own**, separate from the 32 management-frame slots Remote ID uses. MAVLink can
   therefore never take a slot from Remote ID, and never shows in the node's `drop` / `qmax`
   counters (which count only those 32 slots). A datagram past the rate limit or with all 4
   slots busy is dropped and counted (`mav` in the node's stats line: `[on, candidates,
   rate-limited, no slot, analysed, findings]`). A busy bridge sends a few dozen datagrams a
   second; past 100/s the scan misses part of the stream rather than slowing anything else.
4. *In a task* (`mavlink` in `firmware/node-wifi/src/main.rs`): `mavwatch::Watch`, exactly as on
   the LR2021 node. A payload longer than 560 bytes is analysed without reassembly as far as it
   was kept.

**The link-id rule** (what kinds 16-18 compare). Bytes are reassembled per direction of each
UDP/TCP flow (network, addresses, ports), so bytes from two senders are never joined. The
*link* is the **network**: its BSSID (for a four-address mesh frame, the transmitter address).
That is the Wi-Fi equivalent of a SiK NET ID: an intruder who joins a drone's open network and
injects as its ground station is on the same link as the drone, and the injected-frames check
(kind 17) sees two sequence streams for one system and component; two drones on two networks,
both with system ID 1, stay apart. Unlike a NET ID, a BSSID is not something the owner has to
opt in to read: it is in every beacon, and the node's other Wi-Fi detectors (drone hijacking,
evil twins) key on it already. So **over Wi-Fi kinds 16-18 run with `control content on`
alone; `control ids` is not needed**. Two vehicles with the same system ID on *one* network
look like one sender with two streams: that is a real MAVLink conflict on that network and is
reported (mute it if it is expected).

**TCP 5760** is scanned segment by segment through the same per-flow reassembly, without TCP
reassembly: a lost segment loses the frames it cut (they fail their checksum), a retransmitted
segment repeats frames the analyser already counts once (3 s resend rule), and segments are
rarely reordered over one Wi-Fi hop. A full TCP reassembler would need a window of buffer per
connection for what is a minority case (simulators and some bridges' TCP server).

**What reaches the hub**: the same `Frame::Threat` (`MavlinkAttack`) + `Frame::MavlinkEvidence`
pair as from the LR2021 node, with the same `detail`, `source`, `target`, `ssid` and raw frame,
except that `channel` is the Wi-Fi channel (an LR2021 node sends 0) and `freq` its centre
frequency (100 kHz units, 2437 MHz = 24370), `level` the frame's signal. No MAC or IP address is
sent with the alarm: `source` / `target` remain the MAVLink system/component, so a mute rule
works the same for both radios. The hub tables, logs and mutes it exactly like an LR2021
finding (one per node, sender and kind); `channel != 0` makes the label "Hostile drone
telemetry over Wi-Fi" on the screen, the console and the evidence log's threat text, and the
JSON threat line carries `"via":"wifi"` (`"radio"` for SiK/mLRS) besides `"ch"`. The node also
prints `{"t":"mavlink_attack",...}` and a `{"t":"mavlink","on":true|false}` line when the
setting changes. The ESP-NOW gateway no longer takes a `MavlinkEvidence` frame as a sign that
the sender is an aero node (it would have sent Wi-Fi nodes time syncs they ignore).

**Memory (release builds, 2026-11-17)**: the `Watch` (8,240 bytes) lives in the `mavlink`
task's static state and the 4 slots take 2,432 bytes. ESP32-C5: .bss 60,672 -> 71,448 bytes,
.data 12,196 -> 12,260, stack region 206,280 -> 195,448, code +34 KB. ESP32-S3: .bss 49,120 ->
59,904, .data 14,848 -> 14,920, stack region 235,008 -> 224,152, code +32 KB. The heap is
unchanged. hub-p4 stack region 71,936 bytes before and after.

**Tests** (`crates/wifiwatch/src/mavwifi/tests.rs`, 12): extraction from QoS and non-QoS
frames, with and without HT Control, all four address layouts, a trailing FCS; protected,
null, fragmented (802.11 and IP), A-MSDU, ARP, IPv6 and ICMP frames passed over; truncated
datagrams flagged; the queue filter (ports, plausible headers, MAVLink 1, DNS and a QUIC-like
0xFD byte refused, TCP only on MAVLink ports); the rate limit; a QGC-01 camera payload in an
open data frame giving a `MavlinkAttack` with its raw frame; the same frame split over two
datagrams, over three TCP segments, and with another flow's bytes in between; a minute of benign
telemetry, two drones with system ID 1 on two networks, and a protected exploit frame staying
quiet; an injector posing as the ground station on the drone's network caught (kind 17), the
same on another network not. Plumbing: `link` (`protocol_13_wifi_plan_flags`: v12 bytes
unchanged, the entry is never a channel), `coverage` (`wifi_plans_carry_control_flags`: off by
default, a change is a new plan id sent at once, a restarted hub sends the same plans,
`wifi_plans_leave_room_for_flags`), `tracker` (`mavlink_attack_over_wifi`).

**Needs hardware**:

- Whether the ESP32-S3 / C5 promiscuous callback delivers unprotected data frames of other
  networks in full (length, FCS) as the code assumes, at the rates seen near a busy bridge.
- Real bridges (mavesp8266, DroneBridge for ESP32, ArduPilot's own ESP32 boards) and Wi-Fi toys
  that speak MAVLink: their datagram sizes, whether they cut frames across datagrams, and
  whether 100 datagrams a second keeps up.
- CPU cost of the callback check with the setting on, on a channel full of open-network
  traffic; and that Remote ID counts (`rid`, `drop`, `qmax`) do not change with the setting on.
- The node hops channels: a drone's network is only heard while the node is on its channel
  (2.4 GHz node: channel 6 about three quarters of the time with the default plan).

### Sources

- infected-drones (github.com/nicholasaleks/infected-drones), finding READMEs and PoCs read for
  exact payload shapes: QGC-01, QGC-02, QGC-03, MP-01, MP-02, MP-03, MP-04, MAVSDK-01/02/03,
  MAVROS-01, MAVROS-02, DRONEKIT-01. The PoCs were read, never run.
- OWASP drone security top 10 (2026 revision): CVE-2026-1579 (PX4 SERIAL_CONTROL shell without
  signing), CVE-2026-38971 (ArduPilot SERIAL_CONTROL out-of-bounds read), CVE-2020-10283
  (MAVLink signing downgrade); the recommendations to reject non-finite PARAM_SET values and
  bound FTP path lengths.
- MAVLink message definitions (mavlink/mavlink `message_definitions/v1.0/{minimal,standard,
  common}.xml`), the MAVLink FTP and signing guides on mavlink.io.

### Not verified (needs hardware)

- Everything on air: no real SiK or mLRS link carrying MAVLink has been through the node.
- Whether SiK packets on air split MAVLink frames the way the reassembly assumes (it handles
  any split, and loses a frame when a packet in the middle is missed).
- With `control ids` off, SiK uplink and downlink packets of every link share one stream; a
  frame split across packets from two directions is lost (never misread: the checksum fails).
- How often real telemetry trips the thresholds: they come from the documented behaviour of
  ArduPilot and PX4 and from synthetic tests, not from recordings. In particular kind 6 on
  third-party components with sloppy text, and kind 17 on very lossy links.
- No ground station was attacked to confirm that the frames these tests build are the ones the
  PoCs send byte for byte; they were built from the READMEs' field values.
- Not covered: decompression bombs behind normal-looking `.xz` URIs (MAVSDK-03), malicious
  DataFlash logs (QGC-04), anything delivered over IP rather than the radio (files fetched by
  the ground station; MAVLink itself over open Wi-Fi is now covered, see "Over Wi-Fi"), MAVLink
  over encrypted Wi-Fi, and A-MSDU-aggregated frames.
