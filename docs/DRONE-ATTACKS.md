# Attacks on drones

Attacks aimed at drones themselves (rather than at the networks around the
detector, which docs/WIFI-ATTACKS.md covers). Each kind of attack has its own
section below.

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
