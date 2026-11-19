//! State machine, button decoder, ordering and formatting.

extern crate std;

use heapless::String;

use crate::draw;
use crate::model::*;
use crate::ui::*;

fn dv(key: u32, dist: Option<f32>, first_s: u64, last_s: u64, stale: bool) -> DroneView {
    DroneView {
        key,
        first_seen_ms: first_s * 1000,
        last_seen_ms: last_s * 1000,
        stale,
        drone: dist.map(|d| Polar {
            distance_m: d,
            bearing_deg: 0.0,
        }),
        ..DroneView::default()
    }
}

fn state(fix: bool, drones: &[DroneView]) -> UiState {
    let mut s = UiState {
        now_ms: 100_000,
        gps: Gps {
            fix: if fix { Fix::Fix3D } else { Fix::None },
            sats: 8,
            ..Gps::default()
        },
        ..UiState::default()
    };
    for d in drones {
        s.drones.push(d.clone()).unwrap();
    }
    s.total_drones = drones.len() as u16;
    // One receiver listening, so nothing warns unless a test says so.
    s.nodes
        .push(NodeStatus { kind: NodeKind::Wifi24, alive: true, frames_per_s: Some(400), rid_found: 0, drops: 0, uptime_s: 99, silent_s: Some(0) })
        .unwrap();
    s
}

fn keys(s: &UiState) -> std::vec::Vec<u32> {
    s.order()
        .iter()
        .map(|&i| s.drones[usize::from(i)].key)
        .collect()
}

fn three() -> UiState {
    state(
        true,
        &[
            dv(10, Some(900.0), 1, 99, false),
            dv(11, Some(50.0), 2, 99, false),
            dv(12, Some(300.0), 3, 99, false),
        ],
    )
}

// --- ordering ------------------------------------------------------------

#[test]
fn order_with_fix_is_by_distance_then_unplaced_then_stale() {
    let s = state(
        true,
        &[
            dv(1, Some(900.0), 1, 99, false),
            dv(2, None, 5, 99, false),
            dv(3, Some(10.0), 1, 20, true),
            dv(4, Some(80.0), 2, 99, false),
        ],
    );
    assert_eq!(keys(&s), [4, 1, 2, 3]);
}

#[test]
fn order_without_fix_is_newest_first_then_stale_by_last_heard() {
    let s = state(
        false,
        &[
            dv(1, None, 10, 99, false),
            dv(2, None, 30, 98, false),
            dv(3, None, 5, 20, true),
            dv(4, None, 1, 30, true),
        ],
    );
    // Not by last heard (1 was heard more recently than 2): rows must not
    // reshuffle every second.
    assert_eq!(keys(&s), [2, 1, 4, 3]);
}

// --- navigation ----------------------------------------------------------

#[test]
fn up_down_flip_pages_and_wrap() {
    let s = three();
    let mut ui = Ui::default();
    assert_eq!(ui.page(), Page::Radar);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::List);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Signals);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Health);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Settings);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Radar);
    ui.button(Button::Up, &s);
    assert_eq!(ui.page(), Page::Settings);
    // Back from any page goes home.
    ui.button(Button::Back, &s);
    assert_eq!(ui.page(), Page::Radar);
}

#[test]
fn select_picks_nearest_then_opens_detail_and_back_returns() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Pick);
    assert_eq!(ui.cursor(), Some(11), "cursor starts on the nearest");
    ui.button(Button::Down, &s);
    assert_eq!(ui.cursor(), Some(12));
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Detail);
    // Up/Down in detail step through drones in list order.
    ui.button(Button::Down, &s);
    assert_eq!(ui.cursor(), Some(10));
    ui.button(Button::Down, &s);
    assert_eq!(ui.cursor(), Some(11), "wraps");
    // One hold goes straight back to the page it was opened from.
    ui.button(Button::Back, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    assert_eq!(ui.cursor(), None);
    assert_eq!(ui.page(), Page::Radar);
    // Choosing again starts from the drone last looked at.
    ui.button(Button::Select, &s);
    assert_eq!(ui.cursor(), Some(11));
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s);
    // In Detail, a tap does what holding does.
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    ui.button(Button::Select, &s);
    assert_eq!(ui.cursor(), Some(12), "resumes on the drone last opened");
}

#[test]
fn cursor_follows_the_drone_when_the_list_resorts() {
    let mut s = three();
    let mut ui = Ui::default();
    ui.button(Button::Select, &s);
    ui.button(Button::Down, &s); // on 12 (300 m), second in the list
    assert_eq!(ui.cursor(), Some(12));
    s.drones[2].drone = Some(Polar {
        distance_m: 5.0,
        bearing_deg: 0.0,
    });
    assert_eq!(keys(&s)[0], 12, "12 is now nearest");
    ui.button(Button::Down, &s);
    assert_eq!(
        ui.cursor(),
        Some(11),
        "moved from 12's new place, not its old one"
    );
}

#[test]
fn select_with_no_drones_does_nothing() {
    let s = state(true, &[]);
    let mut ui = Ui::default();
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    ui.button(Button::Down, &s); // List
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Browse);
}

#[test]
fn signals_and_health_tap_goes_home_and_settings_edit() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Down, &s);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Signals);
    // Nothing hidden on SIGNALS: a tap does what holding does.
    ui.button(Button::Select, &s);
    assert_eq!((ui.mode(), ui.page()), (Mode::Browse, Page::Radar));
    ui.button(Button::Up, &s);
    ui.button(Button::Up, &s);
    assert_eq!(ui.page(), Page::Health);
    ui.button(Button::Select, &s);
    assert_eq!((ui.mode(), ui.page()), (Mode::Browse, Page::Radar));

    ui.button(Button::Up, &s);
    assert_eq!(ui.page(), Page::Settings);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Edit);
    ui.button(Button::Up, &s);
    assert_eq!(ui.settings_row(), 0, "the cursor stops at the top");
    ui.button(Button::Select, &s); // theme
    assert_eq!(ui.settings().theme, ThemeMode::Fixed(Theme::FieldDay));
    ui.button(Button::Down, &s);
    // Brightness: Select starts adjusting, Up/Down change it and stop at
    // the ends, Select finishes.
    ui.button(Button::Select, &s);
    assert!(ui.adjusting());
    for _ in 0..4 {
        ui.button(Button::Down, &s);
    }
    assert_eq!(ui.settings().brightness, 5, "stops at 5, never wraps to 1");
    for _ in 0..6 {
        ui.button(Button::Up, &s);
    }
    assert_eq!(ui.settings().brightness, 1, "stops at 1");
    ui.button(Button::Select, &s);
    assert!(!ui.adjusting());
    assert_eq!(ui.settings_row_kind(), SettingsRow::Brightness, "Up/Down moved the level, not the cursor");
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s);
    assert!(!ui.settings().buzzer);
    ui.button(Button::Down, &s);
    assert_eq!(ui.settings_row_kind(), SettingsRow::HubMoved);
    // "I moved the hub" forgets home: the first press only arms it.
    ui.button(Button::Select, &s);
    assert_eq!(ui.confirming(), Some(SettingsRow::HubMoved));
    assert_eq!(ui.take_request(), None, "nothing yet");
    ui.button(Button::Select, &s);
    assert_eq!(ui.take_request(), Some(Request::HubMoved));
    assert_eq!(ui.take_request(), None, "taken once");
    assert_eq!(ui.action().map(|a| a.state), Some(ActionState::Pending), "not done until the hub says");
    ui.action_result(Ok(()));
    assert_eq!(ui.action().map(|a| a.state), Some(ActionState::Done));
    // Done: inert until the cursor leaves the row.
    ui.button(Button::Select, &s);
    assert_eq!(ui.take_request(), None);
    assert_eq!(ui.confirming(), None);
    // WP-12: while the hub waits on a new place, the same row accepts it
    // (GPS fine: no confirm step).
    let mut waiting = s.clone();
    waiting.gps.home_pending = true;
    ui.button(Button::Down, &s);
    ui.button(Button::Up, &s);
    assert_eq!(ui.action(), None, "leaving the row forgets the action");
    ui.button(Button::Select, &waiting);
    assert_eq!(ui.take_request(), Some(Request::AcceptHome));
    // With GPS integrity in doubt it asks to confirm.
    waiting.gps.integrity = GpsIntegrity::SuspectSpoofing;
    ui.button(Button::Down, &s);
    ui.button(Button::Up, &s);
    ui.button(Button::Select, &waiting);
    assert_eq!(ui.take_request(), None);
    assert_eq!(ui.confirming(), Some(SettingsRow::HubMoved));
    // The arm runs out.
    ui.tick(s.now_ms + CONFIRM_MS);
    assert_eq!(ui.confirming(), None);
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s); // alarms: nothing active, nothing muted
    assert_eq!(ui.take_request(), None);
    let mut loud = s.clone();
    loud.threats.active = 2;
    // Two alarms sound but neither has been announced yet: nothing to do.
    ui.button(Button::Select, &loud);
    assert_eq!(ui.take_request(), None);
    // A nuisance alarm and a jammer, both seen: Mute takes the nuisance
    // only, carrying the frame the operator looked at.
    loud.threats.mutable = 1;
    loud.threats.snoozable = 1;
    ui.button(Button::Select, &loud);
    assert_eq!(ui.take_request(), Some(Request::MuteAlarms { seen_ms: s.now_ms }));
    // No instant flip under the finger: inert until the cursor leaves.
    loud.threats.mutable = 0;
    ui.button(Button::Select, &loud);
    assert_eq!(ui.take_request(), None);
    ui.button(Button::Down, &s);
    ui.button(Button::Up, &s);
    // Then only the jammer: Snooze, a separate press.
    ui.button(Button::Select, &loud);
    assert_eq!(ui.take_request(), Some(Request::SnoozeAlarms { seen_ms: s.now_ms }));
    ui.button(Button::Down, &s);
    ui.button(Button::Up, &s);
    let mut quiet = s.clone();
    quiet.threats.muted = 2;
    quiet.caveats.silenced.muted = 1;
    quiet.caveats.silenced.snoozed = 1;
    ui.button(Button::Select, &quiet);
    assert_eq!(ui.take_request(), Some(Request::UnmuteAlarms));
    ui.button(Button::Down, &s);
    ui.button(Button::Up, &s);
    // Only the site file's mutes: the row cannot drop those.
    quiet.caveats.silenced = Silenced { site: 3, ..Silenced::default() };
    ui.button(Button::Select, &quiet);
    assert_eq!(ui.take_request(), None);
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s); // nodes at hub: confirm first
    ui.button(Button::Select, &s);
    assert_eq!(ui.take_request(), Some(Request::NodesAtHub));
    ui.action_result(Err("No GPS position: not placed"));
    assert_eq!(ui.action().map(|a| a.state), Some(ActionState::Failed("No GPS position: not placed")));
    ui.button(Button::Down, &s);
    assert_eq!(ui.settings_row_kind(), SettingsRow::NodesAtHub, "the cursor stops at the bottom");
    ui.button(Button::Back, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    assert_eq!(ui.page(), Page::Settings);
    assert_eq!(ui.action(), None, "cleared on leaving");
}

/// A pending action the hub never answers fails after a while.
#[test]
fn unanswered_action_fails() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Up, &s);
    ui.button(Button::Select, &s);
    for _ in 0..5 {
        ui.button(Button::Down, &s);
    }
    assert_eq!(ui.settings_row_kind(), SettingsRow::NodesAtHub);
    ui.button(Button::Select, &s);
    ui.button(Button::Select, &s);
    assert_eq!(ui.take_request(), Some(Request::NodesAtHub));
    ui.tick(s.now_ms + ACTION_WAIT_MS);
    assert!(matches!(ui.action().map(|a| a.state), Some(ActionState::Failed(_))));
    ui.action_result(Ok(()));
    assert!(matches!(ui.action().map(|a| a.state), Some(ActionState::Failed(_))), "a late answer does not turn it into done");
}

/// A hub without a buzzer has no Buzzer row and never asks for a beep.
#[test]
fn no_buzzer_row_without_a_buzzer() {
    let mut ui = Ui::default();
    assert!(ui.rows().contains(&SettingsRow::Buzzer));
    ui.set_has_buzzer(false);
    assert!(!ui.rows().contains(&SettingsRow::Buzzer));
    assert_eq!(ui.rows().len(), 5);
    ui.new_drone(1, 0);
    assert_eq!(ui.take_buzz(), None);
}

/// Brightness maps to the backlight, capped in Red Night.
#[test]
fn backlight_levels_and_the_red_night_cap() {
    let s = three();
    assert_eq!(BRIGHTNESS_PCT, [10, 25, 45, 70, 100]);
    assert_eq!(backlight_pct(1), 10);
    assert_eq!(backlight_pct(5), 100);
    assert_eq!(backlight_pct(0), 10);
    let ui = Ui::new(Settings { brightness: 4, ..Settings::default() });
    assert_eq!(ui.backlight_level(&s), 4);
    let ui = Ui::new(Settings { brightness: 4, theme: ThemeMode::Fixed(Theme::RedNight), ..Settings::default() });
    assert_eq!(ui.backlight_level(&s), RED_NIGHT_MAX_BRIGHTNESS);
    let ui = Ui::new(Settings { brightness: 1, theme: ThemeMode::Fixed(Theme::RedNight), ..Settings::default() });
    assert_eq!(ui.backlight_level(&s), 1, "a lower setting stays lower");
}

/// The Theme row cycles Auto -> Red Night -> Glass Cockpit -> Field Day ->
/// Auto (the dark ones together), the three Auto rows show only while it is
/// Auto, and the change is reported once, when editing ends, as (before,
/// after).
#[test]
fn theme_rows_come_and_go_and_changes_are_reported_once() {
    let s = three();
    let mut ui = Ui::default();
    assert_eq!(ui.settings().theme, ThemeMode::Fixed(Theme::GlassCockpit), "the default");
    assert_eq!(ui.effective_theme(&s), Theme::GlassCockpit);
    ui.button(Button::Up, &s);
    assert_eq!(ui.page(), Page::Settings);
    ui.button(Button::Select, &s);
    assert_eq!(ui.rows().len(), 6);
    ui.button(Button::Select, &s);
    assert_eq!(ui.settings().theme, ThemeMode::Fixed(Theme::FieldDay));
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay, "applies at once");
    ui.button(Button::Select, &s);
    assert_eq!(ui.settings().theme, ThemeMode::Auto);
    assert_eq!(
        ui.rows(),
        &[
            SettingsRow::Theme,
            SettingsRow::DayTheme,
            SettingsRow::NightTheme,
            SettingsRow::SwitchAt,
            SettingsRow::Brightness,
            SettingsRow::Buzzer,
            SettingsRow::HubMoved,
            SettingsRow::Alarms,
            SettingsRow::NodesAtHub
        ]
    );
    assert_eq!(ui.take_settings_change(), None, "nothing until editing ends");
    ui.button(Button::Down, &s);
    assert_eq!(ui.settings_row_kind(), SettingsRow::DayTheme);
    ui.button(Button::Select, &s);
    assert_eq!(ui.settings().day_theme, Theme::RedNight, "Field Day -> Red Night");
    ui.button(Button::Down, &s);
    ui.button(Button::Down, &s);
    assert_eq!(ui.settings_row_kind(), SettingsRow::SwitchAt);
    ui.button(Button::Select, &s);
    assert_eq!(ui.settings().switch_at, SwitchAt::Sunrise);
    for _ in 0..10 {
        ui.button(Button::Down, &s);
    }
    assert_eq!(ui.settings_row(), 8, "no wrap: the cursor stops on the last row");
    ui.button(Button::Back, &s);
    let (before, after) = ui.take_settings_change().expect("a change");
    assert_eq!(before, Settings::default());
    assert_eq!(after.theme, ThemeMode::Auto);
    assert_eq!(after.day_theme, Theme::RedNight);
    assert_eq!(ui.take_settings_change(), None, "taken once");
    // An edit that ends where it began reports nothing; the idle timeout
    // ends an edit like Back.
    ui.button(Button::Select, &s);
    for want in [
        ThemeMode::Fixed(Theme::RedNight),
        ThemeMode::Fixed(Theme::GlassCockpit),
        ThemeMode::Fixed(Theme::FieldDay),
        ThemeMode::Auto,
    ] {
        ui.button(Button::Select, &s);
        assert_eq!(ui.settings().theme, want);
    }
    ui.tick(s.now_ms + IDLE_MS);
    assert_eq!(ui.mode(), Mode::Browse);
    assert_eq!(ui.take_settings_change(), None);
    ui.button(Button::Select, &s);
    ui.button(Button::Select, &s); // Auto -> Red Night: rows shrink
    assert_eq!(ui.rows().len(), 6);
    ui.tick(s.now_ms + IDLE_MS);
    assert_eq!(ui.take_settings_change().map(|c| c.1.theme), Some(ThemeMode::Fixed(Theme::RedNight)));
    // Settings loaded by the hub are not reported back to it.
    ui.set_settings(Settings { theme: ThemeMode::Fixed(Theme::FieldDay), ..Settings::default() });
    assert_eq!(ui.take_settings_change(), None);
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay);
}

/// Auto by the sun at the hub's position, Glass Cockpit before anything is
/// known (fine by day and night), the last position kept when the fix
/// goes, the last verdict kept when time goes.
#[test]
fn auto_theme_follows_the_sun() {
    let mut s = three();
    let ui = Ui::new(Settings { theme: ThemeMode::Auto, ..Settings::default() });
    assert_eq!(ui.effective_theme(&s), Theme::GlassCockpit, "nothing known: Glass Cockpit");
    assert_eq!(ui.auto_status(&s), AutoStatus::Waiting { day: None });
    // Philadelphia, 2026-11-18 16:00 UTC (11:00 local): day.
    s.position = Some((39.95, -75.16));
    s.utc_ms = Some(1_795_017_600_000);
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay);
    let AutoStatus::Sun { day: true, next_utc_ms: Some(next), stale_position: false } = ui.auto_status(&s) else {
        panic!("{:?}", ui.auto_status(&s))
    };
    // Civil dusk 22:11 UTC (US Naval Observatory).
    let dusk = 1_795_017_600_000 + (6 * 60 + 11) * 60_000;
    assert!(next.abs_diff(dusk) < 120_000, "{next} vs {dusk}");
    // 23:00 UTC: night. The fix is gone but its position is remembered.
    s.position = None;
    s.utc_ms = Some(1_795_017_600_000 + 7 * 3_600_000);
    assert_eq!(ui.effective_theme(&s), Theme::RedNight);
    assert!(matches!(ui.auto_status(&s), AutoStatus::Sun { day: false, stale_position: true, .. }));
    // No time either: keep the last verdict.
    s.utc_ms = None;
    assert_eq!(ui.effective_theme(&s), Theme::RedNight);
    assert_eq!(ui.auto_status(&s), AutoStatus::Waiting { day: Some(false) });
    // Sunrise & sunset instead of dawn & dusk: 21:55 UTC is after sunset
    // (21:42) but before dusk (22:11).
    s.utc_ms = Some(1_795_017_600_000 + (5 * 60 + 55) * 60_000);
    s.position = Some((39.95, -75.16));
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay);
    let ui = Ui::new(Settings { theme: ThemeMode::Auto, switch_at: SwitchAt::Sunrise, ..Settings::default() });
    assert_eq!(ui.effective_theme(&s), Theme::RedNight);
}

/// Once observing, Auto holds a due switch back while the operator is
/// busy, switches when clear (saying so), and freezes while GPS may be
/// faked.
#[test]
fn auto_switch_waits_for_a_quiet_moment() {
    let day_ms = 1_795_017_600_000u64; // 16:00 UTC, Philadelphia: day
    let night_ms = day_ms + 7 * 3_600_000;
    let mut s = three();
    s.position = Some((39.95, -75.16));
    s.utc_ms = Some(day_ms);
    let mut ui = Ui::new(Settings { theme: ThemeMode::Auto, ..Settings::default() });
    ui.observe(&s);
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay);
    // Night falls while the operator is choosing a drone: no switch.
    ui.button(Button::Select, &s);
    s.utc_ms = Some(night_ms);
    ui.observe(&s);
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay, "not while choosing");
    ui.button(Button::Back, &s);
    ui.observe(&s);
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay, "not within 2 min of a press");
    // Jamming: held back even after the quiet time.
    s.now_ms += AUTO_QUIET_MS;
    s.threats.jammed = true;
    ui.tick(s.now_ms);
    ui.observe(&s);
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay, "not during an acute warning");
    s.threats.jammed = false;
    // GPS may be faked: frozen.
    s.gps.integrity = GpsIntegrity::Spoofing;
    ui.observe(&s);
    assert_eq!(ui.effective_theme(&s), Theme::FieldDay, "frozen while GPS may be faked");
    s.gps.integrity = GpsIntegrity::Ok;
    ui.observe(&s);
    assert_eq!(ui.effective_theme(&s), Theme::RedNight, "switches once quiet");
    assert_eq!(ui.title_toast(), Some(TitleToast::ThemeSwitched { to: Theme::RedNight, why: "dusk" }));
    ui.tick(s.now_ms + THEME_TOAST_MS);
    assert_eq!(ui.title_toast(), None);
    // A standing warning never holds it back for good.
    let mut ui = Ui::new(Settings { theme: ThemeMode::Auto, ..Settings::default() });
    let mut s2 = s.clone();
    s2.utc_ms = Some(day_ms);
    ui.observe(&s2);
    s2.utc_ms = Some(night_ms);
    s2.threats.jammed = true;
    ui.observe(&s2);
    assert_eq!(ui.effective_theme(&s2), Theme::FieldDay);
    s2.now_ms += AUTO_DEFER_MAX_MS;
    ui.observe(&s2);
    assert_eq!(ui.effective_theme(&s2), Theme::RedNight, "at most AUTO_DEFER_MAX_MS");
}

#[test]
fn gps_integrity_badges() {
    assert_eq!(GpsIntegrity::Ok.badge(), None);
    assert_eq!(GpsIntegrity::Spoofing.badge(), Some("GPS FAKE"));
    assert_eq!(GpsIntegrity::SuspectJamming.badge(), Some("GPS JAM?"));
}

#[test]
fn pick_mode_and_settings_time_out_and_detail_later() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Select, &s);
    ui.tick(s.now_ms + IDLE_MS - 1);
    assert_eq!(ui.mode(), Mode::Pick);
    ui.tick(s.now_ms + IDLE_MS);
    assert_eq!(ui.mode(), Mode::Browse, "the radar readout goes back to 'nearest'");

    let mut ui = Ui::default();
    ui.button(Button::Select, &s);
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s);
    ui.tick(s.now_ms + 3 * IDLE_MS);
    assert_eq!(ui.mode(), Mode::Detail, "someone watching one drone is not kicked out at once");
    ui.tick(s.now_ms + DETAIL_IDLE_MS);
    assert_eq!(ui.mode(), Mode::Browse);
    ui.button(Button::Select, &s);
    assert_eq!(ui.cursor(), Some(12), "the cursor is kept for next time");
}

#[test]
fn page_toast_shows_briefly() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Down, &s);
    assert!(ui.page_toast());
    ui.tick(s.now_ms + PAGE_TOAST_MS);
    assert!(!ui.page_toast());
}

// --- alerts --------------------------------------------------------------

#[test]
fn alert_expires_and_beeps_once() {
    let mut ui = Ui::default();
    ui.new_drone(11, 1_000);
    assert_eq!(ui.alert().map(|a| a.key), Some(11));
    assert_eq!(ui.take_buzz(), Some(Buzz::NewDrone));
    assert_eq!(ui.take_buzz(), None, "handed out once");
    ui.tick(1_000 + ALERT_MS - 1);
    assert!(ui.alert().is_some());
    ui.tick(1_000 + ALERT_MS);
    assert!(ui.alert().is_none());
}

#[test]
fn alerts_stack_and_buzz_is_rate_limited() {
    let mut ui = Ui::default();
    ui.new_drone(1, 1_000);
    assert!(ui.take_buzz().is_some());
    ui.new_drone(2, 1_500);
    ui.new_drone(3, 2_000);
    let a = ui.alert().unwrap().clone();
    assert_eq!((a.key, a.more, a.keys.as_slice()), (3, 2, &[1, 2, 3][..]), "all kept, others counted");
    assert_eq!(a.until_ms, 2_000 + ALERT_MS, "timer restarts");
    assert_eq!(ui.take_buzz(), None, "no beep within BUZZ_GAP_MS");
    ui.new_drone(4, 1_000 + BUZZ_GAP_MS);
    assert!(ui.take_buzz().is_some());
}

/// The banner shows the nearest (or a seriously flagged) new drone, and a
/// close one stays until a key is pressed.
#[test]
fn banner_shows_the_nearest_new_drone_and_close_ones_stay() {
    let s = three(); // 10 at 600 m, 11 at 50 m, 12 at 300 m
    let mut ui = Ui::default();
    ui.new_drone(10, s.now_ms);
    ui.new_drone(12, s.now_ms + 100);
    assert_eq!(ui.alert_drone(&s).map(|d| d.key), Some(12), "nearest of the new ones");
    ui.observe(&s);
    assert!(ui.alert().unwrap().sticky, "within 300 m: stays");
    ui.tick(s.now_ms + 10 * ALERT_MS);
    assert!(ui.alert().is_some());
    ui.button(Button::Up, &s);
    assert!(ui.alert().is_none(), "any press acknowledges it");
    // A far one auto-hides.
    let mut ui = Ui::default();
    ui.new_drone(10, s.now_ms);
    ui.observe(&s);
    assert!(!ui.alert().unwrap().sticky);
    ui.tick(s.now_ms + ALERT_MS);
    assert!(ui.alert().is_none());
    // Flagged at the worst level: shown first, and stays.
    let mut f = s.clone();
    let mut fl = FlagsView::default();
    let _ = fl.rows.push(FlagRow { level: 3, drone: Some(10), ..FlagRow::default() });
    f.flags = Some(fl);
    let mut ui = Ui::default();
    ui.new_drone(10, s.now_ms);
    ui.new_drone(11, s.now_ms);
    assert_eq!(ui.alert_drone(&f).map(|d| d.key), Some(10));
    ui.button(Button::Select, &f);
    assert_eq!(ui.cursor(), Some(10), "Select opens the drone the banner shows");
}

/// After the operator dismisses a banner, a swarm only counts up a NEW
/// chip for a while; every new drone is marked NEW for a minute.
#[test]
fn swarm_lockout_and_new_marks() {
    let s = three();
    let mut ui = Ui::default();
    ui.new_drone(10, s.now_ms);
    assert!(ui.is_new(10));
    ui.button(Button::Up, &s); // dismiss
    ui.new_drone(11, s.now_ms + 1_000);
    ui.new_drone(12, s.now_ms + 2_000);
    assert!(ui.alert().is_none(), "no new banner right after a dismissal");
    assert_eq!(ui.new_chip(), Some(2));
    ui.tick(s.now_ms + SWARM_MS + 1);
    ui.new_drone(13, s.now_ms + SWARM_MS + 1);
    assert!(ui.alert().is_some(), "the lockout ends");
    ui.tick(s.now_ms + NEW_MS);
    assert!(!ui.is_new(10), "NEW lasts a minute");
    assert!(ui.is_new(13));
}

/// A press that began before the banner appeared was meant for the page.
#[test]
fn press_begun_before_the_banner_is_ignored() {
    let s = three();
    let mut ui = Ui::default();
    ui.new_drone(12, s.now_ms);
    ui.button_at(Button::Select, &s, s.now_ms - 300);
    assert!(ui.alert().is_some(), "ignored: the banner stays");
    assert_eq!(ui.mode(), Mode::Browse);
    ui.button_at(Button::Select, &s, s.now_ms);
    assert_eq!(ui.mode(), Mode::Detail);
}

/// Every way out of editing ends the edit, the banner's Select included.
#[test]
fn banner_select_while_editing_ends_the_edit() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Up, &s);
    ui.button(Button::Select, &s);
    ui.button(Button::Select, &s); // theme changed
    ui.new_drone(12, s.now_ms);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Detail);
    assert_eq!(ui.page(), Page::Radar);
    assert!(ui.take_settings_change().is_some(), "the change is handed to the hub");
}

#[test]
fn muted_buzzer_still_shows_banner() {
    let mut ui = Ui::new(Settings {
        buzzer: false,
        ..Settings::default()
    });
    ui.new_drone(1, 0);
    assert!(ui.alert().is_some());
    assert_eq!(ui.take_buzz(), None);
}

#[test]
fn select_on_alert_opens_that_drone_and_other_keys_only_dismiss() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Down, &s);
    ui.button(Button::Down, &s); // Signals page
    ui.new_drone(12, s.now_ms);
    ui.button(Button::Select, &s);
    assert!(ui.alert().is_none());
    assert_eq!(ui.mode(), Mode::Detail);
    assert_eq!(ui.cursor(), Some(12));
    ui.button(Button::Back, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    assert_eq!(ui.page(), Page::Radar, "returns to a page that shows drones");

    let mut ui = Ui::default();
    ui.new_drone(12, s.now_ms);
    ui.button(Button::Down, &s);
    assert!(ui.alert().is_none());
    assert_eq!(ui.page(), Page::Radar, "the dismissing press did not flip the page");
}

// --- touch on the page -----------------------------------------------------

#[test]
fn taps_open_drones_choose_settings_and_respect_the_banner() {
    let s = three();
    let mut ui = Ui::default();
    // A drone (marker, card or row) opens its detail.
    ui.tap(Tap::Drone(12), &s, s.now_ms);
    assert_eq!((ui.mode(), ui.cursor()), (Mode::Detail, Some(12)));
    assert_eq!(ui.tapped(), Some(Tap::Drone(12)), "highlighted briefly");
    // Detail: a tap on the page does nothing but the hint.
    ui.tap(Tap::Drone(10), &s, s.now_ms);
    assert_eq!(ui.cursor(), Some(12));
    assert_eq!(ui.title_toast(), Some(TitleToast::TapHint));
    ui.button(Button::Back, &s);
    // Empty space: the hint only.
    ui.tick(s.now_ms + TAP_HINT_MS);
    ui.tap(Tap::Nothing, &s, s.now_ms);
    assert_eq!(ui.mode(), Mode::Browse);
    assert_eq!(ui.title_toast(), Some(TitleToast::TapHint));

    // Settings: the first tap chooses the row, the second changes it;
    // destructive actions still confirm.
    let mut ui = Ui::default();
    ui.button(Button::Up, &s);
    let mut s2 = s.clone();
    s2.now_ms += 1;
    ui.tap(Tap::SettingsRow(1), &s2, s2.now_ms);
    assert_eq!((ui.mode(), ui.settings_row()), (Mode::Edit, 1));
    assert_eq!(ui.settings().brightness, 3, "never acts on the first tap");
    ui.tap(Tap::SettingsRow(5), &s2, s2.now_ms);
    assert_eq!(ui.settings_row_kind(), SettingsRow::NodesAtHub);
    ui.tap(Tap::SettingsRow(5), &s2, s2.now_ms);
    assert_eq!(ui.take_request(), None, "armed only");
    ui.tap(Tap::SettingsRow(5), &s2, s2.now_ms);
    assert_eq!(ui.take_request(), Some(Request::NodesAtHub));

    // The banner: a tap on it opens its drone, anywhere else dismisses.
    let mut ui = Ui::default();
    ui.new_drone(10, s.now_ms);
    ui.tap(Tap::Drone(11), &s, s.now_ms);
    assert!(ui.alert().is_none());
    assert_eq!(ui.mode(), Mode::Browse, "a tap off the banner only dismisses it");
    let mut ui = Ui::default();
    ui.new_drone(10, s.now_ms);
    ui.tap(Tap::Banner, &s, s.now_ms);
    assert_eq!((ui.mode(), ui.cursor()), (Mode::Detail, Some(10)));

    // A touch that began before the screen changed is ignored.
    let mut ui = Ui::default();
    ui.button(Button::Down, &s); // page flip at now
    ui.tap(Tap::Drone(12), &s, s.now_ms - 100);
    assert_eq!(ui.mode(), Mode::Browse);
}

#[test]
fn flags_choose_a_row_and_open_its_drone() {
    let mut s = three();
    let mut fl = FlagsView::default();
    let _ = fl.rows.push(FlagRow { level: 1, secs: 5, drone: Some(10), ..FlagRow::default() });
    let _ = fl.rows.push(FlagRow { level: 3, secs: 9, drone: Some(12), ..FlagRow::default() });
    let _ = fl.rows.push(FlagRow { level: 2, secs: 9, aircraft: true, ..FlagRow::default() });
    s.flags = Some(fl);
    let mut ui = Ui::default();
    ui.button(Button::Down, &s);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Flags);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Scroll);
    // Listed worst first: 12 (serious), the plane, then 10.
    ui.button(Button::Select, &s);
    assert_eq!((ui.mode(), ui.cursor()), (Mode::Detail, Some(12)));
    ui.button(Button::Back, &s);
    assert_eq!((ui.mode(), ui.page()), (Mode::Browse, Page::Flags), "back to the flags");
    ui.button(Button::Select, &s);
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Scroll, "a plane has no detail");
    ui.button(Button::Down, &s);
    ui.button(Button::Down, &s);
    assert_eq!(ui.scroll(), 2, "stops at the last row");
    ui.tap(Tap::Flag(0), &s, s.now_ms);
    assert_eq!(ui.cursor(), Some(10), "a tapped row opens its drone");
}

#[test]
fn signals_scroll_only_when_rows_are_hidden() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Down, &s);
    ui.button(Button::Down, &s);
    ui.note_overflow(Page::Signals, true, 4);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Scroll);
    for _ in 0..9 {
        ui.button(Button::Down, &s);
    }
    assert_eq!(ui.scroll(), 4);
    ui.button(Button::Select, &s);
    assert_eq!((ui.mode(), ui.page()), (Mode::Browse, Page::Signals), "a tap leaves scrolling");
}

#[test]
fn warnings_in_priority_order() {
    let mut s = three();
    assert!(s.warnings().is_empty());
    s.nodes.push(NodeStatus { kind: NodeKind::Wifi5, alive: false, frames_per_s: None, rid_found: 0, drops: 0, uptime_s: 0, silent_s: Some(90) }).unwrap();
    s.threats.flood_per_s = 9;
    s.threats.jammed = true;
    let w = s.warnings();
    assert_eq!(&w[..], &[Warning::Jamming, Warning::IdFlood, Warning::NodesDown { first: NodeKind::Wifi5, n: 1 }]);
    assert_eq!(w[2].text(&s).0.as_str(), "5 GHz RECEIVER DOWN");
    for n in s.nodes.iter_mut() {
        n.alive = false;
    }
    assert_eq!(s.warnings()[0], Warning::NotListening { alive: 0, total: 2 });
    assert_eq!(s.warnings()[0].text(&s).1.as_str(), "0 of 2 receivers responding");
    s.nodes.clear();
    s.gps.fix = Fix::None;
    let w = s.warnings();
    assert_eq!(w[0], Warning::NotListening { alive: 0, total: 0 });
    assert_eq!(*w.last().unwrap(), Warning::NoPosition);
    assert_eq!(Page::Signals.position(false), (3, 5));
    assert_eq!(Page::Signals.position(true), (4, 6));
}

// --- buttons -------------------------------------------------------------

#[test]
fn decoder_short_and_long_select() {
    let mut k = ButtonDecoder::new();
    assert_eq!(k.poll(false, false, true, 0), None, "nothing on press");
    assert_eq!(k.poll(false, false, true, 300), None);
    assert_eq!(
        k.poll(false, false, false, 310),
        Some(Button::Select),
        "short press on release"
    );

    assert_eq!(k.poll(false, false, true, 1_000), None);
    assert_eq!(
        k.poll(false, false, true, 1_000 + ButtonDecoder::LONG_MS),
        Some(Button::Back)
    );
    assert_eq!(k.poll(false, false, true, 2_500), None, "Back fires once");
    assert_eq!(
        k.poll(false, false, false, 2_600),
        None,
        "no Select after a long press"
    );
}

#[test]
fn decoder_up_down_repeat() {
    let mut k = ButtonDecoder::new();
    assert_eq!(k.poll(true, false, false, 0), Some(Button::Up));
    assert_eq!(k.poll(true, false, false, 100), None);
    assert_eq!(
        k.poll(true, false, false, ButtonDecoder::REPEAT_DELAY_MS),
        Some(Button::Up)
    );
    assert_eq!(
        k.poll(true, false, false, ButtonDecoder::REPEAT_DELAY_MS + 50),
        None
    );
    assert_eq!(
        k.poll(
            true,
            false,
            false,
            ButtonDecoder::REPEAT_DELAY_MS + ButtonDecoder::REPEAT_MS
        ),
        Some(Button::Up)
    );
    assert_eq!(k.poll(false, false, false, 2_000), None);
    assert_eq!(k.poll(false, true, false, 2_010), Some(Button::Down));
}

// --- formatting ----------------------------------------------------------

#[test]
fn distances_round_honestly() {
    let f = |m: f32| {
        let (n, u) = draw::distance(m);
        std::format!("{n}{u}")
    };
    assert_eq!(f(7.4), "7m");
    assert_eq!(f(99.4), "99m");
    assert_eq!(f(323.0), "320m");
    assert_eq!(f(994.0), "990m");
    assert_eq!(f(996.0), "1.0km");
    assert_eq!(f(1_420.0), "1.4km");
    assert_eq!(f(12_600.0), "13km");
}

#[test]
fn compass_and_degrees() {
    assert_eq!(draw::compass(0.0), "N");
    assert_eq!(draw::compass(22.0), "N");
    assert_eq!(draw::compass(23.0), "NE");
    assert_eq!(draw::compass(359.0), "N");
    assert_eq!(draw::compass(200.0), "S");
    assert_eq!(draw::degrees(359.7).as_str(), "000\u{b0}");
    assert_eq!(draw::degrees(47.0).as_str(), "047\u{b0}");
}

#[test]
fn short_ids_keep_the_distinguishing_tail() {
    let mut d = DroneView {
        id_kind: IdKind::Serial,
        id: String::try_from("1581F5FKD229400D").unwrap(),
        ..DroneView::default()
    };
    assert_eq!(draw::short_id(&d, 9).as_str(), "..229400D");
    d.id = String::try_from("SKY04X118").unwrap();
    assert_eq!(draw::short_id(&d, 9).as_str(), "SKY04X118");
    d.id_kind = IdKind::Mac;
    d.id = String::try_from("c0:11:22:33:44:55").unwrap();
    assert_eq!(draw::short_id(&d, 9).as_str(), "MAC334455");
}

#[test]
fn ages() {
    assert_eq!(draw::age(500).as_str(), "now");
    assert_eq!(draw::age(12_000).as_str(), "12 s");
    assert_eq!(draw::age(185_000).as_str(), "3 min", "never m for minutes");
    assert_eq!(draw::uptime(3_725).as_str(), "1 h 02 min");
    assert_eq!(draw::distance_short(1_400.0).as_str(), "1.4 km");
    assert_eq!(draw::metres(Some(85.0)).as_str(), "85 m");
    assert_eq!(left(245).as_str(), "4:05");
    assert_eq!(left(7_140).as_str(), "1 h 59 min");
}

#[test]
fn caveat_lines() {
    assert_eq!(Caveats::default().lines(), [None, None, None, None]);
    let mut c = Caveats { key: EvidenceKey::Test, ..Caveats::default() };
    let l = c.lines();
    assert!(l[0].is_none());
    assert_eq!(l[1].as_deref(), Some("Test key: log not tamper-proof"));
    c.bench = true;
    c.link_open = true;
    assert_eq!(c.lines()[1].as_deref(), Some("Test key: log not tamper-proof  Bench build  Nodes unsecured"));
    assert_eq!(
        &c.notes()[..],
        &["Log not tamper-proof (test key)", "Nodes unsecured: reports can be faked", "Bench build"].map(|t| String::<48>::try_from(t).unwrap())[..]
    );
    assert!(c.faults().is_empty(), "standing notes are not faults");
    c.key = EvidenceKey::Development;
    assert_eq!(c.lines()[1].as_deref(), Some("Dev key: not evidence-grade  Bench build  Nodes unsecured"));
    c = Caveats { evidence: Some(EvidenceStop { why: EvidenceStopWhy::NoCard, waiting: 12, dropped: 0 }), ..Caveats::default() };
    assert_eq!(c.lines()[0].as_deref(), Some("NO EVIDENCE (no SD): 12 waiting"));
    assert_eq!(c.faults().first(), c.lines()[0].as_ref());
    assert_eq!(c.faults().len(), 1);
    assert!(c.lines()[1].is_none());
    c.evidence = Some(EvidenceStop { why: EvidenceStopWhy::FileError, waiting: 3, dropped: 9 });
    assert_eq!(c.lines()[0].as_deref(), Some("NO EVIDENCE (file error): 3 waiting, 9 dropped"));
    // Silenced alarms and the collection switches stay on screen.
    c = Caveats {
        silenced: Silenced { muted: 2, mute_left_s: 7_140, snoozed: 1, snooze_left_s: 245, site: 3 },
        switches: Switches { control_ids: true, control_content: false, audio_clips: true },
        ..Caveats::default()
    };
    let l = c.lines();
    assert_eq!(l[2].as_deref(), Some("muted 2: 1 h 59 min  snoozed 1: 4:05  site 3"));
    assert_eq!(c.notes()[1].as_str(), "1 snoozed, 4:05 left");
    assert_eq!(l[3].as_deref(), Some("ON: control IDs, audio clips"));
    // The longest of each still fits the panel's 52 columns.
    c.silenced = Silenced { muted: 255, mute_left_s: 7_200, snoozed: 255, snooze_left_s: 300, site: 255 };
    c.switches = Switches { control_ids: true, control_content: true, audio_clips: true };
    let l = c.lines();
    assert!(l[2].as_ref().unwrap().len() <= 52, "{:?}", l[2]);
    assert!(l[3].as_ref().unwrap().len() <= 52, "{:?}", l[3]);
}

/// The keyed link's line (docs/LINK-KEYS-DESIGN.md 4.4, 6.4): second in
/// priority, after the evidence state; with every line present, silenced
/// alarms and switches share the fourth.
#[test]
fn caveat_link_line() {
    let mut c = Caveats { link: LinkCaveats { unprovisioned: 2, ..LinkCaveats::default() }, ..Caveats::default() };
    assert_eq!(c.lines(), [Some("UNPROVISIONED NODE 2".try_into().unwrap()), None, None, None]);
    assert_eq!(c.faults().len(), 1, "a link problem with evidence fine is one fault bar");
    assert_eq!(c.faults()[0].as_str(), "UNPROVISIONED NODE 2");
    c.link.old_gateway = true;
    assert_eq!(c.lines()[0].as_deref(), Some("OLD FIRMWARE: gateway  UNPROVISIONED NODE 2"));
    c.link.old_nodes = 3;
    c.link.unprovisioned = 0;
    assert_eq!(c.lines()[0].as_deref(), Some("OLD FIRMWARE: gateway, 3 nodes"));
    c.link = LinkCaveats { old_nodes: 1, ..LinkCaveats::default() };
    assert_eq!(c.lines()[0].as_deref(), Some("OLD FIRMWARE: 1 node"));
    c.link = LinkCaveats { bench_gateway: true, ..LinkCaveats::default() };
    assert_eq!(c.lines()[0].as_deref(), Some("BENCH GATEWAY"));
    for (k, t) in [
        (KitProblem::Missing, "NO KIT KEYS: link closed"),
        (KitProblem::Damaged, "KIT RECORDS DAMAGED: link closed"),
        (KitProblem::OtherKit, "ROSTER FROM OTHER KIT"),
        (KitProblem::BootCounter, "NO BOOT COUNTER: link closed"),
    ] {
        c.link = LinkCaveats { kit: Some(k), ..LinkCaveats::default() };
        assert_eq!(c.lines()[0].as_deref(), Some(t));
    }
    // Under the evidence line, above the build marks.
    c.evidence = Some(EvidenceStop { why: EvidenceStopWhy::NoCard, waiting: 1, dropped: 0 });
    c.bench = true;
    let l = c.lines();
    assert_eq!(l[0].as_deref(), Some("NO EVIDENCE (no SD): 1 waiting"));
    assert_eq!(l[1].as_deref(), Some("NO BOOT COUNTER: link closed"));
    assert_eq!(l[2].as_deref(), Some("Bench build"));
    assert!(l[3].is_none());
    // The wide screen: both are faults (a filled bar each), evidence first;
    // the bench mark stays a note.
    let f = c.faults();
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].as_str(), "NO EVIDENCE (no SD): 1 waiting");
    assert_eq!(f[1].as_str(), "NO BOOT COUNTER: link closed");
    assert_eq!(c.notes().len(), 1);
    // All five: silenced and switches share the last line.
    c.silenced = Silenced { muted: 1, mute_left_s: 60, ..Silenced::default() };
    c.switches = Switches { control_ids: true, ..Switches::default() };
    let l = c.lines();
    assert_eq!(l[3].as_deref(), Some("muted 1: 1:00  ON: control IDs"));
    // Old gateway, a few old nodes and unprovisioned ones fit the panel's
    // 52 columns; past that the renderer cuts the tail (the count of
    // unprovisioned nodes), never the start.
    c.link = LinkCaveats { kit: None, old_gateway: true, old_nodes: 9, unprovisioned: 9, bench_gateway: false };
    let l = c.lines();
    assert!(l[1].as_ref().unwrap().len() <= 52, "{:?}", l[1]);
    c.link = LinkCaveats { kit: None, old_gateway: true, old_nodes: 16, unprovisioned: 16, bench_gateway: false };
    assert!(c.lines()[1].as_ref().unwrap().starts_with("OLD FIRMWARE: gateway, 16 nodes  UNPROVISIONED NODE"));
}
