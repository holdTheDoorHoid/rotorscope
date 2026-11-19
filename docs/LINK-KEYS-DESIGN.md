# Link keys: design (WP-13)

Status: **design only**, nothing here is implemented yet. It replaces the one-key-per-kit
ESP-NOW authentication described in [ESPNOW.md "Securing the link"](ESPNOW.md#securing-the-link)
and changes the link protocol. Every board must be reflashed; a kit with mixed firmware
fails closed and says so.

Decisions that belong to the owner are marked **Owner** with a recommendation.

## 1. What is wrong today, in one paragraph

Every ESP-NOW board (gateway and every node) carries the same 32-byte kit key, compiled into
each firmware image. Flash encryption and secure boot are off, so anyone holding one board
for a minute can read the key over USB. With it they can do everything the kit can: make up
detections from any node, send any node any command (role, plan, image switch, time), and
pose as the gateway. The key is also in every build output, so sharing an image or a flash
dump shares the key. Separately: freshness is order-based and loose (a withheld packet can
be released seconds later), a radio-level ACK counts as delivery, nothing on the node limits
image switches, a full 5 GHz plan can lose its gateway-channel visit and leave a node deaf,
and node numbers on the hub are reused, which can move a mute onto a different node.

## 2. Goals and non-goals

Goals:

1. A captured **node** gives away that node only: it cannot speak as another node, and it
   cannot command, re-time, re-plan or image-switch any other node.
2. A captured **gateway** gives away nothing that lets it forge traffic in either direction.
3. Keys are not in firmware images. Images are identical for every kit.
4. A mixed or unprovisioned kit fails closed, visibly, on the board and on the hub screen.
5. Bound how late a withheld packet can be accepted, both ways.
6. Node robustness: image-switch rate limit, plans that cannot make a node deaf, delivery
   accounting the hub can check, true randomness after the radio starts.

Non-goals (other packages): flash encryption, secure boot, JTAG/download-mode lock-down
(WP-15); keys on the wired bus (section 4.6); confidentiality (detections are what anyone
nearby can hear); jamming (detected elsewhere).

## 3. Recommended design in brief

* A **kit master** key (32 bytes) is made once on the owner's computer. Each ESP-NOW node
  gets a **node key derived from the master and its identity** (slot, generation, MAC).
  The node stores only its own derived key, in a small **provisioning partition** that image
  switches and application reflashes never touch.
* The **hub verifies and seals everything end to end**. The **gateway holds no secret**: it
  forwards sealed packets to the hub over its UART and transmits packets the hub sealed. It
  keeps cheap, non-secret filters (roster MACs, per-MAC rate budget, size, version).
* The hub derives the node keys from the master **once at boot** (from flash now; from a
  secure element later, which is then never in the per-frame path) and keeps them in RAM.
* **Broadcast beacons carry one short tag per node** (each under that node's key), so a
  node can check a broadcast without any shared group key.
* The freshness clock becomes **time-based** (one tick per 10 ms of hub uptime) instead of
  a packet count, which tightens every lateness bound to seconds and lets the hub bound the
  age of node batches too.
* Open kits are a **provisioning choice** (an "open" record), not a build choice.

## 4. Keys

### 4.1 Derivation

All HMAC-SHA256 (the existing `airlink` HMAC), labels are ASCII, integers little-endian.

| Value | Definition | Secret? |
|---|---|---|
| `M` | 32 random bytes from the computer's OS random source (`link-key kit new`) | yes |
| `kit_id` | `HMAC(M, "rotorscope/kit-id/v1")[0..4]` | no, printed |
| `K_node` | `HMAC(M, "rotorscope/node-key/v1" ‖ slot:u8 ‖ gen:u16 ‖ mac:[6])` | yes |
| `K_up` | `HMAC(K_node, "rotorscope/up/v1")` (node to hub) | yes |
| `K_down` | `HMAC(K_node, "rotorscope/down/v1")` (hub to node) | yes |
| `key_check` | `HMAC(K_node, "rotorscope/check/v1")[0..4]` | no, printed |

What identifies a node:

* **`mac`**: the station MAC from eFuse, the address the radio reports as sender. Binding
  the key to it means a record copied to another board does not work there (the firmware
  checks the record's MAC against eFuse at boot), and a packet claiming one MAC is checked
  with that MAC's key only.
* **`slot`** (0..15): the node's place in the kit roster. It is the node's stable number on
  the hub (section 7.5) and its index in the beacon's tag list.
* **`gen`**: bumped whenever a slot is re-provisioned, so a replacement board, or the same
  board after a suspected leak, gets a key unrelated to the old one.

Separate up and down keys mean a packet can never be reflected back in the other direction,
whatever its kind byte says.

Rejected: deriving from the MAC alone (no revocation without a new master; MACs can be
claimed); a random per-node key with no master (the hub would then need a copy of every key
from the computer, and adding a node would need the hub on the bench too; the master costs
nothing and fits a secure element later); ESP-NOW's built-in encryption (about six encrypted
peers, as today's docs note).

### 4.2 Who holds what

| Device | Holds | Where |
|---|---|---|
| Owner's computer | `M`, the roster (slot, gen, MAC, key_check, label) | `tools/kit.key`, `tools/kit.roster`, both git-ignored |
| Node | its own `K_node` (with slot, gen, MAC, kit_id, key_check) | `rskey` partition |
| Gateway | **nothing secret**; the roster MAC list (pushed by the hub, RAM only) | none |
| Hub, no secure element (until one is fitted) | `M`, roster, boot counter; derived `K_up`/`K_down` per slot in RAM | `rskit` partition; RAM |
| Hub, with secure element | roster, boot counter in flash; `M` inside the element; derived keys in RAM | element; `rskit`; RAM |

The roster holds no secrets: an entry for a MAC grants nothing without that MAC's derived
key. The hub derives at most 16 keys at boot (a slow element at tens of milliseconds per
HMAC adds well under a second to boot) and never calls the element per frame. The keyed
HMAC state (both pads absorbed, as `airlink::auth::Key` does today) is what sits in RAM.

**Owner: the master on the hub before a secure element exists.** Recommended: in hub flash
(`rskit`), documented as "a captured hub is a captured kit until the secure element is
fitted". One code path, the same as with the element (`KeySource::derive(slot, gen, mac)`),
and the roster stays secret-free. Alternative: keep `M` on the computer only and write the
derived node keys to the hub. A captured hub then still yields every current node key, so the
practical gain is small, and every node addition needs the hub on the bench.

### 4.3 What a capture gives an attacker

| Captured | They can | They cannot | Recovery |
|---|---|---|---|
| One node | Send reports as that node; read its own traffic | Speak as another node; send any other node a command, plan, image switch, time sync or beacon; learn `M` | `link-key revoke --slot N` (hub roster), then re-provision a replacement at `gen+1`. The rest of the kit is untouched. |
| Gateway | Drop, delay, reorder, duplicate (all bounded, counted, visible) | Forge or alter any packet in either direction (it holds no key) | Reflash or replace it; nothing to rotate |
| Hub, no element | Everything (`M` is in flash) | - | New master, re-provision every node (as today) |
| Hub, with element | Forge as any current node while they hold it powered and can read its RAM (JTAG is still open until WP-15) | Extract `M`; make keys for new slots or generations | Bump `gen` on every slot (re-provision nodes); `M` stays |
| Wired-bus access | Same as today: report as any MAC it joins with; no hub-only frames | (unchanged) | - |

A flash dump or shared image gives nothing: images carry no key.

### 4.4 The provisioning partition

| Board | Partition | Location |
|---|---|---|
| Multi-image C5 node | `rskey`, data, subtype `0x40`, 4 KB | `0x611000` in `tools/partitions-node.csv`, after `rsjob` (space is free up to 8 MB) |
| Single-image node (S3 Wi-Fi, C3 aero, C5 single-image builds) | `rskey`, same | new `tools/partitions-single.csv` (nvs, phy_init, factory, rskey). These boards use espflash's default table today, which has no spare data partition |
| P4 hub | `rskit` (8 KB: two copies of master + roster), `rsboot` (8 KB boot counter) | new `tools/partitions-hub.csv` |
| Gateway | none | - |

All app slots share the one `rskey`, so every image on a board uses the same key. Nothing
the firmware does writes to it. `tools/dev-board.sh` erases `otadata` and `rsjob` by default
and must never add `rskey` to that list (a test in the tool's dry-run output checks this).
`espflash flash` rewrites only the bootloader, the partition table and the app, so app
reflashes leave the key in place as long as the table keeps the same `rskey` offset. That
offset is therefore fixed for good.

Node record (two copies in the partition, at `0x000` and `0x800`; 64 bytes each):

```text
magic "RSK1" (4) | type (1: 1 node, 2 open) | slot (1) | gen (2) | mac (6) | kit_id (4)
| K_node (32) | key_check (4) | reserved, zero (6) | CRC-32 over the rest (4)
```

An open record has type 2 and zeros for every field after it. Hub record (`rskit`, two
copies): `"RSH1"`, type (1 keyed, 2 open), kit_id, `M` (zero when the element holds it),
roster count, then 16 entries of `slot, gen, mac, key_check, revoked`, then a CRC-32.
Boot counter (`rsboot`): an append-only log of 32-bit values (one written per boot,
the sector is erased only when full), seeded with a random value from the computer when the
hub is provisioned.

At boot the firmware takes the first copy with a good CRC and checks the record's MAC
against eFuse. The outcome is one of:

| State | Node does | Seen as |
|---|---|---|
| Keyed, good | Normal keyed link | USB `{"t":"uplink","auth":"kit:<kit_id> slot:<n> gen:<g> check:<key_check>"}` |
| Open record | Unauthenticated link (version 1 packets) | USB warning as today; hub strip `OPEN LINK` |
| Empty, both copies damaged, or MAC mismatch | **No reports over ESP-NOW.** Every 10 s: a USB warning naming the reason (`missing` / `damaged` / `wrong_board`) and one unsealed `Unprovisioned` announcement on the gateway channel | hub strip `UNPROVISIONED NODE` with the count, from the announcements; the board's status LED blinks where it has one |

The announcement is information only: anyone can send one. The gateway forwards at most one
a second in total, and the hub shows only how many distinct MACs it has heard in the last
minute.

### 4.5 Tool flow

`tools/link-key` gains subcommands; every board access goes through `tools/dev-board.sh`
and the lab lock. The key never appears on a terminal. It goes from the tool to a 0600 file
in a private temporary directory, then to `espflash write-bin` at the partition's offset,
and the file is overwritten and deleted at once.

| Task | Command (sketch) |
|---|---|
| Make a kit | `link-key kit new` writes `tools/kit.key` and an empty roster; prints `kit_id` |
| Seal a kit that shipped open | the same, then provision the hub and every node below |
| Provision the hub | `link-key hub <board>` writes `rskit` (master and roster) and seeds `rsboot` |
| Add a node | `link-key node <board> [--slot N]`: reads the MAC with `espflash board-info`, takes the next free slot (or bumps `gen` of slot N), writes `rskey`, adds the roster line, then pushes the roster to the hub (`link-key hub-roster <hub-board>`: roster only, never the master) |
| Replace a node | `link-key node <new-board> --slot N` (`gen+1`, new MAC), then `hub-roster` |
| Revoke | `link-key revoke --slot N`, then `hub-roster` |
| Open kit | `link-key open <board>` (node or hub). `tools/flash-node.sh --open` keeps working as a shortcut |
| Check | `link-key show` prints the roster with `key_check`; each board prints its own at boot; a mismatch is a mistake you can see without any secret leaving a board |

Flashing firmware no longer involves keys: `flash-node.sh` stops checking key IDs in ELFs and
instead reads the boot line and refuses to call the flash good unless it shows `auth` keyed
(or open with `--open`). Kits ship with open records on every board and the docs say so.
The buyer seals the kit with the first four rows.

### 4.6 The wired bus and point-to-point wires stay unkeyed

Recommended (**Owner** to confirm): no keys on the wired bus in this package. Whoever can
touch the bus wires can touch the hub. The origin checks already stop the hub-only frames.
Sealing bus answers would cost every bus node a key record and the hub a verify per answer,
and the change is easy to add later, because the same sealed packet would simply ride inside
a bus answer.

## 5. Where frames are verified

**Recommended: the hub verifies, the gateway is a secret-free relay.**

| Option | Captured gateway | Per-frame cost | Rejected because |
|---|---|---|---|
| A. Gateway holds `M` (or all node keys) and verifies, as today | whole kit | C5 | the gateway is the most exposed box (it transmits beacons and is easy to find) |
| B. Gateway holds only uplink keys, hub holds downlink keys | can forge every node's reports | C5 | forging detections is the main harm |
| **C. Gateway holds nothing; the hub verifies uplinks and seals downlinks** | drop/delay only | P4 | chosen |

Cost. One HMAC-SHA256 over a packet takes `ceil((len + 6 + 9) / 64) + 1` SHA-256 blocks:
18 for a full 1024-byte packet, 3 for a 40-byte one. Software SHA-256 is roughly 2-3 k
cycles a block on these RISC-V cores. That makes about 0.15 ms per full packet on the P4,
the same order on the C5, and the P4 also has a SHA accelerator if needed. The gateway
UART carries at most about 90 full packets a second (921,600 baud), or about 2,000
minimum-size ones. The hub's verify budget is therefore bounded by the UART: under 2 % CPU
for full packets, about 4 % for a stream of tiny forged ones. This is to be **measured
first** (section 10, step S0). Cheap checks run before the HMAC: version, roster lookup by
MAC, epoch, and `(clock, counter)` not below the last accepted pair. No state changes until
the tag is good.

A flood at the gateway (forged packets claiming roster MACs) can no longer be filtered by
tag there. The gateway therefore applies non-secret limits:

* drops packets from MACs not in the roster (counted per MAC, reported once a second);
* gives each roster MAC a budget (recommended **Owner** values: 40 packets/s and 16 KB/s,
  about four times a busy Wi-Fi node) and drops what exceeds it (counted);
* drops a packet byte-identical to the previous one from the same MAC (a radio retry);
* serves its UART queue round-robin per MAC, so one flooded MAC cannot starve the others.

A forger who floods a real node's MAC can then crowd out that node's own packets. That is no
worse than jamming it, and it is visible: the hub's `bad_tag` count rises for that slot and
the delivery-loss check of section 7.3 fires.

## 6. Wire changes (link protocol 14, ESP-NOW packet version 3)

### 6.1 Freshness clock in ticks

The hub's clock becomes `clock = max(previous + 1, hub_uptime_ms / 10)`: one tick per
10 ms, strictly increasing per packet. It runs out after about 497 days, after which the
hub refuses to seal and restarts for a new epoch (the rule today, at a new rate).

* **Epoch**: the next value of the `rsboot` counter (unique per hub boot). The P4 has no
  radio, and its HAL has no true random source, so uniqueness comes from the counter, seeded
  with random bytes at provisioning, not from `Rng`. Epochs need to be unique, not secret.
* **Node floor** (the existing Phase 2 lateness rule, re-scaled): the highest hub clock
  accepted, plus 90 ticks for every second of node uptime since. The real clock grows 100 a
  second, and the 10 % margin covers crystal error with room to spare.

| Packet | Late if | Bound on a held-back recording |
|---|---|---|
| Command, sync, beacon at the node | `clock + 300 < floor` | 3 s, plus 0.1 s for each second the node was jammed before the recording |
| `TimeSync` at the node | `clock + 50 < floor` | 0.5 s, plus 0.1 s per jammed second; `aerohop`'s three-agreeing-syncs rule then applies |
| Node batch at the hub (new) | `now_clock - stamp.clock > 200`, or `stamp.clock > now_clock + 50` | 2 s |

What Phase 2 already covers: `Reject::Late` bounds hub-to-node packets at 16 packet-clock
units, about 8-16 s, and a node jammed for J seconds before the recording accepts it for
about J + 16 s, because the packet clock runs at only about 2 a second. It does nothing for
node batches: the gateway checks order only. The tick clock shrinks the first bound and
adds the second.

For the uplink check, a node's stamp `clock` field changes meaning. It is no longer "the
highest gateway clock heard" but the node's **estimate of the hub clock** when sealing:
`max(previous estimate, last accepted clock + elapsed_ms / 10)`. It is still non-decreasing
within a session, so the `(clock, counter)` ordering rule holds unchanged. Each retry is
re-sealed, as today, so a batch retried for its full 2 s is never late.

The gateway no longer adjusts a `TimeSync`'s time (it cannot: the packet is sealed). The hub
seals each `TimeSync` the moment it writes it to the UART. The gateway sends `urgent`
transmissions before anything else queued and drops them if more than 300 ms old, and it
reports the hold time of each in `TxDone`, so the residual offset (expected under 20 ms
against a 150 ms tolerance) is measured, not assumed.

### 6.2 ESP-NOW packets (version 3)

Header unchanged: `"RS"`, version, kind, seq (6 bytes). Version 1 stays the open-kit
packet. **Version 2 (kit key) is refused** and counted as `old_firmware`, which is how an
un-reflashed board shows up.

| Kind | Direction | Layout after the header | Size |
|---|---|---|---|
| 1 Sync | hub to one node (unicast) | channel (1) ‖ stamp (16) ‖ tag (12, `K_down`) | 35 |
| 2 Frames | node to hub | COBS frames ‖ stamp (16) ‖ tag (12, `K_up`) | max 1024; payload max 990, unchanged |
| 3 Command | hub to one node (unicast) | COBS commands ‖ stamp ‖ tag (`K_down`) | max 1024 |
| 4 Beacon (replaces broadcast use of 1) | hub to all (broadcast) | channel (1) ‖ n (1) ‖ stamp (16, session 0) ‖ n × tag8 | 32 + 8n: 160 for 16 nodes |
| 5 Unprovisioned | node, unsealed | mac (6) ‖ image (1) ‖ firmware (16) | 29 |

The stamp keeps its 16 bytes (`epoch`, `clock`, `session`, `counter`, u32 each). Tag inputs:
`HMAC(K, sender_mac ‖ everything before the tag)[0..12]`, where the sender is the radio's
source address (the gateway's MAC for hub-made packets: the hub learns it from the
gateway's Hello). Beacon tag `i` = `HMAC(K_down[slot i], gateway_mac ‖ header ‖ channel ‖ n
‖ stamp)[0..8]`. A node checks only the tag at its own slot. Eight-byte beacon tags (**Owner**,
recommended): a beacon only teaches a gateway address and advances a clock, and 2^64 guesses
at a few thousand a second is out of reach. Revoked or empty slots get a zero tag.

Every packet size stays within `airlink::MAX_PACKET` (1024), and ESP-NOW v2 allows 1470.

### 6.3 Gateway UART (new `link::gw` types, replacing `Frame`/`Command` on that UART)

The gateway's own frames no longer pass as `link::Frame`s, so the hub never takes a node
frame from the gateway except from inside a packet it has verified. `Via` disappears from
this link: the verified source names the node.

Gateway to hub (`GwUp`, postcard + COBS, largest about 1,050 bytes; the hub's gateway
decoder gets its own `MAX_GW_FRAME = 1100`, and `link::MAX_FRAME` stays 320):

| Message | Fields |
|---|---|
| `Hello` | protocol 14, firmware (16), gateway MAC, channel |
| `Packet` | src MAC (6), rssi (1), held_us (u32), data (up to 1024) |
| `TxDone` | id (u16), to (6), radio_acked (bool), attempts (u8), held_ms (u16) |
| `Refused` | src (6), reason (not_in_roster, over_budget, old_firmware, malformed), count, rssi, window |
| `Stats` | beacons sent, foreign packets, UART drops, per-reason totals |

Hub to gateway (`GwDown`):

| Message | Fields |
|---|---|
| `Roster` | up to 16 MACs (every 5 s and on change) |
| `Beacon` | data (up to 160): broadcast as is. The hub sends a fresh one every 500 ms; the gateway never repeats one |
| `Transmit` | id, to (6), urgent (bool), give_up_ms (u16), data (up to 1024): unicast, retried while the node is on channel, up to `give_up_ms` (commands 2,000; time syncs 300) |
| `Cancel` | to (6): a newer command for this node replaces the queued one |

UART load: today each relayed frame costs its own bytes plus a 9-byte `Via`. Now each packet
costs its payload plus 6 + 28 + about 12 bytes. With about five frames per batch, that comes
out slightly smaller than today.

### 6.4 Link frames (protocol 14)

| Change | Bytes | Why |
|---|---|---|
| New `Frame::NodeHealth` (node to hub, every 5 s, origin Node): shed per kind (8 × u32: known Remote ID, new Remote ID, health, threat, Wi-Fi drone, DJI, open link, other), alternations (2 × u32: DJI, Wi-Fi drone), uplink accounting (frames sent and radio-ACKed, batches sent, batches given up, send attempts: 4 × u32), link rejects by reason (6 × u32), deaf fall-backs (u16), image switches refused (u16) and seconds until the next one is allowed (u16) | ≤ 130 encoded | node Stats follow-up: per-kind drops and alternation; delivery accounting; switch and deaf counters |
| `Frame::Stats` unchanged | - | keeps once-a-second health cheap |
| `HubFix` gains `held: bool` (the position is a held value, not live) and `utc_held: bool`; `utc_pps` is false while the time is held | +2 | phone page shows a held fix as live, and held time keeps its PPS flag |
| New `ThreatKind::LinkLoss` (origin Hub) | - | section 7.3 |
| `ForgedLink` `detail` codes 8 `old_firmware`, 9 `not_in_roster`, 10 `over_budget`, 11 `late_uplink`, 12 `future_uplink` | - | new refusals |
| `Hello` protocol 14 required on keyed links; the hub refuses older peers (gateway and web head) and shows `OLD FIRMWARE: <which>` | - | mixed kit fails closed |

Wired-bus effect: `NodeHealth` fits a single bus unit (`wirebus::MAX_UNIT` 336), and every
5 s it uses well under 1 % of a 1024-byte poll budget. Nothing else on the bus changes.

## 7. Node robustness items

### 7.1 Image-switch rate limit

Today nothing on the node limits switches. Each switch erases `rsjob` and writes `otadata`,
so a stream of role changes is a reboot loop and wears the flash. Per-node keys already mean
only the hub can command one. The node adds its own limit as well:

* A switch is allowed only after the running image has been up for `min_uptime`.
* `min_uptime` = 60 s × 2^streak, capped at 1 h. `streak` is carried in the job record
  (`nodeimage`, record magic `RSJ2`, one byte more): the switching image writes
  `streak + 1` if it ran less than 10 minutes, else 0.
* A refused switch keeps the current role and plan, is counted in `NodeHealth`, and the hub
  sees `applied = false`. Worst case about 24 switches a day once at the cap.

### 7.2 Plans that keep the gateway channel, and a deaf fall-back

* `coverage::plans::with_espnow_visits` **must never drop a visit**. Today it pushes into a
  24-entry plan and silently loses the push when full. It becomes fallible. The coordinator
  shortens the plan (drops its lowest-weight hops) until visits fit, and asserts
  `longest_away_from_6 <= ESPNOW_MAX_AWAY_MS`.
* The node checks the same rule on every plan it is given or reads back from `rsjob`. Its
  working plan holds 49 hops (24 from the wire plus up to 25 inserted visits), so the
  insertion cannot overflow. A plan that still fails the rule is refused (the previous plan
  stays).
* The gateway channel is one constant shared by the gateway and `coverage`
  (`airlink::GATEWAY_CHANNEL`), so they cannot drift apart.
* **Deaf fall-back** (all node images, in `node-common`): with no accepted (verified)
  gateway packet for 60 s while running a hub-given or stored plan, the node switches to its
  role's built-in plan, reports plan id 0 and counts the fall-back. It refuses that plan id
  until reboot.

### 7.3 Delivery accounting

A radio ACK proves only that something on the air acknowledged the frame. The node keeps
counting what it believes it delivered (`NodeHealth`, cumulative), and the hub counts what
it verified per slot. Every `NodeHealth` the hub compares the two over a sliding 60 s. If the
node claims at least 50 frames more than arrived and more than 20 % of what it sent, the hub
raises `LinkLoss` naming the node: an evidence event and a screen entry. Cumulative counters
make a lost `NodeHealth` harmless. If forged ACKs swallow everything, `NodeHealth` stops
arriving too, and the existing deaf-node alarm fires. Downlink: the gateway's `TxDone` is
also radio-level only. The hub already resends `Assign` every few seconds and confirms by
the plan id echoed in `Status`, which stays the proof.

### 7.4 Randomness after the radio starts

Every board with a radio takes its randomness from `esp_hal::rng::Trng::try_new()` after
`WifiController::new` (or the BLE controller) has started. esp-radio raises the hardware
entropy source then. If `try_new` fails, the keyed link does not start, and the board says
so on USB and in its `Hello`. The node `session` is what makes a command recorded before a
node reboot useless after it, and a session that repeats across boots would undo that. The
P4 has neither a radio nor HAL support for its TRNG, so the hub uses the `rsboot` counter
for its epoch, and the secure element's random source once fitted.

### 7.5 Node numbers and mutes

Today hub node numbers are table indexes, reused 60 s after a node goes silent, so a
node-keyed mute or snooze can land on a different node. ESP-NOW nodes now take their number
from their roster slot (stable across boots). Wired and bus nodes keep table numbers. Any
time a number changes hands (new slot occupant at a higher `gen`, a different wired MAC, or
eviction), the tracker drops every node-keyed mute and snooze for that number
(`ThreatTable::forget_node`).

## 8. CVE-2025-52471 (ESP-NOW receive length)

Public facts ([Espressif advisory GHSA-hqhh-cp47-fv5g](https://github.com/espressif/esp-idf/security/advisories/GHSA-hqhh-cp47-fv5g),
[NVD](https://nvd.nist.gov/vuln/detail/CVE-2025-52471)): an integer underflow in the
ESP-NOW receive path of the Wi-Fi libraries. A length taken from the received packet was not
checked well enough, which can give out-of-bounds access and, without memory protection,
possibly code execution. Affected: ESP-IDF v5.4.1, v5.3.3, v5.2.5, v5.1.6 and earlier on
each branch. Fixed: v5.4.2, v5.3.4, v5.2.6, v5.1.7. The fix is an update of the binary
libraries (`components/esp_wifi/lib`), ESP-IDF commit `edc227c` on master (2025-05-29) and
`b1a379d` on release/v5.5. The advisory's workaround for v5.3 and older is to check that
`data_len` in the receive callback is positive.

This repository: every radio firmware lockfile pins **esp-radio 1.0.0-beta.1** with
**esp-wifi-sys-esp32{c3,c5,s3} 0.3.0** (the P4 hub has no radio). Those crates were
generated from **ESP-IDF v6.1-255-g4dbfecac7e3** (esp-wifi-sys commit `6601e3f4`,
2026-09-04). Checked on 2026-11-18:

* GitHub's comparison of `edc227c...4dbfecac7e3` reports "ahead by 9139, behind by 0", so
  the IDF commit the blobs come from contains the fix commit.
* `libespnow.a`, `libnet80211.a` and `libpp.a` for the C3, C5 and S3 in the local cargo
  registry are byte-identical (git blob hashes) to `espressif/esp32-wifi-lib` at `7cd4d7b`,
  the library commit that ESP-IDF `4dbfecac7e3` pins. All carry the internal library
  revision string `e12a754`.

Conclusion: **the pinned blobs contain the fix by provenance.** What cannot be shown without
a test is that the fix is still effective in this later library (no regression). That test
needs a raw 802.11 transmitter sending malformed ESP-NOW action frames, which this programme
rules out on the bench. It belongs in a shielded-enclosure test if ever wanted.

Defence in depth (recommended, **Owner**: it means carrying a patched esp-radio for every
radio firmware, as `firmware/patches/esp-radio` already does for `rid-beacon`): esp-radio's
ESP-NOW receive callback turns `data_len: i32` into a slice length with `as usize` and no
check, then copies it into a heap box. A negative or oversized length from any future blob
bug would become a huge read. The patch drops (and counts) `data_len <= 0` or
`> ESP_NOW_MAX_DATA_LEN_V2`. It is a two-line change in the patched copy, plus a
`[patch.crates-io]` entry in each radio firmware. A CI check (`tools/check-wifi-blobs.sh`)
compares the blob hashes in the lockfile's crates with the list above, so a dependency bump
cannot silently go back to a pre-fix library.

## 9. Test plan

Host tests are written first and fail on today's code. Bench checks use the kit (P4 hub,
gateway `c5c`, nodes `c5a` and `c5b`) and only on-chip injection: the `forgetest` and
`floodtest` builds, and a gateway `forgetest` feature added here. All forged traffic comes
from our own bench boards through their normal radio path. No separate attack transmitter
is used.

New `forgetest` commands (node-wifi). A node in this build behaves as a **captured node**:
it holds its own key only.

| Attack | Host test (crate) | Bench check |
|---|---|---|
| Captured node speaks as another node | `airlink`: `node_key_cannot_seal_for_another_mac`; hub verifier `slot_key_bound_to_mac` | `forge claim <c5b-mac>` (c5a sets its station MAC to c5b's and seals with its own key): hub `bad_tag` for c5b's slot +n, no c5a frames under c5b, c5b keeps reporting |
| Captured node commands another node | `airlink`: `command_under_other_node_key_rejected` | `forge cmd <c5b-mac>` (unicast `Assign` sealed with c5a's key): c5b `link_auth.bad_tag` +1, plan unchanged |
| Captured node forges a beacon | `airlink`: `beacon_tag_checked_per_slot`, `beacon_with_other_slot_tag_rejected` | `forge beacon`: c5b `bad_tag` +1, gateway address unchanged |
| Captured gateway forges or alters | hub verifier: `gateway_has_no_key_and_cannot_forge`, `altered_command_rejected_at_node` | gateway `forgetest`: `gwforge batch` (hub `bad_tag`), `gwforge flip <mac>` (node `bad_tag`), `gwforge replay` (hub `replay`) |
| Withheld node batch | `airlink`: `withheld_batch_late_at_hub` (today accepted in order) | `forge hold 5`: c5a seals a batch, waits 5 s, sends: hub `late_uplink` +1, nothing from it in the tracker |
| Withheld command or time sync | `airlink`: `command_late_after_3s`, `timesync_late_after_half_second`, `jammed_node_bound_is_tenth_of_jam` | `gwforge delay 1000` on the next `TimeSync`: aero node (or c5a in `ROTORSCOPE_BENCH` aero-time mode) `late` +1 |
| Forged ACK / silent loss | hub: `claimed_minus_received_raises_link_loss`, `lost_nodehealth_is_harmless` | `gwforge drop <c5a-mac> 50` (the gateway's radio ACKs, then discards half; same effect as forged ACKs): `LinkLoss` for c5a within 60 s, ends after `gwforge drop off` |
| Key in images | `kitkeys`: record round trip, CRC, MAC mismatch, both copies damaged; a script builds node-wifi in two checkouts with different kits and compares SHA-256 (identical) | flash an image with `rskey` erased: node prints `missing`, sends announcements, hub strip `UNPROVISIONED NODE`; `link-key node --bench-corrupt` gives `damaged` |
| Mixed kit | `airlink`: `v2_packet_is_old_firmware`; hub: `gateway_hello_13_refused` | flash c5b with the current (v2) build: hub `old_firmware` count and strip; gateway at v13: hub strip `OLD FIRMWARE: gateway` |
| Flood on a roster MAC | gateway budget logic in a host-testable module: `per_mac_budget`, `round_robin_uart_queue` | `forge spam 500` (bad-tag packets as c5a): gateway `over_budget`, c5b's delivery unchanged, hub CPU from the `{"t":"hub"}` line under 10 % |
| Image-switch storm | `node-common` policy, host: `switch_backoff_doubles_and_caps` | hub console `bench switchstorm c5a` (alternating Wi-Fi / BLE every 5 s): c5a refuses after the first, `NodeHealth.switch_refused` rises, at most one reboot a minute |
| Deaf plan | `coverage`: `full_24_hop_5ghz_plan_keeps_channel_6` (fails today); node: `stored_plan_without_6_refused`, `deaf_60s_falls_back` | hub console sends a 24-hop 5 GHz plan to c5a: c5a keeps reporting; `floodtest` console `job deaf` stores a channel-6-free job and reboots: c5a back on the built-in plan within 60 s |
| Weak randomness | - (hardware) | every radio board's boot line shows `rng:"trng"`; a `faulttest` hook calling it before the radio starts shows the keyed link refusing to start |
| Node-number reuse | `tracker`: `mute_does_not_follow_number_to_new_node` | (host only) |
| Held fix shown live | `webui`: `held_fix_not_shown_as_live` | GPS emulator `hold` scenario: phone page shows "held" |

## 10. Implementation plan

**S0, de-risk spike (first, throwaway branch, about a day).** The riskiest part is moving
verification and beacon origination to the hub. Beacons then depend on the hub's main loop,
which can pause for a few hundred milliseconds while the airspace pack loads, and the gateway UART must
carry raw packets under flood. The spike adds, behind a bench feature, a gateway mode that
also forwards raw packets and a hub timer that measures HMAC time per packet with a test
key. It then measures: P4 and C5 cycles per full and tiny packet; UART occupancy with
`floodtest` at 1000/s on both nodes; beacon interval jitter from a hub timer task across a
pack load. Pass: verify under 0.3 ms per full packet on the P4, UART under 70 %, beacon gap
under 1 s. If the gap fails, beacons move to a dedicated hub task or interrupt (decided
before WP-13e starts).

Packages, test-first, each owned by one engineer-agent:

| Pkg | Scope | Owns (only these files) | Needs |
|---|---|---|---|
| 13a Kit keys | derivation, records, roster; `link-key` subcommands | new `crates/kitkeys/**`, `tools/link-key/**` | - |
| 13b Auth v3 | v3 packets, tick clock, multi-tag beacon, uplink age, `Reject` codes; v2 kept compiling until 13i | `crates/airlink/src/auth.rs`, `crates/airlink/src/lib.rs` | 13a (key types) |
| 13c Wire types | `link::gw` module, `NodeHealth`, `HubFix` fields, `LinkLoss`, detail codes, protocol 14, origin table | `crates/link/src/lib.rs`, new `crates/link/src/gw.rs`, `crates/link/src/origin.rs` | - |
| 13d Node robustness | plan visits, 49-hop node plans, job record `RSJ2`, switch back-off, deaf fall-back, shared channel constant | `crates/coverage/src/plans.rs`, `crates/nodeimage/**`, `firmware/node-common/src/lib.rs`, `firmware/node-wifi/src/role.rs` | - |
| 13e Hub verifier | `rskit`/`rsboot` reading, key derivation at boot (`KeySource`), per-slot peer state, beacon and command sealing, `GwUp`/`GwDown` handling, delivery accounting, screen strips | `firmware/hub-p4/src/links.rs`, new `firmware/hub-p4/src/linkauth.rs`, `firmware/hub-p4/src/display.rs`, `tools/partitions-hub.csv` | 13a, 13b, 13c |
| 13f Secret-free gateway | raw relay, roster filter, budgets, transmit queue, `TxDone`, `gwforge` | `firmware/gateway/**` | 13b, 13c |
| 13g Node keystore and uplinks | `rskey` reading, `Trng`, shared uplink auth glue, announcements, `NodeHealth` sending, `forge` commands | new `firmware/node-common/src/keystore.rs` and `linkauth.rs`, `firmware/node-{wifi,ble,sdr,lr}/src/uplink.rs`, `firmware/node-wifi/src/forge.rs`, `firmware/node-wifi/src/report.rs`, `tools/partitions-node.csv`, new `tools/partitions-single.csv`, `tools/flash-node.sh`, `tools/dev-board.sh` | 13a, 13b, 13c |
| 13h Tracker and web | `forget_node`, `LinkLoss` threat, held fix | `crates/tracker/**`, `crates/webui/**`, `firmware/hub-p4/src/webhead.rs` | 13c |
| 13i Cut-over | delete compiled key (`build.rs`, `keysrc.rs`, `KIT_KEY`), drop v2, esp-radio length patch, blob check script, docs | `crates/airlink/build.rs`, `crates/airlink/src/keysrc.rs`, `firmware/patches/esp-radio/**`, `firmware/*/Cargo.toml` (patch entries), `tools/check-wifi-blobs.sh`, `docs/ESPNOW.md`, `docs/SPOOFING.md`, `docs/BENCH.md` | all |

Order: S0, then 13a, 13c and 13d in parallel, then 13b and 13h, then 13e, 13f and 13g in
parallel, then 13i. 13a-13d and 13h can merge on their own (no behaviour change until used;
13h's `HubFix` change needs the web head reflashed with the hub). **13e, 13f, 13g and 13i
merge together** as one flag day on an integration branch. They are bench-tested as a set
(section 9) before the owner merges, because the gateway, hub and nodes speak protocol 14
only with each other.

## 11. Limits that remain

* Until a secure element is fitted, a captured hub is a captured kit.
* Until WP-15, a captured board can be reflashed with hostile firmware that keeps its own key:
  a captured node stays a hostile node (revoke it).
* A forger can crowd out one node by flooding its MAC (equivalent to jamming; visible).
* The wired bus is unauthenticated (section 4.6).
* A node that has just booted takes the first genuine beacon of any epoch (no floor yet);
  it moves to the right epoch at its first sync.
* The CVE fix is established by provenance, not by test (section 8).

## 12. Review decisions (2026-11-18)

The design above is accepted as the plan for WP-13, with these decisions and additions.

Decisions on the points marked **Owner** (taken on the recommendations; the owner can still
overrule any of them before the cut-over):

* The master lives in the hub's flash until a secure element is fitted; "a captured hub is a
  captured kit" goes in the docs in those words.
* The wired bus stays unkeyed in this package.
* Beacon tags are 8 bytes. Gateway budget per roster MAC: 40 packets/s and 16 KB/s, to be
  re-checked against the S0 measurements.
* Every radio firmware carries the patched esp-radio with the receive-length check, and the
  blob-hash check script.
* Kits ship with open records; the buyer seals with `link-key`.

Additions the packages must cover:

1. **Epochs are ordered, and the rule is written down and tested (13b).** The epoch is a
   counter now, so say exactly what a node does with a packet from a higher epoch, a lower
   one, and its first packet after boot, and test: an old-epoch beacon and an old `TimeSync`
   replayed (from the gateway's address) to a node that has just booted; the same to a node
   jammed for several minutes; a hub that was re-provisioned and came back with a lower
   counter than its nodes last saw (the kit must recover without reflashing nodes, and the
   recovery path must not reopen the replays above). A `TimeSync` must be useless to a node
   after that node reboots, as a command is.
2. **One boot counter on the hub (13e, with the evidence package).** The `rsboot` counter is
   also the boot counter the evidence log needs (it survives a wiped or swapped SD card,
   which a counter file on the card does not). The evidence work on `sec/p3-evidence` puts
   its counter behind a trait; at integration the flash counter becomes that trait's source,
   and a secure-element counter can replace both later.
3. **The key source is the seam for the secure element (13a, 13e).** `KeySource::derive` and
   the hub's random source must be implementable by an I2C element without touching the
   verifier: no code outside the key source may read the master.
4. **The hub has no proven random source** (the P4 HAL marks its TRNG unsupported). Nothing in
   this package may depend on hub randomness for security; where the hub needs uniqueness it
   uses the boot counter.
5. **Spike S0 runs before 13c, 13e, 13f and 13g start.** 13a and 13d do not depend on it.

## 13. Notes from the finished packages (13a, 13b, 13d), for the ones that follow

What was built differs from the text above in these places; the code and its module docs win.

**Kit keys (13a)**
* The hub's master and roster are two separate records in `rskit` (master copies at `0x0000`
  and `0x0800`, roster copies at `0x1000` and `0x1800`), so `hub-roster` can push the roster
  without the master. The roster record carries the kit id; a roster from another kit is refused.
* Boot counter log (`rsboot`): 8-byte entries (value plus a CRC) over two 4 KB sectors; the
  current value is the largest good entry.
* Generations start at 1. A revoked slot stays taken; re-keying it goes to the next generation.
* `link-key hub` re-seeds `rsboot` unless `--keep-boot` is given. **Still to do (cut-over
  package):** when the hub's counter still reads good, seed above the old value, so nodes do
  not need the 60 s relink after a re-provision.
* Still to do: a Ctrl-C during a board write leaves the 0600 temporary key file behind.

**Packets (13b, `airlink::v3`)**
* Beacon size is 24 + 8n bytes (152 for 16 slots), not 32 + 8n.
* Lateness is exactly 3 s (commands, sync, beacon) and 0.5 s (`TimeSync`) while the node keeps
  hearing beacons, and about 3.3 s and 0.55 s when it hears nothing more.
* **Epoch rule.** A node that has just booted, or has accepted nothing for 60 s ("relink": new
  session, no epoch), takes a genuine beacon of any epoch as a provisional epoch only. A Sync
  proves an epoch and is the only packet that moves a proven node to a higher one. Lower epochs
  are refused, except that a Sync may move a provisional node down. A Command never moves a
  node, and a `TimeSync` needs a proven epoch. A hub that comes back with a lower counter is
  refused for about 60 s, then the nodes relink and carry on: no reflash or power cycle.
* The hub checks the tag even on stale or late batches, so it can tell a genuine node that
  needs a Sync from a forgery.
* **Hub (13e) must:** answer a genuine refused batch (`wants_sync`) with a rate-limited Sync;
  send a Sync before the first `TimeSync` to a node not yet synced; send each `TimeSync` alone
  in its own Command, sealed as it is written to the UART.
* **Nodes (13g) must:** pass `timesync = true` for a Command holding a `TimeSync`; take
  randomness from the hardware generator after the radio starts; re-seal every retry; feed one
  monotonic uptime; not reset the link on the deaf fall-back (`airlink` relinks by itself).
* **Wire types (13c) must:** reserve `ForgedLink` detail codes 13 to 16 for
  `not_for_this_slot`, `unproven`, `malformed` and `foreign`, and size `NodeHealth` for the
  node's nine reject reasons, not six.
* Known residual: a node kept deaf by jamming all through a hub reboot can be handed a held
  `TimeSync` once its epoch moves; the 60 s relink caps how old it can be, and the aero nodes'
  three-agreeing-syncs rule still applies.

**Node robustness (13d)**
* The streak that lengthens the image-switch wait resets only after the image has sat ten
  minutes past its own wait without being asked to switch (the rule as first written could
  never reach the one-hour cap).
* Deaf fall-back: after 60 s without an accepted gateway packet on a hub-given or stored plan
  the node returns to its built-in plan. If the gateway is still silent 30 s later it was an
  outage and the plan is not blamed; otherwise the plan id is refused for 10 minutes, doubling
  on each repeat to a 1 hour cap. Nothing is refused until reboot.
* `coverage` keeps `airlink` as a test-only dependency (a test and a compile-time check hold
  its channel equal to `airlink::GATEWAY_CHANNEL`) until the cut-over deletes the compiled key.
* **Gateway (13f) and nodes (13g) must** replace their own channel constants with
  `airlink::GATEWAY_CHANNEL` (`firmware/gateway/src/main.rs`, `firmware/node-ble/src/uplink.rs`,
  `firmware/node-lr/src/uplink.rs`, `firmware/node-sdr/src/uplink.rs`), wire the deaf fall-back
  into node-sdr and node-lr (only node-wifi has it), grow the JSON buffers in node-wifi's
  `report.rs` for 49-hop plans, and carry `node_common::health()` in `NodeHealth`.

## 14. Spike S0 results (2026-11-18) and what they change

Measured on the real kit (P4 hub, C5 gateway, two C5 nodes) with a throwaway bench build
(branch `spike/p3-s0`, never merged). The design holds, with the changes below.

| Pass line | Result |
|---|---|
| Verify under 0.3 ms per full packet on the P4 | 0.24-0.25 ms in a warm loop; 0.3-0.8 ms per packet in the live path (cold cache). Hub CPU spent verifying: 0.9 % under the heaviest real flood, 2.8 % with full packets at line rate, 6.3 % with tiny packets at line rate |
| Gateway UART under 70 % at the heaviest phase | 4.8 % at most with both nodes flooded at 1000/s (the nodes shed floods before the uplink). The line saturates at 87 full packets/s or about 1,500 tiny ones |
| Beacon gap under 1 s | Longest gap 943 ms, once, at the boot SD mount; at most 546 ms while running. **Only with the gateway receive fix below** |

Corrections to the text above: SHA-256 costs about 4,800 cycles a block on both chips (not
2-3 k); a full verify is 0.24 ms on the P4 and 0.36 ms on the C5; a 16-tag beacon costs the
hub about 0.55 ms. Time-sync path, hub write to the gateway's radio task: median 0.63 ms, worst
80 ms (while the gateway was sending its own beacon), against a 150 ms tolerance.

**Required changes**

* **Gateway (13f).** Without the fix the gateway lost 45-58 % of the hub's 160-byte messages
  (longest stretch without a beacon 12.9 s): its async UART read does not run within the
  ~1.4 ms a 128-byte FIFO gives it, held up by the relay loop and by USB printing that masks
  interrupts. Receive the hub UART in an interrupt into a ring of at least 4 KB with a FIFO
  threshold of 32 bytes or less, and print over USB without masking interrupts (the nodes
  already have an async writer). With both, nothing was lost in 1,486 beacons.
* **Gateway budgets (13f).** Add a total cap of about 50 KB/s on the relay to the hub on top
  of the per-MAC budget (40 packets/s, 16 KB/s) and the round-robin: four flooded roster MACs
  would otherwise fill 70 % of the line.
* **Hub receive path (13e).** A separate decoder for gateway messages with an 1,100-byte
  buffer (static or in PSRAM, not in the main loop's frame: `link::Decoder` stops at
  `MAX_FRAME` = 320); a receive ring of about 64 KB in PSRAM or drained in the interrupt (the
  8 KB ring holds 89 ms at line rate and main-loop passes reach 190 ms, 600 ms at the SD
  mount); budget 0.25-0.75 ms per full verify.
* **Hub auto-detect (13e).** The wired-bus auto-detect recognises a gateway only by readable
  `link::Frame`s, so a gateway speaking the new messages is never recognised and the hub stays
  in bus mode. Recognise the gateway by its new Hello.
* **Beacons (13e).** Sending from the main loop meets the line with 57 ms to spare at boot.
  Also check the beacon timer from the screen-band callback the loop already uses, and split
  the SD mount (it took 567-617 ms), so the margin does not depend on the card.
* **Hub tracker queue (13e).** The 8-entry queue overflowed (`out_full` 2,400-2,670 per flood
  phase) with the raw relay; not compared with today's relay. Measure, and size it.
* **Flash script (13g).** `tools/flash-node.sh` defaults `NODE_SDR_ELF` to the wired node-sdr
  build, which carries no key tag and so passes the key check: an ESP-NOW node can end up with
  a keyless node-sdr slot. Default to the ESP-NOW build and refuse a mismatch.

Not measured: on-air timing of hub-originated beacons and time syncs; the real `link::gw`
message format (the spike used a simpler framing); whether the interrupt ring alone is enough
without the quieter USB printing; 5 GHz node plans.
