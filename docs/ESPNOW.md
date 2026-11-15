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
(ESP-NOW v2; both ends report version 2).

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
node). Like the uplink it is not authenticated: someone nearby could send a
node a fake assignment and make it listen elsewhere. See "Trade-offs" below.

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
* **Not authenticated.** Anyone nearby could send packets that the gateway
  relays as a fake node, and so inject a fake track. ESP-NOW's own encryption
  allows only about 6-7 encrypted peers (Biscuit Shop's "Secure" mode caps at 6
  nodes for what looks like this reason). If needed, a per-packet MAC with a
  shared key in our own header has no peer limit.
* The Biscuit Shop (CodeHedge) publishes no ESP-NOW details: its C5 "Nodes"
  report to a "Manager", but the protocol, packet format and source are
  closed. Nothing here is taken from it.

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
