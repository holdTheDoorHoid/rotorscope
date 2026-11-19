# Site settings: where the nodes are, muted alarms, places to protect

The P4 hub keeps its site settings in one plain text file on its SD card,
`ROTORSCP/SITE.TXT`. You can edit it on a computer, or change it on the hub
itself (console, or "Nodes at hub" on the screen); the hub only ever **adds**
lines to the end, so nothing you typed is lost. Later lines win.

## Where each node stands

The spoofing checks compare how loud a drone is with how far away it claims to
be ([SPOOFING.md](SPOOFING.md)). That works best when the hub knows where each
node is; without it, a node far from the hub can make an honest drone look
suspicious.

* **All nodes next to the hub** (a wired kit, or the bench): on the hub, go to
  Settings, choose **Nodes at hub**, press SEL. The row says "Placed". The hub
  needs its own position first (a GPS fix, or a typed-in position).
* **A node somewhere else:** add a line with its address (shown on the hub's
  Health page) and its position in decimal degrees, latitude first:

      node 10:bd:a3:ce:bd:18 39.95210 -75.16380   # roof, north corner

* **Forget a node's position:** `node 10:bd:a3:ce:bd:18 clear`

## Where the hub itself stands (`home`)

The hub's GPS spoofing checks compare its fixes with *home*, where it
normally stands (docs/GNSS-INTEGRITY.md). By default it learns home from 15
minutes of steady fixes and remembers it on the card. At a fixed site you can
place it instead, which is stronger (a spoofer present when the hub first
learns cannot then become home):

      home 39.95210 -75.16380 12.5   # mast; height in metres above the ellipsoid, optional
      home clear                     # back to learning

A placed home replaces the learned or remembered one at every mount and
cannot be overridden from the screen or console: move the hub, edit the line.
Each change is an evidence "setting" event.

## Silencing alarms

A silenced alarm is still recorded in the evidence log and listed in the
hub's `"t":"threat"` lines (with `"muted":true`), but it no longer raises the
ATTACK badge. **Nothing silenced ever clears JAMMED, AREA JAM / AREA FAKE or
ID FLOOD**: those say the hub may be missing drones, and stay on for as long
as that is true.

Every silence is **one specific thing**: this kind of alarm from this
transmitter, against this victim, with this network name, on this node or at
this frequency, whichever applies to the kind. There are no wildcards: an
empty field means "none", never "any". (Before WP-11b, muting a "node blinded"
alarm muted every blinded node for ever, and a jamming mute covered a whole
band. An attacker could raise cheap nuisance alarms until someone pressed
Mute, and anything real sounding at that moment went quiet with them.)

### On the screen: mute for 2 hours, snooze for 5 minutes

Settings, **Alarms**. The row says what one press will do, and it only ever
acts on the alarms already on the screen when you press it (an alarm that
arrives a moment before you press is not included):

* **"Mute 2 (2h)"**: mutes the nuisance alarms sounding now (Wi-Fi floods,
  fake networks, evil twins, contested control links and so on), each on its
  own, for **2 hours**. Never a jamming or blinded alarm.
* **"Snooze 1 (5m)"**: shown only when the alarms left sounding are the
  safety-critical kinds: jamming, a node blinded, GPS interference in the
  area, a flood of fake IDs, forged node traffic. These can only be
  **snoozed, for 5 minutes**: the alert stops, the JAMMED / AREA / FLOOD box
  stays. If the alarm is still on when the 5 minutes end, it alarms again.
* **"Unmute 3"**: when nothing sounds but you have muted or snoozed something,
  SEL drops all of yours (not the site file's).

Why these numbers: two hours is about one session at a venue, so a nuisance
silenced once stays quiet through it, but a mute tapped in a hurry (or under a
burst of nuisance alarms raised to provoke it) does not last into the next
shift. Five minutes is time to deal with a jammer without the buzzer, short
enough that one still running is announced again. Mutes and snoozes from the
screen live in memory only: they are **not written to the card** and are gone
after a reboot. Each one is logged as an evidence "setting" event saying what
it silences and when it ends.

While anything is silenced the strip along the bottom of every page says so,
for example `MUTED 2 1h59  SNOOZED 1 4m05  SITE MUTES 1`, and the health page
has an ALARMS line ("1 sounding, 3 silenced"). The hub's `{"t":"hub"}` status
line carries `"silenced":N` (alarms sounding but silenced) and
`"mutes":{"muted":N,"snoozed":N,"site":N}` (rules in force).

### In the file: a venue's known false alarm, for good

Some places set off an alarm honestly, for example a cafe that runs an open
guest network with the same name as its secured one (that looks like an "evil
twin"). Mute it for good with one line that names exactly that thing:

      mute evil_twin source=02:11:22:33:44:55 ssid=Cafe WiFi
      mute deauth target=ff:ff:ff:ff:ff:ff
      mute control_contested target=00:00:12:34:56:78 mhz=915.5
      unmute all

The kind names are the ones in the hub's `"t":"threat"` lines. `ssid=` takes
the rest of the line, so put it last. What a line must name:

| Kind | Must name |
|---|---|
| `evil_twin`, `drone_net_clone`, `attack_tool` | `source=` and `ssid=` |
| `karma`, `mavlink_attack` | `source=` |
| `drone_deauth`, `drone_takeover`, `deauth`, `disassoc`, `beacon_flood`, `auth_flood`, `probe_flood`, `ble_spam`, `channel_switch` | `target=` |
| `control_contested` | `target=` (the link identifier: needs `control ids on`) and `mhz=` |
| `jam_*`, `blinded`, `area_gnss`, `id_flood`, `forged_link` | cannot be muted for good: snooze them on the hub |

A `mute` line that names less, or is for a kind that cannot be muted, is **not
honoured**: the hub prints `SITE.TXT line N ignored: mute not honoured: ...`
with the reason, and the load's evidence event counts it. This is also what
happens to the catch-all lines older hubs wrote to the card when you pressed
Mute (`mute blinded`, `mute deauth`, `mute jam_broadband mhz=2437.0`,
`mute area_gnss target=02:00:00:00:00:00`, `mute control_contested mhz=915.5`):
they are dropped, not obeyed. A specific line an older hub wrote (an evil twin
with its source and name, a flood's victim) is kept, for good, like any other.

Addresses and names in an alarm are what the attack's frames claimed, and a
forged frame can claim anyone's ([WIFI-ATTACKS.md](WIFI-ATTACKS.md) "What a
forged frame can make an alarm say"). Check before muting a transmitter for
good: someone who knows the line is there can then use that address.

## Collection switches

      control ids on          # read link identifiers (ELRS UID bytes, SiK NET ID...)
      control content on      # decode sticks, telemetry, positions; keep raw packets

Both are off unless a line says on ([CONTROL-LINKS.md](CONTROL-LINKS.md)).
While one is on, the strip along the bottom of the screen says so
(`ON: control IDs, control content`). (The `audio clips on` switch of the
acoustic branch, not merged here, is shown the same way once it is.)

## Places to protect, and friendly drones

These lines feed the threat levels ([THREAT-PROFILES.md](THREAT-PROFILES.md)):
every drone that reports its position is judged against them, and gets one
level (Notice, Caution, Warning, Critical) with the reasons.

      area house circle 39.95210 -75.16380 150     # name, centre, radius in metres
      area yard poly 39.9520 -75.1640 39.9523 -75.1636 39.9519 -75.1633
      area house ceiling 90                        # highest a drone may fly over it, metres above the hub
      area house clear                             # forget the area
      area here 200                                # (console only) a circle round the hub where it is now
      bubble 300                                   # a circle that moves with the hub's own GPS
      bubble off
      friend 1581F5FKD229400B4WW1 our own Mini     # a drone you know: serial, then a label
      friend 1581F5FKD229400B4WW1 clear

* **Areas.** Up to 8. A name is up to 12 letters, digits, `_` or `-`. A
  circle has a radius of 10 to 10,000 m; a polygon has 3 to 16 corners,
  latitude first for each. Writing an area again replaces its outline and
  keeps its ceiling. The ceiling is 120 m (400 ft) unless a line says
  otherwise. A ninth area is **refused** ("too many areas (8): clear one
  first"): an area is never dropped to make room.
* **`area here 200`** works only on the console (it needs the hub's live
  position, like `node all here`). The hub saves it to the card as an
  ordinary `area here circle LAT LON 200` line, so the place stays put if
  the hub moves later. In the file, `area here 200` itself is ignored.
* **The bubble** is a circle round the hub's current GPS position, so a
  handheld unit protects whoever carries it. It uses the hub's own speed
  and direction: walking past a hovering drone is not "the drone heading
  for you", a drone flying at you is.
* **Friends.** Up to 8. A friend's drone is **lowered, never hidden**: being
  in an area, its pilot at the fence and so on count only as Notice, but
  "heading straight for you" and "fast inbound" are never lowered, because
  a serial number can be faked. A friend line on a swapped card is logged
  like any other change.
* A console line is at most 120 characters, so a polygon with more than
  about five corners belongs in the file (a file line may be 384).

## Several drones at once, and the visit history

Two switches for the rules that look across drones and days (slice 16d):

      crowd 4          # "several drones at once" from 4 drones at the places (default 3)
      crowd off        # don't count drones at once (an event with many friendly drones)
      crowd on         # back to the default, 3
      visits off       # keep no repeat-visitor history, and wipe what is kept
      visits on        # keep it (the default)

* **`crowd`** sets how many drones at or near the places at the same time
  make a Warning for each of them ("one of 4 drones at house at once"). It
  takes 2 to 20, `on` or `off`. **`crowd off` does not switch off the
  jamming part**: any drone at or near a place while the hub hears jamming,
  sees a node blinded or sees GPS interference in the area is still a
  Warning. Friends are never counted.
* **`visits`**: with it on (the default) the hub keeps, on its SD card
  (`ROTORSCP/VISITS.DAT`), which drones and which pilot spots were seen at
  the places, on which days, for **30 days**, so a drone (or a pilot
  standing in the same spot) back on 3 or more days in two weeks is shown as
  a "repeat visitor". It keeps only a fingerprint of the drone's ID (not the
  ID itself) or the 25 m square the pilot stood in, and the dates: no times,
  no paths. Friends are never kept. `visits off` stops it and **wipes what
  was kept**, on the hub and on the card (logged). It needs GPS time: with
  no time, nothing is recorded.
* **To wipe the history** without switching it off, press `W` twice on the
  console (within 5 seconds). It is logged in the evidence log. There is no
  button for it on the screen (the Settings page is full).

## What a load of the file leaves in the evidence log

The hub reads the file at every card mount. A swapped or edited card must not
silence alarms or move the node positions the spoof checks use without a
trace, so every load writes evidence "setting" events:

1. **One summary**: the file's SHA-256 (in the text and as the first 32 data
   bytes), its size and line count, lines used and ignored, how many mutes for
   good, how many mute lines not honoured, how many node positions, the
   control switches, and how many areas, the bubble's radius and how many
   friends, then the `crowd` and `visits` switches. For example `site file
   loaded: sha256 9f2c...e1, 412 bytes, 14 lines (12 used, 2 ignored); 1
   mutes for good, 1 mute lines not honoured; 3 node positions; control ids
   off, control content off; 2 areas, bubble off, 1 friends, several at once
   from 3 drones, visit history kept`.
2. **One per change** from what the hub was using: a mute added or removed, a
   node placed, moved (with the old position) or cleared, e.g. `site file
   changed: node 10:bd:a3:ce:bd:18 at 39.9600000 -75.1638000 (was 39.9521000
   -75.1638000); spoof checks use it`; an area set or removed, the bubble,
   a friend added or removed (`site file changed: friend SN1 "our own Mini"
   added (its area alarms are lowered to Notice; fast inbound never is)`),
   the `crowd` or `visits` switch changed.
   The same file loaded again changes nothing and logs only the summary.
3. **Bounded.** However long the file (it is read a piece at a time; later
   lines win), the hub holds at most 24 mutes, 16 node positions, 8 areas, a bubble and 8 friends, so one
   load can change at most 107 things. At most 48 are logged one by one; the
   rest are counted in one more event (`site file changed: N more changes not
   logged one by one`). The hash always covers the whole file.

Changes to the control switches are logged as their own setting event when
they take effect, as before.

## On the console

Press `L`, type one line exactly as in the file, press Enter (Esc cancels).
The line is appended to the file and logged as an evidence "setting" event.
`node all here` and `area here R` work only here (and `node all here` on the
screen), since they need the hub's live position. `unmute all` drops every
mute, the file's included.

## Snoozing a drone's threat level

On the screen, Settings, **Alarms**: the Snooze row also counts drones at
Warning or above, and SEL snoozes their alerts for **5 minutes**, like a
jamming alarm. A snoozed drone leaves the red line at the bottom of the
screen, but its level is still in the phone page's data and still written to
the evidence log (marked snoozed). Nothing switches the threat levels off.
