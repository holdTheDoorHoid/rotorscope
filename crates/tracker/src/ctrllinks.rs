//! Drone control links (ExpressLRS, mLRS, SiK, OpenLRSng) and control-band
//! energy, as LR2021 nodes report them (link protocol 12, `crates/ctrlwatch`,
//! docs/CONTROL-LINKS.md).
//!
//! - [`ControlTable`]: one entry per link, keyed by system, region and the
//!   link's identifier when the node sends one (only with the owner's
//!   "read IDs" opt-in; without it every link of a system in a region is
//!   one entry). It keeps counts, signal, confidence and, when content
//!   decoding is on, the latest stick positions and telemetry.
//! - A decoded **aircraft position** (`ControlContent::Position`, kind
//!   aircraft, a plausible fix) creates or updates an ordinary drone track
//!   ([`crate::Source::ControlLink`]), so it appears on the radar and list
//!   and in the drone-near-aircraft check, like a DJI DroneID position. The
//!   home point goes to the track's `home`.
//! - [`EnergyBands`]: the latest energy window of each band per node:
//!   hopping transmitters with their candidate systems, steady carriers.
//!
//! Small and fixed-size: the P4 hub keeps the tracker in internal RAM.

use ctrlwatch::content::{AltRef, PositionKind, Telemetry};
use ctrlwatch::{Confidence, ControlContent, LinkId, Probe, Region, System};
use heapless::Vec;
use link::{ControlEnergy, ControlReport, EnergyActivity};
use rid::message::{
    HeightType, HorizontalAccuracy, LatLon, Location, SpeedAccuracy, Status, VerticalAccuracy,
};

use crate::{Config, Stamped, TrackId, TrackState, Tracker};

/// Control links held. A sensor rarely hears more than a few at once; when
/// full, the one heard least recently makes room.
pub const MAX_CONTROL_LINKS: usize = 6;
/// Energy bands held (band x node).
pub const MAX_ENERGY_BANDS: usize = 4;
/// Stick channels kept for display (roll, pitch, throttle, yaw on most
/// radios; the rest are switches).
pub const SHOWN_CHANNELS: usize = 4;

/// Stable handle for a control link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct ControlLinkId(pub u32);

/// The latest stick positions: the first [`SHOWN_CHANNELS`] channels (µs,
/// 0 = not sent) and the link's arm flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sticks {
    pub us: [u16; SHOWN_CHANNELS],
    pub armed: Option<bool>,
    pub rx_ms: u64,
}

/// The latest telemetry items worth a glance.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TelemetrySnap {
    pub battery_mv: Option<u32>,
    pub battery_pct: Option<u8>,
    /// Link quality the link reports about itself (ELRS LQ, %).
    pub lq: Option<u8>,
    pub mode: heapless::String<12>,
    /// Altitude (dm, barometric) if the link sends it.
    pub baro_dm: Option<i32>,
    pub rx_ms: u64,
}

/// One control link.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlLink {
    pub id: ControlLinkId,
    pub system: System,
    pub region: Region,
    /// Only when the node read it (the owner's ID opt-in).
    pub link_id: Option<LinkId>,
    /// Rate / mode of the latest packet, frequency it was heard on.
    pub probe: Probe,
    pub freq_hz: u32,
    pub version: Option<u8>,
    pub best: Confidence,
    /// The node's running counts (latest report).
    pub packets: u32,
    pub confirmed: u32,
    pub rssi: i16,
    pub peak_rssi: i16,
    pub snr_db: i8,
    pub node: u8,
    pub heard_up: bool,
    pub heard_down: bool,
    /// Content was reported encrypted (SiK AES, mLRS privacy).
    pub encrypted: bool,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    pub reports: u32,
    pub sticks: Option<Sticks>,
    pub telemetry: Option<TelemetrySnap>,
    /// The drone track its decoded positions feed.
    pub track: Option<TrackId>,
}

impl ControlLink {
    /// Band name for the screen: "915", "868", "433", "2.4".
    pub fn band(&self) -> &'static str {
        band_name(self.region)
    }
}

pub const fn band_name(r: Region) -> &'static str {
    match r {
        Region::Fcc915 | Region::Au915 | Region::Th920 => "915",
        Region::Eu868 | Region::In866 => "868",
        Region::Au433 | Region::Eu433 | Region::Us433 | Region::Us433W => "433",
        Region::Ism2g4 => "2.4",
    }
}

/// Short system names for small screens (eight characters at most).
pub const fn short_name(s: System) -> &'static str {
    match s {
        System::ExpressLrs => "ELRS",
        System::Mlrs => "mLRS",
        System::Sik => "SiK",
        System::OpenLrsNg => "OpenLRS",
        System::Crossfire => "CRSF?",
        System::Tracer => "Tracer?",
        System::Ghost => "Ghost?",
        System::FrskyR9 => "R9?",
        System::FrskyX => "FrSky?",
        System::FlyskyAfhds2a => "FlySky?",
    }
}

pub const fn confidence_name(c: Confidence) -> &'static str {
    match c {
        Confidence::Energy => "energy",
        Confidence::Likely => "likely",
        Confidence::Confirmed => "confirmed",
    }
}

/// The latest energy window of one band from one node.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergyBand {
    pub region: Region,
    pub node: u8,
    pub windows: u32,
    pub activity: EnergyActivity,
    pub steady: u8,
    pub last_seen_ms: u64,
    /// When a hopper was last seen here (it stays on the screen while
    /// active, even when the next window is quiet).
    pub hopper: Option<(link::HopperReport, u64)>,
}

/// The control-link tables.
#[derive(Debug, Clone, Default)]
pub struct ControlTable {
    links: Vec<ControlLink, MAX_CONTROL_LINKS>,
    bands: Vec<EnergyBand, MAX_ENERGY_BANDS>,
    next_id: u32,
    /// Reports received: summaries, content parts, energy windows.
    pub summaries: u32,
    pub contents: u32,
    pub energy: u32,
    /// Decoded positions that became drone track positions.
    pub positions: u32,
}

fn age_ok(last: u64, now: u64, cfg: &Config) -> bool {
    now.saturating_sub(last) < cfg.remove_after_ms
}

impl ControlTable {
    pub const fn new() -> Self {
        Self {
            links: Vec::new(),
            bands: Vec::new(),
            next_id: 1,
            summaries: 0,
            contents: 0,
            energy: 0,
            positions: 0,
        }
    }

    pub fn links(&self) -> &[ControlLink] {
        &self.links
    }

    pub fn bands(&self) -> &[EnergyBand] {
        &self.bands
    }

    pub fn expire(&mut self, now_ms: u64, cfg: &Config) {
        self.links.retain(|l| age_ok(l.last_seen_ms, now_ms, cfg));
        self.bands.retain(|b| age_ok(b.last_seen_ms, now_ms, cfg));
    }

    /// The entry for a report (made if new). Index into `links`.
    fn entry(&mut self, r: &ControlReport, now_ms: u64) -> usize {
        let s = &r.sighting;
        let key = (s.system, s.region, s.id);
        if let Some(i) = self
            .links
            .iter()
            .position(|l| (l.system, l.region, l.link_id) == key)
        {
            return i;
        }
        if self.links.is_full()
            && let Some(i) = (0..self.links.len()).min_by_key(|&i| self.links[i].last_seen_ms)
        {
            self.links.remove(i);
        }
        let id = ControlLinkId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let _ = self.links.push(ControlLink {
            id,
            system: s.system,
            region: s.region,
            link_id: s.id,
            probe: s.probe,
            freq_hz: s.freq_hz,
            version: s.version,
            best: r.best,
            packets: 0,
            confirmed: 0,
            rssi: s.rssi_dbm,
            peak_rssi: s.rssi_dbm,
            snr_db: s.snr_db,
            node: 0,
            heard_up: false,
            heard_down: false,
            encrypted: false,
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            reports: 0,
            sticks: None,
            telemetry: None,
            track: None,
        });
        self.links.len() - 1
    }

    fn ingest_energy(&mut self, node: u8, e: &ControlEnergy, now_ms: u64) {
        self.energy = self.energy.saturating_add(1);
        let i = match self
            .bands
            .iter()
            .position(|b| b.region == e.region && b.node == node)
        {
            Some(i) => i,
            None => {
                if self.bands.is_full()
                    && let Some(i) =
                        (0..self.bands.len()).min_by_key(|&i| self.bands[i].last_seen_ms)
                {
                    self.bands.remove(i);
                }
                let _ = self.bands.push(EnergyBand {
                    region: e.region,
                    node,
                    windows: 0,
                    activity: EnergyActivity::Quiet,
                    steady: 0,
                    last_seen_ms: now_ms,
                    hopper: None,
                });
                self.bands.len() - 1
            }
        };
        let b = &mut self.bands[i];
        b.windows = e.windows;
        b.steady = e.steady.len() as u8;
        b.last_seen_ms = now_ms;
        if let EnergyActivity::Hopper(h) = &e.activity {
            b.hopper = Some((h.clone(), now_ms));
        }
        b.activity = e.activity.clone();
    }
}

/// Whether a band's last hopper is still worth showing.
pub fn hopper_state(b: &EnergyBand, now_ms: u64, cfg: &Config) -> Option<TrackState> {
    let (_, t) = b.hopper.as_ref()?;
    Some(if now_ms.saturating_sub(*t) >= cfg.stale_after_ms {
        TrackState::Stale
    } else {
        TrackState::Active
    })
}

fn location_from(p: &ctrlwatch::content::Position) -> Location {
    let (alt_geo, height) = match p.alt_mm {
        Some((mm, AltRef::Msl)) => (
            Some(mm as f32 / 1000.0),
            p.rel_alt_mm.map(|r| r as f32 / 1000.0),
        ),
        Some((mm, AltRef::Home)) => (None, Some(mm as f32 / 1000.0)),
        None => (None, p.rel_alt_mm.map(|r| r as f32 / 1000.0)),
    };
    Location {
        status: Status::Undeclared,
        height_type: HeightType::AboveTakeoff,
        direction: p.heading_cdeg.map(|h| (h % 36_000) as f32 / 100.0),
        speed_horizontal: p.ground_speed_cms.map(|s| s as f32 / 100.0),
        speed_vertical: None,
        position: Some(LatLon {
            lat: p.lat_e7 as f64 / 1e7,
            lon: p.lon_e7 as f64 / 1e7,
        }),
        altitude_pressure: None,
        // MSL from the GPS: close enough to the ellipsoid height for the
        // aircraft check, which widens its margin for mixed references.
        altitude_geodetic: alt_geo,
        height,
        horizontal_accuracy: HorizontalAccuracy::Unknown,
        vertical_accuracy: VerticalAccuracy::Unknown,
        baro_accuracy: VerticalAccuracy::Unknown,
        speed_accuracy: SpeedAccuracy::Unknown,
        timestamp: None,
        timestamp_accuracy: None,
    }
}

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// Feed one `Frame::Control` from `node`. Returns the link's entry id.
    pub fn ingest_control(&mut self, node: u8, r: &ControlReport, now_ms: u64) -> ControlLinkId {
        let cfg = self.config;
        self.control.expire(now_ms, &cfg);
        let i = self.control.entry(r, now_ms);
        let s = &r.sighting;
        {
            let l = &mut self.control.links[i];
            l.probe = s.probe;
            l.freq_hz = s.freq_hz;
            l.version = s.version.or(l.version);
            l.best = l.best.max(r.best).max(s.confidence);
            l.packets = r.packets;
            l.confirmed = r.confirmed;
            l.rssi = s.rssi_dbm;
            l.peak_rssi = r.peak_rssi;
            l.snr_db = s.snr_db;
            l.node = node;
            match s.direction {
                ctrlwatch::Direction::Uplink => l.heard_up = true,
                ctrlwatch::Direction::Downlink => l.heard_down = true,
                ctrlwatch::Direction::Unknown => {}
            }
            l.last_seen_ms = now_ms;
            l.reports = l.reports.saturating_add(1);
        }
        if r.part == 0 {
            self.control.summaries = self.control.summaries.saturating_add(1);
        } else {
            self.control.contents = self.control.contents.saturating_add(1);
        }
        for c in &r.content {
            self.control_content(i, node, c, s.rssi_dbm, now_ms);
        }
        self.control.links[i].id
    }

    fn control_content(&mut self, i: usize, node: u8, c: &ControlContent, rssi: i16, now_ms: u64) {
        let l = &mut self.control.links[i];
        match c {
            ControlContent::Channels(ch) => {
                let mut s = Sticks {
                    armed: ch.armed,
                    rx_ms: now_ms,
                    ..Sticks::default()
                };
                for (k, v) in s.us.iter_mut().enumerate() {
                    *v = ch.get(k).unwrap_or(0);
                }
                l.sticks = Some(s);
            }
            ControlContent::Telemetry(t) => {
                let snap = l.telemetry.get_or_insert_with(TelemetrySnap::default);
                snap.rx_ms = now_ms;
                match t {
                    Telemetry::Battery {
                        voltage_mv,
                        remaining_pct,
                        ..
                    } => {
                        snap.battery_mv = Some(*voltage_mv);
                        snap.battery_pct = remaining_pct.or(snap.battery_pct);
                    }
                    Telemetry::Link { uplink_lq, .. } => snap.lq = Some(*uplink_lq),
                    Telemetry::FlightMode(m) => {
                        snap.mode.clear();
                        for ch in m.chars().filter(|c| (' '..='~').contains(c)) {
                            if snap.mode.push(ch).is_err() {
                                break;
                            }
                        }
                    }
                    Telemetry::BaroAltitude { alt_dm, .. } => snap.baro_dm = Some(*alt_dm),
                    _ => {}
                }
            }
            ControlContent::Opaque(ctrlwatch::content::Opaque::Encrypted) => l.encrypted = true,
            ControlContent::Opaque(_) => {}
            ControlContent::Position(p) if p.is_plausible() => {
                self.control_position(i, node, p, rssi, now_ms)
            }
            ControlContent::Position(_) => {}
        }
    }

    /// A decoded position: the link's drone track (made if it has none, or
    /// its track expired).
    fn control_position(
        &mut self,
        i: usize,
        node: u8,
        p: &ctrlwatch::content::Position,
        rssi: i16,
        now_ms: u64,
    ) {
        let system = self.control.links[i].system;
        let want = self.control.links[i].track;
        let ti = match want.and_then(|id| self.tracks.iter().position(|t| t.id == id)) {
            Some(ti) => ti,
            None => self.create(now_ms),
        };
        let t = &mut self.tracks[ti];
        t.control = Some(system);
        if node < 8 {
            t.nodes |= 1 << node;
        }
        t.last_seen_ms = t.last_seen_ms.max(now_ms);
        let ll = LatLon {
            lat: p.lat_e7 as f64 / 1e7,
            lon: p.lon_e7 as f64 / 1e7,
        };
        match p.kind {
            PositionKind::Aircraft => {
                // A fresh Remote ID position (the same drone, if it was
                // ever merged) wins, as with DJI's.
                let rid_fresh = t.location_src == crate::Source::RemoteId
                    && t.location.as_ref().is_some_and(|l| {
                        now_ms.saturating_sub(l.rx_ms) < crate::dji::RID_PREFERRED_MS
                    });
                if !rid_fresh {
                    t.location = Some(Stamped {
                        value: location_from(p),
                        rx_ms: now_ms,
                    });
                    t.location_src = crate::Source::ControlLink;
                }
                self.control.positions = self.control.positions.saturating_add(1);
            }
            PositionKind::Home => {
                t.home = Some(Stamped {
                    value: ll,
                    rx_ms: now_ms,
                })
            }
        }
        let id = t.id;
        let _ = rssi;
        self.control.links[i].track = Some(id);
    }

    /// Feed one `Frame::ControlEnergy` from `node`.
    pub fn ingest_control_energy(&mut self, node: u8, e: &ControlEnergy, now_ms: u64) {
        self.control.ingest_energy(node, e, now_ms);
    }

    /// Control links, in table order.
    pub fn control_links(&self) -> &[ControlLink] {
        self.control.links()
    }

    /// Control-band energy, per band and node.
    pub fn control_bands(&self) -> &[EnergyBand] {
        self.control.bands()
    }

    /// Counters for the hub's stats line.
    pub fn control_counts(&self) -> (u32, u32, u32, u32) {
        let c = &self.control;
        (c.summaries, c.contents, c.energy, c.positions)
    }

    /// Control links heard within [`Config::stale_after_ms`].
    pub fn active_control_count(&self, now_ms: u64) -> usize {
        self.control
            .links()
            .iter()
            .filter(|l| now_ms.saturating_sub(l.last_seen_ms) < self.config.stale_after_ms)
            .count()
    }
}
