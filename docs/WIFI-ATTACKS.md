# Wi-Fi and Bluetooth attack detection

Radio nodes are receive-only, but they hear every management frame on their
channel and every Bluetooth advert in range. That is enough to notice
somebody attacking the networks around the detector (or the detector's own
gear), and to notice a drone carrying an attack payload: a Wi-Fi Pineapple,
an ESP32 Marauder or Flipper Zero Wi-Fi board, a Pwnagotchi, a deauther.

The logic lives in `crates/wifiwatch` (no_std, host-tested). `firmware/node-wifi`
and `firmware/node-ble` feed it and send each finding to the hub as
`link::Frame::Threat(ThreatSighting)` (protocol 9) plus a USB JSON line
`{"t":"threat",...}`. Each kind is reported at most about once a second per
target while it lasts; `count` and `window_ms` say how much was heard.

## What each detector looks for

All rates are *as heard*. A hopping node hears its home channel ~75% of the
time and a visited channel ~12%, so a real flood of 100 frames/s on a visited
channel reads as ~12/s; the thresholds leave room for that.

| Kind | Looks for | Threshold | `detail` |
|---|---|---|---|
| `Deauth`, `Disassoc` | Subtype 12 / 10 frames per BSSID; a retransmission (Retry bit **and** the transmitter's previous sequence number) counts once, see below | 30 in 1 s, or 10/s for 2 s in a row, or 10 to broadcast in 1 s | `& 0xFFFF` most common reason code; bit 16 some went to broadcast; bit 17 all were protected (802.11w, reason unreadable) |
| `AuthFlood` | Authentication / (re)association **requests** to one AP | 30/s from 8+ addresses for 2 s in a row, or 100 in 1 s | distinct sources (lower bound) |
| `ProbeFlood` | Probe requests on the channel | 100 in 1 s from 36+ addresses, or 300 in 1 s from anyone | distinct sources (lower bound) |
| `BeaconFlood` | Transmitters not heard in the last 10-20 s, on a channel heard continuously for 4 s | 20 new in 1 s (keeps reporting down to 10) | distinct names among them |
| `Karma` | One BSSID using several non-empty network names (beacons or probe responses) | 5 names within 30 s | names counted |
| `EvilTwin` | An **open** network using the name of a protected network heard recently | protected seen 5+ times in the last 60 s, open copy 2+ times | `(original security << 8) \| 0` (`wifiwatch::mac::Security`) |
| `ChannelSwitch` | CSA (element 37) / extended CSA (60) in beacons, probe responses or action frames | contradicts the same BSSID's plain beacons on that channel (2 + 2 within 3 s); names no valid channel; still going after 10 s and 20 frames; switch count not falling 10 times; or 10 CSA action frames in 1 s | announced channel |
| `AttackTool` | A tool's own signature (table below) | one frame | index into `wifiwatch::TOOLS` |
| `DroneDeauth` | Deauth / disassoc at a drone's own network (`crates/wifidrone`), retransmissions counted once | 3 in 5 s | reason code + flags + maker; see docs/DRONE-ATTACKS.md |
| `DroneTakeover` | A new station joining a drone's network just after its pilot was kicked off, or while the pilot still flies it | 2 frames from the newcomer | what was seen + why + maker; see docs/DRONE-ATTACKS.md |
| `DroneNetClone` | A drone's network name from a second address (or its own address on a second channel), or a drone name from an address the maker does not own | 2 frames (3 without an original) | flags + maker; see docs/DRONE-ATTACKS.md |
| `BleSpam` | Pairing pop-up adverts (Apple Continuity 0x07/0x0F, Samsung 0x0075, Microsoft Swift Pair, Google Fast Pair 0xFE2C) from addresses not heard in the last 5-10 s | 20 new addresses of one family in 5 s (keeps reporting down to 10) | new addresses in the window; `ssid` = family (`apple`, `samsung`, `microsoft`, `google`) |

**Retransmissions.** A real access point kicking a station that has
already gone sends one deauth and then several retries: the Retry bit set,
the *same* sequence number. Those count once. Until WP-11a every frame with
the Retry bit was skipped, which an injector could exploit: a receiver
discards a retry only when its (transmitter, sequence number) equals the last
frame it took from that transmitter (IEEE 802.11 duplicate detection), so a
deauth stream with the Retry bit set and advancing numbers still disconnects
the victim. The detectors now apply the receiver's own rule (a 4-byte entry
per transmitter, two per BSSID: `wifiwatch::util::Dups`), so such a stream
counts in full. Probe requests use the same rule.

Other fields: `source` is the claimed transmitter (all zero when many),
`target` the network or station hit (all 0xFF for broadcast), `level` the
strongest RSSI in the window, `ssid` the network name involved.

### Known attack tools (`wifiwatch::TOOLS`)

Indices travel on the wire: append only.

| # | Tool | Signature | Source |
|---|---|---|---|
| 0 | Pwnagotchi | Beacons from `de:ad:be:ef:de:ad`, or element 222 carrying its JSON advertisement (223 = compressed) | pwngrid `wifi/defines.go`, `wifi/pack.go` (github.com/evilsocket/pwngrid) |
| 1 | Wi-Fi Pineapple | Setup network `Pineapple_XXXX` (4 hex digits); transmitter OUI `00:13:37` (or `02:13:37`) | Hak5 Mark VII docs; Lab401 PineScan write-up |
| 2 | ESP8266 Deauther | Control network `pwned` | SpacehuhnTech esp8266_deauther wiki, Setup |
| 3 | Ghost ESP | Web-UI network `GhostNet` | Ghost ESP zine (Iffy Books); release notes v1.4.9 |
| 4 | RemoteIDSpoofer | Setup network `ESP_RIDS` | jjshoots/RemoteIDSpoofer `frontend.cpp` line 8 (docs/DRONE-ATTACKS.md) |
| 5 | DroneSploit | A drone renamed to `TELLO-abcdef` / `C-me-abcdef` (module defaults) | dhondta/dronesploit `tello.py` line 28, `cme.py` line 47 (docs/DRONE-ATTACKS.md) |
| 6 | Wifiphisher | Rogue AP address `00:00:00:xx:xx:xx` (`DEFAULT_OUI`) | wifiphisher `constants.py` line 50, `interfaces.py` lines 932-943 (docs/DRONE-ATTACKS.md) |

ESP32 Marauder (and the Flipper Zero Wi-Fi dev board, which usually runs it)
has no fixed beacon of its own; its beacon spam, "Rick Roll" list, deauth and
probe floods are caught by behaviour. The lyric list is deliberately not
stored.

Drone hijack tools (SkyJack, DroneJack, Drone-Hacking-Tool, DroneSploit's
attack modules) have no fixed signature of their own; the drone kinds above
catch what they do. See docs/DRONE-ATTACKS.md, "Wi-Fi drone hijacking".

## Node integration

**node-wifi.** The receive callback now counts deauth, disassoc,
authentication, (re)association-request and probe-request frames from their
headers (`wifiwatch::floods::Floods`, under a critical section) and **never
queues them**: only beacons, probe responses and action frames (what Remote ID
and the other detectors need) take one of the 32 slots. A deauth flood of
thousands of frames a second costs one short callback each and cannot starve
Remote ID. The `threats` task closes the flood windows four times a second;
the drain task runs `wifiwatch::beacons::Beacons` on queued frames. Once
`Beacons` has recognised a drone's network, the callback also hands
deauth / disassoc, authentication, (re)association requests and responses
and data-frame addresses to `wifiwatch::drones::DroneLinks` (`rx::DRONES`;
association responses are no longer queued). Stats line:
`kill` (deauth+disassoc), `auth`, `probe`, `csa`, `threat` totals, `scout`
`[visits, last channel]`.

**node-ble.** The advert handler (BLE host receive loop) runs
`wifiwatch::ble::classify` on every advert, a walk of at most 31 bytes, and
records pairing-family adverts; Remote ID filtering is unchanged. The stats
task reports once a second. Stats line: `pair`, `pair_new`, `threat`.

**Channels (counter-evasion).** Every Wi-Fi node leaves its plan for 105 ms
every 3 s to visit the next channel of its band(s) the plan skips
(`coverage::plans::scout_channel`): 2.4 GHz 1-14 and 5 GHz 36-177 (DFS and
UNII-4 included). 3.5% of air time. The hub's sweeps also gained 2.4 GHz
channel 14 and 5 GHz 169/173/177. For reception on those channels node-wifi
sets a permissive receive regulatory domain (`chip::RX_ALL_CHANNELS`): manual
policy, 2.4 GHz channels 1-14, every 5 GHz channel. The node still transmits
nothing except, in the `espnow` build, ESP-NOW on 2.4 GHz channel 6 as before.

## Known false-alarm cases

- **Evil twin**: a site that really does run an open network with the same
  name as its WPA2 network (rare; an OWE transition pair is recognised and not
  flagged). It will report once a second for as long as both are on the air;
  mute it for good with one specific line in the site file (its source and
  name, docs/SITE.md), or for two hours on the hub.
- **Deauth**: an access point shutting down several networks at once sends a
  broadcast deauth per network and per client; ten or more from one BSSID in a
  second would trip the broadcast rule. Band-steering access points send
  unicast deauths, which stay far below 30/s.
- **Auth flood**: hundreds of clients rejoining one access point after it
  reboots (a lecture hall) can look like two seconds of flood.
- **Beacon flood**: a node carried into a new place (a moving vehicle) sees
  many new networks; each channel needs 4 s of warm-up after a gap of 5 s, so
  a fast-moving node may still trip it.
- **Karma**: an access point that renames its network is counted once; it
  needs five names in 30 s.
- **BLE spam**: walking into a dense crowd of Samsung phones or Fast Pair
  earbuds; 20 new addresses of one family in five seconds is a lot, but not
  impossible at a stadium gate.
- **Channel switch**: an access point announcing a very long countdown (more
  than 10 s) on a DFS radar event.

## What a forged frame can make an alarm say

Every field of these alarms comes from frames anyone in range can send
(WP-11b). The detectors are right that *frames like these were heard*;
what the frames say about who sent them, and to whom, is a claim:

- **`source` (the attacker).** Deauth, disassociation, beacon, login and
  probe floods, Karma and attack tools carry the transmitter address the
  frame states. It is set by the sender, so it can be a neighbour's access
  point, the venue's router or a phone in the crowd. A deauth "from" the
  cafe's own access point is what an attacker sends to frame it.
- **`target` (the victim).** Equally a claim: a flood "against" a device
  may never have reached it.
- **`ssid` (the network name).** Any name can be beaconed; an evil twin of
  "Cafe WiFi" proves only that someone sent that name from another
  address, possibly to get the real one muted.
- **The alarm itself.** Cheap to raise on purpose, to make an operator
  press Mute while something else is happening. A mute therefore only ever
  covers that one source, victim or name, ends after two hours (snoozes of
  jamming-like alarms after five minutes), and never clears JAMMED
  (docs/SITE.md "Silencing alarms").

### Cheapest forgery, and what still gets past (after WP-11a)

Raising each alarm on purpose (left) and hiding the real attack from it
(right), for someone in radio range who has read this source. Rates are
*as heard* by the node (a visited channel is heard ~12% of the time). The
drone kinds are in docs/DRONE-ATTACKS.md, same heading.

| Alarm | Cheapest forgery that raises it | Known evasions that remain |
|---|---|---|
| `deauth`, `disassoc` | 30 frames in a second at one BSSID, any addresses (or 10/s for 2 s, or 10 to broadcast). Retry bit or not, as long as the sequence numbers move. | Stay under 10 a second as heard: a few frames disconnect one client. Repeating one sequence number with the Retry bit hides frames from the count, but the victim discards them too (only the first acts). A channel the node is not on. |
| `auth_flood` | 100 authentication / association requests in a second at one BSSID, or 30/s from 8+ addresses for 2 s. | Stay under the rates. |
| `probe_flood` | 300 probe requests in a second, or 100 from 36+ addresses. | Stay under the rates. |
| `beacon_flood` | 20 new transmitters in a second on a channel heard continuously for 4 s. | Fewer new names a second; reuse addresses heard in the last 10-20 s. |
| `karma` | Probe responses or beacons with 5 names from one address in 30 s. | Answer with a new address per name. |
| `evil_twin` | A protected network's name beaconed 5 times in 60 s, then an open network with the same name twice: both from the forger, so any name can be accused. | A twin that is not open (same security as the original). |
| `channel_switch` | Beacons with a channel-switch element that contradicts the same address's plain beacons (2 + 2 within 3 s), or names no valid channel. | A switch announced once, briefly, to a valid channel, with no plain beacons to contradict it. |
| `attack_tool` | One beacon with a tool's name or address (`pwned`, `GhostNet`, `de:ad:be:ef:de:ad`, ...). | Rename the tool's network or change its address. |
| `ble_spam` | 20 new addresses of one pairing family in 5 s. | Fewer than 20 new addresses per 5 s; a family not in the list. |

The hub labels these fields as claims: the threat JSON line carries
`"claimed":true` whenever it shows `src`, `target` or `ssid` (addresses
only with `show_link_ids`), the evidence text says `claimed "name"`, and
the evidence text of a mute says `claimed source` / `claimed target` /
`claimed name`. The raw frames in the evidence log are what can be checked
later; the summary is not.

## What needs checking on hardware

1. **Receive regulatory domain.** After boot there must be no `set_country`
   `err` line, and scouting must tune 12, 13, 14 and 52-144, 169-177 without
   `set_channel` errors (`ch_err` stays flat, `scout` advances). If the C5
   driver refuses 169-177, drop them from `SCOUT_5` and `WIFI5_SWEEP`.
2. **Scouting cost.** Remote ID rate on channel 6 / 149 should drop by no more
   than ~4% (`per_ch`).
3. **Floods off the queue.** With a deauth flood on the bench (any second
   board running a deauther in a shielded box, or replayed with `mdk4` on a
   Linux laptop **on your own network only**), `drop` and `qmax` must stay low
   and `rid` must keep counting.
4. **Thresholds in the real environment.** Run a node for a day in a busy
   place and confirm no `threat` lines; then exercise each detector: Marauder
   beacon spam and deauth, a Flipper BLE spam app, a Pwnagotchi, an open copy
   of your own WPA2 network.
5. **Protected frames.** Whether the promiscuous callback delivers PMF
   (protected) deauths with the protected bit intact.
