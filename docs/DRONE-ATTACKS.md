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
- **Restarts.** A beacon's timestamp is the drone's radio uptime. When it
  says the drone started more than 2 s later than before, the drone has
  restarted: its controller and any newcomer are forgotten, so the phone
  rejoining (even with a new random address) is not a takeover.

### `DroneDeauth`

**Rule:** 3 fresh (not retried) deauth or disassoc frames that involve a
drone's network (its address as BSSID, sender or receiver) within 5 s.
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
minute ago. That is a drone back from a battery swap telling the phone it
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

`source` = the copy, `target` = the original (all zero if never heard),
`count` = frames from the copy, `ssid` = the name. `detail`: bit 0 the copy
is open; bit 1 its security differs from the original's; bit 2 its address
is not the maker's; bit 3 it uses the original's own address; bit 4 it says
it is on another channel; bits 24-31 the maker. Reported at most once a
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
  Mute it at the hub (the mute is per drone).
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
- A copy using the drone's own address on the drone's own channel cannot be
  told apart over the air without timing analysis (not done).

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
