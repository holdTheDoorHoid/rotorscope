//! The repeat-visitor history (threat rule 17, slice 16d) on the SD card:
//! `ROTORSCP/VISITS.DAT`.
//!
//! **What is in it** (`tracker::behaviour::visits`): for each drone seen at
//! or near a protected place, a fingerprint of its ID and the UTC days it
//! was seen; for each pilot spot, its 25 m square and the days. No times of
//! day, no paths. Kept 30 days (owner decision 2026-11-18), then dropped.
//!
//! **Read** once per card mount, after the site file (a `visits off` line
//! means it is not read but emptied). A file that is damaged, cut off, or
//! from another firmware version is discarded whole: logged (console and a
//! `Setting` evidence event), never a crash, and replaced on the next write.
//!
//! **Written** (whole, rewritten in place) when the history changed, at
//! most every [`SAVE_EVERY_MS`], and at once at a change of UTC day, a wipe,
//! a discarded file or `visits off`. A full table is about 5 KB: ten
//! blocks, well under a second on this bit-banged card; usually far less.
//!
//! **Wiping** (the owner's right to remove what the hub keeps): console `W`,
//! twice within [`CONFIRM_MS`]. The screen has no room for another Settings
//! row (six rows fill it), so there is no screen action. A wipe is a
//! `Setting` evidence event, data byte [`EV_VISITS`].
//!
//! Evidence: every load, discard, wipe and switch is a `Setting` event
//! (code 4) with the data byte [`EV_VISITS`]; no new evidence code.

use core::fmt::Write;

use embedded_sdmmc::{Mode, RawDirectory};
use tracker::behaviour::visits::{self, Loader};

use crate::app::App;
use crate::evlog::Evidence;
use crate::sd::{DIR, Logger, Vm};

/// The file, in `ROTORSCP`.
pub const NAME: &str = "VISITS.DAT";
/// Written at most this often while it keeps changing.
pub const SAVE_EVERY_MS: u64 = 300_000;
/// Console `W` must be pressed twice within this long.
pub const CONFIRM_MS: u64 = 5_000;
/// The data byte of this module's `Setting` events (3 is the site file, 4
/// the GPS memory).
pub const EV_VISITS: u8 = 5;

#[derive(Default)]
pub struct VisitsSd {
    /// A card was recording when last looked.
    mounted: bool,
    /// The file was read (or found missing) since this mount.
    loaded: bool,
    /// Write as soon as the card is free (a wipe, a discarded file, `visits
    /// off`, a new day).
    urgent: bool,
    last_save_ms: u64,
    /// The UTC day last seen.
    day: Option<u16>,
    /// The site file's switch, as last seen.
    on: Option<bool>,
    /// When `W` was first pressed.
    wipe_asked_ms: Option<u64>,
    pub writes: u32,
}

fn setting(ev: &mut Evidence, args: core::fmt::Arguments<'_>) {
    let mut t: heapless::String<200> = heapless::String::new();
    let _ = t.write_fmt(args);
    ev.event(evidence::EventCode::Setting, t.as_bytes(), &[EV_VISITS]);
    crate::println!("hub-p4: {t}");
}

impl VisitsSd {
    /// Every main-loop pass (cheap unless there is something to do).
    #[inline(never)]
    pub fn tick(&mut self, app: &mut App, sd: &Logger, ev: &mut Evidence, now: u64, utc: Option<u64>) {
        if !sd.recording() {
            self.mounted = false;
            return;
        }
        if !self.mounted {
            self.mounted = true;
            self.loaded = false;
        }
        let on = app.tracker.places().visits;
        if self.on != Some(on) {
            if self.on.is_some() {
                if on {
                    setting(ev, format_args!("repeat-visitor history on (visits on): drone IDs and pilot spots seen at the places, 30 days"));
                } else {
                    let (a, b) = app.tracker.visits_mut().clear();
                    setting(ev, format_args!("repeat-visitor history off (visits off): {a} drone IDs and {b} pilot spots erased; {DIR}/{NAME} emptied"));
                }
            }
            self.urgent |= !on;
            self.on = Some(on);
        }
        let today = utc.map(visits::day_of);
        if today.is_some() && self.day.is_some() && today != self.day {
            self.urgent = true;
        }
        if today.is_some() {
            self.day = today;
        }
        let Some((vm, dir)) = sd.folder() else { return };
        if !self.loaded {
            self.loaded = true;
            if on {
                self.load(vm, dir, app, ev, today);
            } else if let Ok(f) = vm.open_file_in_dir(dir, NAME, Mode::ReadOnly) {
                // Nothing kept: a file left from before is emptied.
                let _ = vm.close_file(f);
                app.tracker.visits_mut().clear();
                self.urgent = true;
            }
            return;
        }
        // (After a new day `urgent` waits for the tracker's daily pruning,
        // which may come a second later, or for the day's first sighting.)
        if app.tracker.visits().dirty() && (self.urgent || now.saturating_sub(self.last_save_ms) >= SAVE_EVERY_MS) {
            self.save(vm, dir, app, now);
        }
    }

    fn load(&mut self, vm: &Vm, dir: RawDirectory, app: &mut App, ev: &mut Evidence, today: Option<u16>) {
        let f = match vm.open_file_in_dir(dir, NAME, Mode::ReadOnly) {
            Ok(f) => f,
            Err(embedded_sdmmc::Error::NotFound) => {
                crate::println!("hub-p4: no {DIR}/{NAME} yet: the repeat-visitor history starts now");
                return;
            }
            Err(e) => {
                crate::println!("hub-p4: {DIR}/{NAME} open failed: {e:?}; the history starts now (the file is replaced on the next write)");
                return;
            }
        };
        let mut l = Loader::new();
        let mut buf = [0u8; 512];
        let mut bytes = 0usize;
        loop {
            match vm.read(f, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    bytes += n;
                    l.feed(app.tracker.visits_mut(), &buf[..n]);
                }
            }
        }
        let _ = vm.close_file(f);
        match l.finish(app.tracker.visits_mut(), today) {
            Ok(got) => setting(
                ev,
                format_args!(
                    "repeat-visitor history read from {DIR}/{NAME} ({bytes} bytes): {} drone IDs, {} pilot spots; days older than 30 dropped{}",
                    got.ids,
                    got.spots,
                    if today.is_some() { "" } else { " once GPS time is known" }
                ),
            ),
            Err(e) => {
                self.urgent = true;
                setting(ev, format_args!("{DIR}/{NAME} discarded ({e}, {bytes} bytes): the repeat-visitor history starts again; the file is replaced"));
            }
        }
    }

    fn save(&mut self, vm: &Vm, dir: RawDirectory, app: &mut App, now: u64) {
        self.last_save_ms = now;
        let ok = match vm.open_file_in_dir(dir, NAME, Mode::ReadWriteCreateOrTruncate) {
            Ok(f) => {
                let w = app.tracker.visits().write(|chunk| vm.write(f, chunk).is_ok());
                vm.close_file(f).is_ok() && w
            }
            Err(_) => false,
        };
        if ok {
            app.tracker.visits_mut().take_dirty();
            self.urgent = false;
            self.writes += 1;
        } else {
            crate::println!("hub-p4: {DIR}/{NAME} write failed; tried again in {} min", SAVE_EVERY_MS / 60_000);
        }
    }

    /// Console `W`: the first press asks, a second within [`CONFIRM_MS`]
    /// wipes the history in RAM and empties the card's file. Out of line:
    /// the console commands are inlined into the main loop's frame.
    #[inline(never)]
    pub fn wipe_key(&mut self, app: &mut App, sd: &Logger, ev: &mut Evidence, now: u64) {
        let (a, b) = app.tracker.visits().counts();
        match self.wipe_asked_ms.take() {
            Some(t) if now.saturating_sub(t) <= CONFIRM_MS => {
                app.tracker.visits_mut().clear();
                self.urgent = true;
                let card = match sd.folder() {
                    Some((vm, dir)) => {
                        self.save(vm, dir, app, now);
                        if self.urgent { "the card's copy could not be emptied yet (tried again)" } else { "the card's copy emptied" }
                    }
                    None => "the card's copy is emptied when a card is next free",
                };
                setting(ev, format_args!("repeat-visitor history wiped by the operator (console W): {a} drone IDs and {b} pilot spots erased; {card}"));
            }
            _ => {
                self.wipe_asked_ms = Some(now);
                crate::println!(
                    "hub-p4: cmd press W again within {} s to wipe the repeat-visitor history ({a} drone IDs, {b} pilot spots); anything else cancels",
                    CONFIRM_MS / 1000
                );
            }
        }
    }

    /// Any other console key cancels a pending wipe.
    pub fn cancel_wipe(&mut self) {
        self.wipe_asked_ms = None;
    }
}
