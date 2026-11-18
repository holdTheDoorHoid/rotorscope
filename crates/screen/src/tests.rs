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
    ui.button(Button::Back, &s);
    assert_eq!(ui.mode(), Mode::Pick);
    assert_eq!(
        ui.cursor(),
        Some(11),
        "back keeps the drone under the cursor"
    );
    ui.button(Button::Back, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    assert_eq!(ui.cursor(), None);
    assert_eq!(ui.page(), Page::Radar);
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
fn signals_and_health_select_is_inert_and_settings_edit() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Down, &s);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Signals);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Health);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Browse);

    ui.button(Button::Down, &s);
    assert_eq!(ui.page(), Page::Settings);
    ui.button(Button::Select, &s);
    assert_eq!(ui.mode(), Mode::Edit);
    ui.button(Button::Select, &s); // theme
    assert_eq!(ui.settings().theme, Theme::Day);
    ui.button(Button::Down, &s);
    for _ in 0..3 {
        ui.button(Button::Select, &s); // brightness 3 -> 4 -> 5 -> 1
    }
    assert_eq!(ui.settings().brightness, 1);
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s);
    assert!(!ui.settings().buzzer);
    ui.button(Button::Down, &s);
    assert_eq!(ui.done_row(), None);
    ui.button(Button::Select, &s); // hub moved
    assert_eq!(ui.done_row(), Some(3));
    assert_eq!(ui.take_request(), Some(Request::HubMoved));
    assert_eq!(ui.take_request(), None, "taken once");
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s); // alarms: nothing sounding, nothing muted
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
    // Then only the jammer: Snooze, a separate press.
    loud.threats.mutable = 0;
    ui.button(Button::Select, &loud);
    assert_eq!(ui.take_request(), Some(Request::SnoozeAlarms { seen_ms: s.now_ms }));
    let mut quiet = s.clone();
    quiet.threats.muted = 2;
    quiet.caveats.silenced.muted = 1;
    quiet.caveats.silenced.snoozed = 1;
    ui.button(Button::Select, &quiet);
    assert_eq!(ui.take_request(), Some(Request::UnmuteAlarms));
    // Only the site file's mutes: the row cannot drop those.
    quiet.caveats.silenced = Silenced { site: 3, ..Silenced::default() };
    ui.button(Button::Select, &quiet);
    assert_eq!(ui.take_request(), None);
    ui.button(Button::Down, &s);
    ui.button(Button::Select, &s); // nodes at hub
    assert_eq!(ui.take_request(), Some(Request::NodesAtHub));
    ui.button(Button::Down, &s);
    assert_eq!(ui.settings_row(), 0, "rows wrap");
    ui.button(Button::Back, &s);
    assert_eq!(ui.mode(), Mode::Browse);
    assert_eq!(ui.page(), Page::Settings);
    assert_eq!(ui.done_row(), None, "cleared on leaving");
}

#[test]
fn gps_integrity_badges() {
    assert_eq!(GpsIntegrity::Ok.badge(), None);
    assert_eq!(GpsIntegrity::Spoofing.badge(), Some("GPS FAKE"));
    assert_eq!(GpsIntegrity::SuspectJamming.badge(), Some("GPS JAM?"));
}

#[test]
fn pick_mode_and_settings_time_out_but_detail_does_not() {
    let s = three();
    let mut ui = Ui::default();
    ui.button(Button::Select, &s);
    ui.tick(s.now_ms + IDLE_MS - 1);
    assert_eq!(ui.mode(), Mode::Pick);
    ui.tick(s.now_ms + IDLE_MS);
    assert_eq!(
        ui.mode(),
        Mode::Browse,
        "the radar readout goes back to 'nearest'"
    );

    let mut ui = Ui::default();
    ui.button(Button::Select, &s);
    ui.button(Button::Select, &s);
    ui.tick(s.now_ms + 10 * IDLE_MS);
    assert_eq!(
        ui.mode(),
        Mode::Detail,
        "someone watching one drone is not kicked out"
    );
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
    let a = *ui.alert().unwrap();
    assert_eq!((a.key, a.more), (3, 2), "newest shown, others counted");
    assert_eq!(a.until_ms, 2_000 + ALERT_MS, "timer restarts");
    assert_eq!(ui.take_buzz(), None, "no beep within BUZZ_GAP_MS");
    ui.new_drone(4, 1_000 + BUZZ_GAP_MS);
    assert!(ui.take_buzz().is_some());
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
    ui.button(Button::Down, &s); // Health page
    ui.new_drone(12, s.now_ms);
    ui.button(Button::Select, &s);
    assert!(ui.alert().is_none());
    assert_eq!(ui.mode(), Mode::Detail);
    assert_eq!(ui.cursor(), Some(12));
    ui.button(Button::Back, &s);
    assert_eq!(ui.mode(), Mode::Pick);
    assert_eq!(
        ui.page(),
        Page::Radar,
        "returns to a page that shows drones"
    );

    let mut ui = Ui::default();
    ui.new_drone(12, s.now_ms);
    ui.button(Button::Down, &s);
    assert!(ui.alert().is_none());
    assert_eq!(
        ui.page(),
        Page::Radar,
        "the dismissing press did not flip the page"
    );
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
    assert_eq!(draw::age(12_000).as_str(), "12s");
    assert_eq!(draw::age(185_000).as_str(), "3m");
    assert_eq!(draw::uptime(3_725).as_str(), "1h02m");
}

#[test]
fn caveat_lines() {
    assert_eq!(Caveats::default().lines(), [None, None, None, None]);
    let mut c = Caveats { key: EvidenceKey::Test, ..Caveats::default() };
    let l = c.lines();
    assert!(l[0].is_none());
    assert_eq!(l[1].as_deref(), Some("TEST KEY: log not evidence"));
    c.bench = true;
    c.link_open = true;
    assert_eq!(c.lines()[1].as_deref(), Some("TEST KEY: log not evidence  BENCH BUILD  OPEN LINK"));
    c.key = EvidenceKey::Development;
    assert_eq!(c.lines()[1].as_deref(), Some("DEV KEY: not evidence-grade  BENCH BUILD  OPEN LINK"));
    c = Caveats { evidence: Some(EvidenceStop { why: EvidenceStopWhy::NoCard, waiting: 12, dropped: 0 }), ..Caveats::default() };
    assert_eq!(c.lines()[0].as_deref(), Some("NO EVIDENCE (no SD): 12 waiting"));
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
    assert_eq!(l[2].as_deref(), Some("MUTED 2 1h59  SNOOZED 1 4m05  SITE MUTES 3"));
    assert_eq!(l[3].as_deref(), Some("ON: control IDs, audio clips"));
    // The longest of each still fits the panel's 52 columns.
    c.silenced = Silenced { muted: 255, mute_left_s: 7_200, snoozed: 255, snooze_left_s: 300, site: 255 };
    c.switches = Switches { control_ids: true, control_content: true, audio_clips: true };
    let l = c.lines();
    assert!(l[2].as_ref().unwrap().len() <= 52, "{:?}", l[2]);
    assert!(l[3].as_ref().unwrap().len() <= 52, "{:?}", l[3]);
}
