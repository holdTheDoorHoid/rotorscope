# Wired bus: many nodes on one hub UART

PLAN.md Phase 5a. The radio nodes (XIAO ESP32-C5 boards running node-wifi,
node-ble or node-sdr) share **one** UART of the P4 hub (the UART3 peripheral, on the board's UART1 header since 2026-11-17), over two
signal wires and a ground. The hub asks each node in turn for its reports.
Nothing in the kit transmits on the air: the nodes only listen, and the
ESP-NOW gateway C5 becomes one more listener.

Status (2026-11-17): **built and tested on the laptop only.** The protocol
runs against a simulated wire with up to 8 nodes (below); all node images and
the hub build. Nobody has wired it yet. The bench checklist is at the end.

| Piece | Where |
|---|---|
| Protocol, both ends, host tests and the wire simulation | `crates/wirebus` |
| New messages (protocol 10) | `crates/link`: `Command::Poll`, `JoinSlot`, `Address`; `Frame::BusJoin`, `BusDone` |
| Node glue, shared by the three images | `firmware/node-common/src/bus.rs` (feature `wirebus`) |
| Node images | `wirebus` feature in node-wifi (`cargo fw24-bus`), node-ble (`--features esp32c5,wirebus`), node-sdr (`cargo fw-bus`) |
| Hub | `firmware/hub-p4/src/bus.rs`, wired into `links.rs`; on in the default build (feature `wirebus`) |
| Flashing a multi-image bus node | `NODE_BUS=1 tools/flash-node.sh <board>` |

## How it works, in one page

* **Two wires.** The *command wire* runs from the hub's TX to every node's RX:
  every node hears everything the hub says. The *return wire* runs from every
  node's TX back to the hub's RX. A node's TX is **open-drain**: it can only
  pull the wire down to 0 V, never push it up; one resistor at the hub pulls
  the wire up to 3.3 V whenever nobody pulls it down. So a node that is
  silent is electrically invisible, and two nodes can never fight each
  other's levels.
* **Polling.** Only one node may talk at a time, so the hub asks:
  `Poll { addr, ... }` means "node *addr*, go ahead". That node sends the
  frames it has queued (the same `link` frames a point-to-point wired node
  sends, unchanged) and ends with a `BusDone` carrying a CRC of everything it
  just sent, or a `BusDone` alone if it has nothing. The hub keeps the frames
  only if the CRC matches. Every node is asked at least every 20 ms.
* **Nothing lost.** The next poll to that node says which answer the hub
  accepted (`ack`). A node keeps its last answer until it is acknowledged and
  sends the same answer again otherwise. A broken answer, a lost poll or a
  late node costs a retry, never a frame.
* **Joining.** Now and then the hub asks "anyone new?" (`JoinSlot`). A node
  without an address answers with a `BusJoin` naming its station MAC, in one
  of 8 short slots picked at random. Two nodes in the same slot collide; the
  CRC in the `BusJoin` catches it, both wait a random number of rounds and
  try again. The hub replies `Address { mac, addr }`. The address is tied to
  the MAC: the same node always gets the same address back (after a reboot,
  an image switch, or a hub restart, when it asks for its old address).
* **Who sent what.** The hub writes each node's frames into the same stream
  an ESP-NOW gateway produces, with a `Via { node: MAC }` marker in front of
  each. The node table, tracker, evidence log, coverage coordinator, web-head
  feed and screen therefore see every wired node under its own MAC, exactly as
  they saw ESP-NOW nodes. The hub's node table still keys on the station MAC.
* **Orders to nodes.** `To { node: MAC }` then `Assign`, as on the gateway
  link. Every node hears it; only the named one acts on it. The hub sends
  orders only between answers, as whole frames.
* **Who may send what.** A wire is no proof of honesty: anything soldered
  onto the two wires, or a node running bad firmware, can answer polls.
  The hub passes on only what a node may send (see below).

### Who may send what

The same origin rules as the ESP-NOW link (docs/ESPNOW.md "Who may send
what", `crates/link/src/origin.rs`) apply on the bus, checked by the hub's
`wirebus::Attributor` as each checked answer is unpacked (in the UART
interrupt; it reads only each frame's first bytes):

* A bus node's frame is passed on (with a `Via` naming the node) only if a
  node may originate it. `HubFix`, `Csbee`, `Flags`, a `BusJoin` or
  `BusDone` inside an answer, a `Hello` or `Status` claiming the gateway
  role, and a `forged_link` report are refused.
* A `Via` written by a bus node is honoured only from a node registered as
  a gateway (`Attributor::register_gateway`; a gateway relaying radio
  nodes onto the bus). From any other node it is refused, and the node's
  next frame goes under its own MAC: a node cannot attribute its frames to
  another node. No firmware registers a gateway today (the ESP-NOW gateway
  has no bus mode), so every node-supplied `Via` is refused.
* A registered gateway's `Via` is held and passed on together with its
  frame, which must itself be a node's; its own frames (`Hello`,
  `forged_link`) pass with no `Via`, as on a point-to-point gateway link.
* What reaches the hub's stream is sorted again in `links.rs` like a
  gateway's stream (docs/ESPNOW.md), and the web-head feed sorts it too.

Refusals are counted (`"refused"` in the `"bus"` part of the hub's
`{"t":"hub"}` line) and reported once a second per node as the hub's own
`Threat` `forged_link`, `detail` 5, naming the node's MAC. There is no key
on the bus: anyone with access to the wires can still make up node reports
under any MAC it can join with (a `BusJoin` claims a MAC), but not the
hub-only or gateway frames above, and not under another node's `Via`.

### Messages (protocol 10, appended; sizes at their largest)

| Message | Direction | Bytes on the wire | Meaning |
|---|---|---|---|
| `Poll { addr, epoch, ack, budget }` | hub to nodes | 10 | node `addr`, answer now with at most `budget` bytes; `ack` = your last answer I have |
| `JoinSlot { epoch, slots, slot_us }` | hub to nodes | 9 | nodes without an address: answer in a random slot |
| `Address { mac, addr, epoch, check }` | hub to nodes | 16 | the node with this MAC is `addr` |
| `BusJoin { mac, want, check }` | node to hub | 13 | I am `mac`; last time I was `want` |
| `BusDone { addr, seq, more, crc }` | node to hub | 9 | end of my answer; `seq` numbers it (0 = nothing); `more` = I still have frames |

`epoch` is a random number per hub boot: a node holding an address from an
earlier boot joins again. Old nodes and hubs (protocol 9) count the new
messages as corrupt and carry on (test `protocol_10_bus_append_only`).

### Rules that keep the line clean

* A node talks only (a) when polled by its own address, and only if no newer
  command has arrived by the time it is ready (if the hub has already moved
  on, it stays silent), or (b) in a join slot, while it has no address.
* An answer is made of whole frames only, up to the poll's byte budget, and
  always at least one frame if any is waiting. A frame is never split
  between two answers, so the decoder always sees whole frames.
* Every frame ends in a COBS zero byte, so after any junk the hub resyncs at
  the next zero; the CRC then decides whether the answer is kept.
* After a silent or broken answer the hub waits until the line has been
  quiet for 1 ms before the next poll, so a straggler cannot run into the
  next node's answer.
* A node that misses 10 polls in a row is marked absent and asked only once a
  second. It comes back by itself when it answers again.
* A node not polled for 3 s gives up its address and joins again.

## Timing budget (921600 baud)

One byte takes 10.85 us. A cycle is 20 ms, that is 1843 bytes of line time.

| Step | Time |
|---|---|
| Poll (10 B) | 0.11 ms |
| Node reaction (interrupt-mode task wakes, reads, builds the answer) | ~0.05-0.4 ms (hub allows 4 ms) |
| `BusDone` alone ("nothing") | 0.10 ms |
| Hub reaction (UART interrupt, then the next poll) | ~0.1 ms (RX idle timeout of 10 bit-times + interrupt) |
| Idle node, all in | ~0.3-0.7 ms |
| Byte budget per answer | 1843 / nodes - 64, between 256 and 1024 B: 857 B for 2 nodes, 396 for 4, 256 for 8 |
| Join window (8 slots of 0.6 ms + 4 ms margin), every 1 s (every 0.1 s for 5 s after boot or a join) | ~9 ms |

A full 9-message Remote ID pack is ~245 bytes on the wire (2.7 ms), a
Bluetooth single message ~43, a DroneID flight record ~70, a Threat ~34.

### Simulated results (`cargo test -p wirebus -- --nocapture`)

The simulation (`crates/wirebus/src/sim.rs`) moves every byte at its real wire
time, ANDs bytes that two nodes drive at once (open drain), lets each node
react after the UART idle time plus a random 30-400 us, hands the hub its
bytes the way the P4's UART interrupt does (64 bytes or 108 us of idle), and
stalls the hub's main loop 7 ms every 250 ms (a screen band). Latency runs
from the moment a frame is queued on the node to the moment the hub has
checked it. 10 s per row.

*Quiet* = what one drone gives a node: Stats 1/s, 3 Remote ID packs/s, a
Threat every 5 s. *Busy* = a crowded field per node: 10 Wi-Fi packs + 10
Bluetooth messages + 5 DroneID records + 2 Threats + Stats, every second.

| nodes | traffic | frames/s | bytes/s on the return wire | line busy | mean | p50 | p99 | max | budget |
|---|---|---|---|---|---|---|---|---|---|
| 2 | quiet | 8 | 1740 | 11% | 12.5 ms | 11.7 ms | 21.3 ms | 23.4 ms | 857 |
| 4 | quiet | 16 | 3474 | 19% | 12.2 ms | 12.3 ms | 21.9 ms | 27.1 ms | 396 |
| 8 | quiet | 33 | 6870 | 32% | 11.1 ms | 10.5 ms | 21.9 ms | 29.7 ms | 256 |
| 2 | busy | 55 | 5298 | 15% | 11.5 ms | 11.2 ms | 21.6 ms | 25.0 ms | 857 |
| 4 | busy | 111 | 10538 | 26% | 11.4 ms | 10.9 ms | 23.2 ms | 30.0 ms | 396 |
| 8 | busy | 223 | 21036 | 48% | 12.0 ms | 11.8 ms | 25.7 ms | 31.4 ms | 256 |

"Line busy" is the share of time a poll, an answer or a join window holds the
line; most of it at low load is the nodes' reaction time, not data. Headroom:
8 nodes all flooding full packs moved ~280 frames/s, 69 KB/s, 75% of the
line's 92 KB/s.

Other simulated cases (all pass, every frame accounted for, in order):

* 8 nodes powered at once: all have distinct addresses within 220 ms (6
  seeds), through 21 garbled join chunks (collisions).
* Noise on both wires (1 byte in 5000 replaced): 17 answers refused by the
  CRC in 10 s, all resent, no frame lost or repeated. 1 in 500 on the return
  wire: still exact.
* A node rebooting or switching image (3 s away): back on the same address,
  joined "again".
* The hub rebooting: every node rejoins with its old address. The one thing
  that can repeat: an answer the old hub took but had not yet acknowledged
  is sent again to the new one (4 frames over 6 busy nodes in the test).
* One of 4 nodes unplugged: marked absent; the others' p99 stays 22 ms.
* One node flooding 2000 full packs/s: it gets ~200 frames/s through, the
  other three keep p99 31 ms and lose nothing.
* Nodes reacting 6 ms late 2% of the time: they skip those polls (21 stale
  polls), no frame lost.

## Listening time won back from ESP-NOW

On the wire a node never touches its radio to report: it answers polls from
an interrupt-mode task while the radio keeps listening. What ESP-NOW costs
today (docs/ESPNOW.md, node READMEs) and what the bus costs:

| Node | ESP-NOW today | Wired bus |
|---|---|---|
| Bluetooth (node-ble) | scan off 200 ms of every second for Wi-Fi to send and hear the gateway: **-18% adverts** measured (29.4/s wired vs 24.1/s ESP-NOW) | scan never stops: the wired 29.4/s figure; ~+22% adverts over ESP-NOW |
| Wi-Fi 5 GHz (node-wifi) | a 40 ms visit to 2.4 GHz channel 6 at least once a second (plan 0x8a0c had two): **~4-8% of time off the 5 GHz plan**, plus the retunes | no channel-6 visits: the hub's coordinator marks wired nodes "not relayed" (`downlink.rs`), so plans carry none |
| Wi-Fi 2.4 GHz (node-wifi) | stays on its plan but transmits 2-3 batches/s, ~2-4 ms each with the ACK: **~1% deaf** | 0 |
| DroneID / FPV raw capture (node-sdr) | capture stopped for a send window: ~49 ms + ~25 ms re-lock, at least every 1 s while there is something to send (every 5 s idle): **~7% of capture time** with traffic, ~1.5% idle | capture never stops |
| Reports reach the hub | 50 ms batching plus waiting for channel 6 (up to 1 s on 5 GHz / BLE / node-sdr) | 12 ms mean, 22-26 ms p99 (simulated, 2-8 nodes) |
| On the air | nodes and gateway transmit (beacons, batches) | nothing transmits |

The node side costs CPU instead: about 50 polls a second, each an interrupt
and a few microseconds of work, plus copying the frames out. These figures
are estimates from the ESP-NOW measurements and the simulation. **To
measure** on the bench: node-ble's adverts/s and node-sdr's capture duty on
the bus against the ESP-NOW figures above (the bench checklist, step 8).

## Pins

| Board | Signal | Pin | Notes |
|---|---|---|---|
| XIAO ESP32-C5 node (every image) | RX, from the command wire | **D7 = GPIO12** | same pin as the point-to-point wired build |
| | TX, onto the return wire | **D9 = GPIO9** | open-drain. Not D6 (GPIO11): the chip's ROM prints its boot text there, push-pull, at every reset, which would garble the bus whenever any node reboots. GPIO9 is not a strapping pin and no image uses it |
| P4 hub (CrowPanel Advanced 5"), UART1 header (DIP switch 1 on UART1) | TX, the command wire | **GPIO47** (header "TX") | |
| | RX, the return wire | **GPIO48** (header "RX") | |

**Open drain, how.** esp-hal 1.2 supports it on the C5:
`node_common::bus::uart` makes the pin an `Output` with
`OutputConfig::with_drive_mode(DriveMode::OpenDrain)` and hands it to the
UART as a frozen output signal, so the UART driver cannot set it back to
push-pull. The diode in the wiring guide makes the line safe even if some
image were built the old (push-pull) way.

## Wiring guide (for the person with the soldering iron)

You need, for a kit of up to 8 nodes:

* two signal wires that run past every node, plus a ground wire;
* **one 470 ohm resistor** (any 1/4 W resistor);
* **one small Schottky diode per node**: 1N5817, 1N5819, BAT43 or BAT85 all
  work (not an ordinary 1N4148: its voltage drop is too large);
* optional, one 1 kohm resistor per node (see step 3).

**Before you start:** unplug everything. All signals are 3.3 V. Never connect
a 5 V pin to any of the wires below. Check the pin names printed on each
board; the table above says which pin is which.

1. **Ground.** Connect a GND pin of every node to a GND pin of the P4 (the
   UART1 header has one). Every board must share ground, or nothing works.
2. **The command wire (hub to nodes).** Connect the P4's UART1-header **TX** pin
   (GPIO47) to pin **D7** of every node. One wire, branching to each node.
3. **(Optional, recommended)** Put a 1 kohm resistor in each node's D7 lead,
   at the node. It protects a node that is switched off while the hub is on.
4. **The return wire (nodes to hub).** Connect the P4's UART1-header **RX** pin
   (GPIO48) to one wire that runs past every node. At each node, connect a
   diode between that wire and the node's pin **D9**: the end of the diode
   with the **stripe goes to D9**, the plain end to the return wire.
5. **The pull-up.** Connect the 470 ohm resistor between the return wire and
   **3.3 V**, at the P4 end. The P4's 3.3 V is on the UART1 header (the
   same header as the bus; the GPS takes its power from a 3.3 V pin too;
   share it) or on the I2C header. Only
   one resistor for the whole bus.
6. **Power the nodes separately.** Do not power the nodes from the P4's 5 V
   header pin: it is not meant for several C5 boards (each can draw up to
   about 0.25 A). Use a 5 V USB supply or USB hub rated for all of them,
   into each node's USB-C port, and switch the nodes on with the hub. The
   ground wire from step 1 is still needed.
7. **Length.** Keep the whole run, from the P4 to the farthest node, under
   about **1 m**. Twist each signal wire with a ground wire if you can.

```
 P4 UART1 hdr               node 1            node 2           node N
   TX (GPIO47) ----+---------- D7      +-------- D7      +------- D7
                   |                   |                 |
   RX (GPIO48) ----)----+------|<|---- D9  +---|<|---- D9  +--|<|-- D9
                   |    |    (stripe at D9)
   3.3 V ----[470R]-----+
   GND -------------------------- GND -------------- GND ------------ GND
```

**Longer runs: RS-485.** Past about 1 m, or near motors and radios, use
RS-485 transceivers (~$0.50 each, e.g. MAX3485 / SP3485, 3.3 V parts): one at
the hub and one per node, two twisted pairs (command, return). The simplest
are "TTL to RS-485 with automatic direction" modules: they drive the pair
only while a 0 bit is sent and let go otherwise, which is the same idea as
open drain, so they should need no firmware change. **Not tried yet.**
Transceivers with a separate driver-enable pin need the node to switch it
around its answers, which the firmware does not do yet. CAN (TWAI, ~$1
transceiver per board) is the fallback if polling ever proves limiting.

## Building and flashing

```sh
# Hub (default build polls the bus)
cd firmware/hub-p4 && cargo fw
# One multi-image bus node: node-wifi + node-ble (+ node-sdr if built)
(cd firmware/node-sdr/c5 && cargo fw-bus)
NODE_BUS=1 tools/flash-node.sh <board>
# Single images
(cd firmware/node-wifi/c5 && cargo fw24-bus)     # -> target-bus/
(cd firmware/node-ble && cargo +stable build --release --no-default-features \
    --features esp32c5,wirebus --target riscv32imac-unknown-none-elf)
```

Every image on a bus node must be a bus build (`flash-node.sh` refuses
otherwise): an image built for point-to-point would talk whenever it likes
and garble the other nodes. ESP-NOW (`espnow`) stays as it was, an opt-in
build for a node that cannot be wired; `wirebus` and `espnow` cannot be
combined.

## What the hub prints

* At boot: `hub-p4: node link on the UART1 header ... wired bus`.
* Events: `hub-p4: bus node aa:bb:..:ff joined as 3` (`(again: ...)` for a
  rejoin), `stopped answering`, `answering again`.
* In the 1 Hz `{"t":"hub"}` line: `"bus":{"mode","nodes","present","polls",
  "corrupt","timeouts","join_slots","grants","join_collisions","stray",
  "busy_pct","cmd_bytes","ring_full"}`.
* Right after it, one `{"t":"bus"}` line with each node's counters (the hub
  line's buffer cannot hold 16 nodes): `a` address, `mac`, `x` absent, `p`
  polls, `ok` good answers, `e` empty answers, `to` silent polls, `bad`
  answers refused by CRC, `dup` repeats dropped, `f` frames, `b` bytes, `r`
  mean reply time (us), `rmax` worst reply (us), `wmax` longest gap between
  two good answers (ms), `j` joins.

Each node prints `{"t":"bus","addr",...}` once a second on its USB: polls,
answers, resends, empty, stale (polls skipped because it was late), joins,
granted, lost (addresses given up), cmds (Assigns taken), bad, frames, bytes,
rx_err, tx_err.

## Old gateways on the node link

The hub tells the two kinds of device apart by itself. A device that sends
whole `link` frames without being asked (an ESP-NOW gateway, or one
point-to-point node) while no bus node answers is read the old way: polling
stops and the hub prints `node link carries an unpolled stream`. After 10 s of
silence it tries the bus again. So a kit with the old gateway keeps working
on the new hub firmware. A gateway and bus nodes cannot share the two wires
(the gateway's TX is push-pull and talks whenever it likes); during a
migration either flash the gateway C5 as a bus node, or keep the old gateway
on the node link until all nodes are wired. `--no-default-features` builds a hub that
never polls.

## Bench checklist (needs the wiring above)

1. Hub alone, nothing on the node link: `"bus":{"mode":"bus","nodes":0,...}`, no
   errors.
2. One node (`NODE_BUS=1 tools/flash-node.sh c5a`): within a second the hub
   prints `joined as 1`; the node prints `"addr":1`. In the `{"t":"bus"}`
   line `bad` and `to` stay at 0 (a few `to` right after joining are fine).
   The node appears in the hub's node table under its MAC.
3. Orders: the coordinator's `Assign` reaches the node over the wire (node
   `cmds` counts up; hub shows the node's plan as applied).
4. Three nodes, c5a, c5b and c5c (the old gateway reflashed as a node): three
   different addresses; leave 10 minutes with the test beacon on; expect
   `bad` 0 on every node and tracks on the screen. This is the PLAN's "done
   when" (0 corrupt frames, every role assignment over the wire).
5. Image switch: let the coordinator move a node to Bluetooth; it rejoins
   `(again)` with the same address.
6. Pull one node's D9 wire: `stopped answering` within ~0.2 s, others
   unaffected; plug it back: `answering again`.
7. Restart the hub: every node gets its old address back.
8. Listening time: node-ble adverts/s on the bus against 24.1/s (ESP-NOW) and
   29.4/s (point-to-point); node-wifi's per-channel visits on a 5 GHz plan
   (no channel 6); node-sdr: no uplink windows in its `{"t":"link"}` lines.
9. With an oscilloscope, if one is handy: the return wire's rising edges
   should take well under 0.3 us; if they are slow, use a smaller pull-up
   (330 ohm) or shorter wires.

## Limits and open points

* The protocol, both ends, is host-tested; the firmware glue (interrupt
  executor on the nodes, UART interrupt on the hub, open-drain pin) is not
  yet run on hardware.
* The hub starts a new cycle from its main loop, which stalls up to ~7 ms
  while drawing the screen; answers themselves are followed by the next poll
  from the UART interrupt. A timer interrupt would make the cycle exact.
* A hub restart can repeat the last answer of each node (at most one answer
  per node); repeated detections are harmless to the tracker.
* Up to 16 nodes on the P4 (its node table), 32 addresses in the protocol;
  8 is the size the timing table covers.
* The ESP-NOW gateway firmware has no bus mode (it cannot sit on the bus
  next to wired nodes); it stays the point-to-point gateway it was. The hub
  registers no bus node as a gateway, so a `Via` from the bus is always
  refused; a gateway with a bus mode would need a hub setting naming it.
* RS-485 with driver-enable pins needs node firmware support (not done).
