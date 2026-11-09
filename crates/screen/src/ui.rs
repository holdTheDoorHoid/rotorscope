//! The screen's own state: which page is showing, the cursor, the alert
//! banner and the user's settings. Driven by [`Button`] events and
//! [`Ui::tick`]; read by [`crate::render`].

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
    Health,
    Settings,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Radar, Page::List, Page::Health, Page::Settings];

    pub const fn title(self) -> &'static str {
        match self {
            Page::Radar => "RADAR",
            Page::List => "DRONE LIST",
            Page::Health => "HEALTH",
            Page::Settings => "SETTINGS",
        }
    }

    fn index(self) -> usize {
        match self {
            Page::Radar => 0,
            Page::List => 1,
            Page::Health => 2,
            Page::Settings => 3,
        }
    }

    fn step(self, delta: isize) -> Page {
        let n = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + delta).rem_euclid(n) as usize]
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

/// Colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Dark background, default. Kind to night vision.
    #[default]
    Night,
    /// Light background, for direct sunlight.
    Day,
}

/// User settings the hub should apply (and may persist to flash).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: Theme,
    /// Backlight level 1..=5. The hub maps it to PWM.
    pub brightness: u8,
    /// Beep on a new drone.
    pub buzzer: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::Night,
            brightness: 3,
            buzzer: true,
        }
    }
}

/// Rows on the settings page, in order.
pub const SETTINGS_ROWS: usize = 3;

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
    settings_row: usize,
    alert: Option<Alert>,
    settings: Settings,
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
            alert: None,
            settings,
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
    pub fn settings_row(&self) -> usize {
        self.settings_row
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
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
                Button::Up => self.flip(-1, now),
                Button::Down => self.flip(1, now),
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
                    }
                    Page::Health => {}
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
            Mode::Edit => match b {
                Button::Up => {
                    self.settings_row = (self.settings_row + SETTINGS_ROWS - 1) % SETTINGS_ROWS
                }
                Button::Down => self.settings_row = (self.settings_row + 1) % SETTINGS_ROWS,
                Button::Select => self.change_setting(),
                Button::Back => self.mode = Mode::Browse,
            },
        }
    }

    /// Opening a detail from the banner: return to a drone page afterwards.
    fn back_page_for_detail(&mut self) {
        if !matches!(self.page, Page::Radar | Page::List) {
            self.page = Page::Radar;
        }
    }

    fn flip(&mut self, delta: isize, now: u64) {
        self.page = self.page.step(delta);
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

    fn change_setting(&mut self) {
        let s = &mut self.settings;
        match self.settings_row {
            0 => {
                s.theme = match s.theme {
                    Theme::Night => Theme::Day,
                    Theme::Day => Theme::Night,
                }
            }
            1 => s.brightness = s.brightness % 5 + 1,
            _ => s.buzzer = !s.buzzer,
        }
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
