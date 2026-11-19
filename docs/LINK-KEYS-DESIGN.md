# Link keys: the design as built (WP-13)

Status: **built** (branch `sec/p3-13i`, 2026-11-18): host-tested and compile-checked for every
firmware; **not yet run on hardware** (the bench pass is [BENCH.md "Link keys: bench
checklist"](BENCH.md#link-keys-bench-checklist)). This is the reference for ESP-NOW link
protocol 14 and packet version 3. It replaced the one-key-per-kit authentication (version 2),
which is deleted: no firmware image carries a key. Where the first draft of this design and the
code differ, the code and its module documentation win; section 12 lists what changed during
the build. The user-facing guide is [ESPNOW.md "Securing the link"](ESPNOW.md#securing-the-link).

Decisions that belonged to the owner are marked **Owner**; the recommendations were taken
(section 12) and the owner can still overrule any of them.

## 1. What was wrong before, in one paragraph

Every ESP-NOW board (gateway and every node) carried the same 32-byte kit key, compiled into
each firmware image. Flash encryption and secure boot are off, so anyone holding one board
for a minute could read the key over USB. With it they could do everything the kit can: make up
detections from any node, send any node any command (role, plan, image switch, time), and
pose as the gateway. The key was also in every build output, so sharing an image or a flash
dump shared the key. Separately: freshness was order-based and loose (a withheld packet could
be released seconds later), a radio-level ACK counted as delivery, nothing on the node limited
image switches, a full 5 GHz plan could lose its gateway-channel visit and leave a node deaf,
and node numbers on the hub were reused, which could move a mute onto a different node.

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

## 3. The design in brief

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
peers).

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
HMAC state (both pads absorbed: `kitkeys::LinkKey`) is what sits in RAM.

**Owner: the master on the hub before a secure element exists.** Decided: in hub flash
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
| Hub, no element | Everything (`M` is in flash) | - | New master, re-provision every node |
| Hub, with element | Forge as any current node while they hold it powered and can read its RAM (JTAG is still open until WP-15) | Extract `M`; make keys for new slots or generations | Bump `gen` on every slot (re-provision nodes); `M` stays |
| Wired-bus access | Report as any MAC it joins with; no hub-only frames | (unchanged) | - |

A flash dump or shared image gives nothing: images carry no key.

### 4.4 The provisioning partition

| Board | Partition | Location |
|---|---|---|
| Multi-image C5 node | `rskey`, data, subtype `undefined` (0x06, as `rsjob`; espflash cannot flash a table with a custom data subtype), 4 KB | `0x611000` in `tools/partitions-node.csv`, after `rsjob` (space is free up to 8 MB) |
| Single-image node (S3 Wi-Fi, C3 aero, C5 single-image builds) | `rskey`, same, at `0x3F0000` | `tools/partitions-single.csv` (nvs, phy_init, factory, rskey); these boards must be flashed with it, since espflash's default table has no `rskey` |
| P4 hub | `rskit` (8 KB: two copies of the master record at `0x0000`/`0x0800`, two of the roster at `0x1000`/`0x1800`), `rsboot` (8 KB boot counter) at `0xC10000` and `0xC12000` | `tools/partitions-hub.csv` (Elecrow's four rows, then these two) |
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

An open record has type 2 and zeros for every field after it. Hub records (`rskit`): the
master and the roster are **separate** records, two copies each (`crates/kitkeys/src/hub.rs`),
so `hub-roster` can push the roster without the master; the roster record carries the kit id,
and a roster from another kit is refused. Generations start at 1; a revoked slot stays taken
and is re-keyed at the next generation. Boot counter (`rsboot`, `crates/kitkeys/src/bootlog.rs`):
an append-only log of 8-byte entries (value and a CRC) over two 4 KB sectors, the current value
the largest good entry, a sector erased only when the log moves into it. `link-key hub` seeds
it just above the old value when the old log still reads good (so nodes follow a re-provisioned
hub at once), otherwise with a random value below 2^31 from the computer.

At boot the firmware takes the first copy with a good CRC and checks the record's MAC
against eFuse. The outcome is one of:

| State | Node does | Seen as |
|---|---|---|
| Keyed, good | Normal keyed link | USB `{"t":"uplink","auth":"kit:<kit_id> slot:<n> gen:<g> check:<key_check>"}` |
| Open record | Unauthenticated link (version 1 packets) | USB warning; hub strip `OPEN LINK` |
| Empty, both copies damaged, or MAC mismatch | **No reports over ESP-NOW.** Every 10 s: a USB warning naming the reason (`missing` / `damaged` / `wrong_board`) and one unsealed `Unprovisioned` announcement on the gateway channel | hub strip `UNPROVISIONED NODE` with the count, from the announcements; the board's status LED blinks where it has one |

The announcement is information only: anyone can send one. The gateway forwards at most one
a second in total, and the hub shows only how many distinct MACs it has heard in the last
minute.

### 4.5 Tool flow

`tools/link-key` (`cargo run -p link-key -- ...`); every board access goes through
`tools/dev-board.sh --run` and its per-board lock, and every board-writing command has
`--dry-run`. The key never appears on a terminal. It goes from the tool to a 0600 file in a
private 0700 temporary directory, then to `espflash write-bin` at the partition's offset, and
the file is overwritten and deleted at once. SIGINT, SIGTERM and SIGHUP in that window are
caught: the board command still stops, the file is wiped, and the tool exits with status 130.
Only SIGKILL or a power cut can leave the file behind.

| Task | Command |
|---|---|
| Make a kit | `link-key kit new` writes `tools/kit.key` and an empty roster; prints `kit_id` |
| Seal a kit that shipped open | the same, then provision the hub and every node below |
| Provision the hub | `link-key hub <board>` writes `rskit` (master and roster) and seeds `rsboot` (reading the old counter first; `--keep-boot` leaves it) |
| Add a node | `link-key node <board> [--slot N]`: reads the MAC with `espflash board-info`, takes the next free slot (or bumps `gen` of slot N), writes `rskey`, adds the roster line, then pushes the roster to the hub (`link-key hub-roster <hub-board>`: roster only, never the master) |
| Replace a node | `link-key node <new-board> --slot N` (`gen+1`, new MAC), then `hub-roster` |
| Revoke | `link-key revoke --slot N`, then `hub-roster` |
| Open kit | `link-key open <board>` (node or hub). `tools/flash-node.sh --open` keeps working as a shortcut |
| Check | `link-key show` prints the roster with `key_check`; each board prints its own at boot; a mismatch is a mistake you can see without any secret leaving a board |

Flashing firmware involves no keys: `flash-node.sh` reads the boot line and calls the flash
good only when it shows `auth` keyed (or open with `--open`). Kits ship with open records on
every board and the docs say so; the buyer seals the kit with the first four rows. The
retired `link-key new` / `link-key id` commands, `ROTORSCOPE_LINK_KEY_FILE` and the
`link-key:` ELF tag are gone.

### 4.6 The wired bus and point-to-point wires stay unkeyed

Decided (**Owner**): no keys on the wired bus in this package. Whoever can
touch the bus wires can touch the hub. The origin checks already stop the hub-only frames.
Sealing bus answers would cost every bus node a key record and the hub a verify per answer,
and the change is easy to add later, because the same sealed packet would simply ride inside
a bus answer.

## 5. Where frames are verified

**Built: the hub verifies, the gateway is a secret-free relay.**

| Option | Captured gateway | Per-frame cost | Rejected because |
|---|---|---|---|
| A. Gateway holds `M` (or all node keys) and verifies, as version 2 did | whole kit | C5 | the gateway is the most exposed box (it transmits beacons and is easy to find) |
| B. Gateway holds only uplink keys, hub holds downlink keys | can forge every node's reports | C5 | forging detections is the main harm |
| **C. Gateway holds nothing; the hub verifies uplinks and seals downlinks** | drop/delay only | P4 | chosen |

Cost. One HMAC-SHA256 over a packet takes `ceil((len + 6 + 9) / 64) + 1` SHA-256 blocks:
18 for a full 1024-byte packet, 2 for a 40-byte one. Measured (spike S0, section 13): about
4,800 cycles a block on both chips, so a full verify is 0.24 ms on the P4 (0.3-0.8 ms cold in
the live path) and 0.36 ms on the C5; a 16-tag beacon costs the hub about 0.55 ms. The gateway
UART carries at most about 87 full packets a second (921,600 baud), or about 1,500 tiny
ones, so the hub's verify time is bounded by the UART: 2.8 % of the hub with full packets at
line rate, 6.3 % with tiny ones, 0.9 % under the heaviest real flood. Cheap checks run before
the HMAC: version, roster lookup by MAC, epoch, and `(clock, counter)` not below the last
accepted pair. No state changes until the tag is good.

A flood at the gateway (forged packets claiming roster MACs) can no longer be filtered by
tag there. The gateway therefore applies non-secret limits:

* drops packets from MACs not in the roster (counted per MAC, reported once a second);
* gives each roster MAC a budget (**Owner**: 40 packets/s and 16 KB/s, about four times a
  busy Wi-Fi node), and all of them together a cap of 50 KB/s, and drops what exceeds it
  (counted);
* drops a packet byte-identical to the previous one from the same MAC (a radio retry);
* serves its UART queue round-robin per MAC, so one flooded MAC cannot starve the others.

A forger who floods a real node's MAC can then crowd out that node's own packets. That is no
worse than jamming it, and it is visible: the hub's `bad_tag` count rises for that slot and
the delivery-loss check of section 7.3 fires.

## 6. Wire changes (link protocol 14, ESP-NOW packet version 3)

### 6.1 Freshness clock in ticks

The hub's clock becomes `clock = max(previous + 1, hub_uptime_ms / 10)`: one tick per
10 ms, strictly increasing per packet. It runs out after about 497 days, after which the
hub refuses to seal and restarts for a new epoch (version 2's rule, at a new rate).

* **Epoch**: the next value of the `rsboot` counter (unique per hub boot). The P4 has no
  radio, and its HAL has no true random source, so uniqueness comes from the counter, seeded
  with random bytes at provisioning, not from `Rng`. Epochs need to be unique, not secret.
* **Node floor** (the existing Phase 2 lateness rule, re-scaled): the highest hub clock
  accepted, plus 90 ticks for every second of node uptime since. The real clock grows 100 a
  second, and the 10 % margin covers crystal error with room to spare.

| Packet | Late if | Bound on a held-back recording |
|---|---|---|
| Command, sync, beacon at the node | `clock + 300 < floor` | 3 s while the node keeps hearing beacons, about 3.3 s if it hears nothing more; plus about 0.11 s for each second it was jammed before the recording |
| `TimeSync` at the node | `clock + 50 < floor` | 0.5 s (about 0.55 s hearing nothing), plus about 0.11 s per jammed second; `aerohop`'s three-agreeing-syncs rule then applies |
| Node batch at the hub (new) | `now_clock - stamp.clock > 200`, or `stamp.clock > now_clock + 50` | 2 s |

Before (version 2): `Reject::Late` bounded hub-to-node packets at 16 packet-clock units,
about 8-16 s, a node jammed for J seconds before the recording accepted it for about
J + 16 s, and nothing bounded node batches. The tick clock shrank the first bound and added
the second.

**Epochs are ordered** (`airlink::v3` module docs, rules 1-6). A node that has just booted, or
has taken nothing for 60 s ("relink": new session, no epoch), takes a genuine beacon of any
epoch as a provisional epoch only. A Sync proves an epoch and is the only packet that moves a
proven node to a higher one. Lower epochs are refused, except that a Sync may move a
provisional node down. A Command never moves a node, and a `TimeSync` needs a proven epoch.
A hub that comes back with a lower counter is refused for about 60 s, then the nodes relink
and carry on (no reflash); `link-key hub` avoids even that by seeding above the old counter.
The hub checks the tag even on stale or late batches, so it can tell a genuine node that needs
a Sync (it answers with one, rate-limited) from a forgery.

**A hub restart is not an attack.** For a moment after the hub restarts, every node still seals
under the previous boot's epoch. Those batches are refused (`stale`) and answered with a Sync,
but they are not reported as forged traffic: a *genuine* old-epoch batch from a slot the hub
has not yet accepted in this boot, in the first 90 s of the boot (`STALE_GRACE_MS`), counts as
`catching_up` on the hub's `gwlink` line and raises nothing. Old-epoch traffic is still reported
(`ForgedLink` detail 4) when its tag is bad, once that node has been accepted in this boot (it
is then a replay of recorded traffic), or after the 90 s. Found on the bench, 2026-11-19: before
this rule every hub restart raised one "Forged node traffic" alert per node.

For the uplink check, a node's stamp `clock` field changes meaning. It is no longer "the
highest gateway clock heard" but the node's **estimate of the hub clock** when sealing:
`max(previous estimate, last accepted clock + elapsed_ms / 10)`. It is still non-decreasing
within a session, so the `(clock, counter)` ordering rule holds unchanged. Each retry is
re-sealed, so a batch retried for its full 2 s is never late.

The gateway no longer adjusts a `TimeSync`'s time (it cannot: the packet is sealed). The hub
seals each `TimeSync` the moment it writes it to the UART. The gateway sends `urgent`
transmissions before anything else queued and drops them if more than 300 ms old, and it
reports the hold time of each in `TxDone`, so the residual offset (expected under 20 ms
against a 150 ms tolerance) is measured, not assumed.

### 6.2 ESP-NOW packets (version 3)

Header unchanged: `"RS"`, version, kind, seq (6 bytes). Version 1 stays the open-kit
packet. **Version 2 (kit key) is refused** by its header alone and counted as
`old_firmware`, which is how an un-reflashed board shows up; nothing can make or check a
version 2 tag any more.

| Kind | Direction | Layout after the header | Size |
|---|---|---|---|
| 1 Sync | hub to one node (unicast) | channel (1) ‖ stamp (16) ‖ tag (12, `K_down`) | 35 |
| 2 Frames | node to hub | COBS frames ‖ stamp (16) ‖ tag (12, `K_up`) | max 1024; payload max 990, unchanged |
| 3 Command | hub to one node (unicast) | COBS commands ‖ stamp ‖ tag (`K_down`) | max 1024 |
| 4 Beacon (replaces broadcast use of 1) | hub to all (broadcast) | channel (1) ‖ n (1) ‖ stamp (16, session 0) ‖ n × tag8 | 24 + 8n: 152 for 16 nodes |
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
| `Beacon` | data (up to 152): broadcast as is. The hub sends a fresh one every 500 ms; the gateway never repeats one |
| `Transmit` | id, to (6), urgent (bool), give_up_ms (u16), data (up to 1024): unicast, retried while the node is on channel, up to `give_up_ms` (commands 2,000; time syncs 300) |
| `Cancel` | to (6): a newer command for this node replaces the queued one |

UART load: version 2's gateway sent each relayed frame with a 9-byte `Via`. Now each packet
costs its payload plus 6 + 28 + about 12 bytes. With about five frames per batch, that comes
out slightly smaller than before.

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

Before, nothing on the node limited switches. Each switch erases `rsjob` and writes `otadata`,
so a stream of role changes is a reboot loop and wears the flash. Per-node keys already mean
only the hub can command one. The node adds its own limit as well:

* A switch is allowed only after the running image has been up for `min_uptime`.
* `min_uptime` = 60 s × 2^streak, capped at 1 h. `streak` is carried in the job record
  (`nodeimage`, record magic `RSJ2`, one byte more). It goes up with each switch and resets
  only after the image has sat ten minutes past its own wait without being asked to switch
  (the first draft's rule could never reach the cap).
* A refused switch keeps the current role and plan, is counted in `NodeHealth`, and the hub
  sees `applied = false`. Worst case about 24 switches a day once at the cap.

### 7.2 Plans that keep the gateway channel, and a deaf fall-back

* `coverage::plans::with_espnow_visits` **must never drop a visit**. It used to push into a
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
  role's built-in plan, reports plan id 0 and counts the fall-back. If the gateway is still
  silent 30 s later it was an outage and the plan is not blamed; otherwise that plan id is
  refused for 10 minutes, doubling on each repeat up to 1 hour. Nothing is refused until
  reboot, and the firmware never resets the link itself (`airlink` relinks on its own).

### 7.3 Delivery accounting

A radio ACK proves only that something on the air acknowledged the frame. The node keeps
counting what it believes it delivered (`NodeHealth`, cumulative), and the hub counts what
it verified per slot. Every `NodeHealth` the hub compares the two over a sliding 60 s. If the
node claims at least 50 frames more than arrived and more than 20 % of what it sent, the hub
raises `LinkLoss` naming the node: an evidence event and a screen entry. Cumulative counters
make a lost `NodeHealth` harmless. If forged ACKs swallow everything, `NodeHealth` stops
arriving too, and the existing deaf-node alarm fires.

**Reports swallowed, the rest let through.** The comparison runs when a `NodeHealth` arrives.
A captured gateway can tell which packets carry one (frames are authenticated, not hidden), so
dropping just those would blind the comparison while it dropped whatever else it liked. A node
that has reported before, is still heard (frames verified in the last 10 s, at least 10 since
its last report) and whose last report is 30 s old is therefore raised as the same `LinkLoss`,
with `detail` 0 (the ordinary finding's detail is the percentage missing, 20 or more). Found on
the bench, 2026-11-19: with a steady 80 % drop pattern no health report arrived for 110 s and
nothing was raised. The hub prints the numbers every 5 s:
`hub-p4: delivery node <n>: claimed=<c> received=<r> over <s> s (<k> report(s), last <a> s ago)`. Downlink: the gateway's `TxDone` is
also radio-level only. The hub already resends `Assign` every few seconds and confirms by
the plan id echoed in `Status`, which stays the proof.

### 7.4 Randomness after the radio starts

Every node takes its randomness from the hardware generator only after the radio has
started (`firmware/node-common/src/linkauth.rs`): on the S3 and C3 `esp_hal::rng::Trng::try_new()`
(boot line `"rng":"trng"`); on the C5, whose TRNG esp-hal 1.2 does not model, the RNG once
the radio runs, which Espressif's reference manual describes as true random then
(`"rng":"radio"`). The first draws must pass a health check. If not, the keyed link does not
start: the node sends no reports, says `no_rng` on USB, and its announcements carry
`<firmware> norng`. The node `session` is what makes a command recorded before a
node reboot useless after it, and a session that repeats across boots would undo that. The
P4 has neither a radio nor HAL support for its TRNG, so the hub uses the `rsboot` counter
for its epoch, and the secure element's random source once fitted.

### 7.5 Node numbers and mutes

Before, hub node numbers were table indexes, reused 60 s after a node goes silent, so a
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

Defence in depth (built, **Owner**): esp-radio's ESP-NOW receive callback turned
`data_len: i32` into a slice length with `as usize` and no check, then copied it into a heap
box, so a negative or oversized length from any future blob bug would become a huge read.
Every radio firmware (gateway, node-wifi, node-ble, node-sdr, node-lr, the S3 web head,
rid-beacon) now takes the patched copy in `firmware/patches/esp-radio` through
`[patch.crates-io]`, whose receive callback drops a null pointer or a `data_len` that is
`<= 0` or `> ESP_NOW_MAX_DATA_LEN_V2` (1470) and counts it
(`esp_radio::esp_now::bad_rx_len_drops()`); how the copy is made and checked is in
`firmware/patches/README.md`. The same copy carries the older ESP32-C5 RF front-end clock fix,
which now also reaches node-ble built with ESP-NOW (Wi-Fi and BLE together): a bench check.

`tools/check-wifi-blobs.sh` (offline) checks, for every `esp-wifi-sys-esp32{c3,c5,s3}` crate a
firmware lockfile names, the git blob hashes of the three libraries against this list, which is
identical to `espressif/esp32-wifi-lib` at `7cd4d7b` (checked 2026-11-18). A dependency bump
therefore fails the check until the new libraries' provenance is checked and recorded. It also
checks that every lockfile takes the patched esp-radio and that the patched copy differs from
the crates.io release (its `.crate` SHA-256 checked) only in the two documented files.

| Chip | `libespnow.a` | `libnet80211.a` | `libpp.a` |
|---|---|---|---|
| ESP32-C3 | `f9e4d48d42810e010aebb2330c9b149da7ad478c` | `fedc7af8d98e6ed9f25e83ece00c4b5436fd205c` | `970e34ec35ac8d17509fe8a6093ec86067b8af4c` |
| ESP32-C5 | `b085d2d8265c8d78baf9a3d05ff312a9d047f7a3` | `12e0f60a32766dd48e57e78b5e8d0b4fdf87bed7` | `95e0cb8c4ca8bea21d0edd66d511ea5644215c11` |
| ESP32-S3 | `5804eddb93b3144781f1457e91722a7a19b77ad5` | `a19d4e6cedf2a6c7da64762801e846c9347971dc` | `a1364a6bbdb234699f28546d0ed01ad755f9931c` |

(all `esp-wifi-sys-*` 0.3.0, git blob hashes as `git hash-object` prints them)

## 9. Test plan

Host tests were written first and failed on the old code. Bench checks use the kit (P4 hub,
gateway `c5c`, nodes `c5a` and `c5b`) and only on-chip injection: the `forgetest` and
`floodtest` builds, and the gateway's `gwforge` feature. The step-by-step bench pass, with
commands, is [BENCH.md "Link keys: bench checklist"](BENCH.md#link-keys-bench-checklist). Three
bench aids named below were not built (the hub console `bench switchstorm`, `floodtest`'s
`job deaf` and a hub command sending a 24-hop 5 GHz plan, and the `faulttest` randomness
hook); those rows rest on their host tests. All forged traffic comes
from our own bench boards through their normal radio path. No separate attack transmitter
is used.

New `forgetest` commands (node-wifi). A node in this build behaves as a **captured node**:
it holds its own key only.

| Attack | Host test (crate) | Bench check |
|---|---|---|
| Captured node speaks as another node | `airlink`: `node_key_cannot_seal_for_another_mac`; hub verifier `slot_key_bound_to_mac` | `forge claim <c5b-mac>` (c5a sets its station MAC to c5b's and seals with its own key): hub `bad_tag` for c5b's slot +n, no c5a frames under c5b, c5b keeps reporting |
| Captured node commands another node | `airlink`: `command_under_other_node_key_rejected` | `forge cmd <c5b-mac>` (unicast `Assign` sealed with c5a's key): c5b `link_auth.bad_tag` +1, plan unchanged |
| Captured node forges a beacon | `airlink`: `beacon_tag_checked_per_slot`, `beacon_with_other_slot_tag_rejected` | `forge beacon`: c5b `bad_tag` +1, gateway address unchanged |
| Captured gateway forges or alters | hub verifier: `gateway_has_no_key_and_cannot_forge`, `altered_command_rejected_at_node` | gateway `gwforge`: `gwforge batch` (hub `bad_tag`), `gwforge flip <mac>` (node `bad_tag`), `gwforge replay` (hub `replay`) |
| Withheld node batch | `airlink`: `withheld_batch_late_at_hub` (version 2 accepted it in order) | `forge hold 5`: c5a seals a batch, waits 5 s, sends: hub `late_uplink` +1, nothing from it in the tracker |
| Withheld command or time sync | `airlink`: `command_late_after_3s`, `timesync_late_after_half_second`, `jammed_node_bound_is_tenth_of_jam` | `gwforge delay 1000` on the next `TimeSync`: aero node (or c5a in `ROTORSCOPE_BENCH` aero-time mode) `late` +1 |
| Forged ACK / silent loss | hub: `claimed_minus_received_raises_link_loss`, `lost_nodehealth_is_harmless` | `gwforge drop <c5a-mac> 50` (the gateway's radio ACKs, then discards half; same effect as forged ACKs): `LinkLoss` for c5a within 60 s, ends after `gwforge drop off` |
| Key in images | `kitkeys`: record round trip, CRC, MAC mismatch, both copies damaged; `tools/check-image-identity.sh` builds node-wifi and the gateway in two checkouts (one with decoy key files and the old key variable) and compares SHA-256 (identical); `airlink`: `v2_packet_is_old_firmware` with no key in the build | flash an image with `rskey` erased: node prints `missing`, sends announcements, hub strip `UNPROVISIONED NODE`; `link-key node --bench-corrupt` gives `damaged` |
| Mixed kit | `airlink`: `v2_packet_is_old_firmware`; hub: `gateway_hello_13_refused` | flash c5b with the current (v2) build: hub `old_firmware` count and strip; gateway at v13: hub strip `OLD FIRMWARE: gateway` |
| Flood on a roster MAC | gateway budget logic in a host-testable module: `per_mac_budget`, `round_robin_uart_queue` | `forge spam 500` (bad-tag packets as c5a): gateway `over_budget`, c5b's delivery unchanged, hub CPU from the `{"t":"hub"}` line under 10 % |
| Image-switch storm | `node-common` policy, host: `switch_backoff_doubles_and_caps` | hub console `bench switchstorm c5a` (alternating Wi-Fi / BLE every 5 s): c5a refuses after the first, `NodeHealth.switch_refused` rises, at most one reboot a minute |
| Deaf plan | `coverage`: `full_24_hop_5ghz_plan_keeps_channel_6` (failed before 13d); node: `stored_plan_without_6_refused`, `deaf_60s_falls_back` | hub console sends a 24-hop 5 GHz plan to c5a: c5a keeps reporting; `floodtest` console `job deaf` stores a channel-6-free job and reboots: c5a back on the built-in plan within 60 s |
| Weak randomness | - (hardware) | every radio board's boot line shows `rng:"trng"`; a `faulttest` hook calling it before the radio starts shows the keyed link refusing to start |
| Node-number reuse | `tracker`: `mute_does_not_follow_number_to_new_node` | (host only) |
| Held fix shown live | `webui`: `held_fix_not_shown_as_live` | GPS emulator `hold` scenario: phone page shows "held" |

## 10. How it was built

Spike S0 ran first on a throwaway branch (`spike/p3-s0`, never merged; results in section 13).
Then nine packages, test-first, each with its own files:

| Pkg | Scope | Where it lives |
|---|---|---|
| 13a Kit keys | derivation, records, roster, boot counter log; `link-key` subcommands | `crates/kitkeys`, `tools/link-key` |
| 13b Packets | version 3 packets, tick clock, multi-tag beacon, uplink age, ordered epochs | `crates/airlink/src/v3` |
| 13c Wire types | `link::gw`, `NodeHealth`, `HubFix` held flags, `LinkLoss`, detail codes 8-16, protocol 14 | `crates/link` |
| 13d Node robustness | plan visits, 49-hop node plans, `RSJ2` job record, switch back-off, deaf fall-back, `airlink::GATEWAY_CHANNEL` | `crates/coverage`, `crates/nodeimage`, `firmware/node-common`, node-wifi `role.rs` |
| 13e Hub verifier | `rskit`/`rsboot` at boot, key derivation through `KeySource`, per-slot state, sealing, `GwUp`/`GwDown`, delivery accounting, screen strip | `crates/hublink`, `firmware/hub-p4/src/{linkauth,links,flashrom}.rs`, `tools/partitions-hub.csv` |
| 13f Secret-free gateway | keyless relay, roster filter, budgets, transmit queue, `TxDone`, `gwforge` | `crates/airlink/src/relay.rs`, `firmware/gateway` |
| 13g Node keystore and uplinks | `rskey` reading, randomness after the radio, announcements, `NodeHealth`, `forge` commands | `firmware/node-common/src/{keystore,linkauth}.rs`, `crates/nodeimage/src/uplink.rs`, each node's `uplink.rs`, `tools/partitions-*.csv`, `tools/flash-node.sh` |
| 13h Tracker and web | `forget_node`, `LinkLoss`, held fix | `crates/tracker`, `crates/webui` |
| 13i Cut-over | compiled key and version 2 deleted; esp-radio receive-length patch and blob check; image-identity check; `link-key` boot-counter seeding and interrupt safety; docs | `crates/airlink`, `firmware/patches`, `tools/check-wifi-blobs.sh`, `tools/check-image-identity.sh`, `tools/repro-env.sh` |

The gateway, hub and nodes speak protocol 14 only with each other: 13e, 13f, 13g and 13i go
to the hardware as one set, and the bench checklist is run on that set before the owner
merges it.

**Image identity.** No build reads a key, so an image is the same for every kit:
`tools/check-image-identity.sh` builds node-wifi and the gateway from two independent checkouts,
one with decoy key files and the retired key variable set, and the flashed images (and the ELFs)
came out identical on 2026-11-18. Three things that are not secrets still make builds of one
commit differ, and the check deals with each:

* the build time and date in the app descriptor (`esp-bootloader-esp-idf`; 68 bytes on
  node-wifi): `tools/repro-env.sh` sets `SOURCE_DATE_EPOCH` to the commit's time;
* absolute source paths in panic messages for crates outside each firmware's own workspace
  (the checkout, the cargo registry, so the builder's home directory): `repro-env.sh` adds
  `--remap-path-prefix` to the firmware's own rustflags;
* the checkout's directory itself: cargo hashes the location of every path dependency outside
  the firmware's workspace (every crate under `crates/`) into symbol names, and the linker lays
  the image out differently. Nothing in rustflags undoes that on stable Rust, so identical
  images need the same directory path (the check moves each checkout to one path in turn;
  `--paths` shows the difference). Making it path-free would mean one firmware workspace that
  contains `crates/`, or cargo's unstable `trim-paths`.

`tools/flash-node.sh` builds with `repro-env.sh`; the gateway and hub are built by hand, and
the bench checklist gives the gateway command with it.

## 11. Limits that remain

* **A captured hub is a captured kit** until a secure element is fitted: the master is in the
  hub's flash.
* **No flash encryption or secure boot** (WP-15): reading a board takes a USB cable and a
  minute. A captured node gives away its own key, and can be reflashed with hostile firmware
  that keeps it: a captured node stays a hostile node until its slot is revoked.
* **The wired bus is unauthenticated** (section 4.6): whoever can touch the bus wires can
  report as any node on it; the origin checks still stop the hub-only frames.
* A forger can crowd out one node by flooding its MAC (equivalent to jamming; visible as
  `over_budget`, the node going quiet and `link_loss`).
* A node that has just booted, or relinked, takes the first genuine beacon of any epoch as a
  provisional epoch; it moves to the right one at its first Sync. A node kept deaf by jamming
  all through a hub reboot can be handed a held `TimeSync` once its epoch moves; the 60 s
  relink caps how old it can be, and the aero nodes' three-agreeing-syncs rule still applies.
* **The CVE fix is established by provenance, not by test** (section 8); the esp-radio patch
  is a second line, compile-checked only.
* Nothing is encrypted: anyone in range can read node reports and hub commands.

## 12. Decisions and corrections (history, short)

Review of 2026-11-18: the recommendations marked **Owner** were taken: the master in the hub's
flash until a secure element ("a captured hub is a captured kit"), no keys on the wired bus,
8-byte beacon tags, 40 packets/s and 16 KB/s per roster MAC (plus the 50 KB/s total cap S0
asked for), the patched esp-radio and blob check in every radio firmware, kits ship open.
Additions the packages covered: ordered epochs with tests (section 6.1), one boot counter for
the link and the evidence log (`rsboot`), `KeySource` as the seam for a secure element (no
code outside it reads the master), and no hub randomness relied on for security.

Corrections made while building, already folded into the sections above: beacon size 24 + 8n
(not 32 + 8n); lateness 3 s and 0.5 s while beacons are heard, 3.3 s and 0.55 s otherwise;
master and roster as separate hub records; 8-byte boot counter entries; generations from 1;
the streak rule and the deaf fall-back's blame rule; detail codes 13-16 and nine node-side
reject reasons in `NodeHealth`; SHA-256 at about 4,800 cycles a block. Left over for the
cut-over and done there: seeding the boot counter above a readable old value, removing the
temporary key file on Ctrl-C, `coverage` taking `airlink` as a normal dependency, `NodeHealth`
counted as health in the node's queue budget.

## 13. Spike S0 (2026-11-18), short

Measured on the real kit with a throwaway build. Hub verify 0.24-0.25 ms per full packet warm,
0.3-0.8 ms in the live path; hub time spent verifying 0.9 % under the heaviest real flood,
2.8 % / 6.3 % with full / tiny packets at line rate. Gateway UART at most 4.8 % with both nodes
flooded at 1000/s; the line saturates at about 87 full packets/s. Longest beacon gap 943 ms
(once, at the boot SD mount), at most 546 ms while running, **only after** the gateway's hub
UART moved to an interrupt-fed ring and its USB printing stopped masking interrupts (before
that it lost 45-58 % of the hub's messages). These findings set the gateway's receive path, the
total cap, the hub's 64 KB PSRAM receive ring and 256-frame tracker queue, the separate
gateway-message decoder (1,100 bytes), recognising the gateway by its protocol 14 `Hello`,
beacons from both the main loop and the screen-band callback, and `flash-node.sh` defaulting
to the ESP-NOW node-sdr build. Not measured: on-air timing of hub-made beacons and time syncs,
the real `link::gw` framing under load, 5 GHz node plans.
