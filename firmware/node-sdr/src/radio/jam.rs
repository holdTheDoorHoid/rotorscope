//! Jamming watch on every dwell of either job (`crates/jamwatch`,
//! docs/JAMMING.md). Receive-only: nothing here tunes, transmits or adds
//! dwell time; it reads what the dwell already captured.
//!
//! - During the dwell every trigger probe's power also goes into
//!   `jamwatch::ProbeStats` (`Radio::probe`), and at its end the decode
//!   budget's pressure (`Radio::end_dwell`): a train of DroneID-length
//!   pulses that floods the decoder counts as pulsed interference.
//! - At the end of the dwell (the ring is stopped anyway for the retune)
//!   [`Radio::jam_judge`] runs a 128-frame 64-point spectrogram over the
//!   newest ring bytes (about 2 ms of CPU, no extra air time), lets the
//!   watch assess the dwell, and only when the watch asks (floor raised,
//!   a tone, a sweep or a pulse: never in a quiet band) checks for link
//!   structure with `fpvscan` on 4096 half-banded samples (about a quarter
//!   of the FPV analysis, ~25 ms by the bring-up timing of the full one).
//!   On the FPV job the dwell's own analysis is reused when it exists.
//! - Findings go to the hub as `Frame::Threat` (each kind at most once a
//!   second) and to the console as `jam` lines; notable dwells print a
//!   `jamhop` line.
//!
//! **Not yet run on a chip** (written 2026-11-17): the thresholds rest on
//! the bring-up noise numbers and synthetic tests.

use core::fmt::Write as _;

use droneid::math::{C16, remove_dc};
use esp_println::println;
use jamwatch::{Band, ProbeStats, Watch};
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::ring::{NODE_BYTES, RING_BYTES};

use super::{Dec1, Event, Hop, Line, Radio, capture, now_us, reader};
use crate::ring;

/// FPV job: channels outside the main sweep, one visited after each round
/// (60 ms, plus up to two DC re-measurements of ~22 ms: about 4 % of a
/// 2.8 s sweep), so a drone on a frequency the plans skip is still looked
/// at every ~30 s, and the jamming watch sees those parts of the band.
///
/// - 5181/5201/5221/5241: the 5.170-5.250 GHz band only DJI O4 (and some
///   Autel) video uses (`djilink::plan::O4_BAND_SWEEP_MHZ`; djilink's
///   `DjiO4Video` verdict needs it). LO 5180-5240 MHz, inside the measured
///   reach (5180-5800); detection there not yet tested on air.
/// - 5820-5945: the 5.8 GHz FPV channels above the C5's measured reach
///   (F5-F8, A1-A3, B6-B8, R6-R8, E5-E8, DJI FPV 5839/5878/5914). LO
///   5819-5944 MHz: ABOVE what has been verified on a chip (5816 MHz, the
///   DroneID plan's top hop). The PHY's own DC calibration visits a 5855
///   MHz reference LO, so the PLL probably reaches it; UNVERIFIED. If a
///   chip cannot lock there these dwells read garbage, cost 4 % of the
///   sweep, and `edge off` on the console turns them off.
///
/// 2.3 GHz edges are not added: 2320-2345 is satellite radio (SDARS, with
/// terrestrial OFDM repeaters in cities) and 2305-2360 US WCS LTE, both
/// continuous OFDM that would need their own classification before they
/// could be told from a drone link. 2370-2510 is already in the sweep.
pub const EDGE_MHZ: [u16; 11] = [5181, 5201, 5221, 5241, 5820, 5840, 5860, 5880, 5905, 5925, 5945];

/// Probe spacing (`rawcap::trigger::Config` default block: 2048 bytes at
/// 40 MS/s).
const PROBE_NS: u32 = 51_200;

/// Samples fed to the link check (half-banded, 20 MS/s: 205 µs).
const LINK_N: usize = 4096;
const LINK_BYTES: usize = 2 * LINK_N + 2 * HB_REACH;
/// Ring bytes looked at: the ring minus the node being written.
const SPEC_BYTES: usize = RING_BYTES - NODE_BYTES;

pub struct Jam {
    pub watch: Watch,
    pub probes: ProbeStats,
    scratch: jamwatch::Scratch,
    /// Threat frames queued, link checks run.
    pub threats: u32,
    pub checks: u32,
    pub last_us: u32,
}

impl Jam {
    pub const fn new() -> Self {
        Jam {
            watch: Watch::new(),
            probes: ProbeStats::new(PROBE_NS),
            scratch: jamwatch::Scratch::new(),
            threats: 0,
            checks: 0,
            last_us: 0,
        }
    }

    /// A dwell on `freq` starts.
    pub fn begin(&mut self, freq: u32) {
        let loud = self.watch.loud_x10(freq);
        self.probes.begin(loud);
    }

    /// A new plan: one round takes the sum of its dwells (+ the FPV edge
    /// visit).
    pub fn set_plan(&mut self, plan: &[Hop], edge: bool) {
        let ms: u32 = plan.iter().map(|h| h.1 as u32).sum::<u32>() + if edge { fpvscan::channels::DWELL_MS as u32 } else { 0 };
        self.watch.set_cycle_ms(ms);
    }
}

fn band_name(b: Band) -> &'static str {
    b.name()
}

impl Radio {
    /// The dwell on `cur_freq` is over and the ring stopped: judge it for
    /// jamming. `fpv_link`: the FPV job's own verdict on its capture, if it
    /// analysed one.
    pub(super) fn jam_judge(&mut self, fpv_link: Option<bool>) {
        if !self.phy_ok || self.hil {
            return;
        }
        let t0 = now_us();
        let (produced, _) = ring::produced();
        let len = (produced.min(SPEC_BYTES as u64)) as usize;
        let start = produced - len as u64;
        let get = reader(start);
        let spec = jamwatch::analyse(
            |k| {
                let (i, q) = rawcap::iq::unpack(get(k));
                (2 * i + 1, 2 * q + 1)
            },
            len,
            15,
            &mut self.jam.scratch,
        );
        let Some(a) = self.jam.watch.assess(self.cur_freq, self.s.mhz, &self.jam.probes, spec.as_ref()) else {
            return;
        };
        let link = if !a.needs_link_check() {
            None
        } else if fpv_link.is_some() && !a.pulsed {
            fpv_link
        } else {
            // Where to look: a loud frame for a pulse, else the newest bytes.
            let at = if a.pulsed { spec.as_ref().and_then(|f| f.pulse_at(len)) } else { Some(len) };
            at.and_then(|at| self.link_check(start, len, at))
        };
        let now_ms = now_us() / 1000;
        let v = self.jam.watch.commit(&a, link, now_ms);
        self.jam.last_us = (now_us() - t0) as u32;
        if v.notable() {
            let mut line = Line::new();
            let _ = write!(
                line,
                r#"{{"t":"jamhop","f":{},"lo":{},"band":"{}","floor":{},"rise":{},"p50":{},"p99":{},"loud":{},"runs":{},"on_us":{},"clip_ppm":{},"occ":{},"link":{},"raised":{},"pulsed":{},"oven":{},"pressure":{}"#,
                v.freq,
                v.lo_mhz,
                band_name(v.band),
                Dec1(v.floor_x10),
                Dec1(v.eff_rise_x10),
                Dec1(v.rise50_x10),
                Dec1(v.loud_rise_x10),
                v.loud_pct,
                a.runs,
                a.on_us,
                v.clip_ppm,
                a.spec.map_or(0, |s| s.occupancy_pct),
                match link {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "null",
                },
                v.raised,
                v.pulsed,
                v.oven,
                v.pressure
            );
            if let Some(t) = v.tone {
                let _ = write!(line, r#","tone":{t}"#);
            }
            if let Some((r, c)) = v.sweep {
                let _ = write!(line, r#","sweep":[{r},{c}]"#);
            }
            let _ = write!(line, r#","us":{}}}"#, self.jam.last_us);
            println!("{}", line.as_str());
        }
        let mut out = heapless::Vec::new();
        self.jam.watch.due(now_ms, &mut out);
        for r in out {
            println!(
                r#"{{"t":"jam","kind":"{}","band":"{}","f":{},"width_khz":{},"level":{},"count":{},"window_ms":{},"detail":{}}}"#,
                r.kind.name(),
                band_name(r.band),
                r.freq,
                r.width_khz,
                r.level_db,
                r.count,
                r.window_ms,
                r.detail
            );
            self.jam.threats += 1;
            let _ = self.events.push(Event::Threat(r.to_sighting(now_us())));
        }
    }

    /// `fpvscan`'s link check on [`LINK_N`] half-banded samples of the
    /// stopped ring window `[start, start + len)` around byte `at`. Uses
    /// the capture bank and the FPV scratch (both free between dwells).
    fn link_check(&mut self, start: u64, len: usize, at: usize) -> Option<bool> {
        if len < LINK_BYTES {
            return None;
        }
        let from = at.saturating_sub(LINK_BYTES / 2).min(len - LINK_BYTES);
        let sc = self.fpv_scratch.as_deref_mut()?;
        let cap = &mut capture()[..LINK_N];
        decimate_nibbles(reader(start + from as u64), cap);
        remove_dc(cap);
        self.c16_len = LINK_N;
        self.fpv_cap = false;
        let x: &[C16] = cap;
        let an = fpvscan::analyse(x, sc);
        let mhz = self.s.mhz + fpvscan::channels::OFFSET_MHZ;
        let e = fpvscan::band::extra(x, sc, &an, mhz);
        self.jam.checks += 1;
        Some(jamwatch::link_from_analysis(&an, e.dsss))
    }

    /// Console `jam`: counters and every hop's floor and latest verdict.
    pub fn jam_report(&self) {
        let w = &self.jam.watch;
        println!(
            r#"{{"t":"jam_state","role":"{}","dwells":{},"reports":{},"threats":{},"checks":{},"stale_ms":{},"last_us":{},"ref":[{},{}]}}"#,
            crate::hub::role_name(self.role),
            w.dwells,
            w.reports,
            self.jam.threats,
            self.jam.checks,
            w.stale_ms(),
            self.jam.last_us,
            Dec1(jamwatch::watch::REF_FLOOR_X10[0]),
            Dec1(jamwatch::watch::REF_FLOOR_X10[1])
        );
        for (f, floor, v) in w.hops() {
            match v {
                Some(v) => println!(
                    r#"{{"t":"jam_hop","f":{f},"floor":{},"rise":{},"link":{},"raised":{},"pulsed":{},"age_ms":{}}}"#,
                    Dec1(floor),
                    Dec1(v.eff_rise_x10),
                    v.link,
                    v.raised,
                    v.pulsed,
                    (now_us() / 1000).saturating_sub(v.t_ms)
                ),
                None => println!(r#"{{"t":"jam_hop","f":{f},"floor":{}}}"#, Dec1(floor)),
            }
        }
    }
}
