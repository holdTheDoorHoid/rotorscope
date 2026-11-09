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
| `links` | array | One entry per transport ever heard: `{tr, n, rssi, ch, mac, ctr, age_ms, node}`. `tr` is the `Transport` (`"WifiBeacon"`, `"WifiNan"`, `"BleLegacy"`, `"BleExtended"`, `"BleLongRange"`), `n` the frame count, `ch` the channel, `mac` the last address, `ctr` the last F3411 counter, `node` the last node that heard it |
| `loc` | object \| null | Last Location: `lat`, `lon` (deg), `alt_geo` (m, WGS-84 HAE), `alt_baro` (m), `height` (m) with `height_ref` (`"AboveTakeoff"` \| `"AboveGround"`), `speed` (m/s), `vspeed` (m/s, up positive), `dir` (deg true), `status` (`"Airborne"` etc.), `hacc_m`/`vacc_m` (declared accuracy, m), `age_ms` |
| `op` | object \| null | Operator position from System: `lat`, `lon`, `alt` (m HAE), `src` (`"Takeoff"` \| `"LiveGnss"` \| `"Fixed"`), `age_ms` |
| `self_id` | string \| null | Self-ID description |
| `op_id` | string \| null | Operator ID |
| `own` | object \| null | **`null` when the unit has no recent GPS fix**, which means "own position unknown". Otherwise it holds `dist_m` and `brg` (range in m and bearing in degrees true to the drone), `op_dist_m` and `op_brg` (the same to the operator), and `above_m` (drone height above you in m). Each is `null` if its inputs are missing. Values are rounded to 0.1 |

Example:

```json
{"t":"track","id":1,"state":"active","ids":[{"type":"SerialNumber","id":"1581F5FKD229400D"}],"ua":"HelicopterOrMultirotor","first_ms":4000,"last_ms":4500,"age_ms":500,"rssi":-83,"links":[{"tr":"WifiBeacon","n":1,"rssi":-61,"ch":6,"mac":"60:60:1f:00:00:01","ctr":7,"age_ms":1000,"node":0},{"tr":"BleLongRange","n":1,"rssi":-83,"ch":37,"mac":"c0:11:22:33:44:55","ctr":7,"age_ms":500,"node":1}],"loc":{"lat":39.948889,"lon":-75.15,"alt_geo":140.0,"alt_baro":130.0,"height":50.0,"height_ref":"AboveTakeoff","speed":5.25,"vspeed":0.5,"dir":90.0,"status":"Airborne","hacc_m":3.0,"vacc_m":10.0,"age_ms":1000},"op":{"lat":39.952583,"lon":-75.165222,"alt":15.0,"src":"Takeoff","age_ms":1000},"self_id":"Bench <test> & co","op_id":"FIN87astrdge12k8","own":{"dist_m":1363.9,"brg":107.5,"op_dist_m":0.0,"op_brg":0.0,"above_m":100.0}}
```

A typical line is about 900 bytes. The worst case is 1404 bytes, with all five transports, two IDs and escaped text. Size buffers with `json::MAX_LINE` (1536). The matching sizes for the other formats are `cot::MAX_EVENT` (2048) and `csv::MAX_ROW` (512).

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
utc,hub_ms,track,own_lat,own_lon,own_alt_m,uas_id,id_type,ua_type,transports,drone_lat,drone_lon,drone_alt_m,height_m,speed_mps,op_lat,op_lon,rssi_dbm,dist_m,brg_deg,op_dist_m,op_brg_deg
```

An empty field means unknown. `transports` is `|`-separated. A field is quoted
(RFC 4180) only when it needs to be.

## Tests

`cargo test -p tracker` covers:

- merging across transports and the MAC→ID merge
- BLE MAC rotation and conflicting IDs
- expiry and eviction at capacity
- the once-only alert
- geometry, checked against GeographicLib and Vincenty's published example
- golden output for all three formatters

`cargo build -p tracker --target riscv32imac-unknown-none-elf` checks the `no_std` build.
