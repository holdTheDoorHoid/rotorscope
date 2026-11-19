//! The screen's own state: which page is showing, the cursor, the alert
//! banner and the user's settings. Driven by [`Button`] events and
//! [`Ui::tick`] (and [`Ui::observe`] once a frame); touches on the page
//! arrive as [`Ui::tap`]; read by [`crate::render`] and `wide`.

use heapless::Vec;

use crate::model::{DroneView, GpsIntegrity, UiState};

/// A button event, after debouncing and long-press detection
/// (see [`ButtonDecoder`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Up,
    Down,
    /// Short press of Select.
    Select,
    /// Long press of Select.
    Back,
}

/// What a touch on the page (not on a key) landed on, found by the layout
/// that drew it (`wide::tap_target`). [`Ui::tap`] decides what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tap {
    /// A drone: its radar marker, the radar readout card, or its list row.
    Drone(u32),
    /// Row `n` of the FLAGS page (as listed, worst first).
    Flag(usize),
    /// Row `n` of the settings page (an index into [`Ui::rows`]).
    SettingsRow(usize),
    /// The new-drone banner itself.
    Banner,
    /// Somewhere that does nothing.
    Nothing,
}

/// Top-level pages. Up/Down flip between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Radar,
    List,
    /// Possible FAA rule violations (PLAN.md Phase 12). Only in the cycle
    /// when the hub fills [`UiState::flags`](crate::UiState::flags).
    Flags,
    /// RF emitters and Wi-Fi drones (no position, so not on the radar).
    Signals,
    Health,
    Settings,
}

impl Page {
    pub const ALL: [Page; 6] = [Page::Radar, Page::List, Page::Flags, Page::Signals, Page::Health, Page::Settings];

    pub const fn title(self) -> &'static str {
        match self {
            Page::Radar => "RADAR",
            Page::List => "DRONE LIST",
            Page::Flags => "FLAGS",
            Page::Signals => "SIGNALS",
            Page::Health => "HEALTH",
            Page::Settings => "SETTINGS",
        }
    }

    /// The page's name in a sentence ("back to drone list").
    pub const fn name(self) -> &'static str {
        match self {
            Page::Radar => "radar",
            Page::List => "drone list",
            Page::Flags => "flags",
            Page::Signals => "signals",
            Page::Health => "health",
            Page::Settings => "settings",
        }
    }

    fn index(self) -> usize {
        match self {
            Page::Radar => 0,
            Page::List => 1,
            Page::Flags => 2,
            Page::Signals => 3,
            Page::Health => 4,
            Page::Settings => 5,
        }
    }

    /// Where this page is in the cycle, 1-based, and how many pages there
    /// are (FLAGS counts only when shown): "3/6".
    pub fn position(self, flags: bool) -> (usize, usize) {
        let mut i = 0;
        for p in Self::ALL.iter().filter(|p| flags || **p != Page::Flags) {
            i += 1;
            if *p == self {
                break;
            }
        }
        (i, if flags { 6 } else { 5 })
    }

    /// The next page `delta` away; FLAGS is skipped when `flags` is false.
    fn step(self, delta: isize, flags: bool) -> Page {
        let n = Self::ALL.len() as isize;
        let next = Self::ALL[(self.index() as isize + delta).rem_euclid(n) as usize];
        if next == Page::Flags && !flags { next.step(delta, flags) } else { next }
    }
}

/// What the buttons are doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Up/Down change page; Select enters the page.
    Browse,
    /// Choosing a drone on the radar or list: Up/Down move the cursor,
    /// Select opens its detail.
    Pick,
    /// One drone's detail. Up/Down step through drones.
    Detail,
    /// Editing settings: Up/Down choose a row, Select changes it.
    Edit,
    /// SIGNALS: Up/Down scroll the rows the page could not show. FLAGS:
    /// Up/Down choose a row, Select opens that drone.
    Scroll,
}

/// A colour scheme (the look actually drawn). Every distinction is also
/// carried by shape or text, so any of them can be used for anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Dark, after airliner displays: cyan labels, white data, magenta for
    /// drones and the selection, amber cautions, green only for node health.
    /// The default, and Auto's choice while it knows neither place nor time
    /// (it works by day and by night).
    #[default]
    GlassCockpit,
    /// Dark ink on a light background, for direct sunlight.
    FieldDay,
    /// Pure red only, dim: keeps night vision. One hue, so alerts rely on
    /// outlines, inverse titles and shapes.
    RedNight,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::GlassCockpit, Theme::FieldDay, Theme::RedNight];

    /// The name on screen.
    pub const fn name(self) -> &'static str {
        match self {
            Theme::GlassCockpit => "Glass Cockpit",
            Theme::FieldDay => "Field Day",
            Theme::RedNight => "Red Night",
        }
    }

    /// The word in the site file (`theme red-night`).
    pub const fn word(self) -> &'static str {
        match self {
            Theme::GlassCockpit => "glass-cockpit",
            Theme::FieldDay => "field-day",
            Theme::RedNight => "red-night",
        }
    }

    pub fn from_word(w: &str) -> Option<Theme> {
        Self::ALL.into_iter().find(|t| t.word().eq_ignore_ascii_case(w))
    }

    /// The next one, wrapping round: Red Night -> Glass Cockpit -> Field
    /// Day, so stepping from one dark theme to the other never flashes the
    /// bright one in between.
    pub const fn next(self) -> Theme {
        match self {
            Theme::RedNight => Theme::GlassCockpit,
            Theme::GlassCockpit => Theme::FieldDay,
            Theme::FieldDay => Theme::RedNight,
        }
    }
}

/// The Theme setting: one look always, or switch by the sun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    /// The day theme while the sun is up, the night theme otherwise, from
    /// the hub's position and GPS time ([`crate::sun`]).
    Auto,
    Fixed(Theme),
}

impl Default for ThemeMode {
    fn default() -> Self {
        ThemeMode::Fixed(Theme::GlassCockpit)
    }
}

impl ThemeMode {
    /// The word in the site file (`theme auto`, `theme field-day`).
    pub const fn word(self) -> &'static str {
        match self {
            ThemeMode::Auto => "auto",
            ThemeMode::Fixed(t) => t.word(),
        }
    }

    /// Auto -> Red Night -> Glass Cockpit -> Field Day -> Auto: the two
    /// dark themes next to each other.
    const fn next(self) -> ThemeMode {
        match self {
            ThemeMode::Auto => ThemeMode::Fixed(Theme::RedNight),
            ThemeMode::Fixed(Theme::FieldDay) => ThemeMode::Auto,
            ThemeMode::Fixed(t) => ThemeMode::Fixed(t.next()),
        }
    }
}

/// When Auto switches between the day and the night theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SwitchAt {
    /// Sunrise and sunset (the sun's centre 0.833° below the horizon).
    Sunrise,
    /// Civil dawn and dusk (6° below): the day theme stays on through
    /// twilight, while there is still light to wash the screen out.
    #[default]
    Twilight,
}

impl SwitchAt {
    pub const fn name(self) -> &'static str {
        match self {
            SwitchAt::Sunrise => "Sunrise & sunset",
            SwitchAt::Twilight => "Dawn & dusk",
        }
    }
    /// The word in the site file (`theme-switch twilight`).
    pub const fn word(self) -> &'static str {
        match self {
            SwitchAt::Sunrise => "sunrise",
            SwitchAt::Twilight => "twilight",
        }
    }
    /// The moment Auto switches at, going to day (`true`) or to night.
    pub const fn moment(self, to_day: bool) -> &'static str {
        match (self, to_day) {
            (SwitchAt::Twilight, true) => "dawn",
            (SwitchAt::Twilight, false) => "dusk",
            (SwitchAt::Sunrise, true) => "sunrise",
            (SwitchAt::Sunrise, false) => "sunset",
        }
    }
}

/// Backlight per Brightness level 1..=5, percent.
pub const BRIGHTNESS_PCT: [u8; 5] = [10, 25, 45, 70, 100];
/// Red Night never lights the panel above this level (unless set lower).
pub const RED_NIGHT_MAX_BRIGHTNESS: u8 = 2;

/// The backlight percentage for a Brightness level (clamped to 1..=5).
pub const fn backlight_pct(level: u8) -> u8 {
    let i = if level < 1 { 0 } else if level > 5 { 4 } else { level - 1 };
    BRIGHTNESS_PCT[i as usize]
}

/// User settings the hub should apply (and may persist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemeMode,
    /// Used by Auto while the sun is up.
    pub day_theme: Theme,
    /// Used by Auto at night.
    pub night_theme: Theme,
    pub switch_at: SwitchAt,
    /// Backlight level 1..=5 ([`backlight_pct`]; see [`Ui::backlight_level`]
    /// for the Red Night cap).
    pub brightness: u8,
    /// Beep on a new drone (hubs with a buzzer).
    pub buzzer: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            day_theme: Theme::FieldDay,
            night_theme: Theme::RedNight,
            switch_at: SwitchAt::default(),
            brightness: 3,
            buzzer: true,
        }
    }
}

/// One row of the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    Theme,
    /// Only while Theme is Auto.
    DayTheme,
    /// Only while Theme is Auto.
    NightTheme,
    /// Only while Theme is Auto.
    SwitchAt,
    Brightness,
    /// Only on hubs with a buzzer ([`Ui::set_has_buzzer`]).
    Buzzer,
    /// "Hub location": "I moved the hub", or "Use this spot" while the hub
    /// waits for a decision on a new place.
    HubMoved,
    Alarms,
    NodesAtHub,
}

impl SettingsRow {
    /// An action (something the hub does once), not a setting: listed
    /// after the settings under ACTIONS.
    pub const fn is_action(self) -> bool {
        matches!(self, SettingsRow::HubMoved | SettingsRow::Alarms | SettingsRow::NodesAtHub)
    }
}

const ROWS_FIXED: [SettingsRow; 6] = [
    SettingsRow::Theme,
    SettingsRow::Brightness,
    SettingsRow::Buzzer,
    SettingsRow::HubMoved,
    SettingsRow::Alarms,
    SettingsRow::NodesAtHub,
];
const ROWS_FIXED_QUIET: [SettingsRow; 5] =
    [SettingsRow::Theme, SettingsRow::Brightness, SettingsRow::HubMoved, SettingsRow::Alarms, SettingsRow::NodesAtHub];
const ROWS_AUTO: [SettingsRow; 9] = [
    SettingsRow::Theme,
    SettingsRow::DayTheme,
    SettingsRow::NightTheme,
    SettingsRow::SwitchAt,
    SettingsRow::Brightness,
    SettingsRow::Buzzer,
    SettingsRow::HubMoved,
    SettingsRow::Alarms,
    SettingsRow::NodesAtHub,
];
const ROWS_AUTO_QUIET: [SettingsRow; 8] = [
    SettingsRow::Theme,
    SettingsRow::DayTheme,
    SettingsRow::NightTheme,
    SettingsRow::SwitchAt,
    SettingsRow::Brightness,
    SettingsRow::HubMoved,
    SettingsRow::Alarms,
    SettingsRow::NodesAtHub,
];

impl Settings {
    /// The settings page's rows on a hub with a buzzer. The three Auto rows
    /// show only while Theme is Auto, so a fixed theme has fewer rows to get
    /// lost in. See [`Ui::rows`] for the rows a given hub shows.
    pub fn rows(&self) -> &'static [SettingsRow] {
        self.rows_for(true)
    }

    /// The rows, with or without the Buzzer row.
    pub fn rows_for(&self, buzzer: bool) -> &'static [SettingsRow] {
        match (self.theme, buzzer) {
            (ThemeMode::Auto, true) => &ROWS_AUTO,
            (ThemeMode::Auto, false) => &ROWS_AUTO_QUIET,
            (ThemeMode::Fixed(_), true) => &ROWS_FIXED,
            (ThemeMode::Fixed(_), false) => &ROWS_FIXED_QUIET,
        }
    }
}

/// What Auto is doing right now ([`Ui::auto_status`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStatus {
    /// Theme is not Auto.
    Off,
    /// No position or no time has been known yet: Glass Cockpit (fine by
    /// day and by night), or the last verdict while time is lost.
    Waiting { day: Option<bool> },
    /// Day (`true`) or night by the sun; the next switch in UTC ms, if the
    /// sun crosses the line within a day (not in polar day or night).
    Sun { day: bool, next_utc_ms: Option<u64>, stale_position: bool },
}

/// (UTC minute, rule, position, the next change) Auto last computed.
type NextMemo = (u64, SwitchAt, (f32, f32), Option<u64>);

/// Auto's memory: the last position seen (positions come and go with the
/// fix) and the last verdict, recomputed at most once a UTC minute.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct AutoMemo {
    position: Option<(f32, f32)>,
    /// (UTC minute, rule, position) the verdict is for.
    key: Option<(u64, SwitchAt, (f32, f32))>,
    day: Option<bool>,
    /// The next change, computed only when asked for (settings page), for
    /// the same key.
    next: Option<NextMemo>,
}

/// An action chosen on the settings page, for the hub to carry out
/// ([`Ui::take_request`]); the hub answers with [`Ui::action_result`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// "I moved the hub": forget the GPS home the spoofing checks learned.
    HubMoved,
    /// "Use this spot" (while the hub waits for a decision on a new place):
    /// accept the place the hub found as its home (WP-12).
    AcceptHome,
    /// "Alarms": mute, for two hours, the nuisance alarms sounding in the
    /// frame the operator was looking at (`seen_ms`, the state's
    /// `now_ms`). Never a safety-critical one (those are only snoozed).
    MuteAlarms { seen_ms: u64 },
    /// "Alarms" when only safety-critical alarms sound: snooze them for
    /// five minutes (the status bar still says JAMMED and so on).
    SnoozeAlarms { seen_ms: u64 },
    /// "Alarms" when none sounds: drop the operator's mutes and snoozes
    /// (the site file's stay).
    UnmuteAlarms,
    /// "Nodes at hub": every node stands where the hub is.
    NodesAtHub,
}

/// Where a settings action stands. "Done" is shown only once the hub says
/// it worked ([`Ui::action_result`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    /// Handed to the hub, no answer yet.
    Pending,
    Done,
    /// The hub could not do it; the words say why ("No GPS position: not
    /// placed").
    Failed(&'static str),
}

/// The last settings action of this visit to the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub row: SettingsRow,
    pub request: Request,
    pub state: ActionState,
    at_ms: u64,
}

/// A request for the buzzer, taken by the hub with [`Ui::take_buzz`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buzz {
    NewDrone,
}

/// New drones one banner keeps track of.
pub const ALERT_KEYS: usize = 8;

/// The new-drone banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// The newest drone that arrived while it was up.
    pub key: u32,
    /// Every new drone it is about, oldest first (the first
    /// [`ALERT_KEYS`]). The banner shows the most serious or nearest of
    /// them ([`Ui::alert_drone`]).
    pub keys: Vec<u32, ALERT_KEYS>,
    /// Other new drones that arrived while this banner was up.
    pub more: u8,
    /// Hub ms it appeared: a press that began before then was meant for the
    /// page underneath, and is ignored.
    pub shown_ms: u64,
    pub until_ms: u64,
    /// A drone within [`STICKY_M`] or flagged at the worst level: stays up
    /// until a key is pressed (any key acknowledges it).
    pub sticky: bool,
}

/// What the page title block says instead of the title for a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleToast {
    /// The page's name, just after flipping to it (the title inverted).
    Page,
    /// A touch on the page that does nothing: "Tap a drone, or use the
    /// keys below".
    TapHint,
    /// Auto just switched the look.
    ThemeSwitched { to: Theme, why: &'static str },
}

/// How long the banner stays up (unless sticky).
pub const ALERT_MS: u64 = 5_000;
/// A new drone this close (metres, as broadcast) keeps its banner up until
/// a key is pressed.
pub const STICKY_M: f32 = 300.0;
/// After the operator dismisses a banner, new drones for this long only
/// count up a NEW chip (a swarm or a drone show cannot keep the banner in
/// the way).
pub const SWARM_MS: u64 = 15_000;
/// How long the NEW chip shows after the last arrival it counted.
pub const NEW_CHIP_MS: u64 = 30_000;
/// New drones are marked NEW on the radar and list for this long.
pub const NEW_MS: u64 = 60_000;
/// Minimum gap between beeps, so a swarm or a drone show does not turn the
/// buzzer into a continuous tone.
pub const BUZZ_GAP_MS: u64 = 3_000;
/// Pick, scroll and settings fall back to browsing after this long without
/// a press, so a forgotten cursor never leaves the radar's big readout
/// showing some other drone instead of the nearest one.
pub const IDLE_MS: u64 = 30_000;
/// A detail page goes back to browsing after this long without a press
/// (the drone stays the one Select picks first).
pub const DETAIL_IDLE_MS: u64 = 120_000;
/// How long the page name shows after flipping pages.
pub const PAGE_TOAST_MS: u64 = 1_500;
/// How long the touch hint shows.
pub const TAP_HINT_MS: u64 = 2_500;
/// How long "Switched to Red Night (dusk)" shows.
pub const THEME_TOAST_MS: u64 = 4_000;
/// A destructive action's first press arms it for this long.
pub const CONFIRM_MS: u64 = 4_000;
/// A settings action with no answer from the hub this long has failed.
pub const ACTION_WAIT_MS: u64 = 10_000;
/// How long a tapped thing stays highlighted.
pub const TAP_FLASH_MS: u64 = 400;
/// Auto does not switch the look within this long of a key press or a new
/// drone...
pub const AUTO_QUIET_MS: u64 = 120_000;
/// ...but never holds a switch back for longer than this.
pub const AUTO_DEFER_MAX_MS: u64 = 600_000;

/// What the operator did that Auto counts as "busy" (recent input, new
/// drones), and what it is waiting to switch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct AutoShow {
    /// The look Auto shows now (`None`: not decided yet, or the settings
    /// just changed: decide at once).
    shown: Option<Theme>,
    /// A switch is due since this hub ms, held back while busy.
    waiting_since: Option<u64>,
}

/// The screen state machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Ui {
    page: Page,
    mode: Mode,
    /// Drone key under the cursor (Pick/Detail).
    cursor: Option<u32>,
    /// The drone last looked at: Select starts choosing from it.
    resume: Option<u32>,
    /// Index into `rows()`.
    settings_row: usize,
    /// Brightness row: Up/Down change the level until Select.
    adjusting: bool,
    /// A destructive action armed by its first press, until this hub ms.
    confirm: Option<(SettingsRow, u64)>,
    /// The action chosen: the request for the hub (taken once) and how it
    /// went.
    request: Option<Request>,
    action: Option<Action>,
    /// The row whose action just ran: Select there does nothing until the
    /// cursor leaves the row (no instant flip to "Unmute" under the finger).
    inert: Option<SettingsRow>,
    alert: Option<Alert>,
    /// After a dismissed banner: no new full banner before this hub ms.
    swarm_until: u64,
    /// New drones counted while the banner was held back, and until when
    /// the chip shows.
    new_chip: (u8, u64),
    /// Drones announced in the last [`NEW_MS`]: (key, hub ms).
    recent: Vec<(u32, u64), 16>,
    settings: Settings,
    has_buzzer: bool,
    /// The settings when editing began, to report what changed once it ends.
    edit_start: Option<Settings>,
    /// An edit ended with changes: (before, after), taken once by the hub.
    changed: Option<(Settings, Settings)>,
    auto: core::cell::Cell<AutoMemo>,
    auto_show: AutoShow,
    theme_toast: Option<(Theme, &'static str, u64)>,
    buzz: Option<Buzz>,
    last_buzz_ms: Option<u64>,
    last_input_ms: u64,
    last_new_ms: Option<u64>,
    toast_until_ms: u64,
    tap_hint_until: u64,
    /// The last tap that did something, highlighted briefly.
    tapped: Option<(Tap, u64)>,
    /// Hub ms the screen last changed what a touch would hit (page, mode,
    /// banner): a touch that began before then is ignored.
    changed_ms: u64,
    /// SELECT held this long (ms), for the progress fill on the key.
    held_ms: Option<u32>,
    /// Pages whose rows did not all fit, as the renderer last drew them
    /// (bit per page index), and how far SIGNALS can scroll.
    overflow: core::cell::Cell<u8>,
    scroll_max: core::cell::Cell<u8>,
    /// Scroll offset (SIGNALS) or chosen row (FLAGS).
    scroll: u8,
    now_ms: u64,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

impl Ui {
    pub fn new(settings: Settings) -> Self {
        Self {
            page: Page::Radar,
            mode: Mode::Browse,
            cursor: None,
            resume: None,
            settings_row: 0,
            adjusting: false,
            confirm: None,
            request: None,
            action: None,
            inert: None,
            alert: None,
            swarm_until: 0,
            new_chip: (0, 0),
            recent: Vec::new(),
            settings,
            has_buzzer: true,
            edit_start: None,
            changed: None,
            auto: core::cell::Cell::new(AutoMemo::default()),
            auto_show: AutoShow::default(),
            theme_toast: None,
            buzz: None,
            last_buzz_ms: None,
            last_input_ms: 0,
            last_new_ms: None,
            toast_until_ms: 0,
            tap_hint_until: 0,
            tapped: None,
            changed_ms: 0,
            held_ms: None,
            overflow: core::cell::Cell::new(0),
            scroll_max: core::cell::Cell::new(0),
            scroll: 0,
            now_ms: 0,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    /// The drone under the cursor, in Pick and Detail modes.
    pub fn cursor(&self) -> Option<u32> {
        match self.mode {
            Mode::Pick | Mode::Detail => self.cursor,
            _ => None,
        }
    }
    /// The action the operator chose on the settings page, once.
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// The hub's answer to the last [`Request`]: `Ok` once it is done,
    /// `Err(why)` when it could not be ("No GPS position: not placed").
    /// Only then does the settings page say "done".
    pub fn action_result(&mut self, r: Result<(), &'static str>) {
        if let Some(a) = self.action.as_mut()
            && a.state == ActionState::Pending
        {
            a.state = match r {
                Ok(()) => ActionState::Done,
                Err(why) => ActionState::Failed(why),
            };
        }
    }

    /// The settings action of this visit, while the cursor is on its row.
    pub fn action(&self) -> Option<Action> {
        self.action
    }

    /// The row armed by a first press of a destructive action.
    pub fn confirming(&self) -> Option<SettingsRow> {
        self.confirm.filter(|c| self.now_ms < c.1).map(|c| c.0)
    }

    /// Select does nothing on this row right now (its action just ran).
    pub fn inert(&self, row: SettingsRow) -> bool {
        self.inert == Some(row)
    }

    /// The Brightness row is taking Up/Down.
    pub fn adjusting(&self) -> bool {
        self.mode == Mode::Edit && self.adjusting
    }

    /// The cursor's row on the settings page, as an index into
    /// [`Ui::rows`].
    pub fn settings_row(&self) -> usize {
        self.settings_row
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The settings page's rows on this hub.
    pub fn rows(&self) -> &'static [SettingsRow] {
        self.settings.rows_for(self.has_buzzer)
    }

    /// Whether this hub has a buzzer (default yes). A hub without one
    /// (the P4) says so, and the Buzzer row is left out.
    pub fn set_has_buzzer(&mut self, yes: bool) {
        self.has_buzzer = yes;
        self.settings_row = self.settings_row.min(self.rows().len() - 1);
    }
    pub fn has_buzzer(&self) -> bool {
        self.has_buzzer
    }

    /// Replace the settings (from the hub's saved ones, e.g. the P4's site
    /// file). Not reported by [`Ui::take_settings_change`].
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.settings_row = self.settings_row.min(self.rows().len() - 1);
        if let Some(start) = self.edit_start.as_mut() {
            *start = settings;
        }
        self.auto_show = AutoShow::default();
    }

    /// The operator finished editing the settings (Back, the idle timeout,
    /// or the banner taking them to a drone) and something changed:
    /// `(before, after)`, handed out once, so the hub saves one line per
    /// changed setting rather than one per press.
    pub fn take_settings_change(&mut self) -> Option<(Settings, Settings)> {
        self.changed.take()
    }

    /// The page Up (`delta` -1) or Down (+1) goes to while browsing, for
    /// the key labels.
    pub fn page_after(&self, delta: isize, state: &UiState) -> Page {
        self.page.step(delta, state.flags.is_some())
    }

    /// The look to draw now. Fixed themes are just that. Auto picks the day
    /// or night theme by the sun at the hub's position (the last one seen,
    /// if the fix is gone) and GPS time; before both have been known it is
    /// Glass Cockpit. Once [`Ui::observe`] runs, Auto holds a due switch
    /// back while the operator is busy (see there).
    pub fn effective_theme(&self, state: &UiState) -> Theme {
        match self.settings.theme {
            ThemeMode::Fixed(t) => t,
            ThemeMode::Auto => self.auto_show.shown.unwrap_or_else(|| self.auto_target(state)),
        }
    }

    /// The look Auto wants by the sun right now.
    fn auto_target(&self, state: &UiState) -> Theme {
        match self.auto_day(state) {
            Some(true) => self.settings.day_theme,
            Some(false) => self.settings.night_theme,
            None => Theme::GlassCockpit,
        }
    }

    /// The Brightness level to light the panel at: the setting, capped at
    /// [`RED_NIGHT_MAX_BRIGHTNESS`] while Red Night shows (a lower setting
    /// stays lower). The hub maps it with [`backlight_pct`].
    pub fn backlight_level(&self, state: &UiState) -> u8 {
        let b = self.settings.brightness.clamp(1, 5);
        if self.effective_theme(state) == Theme::RedNight { b.min(RED_NIGHT_MAX_BRIGHTNESS) } else { b }
    }

    /// What Auto is doing, for the settings page (computes the next switch
    /// time, once a minute).
    pub fn auto_status(&self, state: &UiState) -> AutoStatus {
        if self.settings.theme != ThemeMode::Auto {
            return AutoStatus::Off;
        }
        let day = self.auto_day(state);
        let m = self.auto.get();
        let (Some(utc), Some(pos), Some(day)) = (state.utc_ms, m.position, day) else {
            return AutoStatus::Waiting { day };
        };
        let at = self.settings.switch_at;
        let minute = utc / 60_000;
        let next = match m.next {
            Some((k, a, p, n)) if k == minute && a == at && p == pos => n,
            _ => {
                let n = crate::sun::next_change(utc as i64, f64::from(pos.0), f64::from(pos.1), at).map(|t| t as u64);
                self.auto.set(AutoMemo { next: Some((minute, at, pos, n)), ..self.auto.get() });
                n
            }
        };
        AutoStatus::Sun { day, next_utc_ms: next, stale_position: state.position.is_none() }
    }

    /// Day or night by the sun, `None` until it has been known once.
    fn auto_day(&self, state: &UiState) -> Option<bool> {
        let mut m = self.auto.get();
        if let Some(p) = state.position {
            m.position = Some(p);
        }
        if let (Some(utc), Some(pos)) = (state.utc_ms, m.position) {
            let at = self.settings.switch_at;
            let key = (utc / 60_000, at, pos);
            if m.key != Some(key) {
                m.key = Some(key);
                m.day = Some(crate::sun::is_day(utc as i64, f64::from(pos.0), f64::from(pos.1), at));
            }
        }
        self.auto.set(m);
        m.day
    }

    /// The banner, if one is showing.
    pub fn alert(&self) -> Option<&Alert> {
        self.alert.as_ref()
    }

    /// The drone the banner shows: of the new drones it is about, one
    /// flagged at the worst level, else the nearest with a position, else
    /// the newest still tracked.
    pub fn alert_drone<'a>(&self, state: &'a UiState) -> Option<&'a DroneView> {
        let a = self.alert.as_ref()?;
        let mut best: Option<(&DroneView, (bool, u32))> = None;
        for k in a.keys.iter().rev() {
            let Some(d) = state.drone(*k) else { continue };
            let flagged = state.flag_level(*k).is_some_and(|l| l >= 3);
            let dist = d.drone.map_or(u32::MAX, |p| p.distance_m.max(0.0) as u32);
            let rank = (!flagged, dist);
            if best.is_none_or(|b| rank < b.1) {
                best = Some((d, rank));
            }
        }
        best.map(|b| b.0)
    }

    /// True while the page name should be shown after a page flip.
    pub fn page_toast(&self) -> bool {
        self.mode == Mode::Browse && self.now_ms < self.toast_until_ms
    }

    /// What the title block says instead of the page title right now.
    pub fn title_toast(&self) -> Option<TitleToast> {
        if self.now_ms < self.tap_hint_until {
            Some(TitleToast::TapHint)
        } else if let Some((to, why, until)) = self.theme_toast
            && self.now_ms < until
        {
            Some(TitleToast::ThemeSwitched { to, why })
        } else if self.page_toast() {
            Some(TitleToast::Page)
        } else {
            None
        }
    }

    /// New drones counted while the banner was held back (the NEW chip).
    pub fn new_chip(&self) -> Option<u8> {
        (self.new_chip.0 > 0 && self.now_ms < self.new_chip.1).then_some(self.new_chip.0)
    }

    /// This drone was announced in the last [`NEW_MS`] (marked NEW).
    pub fn is_new(&self, key: u32) -> bool {
        self.recent.iter().any(|&(k, t)| k == key && self.now_ms.saturating_sub(t) < NEW_MS)
    }

    /// The tap being highlighted, briefly.
    pub fn tapped(&self) -> Option<Tap> {
        self.tapped.filter(|t| self.now_ms < t.1).map(|t| t.0)
    }

    /// SELECT held for this long, for the key's progress fill (the hub
    /// sets it while the finger is down: [`Ui::set_held`]).
    pub fn held_ms(&self) -> Option<u32> {
        self.held_ms
    }
    pub fn set_held(&mut self, ms: Option<u32>) {
        self.held_ms = ms;
    }

    /// The renderer says whether `page` had rows it could not show, and how
    /// far SIGNALS can scroll (`max`): Select on such a page scrolls.
    pub fn note_overflow(&self, page: Page, hidden: bool, max: u8) {
        let bit = 1u8 << page.index();
        let v = self.overflow.get();
        self.overflow.set(if hidden { v | bit } else { v & !bit });
        if page == Page::Signals {
            self.scroll_max.set(max);
        }
    }
    /// Whether `page` had rows it could not show when last drawn.
    pub fn overflows(&self, page: Page) -> bool {
        self.overflow.get() & (1 << page.index()) != 0
    }
    /// SIGNALS scroll offset, or the chosen FLAGS row (Scroll mode).
    pub fn scroll(&self) -> u8 {
        self.scroll
    }

    /// The hub's tracker reported a new drone (`tracker.take_new_drone`).
    /// Shows the banner (or, within [`SWARM_MS`] of the operator dismissing
    /// one, counts it on the NEW chip) and, unless muted or rate-limited,
    /// asks for a beep.
    pub fn new_drone(&mut self, key: u32, now_ms: u64) {
        self.now_ms = now_ms;
        self.last_new_ms = Some(now_ms);
        self.recent.retain(|&(k, t)| k != key && now_ms.saturating_sub(t) < NEW_MS);
        if self.recent.is_full() {
            self.recent.remove(0);
        }
        let _ = self.recent.push((key, now_ms));
        match self.alert.as_mut() {
            Some(a) => {
                a.key = key;
                if a.keys.push(key).is_err() {
                    a.keys.remove(0);
                    let _ = a.keys.push(key);
                }
                a.more = a.more.saturating_add(1);
                a.until_ms = now_ms + ALERT_MS;
            }
            None if now_ms < self.swarm_until => {
                self.new_chip = (if self.now_ms < self.new_chip.1 { self.new_chip.0 } else { 0 }.saturating_add(1), now_ms + NEW_CHIP_MS);
            }
            None => {
                let mut keys = Vec::new();
                let _ = keys.push(key);
                self.alert = Some(Alert { key, keys, more: 0, shown_ms: now_ms, until_ms: now_ms + ALERT_MS, sticky: false });
                self.changed_ms = now_ms;
            }
        }
        let quiet = self.last_buzz_ms.is_some_and(|t| now_ms.saturating_sub(t) < BUZZ_GAP_MS);
        if self.settings.buzzer && self.has_buzzer && !quiet {
            self.buzz = Some(Buzz::NewDrone);
            self.last_buzz_ms = Some(now_ms);
        }
    }

    /// A pending buzzer request, handed out once.
    pub fn take_buzz(&mut self) -> Option<Buzz> {
        self.buzz.take()
    }

    /// Advance time: expires the banner (unless sticky), the confirm step,
    /// a settings action the hub never answered, and idle modes.
    pub fn tick(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        if self.alert.as_ref().is_some_and(|a| !a.sticky && now_ms >= a.until_ms) {
            self.alert = None;
            self.changed_ms = now_ms;
        }
        if self.confirm.is_some_and(|c| now_ms >= c.1) {
            self.confirm = None;
        }
        if let Some(a) = self.action.as_mut()
            && a.state == ActionState::Pending
            && now_ms.saturating_sub(a.at_ms) >= ACTION_WAIT_MS
        {
            a.state = ActionState::Failed("No answer from the hub");
        }
        let idle = now_ms.saturating_sub(self.last_input_ms);
        let leave = match self.mode {
            Mode::Pick | Mode::Edit | Mode::Scroll => idle >= IDLE_MS,
            Mode::Detail => idle >= DETAIL_IDLE_MS,
            Mode::Browse => false,
        };
        if leave {
            self.back_to_browse();
        }
    }

    /// Once a frame, with the state being drawn: a new drone close by or
    /// flagged keeps its banner up, and Auto switches the look when it is
    /// due and the operator is not busy (see [`Ui::effective_theme`]).
    ///
    /// Auto never switches while a banner is up, while choosing, reading a
    /// detail or editing, while an acute warning holds (jamming, a drone
    /// near a plane, faked GPS, fake IDs, a serious flag), or within
    /// [`AUTO_QUIET_MS`] of a key press or a new drone; it waits at most
    /// [`AUTO_DEFER_MAX_MS`]. While the hub's own GPS may be faked it does
    /// not switch at all (the time may be faked too). A switch shows
    /// "Switched to Red Night (dusk)" in the title block.
    pub fn observe(&mut self, state: &UiState) {
        let now = state.now_ms;
        self.now_ms = self.now_ms.max(now);
        if let Some(a) = self.alert.as_mut()
            && !a.sticky
        {
            a.sticky = a.keys.iter().any(|&k| {
                state.drone(k).is_some_and(|d| d.drone.is_some_and(|p| p.distance_m <= STICKY_M))
                    || state.flag_level(k).is_some_and(|l| l >= 3)
            });
        }
        if self.settings.theme != ThemeMode::Auto {
            self.auto_show = AutoShow::default();
            return;
        }
        let want = self.auto_target(state);
        let Some(cur) = self.auto_show.shown else {
            self.auto_show = AutoShow { shown: Some(want), waiting_since: None };
            return;
        };
        if cur == want {
            self.auto_show.waiting_since = None;
            return;
        }
        if matches!(state.gps.integrity, GpsIntegrity::SuspectSpoofing | GpsIntegrity::Spoofing) {
            return;
        }
        let recent = |t: u64| now.saturating_sub(t) < AUTO_QUIET_MS;
        let busy = self.alert.is_some()
            || self.mode != Mode::Browse
            || recent(self.last_input_ms)
            || self.last_new_ms.is_some_and(recent)
            || state.warnings().iter().any(|w| w.acute());
        let since = *self.auto_show.waiting_since.get_or_insert(now);
        if busy && now.saturating_sub(since) < AUTO_DEFER_MAX_MS {
            return;
        }
        let day = self.auto_day(state);
        let why = day.map_or("", |d| self.settings.switch_at.moment(d));
        self.auto_show = AutoShow { shown: Some(want), waiting_since: None };
        self.theme_toast = Some((want, why, now + THEME_TOAST_MS));
    }

    /// A touch on the page (not on a key) that `wide::tap_target` resolved
    /// to `t`, released at `state.now_ms`, the finger down at `pressed_ms`.
    /// A touch that began before the screen last changed is ignored.
    ///
    /// The banner keeps first-press safety: a tap on it opens its drone,
    /// anywhere else only dismisses it. Drones (radar marker, readout card,
    /// list row) and tracked drones' flag rows open their detail; a
    /// settings row is chosen first and changed by a second tap on it
    /// (destructive actions still ask to confirm). Anything else shows the
    /// hint "Tap a drone, or use the keys below".
    pub fn tap(&mut self, t: Tap, state: &UiState, pressed_ms: u64) {
        let now = state.now_ms;
        self.now_ms = now;
        if pressed_ms < self.changed_ms {
            return;
        }
        if self.alert.is_some() {
            let b = if t == Tap::Banner { Button::Select } else { Button::Up };
            return self.button_at(b, state, pressed_ms);
        }
        let flash = |ui: &mut Self| ui.tapped = Some((t, now + TAP_FLASH_MS));
        match (t, self.mode) {
            (Tap::Drone(k), Mode::Browse | Mode::Pick) if state.drone(k).is_some() => {
                self.last_input_ms = now;
                flash(self);
                self.cursor = Some(k);
                self.set_mode(Mode::Detail, now);
            }
            (Tap::Flag(i), Mode::Browse | Mode::Scroll) if self.page == Page::Flags => {
                let key = state.flags.as_ref().and_then(|f| f.rows.get(i)).and_then(|r| r.drone).filter(|k| state.drone(*k).is_some());
                match key {
                    Some(k) => {
                        self.last_input_ms = now;
                        flash(self);
                        self.cursor = Some(k);
                        self.set_mode(Mode::Detail, now);
                    }
                    None => self.tap_hint_until = now + TAP_HINT_MS,
                }
            }
            (Tap::SettingsRow(i), Mode::Browse | Mode::Edit) if self.page == Page::Settings && i < self.rows().len() => {
                self.last_input_ms = now;
                flash(self);
                if self.mode == Mode::Browse {
                    self.begin_edit(now);
                    self.move_row(i);
                } else if self.settings_row == i {
                    if self.adjusting {
                        self.adjusting = false;
                    } else {
                        self.change_setting(state);
                    }
                } else {
                    self.adjusting = false;
                    self.move_row(i);
                }
            }
            _ => self.tap_hint_until = now + TAP_HINT_MS,
        }
    }

    /// Handle one button press. `state` is the frame being shown, so the
    /// cursor moves through drones in the order they are drawn.
    pub fn button(&mut self, b: Button, state: &UiState) {
        self.button_at(b, state, state.now_ms)
    }

    /// As [`Ui::button`], for a press whose key went down at `pressed_ms`
    /// (Select fires on release, Back after the hold): a press that began
    /// before the banner appeared was meant for the page underneath and is
    /// ignored (the banner stays).
    pub fn button_at(&mut self, b: Button, state: &UiState, pressed_ms: u64) {
        let now = state.now_ms;
        self.now_ms = now;
        self.last_input_ms = now;

        // The banner takes the first press: Select opens its drone,
        // anything else just dismisses it (so a press meant for the page
        // underneath does nothing surprising). Any press acknowledges a
        // sticky banner. After a dismissal, new arrivals only count up the
        // NEW chip for a while.
        if let Some(a) = self.alert.as_ref() {
            if pressed_ms < a.shown_ms {
                return;
            }
            let open = if b == Button::Select { self.alert_drone(state).map(|d| d.key) } else { None };
            self.alert = None;
            self.changed_ms = now;
            self.swarm_until = now + SWARM_MS;
            if let Some(k) = open {
                if self.mode == Mode::Edit {
                    self.end_edit();
                }
                if !matches!(self.page, Page::Radar | Page::List | Page::Flags) {
                    self.page = Page::Radar;
                }
                self.cursor = Some(k);
                self.set_mode(Mode::Detail, now);
            }
            return;
        }

        match self.mode {
            Mode::Browse => match b {
                Button::Up => self.flip(-1, now, state.flags.is_some()),
                Button::Down => self.flip(1, now, state.flags.is_some()),
                Button::Select => match self.page {
                    Page::Radar | Page::List => {
                        let order = state.order();
                        if let Some(&first) = order.first() {
                            let start = self.resume.filter(|k| state.drone(*k).is_some()).unwrap_or(state.drones[usize::from(first)].key);
                            self.cursor = Some(start);
                            self.set_mode(Mode::Pick, now);
                        }
                    }
                    Page::Settings => self.begin_edit(now),
                    Page::Flags if state.flags.as_ref().is_some_and(|f| !f.rows.is_empty()) => {
                        self.scroll = 0;
                        self.set_mode(Mode::Scroll, now);
                    }
                    Page::Signals if self.overflows(Page::Signals) => {
                        self.scroll = 0;
                        self.set_mode(Mode::Scroll, now);
                    }
                    // Nothing to do with a short press here: a tap does
                    // what holding does.
                    Page::Flags | Page::Signals | Page::Health => self.go_radar(now),
                },
                Button::Back => self.go_radar(now),
            },
            Mode::Pick => match b {
                Button::Up => self.move_cursor(-1, state),
                Button::Down => self.move_cursor(1, state),
                Button::Select => {
                    if self.cursor.and_then(|k| state.drone(k)).is_some() {
                        self.set_mode(Mode::Detail, now);
                    }
                }
                Button::Back => self.back_to_browse(),
            },
            Mode::Detail => match b {
                Button::Up => self.move_cursor(-1, state),
                Button::Down => self.move_cursor(1, state),
                // One press (tap or hold) goes straight back to the page
                // the drone was opened from.
                Button::Select | Button::Back => self.back_to_browse(),
            },
            Mode::Edit => {
                let n = self.rows().len();
                match (b, self.adjusting) {
                    (Button::Up, true) => self.settings.brightness = self.settings.brightness.clamp(1, 5).saturating_sub(1).max(1),
                    (Button::Down, true) => self.settings.brightness = (self.settings.brightness.clamp(1, 5) + 1).min(5),
                    (Button::Select, true) => self.adjusting = false,
                    (Button::Up, false) => self.move_row(self.settings_row.saturating_sub(1)),
                    (Button::Down, false) => self.move_row((self.settings_row + 1).min(n - 1)),
                    (Button::Select, false) => self.change_setting(state),
                    (Button::Back, _) => self.back_to_browse(),
                }
            }
            Mode::Scroll => match b {
                Button::Up => self.scroll = self.scroll.saturating_sub(1),
                Button::Down => {
                    let max = match self.page {
                        Page::Flags => state.flags.as_ref().map_or(0, |f| f.rows.len().saturating_sub(1) as u8),
                        _ => self.scroll_max.get(),
                    };
                    self.scroll = (self.scroll + 1).min(max);
                }
                Button::Select if self.page == Page::Flags => {
                    let key = state.flags.as_ref().and_then(|f| f.listed(usize::from(self.scroll))).and_then(|r| r.drone);
                    if let Some(k) = key.filter(|k| state.drone(*k).is_some()) {
                        self.cursor = Some(k);
                        self.set_mode(Mode::Detail, now);
                    }
                }
                Button::Select | Button::Back => self.back_to_browse(),
            },
        }
    }

    /// A touch above the key strip that the layout has no target for (the
    /// hub without `wide::tap_target`): show the hint.
    pub fn page_tap(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        self.tap_hint_until = now_ms + TAP_HINT_MS;
    }

    fn set_mode(&mut self, m: Mode, now: u64) {
        if m != self.mode {
            self.changed_ms = now;
        }
        self.mode = m;
    }

    /// Back to browsing the current page, from any mode (every way out of
    /// editing ends the edit: changes handed over, marks cleared).
    fn back_to_browse(&mut self) {
        match self.mode {
            Mode::Edit => self.end_edit(),
            Mode::Pick | Mode::Detail => self.resume = self.cursor.or(self.resume),
            _ => {}
        }
        self.cursor = None;
        let now = self.now_ms;
        self.set_mode(Mode::Browse, now);
    }

    fn go_radar(&mut self, now: u64) {
        if self.page != Page::Radar {
            self.page = Page::Radar;
            self.changed_ms = now;
        }
    }

    fn begin_edit(&mut self, now: u64) {
        self.settings_row = 0;
        self.adjusting = false;
        self.edit_start = Some(self.settings);
        self.set_mode(Mode::Edit, now);
    }

    fn flip(&mut self, delta: isize, now: u64, flags: bool) {
        self.page = self.page.step(delta, flags);
        self.toast_until_ms = now + PAGE_TOAST_MS;
        self.changed_ms = now;
    }

    fn move_cursor(&mut self, delta: isize, state: &UiState) {
        let order = state.order();
        if order.is_empty() {
            self.cursor = None;
            let now = self.now_ms;
            self.set_mode(Mode::Browse, now);
            return;
        }
        let n = order.len() as isize;
        let at = self.cursor.and_then(|k| order.iter().position(|&i| state.drones[usize::from(i)].key == k));
        let next = match at {
            Some(p) => (p as isize + delta).rem_euclid(n),
            None => 0,
        };
        self.cursor = Some(state.drones[usize::from(order[next as usize])].key);
    }

    /// Move the settings cursor (it stops at the ends). Leaving a row
    /// disarms its confirm step and forgets its action.
    fn move_row(&mut self, i: usize) {
        if i != self.settings_row {
            self.confirm = None;
            self.inert = None;
            self.action = None;
        }
        self.settings_row = i;
    }

    /// Leaving the settings: forget marks, report changes.
    fn end_edit(&mut self) {
        self.action = None;
        self.confirm = None;
        self.inert = None;
        self.adjusting = false;
        if let Some(start) = self.edit_start.take()
            && start != self.settings
        {
            // Two edits before the hub took the first: keep the oldest
            // "before" so every changed setting is still reported.
            let before = self.changed.map_or(start, |c| c.0);
            self.changed = Some((before, self.settings));
        }
    }

    /// The row under the cursor.
    pub fn settings_row_kind(&self) -> SettingsRow {
        let rows = self.rows();
        rows[self.settings_row.min(rows.len() - 1)]
    }

    /// The request a press on an action row would make now (`None`: the
    /// Alarms row with nothing to do).
    pub fn row_request(row: SettingsRow, state: &UiState) -> Option<Request> {
        match row {
            // One row, two meanings: while the hub has found a new place
            // and waits, Select accepts it; otherwise Select forgets home
            // (both say "the hub really is here").
            SettingsRow::HubMoved if state.gps.home_pending => Some(Request::AcceptHome),
            SettingsRow::HubMoved => Some(Request::HubMoved),
            SettingsRow::Alarms if state.threats.mutable > 0 => Some(Request::MuteAlarms { seen_ms: state.now_ms }),
            SettingsRow::Alarms if state.threats.snoozable > 0 => Some(Request::SnoozeAlarms { seen_ms: state.now_ms }),
            SettingsRow::Alarms if state.caveats.silenced.operator() > 0 => Some(Request::UnmuteAlarms),
            SettingsRow::NodesAtHub => Some(Request::NodesAtHub),
            _ => None,
        }
    }

    /// Whether `r` asks to confirm first: forgetting the hub's home and
    /// placing every node overwrite what the spoofing checks rely on; using
    /// a new spot too while the hub's GPS may be jammed or faked.
    pub fn needs_confirm(r: Request, state: &UiState) -> bool {
        match r {
            Request::HubMoved | Request::NodesAtHub => true,
            Request::AcceptHome => state.gps.integrity != GpsIntegrity::Ok,
            _ => false,
        }
    }

    fn change_setting(&mut self, state: &UiState) {
        let row = self.settings_row_kind();
        if self.inert == Some(row) {
            return;
        }
        let now = state.now_ms;
        let s = &mut self.settings;
        match row {
            SettingsRow::Theme => s.theme = s.theme.next(),
            SettingsRow::DayTheme => s.day_theme = s.day_theme.next(),
            SettingsRow::NightTheme => s.night_theme = s.night_theme.next(),
            SettingsRow::SwitchAt => {
                s.switch_at = match s.switch_at {
                    SwitchAt::Sunrise => SwitchAt::Twilight,
                    SwitchAt::Twilight => SwitchAt::Sunrise,
                }
            }
            SettingsRow::Brightness => self.adjusting = true,
            SettingsRow::Buzzer => s.buzzer = !s.buzzer,
            SettingsRow::HubMoved | SettingsRow::Alarms | SettingsRow::NodesAtHub => {
                let Some(r) = Self::row_request(row, state) else { return };
                if Self::needs_confirm(r, state) && self.confirming() != Some(row) {
                    self.confirm = Some((row, now + CONFIRM_MS));
                    return;
                }
                self.confirm = None;
                self.request = Some(r);
                self.action = Some(Action { row, request: r, state: ActionState::Pending, at_ms: now });
                self.inert = Some(row);
            }
        }
        if matches!(row, SettingsRow::Theme | SettingsRow::DayTheme | SettingsRow::NightTheme | SettingsRow::SwitchAt) {
            // The operator's own change shows at once.
            self.auto_show = AutoShow::default();
        }
        // The Theme row stays where it is (row 0) as the Auto rows come
        // and go; keep the cursor in range anyway.
        self.settings_row = self.settings_row.min(self.rows().len() - 1);
    }
}

/// Turns three debounced button levels into [`Button`] events.
///
/// - Up/Down fire on press and auto-repeat while held.
/// - Select fires on **release** if it was a short press, and Back fires
///   as soon as Select has been held for [`ButtonDecoder::LONG_MS`] (so the
///   user feels the long press take effect without having to let go).
///
/// Call [`ButtonDecoder::poll`] every 10-20 ms with debounced levels
/// (`true` = pressed).
#[derive(Debug, Clone, Default)]
pub struct ButtonDecoder {
    up: Held,
    down: Held,
    select: Option<u64>,
    long_fired: bool,
    /// When the last Select press began.
    select_start: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Held {
    since: Option<u64>,
    next_repeat: u64,
}

impl Held {
    fn poll(&mut self, pressed: bool, now: u64) -> bool {
        match (pressed, self.since) {
            (true, None) => {
                self.since = Some(now);
                self.next_repeat = now + ButtonDecoder::REPEAT_DELAY_MS;
                true
            }
            (true, Some(_)) if now >= self.next_repeat => {
                self.next_repeat = now + ButtonDecoder::REPEAT_MS;
                true
            }
            (false, _) => {
                self.since = None;
                false
            }
            _ => false,
        }
    }
}

impl ButtonDecoder {
    pub const LONG_MS: u64 = 700;
    pub const REPEAT_DELAY_MS: u64 = 500;
    pub const REPEAT_MS: u64 = 200;

    pub fn new() -> Self {
        Self::default()
    }

    /// When the Select press that produced the last Select or Back began
    /// (for [`Ui::button_at`]).
    pub fn select_started(&self) -> u64 {
        self.select_start
    }

    /// At most one event per call; Up wins over Down wins over Select.
    pub fn poll(&mut self, up: bool, down: bool, select: bool, now_ms: u64) -> Option<Button> {
        let u = self.up.poll(up, now_ms);
        let d = self.down.poll(down, now_ms);
        let s = match (select, self.select) {
            (true, None) => {
                self.select = Some(now_ms);
                self.select_start = now_ms;
                self.long_fired = false;
                None
            }
            (true, Some(t)) if !self.long_fired && now_ms - t >= Self::LONG_MS => {
                self.long_fired = true;
                Some(Button::Back)
            }
            (false, Some(_)) => {
                self.select = None;
                (!self.long_fired).then_some(Button::Select)
            }
            _ => None,
        };
        if u {
            Some(Button::Up)
        } else if d {
            Some(Button::Down)
        } else {
            s
        }
    }
}
