# tracker

Hub logic, `no_std` and allocation-free: merge Remote ID detections from every
radio node into one track per drone, compute range and bearing from the unit,
and format tracks as JSON lines, TAK Cursor-on-Target and CSV. The laptop
viewer (`tools/rid-view`) runs the same code, so the hub and a laptop agree.

## API in brief

```rust
let mut tracker: Tracker = Tracker::new(Config::default()); // 64 tracks; Tracker<32> for fewer
tracker.set_own_position(lat, lon, Some(hae_m), now_ms);     // from GPS; omit/clear when no fix
tracker.ingest(node, &detection, now_ms);                     // link::Detection, raw messages
tracker.ingest_messages(&obs, &decoded, errors);            // already-decoded (laptop JSON)
while let Some(id) = tracker.take_new_drone(now_ms) { beep(id) } // once per drone
tracker.expire(now_ms);                                       // call ~1 Hz

let ctx = tracker.context(now_ms, utc_ms);  // utc_ms: Option<u64>, from GPS time
for t in tracker.tracks() {
    json::track_line(t, &ctx, &mut buf)?;   // phone page / USB stream
    cot::drone_event(t, &ctx, &mut buf)?;   // TAK, needs utc_ms and a position
    cot::operator_event(t, &ctx, &mut buf)?;
    csv::row(t, &ctx, &mut buf)?;           // SD log; csv::HEADER first
}
tracker.ingest_open_link(node, &sighting, now_ms);           // link::OpenLinkSighting
for l in tracker.open_links() {
    json::link_line(l, &ctx, &mut buf)?;    // {"t":"link",...}
    csv::link_row(l, &ctx, &mut buf)?;      // same CSV columns, track "L<n>"
}
tracker.ingest_aircraft(&report, now_ms);                     // adsb::Aircraft (ADSBee CSBee line)
for a in tracker.aircraft() {
    json::aircraft_line(a, tracker.tracks(), &ctx, &mut buf)?; // {"t":"aircraft",...}
    cot::aircraft_event(a, tracker.tracks(), &ctx, &mut buf)?; // ICAO-<hex>; Duplicate if it is a RID drone
    csv::aircraft_row(a, &ctx, &mut buf)?;  // same CSV columns, track "ICAO-<hex>"
}
```

`now_ms` is hub uptime in milliseconds throughout. UTC is only needed for CoT
and the CSV `utc` column.

**Identity.** The key is the Basic ID UAS ID. A track holds at most one ID per
ID type (e.g. a serial number and a session ID). Until a Basic ID arrives, a
track is keyed by the transmitter MAC. The MAC is matched within a medium: Wi-Fi
beacon and NAN share one namespace, and the three BLE transports share another.
When an ID turns up on a MAC-keyed track, or a frame links two tracks, they are
merged. Two IDs of the same type with different values are never merged.

**Expiry.** A track goes `stale` after 60 s without a frame and is removed after
10 min. When the table is full, the oldest stale track is evicted, or else the
least recently heard one.

**Alerts.** `take_new_drone` hands out each track once. A track that has an ID
is due at once. A MAC-only track waits 1 s (`announce_grace_ms`), so that BLE
adverts about to merge into a known drone do not beep twice.

## JSON line schema (`json::track_line`)

There is one object per track, ending with `\n`. Fields are always present, and
`null` means unknown. Enum values use the `rid` crate's names, the same ones
the nodes' `{"t":"rid",...}` lines use.

| Field | Type | Meaning |
|---|---|---|
| `t` | `"track"` | Line type tag |
| `id` | int | Track handle. Stable while the track lives, never reused, not stable across reboots |
| `state` | `"active"` \| `"stale"` | Stale means not heard for `stale_after_ms` |
| `ids` | array of `{type, id}` | Basic IDs: `type` is an `IdType` (`"SerialNumber"`, `"CaaRegistration"`, `"UtmAssigned"`, `"SpecificSession"`, `"None"`, or `{"Reserved":n}`). `id` is text, or lowercase hex for binary IDs. Empty until a Basic ID arrives |
| `ua` | string \| null | `UaType`, e.g. `"HelicopterOrMultirotor"` |
| `first_ms`, `last_ms` | int | Hub uptime when the track was first and last heard |
| `age_ms` | int | Time since last heard |
| `rssi` | int \| null | RSSI of the most recently heard frame, in dBm |
| `links` | array | One entry per **slot** ever heard (a transport, and for Wi-Fi also the band; see "Bands" below): `{tr, n, rssi, ch, band, mac, ctr, age_ms, node}`. `tr` is the `Transport` (`"WifiBeacon"`, `"WifiNan"`, `"BleLegacy"`, `"BleExtended"`, `"BleLongRange"`), `n` the frame count, `ch` the channel, `band` `"2.4"` or `"5"` (GHz), `mac` the last address, `ctr` the last F3411 counter, `node` the last node that heard it. A Wi-Fi transport heard on both bands has two entries with the same `tr` |
| `loc` | object \| null | Last Location: `lat`, `lon` (deg), `alt_geo` (m, WGS-84 HAE), `alt_baro` (m), `height` (m) with `height_ref` (`"AboveTakeoff"` \| `"AboveGround"`), `speed` (m/s), `vspeed` (m/s, up positive), `dir` (deg true), `status` (`"Airborne"` etc.), `hacc_m`/`vacc_m` (declared accuracy, m), `age_ms` |
| `op` | object \| null | Operator position from System: `lat`, `lon`, `alt` (m HAE), `src` (`"Takeoff"` \| `"LiveGnss"` \| `"Fixed"`), `age_ms` |
| `self_id` | string \| null | Self-ID description |
| `op_id` | string \| null | Operator ID |
| `own` | object \| null | **`null` when the unit has no recent GPS fix**, which means "own position unknown". Otherwise it holds `dist_m` and `brg` (range in m and bearing in degrees true to the drone), `op_dist_m` and `op_brg` (the same to the operator), and `above_m` (drone height above you in m). Each is `null` if its inputs are missing. Values are rounded to 0.1 |
| `loc_src`, `op_src` | string \| null | Where `loc` / `op` came from: `"RemoteId"`, `"DroneId"` (OcuSync burst) or `"DjiWifi"` (legacy Wi-Fi element); null when absent. Added 2026-11-14 (protocol 4) |
| `home` | object \| null | DJI home point, `{lat, lon, age_ms}` (DroneID only). Added 2026-11-14 |
| `dji` | object \| null | What DJI's own broadcasts said; null when none was heard: `product` (DJI code \| null), `model` (name \| null), `ids_shown` (false with `hide_droneid_ids`), `key_id` (O4 session key ID, 8 hex digits; null when not O4 or hidden), `encrypted` (O4: presence only), `undecoded` (the shared track for bursts that did not decode), `ocusync` (`{n, rssi, snr, mhz, age_ms, node}` \| null), `wifi` (`{n, rssi, ch, band, mac, age_ms, node}` \| null). Added 2026-11-14 |

Example:

```json
{"t":"track","id":1,"state":"active","ids":[{"type":"SerialNumber","id":"1581F5FKD229400D"}],"ua":"HelicopterOrMultirotor","first_ms":4000,"last_ms":4500,"age_ms":500,"rssi":-83,"links":[{"tr":"WifiBeacon","n":1,"rssi":-61,"ch":6,"band":"2.4","mac":"60:60:1f:00:00:01","ctr":7,"age_ms":1000,"node":0},{"tr":"BleLongRange","n":1,"rssi":-83,"ch":37,"band":"2.4","mac":"c0:11:22:33:44:55","ctr":7,"age_ms":500,"node":1}],"loc":{"lat":39.948889,"lon":-75.15,"alt_geo":140.0,"alt_baro":130.0,"height":50.0,"height_ref":"AboveTakeoff","speed":5.25,"vspeed":0.5,"dir":90.0,"status":"Airborne","hacc_m":3.0,"vacc_m":10.0,"age_ms":1000},"op":{"lat":39.952583,"lon":-75.165222,"alt":15.0,"src":"Takeoff","age_ms":1000},"self_id":"Bench <test> & co","op_id":"FIN87astrdge12k8","own":{"dist_m":1363.9,"brg":107.5,"op_dist_m":0.0,"op_brg":0.0,"above_m":100.0}}
```

A typical line is about 900 bytes (about 1.1 KB with a `dji` object). The worst case is 2216 bytes, with all seven slots, two IDs, escaped text and both DJI sources. Size buffers with `json::MAX_LINE` (2304; 1792 before protocol 4). The matching sizes for the other formats are `cot::MAX_EVENT` (2048) and `csv::MAX_ROW` (512).

## Bands (added 2026-11-09)

A track keeps one set of link statistics per **slot**, not per transport. The
slots are `tracker::SLOTS`: Wi-Fi beacon and NAN on 2.4 GHz, the same two on
5 GHz, and the three Bluetooth transports (always 2.4 GHz). The band comes from
the channel the node reported: 1-14 is 2.4 GHz, anything above 14 is 5 GHz,
and 0 ("unknown") counts as 2.4 GHz.

Before this, one Wi-Fi slot per transport meant a drone heard by both the
2.4 GHz node and the 5 GHz node had its frames overwrite each other's RSSI and
channel, and only one band ever showed.

Schema impact, all backward-compatible (`"v"` stays 1):

| Output | Change |
|---|---|
| JSON `links[]` | New `band` field on every entry (`"2.4"` or `"5"`). A Wi-Fi transport heard on both bands now has **two** entries with the same `tr`; a reader that ignores `band` still gets valid entries |
| CSV `transports` | One name per slot; the 5 GHz slots are `WifiBeacon5G` and `WifiNan5G`. The header is unchanged |
| CoT remarks | `Transport: WifiBeacon+WifiBeacon5G`, same names as CSV |
| Rust API | `Track::links` is `[LinkStats; 7]` indexed by `Slot::index`. New: `Slot`, `Band`, `SLOTS`, `Track::slot`, `Track::slots`, `Track::latest_slot`. `Track::link(transport)` now returns a `LinkStats` **by value**, combining both bands (counts added, other fields from the newest band). `Track::transports()` and `latest_link()` keep their meaning |

MAC matching ignores the band: a Wi-Fi MAC is one namespace whichever band it
was heard on.

## Open FPV links (added 2026-11-09)

Wi-Fi nodes also recognise open-source digital FPV / telemetry links that
inject ordinary 802.11 frames: wfb-ng (and OpenIPC), OpenHD, RubyFPV and
DroneBridge for Linux (`crates/openlink` has the byte rules and the
false-positive reasoning). A node sends a `link::OpenLinkSighting` per stream
at most about once a second; `Tracker::ingest_open_link` keeps them in a
separate table of `openlinks::MAX_LINK_TRACKS` (8) entries with their own ids
(`LinkTrackId`). They never create, merge with or alert on a drone track.

- **Key.** System plus the cleartext identifier: wfb-ng's link ID, RubyFPV's
  vehicle ID, DroneBridge's comm ID. A link's streams (wfb-ng's video down,
  MAVLink up and down, tunnel) are one entry, each stream kept separately
  (`MAX_STREAMS` = 6, per port, direction and node). OpenHD sends no ID, so
  all OpenHD in range is one entry; so is any traffic of a system without an ID.
- **Expiry.** As drones: stale after `stale_after_ms`, removed after
  `remove_after_ms`. A full table drops the link heard least recently.
- **Privacy (project policy).** `Config::show_link_ids` is **false by
  default**. Then every output (JSON, CSV, the laptop table) leaves out the
  identifier *and* the transmitter MAC (for wfb-ng the MAC embeds the link
  ID). The system, ports, directions, channel, RSSI and counts are always
  shown. `openlinks::shown_id` is the one place the rule is applied. The hub
  sets it from its `show-link-ids` build feature; `rid-view` from
  `--show-link-ids`. Video and MAVLink content is never decoded anywhere.
- **No position, no CoT.** A sighting has no position, so there is nothing
  for TAK to plot; `cot` ignores open links.

### Link line schema (`json::link_line`)

| Field | Type | Meaning |
|---|---|---|
| `t` | `"link"` | Line type tag |
| `id` | int | Link handle (separate numbering from drone tracks) |
| `state` | `"active"` \| `"stale"` | As tracks |
| `sys` | string | `"WfbNg"`, `"OpenHd"`, `"RubyFpv"`, `"DroneBridge"` |
| `label` | string | For people: `"wfb-ng/OpenIPC"`, `"OpenHD"`, `"RubyFPV"`, `"DroneBridge"` |
| `ids_shown` | bool | The unit's `show_link_ids` setting |
| `link_id` | string \| null | Only when `ids_shown`: wfb-ng's link ID as six hex digits (`"7505d6"` is the default `link_domain`), else decimal. Null when hidden or not sent |
| `first_ms`, `last_ms`, `age_ms` | int | As tracks |
| `rssi`, `ch`, `band` | | Of the stream heard most recently |
| `down`, `up` | bool | Heard air-to-ground / ground-to-air |
| `pkts` | int | Sum of the streams' running frame counts |
| `streams` | array | `{port, kind, dir, ch, band, rssi, n, mac, age_ms, node}`: `kind` is wfb-ng's `"video"` / `"mavlink"` / `"tunnel"` (null otherwise), `dir` `"AirToGround"` / `"GroundToAir"` / `"Unknown"`, `n` the node's running count for that stream, `mac` null unless `ids_shown` |

A worst-case line is about 1.2 KB; `json::MAX_LINK_LINE` (1344) always holds one.

### CSV

Open links share the SD log's columns: `track` is `L<n>`, `transports` is the
system (`WfbNg`, `WfbNg5G` for 5 GHz, `|`-separated if heard on both bands),
`rssi_dbm` the latest, `uas_id` / `id_type` the identifier and its name
(`link_id`, `vehicle_id`, `comm_id`) only with `show_link_ids`; every
position column is empty.

## ADS-B aircraft (added 2026-11-09, PLAN.md Phase 6)

Crewed aircraft from an ADSBee 1090 (its CSBee text, parsed by `crates/adsb`)
live in a third table, module `aircraft`, sized by the tracker's second const
generic: `Tracker<N, A>`, default `A = aircraft::MAX_AIRCRAFT` (32, about 120
bytes each). A hub built without an ADSBee uses `A = 0` and pays nothing.

- **Never a drone, never an alert.** `ingest_aircraft` never touches the drone
  table or `take_new_drone`. The only extra notion is "low aircraft nearby"
  (`AircraftGeometry::low_nearby`: airborne, within 5 km and at most 610 m /
  2000 ft above the unit), shown quietly as text.
- **Key:** the 24-bit ICAO address plus the receiver (1090 MHz or UAT 978).
- **Freshness:** the ADSBee lists every aircraft in its table once a second,
  even ones it stopped hearing. A report counts as hearing the aircraft only
  when its frame counts or "updated" flags say so, and the position is only
  re-stamped when the ADSBee says it was updated, so `pos_age_ms` is real. A
  report of an aircraft not currently heard never creates an entry.
- **Expiry:** stale after 15 s, removed after 60 s (`AIRCRAFT_STALE_MS`,
  `AIRCRAFT_REMOVE_MS`): aircraft move fast.
- **Full table:** the least useful entry makes room (stale, then no
  position, then farthest); a newcomer that would itself be the least useful
  is dropped.
- **Range and bearing:** the same Vincenty code as drones. `above_m` is the
  aircraft's geometric altitude (or, without one, pressure altitude, which is
  close to height above sea level) minus the own fix's height.
- **Same drone twice:** an aircraft whose emitter category is UAV, within 300 m
  of an active Remote ID track, is taken to be that drone
  (`aircraft::same_drone`): its JSON `drone` field names the track and its CoT
  event is not sent (`FormatError::Duplicate`).

### Aircraft line schema (`json::aircraft_line`)

| Field | Type | Meaning |
|---|---|---|
| `t` | `"aircraft"` | |
| `icao` | string | Six upper-case hex digits |
| `src` | `"1090"` \| `"978"` | Receiver (978 = UAT, ADSBee 1090U) |
| `aq` | int | UAT address qualifier (2, 3 = relayed by TIS-B); 0 for 1090 |
| `state` | `"active"` \| `"stale"` | By the aircraft thresholds above |
| `call` | string \| null | Callsign (flight ID) |
| `squawk` | string \| null | Four octal digits, e.g. `"1200"` |
| `emerg` | string \| null | `"hijack"`, `"radio failure"` or `"emergency"` for squawk 7500/7600/7700 |
| `cat`, `kind`, `label` | int, string, string | Emitter category number, its name (`"Rotorcraft"`, `"Light"`, ...), and an English label (`"Helicopter"`) |
| `mil` | bool | A military extended squitter was heard (1090 only) |
| `air` | bool | Airborne |
| `lat`, `lon`, `pos_age_ms` | number \| null | Last position and how old it is |
| `alt_baro_ft`, `alt_geo_ft` | int \| null | Pressure altitude and geometric (WGS-84) altitude, feet |
| `spd_kt`, `trk`, `vr_fpm` | number \| null | Speed (kt), direction (deg), vertical rate (ft/min, up positive) |
| `rssi` | int | dBm of the last message |
| `first_ms`, `last_ms`, `age_ms` | int | As for tracks (`last_ms` = last report in which it was heard) |
| `drone` | int \| null | The drone track id this ADS-B "UAV" probably is |
| `own` | object \| null | `dist_m`, `brg`, `above_m`, `low` (bool); null without an own position |

### CSV

Aircraft rows share the columns too: `track` is `ICAO-<hex>`, `uas_id` the
callsign with `id_type` `Callsign`, `ua_type` the category name,
`transports` `Adsb1090` or `Uat978`, `drone_lat`/`drone_lon` the position,
`drone_alt_m` the geometric altitude, `speed_mps`, `rssi_dbm`, `dist_m`,
`brg_deg`, and the last column `baro_alt_m` the pressure altitude.

### Cursor-on-Target

uid `ICAO-<hex>` (the ADS-B-to-TAK convention), contact callsign the flight ID
(or the uid), `how="m-g"`, `stale` = position time + 30 s. Type by emitter
category: rotorcraft `a-n-A-C-H`, lighter-than-air `a-n-A-C-L`, fixed wing and
unknown `a-n-A-C-F` (neutral civil air: a detector has no basis for another
affiliation), UAV `a-u-A-M-F-Q` (as Remote ID drones), ground vehicles
`a-n-G-E-V`, obstacles `b-m-p-s-m`. `hae` is the geometric altitude, or the
pressure altitude with `altsrc="baro"`.

## DJI DroneID (added 2026-11-14, protocol 4)

`ingest_droneid` (OcuSync burst, `link::DroneIdSighting`) and
`ingest_dji_wifi` (legacy Wi-Fi element, decoded with `rid::dji`) put DJI's
own broadcasts on ordinary drone tracks; `ingest_frame` routes any
`link::Frame` to the right one.

- **Scaling** follows `crates/droneid` (`payload::FlightInfo`): radians x 1e7
  to degrees, altitude and height in feet, velocities in cm/s. The Wi-Fi
  element's extension is "believed" metres (see `rid::dji`).
- **Merge:** a plaintext serial is stored as a `SerialNumber` ID. DJI's Remote
  ID serial is the same, so a DJI drone heard on both is one track,
  whichever arrives first (and a MAC-keyed Remote ID track joins it when its
  Basic ID arrives).
- **Position:** Remote ID wins while it is younger than 3 s
  (`dji::RID_PREFERRED_MS`); `loc_src` says which is shown. DJI altitude is
  barometric, so it goes into `alt_baro`, never `alt_geo` (so no "above you").
- **O4** (encrypted): a presence-only track per session key ID. **Undecoded
  bursts**: all join one presence track. Neither has a position, so neither
  makes CoT; both make CSV rows (`transports` `DroneIdO4` / `DroneId`).
- **Privacy:** shown by default (owner decision). `Config::hide_droneid_ids`
  keeps the decoded serial, the O4 key ID and the flight-purpose text out of
  every output; the serial still matches for merging, and a serial the drone
  also sends in Remote ID is shown.

## RF emitters and Wi-Fi drones (added 2026-11-14, protocol 4)

Two more position-less tables, module `signals`, aged like open links.

- `ingest_rf(node, &RfSighting, now)`: one `Emitter` per kind and frequency
  (sightings within 2 MHz, or half the occupied width, are one). Latest and
  peak level above the noise floor, duty, line rate (last non-zero), width.
- `ingest_wifi_drone(node, &WifiDroneSighting, now)`: one `WifiDrone` per
  address, maker name from `crates/wifidrone` (index on the wire). The
  address is shown only with `show_link_ids`; the network name is shown.
- `take_new_signal(now)`: once per entry, for analog video, HDZero,
  LTE-style, narrow Wi-Fi and every Wi-Fi drone. **Never** for plain
  `Energy` or an unconfirmed `FpvCarrier`.
- `ingest_status` / `ingest_hello`, `nodes()`: per node role, `can` mask and
  applied plan id, for the health view.

### Lines

`json::rf_line` (`"t":"rf"`): `id`, `state`, `kind` (`link::RfKind` name),
`label`, `alerts`, `mhz`, `width_khz` (null if unknown), `snr`, `peak_snr`,
`duty`, `line_hz` (null if none), `n`, `node`, `first_ms`, `last_ms`,
`age_ms`. `json::MAX_RF_LINE` (384).

`json::wifi_drone_line` (`"t":"wifidrone"`): `id`, `state`, `maker`,
`maker_id`, `ssid`, `ids_shown`, `mac` (null unless shown), `ch`, `band`,
`rssi`, `n`, `node`, `first_ms`, `last_ms`, `age_ms`.
`json::MAX_WIFI_DRONE_LINE` (448).

CSV: `csv::rf_row` (track `R<n>`, `uas_id` the MHz with `id_type`
`FreqMHz`, `ua_type` the kind, `transports` `Rf`, `rssi_dbm` = **level above
the noise floor**) and `csv::wifi_drone_row` (track `W<n>`, `uas_id` the
network name with `id_type` `Ssid`, `ua_type` the maker, `transports`
`WifiDrone` or `WifiDrone5G`; no address). Neither makes CoT.

## Phone page API (`json::state`, `json::parse_own_pos`)

The hub's hotspot serves the page in `web/index.html` (bundled by
`crates/webui`) and three endpoints. `rid-view --http ADDR` serves the same
on a laptop.

| Request | Answer |
|---|---|
| `GET /` | The page: `webui::INDEX_HTML_GZ` with `Content-Encoding: gzip`, `Content-Type: webui::CONTENT_TYPE`, `ETag: webui::ETAG` (answer `304` when `webui::etag_matches(If-None-Match)`), `Cache-Control: no-cache` |
| `GET /api/state` | `200`, `application/json`, `Cache-Control: no-store`, body from `json::state` |
| `POST /api/own-pos` | Body `{"lat":..,"lon":..,"alt":..}` (`alt` optional or `null`, metres above the WGS-84 ellipsoid) or `{"clear":true}`. Check it with `json::parse_own_pos`: `200 {"ok":true}`, or `400 {"error":"<OwnPosError::message()>"}` |

The page polls `/api/state` about once a second, and says "connection lost"
once no answer has arrived for 3.5 s.

### Hub side

```rust
let nodes: [json::NodeStatus; 3] = /* one per attached node */;
let hub = json::HubStatus {
    own_source: json::OwnSource::Gps,   // what ctx.own came from
    manual: saved_manual,               // Option<json::ManualPos>, whether or not in use
    gps: json::GpsStatus { state: json::GpsState::Fix3d, sats: Some(9) },
    recording: sd_log_open,
    nodes: &nodes,
    adsb: None,                         // Some(json::AdsbStatus) with an ADSBee
};
let ctx = tracker.context(now_ms, utc_ms);
let n = json::state(tracker.tracks(), tracker.open_links(), tracker.aircraft(), &ctx, &hub, &mut buf)?;
// buf: json::state_max_len(tracker.len(), tracker.open_links().len(), tracker.aircraft().len(), nodes.len())
```

`pub fn state(tracks: &[Track], links: &[LinkTrack], aircraft: &[AircraftTrack], ctx: &Context, hub: &HubStatus, buf: &mut [u8]) -> Result<usize, FormatError>`
writes one object and `\n`. A buffer of `state_max_len(tracks, links, aircraft, nodes)`
always fits (64 tracks, 8 links, 32 aircraft and 4 nodes: about 147 KB, so
PSRAM); a typical object is about 1 KB per track. Too small a buffer is
`BufferTooSmall`, never a truncated object. The open links come before the
tracks, separated by exactly `json::STATE_LINKS_TO_TRACKS`; the aircraft come
after them, separated by `json::STATE_TRACKS_TO_AIRCRAFT`; the object ends
with `json::STATE_END`, so a server can stream it piece by piece.

**Which position is used.** The page assumes this policy, and the hub
implements it: a GPS fix wins; a typed (manual) position is used only while
the GPS has no fix. The tracker forgets a fix after `own_fix_max_age_ms`, so
the hub re-applies the manual position every tick with
`set_own_position(lat, lon, alt, now_ms)` and reports `OwnSource::Manual`.
Keep the manual position across reboots if you can; the page shows it as
`manual` either way.

### State object schema (`"v":1`)

| Field | Type | Meaning |
|---|---|---|
| `t` | `"state"` | Object type tag |
| `v` | int | Schema version (`json::STATE_VERSION`), bumped on incompatible change |
| `now_ms` | int | Hub uptime. The page treats a decrease as a hub restart |
| `utc_ms` | int \| null | Unix ms from GPS time, `null` without it |
| `own` | object \| null | The position in use: `lat`, `lon`, `alt` (m HAE \| null), `source` (`"gps"` \| `"manual"`), `age_ms`. **`null` means own position unknown** (P4): then every track's `own` is null too |
| `manual` | object \| null | The typed position on file, `{lat, lon, alt}`, even when GPS is in use |
| `gps` | object | The GPS receiver: `state` (`"absent"` \| `"searching"` \| `"2d"` \| `"3d"`) and `sats` (int \| null) |
| `rec` | bool | The SD-card log is being written |
| `nodes` | array | One per radio node: `role` (`"Wifi24"` \| `"Wifi5"` \| `"Ble"` \| `"Fpv"` \| `"DroneId"` \| `"Gateway"`), `can` (mask of `NodeRole::bit` from its Status frame, null before one; added 2026-11-14), `plan` (the hub plan it applied, 0 = built-in, null before a Status; added 2026-11-14), `alive` (heard recently), `age_ms` (since its last frame; null if never), `seen_per_s` (all frames it saw per second; null before its first Stats), `rid_per_s` (Remote ID detections the hub got per second), `drops` (total frames it dropped; null before its first Stats) |
| `adsb` | object \| null | The ADS-B receiver (`json::AdsbStatus`): `alive`, `age_ms`, `frames_per_s`, `aircraft` (in the ADSBee's own table), `errors` (lines refused). `null` when the hub has none built in. Added 2026-11-09 |
| `open_links` | array | Every open FPV link, each exactly the `json::link_line` object above (in table order). Added 2026-11-09; `"v"` stays 1 (a new field) |
| `tracks` | array | Every track, each exactly the `json::track_line` object above (in table order; the page sorts) |
| `aircraft` | array | Every ADS-B aircraft, each exactly the `json::aircraft_line` object above (in table order). Added 2026-11-09; `"v"` stays 1 |
| `rf` | array | **`json::state_full` only:** every RF emitter, each the `json::rf_line` object. Added 2026-11-14; readers treat a missing `rf` as empty |
| `wifi_drones` | array | **`json::state_full` only:** every Wi-Fi drone, each the `json::wifi_drone_line` object. Added 2026-11-14 |

`json::state` is unchanged (it still ends `],"aircraft":[...]}`), so the S3
hub's piece-by-piece streaming of it keeps working. `json::state_full` writes
the same object, then `json::STATE_AIRCRAFT_TO_RF`, the emitters,
`json::STATE_RF_TO_WIFI`, the Wi-Fi drones and `json::STATE_END`; size its
buffer with `json::state_full_max_len`. rid-view serves `state_full`.

Example (tracks shortened):

```json
{"t":"state","v":1,"now_ms":5000,"utc_ms":1794232987250,"own":{"lat":39.952583,"lon":-75.165222,"alt":40.0,"source":"gps","age_ms":100},"manual":null,"gps":{"state":"3d","sats":9},"rec":true,"nodes":[{"role":"Wifi24","alive":true,"age_ms":300,"seen_per_s":412,"rid_per_s":1,"drops":0},{"role":"Ble","alive":false,"age_ms":null,"seen_per_s":null,"rid_per_s":0,"drops":null}],"adsb":null,"open_links":[],"tracks":[{"t":"track","id":1,...}],"aircraft":[]}
```

**New-drone alerts on the page** follow the hub's buzzer rule without extra
fields: a track id the page has not seen before alerts once it has an ID, or
1 s after `first_ms`. Tracks present when the page loads do not alert.

**Security.** Self-ID, operator ID and UAS ID text come from the air and can
contain anything; the page HTML-escapes every one. Anyone on the hotspot can
`POST /api/own-pos`, which only moves the distances shown.

## Cursor-on-Target

The events follow DragonSync's conventions (Apache-2.0, WarDragon's TAK
bridge), so ATAK users see what they already know:

- **Drone:** uid and callsign `drone-<UAS ID>`, `how="m-g"`, yellow `<color argb="-256"/>`, plus `<track course speed>` and a `Key: value;` `<remarks>`.
- **Operator:** `pilot-<UAS ID>`, type `b-m-p-s-m`, with the civilian Person icon.

Before a Basic ID arrives, `<UAS ID>` is `mac-<hex>`.

| UA type | CoT type |
|---|---|
| Multirotor, helicopter, gyroplane, hybrid lift, none/unknown | `a-u-A-M-H-R` (as DragonSync) |
| Aeroplane, ornithopter, glider | `a-u-A-M-F-Q`. DragonSync uses `a-f-A-f`, which marks the aircraft friendly, so we do not copy it |
| Balloons, kite, airship, parachute, rocket, tethered, ground obstacle, other | `b-m-p-s-m` (as DragonSync) |

`time` is when the position was received. `stale` is `time` plus 2 × `stale_after_ms`. `ce` and `le` are the drone's declared accuracy, or 9999999.0 when unknown.

## CSV (`csv::HEADER`, `csv::row`)

```
utc,hub_ms,track,own_lat,own_lon,own_alt_m,uas_id,id_type,ua_type,transports,drone_lat,drone_lon,drone_alt_m,height_m,speed_mps,op_lat,op_lon,rssi_dbm,dist_m,brg_deg,op_dist_m,op_brg_deg,baro_alt_m
```

`baro_alt_m` (pressure altitude) was added 2026-11-09 with ADS-B; for drones
it is the Remote ID pressure altitude. Read columns by name.

An empty field means unknown. `transports` is `|`-separated. A field is quoted
(RFC 4180) only when it needs to be.

## Tests

`cargo test -p tracker` covers:

- merging across transports and the MAC→ID merge
- the Wi-Fi band split: both bands kept, MACs matched across bands, JSON/CSV/CoT labels
- BLE MAC rotation and conflicting IDs
- expiry and eviction at capacity
- the once-only alert
- geometry, checked against GeographicLib and Vincenty's published example
- golden output for all three formatters
- the `/api/state` object (golden, no fix, many tracks, every too-small buffer) and `own-pos` parsing
- open FPV links (`tests_openlinks.rs`): kept apart from drones, merged per link, bounded, expired,
  golden JSON and CSV, and no identifier in any output unless `show_link_ids`
- protocol 4 (`tests_signals.rs`): DroneID scaling against `crates/droneid`, DroneID + Remote ID
  serial merge in both orders, Remote ID position preferred while fresh, O4 and undecoded presence
  tracks, `hide_droneid_ids`, DJI Wi-Fi elements built with `rid::encode::dji_flight_info`, RF
  emitter keying/alerts/ageing, Wi-Fi drones with hidden addresses, node Status, `ingest_frame`,
  `state_full`, and worst-case sizes

`cargo build -p tracker --target riscv32imac-unknown-none-elf` checks the `no_std` build.
