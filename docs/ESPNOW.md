# ESP-NOW node link (experimental)

A node that cannot be wired to the hub can send its reports over ESP-NOW to a
**gateway**: one XIAO ESP32-C5 (`firmware/gateway`) wired to a hub UART. The
gateway relays every node's frames on that one UART, so the hub sees one more
wired link that speaks for many nodes. The onboard C6 of the P4 board was the
first choice, but its factory firmware has no ESP-NOW support (see
`firmware/hub-p4/BRINGUP.md`); owner decision 2026-11-14: C5 gateway for now.

## What goes over the air

`crates/airlink`: a 6-byte header (`RS`, version 1, kind, sequence number),
then as many whole `link` frames as fit (they are already COBS-delimited, so
the gateway just splits on the zero bytes). Up to 1024 bytes per packet
(ESP-NOW v2; both ends report version 2). With a kit key (see "Securing the
link" below) the header says version 2 and every packet ends in a 28-byte
trailer: a freshness stamp and an authentication tag.

* **Gateway beacon**, broadcast every 500 ms on its channel (2.4 GHz ch 6).
  Nodes learn the gateway's address from it.
* **Node batch**, unicast to the gateway. Unicast gets the radio's ACK and
  automatic retries; broadcast gets neither. Sequence numbers let the gateway
  count real losses and drop duplicates (a retry whose ACK was lost).

## Efficiency choices

* **Batching**: frames wait up to 50 ms and go as one packet. Every ESP-NOW
  packet costs preamble, header, ACK and contention whatever its size, and a
  Wi-Fi node cannot hear drones while it transmits.
* **No channel switching for the 2.4 GHz node**: channel 6 is 300 of every
  400 ms of its hop plan, so it sends only while it is there. The hopper waits
  for a send in flight before retuning (`uplink::wait_idle`), so sends are
  not cut off.
* **Retry, then drop**: a failed batch is retried for 2 s, then dropped and
  counted (a dead gateway must not back the node up). A gateway not heard for
  5 s is forgotten until its next beacon.
* **Same frames as the wire**: nothing is re-encoded on the node or gateway.

## On the hub's side

The gateway writes `link::Frame::Via { node, rssi }` before each relayed
frame (protocol version 3). It is a marker, not a wrapper, so relayed frames
keep their full size budget. The S3 hub accepts and ignores `Via` (relayed
frames count against the gateway's link); the P4 hub will use it for per-node
health. Gateway TX is XIAO D6 (GPIO11), 921600 baud, like a node.

## Downlink: hub to node (protocol 4)

The hub decides what every node listens to (`crates/coverage`, see its
README) and sends each node a `link::Command::Assign`. For a relayed node it
goes through the gateway:

1. **Hub to gateway, wire.** The hub writes `Command::To { node }` then
   `Command::Assign(..)` (postcard + COBS, like every link frame) on its
   UART TX, 921600 baud. On the P4 that is the UART3-IN header **TX =
   GPIO27**, wired to the gateway's **D7 = GPIO12** (its UART1 RX). The hub
   decides every 2 s, sends a changed assignment at once and resends every
   assignment every 5 s.
2. **Gateway to node, air.** The gateway pairs each `To` with the `Assign`
   after it and sends that node an `airlink` **command packet** (kind 3):
   the usual 6-byte header, then the `Assign` frame exactly as the hub
   encoded it. Unicast, so the radio ACKs it (the peer is added on first
   use). One sequence number per node; a retry reuses it.
3. **Retries.** A node only hears the gateway while it is on channel 6. So
   the gateway sends at once, again right after it next hears from that
   node (the node is on 6 at that moment), and at each beacon, 4 tries in
   all, then gives up and counts a failure. A newer command for the same
   node replaces an older one that was not delivered yet.

The gateway's USB log has a `"t":"down"` line per node once a second:
commands from the hub, packets sent, ACKed, given up, replaced, and whether
one is still pending; the `"t":"gw"` line counts bytes and commands from the
hub and any corrupt or orphan (no `To`) commands.

Nodes built before kind 3 existed do not parse it and ignore it.

**Node side (branch `node-roles`).** node-wifi and node-ble take the command
packet in their uplink, drop a repeat (same gateway, same sequence number)
and hand each `Assign` to their `role` module:

* node-wifi applies a Wi-Fi job at once (new channel plan, band chosen per
  hop, so a 5 GHz plan's channel-6 visits work) and echoes the plan id in
  `Frame::Status`; it still sends only while tuned to 6.
* node-ble echoes the plan id (Bluetooth has no plan). It parks Wi-Fi on
  channel 6 and stops its scan for 200 ms of every second to send and to
  hear the gateway; that costs ~18% of the adverts it hears (its README).
* A role the running image cannot do switches image (OTA slots,
  `firmware/node-common`, `tools/flash-node.sh`); the other image boots with
  the job and reports it. The node's MAC is the same in every image, so the
  gateway and hub keep tracking it across the switch.
* Still open: a relayed raw-capture node (DroneID, FPV) has frequencies in
  its plan, not channels; it must retune briefly to channel 6 by itself to
  send and to hear the gateway (node-sdr's side).

Downlink is new transmitting by the gateway, but only to our own nodes and
only when the hub has something to say (a few short packets every 5 s per
node). Without a kit key it is not authenticated: someone nearby could send a
node a fake assignment and make it listen elsewhere. With one, a node takes
only commands that carry a good tag and echo its own random session (see
"Securing the link").

## Which nodes can use it

| Node | ESP-NOW | Why |
|---|---|---|
| Wi-Fi 2.4 GHz (S3, or C5 `cargo fw24-now`) | yes, built and tested | already dwells on ch 6 |
| Wi-Fi 5 GHz (C5) | yes (same image, band chosen at run time) | 40 ms on ch 6 at least once a second, from the plan |
| BLE (C5, `espnow` feature) | yes, built and tested | time-shares the radio: scan off 200 ms of every second (~18% fewer adverts) |
| Raw capture (node-sdr, `cargo fw-now`) | yes, built and tested (2026-11-14, c5b) | pauses capture for a short window, retunes to ch 6, sends, re-locks receive-only; applies `Assign` (firmware/node-sdr/README.md "Hub link") |

## Trade-offs to know

* **ESP-NOW nodes transmit.** A wired node is receive-only and invisible; an
  ESP-NOW node and the gateway (its beacons) can be seen by anyone scanning,
  and their MAC addresses are fixed. That is why this is a build option, not
  the default.
* **Authenticated only with a kit key.** Without one (the default until you
  make a key), anyone nearby can send packets that the gateway relays as a
  fake node (a fake track, or a real node's reports drowned out), or send a
  node a fake assignment (blind it). Boards built without a key say so at
  boot, and the gateway tells the hub in its `Hello` ("gw OPEN no key").
  With a key, every packet both ways carries a 96-bit HMAC-SHA256 tag and a
  freshness stamp in our own trailer; forged and replayed packets are dropped
  and the gateway reports them to the hub as an attack (`ForgedLink`). This
  is our own MAC rather than ESP-NOW's encryption, which allows only about
  6-7 encrypted peers (Biscuit Shop's "Secure" mode caps at 6 nodes for what
  looks like this reason); ours has no peer limit. Nothing is encrypted:
  detections are what anyone nearby can hear anyway.
* **The key is in every board's flash.** Someone who steals a node can read
  the key out of it. If a board is lost, make a new key and reflash the whole
  kit (below); the old key then opens nothing.
* **Jamming still works.** A key stops forgery and replay, not someone
  drowning channel 6 in noise; that shows up as jamming and as nodes going
  quiet on the hub.
* The Biscuit Shop (CodeHedge) publishes no ESP-NOW details: its C5 "Nodes"
  report to a "Manager", but the protocol, packet format and source are
  closed. Nothing here is taken from it.

## Securing the link

One secret key per kit, shared by the gateway and every ESP-NOW node. It is
a small file on your computer, `tools/link.key`, that never goes into git
(the repository is public; `.gitignore` already excludes it). The build
bakes it into each board.

**Rule: all or nothing.** A gateway built with the key talks only to nodes
built with the same key, and a node built with the key talks only to a
gateway built with it. A mixed kit does not half-work: a node without the
key is ignored by a keyed gateway (and reported as an attack, reason
"unauthenticated"), and a keyed node never finds an unkeyed gateway. When
you add the key, reflash **every** ESP-NOW board.

### 1. Make the key (once per kit)

From the repository folder:

```sh
cargo run -p link-key -- new
```

It writes `tools/link.key` and prints the key's **ID**, eight letters and
digits like `59507916`. The ID names the key without giving it away; write
it down. Back the file up somewhere private (a password manager, a USB stick
in a drawer). Never paste the file's contents anywhere. To see the ID again
later: `cargo run -p link-key -- id`.

### 2. Flash every board

Build and flash exactly as before; the build finds `tools/link.key` by
itself and prints `ESP-NOW link key <ID>` while compiling.

* Gateway: `cd firmware/gateway && cargo fw`, then
  `tools/dev-board.sh firmware/gateway/target/riscv32imac-unknown-none-elf/release/gateway 60 c5c`.
* Each multi-image node: `tools/flash-node.sh <board>`. It builds node-wifi
  and node-ble with the key and refuses to flash if the node-sdr image was
  built with a different key (or none): rebuild that one first
  (`cd firmware/node-sdr/c5 && cargo fw-now`, then pass its ELF in
  `NODE_SDR_ELF`).

`tools/dev-board.sh` prints the key ID of each image it flashes, and a
**WARNING** if an image has no key or a different one from `tools/link.key`.

Builds in another checkout of the repository need the file there too, or
`ROTORSCOPE_LINK_KEY_FILE=/path/to/link.key` in front of the command.
`ROTORSCOPE_LINK_KEY_FILE=none` builds an open (unauthenticated) image on
purpose.

### 3. Check it worked

* **At boot**, on each board's USB log: the gateway's first line ends in
  `"key":"link-key:<ID>"`; each node prints `{"t":"uplink","auth":"link-key:<ID>"}`.
  The ID must be the same on every board. A board built without the key
  prints a `"t":"warn"` line saying `built WITHOUT a link key` instead.
* **Once a second**, the gateway's `"t":"gw"` line has `"key":"<ID>"` and
  counters `bad_tag`, `replay`, `unauth`, `stale` (rejected packets) and
  `syncs`. On a healthy kit the four reject counters stay at 0 (a few
  `stale` right after the gateway restarts are normal).
* **Nodes still report**: the hub keeps showing every ESP-NOW node, and the
  gateway's `"t":"node"` lines keep counting packets. A node that shows up
  only as rejects was built without the key, or with an old one.
* **On the hub**, the gateway's own `Hello` says `gw key <ID>` (or
  `gw OPEN no key`); rejected packets arrive as `Threat` frames of kind
  `forged_link` naming the claimed node, at most once a second.

### 4. If a board is lost or you think the key leaked

`cargo run -p link-key -- new --force`, then reflash the gateway and every
node. Boards with the old key go deaf to the kit (that is the point).

### How it works (for reviewers)

`crates/airlink/src/auth.rs` holds the whole scheme and its tests:

* **Tag**: HMAC-SHA256 under the 32-byte kit key, truncated to 12 bytes, over
  the sender's MAC address and the whole packet (header, payload, stamp).
  The sender's address is covered, so a genuine packet from one node cannot
  be replayed as another node's.
* **Stamp** (16 bytes): gateway epoch (random per gateway boot), gateway
  clock (+1 per gateway packet), node session (random per node boot), node
  counter (+1 per transmission).
* **Uplink replay protection**: the gateway takes a node packet only if its
  epoch is the current one and `(clock, counter)` is above the last one taken
  from that address. A node that reboots can only send after hearing a gateway
  packet sent after its boot, so its first `(clock, 0)` is above everything
  from its previous life: no flash writes needed.
* **Downlink replay protection**: a command echoes the node's session and
  newest counter, so it can only have been made during this node boot, after
  that packet; the node takes only increasing `(echoed counter, clock)`.
  Broadcast beacons carry no echo and never move a node to another epoch.
* **Resync**: a node packet with a good tag but a stale epoch (the gateway
  rebooted) or an old counter is answered with a sync, a beacon sent to that
  node echoing its session, at most once a second per node.
* **Wrap**: the node counter is 32 bits; at the end it starts a new session
  and waits for a newer gateway clock. The gateway restarts (new epoch) if
  its own 32-bit clock ever runs out (decades).
* **Cost**: 28 bytes per packet (about 0.22 ms of air at 1 Mbit/s), and a
  software SHA-256 of the packet (the C5 runs one 64-byte block in an
  estimated 15-20 us; about 40 us for a beacon, 60-150 us for a typical
  batch, 0.35 ms for a full 1 KB one). The C5 has a SHA accelerator if this
  ever matters.
* **Reports**: `link::ThreatKind::ForgedLink`, `detail` 1 bad tag, 2 replay,
  3 unauthenticated, 4 stale (see its doc comment).

Not verified on hardware yet (2026-11-17): everything above passes host
tests and all four images build with and without a key; the radio behaviour
(syncs after a gateway restart, node image switches, the threat report on the
hub) needs a bench run.

## Bench results (2026-11-14, c5a node, c5c gateway, c5b test beacon, ~1 m)

| Run | Node | Gateway | Result |
|---|---|---|---|
| Idle, 50 s | 50 packets, 1 failed send | 55 packets, 1 "lost" (the failed send; node then advanced its sequence, fixed since) | PASS |
| Test beacon (Wi-Fi beacon + NAN, ch 6), 75 s | 107 Remote ID detections, 182 packets, 0 failed, 0 dropped | 227 packets, 0 lost, 0 duplicates, RSSI -17 dBm | PASS |
| Same, gateway decoding every relayed frame, 60 s | 0 failed; 2 batches dropped while the gateway was being reflashed (by design) | 160 frames decoded, 0 corrupt, 86 detections, 0 lost | PASS |

Average wait per send (queue + air + ACK) was about 3.7 ms.

### PHY rate (2026-11-14, same setup, test beacon on, 60 s per rate)

Build-time choice: `ROTORSCOPE_ESPNOW_RATE=<rate> cargo fw24-now`. The
gateway enables Espressif Long Range in its protocol set, so it hears every
rate below.

| Rate | Packets | Failed | Wait per send |
|---|---|---|---|
| 1M (default, 802.11b) | 147 | 0 | 3.3 ms |
| 11M | 143 | 0 | 2.3 ms |
| 24M (11g) | 146 | 0 | 1.9 ms |
| 54M (11g) | 145 | 0 | 1.6 ms |
| MCS7 (11n) | 159 | 0 | 1.9 ms |
| LR 250 kbit/s | 137 | 0 | 9.3 ms |

Gateway over all six runs: 1121 packets, 0 lost, 0 duplicates, 1369 frames
decoded, 0 corrupt. At this traffic (2-3 packets a second) the fastest rate
saves under 0.5% of the node's listening time over 1M, so rate does not matter
for efficiency here; it matters for range. Keep 1M by default; LR is the
candidate for long links (costs ~9 ms per packet, about 2% of listening time
at this load). Range is not measured yet.

### Two nodes (2026-11-14, c5a and c5b both ESP-NOW nodes, 60 s)

Gateway tracked both separately: 86 and 74 packets, 0 lost, 0 duplicates,
191 frames decoded, 0 corrupt.

Flash: `tools/dev-board.sh firmware/gateway/target/riscv32imac-unknown-none-elf/release/gateway 60 c5c`
and the node from `firmware/node-wifi/target-espnow/...`.

## Not done yet

* Range test (walk the node away; then try the Long Range PHY).
* More than two nodes, and two nodes under heavy traffic.
* The node side of the downlink (applying an `Assign`) for Wi-Fi and BLE nodes; node-sdr applies it (DroneID and FPV plans).
* The S3 build of node-wifi with `espnow` compiles; it has not been run.

### Node roles and image switching (2026-11-15, c5a node, c5c gateway, P4 hub)

c5a ran the multi-image firmware (`tools/flash-node.sh`: node-wifi in
`ota_0`, node-ble in `ota_1`, `ota_2` empty), c5b another agent's node-sdr
reporting DroneID through the same gateway.

* **Status and `can`:** c5a reported `can` = Wi-Fi 2.4 + Wi-Fi 5 + Bluetooth
  (7). With c5b (an older image, no Status) holding the 2.4 GHz slot, the hub
  gave c5a Bluetooth, which it could only know from the Status.
* **Plans:** Wi-Fi 2.4 plan `0x1503` applied and echoed (hub: `applied=2`);
  per-channel visits 6:1328, 1:663, 11:663 in 270 s, matching 6/1/6/11. Wi-Fi 5
  plan `0x8a0c` (18 hops, 149 anchor, 153-165 and 36-48, two 40 ms visits to
  6) applied: 0 band-change errors, 0 channel errors, every channel visited
  per cycle, uplink 0 failed sends.
* **Role switching:** with a bench build (`ROTORSCOPE_BENCH_FLIP=1`, which
  hides roles from `can` so one node flips), the hub moved c5a
  Bluetooth -> Wi-Fi 5 -> Bluetooth ... 7 times in 5.5 minutes in one run;
  every switch came from a hub command, every new image started with the
  plan id the hub sent and kept reporting through the gateway.
