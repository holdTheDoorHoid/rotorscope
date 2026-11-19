# Attacks and interference

**Level: 🟢 Beginner, then 🟡 Intermediate** · The attacks and interference Rotorscope watches
for, how it notices each, what has to be switched on, how far each is proven, and which deep dive
to read. Every alarm type is listed in one table at the end.

## 🟢 In plain words

Drones are not the only thing in the air. Someone may be faking drones to confuse a detector,
attacking the Wi-Fi around you, jamming the radio bands, trying to take over a drone, or
interfering with GPS. Rotorscope's listeners hear enough to notice many of these. Terms you may
not know are in the [glossary](../project/glossary.md).

> [!IMPORTANT]
> These are **detectors, not defences**. Rotorscope reports and logs; it never blocks,
> jams, disconnects or transmits at anything. An alarm is a claim based on what the radios heard,
> and anyone with a transmitter can try to cause one. Every alarm is written to the
> [evidence log](../EVIDENCE.md) so it can be checked later.

On the hub's screen, protocol attacks raise the boxed word **ATTACK** in the status bar, jamming
raises **JAMMED**, a flood of fake identities raises **FAKE IDS**, and GPS interference in the area
raises **AREA GPS JAM** or **AREA GPS FAKE**. What each one means on the screen, and how to mute a nuisance alarm, is on
[Alarms and warnings](../use/alarms.md). Jamming, a blinded node, area GPS, ID floods and forged
link traffic can only be snoozed for five minutes, never muted for good
([Site settings](../SITE.md)).

**How far this is proven, as of 2026-11-18:** almost all of it is **simulated only**: tested with
made-up signals on a computer, and in some cases with fake frames injected inside a board on the
bench. No real attacker, jammer or spoofer has been near a Rotorscope unit. The one exception is
the check on Rotorscope's own board-to-board link, which has been **proven on the air** with a
bench-only forging build.

## 🟢 The families

### Spoofed and fake drones

**The attack.** Remote ID and DJI DroneID carry no proof of who sent them. Cheap ESP32 and ESP8266
programs put invented drones on the air, and a forger can copy a real drone's serial number and
send it with a false position.

**How Rotorscope notices.** The hub runs 23 consistency checks on every drone (does it move faster
than any drone flies, is it far louder than its claimed distance allows, does its message counter
never change, and so on) and gives each track a confidence score. A frame that does not fit a
drone's established path is held as a rival claim instead of moving the drone, and two paths for
one ID become a **dispute** shown on screen. It also follows a drone that changes its ID.

**Needs:** nothing switched on. Telling the hub where each node stands sharpens the
signal-strength check ([Site settings](../SITE.md)). **Proof: simulated only.**
**Deep dive:** [Spoofed drones](../SPOOFING.md).

### Floods of fake identities

**The attack.** Instead of one convincing fake, send hundreds of new fake drones a second to bury
the real ones and fill the hub's tables.

**How Rotorscope notices.** A new identity has to earn its place: a drone is only announced once it
has been heard twice at least a second apart, and a busy table never throws out a confirmed drone
that is still transmitting. When new identities arrive far faster than a real sky produces, the
hub raises the flood alarm (`id_flood`; **FAKE IDS** on the screen), for drones, aircraft or control links. It also notices one
node discarding a flood.

**Needs:** nothing. **Proof: simulated only** (host tests, and 2026-11-18 bench runs in which a
node injected fake frames inside itself). **Deep dive:** [Spoofed drones](../SPOOFING.md), "Floods".

### Wi-Fi and Bluetooth attacks

**The attack.** Kicking people off their Wi-Fi with forged disconnect messages, flooding the air
with fake networks, setting up a copy of a real network to catch phones (an "evil twin"), answering
for any network a phone asks about (Karma), forcing devices to another channel, flooding logins or
probe requests, or spamming Bluetooth pairing pop-ups. Tools like a Wi-Fi Pineapple, an ESP32
Marauder or a Pwnagotchi do these, and some have their own recognisable signature.

**How Rotorscope notices.** The Wi-Fi nodes count disconnect, login and probe frames from their
headers (without letting a flood crowd out Remote ID), and watch beacons for floods, twins and
known tools. The Bluetooth node counts pairing pop-up adverts from new addresses.

**Needs:** a Wi-Fi or Bluetooth node; nothing switched on. Transmitter and victim addresses are
shown only on a hub built with `show-link-ids`; network names are shown. **Proof: simulated only.**
**Deep dive:** [Wi-Fi and Bluetooth attacks](../WIFI-ATTACKS.md).

### Jamming

**The attack.** Drowning out the 2.4 and 5.8 GHz bands that drone control and video use, with
wideband noise, a single tone, a sweeping carrier or pulses. Anti-drone guns and DIY jammers do
this.

**How Rotorscope notices.** A raw-capture C5 checks every dwell against a noise floor that cannot
learn a jammer as "normal", and recognises the four shapes. Above all it **fails loud**: if a
band's floor rises so far that drone links could no longer be heard, it reports **Blinded** with
an estimate of how much detection range is left. The hub also raises Blinded itself when any node
suddenly hears almost nothing it normally hears. It cannot hear 433 or 915 MHz, 1.2 GHz or GPS
jamming; those need other hardware.

**Needs:** a raw-capture C5 (node-sdr, DroneID or FPV job) for the four jamming kinds; the hub's
deaf-node check works with any node. **Proof: simulated only** (synthetic captures; the deaf-node
check was exercised on the bench with an injected flood). **Deep dive:** [Jamming](../JAMMING.md).

### Attacks on drones

Three attacks aimed at someone else's drone rather than at the detector:

- **Wi-Fi drone hijacking.** Toy and cheap drones fly over their own Wi-Fi network. An attacker
  kicks the pilot's phone off and joins in their place, or sets up a copy of the drone's network.
  Once a node recognises a drone's network, it raises **drone kicked off its Wi-Fi**, **drone Wi-Fi
  taken over** or **copy of a drone's network**. *Needs:* a Wi-Fi node; nothing switched on.
- **A second transmitter on a control link.** Hijacks of ExpressLRS, mLRS and SiK telemetry links
  all add a radio the link should not have. The LR2021 node notices two transmitters' clocks or
  packet numbering on one link. *Needs:* an LR2021 node (none built yet); the link's identifier is
  shown only with `control ids on`, and one rule needs `control content on`.
- **Hostile MAVLink telemetry.** MAVLink is the common message language between a drone and its
  ground-control software. Messages on a drone's telemetry that bait known ground-station
  exploits, or try to reach a drone's command shell. *Needs:* `control content on` (some kinds
  also `control ids on`). Read by the LR2021 node, and by Wi-Fi nodes on open Wi-Fi.

**Proof: simulated only** for all three. **Deep dive:** [Attacks on drones](../DRONE-ATTACKS.md);
the two settings are explained in [Control links](../CONTROL-LINKS.md).

### GPS trouble

- **At the hub.** If the hub's own GPS is jammed or spoofed, every distance and every evidence
  timestamp would be wrong. The hub runs fifteen checks on its receiver's reports. When
  spoofing is suspected it **holds its last trusted position and time** rather than follow the fake.
  *Needs:* a GPS module on the hub. **Proof: simulated only** (laptop tests and an on-chip
  imitation of a GPS receiver). **Deep dive:**
  [GPS jamming and spoofing at the hub](../GNSS-INTEGRITY.md).
- **In the area.** Jamming or spoofing affects every GPS receiver in range, so the hub uses other
  receivers as witnesses: several aircraft reporting poor position quality at once, or several
  confirmed, independent drones jumping together. It raises **GPS interference nearby**
  (`area_gnss`). *Needs:* ADS-B aircraft (an ADSBee) or drones in range; the hub's own GPS only adds
  weight. **Proof: simulated only.** **Deep dive:** [Attacks on drones](../DRONE-ATTACKS.md), "GPS
  interference in the area".

### Forged traffic on Rotorscope's own link

**The attack.** Pretending to be one of the kit's own nodes over the air, to inject fake drones or
commands, or replaying recorded packets.

**How Rotorscope notices.** On a sealed kit every packet between a node and the hub is signed with
that node's own key. The hub drops forged, replayed, late or unsigned packets, and packets from
boards still on the old firmware, and raises **forged node traffic** (`forged_link`) naming the
claimed node, with the reason; the gateway, which holds no key, reports the packets it refused
(not in the kit's roster, over its share) for the hub to raise too. Sealed or not, the hub also
refuses any frame of a type its sender may never originate. If a node says it sent far more than
the hub received, the hub raises **node reports going missing** (`link_loss`).

**Needs:** a sealed kit: the kit master made with `link-key kit new` and each board provisioned
with `link-key` (kits ship open). **Proof:** before the 2026-11-18 cut-over the origin check was
**proven on the air** (a bench-only forging node's seven forbidden frame kinds were each refused at
the gateway, which held the kit's one key then, within about a second); the per-node keys, the
hub's checks and `link_loss` are **simulated only** (tested on a computer, built for the boards). **Deep dive:**
[ESP-NOW in depth](../ESPNOW.md), "Securing the link".

## 🟡 Every alarm type

Each kind below is a `link::ThreatKind` (`crates/link/src/lib.rs`): its name as it appears in JSON,
the evidence log and site-file mute lines.

| Kind | What it means | Family | Deep dive |
|---|---|---|---|
| `deauth` | Disconnect frames well above the background rate | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `disassoc` | Disassociation frames, likewise | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `beacon_flood` | Many new networks at once | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `evil_twin` | A known network's name from a second transmitter that does not match it (different security, channel or maker) | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `karma` | One transmitter using many network names | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `channel_switch` | Announcements telling devices to change channel | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `auth_flood` | Login request flood at one access point | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `probe_flood` | Probe request flood | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `attack_tool` | A known attack tool's own signature | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `ble_spam` | Bluetooth pairing pop-up flood | Wi-Fi and Bluetooth | [WIFI-ATTACKS](../WIFI-ATTACKS.md) |
| `jam_broadband` | Noise floor raised across a wide span | Jamming | [JAMMING](../JAMMING.md) |
| `jam_tone` | A strong narrow tone | Jamming | [JAMMING](../JAMMING.md) |
| `jam_sweep` | A carrier sweeping across the band | Jamming | [JAMMING](../JAMMING.md) |
| `jam_pulsed` | Strong pulsed interference, including pulses that flood the DroneID decoder | Jamming | [JAMMING](../JAMMING.md) |
| `blinded` | A node can no longer hear what it normally hears | Jamming | [JAMMING](../JAMMING.md) |
| `forged_link` | Forged, replayed or forbidden traffic on the kit's own link | Own link | [ESPNOW](../ESPNOW.md) |
| `drone_deauth` | Disconnect frames aimed at a drone's own Wi-Fi | Attacks on drones | [DRONE-ATTACKS](../DRONE-ATTACKS.md) |
| `drone_takeover` | A new device joining a drone's Wi-Fi after the pilot was kicked off | Attacks on drones | [DRONE-ATTACKS](../DRONE-ATTACKS.md) |
| `drone_net_clone` | A copy of a drone's network | Attacks on drones | [DRONE-ATTACKS](../DRONE-ATTACKS.md) |
| `control_contested` | A control or telemetry link heard from more transmitters than it can have | Attacks on drones | [DRONE-ATTACKS](../DRONE-ATTACKS.md) |
| `mavlink_attack` | Hostile content on a drone's MAVLink telemetry | Attacks on drones | [DRONE-ATTACKS](../DRONE-ATTACKS.md) |
| `area_gnss` | GPS jamming or spoofing in the area, seen through other receivers | GPS trouble | [DRONE-ATTACKS](../DRONE-ATTACKS.md) |
| `id_flood` | Far more new identities a second than a real sky produces | Floods | [SPOOFING](../SPOOFING.md) |
| `link_loss` | A node says it sent far more reports than the hub received: something is swallowing them | Own link | [ESPNOW](../ESPNOW.md) |

Two families are not threat kinds:

- **Spoof suspicions and disputes** are marks on a drone's own track (a confidence score, a
  "disputed" state), logged as their own evidence events. See [SPOOFING](../SPOOFING.md).
- **The hub's own GPS** has its own verdict (`ok`, suspected or confirmed jamming or spoofing),
  shown on the health page and logged as evidence events 18 and 19. See
  [GNSS-INTEGRITY](../GNSS-INTEGRITY.md).

The hub keeps up to 16 threats at once, and never drops an active jamming, blinded, area-GPS or
contested-link threat to make room for a flood (`crates/tracker/src/threats.rs`).

## Where this lives in the code

- [`crates/link/src/lib.rs`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/crates/link/src/lib.rs):
  `ThreatKind` and `ThreatSighting`
- [`crates/tracker`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/tracker):
  `threats.rs` (the hub's threat table), `spoof.rs`, `claims.rs`, `continuity.rs`,
  `probation.rs` (floods), `area_gnss.rs`
- [`crates/wifiwatch`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/wifiwatch):
  Wi-Fi and Bluetooth attacks, drone Wi-Fi hijacking, MAVLink over Wi-Fi
- [`crates/jamwatch`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/jamwatch):
  jamming
- [`crates/ctrlwatch`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/ctrlwatch)
  and [`crates/mavwatch`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/mavwatch):
  contested control links and hostile MAVLink
- [`crates/gnss`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/gnss): the hub's
  GPS integrity
- [`crates/airlink`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/crates/airlink): the
  signed node link
- Deep dives: [Spoofed drones](../SPOOFING.md), [Wi-Fi and Bluetooth attacks](../WIFI-ATTACKS.md),
  [Jamming](../JAMMING.md), [Attacks on drones](../DRONE-ATTACKS.md),
  [GPS jamming and spoofing at the hub](../GNSS-INTEGRITY.md)

**Next:** [Spoofed drones](../SPOOFING.md)
