# The link between boards

**Level: 🟡 Intermediate** · How Rotorscope's boards talk to each other: the shared message
format, what each message carries, the ESP-NOW radio hop and its signing, the wired bus, and
the commands the hub sends back to the nodes.

## In plain words

Every board in a kit speaks one small language, defined once in the shared `link` crate.

- A node's report is a **frame**: a short, packed message such as "I heard this Remote ID
  message, on this channel, this loud". The hub's orders to a node are **commands**: "listen to
  these channels", "the time is now ...".
- The frames travel over a serial wire, or by radio through the gateway. The language is the
  same either way; only the road changes.
- The hub checks two things: that the message really came from that node of this kit (when the
  kit is sealed: each node has a key of its own), and that the sender is allowed to say that kind
  of thing. A node may report a
  drone. A node may never claim to be the hub and announce where the kit is.

This page explains the ideas. The byte-by-byte layout is in the
[wire protocol reference](../dev/protocol.md); unfamiliar terms are in the
[glossary](../project/glossary.md).

## The wire format

A frame is a Rust value (`link::Frame`) turned into bytes in two steps:

1. **postcard** packs it into a compact binary form: small numbers take one byte, and there are
   no field names, only the values in a fixed order.
2. **COBS** (Consistent Overhead Byte Stuffing) rewrites those bytes so that none of them is
   zero, then adds a single zero byte at the end. A zero therefore always means "end of
   frame".

So a board that joins mid-stream, or meets noise, skips to the next zero and is back in step at
the next frame; a damaged frame is counted as corrupt and costs only itself. The largest encoded
frame is 320 bytes (`link::MAX_FRAME`).

Serial links between boards run at 921600 baud (bits per second).

## What the frames carry

The table groups the frames by who is allowed to send them. That rule is written once, in
`crates/link/src/origin.rs`, as one table with no "anything else" case: a new frame type does not
even compile until someone decides who may send it.

| Frame | Who may send it | What it carries |
|---|---|---|
| `Hello` | a node | "I am here": role, protocol version, firmware version; sent at start-up and every few seconds |
| `Status` | a node | the roles the node's images can do (`can`) and the plan number it applied |
| `NodeHealth` | a node | every 5 s: what the node sent and turned away, and what it refused from the hub |
| `Stats` | a node | once a second: frames seen, Remote ID found, frames dropped |
| `Detection` | a node | one raw Remote ID message (or a pack of them) with channel, signal strength, transmitter address and time |
| `OpenLink` | a node | an open digital FPV link stream (wfb-ng, OpenHD, RubyFPV, DroneBridge) |
| `DroneId` | a node | one decoded DJI DroneID burst, or an encrypted O4 record |
| `DjiWifi` | a node | DJI's older Wi-Fi DroneID element, raw; the hub decodes it |
| `WifiDrone` | a node | a Wi-Fi network that looks like a known drone maker's |
| `Rf` | a node | a raw-capture finding: analog video, a digital video link fingerprint, energy |
| `Threat` | a node, for the attacks it detects | a Wi-Fi or Bluetooth attack, an attack on a drone, jamming, or a blinded node |
| `Air`, `AeroStatus` | a node (the LR2021 aero node) | one raw aircraft frame (1090, 978, FLARM, OGN), and the node's state |
| `Control`, `ControlEnergy`, `MavlinkEvidence` | a node (LR2021 or Wi-Fi) | a drone control link, an energy map of a band, the MAVLink frame behind an alarm |
| `Via` | the hub only (its wired-bus reader, and its feed to the web head) | "the next frame came from the node with this address" |
| `Threat` of kinds `ForgedLink`, `AreaGnss`, `IdFlood`, `LinkLoss` | the hub only | packets it refused, GPS interference in the area, a flood of made-up drones, a node's reports going missing |
| `HubFix` | the hub only, to its web head | the kit's own GPS position, time and receiver state |
| `Csbee` | the hub only, to its web head | one line from the hub's ADSBee aircraft receiver |
| `Flags` | the hub only, to its web head | possible FAA rule violations on one track |
| `BusJoin`, `BusDone` | wired-bus control only | joining the bus, and the end of a node's answer |

`Via` is a marker, not a wrapper: it goes in front of a frame rather than around it, so frames
keep their full size. A `Via` names exactly one frame, the one after it. Until 2026-11-18 the
ESP-NOW gateway wrote it too; now the gateway sends no frames of its own (below).

## Protocol versions

The current version is **14** (`link::PROTOCOL_VERSION`). Every board announces it in its
`Hello`.

The rule that keeps old and new boards working together is **append only**: postcard numbers
the kinds of frame by position, so a new kind always goes at the end. An older board counts the
unknown frame as corrupt and carries on; everything else still decodes as before. The comment
above `PROTOCOL_VERSION` in `crates/link/src/lib.rs` lists what each version added, from 1
(Hello, Detection, Stats) to 14 (per-node link keys). Version 14 broke the rule once, on purpose:
the hub's position frame gained two fields, so the hub and its web head are updated together.

## Commands: from the hub to the nodes

The hub talks back with `link::Command`, framed exactly like a frame:

| Command | Meaning |
|---|---|
| `Assign` | "listen as described": a role and a **plan** (channels with how long to stay on each, or radio frequencies for a raw-capture node), and a plan number |
| `To` | "the next command is for the node with this address" |
| `TimeSync` | "it is now this time (UTC)", about once a second, with a flag saying whether the hub's time is pinned to a GPS PPS pulse |
| `Poll`, `JoinSlot`, `Address` | wired-bus control (below) |

Three kinds of thing travel down this way:

- **Role plans.** The coverage coordinator decides every 2 s; a changed assignment goes out at
  once, and every assignment is sent again every 5 s whether or not it changed
  (`firmware/hub-p4/src/downlink.rs`). Repeating is harmless: an `Assign` that matches what the
  node is already doing changes nothing. The node echoes the plan number in its `Status`, which
  is how the hub knows it was applied (see [Who listens where](coverage.md)).
- **Time.** `TimeSync` goes to nodes that need UTC; today that is only the LR2021 aero node,
  which uses it to follow FLARM and OGN frequency hops (see
  [Position and time](position-and-time.md)).
- **Settings.** The two control-link opt-ins (read link identifiers; decode link content) ride
  inside the plan as one special entry that is a setting, not a channel. A node that does not
  know the entry treats both as off.

## The ESP-NOW hop

ESP-NOW is Espressif's way for ESP32 chips to send short radio packets straight to each other,
without a Wi-Fi network. A node that is not wired sends its frames this way to the **gateway**,
which hands each packet to the hub over its wire exactly as it was received. The gateway holds no
key and checks nothing that needs one; the hub checks every packet. The details are in
[ESP-NOW in depth](../ESPNOW.md); the ideas are:

- **Packets.** The `airlink` crate puts a 6-byte header in front of as many whole frames as
  fit, up to 1024 bytes. Frames wait up to 50 ms and go together: a node cannot listen while it
  transmits.
- **Channel 6 only.** The gateway sits on 2.4 GHz channel 6 and puts the hub's beacon on the air
  twice a second. A node sends only while its own plan has it on channel 6, and hears the hub's
  commands only then. That is why Wi-Fi plans for ESP-NOW nodes include short channel-6 visits.
- **A key per node.** On a sealed kit every node has its own key, worked out from the kit's
  master key and the node's slot and address, and kept in a small part of its flash that
  firmware updates never touch (`rskey`). The hub holds the master and works out every node's key
  at boot. Firmware images hold no key and are the same for every kit. Each packet ends with a
  28-byte trailer: a freshness stamp and a tag (HMAC-SHA256, cut to 12 bytes) over the sender's
  address and the whole packet, under that node's key. Nothing is encrypted: detections are what
  anyone nearby can hear anyway. The tag proves that the packet came from that one node.
- **Replay protection.** The hub keeps a clock that ticks every 10 ms, and a boot counter that
  goes up at every hub restart. The hub takes a node's packet only if it is newer than the last
  one from that node and no more than 2 s old. A node takes a command only if it echoes the
  node's own random number for this boot and is newer than the last, and refuses anything more
  than about 3 s late, so a recorded command cannot be played back later.
- **What the gateway refuses.** Without any key it can still drop packets from addresses that
  are not in the kit's roster, floods (more than its share per address), and packets from boards
  still on the old firmware. It reports these to the hub, which raises them as **forged node
  traffic**.
- **What the hub refuses.** Packets with a bad tag, replayed, stale, late or unsigned ones, and
  any frame a node may not send (the table above). Each refusal is counted and shown, at most
  once a second per sender, as a **forged node traffic** threat.

A kit **ships open**: it works, unauthenticated, and the P4 shows `OPEN LINK` on its screen until
you seal it with `link-key` ([ESP-NOW in depth](../ESPNOW.md#kits-ship-open-sealing-yours)).

> [!IMPORTANT]
> A node's key is in its own flash, unprotected, so a stolen node gives away that node's key and
> nothing more: revoke its slot and it is worth nothing. The hub holds the kit master, so **a
> stolen hub is a stolen kit** until a secure element is fitted. Even a node someone controls
> cannot make the hub accept a false kit position, ADSBee aircraft or FAA flags
> ([ESPNOW.md](../ESPNOW.md), "What a capture gives away" and "Who may send what").

**Proof.** Before the 2026-11-18 cut-over (one key for the whole kit, checked at the gateway) the
ESP-NOW relay was **proven on the air** on the bench (two nodes, 0 lost, 0 corrupt; image switches
and hub assignments delivered), and a keyed kit refused each of seven forbidden frame kinds from a
deliberately forging test build within about a second. The current link, with a key per node and
a gateway that holds none, is **simulated only** (tested on a computer and built for the boards). Radio range has not been
measured.

## The wired bus

The wired bus puts many nodes on **one** serial port of the hub, with two signal wires and a
ground ([The wired bus in depth](../WIREBUS.md)):

- The hub's transmit wire goes to every node. Every node's transmit pin joins one shared return
  wire, and can only pull it low, so two nodes can never fight.
- The hub **polls**: "node 3, go ahead". Only that node answers, with its queued frames and a
  checksum, and keeps the answer until the next poll confirms the hub got it.
- New nodes join in short random slots; a node's bus address is tied to its own address, so it
  gets the same one back after a reboot or an image switch.
- The hub puts a `Via` in front of each wired node's frames, so the rest of the hub knows which
  node sent each one, with the same "who may send what" rules as for ESP-NOW nodes.

On the bus no node transmits on the air, and none loses listening time to sending. PLAN.md
(Phase 5a) makes it the intended default, with ESP-NOW kept for nodes that cannot be wired.

**Its true state as of 2026-11-18: simulated only.** The protocol is tested on a computer against
a simulated wire with up to 8 nodes (collisions, noise, reboots, unplugging, a flooding node), and
the firmware builds with it, but nobody has wired it yet. There is no key on the bus: anyone with
access to the wires can make up node reports, though not hub-only frames.

## Where this lives in the code

- `crates/link/src/lib.rs`: `Frame`, `Command`, `Assignment`, `PROTOCOL_VERSION`, encoding and
  the stream decoder.
- `crates/link/src/origin.rs`: who may send each frame.
- `crates/link/src/forward.rs`: the hub's feed to the web head.
- `crates/link/src/gw.rs`: what the gateway and the hub say to each other.
- `crates/airlink/src/lib.rs`, `v3/`, `relay.rs`: ESP-NOW packets, sealing and replay
  protection, and the gateway's keyless filters.
- `crates/kitkeys`, `crates/hublink`: the kit's keys and the hub's side of the link.
- `tools/link-key`: makes the kit master and writes each board's keys.
- `crates/wirebus`: the wired bus, both ends, and its wire simulation.
- `firmware/gateway/src/main.rs`; `firmware/hub-p4/src/links.rs`, `bus.rs`, `downlink.rs`.

**Next:** [The tracker](tracker.md)
