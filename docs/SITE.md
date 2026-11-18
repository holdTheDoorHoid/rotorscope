# Site settings: where the nodes are, and muted alarms

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

## What a load of the file leaves in the evidence log

The hub reads the file at every card mount. A swapped or edited card must not
silence alarms or move the node positions the spoof checks use without a
trace, so every load writes evidence "setting" events:

1. **One summary**: the file's SHA-256 (in the text and as the first 32 data
   bytes), its size and line count, lines used and ignored, how many mutes for
   good, how many mute lines not honoured, how many node positions, and the
   control switches. For example `site file loaded: sha256 9f2c...e1, 412
   bytes, 14 lines (12 used, 2 ignored); 1 mutes for good, 1 mute lines not
   honoured; 3 node positions; control ids off, control content off`.
2. **One per change** from what the hub was using: a mute added or removed, a
   node placed, moved (with the old position) or cleared, e.g. `site file
   changed: node 10:bd:a3:ce:bd:18 at 39.9600000 -75.1638000 (was 39.9521000
   -75.1638000); spoof checks use it`. The same file loaded again changes
   nothing and logs only the summary.
3. **Bounded.** However long the file (it is read a piece at a time; later
   lines win), the hub holds at most 24 mutes and 16 node positions, so one
   load can change at most 80 things. At most 48 are logged one by one; the
   rest are counted in one more event (`site file changed: N more changes not
   logged one by one`). The hash always covers the whole file.

Changes to the control switches are logged as their own setting event when
they take effect, as before.

## On the console

Press `L`, type one line exactly as in the file, press Enter (Esc cancels).
The line is appended to the file and logged as an evidence "setting" event.
`node all here` works only here and on the screen, since it needs the hub's
live position. `unmute all` drops every mute, the file's included.
