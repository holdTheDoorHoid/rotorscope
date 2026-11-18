//! What the hub remembers about its own GPS across boots (WP-12,
//! docs/GNSS-INTEGRITY.md "Home" and "Time floor"): its home and the latest
//! time it trusted, in `ROTORSCP/GPSMEM.TXT`, and the operator's three GPS
//! decisions (hub moved, accept the new home, accept GPS time).
//!
//! **Writes** go through the SD logger (`Logger::write_gps_mem`: queued, done
//! by its own step when the card is free, never from the GPS path) and are
//! rare: when home is learned, confirmed, forgotten or placed, and the time
//! **at most once an hour** (first at the first trusted time after the card
//! is mounted). Each rewrites one sector-sized file: a few writes a day.
//!
//! **Evidence**: every load, learn, change and forget is a `Setting` event;
//! a new place waiting for the operator is one too.
//!
//! Loop-back bench builds (`gpsloop`) use `GPSLOOP.TXT` and never remember
//! the time (the emulated receiver restarts its clock at every boot, which
//! would look like time going back); the `early` scenario sets a remembered
//! time itself (gpsloop.rs).

use core::fmt::Write;

use crate::evlog::Evidence;
use crate::gps::{Gps, Memory};
use crate::sd::Logger;

/// The remembered time is written at most this often (ms).
pub const TIME_EVERY_MS: u64 = 3_600_000;

#[derive(Default)]
pub struct GpsMem {
    /// When the time was last written (hub ms), this boot.
    time_written: Option<u64>,
    /// The card's memory has been read (or there is none) this boot.
    pub loaded: bool,
}

const fn remembers_time() -> bool {
    !cfg!(feature = "gpsloop")
}

impl GpsMem {
    /// The card was mounted: apply what it remembered.
    pub fn loaded(&mut self, gps: &mut Gps, ev: &mut Evidence, m: Option<Memory>) {
        self.loaded = true;
        let Some(m) = m else {
            setting(ev, format_args!("GPS memory: none on the card (no remembered home or time yet)"));
            return;
        };
        if let Some((lat, lon, alt)) = m.home {
            if gps.set_home(lat, lon, alt, gnss::HomeOrigin::Remembered) {
                setting(ev, format_args!("GPS home loaded from the card: {lat:.7} {lon:.7}; spoofing checks use it from now"));
            } else {
                setting(
                    ev,
                    format_args!("GPS home on the card ({lat:.7} {lon:.7}) not used: the hub already has a {} home", gps.home_origin().map_or("?", |o| o.name())),
                );
            }
        }
        if remembers_time()
            && let Some(t) = m.time_ms
        {
            gps.set_remembered_time(Some(t));
            let c = crate::civil::civil(t);
            setting(
                ev,
                format_args!(
                    "GPS memory: latest trusted time {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC; earlier GPS time is untrusted",
                    c.year, c.month, c.day, c.hour, c.minute, c.second
                ),
            );
        }
    }

    /// Once a second: store a learned home, report a waiting one, remember
    /// the time hourly.
    pub fn tick(&mut self, gps: &mut Gps, sd: &mut Logger, ev: &mut Evidence, now: u64) {
        match gps.take_home_event() {
            Some(gnss::HomeEvent::Learned { lat, lon, .. }) => {
                setting(ev, format_args!("GPS home learned: {lat:.7} {lon:.7} (15 min of steady fixes); remembered on the card"));
                self.save(gps, sd, now);
            }
            Some(gnss::HomeEvent::Pending { lat, lon, dist_m }) => {
                setting(
                    ev,
                    format_args!(
                        "GPS: the hub seems to stand at {lat:.7} {lon:.7}, {dist_m:.0} m from its {} home; NOT adopted: confirm on the screen (Settings, New home) or console H if the hub was moved, else this is spoofing",
                        gps.home_origin().map_or("?", |o| o.name())
                    ),
                );
            }
            None => {}
        }
        if remembers_time()
            && self.loaded
            && gps.trusted_utc_now(now).is_some()
            && self.time_written.is_none_or(|t| now.saturating_sub(t) >= TIME_EVERY_MS)
        {
            self.save(gps, sd, now);
        }
    }

    /// Queue the file: home and (when trusted now) the time.
    fn save(&mut self, gps: &mut Gps, sd: &mut Logger, now: u64) {
        let utc = if remembers_time() { gps.trusted_utc_now(now) } else { None };
        if let Some(u) = utc {
            self.time_written = Some(now);
            // What we write is also the floor from now on.
            gps.set_remembered_time(Some(u));
        }
        sd.write_gps_mem(gps.memory_text(utc));
    }

    /// Hub moved (screen "Hub moved", console `m`): forget home, relearn.
    pub fn forget_home(&mut self, gps: &mut Gps, sd: &mut Logger, ev: &mut Evidence, from: &str, now: u64) {
        if gps.home_origin() == Some(gnss::HomeOrigin::Placed) {
            setting(ev, format_args!("hub moved ({from}): home is placed in the site file; edit its home line (or 'home clear') instead"));
            return;
        }
        gps.integrity_forget_home();
        setting(ev, format_args!("hub moved: GPS home forgotten, relearning (from {from})"));
        self.save(gps, sd, now);
    }

    /// The operator confirms the place waiting since a "NOT adopted" event.
    pub fn confirm_home(&mut self, gps: &mut Gps, sd: &mut Logger, ev: &mut Evidence, from: &str, now: u64) {
        match gps.confirm_home() {
            Some((lat, lon, _)) => {
                setting(ev, format_args!("GPS home confirmed by the operator ({from}): {lat:.7} {lon:.7}; remembered on the card"));
                self.save(gps, sd, now);
            }
            None => setting(ev, format_args!("GPS new home ({from}): nothing waiting to confirm{}", if gps.home_origin() == Some(gnss::HomeOrigin::Placed) { " (home is placed in the site file)" } else { "" })),
        }
    }

    /// Console `T`: accept GPS time as it now is.
    pub fn accept_time(&mut self, gps: &mut Gps, ev: &mut Evidence, from: &str) {
        gps.accept_time();
        self.time_written = None;
        setting(ev, format_args!("GPS time accepted by the operator ({from}): the hub's clock and the remembered time follow GPS again"));
    }

    /// The site file's placed home changed.
    pub fn placed(&mut self, gps: &mut Gps, sd: &mut Logger, ev: &mut Evidence, home: Option<(f64, f64, Option<f64>)>, now: u64) {
        match home {
            Some((lat, lon, alt)) => {
                gps.set_home(lat, lon, alt, gnss::HomeOrigin::Placed);
                setting(ev, format_args!("GPS home placed from the site file: {lat:.7} {lon:.7}"));
            }
            None if gps.home_origin() == Some(gnss::HomeOrigin::Placed) => {
                gps.integrity_forget_home();
                setting(ev, format_args!("GPS placed home cleared: relearning"));
                self.save(gps, sd, now);
            }
            None => {}
        }
    }
}

fn setting(ev: &mut Evidence, args: core::fmt::Arguments<'_>) {
    let mut t: heapless::String<256> = heapless::String::new();
    let _ = t.write_fmt(args);
    ev.event(evidence::EventCode::Setting, t.as_bytes(), &[4]);
    crate::println!("hub-p4: {t}");
}
