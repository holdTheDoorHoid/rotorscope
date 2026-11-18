# Site settings: where the nodes are, and muted alarms

The P4 hub keeps two kinds of site settings in one plain text file on its SD
card, `ROTORSCP/SITE.TXT`. You can edit it on a computer, or change it on the
hub itself (screen or console); the hub only ever **adds** lines to the end, so
nothing you typed is lost. Later lines win.

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

## Muting false alarms

Some places set off an alarm honestly, for example a cafe that runs an open
network with the same name as its secured one (that looks like an "evil
twin"). A muted alarm is still recorded in the evidence log, but it no longer
raises the ATTACK / JAMMED badge or counts as jamming.

* **On the hub:** Settings, **Alarms**. While alarms are sounding it says
  "Mute 2 now": SEL mutes exactly those (the same network name and transmitter,
  the same victim, or the same band for jamming). When nothing sounds but some
  are muted, it says "Unmute 3": SEL removes every mute.
* **In the file:**

      mute evil_twin source=02:11:22:33:44:55 ssid=Cafe WiFi
      mute deauth target=ff:ff:ff:ff:ff:ff
      mute jam_broadband mhz=2437.0
      unmute all

  The kind names are the ones in the hub's `"t":"threat"` lines. `ssid=` takes
  the rest of the line, so put it last.

## Sound clips

```
audio clips off     # the hub stores no sound at all (it still detects and alerts)
audio clips on      # the default
```

While the hub believes it hears a drone it saves a clip under `ROTORSCP/AUDIO` (docs/ACOUSTIC.md).
A microphone outdoors also hears people, and Pennsylvania requires everyone's consent to record
a conversation: where that matters, post a notice or turn clips off.

## On the console

Press `L`, type one line exactly as in the file, press Enter (Esc cancels).
`node all here` works only here and on the screen, since it needs the hub's
live position. Every change is also logged as an evidence "setting" event.
