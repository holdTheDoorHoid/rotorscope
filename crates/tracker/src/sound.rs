//! Sound contacts: what the hub's microphone hears (PLAN.md Phase 15).
//!
//! The acoustic detector (`crates/acoustic`) says, about once a second,
//! whether it hears a rotor-like sound, and at what pitch. This module turns
//! those reports into **contacts** (one per stretch of hearing) and asks the
//! question the detector cannot: *do we know what it is?*
//!
//! - **With radio.** A drone, an FPV link, a DJI fingerprint or a Wi-Fi
//!   drone was heard by radio in the last [`RADIO_RECENT_MS`]. Sound carries
//!   about 50-200 m, so the drone is close.
//! - **Aircraft.** No drone on the radio, but a crewed aircraft we track
//!   (ADS-B, Mode S, UAT, FLARM) is airborne within [`AIRCRAFT_AUDIBLE_M`].
//!   Helicopters and propeller aircraft make the same ladder of tones as a
//!   drone; this is the likeliest source and it does not alert.
//! - **No radio.** Neither. A drone that transmits nothing (a stored route,
//!   a fibre-optic FPV), an aircraft with its transponder off, or a machine
//!   on the ground. Alerts as "possible drone heard, no radio": a weaker
//!   claim than any radio detection, and worded so.
//!
//! - **Low hum.** Whatever the radio says or does not say, a contact whose
//!   pitch stays below [`LOW_HUM_BELOW_HZ`] is logged and shown but does not
//!   alert on its own: fans, engines, compressors and crewed aircraft live
//!   there, and the camera and FPV drones this hub is for do not (every
//!   drone recording we have sits at 158 Hz or higher; every false alert in
//!   the first day's indoor run sat at 102-120 Hz). A drone on the radio
//!   still makes it "with radio".
//!
//! One microphone gives no direction, so with several radio tracks live the
//! contact cannot say which of them is the audible one.
//!
//! A contact's kind only ever strengthens while it lasts (no radio ->
//! aircraft -> with radio), so one fly-past is not relabelled back and
//! forth. Low hum against no radio is the exception: it follows what most
//! of the contact's reports said, so a hum whose pitch rises into the drone
//! range does alert, and one second at the wrong octave does not.

use heapless::Vec;
use serde::Serialize;

use crate::aircraft::AircraftKey;
use crate::signals::rf_alerts;
use crate::text::FormatError;
use crate::{Context, Tracker};

pub mod clip;

/// Contacts held.
pub const MAX_CONTACTS: usize = 16;
/// Silence this long ends a contact; hearing again starts a new one.
pub const CONTACT_GAP_MS: u64 = 5_000;
/// A contact must last this long before it alerts.
pub const ANNOUNCE_AFTER_MS: u64 = 5_000;
/// "Heard by radio lately".
pub const RADIO_RECENT_MS: u64 = 15_000;
/// Slant range inside which a crewed aircraft is taken as audible, metres.
/// A heuristic (after `audio-flyover-geolocation`'s 2.5-3 km).
pub const AIRCRAFT_AUDIBLE_M: f64 = 3_000.0;
/// A contact heard mostly below this pitch is a "low hum" and does not
/// alert by itself: the recommended line, and the one in force unless the
/// site says otherwise ([`LowHum`]). Lowest drone pitch in our recordings:
/// 158 Hz (Mavic Pro); heavy-lift drones with 20 inch and larger propellers
/// do sit below it and are the price (docs/ACOUSTIC.md "Low hums").
pub const LOW_HUM_BELOW_HZ: u16 = 140;
/// The lowest and highest line a site may set (`low hum below <Hz>`).
pub const LOW_HUM_MIN_HZ: u16 = 80;
pub const LOW_HUM_MAX_HZ: u16 = 400;

/// The site's low-hum setting (`low hum alerts on|off`, `low hum below
/// <Hz>` in the site file, `tracker::site`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowHum {
    /// Low hums alert like any other unexplained sound. Off is recommended.
    pub alerts: bool,
    /// The line, Hz. [`LOW_HUM_BELOW_HZ`] is recommended.
    pub below_hz: u16,
}

impl LowHum {
    pub const RECOMMENDED: Self = Self { alerts: false, below_hz: LOW_HUM_BELOW_HZ };

    /// Pitch below which a report counts as low: nothing does when low
    /// hums are to alert.
    const fn line(self) -> u16 {
        if self.alerts { 0 } else { self.below_hz }
    }
}

impl Default for LowHum {
    fn default() -> Self {
        Self::RECOMMENDED
    }
}

/// Stable handle for a sound contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SoundId(pub u32);

/// One report from the acoustic detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundReport {
    /// The detector is in its "heard" state.
    pub heard: bool,
    /// Blade-pass pitch, Hz.
    pub pitch_hz: u16,
    /// 0-100.
    pub confidence: u8,
    /// Loudness, dB relative to full scale.
    pub level_dbfs: i8,
    /// Pitch says small, fast propellers (FPV) rather than large ones.
    pub small_props: bool,
}

/// What a contact most likely is. Ordered weakest to strongest explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum SoundKind {
    LowHum,
    NoRadio,
    Aircraft,
    WithRadio,
}

impl SoundKind {
    /// Short plain-English name for the screen and alerts.
    pub const fn label(self) -> &'static str {
        match self {
            SoundKind::LowHum => "Low hum heard (fan, engine or aircraft)",
            SoundKind::NoRadio => "Possible drone heard, no radio",
            SoundKind::Aircraft => "Aircraft heard",
            SoundKind::WithRadio => "Drone audible (close)",
        }
    }

    /// Whether this kind raises an alert. A tracked aircraft does not, and
    /// neither does a low hum.
    pub const fn alerts(self) -> bool {
        !matches!(self, SoundKind::Aircraft | SoundKind::LowHum)
    }
}

/// One stretch of hearing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundContact {
    pub id: SoundId,
    pub kind: SoundKind,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Latest pitch and the range it covered, Hz.
    pub pitch_hz: u16,
    pub pitch_min_hz: u16,
    pub pitch_max_hz: u16,
    pub confidence: u8,
    pub peak_confidence: u8,
    pub level_dbfs: i8,
    pub peak_level_dbfs: i8,
    pub small_props: bool,
    /// Detector reports merged into this contact.
    pub reports: u32,
    /// How many of them were below the low-hum line ([`LowHum`]).
    pub low_reports: u32,
    /// The nearest audible aircraft when the contact was (last) explained
    /// by one, and its slant range, metres.
    pub aircraft: Option<AircraftKey>,
    pub aircraft_range_m: u32,
    /// Number of the audio clip the hub saved for this contact (0: none).
    pub clip: u16,
    pub(crate) announced: bool,
}

impl SoundContact {
    /// Still being heard.
    pub fn active(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_seen_ms) < CONTACT_GAP_MS
    }

    pub fn duration_ms(&self) -> u64 {
        self.last_seen_ms.saturating_sub(self.first_seen_ms)
    }

    /// Whether this contact is at the point that raises an alert: it alerts
    /// as a drone-like sound (not explained by a tracked aircraft), has
    /// lasted [`ANNOUNCE_AFTER_MS`] and is still being heard. The alert
    /// ([`Tracker::take_new_sound`]) and the audio clip
    /// ([`clip::may_start`]) both use exactly this.
    pub fn alert_due(&self, now_ms: u64) -> bool {
        self.kind.alerts() && self.active(now_ms) && self.duration_ms() >= ANNOUNCE_AFTER_MS
    }

    /// "Large propellers" or "Small propellers".
    pub fn size_label(&self) -> &'static str {
        if self.small_props { "Small propellers" } else { "Large propellers" }
    }
}

pub struct SoundTable {
    contacts: Vec<SoundContact, MAX_CONTACTS>,
    next_id: u32,
    low_hum: LowHum,
}

impl SoundTable {
    pub const fn new() -> Self {
        Self { contacts: Vec::new(), next_id: 1, low_hum: LowHum::RECOMMENDED }
    }

    pub fn all(&self) -> &[SoundContact] {
        &self.contacts
    }

    pub(crate) fn expire(&mut self, now_ms: u64, remove_after_ms: u64) {
        self.contacts.retain(|c| now_ms.saturating_sub(c.last_seen_ms) < remove_after_ms);
    }
}

impl Default for SoundTable {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// Whether anything that is (almost certainly) a drone was heard by
    /// radio within [`RADIO_RECENT_MS`]. Drone tracks count only once
    /// confirmed (`probation`): one forged frame is not a radio drone.
    pub fn radio_drone_recent(&self, now_ms: u64) -> bool {
        let recent = |last: u64| now_ms.saturating_sub(last) < RADIO_RECENT_MS;
        self.tracks.iter().any(|t| t.confirmed() && recent(t.last_seen_ms))
            || self.open_links().iter().any(|l| recent(l.last_seen_ms))
            || self.emitters().iter().any(|e| rf_alerts(e.kind) && recent(e.last_seen_ms))
            || self.wifi_drones().iter().any(|w| recent(w.last_seen_ms))
    }

    /// The nearest airborne crewed aircraft within [`AIRCRAFT_AUDIBLE_M`]
    /// (slant range), heard from within [`RADIO_RECENT_MS`]. Needs the hub's
    /// own position. Only established aircraft (`aircraft` "Standing"): an
    /// unproven address, which one forged ADS-B frame can make, must not
    /// explain a drone's sound away and so silence its alert and its clip.
    pub fn audible_aircraft(&self, now_ms: u64) -> Option<(AircraftKey, u32)> {
        let ctx = self.context(now_ms, None);
        let mut best: Option<(AircraftKey, u32)> = None;
        for a in self.aircraft() {
            if !a.airborne || !a.established() || now_ms.saturating_sub(a.last_seen_ms) >= RADIO_RECENT_MS {
                continue;
            }
            let g = ctx.aircraft_geometry(a);
            let Some(r) = g.range else { continue };
            let up = g.above_m.unwrap_or(0.0);
            let slant = libm::sqrt(r.distance_m * r.distance_m + up * up);
            if slant <= AIRCRAFT_AUDIBLE_M && best.is_none_or(|b| slant < f64::from(b.1)) {
                best = Some((a.key, slant as u32));
            }
        }
        best
    }

    /// Take one report from the acoustic detector (about once a second).
    /// Returns the contact it belongs to while the detector hears something.
    pub fn ingest_sound(&mut self, r: &SoundReport, now_ms: u64) -> Option<SoundId> {
        self.sounds.expire(now_ms, self.config.remove_after_ms);
        if !r.heard {
            return None;
        }
        let aircraft = self.audible_aircraft(now_ms);
        let explained = if self.radio_drone_recent(now_ms) {
            Some(SoundKind::WithRadio)
        } else if aircraft.is_some() {
            Some(SoundKind::Aircraft)
        } else {
            None
        };
        let low = u32::from(r.pitch_hz < self.sounds.low_hum.line());
        // Unexplained: a low hum if most of the contact was heard low.
        let unexplained = |low_reports: u32, reports: u32| {
            if low_reports * 2 > reports { SoundKind::LowHum } else { SoundKind::NoRadio }
        };
        let table = &mut self.sounds;
        if let Some(c) = table.contacts.iter_mut().find(|c| c.active(now_ms)) {
            c.last_seen_ms = now_ms;
            c.reports = c.reports.saturating_add(1);
            c.low_reports = c.low_reports.saturating_add(low);
            let mut kind = explained.unwrap_or_else(|| unexplained(c.low_reports, c.reports));
            if c.kind >= SoundKind::Aircraft {
                kind = kind.max(c.kind);
            }
            c.kind = kind;
            c.pitch_hz = r.pitch_hz;
            c.pitch_min_hz = c.pitch_min_hz.min(r.pitch_hz);
            c.pitch_max_hz = c.pitch_max_hz.max(r.pitch_hz);
            c.confidence = r.confidence;
            c.peak_confidence = c.peak_confidence.max(r.confidence);
            c.level_dbfs = r.level_dbfs;
            c.peak_level_dbfs = c.peak_level_dbfs.max(r.level_dbfs);
            c.small_props = r.small_props;
            if let Some((key, range)) = aircraft {
                c.aircraft = Some(key);
                c.aircraft_range_m = range;
            }
            return Some(c.id);
        }
        if table.contacts.is_full() {
            // Drop the one heard least recently.
            let oldest = (0..table.contacts.len()).min_by_key(|&i| table.contacts[i].last_seen_ms)?;
            table.contacts.swap_remove(oldest);
        }
        let id = SoundId(table.next_id);
        table.next_id = table.next_id.wrapping_add(1).max(1);
        let _ = table.contacts.push(SoundContact {
            id,
            kind: explained.unwrap_or_else(|| unexplained(low, 1)),
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            pitch_hz: r.pitch_hz,
            pitch_min_hz: r.pitch_hz,
            pitch_max_hz: r.pitch_hz,
            confidence: r.confidence,
            peak_confidence: r.confidence,
            level_dbfs: r.level_dbfs,
            peak_level_dbfs: r.level_dbfs,
            small_props: r.small_props,
            reports: 1,
            low_reports: low,
            aircraft: aircraft.map(|a| a.0),
            aircraft_range_m: aircraft.map_or(0, |a| a.1),
            clip: 0,
            announced: false,
        });
        Some(id)
    }

    /// The low-hum setting in force.
    pub fn low_hum(&self) -> LowHum {
        self.sounds.low_hum
    }

    /// Change the low-hum setting. Reports already counted keep the line
    /// they were counted against.
    pub fn set_low_hum(&mut self, to: LowHum) {
        self.sounds.low_hum = to;
    }

    pub fn sounds(&self) -> &[SoundContact] {
        self.sounds.all()
    }

    pub fn get_sound(&self, id: SoundId) -> Option<&SoundContact> {
        self.sounds.contacts.iter().find(|c| c.id == id)
    }

    /// The contact being heard right now, if any.
    pub fn active_sound(&self, now_ms: u64) -> Option<&SoundContact> {
        self.sounds.contacts.iter().find(|c| c.active(now_ms))
    }

    /// Record which audio clip the hub saved for a contact.
    pub fn set_sound_clip(&mut self, id: SoundId, clip: u16) {
        if let Some(c) = self.sounds.contacts.iter_mut().find(|c| c.id == id) {
            c.clip = clip;
        }
    }

    /// The next sound contact to alert on, exactly once each: one that has
    /// lasted [`ANNOUNCE_AFTER_MS`], is still being heard, and is not
    /// explained by a tracked aircraft.
    pub fn take_new_sound(&mut self, now_ms: u64) -> Option<SoundId> {
        let c = self.sounds.contacts.iter_mut().find(|c| !c.announced && c.alert_due(now_ms))?;
        c.announced = true;
        Some(c.id)
    }
}

#[derive(Serialize)]
struct SoundLine {
    t: &'static str,
    id: SoundId,
    /// `"LowHum"`, `"NoRadio"`, `"Aircraft"` or `"WithRadio"`.
    kind: SoundKind,
    label: &'static str,
    alerts: bool,
    active: bool,
    pitch_hz: u16,
    pitch_min_hz: u16,
    pitch_max_hz: u16,
    size: &'static str,
    confidence: u8,
    peak_confidence: u8,
    level_dbfs: i8,
    peak_level_dbfs: i8,
    /// ICAO address (hex) of the aircraft that explains it, and its slant
    /// range in metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    aircraft: Option<heapless::String<6>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aircraft_m: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clip: Option<u16>,
    n: u32,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
}

/// A buffer this size always holds one sound line (see the test).
pub const MAX_SOUND_LINE: usize = 448;
const _: () = assert!(MAX_SOUND_LINE <= crate::json::MAX_LINE);

/// Write one sound contact as a JSON object followed by `\n` (`"t":"sound"`).
pub fn sound_line(c: &SoundContact, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    use core::fmt::Write;
    let aircraft = c.aircraft.map(|k| {
        let mut s: heapless::String<6> = heapless::String::new();
        let _ = write!(s, "{}", crate::aircraft::IcaoHex(k.icao));
        s
    });
    let line = SoundLine {
        t: "sound",
        id: c.id,
        kind: c.kind,
        label: c.kind.label(),
        alerts: c.kind.alerts(),
        active: c.active(ctx.now_ms),
        pitch_hz: c.pitch_hz,
        pitch_min_hz: c.pitch_min_hz,
        pitch_max_hz: c.pitch_max_hz,
        size: c.size_label(),
        confidence: c.confidence,
        peak_confidence: c.peak_confidence,
        level_dbfs: c.level_dbfs,
        peak_level_dbfs: c.peak_level_dbfs,
        aircraft_m: aircraft.is_some().then_some(c.aircraft_range_m),
        aircraft,
        clip: (c.clip != 0).then_some(c.clip),
        n: c.reports,
        first_ms: c.first_seen_ms,
        last_ms: c.last_seen_ms,
        age_ms: ctx.now_ms.saturating_sub(c.last_seen_ms),
    };
    let n = serde_json_core::to_slice(&line, buf).map_err(|_| FormatError::BufferTooSmall)?;
    let nl = buf.get_mut(n).ok_or(FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}
