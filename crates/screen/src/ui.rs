//! The screen's own state: which page is showing, the cursor, the alert
//! banner and the user's settings. Driven by [`Button`] events and
//! [`Ui::tick`]; read by [`crate::render`].

use core::cell::Cell;

use crate::model::UiState;

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
    pub const ALL: [Page; 6] = [
        Page::Radar,
        Page::List,
        Page::Flags,
        Page::Signals,
        Page::Health,
        Page::Settings,
    ];

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

    /// The next page `delta` away; FLAGS is skipped when `flags` is false.
    fn step(self, delta: isize, flags: bool) -> Page {
        let n = Self::ALL.len() as isize;
        let next = Self::ALL[(self.index() as isize + delta).rem_euclid(n) as usize];
        if next == Page::Flags && !flags {
            next.step(delta, flags)
        } else {
            next
        }
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
}

/// A colour scheme (the look actually drawn). Every distinction is also
/// carried by shape or text, so any of them can be used for anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Dark, after airliner displays: cyan labels, white data, magenta for
    /// drones and the selection, amber cautions, green OK. The default.
    #[default]
    GlassCockpit,
    /// Dark ink on a light background, for direct sunlight.
    FieldDay,
    /// Red only, dim: keeps night vision. One hue, so alerts rely on boxes,
    /// inverse text and shapes.
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

    /// The next one, wrapping round.
    pub const fn next(self) -> Theme {
        match self {
            Theme::GlassCockpit => Theme::FieldDay,
            Theme::FieldDay => Theme::RedNight,
            Theme::RedNight => Theme::GlassCockpit,
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

    /// Auto -> Glass Cockpit -> Field Day -> Red Night -> Auto.
    const fn next(self) -> ThemeMode {
        match self {
            ThemeMode::Auto => ThemeMode::Fixed(Theme::GlassCockpit),
            ThemeMode::Fixed(Theme::RedNight) => ThemeMode::Auto,
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
}

/// User settings the hub should apply (and may persist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemeMode,
    /// Used by Auto while the sun is up.
    pub day_theme: Theme,
    /// Used by Auto at night, and while Auto does not know yet.
    pub night_theme: Theme,
    pub switch_at: SwitchAt,
    /// Backlight level 1..=5. The hub maps it to PWM.
    pub brightness: u8,
    /// Beep on a new drone.
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
    Buzzer,
    /// "Hub moved", or "New home" while the hub waits for one.
    HubMoved,
    Alarms,
    NodesAtHub,
}

const ROWS_FIXED: [SettingsRow; 6] = [
    SettingsRow::Theme,
    SettingsRow::Brightness,
    SettingsRow::Buzzer,
    SettingsRow::HubMoved,
    SettingsRow::Alarms,
    SettingsRow::NodesAtHub,
];
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

impl Settings {
    /// The settings page's rows, in order. The three Auto rows show only
    /// while Theme is Auto, so a fixed theme has fewer rows to get lost in.
    pub fn rows(&self) -> &'static [SettingsRow] {
        match self.theme {
            ThemeMode::Auto => &ROWS_AUTO,
            ThemeMode::Fixed(_) => &ROWS_FIXED,
        }
    }
}

/// What Auto is doing right now ([`Ui::auto_status`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStatus {
    /// Theme is not Auto.
    Off,
    /// No position or no time has been known yet: the night theme (dark is
    /// the safer guess), or the last one in effect.
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
/// ([`Ui::take_request`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// "Hub moved": forget the GPS home the spoofing checks learned.
    HubMoved,
    /// "New home" (the row's label while the hub waits for one): accept
    /// the place the hub found as its home (WP-12).
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

/// A request for the buzzer, taken by the hub with [`Ui::take_buzz`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buzz {
    NewDrone,
}

/// The new-drone banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    /// The drone that triggered it (the newest, if several arrived).
    pub key: u32,
    pub until_ms: u64,
    /// Other new drones that arrived while this banner was up.
    pub more: u8,
}

/// How long the banner stays up.
pub const ALERT_MS: u64 = 5_000;
/// Minimum gap between beeps, so a swarm or a drone show does not turn the
/// buzzer into a continuous tone.
pub const BUZZ_GAP_MS: u64 = 3_000;
/// Pick mode and settings fall back to browsing after this long without a
/// press, so a forgotten cursor never leaves the radar's big readout showing
/// some other drone instead of the nearest one.
pub const IDLE_MS: u64 = 30_000;
/// How long the page name shows after flipping pages.
pub const PAGE_TOAST_MS: u64 = 1_500;

/// The screen state machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Ui {
    page: Page,
    mode: Mode,
    /// Drone key under the cursor (Pick/Detail).
    cursor: Option<u32>,
    /// Index into `settings.rows()`.
    settings_row: usize,
    /// An action row was chosen: the request for the hub (taken once), and
    /// which row says "done" until the settings are left.
    request: Option<Request>,
    done_row: Option<SettingsRow>,
    alert: Option<Alert>,
    settings: Settings,
    /// The settings when editing began, to report what changed once it ends.
    edit_start: Option<Settings>,
    /// An edit ended with changes: (before, after), taken once by the hub.
    changed: Option<(Settings, Settings)>,
    auto: Cell<AutoMemo>,
    buzz: Option<Buzz>,
    last_buzz_ms: Option<u64>,
    last_input_ms: u64,
    toast_until_ms: u64,
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
            settings_row: 0,
            request: None,
            done_row: None,
            alert: None,
            settings,
            edit_start: None,
            changed: None,
            auto: Cell::new(AutoMemo::default()),
            buzz: None,
            last_buzz_ms: None,
            last_input_ms: 0,
            toast_until_ms: 0,
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

    /// The action row chosen during this visit to the settings, if any.
    pub fn done_row(&self) -> Option<SettingsRow> {
        self.done_row
    }

    /// The cursor's row on the settings page, as an index into
    /// [`Settings::rows`].
    pub fn settings_row(&self) -> usize {
        self.settings_row
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Replace the settings (from the hub's saved ones, e.g. the P4's site
    /// file). Not reported by [`Ui::take_settings_change`].
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
        let n = self.settings.rows().len();
        self.settings_row = self.settings_row.min(n - 1);
        if let Some(start) = self.edit_start.as_mut() {
            *start = settings;
        }
    }

    /// The operator finished editing the settings (Back, or the idle
    /// timeout) and something changed: `(before, after)`, handed out once,
    /// so the hub saves one line per changed setting rather than one per
    /// press.
    pub fn take_settings_change(&mut self) -> Option<(Settings, Settings)> {
        self.changed.take()
    }

    /// The page Up (`delta` -1) or Down (+1) goes to while browsing, for
    /// the key labels.
    pub fn page_after(&self, delta: isize, state: &UiState) -> Page {
        self.page.step(delta, state.flags.is_some())
    }

    /// The look to draw now. Fixed themes are just that; Auto picks the day
    /// or night theme by the sun at the hub's position (the last one seen,
    /// if the fix is gone) and GPS time. Before both are known it is the
    /// night theme: dark is the safer guess at an unknown hour. Without
    /// time it keeps the last verdict.
    pub fn effective_theme(&self, state: &UiState) -> Theme {
        let s = &self.settings;
        match s.theme {
            ThemeMode::Fixed(t) => t,
            ThemeMode::Auto => {
                if self.auto_day(state).unwrap_or(false) {
                    s.day_theme
                } else {
                    s.night_theme
                }
            }
        }
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
    /// True while the page name should be shown after a page flip.
    pub fn page_toast(&self) -> bool {
        self.mode == Mode::Browse && self.now_ms < self.toast_until_ms
    }

    /// The hub's tracker reported a new drone (`tracker.take_new_drone`).
    /// Shows the banner and, unless muted or rate-limited, asks for a beep.
    pub fn new_drone(&mut self, key: u32, now_ms: u64) {
        self.now_ms = now_ms;
        let more = match self.alert {
            Some(a) if now_ms < a.until_ms => a.more.saturating_add(1),
            _ => 0,
        };
        self.alert = Some(Alert {
            key,
            until_ms: now_ms + ALERT_MS,
            more,
        });
        let quiet = self
            .last_buzz_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < BUZZ_GAP_MS);
        if self.settings.buzzer && !quiet {
            self.buzz = Some(Buzz::NewDrone);
            self.last_buzz_ms = Some(now_ms);
        }
    }

    /// A pending buzzer request, handed out once.
    pub fn take_buzz(&mut self) -> Option<Buzz> {
        self.buzz.take()
    }

    /// Advance time: expires the banner, the page toast and idle modes.
    pub fn tick(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        if self.alert.is_some_and(|a| now_ms >= a.until_ms) {
            self.alert = None;
        }
        let idle = now_ms.saturating_sub(self.last_input_ms) >= IDLE_MS;
        if idle && matches!(self.mode, Mode::Pick | Mode::Edit) {
            if self.mode == Mode::Edit {
                self.end_edit();
            }
            self.mode = Mode::Browse;
            self.cursor = None;
        }
    }

    /// Handle one button press. `state` is the frame being shown, so the
    /// cursor moves through drones in the order they are drawn.
    pub fn button(&mut self, b: Button, state: &UiState) {
        let now = state.now_ms;
        self.now_ms = now;
        self.last_input_ms = now;

        // The banner takes the first press: Select opens the new drone,
        // anything else just dismisses it (so a press meant for the page
        // underneath does nothing surprising).
        if let Some(a) = self.alert.take() {
            if b == Button::Select && state.drone(a.key).is_some() {
                if self.mode != Mode::Detail {
                    self.back_page_for_detail();
                }
                self.cursor = Some(a.key);
                self.mode = Mode::Detail;
            }
            return;
        }

        match self.mode {
            Mode::Browse => match b {
                Button::Up => self.flip(-1, now, state.flags.is_some()),
                Button::Down => self.flip(1, now, state.flags.is_some()),
                Button::Select => match self.page {
                    Page::Radar | Page::List => {
                        if let Some(&first) = state.order().first() {
                            self.cursor = Some(state.drones[usize::from(first)].key);
                            self.mode = Mode::Pick;
                        }
                    }
                    Page::Settings => {
                        self.settings_row = 0;
                        self.mode = Mode::Edit;
                        self.edit_start = Some(self.settings);
                    }
                    Page::Health | Page::Signals | Page::Flags => {}
                },
                Button::Back => {
                    self.page = Page::Radar;
                }
            },
            Mode::Pick => match b {
                Button::Up => self.move_cursor(-1, state),
                Button::Down => self.move_cursor(1, state),
                Button::Select => {
                    if self.cursor.and_then(|k| state.drone(k)).is_some() {
                        self.mode = Mode::Detail;
                    }
                }
                Button::Back => {
                    self.mode = Mode::Browse;
                    self.cursor = None;
                }
            },
            Mode::Detail => match b {
                Button::Up => self.move_cursor(-1, state),
                Button::Down => self.move_cursor(1, state),
                Button::Select => {}
                Button::Back => {
                    // Back to choosing, with the cursor on this drone.
                    self.mode = if self.cursor.and_then(|k| state.drone(k)).is_some() {
                        Mode::Pick
                    } else {
                        self.cursor = None;
                        Mode::Browse
                    };
                }
            },
            Mode::Edit => {
                let n = self.settings.rows().len();
                match b {
                    Button::Up => self.settings_row = (self.settings_row + n - 1) % n,
                    Button::Down => self.settings_row = (self.settings_row + 1) % n,
                    Button::Select => self.change_setting(state),
                    Button::Back => {
                        self.mode = Mode::Browse;
                        self.end_edit();
                    }
                }
            }
        }
    }

    /// Opening a detail from the banner: return to a drone page afterwards.
    fn back_page_for_detail(&mut self) {
        if !matches!(self.page, Page::Radar | Page::List) {
            self.page = Page::Radar;
        }
    }

    fn flip(&mut self, delta: isize, now: u64, flags: bool) {
        self.page = self.page.step(delta, flags);
        self.toast_until_ms = now + PAGE_TOAST_MS;
    }

    fn move_cursor(&mut self, delta: isize, state: &UiState) {
        let order = state.order();
        if order.is_empty() {
            self.cursor = None;
            self.mode = Mode::Browse;
            return;
        }
        let n = order.len() as isize;
        let at = self.cursor.and_then(|k| {
            order
                .iter()
                .position(|&i| state.drones[usize::from(i)].key == k)
        });
        let next = match at {
            Some(p) => (p as isize + delta).rem_euclid(n),
            None => 0,
        };
        self.cursor = Some(state.drones[usize::from(order[next as usize])].key);
    }

    /// Leaving the settings: forget "done" marks, report changes.
    fn end_edit(&mut self) {
        self.done_row = None;
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
        let rows = self.settings.rows();
        rows[self.settings_row.min(rows.len() - 1)]
    }

    fn change_setting(&mut self, state: &UiState) {
        let row = self.settings_row_kind();
        let s = &mut self.settings;
        let r = match row {
            SettingsRow::Theme => {
                s.theme = s.theme.next();
                None
            }
            SettingsRow::DayTheme => {
                s.day_theme = s.day_theme.next();
                None
            }
            SettingsRow::NightTheme => {
                s.night_theme = s.night_theme.next();
                None
            }
            SettingsRow::SwitchAt => {
                s.switch_at = match s.switch_at {
                    SwitchAt::Sunrise => SwitchAt::Twilight,
                    SwitchAt::Twilight => SwitchAt::Sunrise,
                };
                None
            }
            SettingsRow::Brightness => {
                s.brightness = s.brightness % 5 + 1;
                None
            }
            SettingsRow::Buzzer => {
                s.buzzer = !s.buzzer;
                None
            }
            // One row, two meanings: while the hub has found a new place
            // and waits, SEL accepts it; otherwise SEL forgets home (both
            // say "the hub really is here").
            SettingsRow::HubMoved if state.gps.home_pending => Some(Request::AcceptHome),
            SettingsRow::HubMoved => Some(Request::HubMoved),
            SettingsRow::Alarms if state.threats.mutable > 0 => Some(Request::MuteAlarms { seen_ms: state.now_ms }),
            SettingsRow::Alarms if state.threats.snoozable > 0 => Some(Request::SnoozeAlarms { seen_ms: state.now_ms }),
            SettingsRow::Alarms if state.caveats.silenced.operator() > 0 => Some(Request::UnmuteAlarms),
            SettingsRow::Alarms => None,
            SettingsRow::NodesAtHub => Some(Request::NodesAtHub),
        };
        if r.is_some() {
            self.request = r;
            self.done_row = Some(row);
        }
        // The Theme row stays where it is (row 0) as the Auto rows come
        // and go; keep the cursor in range anyway.
        self.settings_row = self.settings_row.min(self.settings.rows().len() - 1);
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

    /// At most one event per call; Up wins over Down wins over Select.
    pub fn poll(&mut self, up: bool, down: bool, select: bool, now_ms: u64) -> Option<Button> {
        let u = self.up.poll(up, now_ms);
        let d = self.down.poll(down, now_ms);
        let s = match (select, self.select) {
            (true, None) => {
                self.select = Some(now_ms);
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
