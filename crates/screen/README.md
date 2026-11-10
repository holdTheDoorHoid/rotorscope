# screen

The on-device screen (PLAN.md O2, O6, P2, P4): a 320×240 landscape RGB565 panel
(ST7789 2.0" or ILI9341 2.8" class) and three buttons. `no_std`, no allocator.
It draws with `embedded-graphics` 0.8, so the hub can drive any panel through `mipidsi`.

Every screen is rendered to PNG on the laptop by `cargo test -p screen`. The images
are in [`snapshots/`](snapshots/). Look at them before reading the rest.

## API

```rust
use screen::{render, Ui, UiState, ButtonDecoder, from_tracker::fill_from_tracker};

let mut ui = Ui::default();            // or Ui::new(settings_loaded_from_flash)
let mut keys = ButtonDecoder::new();
let mut state = UiState::default();    // ~8 KB; keep it in a static

// every frame (10-20 Hz is plenty):
fill_from_tracker(&tracker, &tracker.context(now, utc), &mut state); // feature "tracker"
state.gps = Gps { fix, sats };   state.nodes = ...;   state.power = ...;   state.sd = ...;
state.hub_uptime_s = ...;
while let Some(id) = tracker.take_new_drone(now) { ui.new_drone(id.0, now); }
if ui.take_buzz().is_some() { buzzer.chirp(); }
if let Some(b) = keys.poll(up, down, select, now) { ui.button(b, &state); }
ui.tick(now);
render(&ui, &state, &mut framebuffer)?;   // any DrawTarget<Color = Rgb565>
panel.flush(&framebuffer);
apply_backlight(ui.settings().brightness);   // 1..=5
```

| Item | What it is |
|---|---|
| `UiState` | Plain view-model: `now_ms`, `utc_ms`, `gps`, `nodes` (≤5), `power`, `sd`, `drones` (≤32 `DroneView`), `total_drones`, `hub_uptime_s`. The screen sorts the drones itself. |
| `DroneView` | `key` (TrackId), `id` + `id_kind`, `ua`, `stale`, first/last seen, `drone` and `operator` (`Polar`: distance + bearing from you), `drone_to_operator_m`, height/alt/above-you, speed/vspeed/track, `radios` (W N B L 5), `rssi` per radio, `self_id`, `operator_id`. |
| `Ui` | State machine: page, mode, cursor (by drone key, so it follows a drone while the list re-sorts), alert banner, settings, buzzer request. `button(Button, &UiState)`, `tick(now_ms)`, `new_drone(key, now_ms)`, `take_buzz()`. |
| `Button` | `Up`, `Down`, `Select` (short press), `Back` (long press of Select). |
| `ButtonDecoder` | Raw debounced levels → `Button`. Select fires on release; Back fires once Select has been held for 700 ms. Up/Down auto-repeat after 500 ms, every 200 ms. |
| `render(&Ui, &UiState, &mut D)` | Clears and draws one whole frame. It takes the `Ui` as well as the `UiState`, because the page and cursor belong to the screen, not the hub. |
| `band::Band`, `render_banded(&Ui, &UiState, &mut Band, flush)` | Draw the frame 24 rows at a time into a 15 KB buffer (`BAND_ROWS`) and hand each band to `flush(top, rows, pixels)`. For hubs without PSRAM; the test `banded_render_matches_full_frame` checks it gives exactly the full-frame pixels on every page. |
| `Fix` | `None` (searching), `Fix2D`, `Fix3D`, `Manual` (a typed position is in use, "POS MAN"), `Absent` (no GPS receiver, "NO GPS"). |
| `SdLog` | `Off`, `Recording`, `NoCard` (quiet "no SD"), `Error` ("SD!"). |
| `from_tracker::fill_from_tracker` | Feature `tracker`. Fills `drones`, `total_drones`, `now_ms` and `utc_ms`. If the tracker considers the own position too old, it sets a 2D/3D/manual `gps.fix` to `None`, so the layout and the numbers agree. When there are more than 32 tracks, it picks active before stale, then nearest, then newest. |

The renderer does not depend on the tracker. Only the optional adapter does.

## Buttons and pages

```
          Up/Down                 Select              Select
 RADAR ⇄ LIST ⇄ HEALTH ⇄ SETTINGS ─────► choose a drone ─────► DETAIL
   ▲                                        (cursor)          Up/Down: next drone
   └──── hold Select (Back) ◄──────────────────┴──────◄──────── hold Select
```

- **Browse** (the default): Up/Down flip between pages, wrapping round. A page-name toast
  shows for 1.5 s after each flip. Back from any page goes home to the radar.
- **Choose:** Select on the Radar or List puts a cursor on the nearest drone (on the newest one
  without a fix). Up/Down move it, Select opens the detail, Back leaves.
- **Detail:** Up/Down step through drones in list order. Back returns to choosing, with the
  cursor on that drone.
- **Settings:** Select to edit. Up/Down pick a row, Select changes it, Back finishes. The
  settings are theme (Night/Day), brightness 1-5, and buzzer on/off. The hub applies them
  and may save them to flash.
- **Alert banner:** the first press goes to the banner. Select opens that drone's detail. Up,
  Down or Back only dismiss it, so a press meant for the page underneath never does
  something unexpected.
- **Timeouts:** choosing and settings return to browsing after 30 s with no press. Detail does
  not time out (see the tensions below).

## Screens

| Snapshot | What it shows |
|---|---|
| `radar_0_drones` | Empty radar, "Listening for drones", count 0 (grey). |
| `radar_1_drone` | One drone: big distance, unit word, a compass rose with an arrow, the compass point and degrees, height, ID. The pilot is a blue X joined to the drone. |
| `radar_5_drones` | Five active drones and one stale one, one beyond 3 km, and one with no position ("+1 without position"). |
| `radar_5_drones_day` | The same in the Day theme. |
| `radar_pick` | Choosing a drone: the readout says **DRONE 3** in an inverse box instead of NEAREST. |
| `list_5_drones`, `list_pick` | The list sorted by distance, with a stale divider and the selected row inverted. |
| `no_fix`, `list_no_fix` | P4: list-first, a "NO POSITION - distances unavailable" bar, and signal bars instead of distance. |
| `alert`, `alert_more` | The new-drone banner, with "+1 more new". |
| `detail`, `detail_no_fix` | Full ID, drone and pilot direction, height, speed, operator ID, Self-ID, signal per radio, and when last heard. With no fix, the pilot row shows the drone-to-pilot distance, which still works. |
| `health` | Per node: frames/s, RID frames found, drops, uptime. A dead node shows NOT RESPONDING. Below that: GPS, tracks, SD log and power. |
| `health_aircraft` | A hub with an ADSBee: the ADS node in the list and status bar (four nodes pack closer), and an AIRCRAFT line: active count, how many are LOW (accent colour), the nearest by callsign with distance and altitude. Never an alert. |
| `settings` | The settings page, mid-edit. |
| `status_warnings` | Status bar under stress: a 2D fix, every node dead, an SD error, 9% battery and no UTC. |
| `status_gps_absent` | No GPS receiver ("NO GPS"), no SD card ("no SD"), on USB: the bench hub today. |
| `status_manual_pos` | A typed position in use ("POS MAN"): distances work without a GPS fix. |
| `page_toast` | The page name after flipping. |

**Status bar (every screen):**

- GPS: "GPS 11", or an inverse **NO FIX** box. A 2D fix shows a small "2D".
- One dot per node with its label (2.4 / 5 / BT): filled when alive, hollow and slashed when dead.
- **REC** with a dot when the SD log is on. An inverse **SD!** box when it is failing.
- Battery % with an icon, or **USB**.
- UTC as HH:MM, labelled "UTC".

## Design decisions

- **Answer in under a second.** The radar page has one big number, the distance to the nearest
  active drone. It is drawn in 44 px block digits (about 5.6 mm tall on a 2.0" panel, 7.8 mm on
  a 2.8"), with the unit as a word below it. Next comes the direction twice: as an arrow and as
  "NE 047°". The drone count sits in the same corner on every radar frame.
- **Big digits are drawn, not fonts.** They are seven-segment-style blocks made of rectangles,
  with a flagged and footed "1" so a lone 1 is not mistaken for a divider. They are crisp at any
  size, need no font data, and carry no licence question. All the other text uses
  embedded-graphics' bundled X11 misc-fixed fonts (public domain, Latin-1 for "°"). No GPL
  code and no extra font crate.
- **Never colour alone.** Active drones are filled diamonds and stale ones are hollow and
  grey. The nearest or selected drone is larger and ringed. The pilot is an X. A dead node is
  hollow and slashed. Warnings are inverse boxes with words. Each radio is a letter (W N B L 5)
  in a fixed column, with a dot where it was not heard. Signal is bars of rising height. The
  palette could be swapped for monochrome or red-only without losing anything.
- **Contrast.** In the Night theme, text is white on black (21:1), secondary text is grey
  #AAA (about 9:1), stale is #787878 (about 4.6:1), and drones are amber (#FFB800).
  Amber stays bright and distinct for all three common colour-vision deficiencies. Range rings
  are deliberately faint so they do not compete with drones.
- **Night by default, Day for sun.** A dark screen keeps night vision. In direct sun, dark
  text on a light background usually reads better on a transmissive TFT, so a Day theme is one
  setting away. Brightness is a setting (1-5) that the hub maps to backlight PWM.
- **Log-ish range scale with fixed rings** at 100 m, 300 m, 1 km and 3 km. It does not
  auto-scale, so a position on the plot always means the same thing and people learn it.
  Nearby drones get room, and far ones still fit. Anything past 3 km is a hollow or filled
  pointer on the edge.
- **Rank numbers link the views.** The radar labels markers 1-9 in list order, so "drone 2" is
  the same drone on the radar, the list and the detail.
- **Rounded numbers.** Distances are rounded to 1 m below 100 m, 10 m below 1 km, then 0.1 km.
  This is about what GPS on both ends can support, and it stops the last digit flickering.
- **Shortened IDs keep the tail.** A serial number's first four characters are the
  manufacturer code (1581 is DJI), so lists show "..229400D". The detail page shows the full ID.
- **Stable list order.** With a fix, the list is sorted by distance. Without one, it is
  sorted by when each drone was first heard, newest first, not by last heard as the brief
  suggested. Every active drone is heard about once a second, so sorting by last heard would
  reshuffle the rows on every frame. The age column still shows last heard. Stale drones go
  below a divider, most recently heard first.
- **Buzzer rate limit.** At most one beep per 3 s. A swarm or a drone show raises one alert
  with "+N more new", not a continuous tone. Muting the buzzer keeps the banner.
- **Full-frame redraw.** `render` clears and redraws everything. Draw into a framebuffer and
  flush it, so the panel never shows a half-drawn frame. 320×240×2 is 150 KB, which fits in
  the S3's PSRAM. Without PSRAM use `render_banded`: the same frame in 10 bands of 15 KB, each
  sent finished, so nothing flickers either. It costs one `render` pass per band; `Band`
  rejects fills and glyphs outside the band before generating their pixels. On the S3 hub
  the radar page takes ~75 ms for all 10 bands.

## Tensions (for the owner)

1. **North-up radar without a compass can mislead.** People expect a radar to turn with them.
   Here "up" is always north. If you face east, a drone straight ahead is drawn on the right.
   Mitigations:
   - "NORTH UP / not the way you face" in the radar's corner.
   - A boxed **N** at the top of the plot.
   - The readout's arrow sits inside a small compass rose with its own N.
   - "You" is a ring and dot, never a triangle, which would imply a facing.
   - The bearing is always given as words too ("NE 047°").

   It stays imperfect for anyone who doesn't know which way north is. **Proposal:** a magnetometer
   (QMC5883L or LIS3MDL, about $2, I²C) would allow a true heading-up mode. Hold the unit flat
   and the radar turns with you. Worth adding to the Phase 5 board.
2. **Selection versus "nearest".** While you are choosing a drone, the big readout shows the
   selected drone, not the nearest. A forgotten cursor would leave the glance answer wrong.
   It is labelled **DRONE 3** in an inverse box instead of NEAREST, and it times out after 30 s.
3. **Detail never times out.** Someone watching one drone should not be thrown out, and a
   fixed sensor left on a detail page was left there on purpose. The cost is that a unit left
   on a detail does not show new drones as a radar would. The new-drone banner still
   appears over it.
4. **Signal strength is not distance.** Without a fix, signal bars are the only hint of
   closeness. Wi-Fi and BLE transmit at different powers, so a strong BLE signal may be
   farther away than a weak Wi-Fi one. The no-fix page says "signal is not distance" and does
   not sort by it.
5. **Small text on a 2.0" panel.** The status bar and labels are 10-13 px, about 1.3-1.7 mm on
   a 2.0" panel. That is fine at reading distance but not at arm's length. Only the big number,
   the bearing and the count are designed for a glance. A **2.8" panel** makes everything about
   40% larger for little extra cost, and is the better choice for handheld use.
6. **Height reference.** The drone reports height "above takeoff" (sometimes "above ground").
   That is not the same as height above you, which is shown separately when the unit has a 3D fix.
7. ~~5 GHz Wi-Fi is inferred.~~ Fixed 2026-11-09: the tracker keeps 2.4 and 5 GHz Wi-Fi in
   separate slots, so a drone heard on both bands shows both "W" and "5", each with its own signal.
8. **UTC, not local time.** Logs and TAK are in UTC, and the unit has no time zone setting.
   The clock is labelled "UTC" so nobody mistakes it for local time.

## What the hub must provide

- The tracker, through `fill_from_tracker`, or a filled `drones` list if you write your own.
- GPS: the fix type and satellites used (`Gps`), and UTC in Unix ms (`utc_ms`, from GPS time).
- Per node (`NodeStatus`): kind, alive (heard within a timeout of your choice), frames/s, RID
  frames found, drops and uptime. The node link needs to carry these counters.
- Power (`Power`): USB or battery %. SD log state (`SdLog`). Hub uptime.
- Buttons: debounced levels into `ButtonDecoder::poll` every 10-20 ms.
- Buzzer: call `ui.take_buzz()` each frame and chirp when it returns `Some`.
- Settings: read `ui.settings()` to set the backlight PWM. Optionally save it and restore it
  with `Ui::new(settings)` at boot.
- A framebuffer to render into, then flush to the panel.

## Tests

`cargo test -p screen` runs:

- **The state machine:** navigation, the cursor following a re-sorted drone, the banner taking
  the first press, alert expiry and stacking, the buzzer rate limit and mute, idle timeouts,
  and the settings edit.
- **The button decoder:** short and long press, and repeat.
- **Sort order, and formatting** of distances, bearings, IDs and ages.
- **The tracker adapter**, against a real `tracker::Tracker`.
- **Banded rendering** equals full-frame rendering, pixel for pixel, at 24, 7 and 1 rows per band.
- **Every snapshot.** Each one also asserts that no pixel was drawn off the panel.

`cargo build -p screen --target riscv32imac-unknown-none-elf` checks the `no_std` build.
