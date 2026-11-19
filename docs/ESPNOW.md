# ESP-NOW node link (experimental)

Since 2026-11-17 (PLAN.md Phase 5a) the hub's default build uses the
wired bus (docs/WIREBUS.md): no radio transmission and no listening time
lost. The wired bus has not yet been wired on hardware; ESP-NOW is the only
node link proven on hardware so far. ESP-NOW stays an opt-in build for a node that cannot be wired.

A node that cannot be wired to the hub can send its reports over ESP-NOW to a
**gateway**: one XIAO ESP32-C5 (`firmware/gateway`) wired to a hub UART. The
gateway is a relay that holds no secret: it passes each node's packets to the
hub as they were received, and puts on the air what the hub hands it. The hub
checks every packet itself. The onboard C6 of the P4 board was the first
choice, but its factory firmware has no ESP-NOW support (see
`firmware/hub-p4/BRINGUP.md`); owner decision 2026-11-14: C5 gateway for now.

Since the cut-over of 2026-11-18 (link protocol 14, ESP-NOW packet version 3,
docs/LINK-KEYS-DESIGN.md) every node has a key of its own, kept in a small
flash partition of its own and never in a firmware image. Older notes on this
page that speak of one kit key, a gateway that checks tags, or `Via` frames
on the gateway's wire describe the firmware before that date.

## What goes over the air

`crates/airlink`: a 6-byte header (`RS`, version, kind, sequence number),
then the payload. Up to 1024 bytes per packet (ESP-NOW v2 allows 1470; both
ends report version 2).

| Version | Used by | What it is |
|---|---|---|
| 3 | a sealed kit | every packet but the unprovisioned announcement ends in a freshness stamp and an authentication tag (below) |
| 1 | an open kit | the same packets with no stamp and no tag: nothing is authenticated |
| 2 | nothing any more | the retired one-key-per-kit packet. Recognised by its header and refused as `old_firmware`: that is how a board still on the old firmware shows up |

Version 3 kinds:

* **Beacon**, made and sealed by the hub twice a second (one in every 500 ms, at a place in it
  that varies, so a node that listens only briefly each second cannot stay out of step), broadcast by the
  gateway on its channel (2.4 GHz ch 6). It carries one short tag per node,
  each under that node's key. Nodes learn the gateway's address from it.
* **Frames** (a node batch), unicast to the gateway. Unicast gets the radio's
  ACK and automatic retries; broadcast gets neither. Sequence numbers let the
  hub count real losses, and a byte-identical retry is dropped at the gateway.
* **Sync** and **Command**, sealed by the hub for one node, unicast by the
  gateway (below).
* **Unprovisioned**, an unsealed announcement from a node that has no usable
  key record, once every 10 s (see "When something is wrong").

## Efficiency choices

* **Batching**: frames wait up to 50 ms and go as one packet. Every ESP-NOW
  packet costs preamble, header, ACK and contention whatever its size, and a
  Wi-Fi node cannot hear drones while it transmits.
* **No channel switching for the 2.4 GHz node**: channel 6 is 300 of every
  400 ms of its hop plan, so it sends only while it is there. The hopper waits
  for a send in flight before retuning (`uplink::wait_idle`), so sends are
  not cut off.
* **Retry, then drop**: a failed batch is retried for 2 s (sealed afresh for
  every attempt), then dropped and counted (a dead gateway must not back the
  node up). A gateway neither heard nor reached for 30 s is forgotten until
  its next beacon.
* **Same frames as the wire**: nothing is re-encoded on the node, the gateway
  or the hub.

## On the hub's side

The gateway speaks `link::gw` on its UART, never ordinary link frames
(protocol 14; XIAO D6 = GPIO11 to the hub, D7 = GPIO12 from it, 921600
baud). Up, each node packet arrives whole as `GwUp::Packet`, with the radio's
source address, its RSSI and how long it waited in the gateway. The hub (the
P4, `firmware/hub-p4/src/links.rs` with the logic in `crates/hublink`)
looks the address up in its roster, checks the packet's tag and freshness
with that node's key, and only then sorts the frames inside it like any
node's (see "Who may send what"). A node's number on the hub is its roster
slot plus one, the same across reboots. The gateway's own reports are
`Hello` (protocol, firmware, its address, channel), `Refused` (packets it
dropped, per address and reason, once a second), `TxDone` and `Stats`.

The hub recognises the gateway by its protocol 14 `Hello` and then reads the
link as a gateway link; until then it reads link frames (a wired node, or the
wired bus). A gateway that answers with an older protocol is refused on a
sealed hub and shows as `OLD FIRMWARE: gateway`.

## Downlink: hub to node

The hub decides what every node listens to (`crates/coverage`, see its
README) and sends each node a `link::Command::Assign`. For a relayed node:

1. **Hub.** It seals a Command packet for that node with that node's
   downlink key and hands it to the gateway as `GwDown::Transmit`, with how
   long to keep trying (2 s for commands). It decides every 2 s, sends a
   changed assignment at once and resends every assignment every 5 s. The
   echo of the plan id in the node's `Status` is the proof it arrived. A
   `TimeSync` (aero nodes) goes alone in its own packet, sealed the moment it
   is written, marked urgent, with 300 ms to live. A newer command for a node
   replaces a queued one (`GwDown::Cancel`).
2. **Gateway.** It sends the packet unicast at once, again whenever it hears
   that node (the node is on channel 6 then) and blind every 50 ms, until the
   radio acknowledges it or the time runs out; urgent ones first. Each ends
   in one `GwUp::TxDone` (acknowledged or not, attempts, how long it was
   held). The gateway cannot change a packet: the node would refuse it.
3. **Node.** It takes a Command only if the tag is right for its own key and
   the stamp echoes its own random session (made this boot, after a packet it
   sent), is newer than the last one taken, and is not late (below).

The gateway's USB log has a `"t":"gw"` line once a second: what it heard,
relayed and refused (per reason), the hub's messages, beacons sent, and the
transmit queue (`tx_acked`, `tx_given_up`, ...). It always says
`"key":"none"`.

**Node side.** node-wifi, node-ble, node-sdr and node-lr take the command
packet in their uplink and hand each `Assign` to their `role` module:

* node-wifi applies a Wi-Fi job at once (new channel plan, band chosen per
  hop, so a 5 GHz plan's channel-6 visits work) and echoes the plan id in
  `Frame::Status`; it still sends only while tuned to 6. A plan that would
  keep it away from channel 6 for more than a second is refused.
* node-ble echoes the plan id (Bluetooth has no plan). It parks Wi-Fi on
  channel 6 and stops its scan for 200 ms of every second to send and to
  hear the gateway; that costs ~18% of the adverts it hears (its README).
* A role the running image cannot do switches image (OTA slots,
  `firmware/node-common`, `tools/flash-node.sh`), at most once a minute,
  backing off to once an hour if asked again and again; the other image
  boots with the job and reports it. Every image on a board uses the same
  key record and the same MAC, so the hub keeps tracking it across the
  switch.
* A node that hears no genuine hub packet for 60 s on a plan the hub gave it
  goes back to its built-in plan (the "deaf fall-back") and says so.

## Which nodes can use it

| Node | ESP-NOW | Why |
|---|---|---|
| Wi-Fi 2.4 GHz (S3, or C5 `cargo fw24-now`) | yes, built and tested | already dwells on ch 6 |
| Wi-Fi 5 GHz (C5) | yes (same image, band chosen at run time) | 40 ms on ch 6 at least once a second, from the plan |
| BLE (C5, `espnow` feature) | yes, built and tested | time-shares the radio: scan off 200 ms of every second (~18% fewer adverts) |
| Raw capture (node-sdr, `cargo fw-now`) | yes, built and tested (2026-11-14, c5b) | pauses capture for a short window, retunes to ch 6, sends, re-locks receive-only; applies `Assign` (firmware/node-sdr/README.md "Hub link") |

"Tested" above means the link before 2026-11-18. The version 3 link is
host-tested and compile-checked; its bench pass is docs/BENCH.md, "Link
keys: bench checklist".

## Trade-offs to know

* **ESP-NOW nodes transmit.** A wired node is receive-only and invisible; an
  ESP-NOW node and the gateway (its beacons) can be seen by anyone scanning,
  and their MAC addresses are fixed. That is why this is a build option, not
  the default.
* **Kits ship open.** A board with an open record runs the unauthenticated
  version 1 link: anyone nearby can then send packets that the hub takes as a
  node's (a fake track, or a real node's reports drowned out), or send a node
  a fake assignment (blind it). The hub shows `OPEN LINK` while it is open.
  Sealing a kit takes a few commands (below).
* **Sealed, every packet both ways carries a 96-bit HMAC-SHA256 tag** under
  that node's own key and a freshness stamp, in our own trailer. Forged,
  replayed and late packets are refused and reported to the hub as an attack
  (`ForgedLink`). This is our own scheme rather than ESP-NOW's encryption,
  which allows only about 6-7 encrypted peers (Biscuit Shop's "Secure" mode
  caps at 6 nodes for what looks like this reason); ours has no peer limit.
  Nothing is encrypted: detections are what anyone nearby can hear anyway.
* **A board's key is in its flash, unprotected.** Flash encryption and secure
  boot are off (a later package). Someone who takes a node can read that
  node's key and speak as that node, and nothing more; revoke it (below).
  The hub holds the kit master: **a captured hub is a captured kit** until a
  secure element is fitted.
* **Jamming still works.** Keys stop forgery and replay, not someone
  drowning channel 6 in noise, or a forger flooding one node's address so the
  gateway's per-address budget crowds that node out. Both show: as jamming,
  nodes going quiet, `over_budget` refusals, and the hub's "Node reports
  going missing" alarm.
* The Biscuit Shop (CodeHedge) publishes no ESP-NOW details: its C5 "Nodes"
  report to a "Manager", but the protocol, packet format and source are
  closed. Nothing here is taken from it.

## Securing the link

In one paragraph: the owner's computer makes a **kit master** key once.
Each ESP-NOW node gets a **key of its own**, worked out from the master and
the node's identity (its roster slot, a generation number and its MAC
address), written to a small partition (`rskey`) that firmware updates never
touch. The **hub** holds the master and the **roster** (which slot is which
board) and works out every node's key at boot. The hub checks every node
packet and seals everything that goes to a node. The **gateway holds
nothing secret**. Firmware images hold no key at all: they are the same for
every kit, and sharing an image or a flash dump of the gateway gives nothing
away.

### Who holds what

| Where | Holds | Secret? |
|---|---|---|
| Your computer, `tools/kit.key` | the kit master | **yes**. Git-ignored, readable only by you. Back it up privately |
| Your computer, `tools/kit.roster` | slot, generation, MAC, key check and a label per node | no (git-ignored because it is per kit) |
| Each node, `rskey` partition | its own key, slot, generation, MAC, kit id, key check | yes, that node's only |
| Hub, `rskit` partition | the master and the roster | **yes** |
| Hub, `rsboot` partition | the boot counter (one value per boot; the link's epoch and the evidence log's boot number) | no |
| Gateway | the roster's MAC addresses, pushed by the hub, in RAM only | no |

Nothing ever prints a key. The **kit id** and each node's **key check**
(eight hex digits each) name them without giving them away: `link-key show`
prints them, and each board prints its own at boot, so a mismatch is a
mistake you can see.

### What a capture gives away

| Someone takes | They can | They cannot | What to do |
|---|---|---|---|
| One node | send reports as that node; read its own traffic | speak as another node; send any other node a command, plan, image switch, time or beacon; learn the master | revoke its slot, push the roster, provision a replacement (below). The rest of the kit is untouched |
| The gateway | drop, delay, reorder or repeat traffic (bounded, counted, visible on the hub) | forge or alter any packet either way: it holds no key | reflash or replace it; there is nothing to rotate |
| The hub | everything (the master is in its flash) | | make a new kit master and provision every board again |
| A flash dump or shared image of any board but a node or the hub | nothing | | |

Reading a board's flash takes the board and a USB cable for about a minute.
Until secure boot exists (a later package), a captured board can also be
reflashed with hostile firmware that keeps its key: a captured node stays a
hostile node until its slot is revoked.

### Kits ship open; sealing yours

A kit as shipped has **open** records on every board: it works, unauthenticated,
and the hub's strip says `OPEN LINK`. To seal it, from the repository folder,
with the boards on USB and registered with `tools/dev-board.sh --register`
(the P4 hub's port is not an Espressif one: add a `p4` line to
`tools/boards.local` by hand, see docs/BENCH.md "Link keys: bench
checklist"):

1. **Make the kit (once):** `cargo run -p link-key -- kit new`. It writes
   `tools/kit.key` (secret) and an empty `tools/kit.roster`, and prints the
   kit id. Back `kit.key` up somewhere private (a password manager, a USB
   stick in a drawer). Never paste it anywhere.
2. **Flash current firmware on every board** (images carry no key, so this
   can come first or last): the hub with `tools/partitions-hub.csv` as its
   partition table (it adds `rskit` and `rsboot` after Elecrow's own rows),
   the gateway as before, each multi-image node with `tools/flash-node.sh`.
3. **Provision the hub:** first `cargo run -p link-key -- hub p4 --dry-run`,
   which prints the exact commands without touching the board, then
   `cargo run -p link-key -- hub p4`. It writes the master and roster to
   `rskit` and seeds the boot counter (above the old one if the hub already
   had a readable counter).
4. **Provision each node:** `cargo run -p link-key -- node c5a --label "roof"`
   (it reads the board's MAC, takes the next free slot, writes `rskey`, and
   records the slot in the roster), then push the roster to the hub:
   `cargo run -p link-key -- hub-roster p4` (or add `--hub p4` to the node
   command). `hub-roster` sends the roster only, never the master.
5. **Check:** each node's boot line (below) shows the kit id and the same key
   check that `cargo run -p link-key -- show` lists for its slot, the hub's
   strip no longer says `OPEN LINK`, and the nodes keep reporting.

Every `link-key` command that writes a board accepts `--dry-run`. A key
image goes to a 0600 file in a private temporary directory, `espflash
write-bin` writes it, and the file is overwritten and deleted at once, also
if you press Ctrl-C (the command then ends with "interrupted" and status 130:
run it again). Only `kill -9` or a power cut in that second can leave the
file behind (`link-key-<pid>-<random>` in the system temporary directory;
delete it). Single-image nodes (S3 Wi-Fi, C3 aero) need
`--table tools/partitions-single.csv` and must be flashed with that table.

**Back to open on purpose:** `link-key open <board>` (a node) or `link-key
open p4 --hub`; `tools/flash-node.sh --open <board>` then calls an open node's
flash good. Revoke an opened node's slot if it was in the roster.

### Checking it worked

* **Node boot line (USB):** `{"t":"uplink","auth":"kit:<kit id> slot:<n>
  gen:<g> check:<key check>","rng":"radio","link":"v3"}` (`"rng":"trng"`
  on the S3 and C3). `tools/flash-node.sh`
  reads it and calls the flash good only when it is there (or an open record
  with `--open`). An open record prints a `"t":"warn"` line with
  `"auth":"open"`; a missing or unusable record prints `"auth":"none"` with
  the reason (`missing`, `damaged`, `wrong_board`, `no_rng`).
* **Node:** once it has refused anything, a `{"t":"link_auth",...}` line
  comes with its periodic uplink line and counts every hub packet it refused,
  by reason (`bad_tag`, `replay`, `stale`, `late`, `old_firmware`,
  `not_for_this_slot`, `unproven`, ...). On a healthy kit the line does not
  appear, apart from a few `unproven` or `stale` around a hub restart.
* **Gateway:** its `hello` line says `"key":"none"` and "relay only"; its
  `"t":"roster"` line says when the hub's first roster arrived (before it,
  no node packet is relayed); the `"t":"gw"` line counts `not_in_roster`,
  `over_budget`, `old_firmware` and `malformed` refusals.
* **Hub console:** at boot `hub-p4: linkauth: rskit keyed, kit <id>, <n>
  node slot(s) loaded`, one `slot <s> = node <number> <mac>` line per slot,
  and `rsboot written <value> ...: this boot's epoch`. Every second the
  `hub-p4: gwlink` line counts packets taken and refused per reason
  (`bad_tag`, `replay`, `stale`, `old_fw`, `not_in_roster`, `late_uplink`,
  ...), syncs, commands and time syncs. Refusals reach the tracker and the
  screen as `Threat` frames of kind `forged_link` naming the claimed node, at
  most once a second per node and reason.

### Revoking and replacing a node

A node lost, stolen, or suspected:

1. `cargo run -p link-key -- revoke --slot <n>`, then
   `cargo run -p link-key -- hub-roster p4`. The hub stops deriving that
   slot's key, gives its beacon tag zeros and leaves its address out of the
   roster it pushes to the gateway, which then refuses the node's packets as
   `not_in_roster`. The slot stays taken.
2. A replacement (or the same board, once you trust it again):
   `cargo run -p link-key -- node <board> --slot <n>`, then `hub-roster`. It
   gets the next generation of that slot: a key unrelated to the old one, so
   the old board's key is worth nothing. The node keeps its hub node number
   (slot + 1), and the hub drops any mutes and snoozes that were on that
   number, so a mute never carries over to a different board.

Re-provisioning the **hub** (a new board, or after a wipe) is `link-key hub
p4` again. When the old boot counter still reads, the new one starts above
it and the nodes follow at once; otherwise they follow within about a minute
(each node starts over after 60 s without a packet it can take). No node
needs reflashing either way. A new **kit master** (`kit new --force`) means
provisioning the hub and every node again.

### When something is wrong: what the hub's screen says

The bottom strip of the P4's screen keeps one line for the link, and says
nothing there while all is well.

| Strip | Means | Do |
|---|---|---|
| `OPEN LINK` | the hub has an open record: nothing on the ESP-NOW link is authenticated | seal the kit (above), or leave it open on purpose |
| `NO KIT KEYS: link closed` | the hub has no `rskit` records (never provisioned, or flashed with a table that has no `rskit`) | flash with `tools/partitions-hub.csv`, then `link-key hub p4` |
| `KIT RECORDS DAMAGED: link closed` | neither copy of the master or roster reads back good | `link-key hub p4` again |
| `NO BOOT COUNTER: link closed` | no boot counter could be recorded this boot (missing, damaged, used up, or a `rskit-readonly` build). An epoch must never repeat, so the keyed link stays closed | `link-key hub p4` (it seeds `rsboot`); with `rskit-readonly` this is expected |
| `ROSTER FROM OTHER KIT` | the roster belongs to another kit (or no slot's key check matched): keyed with no nodes | `link-key hub-roster p4` from the right kit folder |
| `OLD FIRMWARE: gateway` / `: <n> nodes` | packets in the retired version 2 format, or a gateway speaking an older protocol | reflash that board |
| `UNPROVISIONED NODE <n>` | `<n>` different boards announced in the last minute that they have no usable key record (anyone can send that announcement: it is information, not proof) | provision them, or find out whose they are |
| `BENCH GATEWAY` | the gateway is a `gwforge` bench build that forges traffic on purpose | never leave it in a fielded kit |

Threats on the screen and in the evidence log: **Forged node traffic**
(`forged_link`: packets refused, the reason in its `detail`; the codes are in
`link::ThreatKind::ForgedLink`'s documentation) and **Node reports going
missing** (`link_loss`: a node says it sent at least 50 frames more in the
last minute, or the last five, than the hub received, and more than a fifth
of what it sent;
something is swallowing its packets after the radio acknowledged them). An
unprovisioned node's LED blinks where it has one.

### Boards still on the old firmware

Firmware before 2026-11-18 sealed every packet with one key per kit,
compiled into each image from `tools/link.key`. That key is gone from the
build: nothing reads `tools/link.key` or `ROTORSCOPE_LINK_KEY_FILE` any more,
and `link-key new` / `link-key id` are removed. A board still running that
firmware is refused as `old_firmware` and shows on the strip; reflash it and
provision it as above. If you still have a `tools/link.key`, it stays
git-ignored; keep it only until every board is reflashed (it is the key of
those old boards), then delete it.

### How it works (for reviewers)

The reference is docs/LINK-KEYS-DESIGN.md (as built). In short:

* **Keys** (`crates/kitkeys`): node key = HMAC-SHA256(master, label, slot,
  generation, MAC); separate up and down keys from it, so a packet can never
  be reflected back the other way. Records with two copies and a CRC each;
  the node checks the record's MAC against its eFuse MAC at boot.
* **Packets** (`crates/airlink/src/v3`): tag = HMAC-SHA256 over the sender's
  MAC and everything before the tag, truncated to 12 bytes (8 per node in a
  beacon). The sender's address is covered, so a genuine packet from one node
  cannot pass as another's.
* **Freshness:** the hub's clock counts 10 ms ticks; the epoch is the hub's
  boot counter. A node takes a hub packet only if it is newer than the last
  one and at most 3 s late (0.5 s for a `TimeSync`; about 0.1 s more per
  second the node was jammed before); the hub refuses a node batch older than
  2 s or more than 0.5 s ahead. Commands echo the node's random per-boot
  session, so nothing recorded before a node reboot works after it.
* **Gateway** (`crates/airlink/src/relay.rs`, no key type even named there):
  roster filter, 40 packets/s and 16 KB/s per address, 50 KB/s in total,
  byte-identical retries dropped, round-robin per address.
* **Cost**: 28 bytes per packet; about 0.24 ms of the P4 to check a full
  1 KB packet (measured), a few percent of the hub's time at the UART's line
  rate.
* **Defence in depth on the radio**: every radio firmware carries a patched
  esp-radio that drops an impossible received ESP-NOW length, and
  `tools/check-wifi-blobs.sh` checks that the Espressif radio libraries are
  the ones with the fix for CVE-2025-52471 (firmware/patches/README.md).

## Who may send what

The key proves a packet came from one particular node of this kit. It does
not say what that node may send. Every hop also checks **origin**: who may
originate each frame type (`crates/link/src/origin.rs`, one table with no
catch-all, so a new frame type does not compile until someone decides).

| Who | Frames |
|---|---|
| A node | `Hello` and `Status` with a node role, `Detection`, `Stats`, `NodeHealth`, `OpenLink`, `DroneId`, `Rf`, `DjiWifi`, `WifiDrone`, `Air`, `AeroStatus`, `Control`, `ControlEnergy`, `MavlinkEvidence`, `Threat` of every kind a node detects |
| The gateway | only its own `link::gw` messages (`Hello`, `Packet`, `Refused`, `TxDone`, `Stats`); it never sends link frames |
| The hub only (to its web head) | `HubFix` (the kit's GPS position and UTC), `Csbee` (aircraft from the hub's ADSBee), `Flags` (FAA flags), `Threat` `area_gnss`, `id_flood` and `link_loss`, and its own `forged_link` reports (filed in the table under the gateway role, which no node may take) |
| Bus control (wired bus only) | `BusJoin`, `BusDone` |

Where it is enforced:

* **P4 hub** (`firmware/hub-p4/src/links.rs`, `link::origin::node_batch`):
  the frames inside a verified node packet are taken only if a node may send
  them. Anything else (a hub-only frame, a `Via`, bus control, a frame that
  does not decode) is dropped, counted (`refused=` on the once-a-second
  `hub-p4: link` line, `"refused"` in the `{"t":"hub"}` line) and raised once
  a second per node as the hub's own `forged_link` threat, `detail` 5
  (origin). The same sorter guards the wired bus and a point-to-point wired
  node.
* **Web head feed** (`crates/link/src/forward.rs`): the same sort, so a
  frame refused at the hub is never passed on, and `HubFix`, `Csbee` and
  `Flags` reach the web head only as the hub's own (`refused=` on the
  `hub-p4: webhead` line).
* **Web head** (`firmware/hub/src/links.rs`, `link::origin::Sorter`), the
  last line of defence: each link is sorted the same way, except that the
  P4's link (`Sorter::hub_link`) also takes the hub-only frames when no
  `Via` comes before them. In a `webhead` build only link 0 is the P4's, so a
  node wired straight to another link cannot send them; a build without
  `webhead` takes them on any link. Refusals count in the `hub` line's
  `relay_refused`.

So a captured node, which can seal packets as itself, still **cannot** move
the kit's own position or clock, show made-up aircraft from the hub's ADS-B
receiver or made-up FAA flags, name another node as the sender of its frames,
pass as the gateway or the hub, or inject wired-bus control frames, and every
attempt is counted and reported (`forged_link` 5). What it **can** do is
report made-up detections as itself, which is why a node you no longer trust
gets its slot revoked.

On an **open** kit anyone in range can send node packets under any address,
with the same origin limits.

## Bench results before the cut-over

The runs below (2026-11-14 to 2026-11-15) used the link as it was then: one kit key or none,
and a gateway that split batches into frames and wrote `Via` markers to the hub. The radio
numbers (loss, rate, wait per send) still describe ESP-NOW on this hardware.

### Bench results (2026-11-14, c5a node, c5c gateway, c5b test beacon, ~1 m)

| Run | Node | Gateway | Result |
|---|---|---|---|
| Idle, 50 s | 50 packets, 1 failed send | 55 packets, 1 "lost" (the failed send; node then advanced its sequence, fixed since) | PASS |
| Test beacon (Wi-Fi beacon + NAN, ch 6), 75 s | 107 Remote ID detections, 182 packets, 0 failed, 0 dropped | 227 packets, 0 lost, 0 duplicates, RSSI -17 dBm | PASS |
| Same, gateway decoding every relayed frame, 60 s | 0 failed; 2 batches dropped while the gateway was being reflashed (by design) | 160 frames decoded, 0 corrupt, 86 detections, 0 lost | PASS |

Average wait per send (queue + air + ACK) was about 3.7 ms.

#### PHY rate (2026-11-14, same setup, test beacon on, 60 s per rate)

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

#### Two nodes (2026-11-14, c5a and c5b both ESP-NOW nodes, 60 s)

Gateway tracked both separately: 86 and 74 packets, 0 lost, 0 duplicates,
191 frames decoded, 0 corrupt.

Flash: `tools/dev-board.sh firmware/gateway/target/riscv32imac-unknown-none-elf/release/gateway 60 c5c`
and the node from `firmware/node-wifi/target-espnow/...`.

### Not done yet

* Range test (walk the node away; then try the Long Range PHY).
* More than two nodes, and two nodes under heavy traffic.
* ~~The node side of the downlink (applying an `Assign`) for Wi-Fi and BLE nodes; node-sdr applies it (DroneID and FPV plans).~~ *Update 2026-11-18: done; Wi-Fi and BLE nodes apply it too ("Node roles and image switching", 2026-11-15, below).*
* The S3 build of node-wifi with `espnow` compiles; it has not been run.

#### Node roles and image switching (2026-11-15, c5a node, c5c gateway, P4 hub)

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
