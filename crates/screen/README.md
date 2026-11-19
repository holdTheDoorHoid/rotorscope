# screen

The on-device screen (PLAN.md O2, O6, P2, P4): a 320×240 landscape RGB565 panel
(ST7789 2.0" or ILI9341 2.8" class) and three buttons. `no_std`, no allocator.
It draws with `embedded-graphics` 0.8, so the hub can drive any panel through `mipidsi`.

Every screen is rendered to PNG on the laptop by `cargo test -p screen`. The images
are in [`snapshots/`](snapshots/) (320×240) and [`snapshots/wide/`](snapshots/wide/)
(the P4 hub's native 800×480 layout, one folder per theme). Look at them before
reading the rest.

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
if let Some(b) = keys.poll(up, down, select, now) {
    ui.button_at(b, &state, keys.select_started());   // a press begun before a banner is ignored
}
if let Some(r) = ui.take_request() { ui.action_result(do_it(r)); }   // "done" only when it worked
ui.tick(now);
ui.observe(&state);                      // sticky banners, Auto's quiet-moment switch
render(&ui, &state, &mut framebuffer)?;   // any DrawTarget<Color = Rgb565>
panel.flush(&framebuffer);
apply_backlight(screen::backlight_pct(ui.backlight_level(&state)));   // Red Night caps it
```

| Item | What it is |
|---|---|
| `UiState` | Plain view-model: `now_ms`, `utc_ms`, `gps`, `nodes` (≤5), `power`, `sd`, `drones` (≤32 `DroneView`), `total_drones`, `hub_uptime_s`. The screen sorts the drones itself. |
| `DroneView` | `key` (TrackId), `id` + `id_kind`, `ua`, `stale`, first/last seen, `drone` and `operator` (`Polar`: distance + bearing from you), `drone_to_operator_m`, height/alt/above-you, speed/vspeed/track, `radios` (W N B L 5), `rssi` per radio, `self_id`, `operator_id`. |
| `Ui` | State machine: page, mode, cursor (by drone key, so it follows a drone while the list re-sorts), alert banner, settings, buzzer request, settings actions and their results. `button` / `button_at(Button, &UiState, pressed_ms)`, `tap(Tap, &UiState, pressed_ms)`, `tick(now_ms)`, `observe(&UiState)`, `new_drone(key, now_ms)`, `take_buzz()`, `take_request()` + `action_result(Result)`, `backlight_level`, `set_has_buzzer`, `set_held`. |
| `Button` | `Up`, `Down`, `Select` (short press), `Back` (long press of Select). |
| `Tap` | A touch on the page, as `wide::tap_target` resolved it: `Drone(key)`, `Flag(row)`, `SettingsRow(i)`, `Banner`, `Nothing`. |
| `Warning` | `UiState::warnings()`: the conditions that make the screen partly blind or its numbers doubtful, highest first (see "Warnings"). |
| `ButtonDecoder` | Raw debounced levels → `Button`. Select fires on release; Back fires once Select has been held for 700 ms (the P4's touch keys use `wide::HOLD_MS`, 1000 ms). Up/Down auto-repeat after 500 ms, every 200 ms. `select_started()` is when the last Select press began. |
| `render(&Ui, &UiState, &mut D)` | Clears and draws one whole frame. It takes the `Ui` as well as the `UiState`, because the page and cursor belong to the screen, not the hub. |
| `band::Band`, `render_banded(&Ui, &UiState, &mut Band, flush)` | Draw the frame 24 rows at a time into a 15 KB buffer (`BAND_ROWS`) and hand each band to `flush(top, rows, pixels)`. For hubs without PSRAM; the test `banded_render_matches_full_frame` checks it gives exactly the full-frame pixels on every page. |
| `Fix` | `None` (searching), `Fix2D`, `Fix3D`, `Manual` (a typed position is in use, "POS TYPED"), `Absent` (no GPS receiver, "NO GPS"). |
| `SdLog` | `Off`, `Recording`, `NoCard` (quiet "no SD"), `Error` ("SD!"). |
| `from_tracker::fill_from_tracker` | Feature `tracker`. Fills `drones`, `total_drones`, `now_ms` and `utc_ms`. If the tracker considers the own position too old, it sets a 2D/3D/manual `gps.fix` to `None`, so the layout and the numbers agree. When there are more than 32 tracks, it picks active before stale, then nearest, then newest. |

The renderer does not depend on the tracker. Only the optional adapter does.

## Buttons and pages

```
          Up/Down                                   Select              Select
 RADAR ⇄ LIST ⇄ FLAGS ⇄ SIGNALS ⇄ HEALTH ⇄ SETTINGS ─────► choose a drone ─────► DETAIL
   ▲                                                          (cursor)          Up/Down: next drone
   └──── hold Select (Back) ◄──── hold: stop choosing ◄──────────┘   tap or hold: back to the page
```

- **Browse** (the default): Up/Down flip between pages, wrapping round. The page's title
  block inverts for 1.5 s after each flip (it never covers the page). Hold from any page goes
  home to the radar. FLAGS is in the cycle only when the hub supplies flags. Every page title
  carries its place in the cycle ("3/6").
- **Choose:** Select on the Radar or List puts a cursor on the nearest drone (on the newest one
  without a fix), or on the drone last looked at. Up/Down move it, Select opens the detail,
  hold stops choosing ("CHOOSING" beside the title meanwhile).
- **Detail:** Up/Down step through drones in list order. One press of the middle key, tap or
  hold, goes straight back to the page the drone was opened from (the key says "Back to radar"
  / "Back to drone list" / "Back to flags"). After 2 min with no press it goes back by itself,
  keeping the drone for the next Select.
- **Pages with nothing to choose** (Health; Signals with every row showing; Flags with no
  rows): a tap does what holding does (back to the radar), and the key says "tap or hold".
- **Signals and Flags with more rows:** Select scrolls (SIGNALS: one list of every row, Up/Down
  scroll, tap or hold leaves) or chooses (FLAGS: Up/Down move a row cursor, Select opens that
  drone's detail when it is a tracked drone, hold leaves). Health does not scroll (see below).
- **Settings:** Select to edit ("EDITING"). Up/Down pick a row and stop at the ends, Select
  changes it, hold finishes. Settings first: Theme (Auto → Red Night → Glass Cockpit → Field
  Day → Auto, so the dark themes sit together), Brightness (Select, then left / right keys
  dimmer / brighter, stopping at 1 and 5; Select or hold to finish), Buzzer (only on hubs with
  one). While Theme is Auto: Day theme, Night theme, Switch at. Then, under **ACTIONS**,
  drawn as chips with a chevron: Hub location ("I moved the hub"; "New spot found / Use this
  spot" while the hub waits on a new place), Alarms ("Mute Flood of fake IDs +1 · 2 h",
  "Snooze Jamming (wideband) · 5 min", "Unmute 2"), Nodes at hub ("Place all here").
  Forgetting the hub's home and placing every node ask to confirm: the first press relabels
  the key **PRESS AGAIN TO CONFIRM** ("place 3 nodes here") for 4 s; "Use this spot" asks too
  while the hub's GPS may be jammed or faked, and the page then warns "GPS may be faked: use
  this spot only if you really moved the hub". An action shows "Working..." until the hub
  answers (`Ui::action_result`): "Placed", "Muted" and so on with a check mark only when it
  worked, else the reason ("No GPS position: not placed"); no answer in 10 s is "No answer
  from the hub". A row whose action just ran ignores Select until the cursor leaves it (no
  flip to "Unmute" under the finger). When an edit ends (hold, the 30 s timeout, or the banner
  taking you to a drone) with something changed, `Ui::take_settings_change` hands the hub
  (before, after) once; the P4 appends `theme ...` / `brightness N` lines to `SITE.TXT`.
- **Alert banner:** the first press goes to the banner. Select opens its drone's detail; Up,
  Down or hold only dismiss it. A press that began before the banner appeared is ignored
  (`Ui::button_at`). The banner shows the most serious of the new drones (flagged at the worst
  level) or else the nearest, with "+N more new". A new drone within 300 m (as broadcast) or
  flagged Serious keeps its banner until a key is pressed ("close by: stays until you press a
  key"); others go after 5 s. After you dismiss a banner, new drones for 15 s only count up a
  **NEW n** chip in the status bar, so a swarm cannot keep the banner in the way. Every new
  drone is marked **NEW** on the radar and in the list's Heard column for a minute.
- **Touch (P4).** The keys fill the bottom strip (y 414-476, touch from 410). The page can be
  touched too (`wide::tap_target` runs the same layout without pixels to find what is under the
  finger, then `Ui::tap`): a drone marker (within 26 px, nearest wins), the radar's readout card
  or a list row opens that drone; a flag row opens its drone when it is a tracked one; a
  settings row is chosen by the first tap and changed by a second tap on it (never on the
  first, and destructive actions still confirm); on the banner, a tap on it shows its drone and
  anywhere else only dismisses it; on a detail page touches do nothing. A tapped row or marker
  is highlighted briefly. Anything else shows "Tap a drone, or use the keys below" in the
  title block and flashes the top of the key strip. The hub fires a tap on release, ignores
  one that moved more than 20 px, and the state machine ignores one that began before the
  screen last changed. Rows are at least 40 px tall.
- **Timeouts:** choosing, scrolling and settings return to browsing after 30 s with no press;
  detail after 2 min.

## Warnings

`UiState::warnings()`, highest first: **NOT LISTENING** (no drone receiver answering, or none
reported), a drone near a crewed aircraft at caution or worse ("DRONE 240 m FROM N911PH"),
**JAMMING** ("drones may be missed"), **GPS MAY BE FAKED** (the hub's own), **AREA GPS FAKED**,
**FAKE IDS**, a Serious flag ("POSSIBLE SERIOUS RULE ISSUE · see Flags"), some receivers down
("5 GHz RECEIVER DOWN · drones on 5 GHz won't be heard"), AREA GPS JAMMED, GPS JAMMED, and NO
POSITION. The wide screen puts the first in a band under the status bar on every page with "+N
more", and the status bar shows each as a chip. The radar's empty readout says NOT LISTENING /
JAMMED / NOT ALL RECEIVERS LISTENING rather than calm words, and the count dims and says
"receivers down: may be out of date" when nothing listens. Auto never switches the look during
the acute ones (all but receivers down, area/own GPS jammed and no position, which can hold for
good).

## Screens

| Snapshot | What it shows |
|---|---|
| `radar_0_drones` | Empty radar: "No drone IDs heard. Drones without Remote ID may only show on Signals.", count 0 (grey). |
| `radar_1_drone` | One drone: big distance, unit word, a compass rose with an arrow, the compass point and degrees, height, ID. The pilot is a blue X joined to the drone. |
| `radar_5_drones` | Five active drones and one stale one, one beyond 3 km, and one with no position ("+1 without position"). |
| `radar_5_drones_day` | The same in the Field Day theme. |
| `radar_pick` | Choosing a drone: the readout says **DRONE 3** in an inverse box instead of NEAREST. |
| `list_5_drones`, `list_pick` | The list sorted by distance, with a stale divider and the selected row inverted. |
| `no_fix`, `list_no_fix` | No GPS position: the home page shows the drone list instead of the radar (the P4 layout), with a "NO POSITION - distances unavailable" bar, and signal bars instead of distance. |
| `alert`, `alert_more` | The new-drone banner, with "+1 more new". |
| `detail`, `detail_no_fix` | Full ID, drone and pilot direction, height, speed, operator ID, Self-ID, signal per radio, and when last heard. With no fix, the pilot row shows the drone-to-pilot distance, which still works. |
| `health` | Per node: frames/s, ID messages, lost, uptime. A dead node shows NOT RESPONDING. Below that: GPS, drones seen, SD log and power. |
| `health_aircraft` | A hub with an ADSBee: the ADS node in the list and status bar (four nodes pack closer), and an AIRCRAFT line: active count, how many are LOW (accent colour), the nearest by callsign with distance and altitude. Never an alert. |
| `settings` | The settings page, mid-edit. |
| `settings_actions`, `settings_snooze` | The Alarms row: "Mute 2, 2 h" for nuisance alarms; "Snooze 1, 5 min" when only a jammer (or another safety-critical alarm) sounds. |
| `caveats_alarms`, `caveats_alarms_health` | The strip with alarms silenced (`muted 2: 1 h 59 min  snoozed 1: 4:05  site 1`) and the collection switches on (`ON: control IDs, control content`); the health page's ALARMS line. |
| `status_warnings` | Status bar under stress: a 2D fix, every node dead (NOT LISTENING), an SD error, 9% battery and no UTC. |
| `status_gps_absent` | No GPS receiver ("NO GPS"), no SD card ("no SD"), on USB: the bench hub today. |
| `status_manual_pos` | A typed position in use ("POS MAN" at 320x240, "POS TYPED" wide): distances work without a GPS fix. |
| `page_toast` | The page name after flipping. |

**Status bar (every screen):**

- GPS: "GPS 11", or an inverse **NO FIX** box. A 2D fix shows a small "2D". While the hub's own
  GPS may be faked or jammed, the GPS item itself is the chip (**GPS FAKE?**).
- Wide: every warning as a chip (PLANE NEAR, JAMMED, AREA GPS FAKE, FAKE IDS, FLAG...), dead
  receivers as one boxed chip ("5 DOWN", "DOWN 2.4 5 BT"), the NEW chip, then one dot per live
  node with its label (2.4 / 5 / BT / DJI / LINK / PLANES / LR; a hollow ring when it hears
  nothing); "+N" for what does not fit. 320x240: the worst one chip (NOT LISTENING when no
  receiver answers) or the node dots, a dead node as "5 DOWN".
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
- **Contrast.** In Glass Cockpit, data is white on near-black (about 20:1), field names and
  column heads are cyan #46C8F0 (about 10:1), secondary text (notes, units, "+N more",
  degrees) is #8FA3B5, stale is #6E8090 (about 4.7:1), drones are magenta (#FF4FD8) and the
  pilot lavender (#C9A0FF; green is node health only). Range rings are solid, 1 px, at 3:1 or
  better against the background in every theme (#3A6A90 in Glass Cockpit).
- **Three themes, Glass Cockpit by default.** Glass Cockpit (as above, amber cautions), Field
  Day (dark ink on light grey for direct sun; drones dark orange #C2410C, kept apart from the
  amber #F2B200 caution fills with black ink; caution text on the light page #92400E; status
  chips dark ink on amber; a pressed key turns light, never dark as if off) and Red Night
  (pure red only, every colour (R, 0, 0), since green and blue light is what costs night
  vision; dim text at 3:1 or better against black; large areas such as the warning band, the
  fault bar, a pressed key and the banner are outlined, not filled). Drones and cautions have
  separate colours, and every distinction is also a shape or a word. Brightness 1-5 maps to
  10/25/45/70/100 % (`backlight_pct`); Red Night caps it at 2 unless set lower
  (`Ui::backlight_level`), and the row says so ("3 (2 in Red Night)").
- **Auto switches by the sun.** `screen::sun` is NOAA's solar-position algorithm (pure,
  `no_std`, f64 through `libm`), tested against US Naval Observatory times for Philadelphia,
  Sydney and Tromsø (polar day and night) to within two minutes. Auto shows the day theme
  while the sun is above -0.833° (sunrise and sunset) or -6° (civil dawn and dusk), from the
  hub's position (`UiState::position`, filled by `fill_from_tracker`) and GPS time. It
  recomputes once a UTC minute. Before position and time have ever been known it shows Glass
  Cockpit (it works by day and by night) and the settings page says "Waiting for GPS · showing
  Glass Cockpit until it knows the time"; a lost fix keeps the last position, lost time keeps
  the last verdict. With `Ui::observe` called each frame, a due switch waits for a quiet
  moment: never while a banner is up, while choosing, reading a detail or editing, during an
  acute warning, or within 2 min of a key press or a new drone (at most 10 min); never at all
  while the hub's GPS may be faked. A switch says "Switched to Red Night (dusk)" in the title
  block for 4 s. The settings page says "Now: Glass Cockpit (daytime) · Red Night at dusk, in
  8 h 30 min (22:33 UTC)". `Ui::effective_theme` is the look to draw; both renderers use it.
- **Log-ish range scale with fixed rings** at 100 m, 300 m, 1 km and 3 km. It does not
  auto-scale, so a position on the plot always means the same thing and people learn it.
  Nearby drones get room, and far ones still fit. Anything past 3 km is a marker on the rim
  labelled with its distance ("4.8 km"), not an arrow that would read as a heading. The wide
  radar labels each ring where it crosses the 135° line and says "rings not to scale".
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
  with "+N more new", not a continuous tone. Muting the buzzer keeps the banner. A hub without
  a buzzer (`Ui::set_has_buzzer(false)`, the P4) has no Buzzer row and never asks for a beep.
- **Units.** One space between every number and its unit ("320 m", "1.4 km"); never "m" for
  minutes ("12 s", "4 min", "1 h 02 min", "4:05 left"). Heights lead with "87 m above you"
  when the hub has a 3D fix, else "85 m above its takeoff". Flag evidence is in metres first,
  with feet in brackets where the FAA rule is in feet.
- **Claims, not facts.** Positions are what drones broadcast: the readout says "as
  broadcast", the banner "Position as broadcast by the drone", the detail's CLAIM row (right
  under where the drone and its pilot are) "Sent by the drone itself; can be faked". The pilot
  is "Pilot (reported)" with "May be old or faked. Do not approach: report it." Two
  transmitters using one ID keep the ID visible beside "! 2 transmitters use this ID", the big
  number is dimmed, and both claims are drawn the same way (no verdict on which is real).
- **Full-frame redraw.** `render` clears and redraws everything. Draw into a framebuffer and
  flush it, so the panel never shows a half-drawn frame. 320×240×2 is 150 KB, which fits in
  the S3's PSRAM. Without PSRAM use `render_banded`: the same frame in 10 bands of 15 KB, each
  sent finished, so nothing flickers either. It costs one `render` pass per band; `Band`
  rejects fills and glyphs outside the band before generating their pixels. On the S3 hub
  the radar page takes ~75 ms for all 10 bands.

## The wide layout (P4, feature `wide`)

`screen::wide` draws the same pages at 800×480: a 38 px status bar, the warning band (34 px,
while any warning holds), the page, the fault bar and notes lines, and three context keys along
the bottom (y 414-476, filling the touch zone from 410) whose labels say what each key does in
the current state (`wide::key_labels`). The middle key's small second line is always in the
same place and casing: "hold: back to radar", "hold: stop choosing", "hold: finish", "tap or
hold" where a tap does the same as holding, or what a confirm will do. Only a page's main
action is drawn in the accent; a key that only goes back is drawn plain. While the middle key
is held it fills up towards the hold (`Ui::set_held`, `wide::HOLD_MS`). `wide::key_at` maps a
touch to a key; `wide::tap_target` maps a touch on the page to what is drawn there.

- **Strip above the keys.** The most serious drone's threat level, when there is one at Caution
  or above and not snoozed (`UiState::threat_strip`), is a filled bar at the top of the strip:
  red from Warning up, caution-coloured at Caution.
  Only a fault (evidence not written) is a filled bar; standing notes
  (test key, open link, bench build, mutes, collection switches) are at most two muted lines
  with a small amber tick, "+N notes: see Health" when more; the health page lists them all.
  With the strip and band up, the radar readout keeps its HEIGHT and ID rows (the big number
  shrinks first).
- **Where am I.** Every page has a title with its place in the cycle ("DRONE LIST 2/6 5"); the
  no-fix home page is "HOME · NO POSITION"; choosing, editing and scrolling show a one-word
  mode chip.
- **Lettering** is anti-aliased: 4-bit glyphs rasterised from B612 and B612 Mono (Glass
  Cockpit, Red Night) and IBM Plex Sans Condensed (Field Day) by `tools/fontgen`
  (`cargo run -p fontgen --release`). The TTFs and their SIL OFL licences are in
  [`fonts/`](fonts/); the generated `src/wide/glyphs.rs` and `glyphs.bin` (235 KB of
  bitmaps, about 270 KB of flash with metrics) are committed, so building needs neither.
  Big numerals (48/72/104 px) hold digits, `.`, `-` and space only. Arrows, diamonds, the
  pilot's X, check marks and chevrons are drawn as anti-aliased shapes (the fonts have
  ASCII, `°` and `·` only).
- **Banded:** `wide::render` draws one band (`wide::Canvas`, whole 800-pixel rows) and every
  primitive clips to its rows first; `wide::render_banded` runs the bands. A test checks
  bands of 48, 7 and 1 rows equal the full frame on every case, and that nothing is drawn
  off the panel. `tap_target` runs the same layout into a canvas with no rows.

### Wide snapshot cases (`snapshots/wide/<theme>/`)

Besides the 320x240 names above (same states at 800x480):

| Snapshot | What it shows |
|---|---|
| `band_not_listening`, `band_no_receivers` | Every receiver down / none reported: NOT LISTENING band, chips, the readout and count say so. |
| `band_some_nodes_down`, `list_some_nodes_down` | One receiver down: "5 GHz RECEIVER DOWN · drones on 5 GHz won't be heard", "NOT ALL RECEIVERS LISTENING". |
| `band_near_plane` | A drone 240 m from N911PH. |
| `band_many_warnings`, `detail_with_band` | Jamming, fake IDs, faked GPS (own and area), a dead and a deaf node: the band with "+4 more", every chip, "+N" where they run out. |
| `radar_jammed_empty`, `radar_links_without_id` | Empty radar while jammed; control links / video / Wi-Fi heard but no IDs: "+8 radio links, no ID: see Signals" (the longest wording that fits). |
| `health_down_and_deaf` | NOT RESPONDING with "last heard 2 min ago · drones on 5 GHz won't be heard · check power, cable, range"; HEARS NOTHING for a live node at 0 frames/s. |
| `alert_close_sticky` | A new drone at 75 m: the banner stays until a key is pressed. |
| `radar_swarm_chip`, `list_new_marks` | After a dismissal, new arrivals as a NEW 2 chip; NEW marks on the radar and in the list. |
| `pick_key_held` | The middle key pressed and 60 % of the way to a hold. |
| `radar_tap_hint` | A touch that did nothing: the hint in the title block, the key strip flashed. |
| `settings_tapped_row` | A tapped settings row: chosen and highlighted, nothing changed. |
| `settings_brightness_adjust`, `settings_brightness_capped` | Adjusting brightness (Dimmer / Brighter keys); "4 (2 in Red Night)" in the Red Night folder. |
| `settings_confirm_nodes`, `settings_action_failed`, `settings_action_done`, `settings_alarms_muted` | PRESS AGAIN TO CONFIRM ("place 3 nodes here"); "No GPS position: not placed"; "Learning new spot..."; "Muted" with a check, the key inert. No Buzzer row (a hub without one). |
| `settings_use_spot_doubt` | "Use this spot" while GPS may be faked: confirm, and the warning line. |
| `flags`, `flags_choose` | Check / Watch / Likely / Serious pills, sorted by level then age, metres-first evidence, "plane" for an aircraft; choosing a row to open its drone. |
| `signals_scroll` | SIGNALS scrolled: one list, "rows 4-13 of 17". |
| `radar_theme_switched` | Auto just switched: "Switched to Red Night (dusk)". |
| `caveats_notes_only` | Standing notes only: one muted line, no filled bar. |
| `threat_strip_critical`, `threat_strip_full` | A drone's threat level (docs/THREAT-PROFILES.md): a filled bar above the faults, red from Warning up, caution-coloured at Caution (Red Night: an outline, heavier from Warning); with a fault and notes as well. |

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
3. **Detail times out after 2 min.** Someone watching one drone is not thrown out at once,
   but a unit left on a detail goes back to the page it came from, so the glance answer is
   the nearest drone again. The drone stays the first one Select picks. The detail also shows
   "NEAREST 75 m NW · 5 drones" in its corner meanwhile.
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
- Per node (`NodeStatus`): kind, alive (heard within a timeout of your choice), frames/s
  (`None` if unknown; `Some(0)` on a live node shows HEARS NOTHING), ID messages found, frames
  lost, uptime, and seconds since last heard (`silent_s`, for "last heard 2 min ago").
- `Threats::mutable_what` / `snoozable_what` (the tracker's `alarm_names`) so the Alarms row
  names what it acts on; `FlagRow::drone` (the track key) and `aircraft` for the FLAGS page.
- Power (`Power`): USB or battery %. SD log state (`SdLog`). Hub uptime.
- Buttons: debounced levels into `ButtonDecoder::poll` every 10-20 ms.
- Buzzer: call `ui.take_buzz()` each frame and chirp when it returns `Some`; without one,
  `ui.set_has_buzzer(false)`.
- Settings: `ui.backlight_level(&state)` for the backlight. Optionally save the settings and
  restore them with `Ui::new(settings)` at boot.
- Actions: `ui.take_request()`, do it, then `ui.action_result(Ok(()))` or `Err("why")`.
- A framebuffer to render into, then flush to the panel.

## Tests

`cargo test -p screen` runs:

- **The state machine:** navigation, the cursor following a re-sorted drone, the banner taking
  the first press (and ignoring a press begun before it), which new drone it shows, sticky
  banners, the swarm lockout and NEW marks, alert expiry and stacking, the buzzer rate limit
  and mute, idle timeouts, the settings edit (cursor stops, brightness adjust, confirm steps,
  action results, inert rows, every way out ending the edit), Auto's quiet-moment switching,
  taps, flag choosing, signal scrolling, the warnings' order and words, the backlight cap.
- **Key labels and touch:** the keys name what the state machine does; `tap_target` finds
  the readout card, list rows, settings rows, the banner and every radar marker.
- **The button decoder:** short and long press, and repeat.
- **Sort order, and formatting** of distances, bearings, IDs and ages.
- **The tracker adapter**, against a real `tracker::Tracker`.
- **Banded rendering** equals full-frame rendering, pixel for pixel, at 24, 7 and 1 rows per band.
- **Every snapshot.** Each one also asserts that no pixel was drawn off the panel.

`cargo build -p screen --target riscv32imac-unknown-none-elf` checks the `no_std` build.
