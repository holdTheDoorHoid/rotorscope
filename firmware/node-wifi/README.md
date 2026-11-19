# node-wifi: Wi-Fi Remote ID listener

One source tree, two nodes:

| Node | Chip | Band | Channel plan |
|---|---|---|---|
| A | ESP32-S3, or ESP32-C5 built `cargo fw24` | 2.4 GHz | 6 (150 ms) → 1 (50 ms) → 6 (150 ms) → 11 (50 ms): ~75% on 6 |
| C | ESP32-C5 | 5 GHz | 149 (150 ms) alternating with 153/157/161/165 (50 ms each): ~75% on 149. Set `INCLUDE_UNII1` in `src/chip.rs` to add 36–48. |

It listens in promiscuous mode (receive-only), finds Remote ID beacons (vendor element
`FA:0B:BC`/`0x0D`) and NAN action frames (to `51:6F:9A:01:00:00`) with `crates/rid`, and reports each
hit on USB as a JSON line and to the hub as a `link::Frame`.

It also recognises open digital FPV / telemetry links that inject ordinary 802.11 data frames
(wfb-ng and OpenIPC, OpenHD, RubyFPV, DroneBridge for Linux) with `crates/openlink`, and reports each
stream at most once a second with a running frame count. Nothing is decoded beyond their headers.

## Build

The chip is chosen by the directory you build from. Each holds only the toolchain pin and cargo
config; both share `Cargo.toml` and `src/`, so cargo finds the package one level up.

```sh
# Node A, ESP32-S3 (Xtensa: Espressif's `esp` toolchain from espup)
. ~/export-esp.sh
cd firmware/node-wifi/s3 && cargo fw
#   -> firmware/node-wifi/target/xtensa-esp32s3-none-elf/release/node-wifi

# Node C, ESP32-C5 (RISC-V: stable Rust)
cd firmware/node-wifi/c5 && cargo fw
# Node A on an ESP32-C5 instead (2.4 GHz, same plan as the S3; verified on air 2026-11-12)
cd firmware/node-wifi/c5 && cargo fw24   # -> ../target-band24/riscv32imac-unknown-none-elf/release/node-wifi
#   -> firmware/node-wifi/target/riscv32imac-unknown-none-elf/release/node-wifi
```

`cargo fw` is an alias for `cargo build --release --features esp32s3` (or `esp32c5`). Building
from the wrong directory, or without a chip feature, stops in `build.rs` with a message saying so.

Flash on the bench with `tools/dev-board.sh <elf> [seconds]` (it holds the shared board lock).

## Outputs

### USB (USB-Serial-JTAG): JSON lines

Each line is one JSON object with a `"t"` tag. The ESP-IDF second-stage bootloader prints a few
`I (..) boot:` lines at reset, and a panic prints a backtrace: readers should skip lines that do
not start with `{`. Diagnostic logging is off (`LOG_LEVEL` in `src/main.rs`) to keep the stream clean.

```json
{"t":"hello","fw":"node-wifi","ver":"0.1.0","chip":"esp32c5","role":"Wifi24","proto":4,"fcs":4,"can":7,"images":["node-wifi","node-ble",null],"plan_id":0,"plan":[[6,150],[1,50],[6,150],[11,50]]}
{"t":"stats","up_us":59266007,"ch":6,"seen":3131,"fps":62,"data":5210,"open":0,"open_ev":0,"rid":0,"drop":0,"qmax":1,"other":0,"trunc":12,"ch_err":0,"hub_drop":0,"iefit":[2886,0,0],"per_ch":[[6,2652],[1,410],[11,69]]}
{"t":"rid","tr":"WifiBeacon","ch":6,"rssi":-60,"mac":"aa:bb:cc:dd:ee:ff","ctr":12,"up_us":123456,"msgs":[{"BasicId":{...}},{"Location":{...}},"0d1f..."]}
{"t":"open","sys":"WfbNg","id":7669206,"port":0,"dir":"AirToGround","ch":161,"rssi":-63,"mac":"57:42:75:05:d6:00","up_us":123456,"n":4321}
{"t":"err","what":"set_channel","ch":149,"e":"..."}
```

- `rid`: one per Remote ID frame. `msgs` holds each message decoded by `rid` (serde shape of
  `rid::message::Message`), or its 25 bytes as hex if the decoder rejected it. If the payload could
  not be split into messages at all, `msgs` is empty and `err` names the `rid::Error`.
- `open`: one stream of an open FPV link (system, port, direction) with new frames, at most once a
  second per stream. `sys` is `WfbNg` / `OpenHd` / `RubyFpv` / `DroneBridge`; `id` the cleartext
  identifier (wfb-ng link ID, here the default `0x7505d6`; RubyFPV vehicle ID; DroneBridge comm ID) or
  null; `n` the frames counted on that stream since the node started tracking it. The node is a raw
  sensor and prints the identifier; the hub and `rid-view` hide it unless their operator opts in.
- `stats`, once a second. Totals since boot unless noted:
  `seen` management frames, `fps` of them in the last second, `data` data frames looked at, `open`
  of those recognised as an open link, `open_ev` open-link streams evicted from the 16-entry table,
  `rid` Remote ID frames,
  `drop` frames lost to a full queue, `qmax` most queue slots in use in the last second (of 32),
  `other` callbacks that were neither management nor data (filter leaks), `trunc` frames over 512 bytes
  (kept truncated), `ch_err` failed channel changes, `hub_drop` hub frames not sent (everything in
  `shed` plus encode / UART errors), `per_ch` frames per channel, and `iefit` (below).
  Under a frame flood (docs/BENCH.md "Frame flood"): `shed` hub frames turned away per kind
  (`rid_known`, `rid_new`, `health`, `threat`, `wdrone`, `dji`, `open`, `other`: over the kind's
  budget, over a known Remote ID source's rate, or no room left for that kind in the 8-frame hub
  queue; a frame shed by its budget is not printed on USB either), `rid_src` Remote ID sources known
  now (their frames get the queue's reserved room), `usb_drop` USB lines dropped (host slow or not
  reading), `alt` [DJI, Wi-Fi drone] report alternations (one address going back to an earlier
  content after a different one: two transmitters on one address), `clamp` frames whose `sig_len`
  claimed more than one receive buffer holds (clamped before reading).
- USB output never blocks: lines go through an async writer (`src/usb.rs`) and are dropped (counted
  in `usb_drop`) when the host does not keep up. Only a panic backtrace is written directly.
- `err`: something the node could not do. A `set_channel` error is printed once per plan entry,
  then only counted in `ch_err`.

### Hub UART: `link::Frame`

postcard + COBS via `link::encode`, on UART1 TX at 921600 baud (`chip::HUB_BAUD`):
S3 GPIO43, C5 GPIO11 (the boards' TX pins; see `hub_tx_pin!` in `src/chip.rs`). `Hello` every 5 s,
`Stats` every second, `Detection` (raw messages) per hit, `OpenLink` per `open` line (protocol 2).
`Stats::dropped` is everything the node lost: receive-queue drops (`drop`) plus hub frames not sent
(`hub_drop`); the split by kind is only on USB (`shed`), since the frame has no field for it. On the S3 the ROM bootloader also prints
on GPIO43 for a moment after reset; the hub's decoder discards that as a corrupt frame.

## Design notes

- **The receive callback only copies.** It runs on the Wi-Fi driver's thread. It drops anything
  that is not a management frame (checking frame control itself, as well as setting the driver's
  management-only filter), copies up to 512 bytes into one of 32 static slots, and passes the slot's
  index to the `drain` task. A full queue is counted in `drop`, never waited on.
- **Data frames never use the queue.** With `chip::OPEN_LINKS` (on) the promiscuous filter also passes
  data frames, and the callback classifies them on the spot with `openlink::classify`: ordinary (QoS)
  data fails on its first frame-control byte, other plain data on the address bytes that follow, so a
  busy channel costs one short callback per frame and nothing more. A hit only bumps that stream's
  counter in a 16-entry table (`rx::SIGHTINGS`, under a critical section); no slot is taken, so data
  traffic can never crowd Remote ID out of the 32 slots. The `open_links` task reads the table every
  250 ms and reports each stream that has new frames at most once a second. If `drop` ever rises on a
  busy channel, set `OPEN_LINKS` to `false`: the callback count, not the queue, is the cost.
- **Cheap pre-check before decoding.** `drain` only calls `rid::wifi::extract` on beacons containing
  the `FA 0B BC 0D` byte sequence and on action frames addressed to the NAN Remote ID address.
- **FCS: `PromiscuousPkt.data` ends with 4 bytes that are not part of the frame.** esp-radio sizes
  `data` by `sig_len`, which includes the FCS slot on every MAC generation. `rx::RxFrame::mpdu()`
  drops them before anything parses the frame (`chip::FCS_LEN`). Checked on hardware: on the S3,
  2,886 of 2,886 beacons in a 60 s run had their element list end exactly at `sig_len - 4`
  (`iefit:[2886,0,0]`). On the C5, DRONK measured the same (the MPDU ends at `sig_len - 4`; the
  4 bytes are not a valid CRC). The `iefit` counter keeps checking: `[at sig_len-4, at sig_len,
  neither]`. If the middle number ever dominates, `FCS_LEN` is wrong for that chip/driver.
- **Country and band.** Both chips call `esp_wifi_set_country_code("US")` directly: esp-radio
  1.0.0-beta.1's own `CountryInfo` path passes a zero 5 GHz channel mask, and upstream's default
  is moving to "01" (world-safe), which rejects channel 149. The C5 also sets the band to 5 GHz
  explicitly before the first channel change. These two calls (and the promiscuous filter) go
  straight to the Wi-Fi blob through `esp-wifi-sys-*`, because esp-radio does not wrap them yet.
- **esp-radio version.** `=1.0.0-beta.1`. beta.2 is yanked on crates.io (its only change fixed
  ESP32-S31 builds).

## Open FPV links: what is not known yet

Built and unit-tested only. Nothing has been checked against a real transmitter:
- **HT coding.** wfb-ng's default video profile sends MCS frames with LDPC and STBC. Whether the S3's
  and C5's promiscuous receivers deliver LDPC- or STBC-coded HT frames at all is unverified until
  bench-tested; OpenHD's default 40 MHz channels are a similar unknown. Frames the radio cannot decode
  are simply never seen (no false alarm, just a miss).
- **Unusual addresses.** wfb-ng's and RubyFPV's transmitter addresses are group addresses, and
  DroneBridge frames have no MAC header at all. Whether the ESP's receive path passes such frames to
  the promiscuous callback is unverified.
- **RTS mode.** wfb-ng and DroneBridge can also send their header in RTS control frames. The filter
  does not pass control frames, so those links are not seen; `openlink` would classify them.
- **Channels.** The C5's plan visits 161 and 165 (wfb-ng's and OpenIPC's defaults) for 50 ms each and
  sits on 149 (OpenHD's default) most of the time; the S3 hears 2.4 GHz links on 1/6/11 only.

## Status

- ESP32-S3: verified on the bench (60 s runs: no drops, queue never above 1 slot, no panics, hop plan
  followed). No Remote ID source was available yet, so the decode/report path has not seen a
  real frame on hardware.
- ESP32-C5: builds; not yet run on hardware. First boot should show no `set_channel` errors and
  `per_ch` rising on 149.
- *Update 2026-11-18: both superseded on air (docs/BENCH.md). On 2026-11-12 the S3 decoded Wi-Fi
  beacon and NAN Remote ID from a C5 `rid-beacon` (`--group cross`), and the C5 decoded 5 GHz
  beacon and NAN (`--group c5`) and, built `cargo fw24`, 2.4 GHz beacon and NAN from a C5 and from
  the S3. On 2026-11-14 the C5 heard fake open FPV links on channels 6, 149, 161 and 165 (16 of 16
  PASS, `--group openlink`).*


## Bench tests

Two-board tests that flash and judge this firmware: `tools/bench/run.py --list` (see `docs/BENCH.md`).

## Roles, hub commands and the multi-image node (protocol 4)

- **Status.** With every Hello (boot, then every 5 s) and whenever its job
  changes, the node sends `Frame::Status { role, can, plan }`: the role it
  is running (Wifi24 or Wifi5), the roles the *board* can take (this image's
  plus those of every other image in its flash, read from the app slots'
  descriptors; `crates/nodeimage`, `firmware/node-common`) and the plan id it
  applied (0 = built-in).
- **Assign.** From the ESP-NOW gateway (airlink command packets) or, on a
  wired node, the hub UART's RX pin: **C5 GPIO12 (XIAO D7)**, S3 GPIO44 (D7),
  921600 baud, the same `link::Command` stream the gateway reads. A Wi-Fi job
  is applied at once: the hopper drops its current dwell and starts the new
  plan's `(channel, dwell ms)` list; empty plan = built-in plan for that role.
  The band (2.4 / 5 GHz) is chosen per hop from the channel, so one C5 image
  does both; `band24` only picks the role it boots in. ESP-NOW builds add a
  40 ms visit to channel 6 at least once a second if a plan lacks one (same
  rule as the hub, `coverage::plans::with_espnow_visits`) and still send only
  while tuned to 6.
- **Another role** (Bluetooth, DroneID, FPV): `node_common::Images::switch`
  writes the job to the `rsjob` sector, points `otadata` at the slot holding
  that image and resets; the other image starts with the job and echoes its
  plan id. No image for the role: an `err` line, the node carries on.
- USB lines: `assign` (new job applied), `switch` (leaving for another
  image); `hello` has `can`, `images` (per slot) and the plan; `stats` has
  `role`, `plan_id`, `cmds` [applied, repeats, rejected], `per_ch`
  `[channel, frames, visits]` for the current plan, `band_err`, `dji`, `wdrone`.

Flash a multi-image node (node-wifi in `ota_0`, node-ble in `ota_1`, node-sdr
in `ota_2` when built) with `tools/flash-node.sh <board>`; layout in
`tools/partitions-node.csv`.

## DJI Wi-Fi DroneID and Wi-Fi drones

- Beacons carrying DJI's legacy DroneID vendor element (OUI `26:37:12`) are
  forwarded raw as `Frame::DjiWifi` (element body from the OUI on; the hub
  decodes it with `rid::dji::parse_element`). The USB `dji` line has only
  channel, RSSI, address, subcommand and length: the node never decodes the
  record, so it never prints a serial.
- Beacons and probe responses whose address or network name matches the
  maker table (`wifidrone::classify`) go out as `Frame::WifiDrone`, USB `wifidrone`.
- Both at most once a second per transmitter *and content* (24 pairs tracked per kind, at most two
  contents per transmitter a second; `wifiwatch::contentlimit`), so frames forged from a drone's own
  address with another content cannot take the drone's place. An address whose reports alternate
  between contents is flagged: `"alt":true` on its `dji` / `wifidrone` lines for a minute, and the
  stats line's `alt` counts alternations. Then each kind's budget (8 a second) caps the total.
- Neither has been seen from a real drone yet (no DJI Wi-Fi drone or listed
  maker's drone on the bench).

## Attack detection (protocol 9)

`crates/wifiwatch` looks for hostile Wi-Fi activity; see `docs/WIFI-ATTACKS.md`
for what each detector looks for, thresholds and known false alarms.

- **Header-only frames never use the queue.** Deauthentication,
  disassociation, authentication, (re)association and probe requests are
  counted in the receive callback from their headers (`rx::FLOODS`, under a
  critical section) and dropped; only beacons, probe responses and action
  frames take a slot. A deauth flood cannot crowd Remote ID out. The
  `threats` task closes the flood windows four times a second.
- Beacons, probe responses and action frames go through
  `wifiwatch::beacons` in `drain` (beacon floods, Karma, evil twins,
  channel-switch abuse, known tools).
- Each finding: USB `{"t":"threat","kind":"deauth","ch":6,"rssi":-48,"n":412,"win_ms":1000,"src":..,"dst":..,"detail":7,"ssid":"","up_us":..}`
  (`tool` names an `attack_tool`), and `Frame::Threat` to the hub.
- Stats: `kill` (deauth + disassoc frames), `auth`, `probe`, `csa`, `threat`
  totals; `scout` `[visits, last channel]`. `per_ch` now also counts the
  header-only frames.
- **Scouting.** Every 3 s the hopper leaves its plan for 105 ms to visit the
  next channel of its band(s) the plan skips (`coverage::plans::scout_channel`):
  2.4 GHz 1-14, 5 GHz 36-177. For that the radio's receive regulatory domain is
  opened up (`chip::RX_ALL_CHANNELS`: manual policy, 2.4 GHz 1-14, every 5 GHz
  channel); a failure prints `{"t":"err","what":"esp_wifi_set_country"}` and the
  node carries on with the US set. Nothing new is transmitted.
- Not yet checked on hardware: tuning 12-14 and 169-177, the scouting cost on
  Remote ID, and behaviour under a real deauth flood.
- **Hostile MAVLink over open Wi-Fi** (link protocol 13, `wifiwatch::mavwifi`,
  docs/DRONE-ATTACKS.md "Over Wi-Fi"): only while the hub's `control content`
  opt-in is on (it arrives as a `ControlFlags` entry at the end of the Wi-Fi
  plan; off at boot). The callback then reads unprotected data frames' headers
  down to UDP/TCP and queues likely MAVLink (own 4 slots, 100 a second, bursts
  of 10) for the `mavlink` task, which runs `mavwatch` and sends `Threat`
  (`MavlinkAttack`, with the Wi-Fi channel) + `MavlinkEvidence`. USB:
  `{"t":"mavlink","on":true}` on a change, `{"t":"mavlink_attack",...}` per
  finding; stats `mav` = `[on, candidates, rate-limited, no slot, analysed,
  findings]`.

## ESP-NOW uplink (experimental)

`cargo fw24-now` (from `c5/`) builds the 2.4 GHz C5 node with the `espnow`
feature: hub frames go over ESP-NOW to `firmware/gateway` instead of the UART.
The node then transmits. See `docs/ESPNOW.md`.
