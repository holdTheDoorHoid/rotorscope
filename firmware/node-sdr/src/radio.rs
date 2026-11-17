//! The capture loop: settings, the trigger over Path B's ring, freezing a
//! window, measuring and decoding it, snapshots, and dumps.
//!
//! Single-threaded by design: everything here runs in `main`'s task, so the
//! ring, the capture bank and the dump source can never change under a
//! command. The only concurrency is the DMA interrupt, which only counts.

use core::fmt::Write as _;

use droneid::math::{C16, remove_dc};
use esp_println::println;
use rawcap::cmd::{Command, Dcal, Mode};
use rawcap::crc32::Crc32;
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::dump::{self, CHUNK, Enc, Kind, LINE_MAX, Meta};
use rawcap::iq::{POWER, Window};
use rawcap::ring::{NODE_BYTES, RING_BYTES, window_ok};
use rawcap::trigger::{self, Rise, Trigger, db10_x10, extent};
use rawcap::word;

use heapless::Vec as HVec;
use link::{DroneIdRecord, DroneIdSighting, NodeRole, RfKind, RfSighting};

use crate::{engine, phy, ring, route};

mod dji;
mod jam;

/// Something for the hub, drained by `main` after every call into the radio.
#[derive(Clone, Debug)]
pub enum Event {
    /// `hil`: produced by a hardware-in-the-loop run from a recording.
    DroneId { s: DroneIdSighting, hil: bool },
    /// An FPV verdict for the channel at `mhz`.
    Rf { s: RfSighting, mhz: u16 },
    /// Jamming seen on the drone bands (`radio/jam.rs`).
    Threat(link::ThreatSighting),
}

/// One hop: centre in 100 kHz units, dwell ms (as `link::Assignment::plan`).
pub type Hop = (u32, u16);
pub type Plan = HVec<Hop, 48>;

/// The built-in DroneID plan (crates/coverage DRONEID_58 + DRONEID_24, 700 ms
/// each: a burst comes every ~640 ms, so one dwell catches one).
///
/// 5.8 GHz: hops every 10 MHz from 5746.5 to 5816.5. Each hop tunes the
/// whole MHz below it, so the hop's own channel lands at +0.5 MHz and the
/// channel 5 MHz lower at -4.5 MHz, both whole inside the 20 MHz capture.
/// Together they cover DroneID's 5 MHz raster from 5741.5 to 5816.5: the
/// Avata 2 recording (RFUAV, docs/DJI-LINKS.md) sent O4 DroneID at 5741.5,
/// 5751.5, 5756.5, 5761.5, 5771.5, 5776.5, 5791.5 and 5796.5, more than the
/// published seven-channel schedule (which 5756.5/5776.5/5796.5/5816.5
/// still cover, docs/DRONEID-O4.md). 2.4 GHz: the schedule's three, plus
/// 2459.5 (King-Of-Knights) and 2399.5. The O4 follower (`droneid::follow`)
/// visits predicted channels between these hops.
pub const DRONEID_PLAN: [Hop; 13] = [
    (57465, 700),
    (57565, 700),
    (57665, 700),
    (57765, 700),
    (57865, 700),
    (57965, 700),
    (58065, 700),
    (58165, 700),
    (24145, 700),
    (24295, 700),
    (24445, 700),
    (24595, 700),
    (23995, 700),
];

/// DroneID burst centres the receiver accepts in an assignment: the C5's
/// measured reach, 2200-2650 and 5180-5800 MHz LO (100 kHz units), plus
/// the 5806.5 and 5816.5 MHz hops (LO 5806 / 5816 MHz, above the measured
/// range: not yet verified on a chip).
fn droneid_freq_ok(f: u32) -> bool {
    (22_000..=26_500).contains(&f) || (51_800..=58_170).contains(&f)
}

/// O4 follower: arrive this long before a predicted window opens (a retune
/// with a DC calibration costs about 22 ms on 5 GHz).
const FOLLOW_LEAD_US: u64 = 30_000;
/// A plan dwell cut shorter than this by an upcoming prediction is not
/// worth the retune: go to the predicted channel early instead.
const FOLLOW_MIN_DWELL_US: u64 = 120_000;

/// What the O4 follower did, for the stats line.
#[derive(Clone, Copy, Debug, Default)]
pub struct FollowStats {
    /// Predicted windows visited, and those with a valid packet of the
    /// predicted session in them.
    pub visits: u32,
    pub hits: u32,
    /// Settled windows with nothing (passed to the follower as misses).
    pub misses: u32,
}

/// The predicted window the receiver is sitting in.
#[derive(Clone, Copy, Debug)]
struct Visit {
    a: droneid::follow::Appointment,
    /// Tuned before the window opened (settled coverage).
    settled: bool,
    heard: bool,
    /// `n.accepted` when the visit began: a decode during the window means
    /// the ring was stopped for part of it, so a miss would not be fair.
    accepted: u32,
}

/// FPV retunes within this distance of the last DC calibration reuse it (a
/// calibration costs ~22 ms on 5 GHz, a whole dwell is 60 ms). 40 MHz was
/// too far (2026-11-14, c5b): the residual DC offset raised the probe level
/// of L2/L4 by ~10 dB and their (flat, DC-free) noise passed for HDZero.
const FPV_DCAL_MHZ: u16 = 8;

/// Decoder input: 16,384 samples at 20 MS/s (819 µs), in the upper half of
/// SRAM block 1, the same 64 KiB the dump engine writes snapshots into.
pub const CAPTURE_ADDR: usize = engine::SNAPSHOT_ADDR;
pub const CAPTURE_LEN: usize = 16_384;
const _: () = assert!(core::mem::size_of::<C16>() == 4 && core::mem::align_of::<C16>() <= 4);
const _: () = assert!(CAPTURE_ADDR + 4 * CAPTURE_LEN <= 0x4084_0000);

/// Path B window: 40 MS/s bytes feeding [`CAPTURE_LEN`] decimated samples.
pub const WINDOW_BYTES: usize = 2 * CAPTURE_LEN + 2 * HB_REACH;
/// Bytes kept before the first hot probe (~102 µs).
const PRE_BYTES: u64 = 4096;
/// Extent measurement segment: 400 bytes = 10 µs at 40 MS/s.
const SEG: usize = 400;
/// Sample stride when measuring a frozen window's burst (see `window`).
const SUB: usize = 4;
/// Largest Path B grab: the ring minus the node being written.
const GRAB_BYTES: usize = RING_BYTES - NODE_BYTES;

pub const RATE_B: u32 = ring::BYTE_RATE;
pub const RATE_A: u32 = 20_000_000;
pub const RATE_DECODE: u32 = 20_000_000;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub mhz: u16,
    pub gain: u8,
    pub lpf: Option<u8>,
    pub window: Window,
    pub falling_edge: bool,
    pub swap_iq: bool,
    pub trig: trigger::Config,
    pub mode: Mode,
    /// Print serial numbers / UUIDs / key IDs unredacted.
    pub reveal: bool,
    pub pins: [u8; 8],
    /// Fine LO offset, kHz.
    pub offset_khz: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            // The whole MHz below DroneID's 5756.5 MHz channel (OcuSync 2 and
            // O4 both use it): the burst sits at +0.5 MHz, clear of the DC
            // spike (RESEARCH.md "Can a C5 do it?").
            mhz: 5756,
            gain: phy::GAIN_DEFAULT,
            lpf: Some(phy::LPF_DEFAULT),
            window: Window::CLIP,
            falling_edge: false,
            swap_iq: false,
            trig: trigger::Config::default(),
            mode: Mode::Detect,
            reveal: false,
            pins: route::PINS_XIAO,
            offset_khz: 0,
        }
    }
}

/// What the capture bank and ring currently hold for `dump`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Nothing,
    /// A frozen Path B window `[start, start + len)` in the stopped ring.
    Ring { start: u64, len: usize },
    /// Path A words in the bank (raw, not yet converted).
    Snap { words: usize },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub accepted: u32,
    pub rejected: u32,
    pub lapped: u32,
    pub skipped: u32,
    pub bursts: u32,
    pub crc_ok: u32,
    pub gain_lost: u32,
    pub last_decode_ms: u32,
}

/// What a dump streams from (kept so `resend` sends the same bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    RingWindow { start: u64 },
    Words,
    Capture,
}

pub struct Radio {
    pub s: Settings,
    trig: Trigger,
    next_block: u64,
    pending: Option<Rise>,
    held: Held,
    /// The capture bank holds valid decoder input (`CAPTURE_LEN` or fewer samples).
    c16_len: usize,
    pub n: Counters,
    dump_id: u32,
    last_dump: Option<(Meta, Source)>,
    /// Only one non-burst ("ev") line per this many µs.
    ev_quiet_until: u64,
    #[cfg(feature = "decode")]
    rx: Option<&'static mut droneid::rx::Receiver>,
    pub phy_ok: bool,
    /// What the node is doing, the plan it follows and where it is in it.
    pub role: NodeRole,
    plan: Plan,
    pub plan_id: u16,
    hop_idx: usize,
    hop_until_us: u64,
    /// false: parked on `s.mhz` (a console `freq`), the plan is not walked.
    pub hopping: bool,
    /// A console `role` holds the job against hub assignments until `role auto`.
    pub pinned: bool,
    /// What is being listened to now, 100 kHz units (burst or channel centre).
    pub cur_freq: u32,
    pub events: HVec<Event, 8>,
    /// FPV: level meter for this dwell, floor across the sweep, and whether
    /// the capture bank holds this dwell's window.
    meter: fpvscan::Meter,
    floors: fpvscan::band::Floors,
    fpv_cap: bool,
    fpv_scratch: Option<&'static mut fpvscan::Scratch>,
    pub fpv_verbose: bool,
    /// FPV: DJI link fingerprints (`crates/djilink`, `radio/dji.rs`).
    dji: dji::Dji,
    /// Jamming watch over every dwell of either job (`radio/jam.rs`).
    pub jam: jam::Jam,
    /// FPV: visit one channel outside the main sweep after each round
    /// (`radio/jam.rs` `EDGE_MHZ`); `edge` = on that visit now.
    pub edge_on: bool,
    edge: bool,
    edge_idx: usize,
    sweep: SweepStats,
    /// A hardware-in-the-loop run is in progress (events are marked).
    hil: bool,
    /// CRC of the decoder input as built in a HIL run (checked after decoding).
    hil_cap_crc: u32,
    /// Last receive DC calibration (ours), and the setting it was made for.
    dc: phy::DcCal,
    dc_for_mhz: u16,
    dc_auto: bool,
    /// O4 channel follower (`droneid::follow`): sessions, the window being
    /// visited, counters, the start time of the burst being decoded.
    follow: droneid::follow::Follower,
    visit: Option<Visit>,
    pub follow_stats: FollowStats,
    pub follow_on: bool,
    burst_us: u64,
}

fn now_us() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_micros()
}

/// Byte `k` of the stream window starting at `start` (k < RING_BYTES),
/// without a 64-bit modulo per byte (a library call on RV32).
fn reader(start: u64) -> impl Fn(usize) -> u8 {
    let bytes = ring::bytes();
    let off = (start % RING_BYTES as u64) as usize;
    move |k| {
        let i = off + k;
        bytes[if i >= RING_BYTES { i - RING_BYTES } else { i }]
    }
}

fn cap_crc() -> u32 {
    let mut c = Crc32::new();
    // SAFETY: the capture bank as bytes, read only.
    c.update(unsafe { core::slice::from_raw_parts(CAPTURE_ADDR as *const u8, 4 * CAPTURE_LEN) });
    c.finish()
}

fn capture() -> &'static mut [C16] {
    // SAFETY: the upper half of SRAM block 1 is reserved for us
    // (ld/sdr_bank.x); single-threaded use, never across a snapshot.
    unsafe { core::slice::from_raw_parts_mut(CAPTURE_ADDR as *mut C16, CAPTURE_LEN) }
}

impl Radio {
    pub fn new(s: Settings) -> Self {
        let mut r = Radio {
            s,
            trig: Trigger::new(s.trig),
            next_block: 0,
            pending: None,
            held: Held::Nothing,
            c16_len: 0,
            n: Counters::default(),
            dump_id: 0,
            last_dump: None,
            ev_quiet_until: 0,
            #[cfg(feature = "decode")]
            rx: None,
            phy_ok: false,
            role: NodeRole::DroneId,
            plan: Plan::from_slice(&DRONEID_PLAN).unwrap_or_default(),
            plan_id: 0,
            hop_idx: DRONEID_PLAN.len() - 1,
            hop_until_us: 0,
            hopping: true,
            pinned: false,
            cur_freq: s.mhz as u32 * 10 + 5,
            events: HVec::new(),
            meter: fpvscan::Meter::new(),
            floors: fpvscan::band::Floors::new(),
            fpv_cap: false,
            fpv_scratch: None,
            fpv_verbose: false,
            dji: dji::Dji::new(),
            jam: jam::Jam::new(),
            edge_on: true,
            edge: false,
            edge_idx: 0,
            sweep: SweepStats::default(),
            hil: false,
            hil_cap_crc: 0,
            dc: phy::DcCal::default(),
            dc_for_mhz: 0,
            dc_auto: true,
            follow: droneid::follow::Follower::new(),
            visit: None,
            follow_stats: FollowStats::default(),
            follow_on: true,
            burst_us: 0,
        };
        r.jam.set_plan(&DRONEID_PLAN, false);
        r
    }

    pub fn set_fpv_scratch(&mut self, s: &'static mut fpvscan::Scratch) {
        self.fpv_scratch = Some(s);
    }

    #[cfg(feature = "decode")]
    pub fn set_receiver(&mut self, rx: &'static mut droneid::rx::Receiver) {
        self.rx = Some(rx);
    }

    /// Debug-bus lanes for the current window and orientation.
    fn lanes(&self) -> [u8; 8] {
        let l = self.s.window.diag_lanes();
        if self.s.swap_iq { [l[4], l[5], l[6], l[7], l[0], l[1], l[2], l[3]] } else { l }
    }

    fn tune(&self) -> phy::Tune {
        phy::Tune { mhz: self.s.mhz, gain: self.s.gain, lpf: self.s.lpf }
    }

    /// Bring the receiver up: tune, start the debug-bus feed, route it, start
    /// the ring. Refuses the private PHY calls on an unknown blob.
    pub fn boot(&mut self) {
        self.phy_ok = phy::abi_ok();
        if !self.phy_ok {
            println!(
                r#"{{"t":"err","what":"phy_abi","version":"{}","want":"{}","note":"refusing private PHY calls"}}"#,
                phy::version(),
                phy::PHY_VERSION
            );
            return;
        }
        // First the PHY's own boot calibration as found (`dc_cal.n` 0), then,
        // with `dcal auto`, ours at this LO.
        let mut rb = phy::apply(self.tune());
        self.dc = phy::dc_readback();
        self.print_phy(&rb);
        if self.dc_auto {
            rb = self.dc_cal(self.s.mhz);
            self.print_phy(&rb);
        }
        route::connect(&self.s.pins, self.lanes());
        ring::install();
        self.restart();
    }

    /// (Re)start the free-running engine and the ring; forget held data.
    fn restart(&mut self) {
        self.held = Held::Nothing;
        self.pending = None;
        self.next_block = 0;
        self.jam.probes.gap();
        if !self.phy_ok || self.s.mode == Mode::Idle {
            ring::stop();
            engine::stop();
            return;
        }
        engine::free_run();
        ring::start(self.s.falling_edge);
    }

    /// Tune and lock the receiver; with `dcal auto` on, re-measure the DC
    /// offset first whenever the LO changed (the calibration covers every
    /// gain index, so a gain or filter change needs none).
    fn apply(&mut self) -> phy::Readback {
        let rb = phy::apply(self.tune());
        if self.dc_auto && self.dc_for_mhz != self.s.mhz {
            return self.dc_cal(self.s.mhz);
        }
        rb
    }

    /// Run the DC calibration (`mhz` 0 = the PHY's reference LOs), then
    /// re-apply our settings, which rebuilds the gain memory from it and
    /// forces the gain and the AGC off again. The engine stays stopped
    /// until the caller restarts it.
    fn dc_cal(&mut self, mhz: u16) -> phy::Readback {
        ring::stop();
        engine::stop();
        self.dc = phy::dc_recal(mhz, self.dc.n + 1);
        self.dc_for_mhz = self.s.mhz;
        phy::apply(self.tune())
    }

    fn print_phy(&self, rb: &phy::Readback) {
        let d = &self.dc;
        let mut rf = Line::new();
        for (k, e) in d.rf_dc.iter().enumerate() {
            let _ = write!(rf, "{}[{},{}]", if k == 0 { "" } else { "," }, e[0], e[1]);
        }
        println!(
            r#"{{"t":"phy","version":"{}","abi_ok":{},"mhz":{},"gain_forced":{},"gain":{},"gain_max":{},"agc_off":{},"lpf":[{},{}],"txq_off":{},"dc_cal":{{"n":{},"mhz":{},"band":"{}","us":{},"valid":{},"iq_kept":{},"auto":{},"rf_dc":[{}]}}}}"#,
            phy::version(),
            self.phy_ok,
            self.s.mhz,
            rb.gain_forced,
            rb.gain,
            rb.gain_max,
            rb.agc_off,
            rb.lpf[0],
            rb.lpf[1],
            rb.txq_off,
            d.n,
            d.mhz,
            if d.band5 { "5g" } else { "2g4" },
            d.us,
            d.valid,
            d.iq_kept,
            self.dc_auto,
            rf.as_str()
        );
    }

    /// Feed the trigger every probe the ring has ready. Never prints, so it is
    /// safe to call between the pieces of a paced output line.
    pub fn probe(&mut self) {
        if !self.phy_ok || self.s.mode == Mode::Idle || self.held != Held::Nothing {
            return;
        }
        let (produced, _) = ring::produced();
        let bytes = ring::bytes();
        let cfg = self.s.trig;
        // Probes: one per block, never wrapping (the ring is a whole number
        // of 2048-byte blocks).
        while self.pending.is_none() && self.next_block + cfg.probe as u64 <= produced {
            if produced - self.next_block > (RING_BYTES - NODE_BYTES) as u64 {
                // Fell a lap behind (a long command or decode): skip to recent data.
                let to = (produced - (RING_BYTES / 2) as u64) / cfg.block as u64 * cfg.block as u64;
                self.n.skipped += ((to - self.next_block) / cfg.block as u64) as u32;
                self.next_block = to;
                continue;
            }
            let off = (self.next_block % RING_BYTES as u64) as usize;
            let mut p = 0u32;
            let fpv = self.role == NodeRole::Fpv;
            let (mut si, mut sq) = (0i32, 0i32);
            for &b in &bytes[off..off + cfg.probe as usize] {
                p += POWER[b as usize] as u32;
                if fpv {
                    let (i, q) = rawcap::iq::unpack(b);
                    si += 2 * i as i32 + 1;
                    sq += 2 * q as i32 + 1;
                }
            }
            // FPV levels without the probe's DC: a receiver DC residual
            // counts in the raw power and made flat noise read 10-16 dB
            // "loud" on some LOs (2026-11-14). POWER is in (2n+1)^2 units,
            // so the DC power of the probe is (si^2 + sq^2) / n.
            let p_ac = if fpv {
                let dc = ((si * si + sq * sq) as u32) / cfg.probe;
                p.saturating_sub(dc).max(cfg.probe)
            } else {
                p
            };
            // The probe may have been overwritten while summed: re-check.
            let (now, _) = ring::produced();
            if window_ok(self.next_block, self.next_block + cfg.probe as u64, now, NODE_BYTES) {
                self.jam.probes.feed(if fpv { p_ac } else { p });
                if self.role == NodeRole::Fpv {
                    self.meter.feed(p_ac);
                    self.dji.timeline.feed(p_ac);
                    // One capture per dwell is enough; later rises only count.
                    let r = self.trig.feed(self.next_block, p);
                    if !self.fpv_cap && self.pending.is_none() {
                        self.pending = r;
                    }
                } else {
                    self.pending = self.trig.feed(self.next_block, p);
                }
            }
            self.next_block += cfg.block as u64;
        }
    }

    /// Called after the ring signalled new data (and periodically).
    pub fn poll(&mut self) {
        if !self.phy_ok || self.s.mode == Mode::Idle || self.held != Held::Nothing {
            return;
        }
        self.probe();
        let (produced, _) = ring::produced();
        let Some(r) = self.pending else { return };
        let start = r.pos.saturating_sub(PRE_BYTES);
        let end = start + WINDOW_BYTES as u64;
        if produced < end {
            return;
        }
        ring::stop();
        self.pending = None;
        let (produced, _) = ring::produced();
        // When the burst rose: the ring runs at RATE_B bytes a second.
        self.burst_us = now_us().saturating_sub(produced.saturating_sub(r.pos) * 1_000_000 / RATE_B as u64);
        if !window_ok(start, end, produced, NODE_BYTES) {
            self.n.lapped += 1;
            self.restart();
            return;
        }
        if self.role == NodeRole::Fpv {
            self.fpv_take(start);
            self.restart_keep_floor();
            return;
        }
        let keep = self.window(start, r);
        if keep && self.s.mode == Mode::Hold {
            self.held = Held::Ring { start, len: WINDOW_BYTES };
            println!(r#"{{"t":"held","kind":"nib8","pos":{start},"bytes":{WINDOW_BYTES},"note":"dump, then resume"}}"#);
        } else {
            self.restart();
        }
    }

    /// Measure, gate, convert and decode one frozen Path B window. Returns
    /// whether it passed the duration gate.
    fn window(&mut self, start: u64, r: Rise) -> bool {
        let get = reader(start);
        let cfg = self.s.trig;
        // Every SUB-th sample of a segment, scaled back up: the length and level
        // only need a power estimate per 10 µs segment, and this is the costly
        // part of a look (the ring is stopped while it runs: ~4 ms per look at
        // full rate, 2026-11-12). Only the segments around the trigger's hot run
        // are measured when the trigger reported one.
        let seg_power = |k: usize| -> u32 {
            (k * SEG..(k + 1) * SEG).step_by(SUB).map(|j| POWER[get(j) as usize] as u32).sum::<u32>() * SUB as u32
        };
        let segs = WINDOW_BYTES / SEG;
        let (lo, hi) = if r.probes > 0 {
            let at = (r.pos - start) as usize;
            let lo = at.saturating_sub(cfg.block as usize) / SEG;
            let hi = (at + (r.probes as usize + 1) * cfg.block as usize).div_ceil(SEG).min(segs);
            (lo, hi)
        } else {
            (0, segs)
        };
        // Floor per segment, from the trigger's per-probe floor.
        let floor_seg = (r.floor as u64 * SEG as u64 / cfg.probe as u64).max(1);
        let thr = (floor_seg * cfg.ratio_q8 as u64 / 256) as u32;
        let (first, n) = extent((lo..hi).map(seg_power), thr);
        let first = first + lo;
        // u64: (n * SEG) * 1e6 overflows u32 past ~107 µs (found by the HIL test:
        // every DroneID-length burst used to measure as tens of µs and be rejected).
        let dur_us = ((n * SEG) as u64 * 1_000_000 / RATE_B as u64) as u32;
        let mean_seg: u64 = if n > 0 { (first..first + n).map(|k| seg_power(k) as u64).sum::<u64>() / n as u64 } else { 0 };
        // 0 dBFS = both axes on the rails (POWER 450 per sample).
        let lvl = db10_x10(mean_seg.max(1)) - db10_x10(450 * SEG as u64);
        let snr = db10_x10(mean_seg.max(1)) - db10_x10(floor_seg);
        let ok = (cfg.min_us..=cfg.max_us).contains(&dur_us);
        if !ok {
            self.n.rejected += 1;
            let t = now_us();
            if t >= self.ev_quiet_until || self.hil {
                self.ev_quiet_until = t + 200_000;
                println!(
                    r#"{{"t":"ev","pos":{start},"mhz":{},"dur_us":{dur_us},"lvl_db":{},"snr_db":{},"note":"energy, not DroneID length"}}"#,
                    self.s.mhz,
                    Dec1(lvl),
                    Dec1(snr)
                );
            }
            return false;
        }
        self.n.accepted += 1;
        let cap = capture();
        decimate_nibbles(&get, cap);
        remove_dc(cap);
        self.c16_len = CAPTURE_LEN;
        if self.hil {
            self.hil_cap_crc = cap_crc();
        }
        let mut line = Line::new();
        let _ = write!(
            line,
            r#"{{"t":"burst","pos":{start},"mhz":{},"dur_us":{dur_us},"lvl_db":{},"snr_db":{},"gain":{},"win":"{}""#,
            self.s.mhz,
            Dec1(lvl),
            Dec1(snr),
            self.s.gain,
            self.s.window.name()
        );
        let mut found = HVec::<DroneIdSighting, 4>::new();
        self.decode_into(&mut line, CAPTURE_LEN, Some((&mut found, lvl, snr)));
        println!("{}}}", line.as_str());
        for s in found {
            if let DroneIdRecord::O4 { key_id, .. } = &s.record
                && !self.hil
            {
                self.follow_observe(*key_id);
            }
            let _ = self.events.push(Event::DroneId { s, hil: self.hil });
        }
        true
    }

    /// Decode the first `n` capture samples, appending `,"dec":[...]`. With
    /// `out`, also build a hub sighting for every burst that matched the
    /// reference pattern (`lvl`, `snr`: the energy measurement, tenths of a dB).
    fn decode_into(&mut self, line: &mut Line, n: usize, out: Option<(&mut HVec<DroneIdSighting, 4>, i32, i32)>) {
        #[cfg(feature = "decode")]
        {
            let Some(rx) = self.rx.as_deref_mut() else {
                let _ = write!(line, r#","dec":null"#);
                return;
            };
            let t0 = now_us();
            let reveal = self.s.reveal;
            let _ = write!(line, r#","dec":["#);
            let mut first = true;
            let mut n_ok = 0;
            let mut n_all = 0;
            let freq = self.cur_freq;
            // Where a burst on `freq` must appear in the capture (the LO is
            // the whole MHz below it: +500 kHz).
            let expect_hz = freq as i64 * 100_000 - self.s.mhz as i64 * 1_000_000;
            let mut out = out;
            rx.decode_all(&capture()[..n], &mut |res| {
                if let (Some((v, lvl, snr)), Ok(b)) = (out.as_mut(), &res)
                    && let Some(s) = sighting(b, freq, expect_hz, *lvl, *snr)
                {
                    let _ = v.push(s);
                }
                n_all += 1;
                if !first {
                    let _ = write!(line, ",");
                }
                first = false;
                match res {
                    Ok(b) => {
                        n_ok += b.crc24_ok as u32;
                        crate::report::burst_json(line, &b, reveal);
                    }
                    Err((_, e)) => {
                        let _ = write!(line, r#"{{"err":"{e:?}"}}"#);
                    }
                }
            });
            let ms = ((now_us() - t0) / 1000) as u32;
            let _ = write!(line, r#"],"ms":{ms}"#);
            self.n.bursts += n_all;
            self.n.crc_ok += n_ok;
            self.n.last_decode_ms = ms;
        }
        #[cfg(not(feature = "decode"))]
        {
            let _ = (n, out);
            let _ = write!(line, r#","dec":null"#);
        }
    }

    fn stats_line(&self, line: &mut Line) {
        let (produced, overrun) = ring::produced();
        let _ = write!(
            line,
            r#"{{"t":"stats","up_us":{},"role":"{}","plan":{},"hop":{},"f":{},"mode":"{}","mhz":{},"gain":{},"win":"{}","produced":{produced},"overrun":{overrun},"isrs":{},"dma_err":{},"wb_lost":{},"probes":{},"rises":{},"gated":{},"floor":{},"accepted":{},"rejected":{},"lapped":{},"skipped":{},"bursts":{},"crc_ok":{},"decode_ms":{},"gain_lost":{},"held":{},"follow":[{},{},{},{}],"heap_free":{}}}"#,
            now_us(),
            crate::hub::role_name(self.role),
            self.plan_id,
            self.hopping,
            self.cur_freq,
            self.s.mode.name(),
            self.s.mhz,
            self.s.gain,
            self.s.window.name(),
            ring::ISRS.load(core::sync::atomic::Ordering::Relaxed),
            ring::DMA_ERRORS.load(core::sync::atomic::Ordering::Relaxed),
            ring::WB_LOST.load(core::sync::atomic::Ordering::Relaxed),
            self.trig.probes,
            self.trig.rises,
            self.trig.gated,
            self.trig.floor(),
            self.n.accepted,
            self.n.rejected,
            self.n.lapped,
            self.n.skipped,
            self.n.bursts,
            self.n.crc_ok,
            self.n.last_decode_ms,
            self.n.gain_lost,
            self.held != Held::Nothing,
            self.follow.active(),
            self.follow_stats.visits,
            self.follow_stats.hits,
            self.follow_stats.misses,
            esp_alloc::HEAP.free()
        );
    }

    pub fn stats(&self) {
        let mut line = Line::new();
        self.stats_line(&mut line);
        println!("{}", line.as_str());
    }

    /// The once-a-second stats line, written in 128-byte pieces with a probe
    /// pass between them. Printed whole (esp-println, interrupts off) it took
    /// about 1.6 ms over USB, longer than the 1.43 ms ring: the loop fell a lap
    /// behind and the DMA interrupt ran a lap late every second. Rule: nothing
    /// printed while the ring runs may take more than about a millisecond
    /// (~300 bytes); `ev` lines are ~180.
    pub fn stats_paced(&mut self) {
        let mut line = Line::new();
        self.stats_line(&mut line);
        crate::out::paced(line.as_str(), 128, || self.probe());
    }

    /// Housekeeping between captures (about once a second).
    pub fn tick(&mut self) {
        if self.phy_ok && self.held == Held::Nothing && phy::reassert(self.s.gain) {
            self.n.gain_lost += 1;
        }
    }

    // ---------------------------------------------------------- commands --

    pub fn command(&mut self, c: Command) {
        match c {
            Command::Help => help(),
            Command::Stats => self.stats(),
            Command::Phy => self.phy_report(),
            Command::Freq(mhz) => {
                // A hand-tuned frequency parks the node until `hop on`.
                self.hopping = false;
                self.cur_freq = if self.role == NodeRole::DroneId { mhz as u32 * 10 + 5 } else { (mhz as u32 + 1) * 10 };
                self.s.mhz = mhz;
                self.retune();
            }
            Command::OffsetKhz(k) => {
                self.s.offset_khz = k;
                self.retune();
            }
            Command::Pins(alt) => {
                ring::stop();
                route::release(&self.s.pins);
                self.s.pins = if alt { route::PINS_ALT } else { route::PINS_XIAO };
                route::connect(&self.s.pins, self.lanes());
                self.restart();
                ok("pins", if alt { "alt" } else { "xiao" });
            }
            Command::Gain(g) => {
                self.s.gain = g;
                self.retune();
            }
            Command::Lpf(l) => {
                self.s.lpf = l;
                self.retune();
            }
            Command::Dcal(d) => {
                if !self.phy_ok {
                    return err("phy_abi");
                }
                match d {
                    Dcal::Auto(on) => {
                        self.dc_auto = on;
                        ok("dcal_auto", if on { "on" } else { "off" });
                    }
                    Dcal::Now | Dcal::Ref => {
                        self.dc_cal(if d == Dcal::Now { self.s.mhz } else { 0 });
                        self.retune();
                    }
                }
            }
            Command::Win(w) => {
                self.s.window = w;
                route::set_lanes(&self.s.pins, self.lanes());
                self.trig.reset();
                ok("win", w.name());
            }
            Command::Edge(f) => {
                self.s.falling_edge = f;
                self.restart();
                ok("edge", if f { "fall" } else { "rise" });
            }
            Command::Swap(on) => {
                self.s.swap_iq = on;
                route::set_lanes(&self.s.pins, self.lanes());
                self.trig.reset();
                ok("swap", if on { "on" } else { "off" });
            }
            Command::Trig { ratio_db, min_us, max_us } => {
                self.s.trig.ratio_q8 = rawcap::cmd::ratio_q8(ratio_db);
                if let (Some(a), Some(b)) = (min_us, max_us) {
                    self.s.trig.min_us = a;
                    self.s.trig.max_us = b;
                }
                self.trig.cfg = self.s.trig;
                println!(
                    r#"{{"t":"ok","trig_db":{ratio_db},"min_us":{},"max_us":{}}}"#,
                    self.s.trig.min_us, self.s.trig.max_us
                );
            }
            Command::Mode(m) => {
                self.s.mode = m;
                self.restart();
                ok("mode", m.name());
            }
            Command::Snap => {
                self.snap();
            }
            Command::Grab => self.grab(),
            Command::Level => {
                if self.snap() {
                    self.restart();
                }
            }
            Command::Decode => self.decode_held(),
            Command::Dump { raw, enc } => self.dump(raw, enc),
            Command::Resend { first, last } => self.resend(first, last),
            Command::Reveal(on) => {
                self.s.reveal = on;
                ok("reveal", if on { "on" } else { "off" });
            }
            Command::Bootloader => {
                println!(r#"{{"t":"ok","bootloader":"rebooting into download mode"}}"#);
                // Let the line drain before the reset cuts USB.
                let t = now_us();
                while now_us() - t < 50_000 {}
                ring::stop();
                engine::release();
                // SAFETY: stopping the running driver; nothing uses it after.
                let _ = unsafe { esp_wifi_sys_esp32c5::include::esp_wifi_stop() };
                // C5VRX `rf_reboot_to_download`: the USB reset does not reset
                // the modem domain, and the forced clocks plus a running dump
                // writer can keep the ROM loader from answering; so stop them
                // and ask the ROM for download mode across a software reset.
                esp_hal::peripherals::LP_AON::regs().sys_cfg().modify(|_, w| w.force_download_boot().set_bit());
                esp_hal::system::software_reset();
            }
            Command::Resume => {
                self.restart();
                ok("resume", self.s.mode.name());
            }
        }
    }

    fn retune(&mut self) {
        if !self.phy_ok {
            return err("phy_abi");
        }
        ring::stop();
        let mut rb = self.apply();
        if self.s.offset_khz != 0 {
            phy::set_offset_khz(self.s.offset_khz, self.s.gain);
            rb = phy::readback();
        }
        self.trig.reset();
        self.print_phy(&rb);
        self.restart();
    }

    fn phy_report(&self) {
        let rb = phy::readback();
        self.print_phy(&rb);
        let lanes = route::read_lanes(&self.s.pins);
        let mut samples = [0u8; 4];
        for s in samples.iter_mut() {
            *s = route::sample_inputs(&self.s.pins);
        }
        let (produced, overrun) = ring::produced();
        println!(
            r#"{{"t":"bus","pins":{:?},"out_sel":{:?},"pad_samples":{:?},"parl_rx_cnt":{},"dma_node":{},"produced":{produced},"overrun":{overrun},"isrs":{}}}"#,
            self.s.pins,
            lanes,
            samples,
            ring::parl_rx_count(),
            ring::current_node().map(|n| n as i32).unwrap_or(-1),
            ring::ISRS.load(core::sync::atomic::Ordering::Relaxed)
        );
    }

    /// Path A snapshot into the bank; prints level statistics and advice.
    fn snap(&mut self) -> bool {
        if !self.phy_ok {
            err("phy_abi");
            return false;
        }
        ring::stop();
        self.pending = None;
        let _ = phy::reassert(self.s.gain);
        match engine::snapshot(engine::RATE_20, engine::SNAPSHOT_WORDS, 20_000) {
            Ok(us) => {
                self.held = Held::Snap { words: engine::SNAPSHOT_WORDS };
                self.c16_len = 0;
                let s = word::stats(engine::snapshot_words(engine::SNAPSHOT_WORDS));
                let advice = advice(&s, self.s.window);
                println!(
                    r#"{{"t":"snap","words":{},"us":{us},"mhz":{},"gain":{},"dc":[{},{}],"rms":[{},{}],"peak":{},"railed":{},"over128":{},"over256":{},"gain_word":[{},{}],"advice":"{advice}"}}"#,
                    s.n,
                    self.s.mhz,
                    self.s.gain,
                    s.dc_i,
                    s.dc_q,
                    Dec1(s.rms_i_x10 as i32),
                    Dec1(s.rms_q_x10 as i32),
                    s.peak,
                    s.railed,
                    s.over128,
                    s.over256,
                    s.gain_first,
                    s.gain_last
                );
                true
            }
            Err(e) => {
                println!(r#"{{"t":"err","what":"snapshot","e":"{e:?}"}}"#);
                self.restart();
                false
            }
        }
    }

    /// Freeze the whole ring now.
    fn grab(&mut self) {
        if !self.phy_ok || self.s.mode == Mode::Idle {
            return err("ring not running (mode idle?)");
        }
        ring::stop();
        self.pending = None;
        let (produced, overrun) = ring::produced();
        if produced < GRAB_BYTES as u64 {
            println!(r#"{{"t":"err","what":"grab","produced":{produced},"note":"ring has not filled: no data from PARL_IO/DMA (see BRINGUP.md step 6)"}}"#);
            self.restart();
            return;
        }
        let start = produced - GRAB_BYTES as u64;
        self.held = Held::Ring { start, len: GRAB_BYTES };
        self.c16_len = 0;
        // Quick look: mean power and the nibble histogram corners.
        let get = reader(start);
        let mut p = 0u64;
        let mut rails = 0u32;
        for k in 0..GRAB_BYTES {
            let b = get(k);
            p += POWER[b as usize] as u64;
            let (i, q) = rawcap::iq::unpack(b);
            rails += (i == 7 || i == -8 || q == 7 || q == -8) as u32;
        }
        let lvl = db10_x10(p.max(1)) - db10_x10(450 * GRAB_BYTES as u64);
        println!(
            r#"{{"t":"grab","pos":{start},"bytes":{GRAB_BYTES},"overrun":{overrun},"lvl_db":{},"at_rail":{rails},"win":"{}"}}"#,
            Dec1(lvl),
            self.s.window.name()
        );
    }

    fn decode_held(&mut self) {
        match self.held {
            Held::Nothing if self.c16_len > 0 => {}
            Held::Nothing => return err("nothing held: snap, grab or mode hold first"),
            Held::Ring { start, len } => {
                let n = ((len - 2 * HB_REACH) / 2).min(CAPTURE_LEN);
                decimate_nibbles(reader(start), &mut capture()[..n]);
                remove_dc(&mut capture()[..n]);
                self.c16_len = n;
            }
            Held::Snap { words } => {
                // In place: word k and sample k share the same 4 bytes.
                let p = CAPTURE_ADDR as *mut u32;
                let c = CAPTURE_ADDR as *mut C16;
                for k in 0..words {
                    // SAFETY: inside the bank; read before write at the same index.
                    unsafe {
                        let w = p.add(k).read_volatile();
                        c.add(k).write(word::to_c16(w, self.s.swap_iq));
                    }
                }
                remove_dc(&mut capture()[..words]);
                self.c16_len = words;
                // The raw words are gone; only the c16 form can be dumped now.
                self.held = Held::Nothing;
            }
        }
        let mut line = Line::new();
        let _ = write!(line, r#"{{"t":"decode","samples":{},"rate":{}"#, self.c16_len, RATE_DECODE);
        self.decode_into(&mut line, self.c16_len, None);
        println!("{}}}", line.as_str());
    }

    // ------------------------------------------------------------- dumps --

    fn source_byte(&self, src: Source, k: usize) -> u8 {
        match src {
            Source::RingWindow { start } => reader(start)(k),
            Source::Words => {
                // SAFETY: inside the bank (bounds checked by the caller's length).
                let w = unsafe { ((CAPTURE_ADDR + (k & !3)) as *const u32).read_volatile() };
                (w >> (8 * (k & 3))) as u8
            }
            Source::Capture => {
                let c = capture()[k / 4];
                let v = if k & 2 == 0 { c.re } else { c.im };
                (v as u16 >> (8 * (k & 1))) as u8
            }
        }
    }

    fn dump(&mut self, raw: bool, enc: Enc) {
        let (kind, rate, bytes, src, pos) = match (self.held, raw) {
            (Held::Ring { start, len }, true) => (Kind::Nib8, RATE_B, len, Source::RingWindow { start }, start),
            (Held::Snap { words }, true) => (Kind::W32, RATE_A, 4 * words, Source::Words, 0),
            (_, false) | (Held::Nothing, true) if self.c16_len > 0 => {
                (Kind::C16, RATE_DECODE, 4 * self.c16_len, Source::Capture, 0)
            }
            (Held::Snap { .. }, false) => return err("dump c16 needs decode first (it converts the snapshot)"),
            (Held::Ring { .. }, false) => return err("dump c16 needs decode first"),
            _ => return err("nothing held: snap, grab or mode hold first"),
        };
        let mut crc = Crc32::new();
        let mut chunk = [0u8; CHUNK];
        let mut k = 0;
        while k < bytes {
            let n = CHUNK.min(bytes - k);
            for (j, c) in chunk[..n].iter_mut().enumerate() {
                *c = self.source_byte(src, k + j);
            }
            crc.update(&chunk[..n]);
            k += n;
        }
        self.dump_id = self.dump_id.wrapping_add(1);
        let meta = Meta {
            id: self.dump_id,
            kind,
            rate,
            bytes: bytes as u32,
            freq_khz: self.s.mhz as u32 * 1000,
            gain: self.s.gain,
            lsb: self.s.window.lsb(),
            swap_iq: self.s.swap_iq,
            lpf: self.s.lpf.map(|v| v as i8).unwrap_or(-1),
            enc,
            t_us: now_us(),
            pos,
            crc: crc.finish(),
        };
        self.last_dump = Some((meta, src));
        if !crate::out::line(format_args!("@H {meta}")) {
            return;
        }
        if !self.send_lines(&meta, src, 0, meta.lines().saturating_sub(1)) {
            return;
        }
        let mut buf = [0u8; LINE_MAX];
        crate::out::line(format_args!("{}", dump::end_line(&mut buf, &meta)));
    }

    /// Returns false if the host stopped reading (the rest is not sent).
    fn send_lines(&self, meta: &Meta, src: Source, first: u32, last: u32) -> bool {
        let mut buf = [0u8; LINE_MAX];
        let mut chunk = [0u8; CHUNK];
        for seq in first..=last.min(meta.lines().saturating_sub(1)) {
            let a = seq as usize * CHUNK;
            let n = CHUNK.min(meta.bytes as usize - a);
            for (j, c) in chunk[..n].iter_mut().enumerate() {
                *c = self.source_byte(src, a + j);
            }
            if !crate::out::line(format_args!("{}", dump::data_line(&mut buf, meta.id, seq, &chunk[..n], meta.enc))) {
                return false;
            }
        }
        true
    }

    fn resend(&self, first: u32, last: u32) {
        let Some((meta, src)) = self.last_dump else { return err("no dump to resend") };
        // The source must not have changed since.
        let intact = match src {
            Source::RingWindow { start } => self.held == Held::Ring { start, len: meta.bytes as usize },
            Source::Words => matches!(self.held, Held::Snap { .. }),
            Source::Capture => self.c16_len * 4 == meta.bytes as usize,
        };
        if !intact {
            return err("the dumped capture has been replaced; dump again");
        }
        if self.send_lines(&meta, src, first, last) {
            let mut buf = [0u8; LINE_MAX];
            crate::out::line(format_args!("{}", dump::end_line(&mut buf, &meta)));
        }
    }
}

// ------------------------------------------------- plan, FPV, HIL, uplink --

/// Per-sweep counts of the FPV scanner's verdicts.
#[derive(Clone, Copy, Debug, Default)]
struct SweepStats {
    n: u32,
    t0_us: u64,
    kinds: [u16; 8],
    analysed: u16,
    analyse_us_max: u32,
}

fn kind_index(k: fpvscan::Kind) -> usize {
    use fpvscan::Kind::*;
    match k {
        Quiet => 0,
        AnalogVideo => 1,
        FpvCarrier => 2,
        Hdzero => 3,
        LteStyle => 4,
        WifiNarrow => 5,
        Wifi => 6,
        Energy => 7,
    }
}

const KIND_NAMES: [&str; 8] = ["quiet", "analog_video", "fpv_carrier", "hdzero", "lte_style", "wifi_narrow", "wifi", "energy"];

fn rf_kind(k: fpvscan::Kind) -> Option<RfKind> {
    use fpvscan::Kind::*;
    Some(match k {
        AnalogVideo => RfKind::AnalogVideo,
        FpvCarrier => RfKind::FpvCarrier,
        Hdzero => RfKind::Hdzero,
        LteStyle => RfKind::LteStyle,
        WifiNarrow => RfKind::WifiNarrow,
        Energy => RfKind::Energy,
        Quiet | Wifi => return None,
    })
}

/// Largest distance (Hz) of an undecoded burst from where a DroneID burst on
/// the tuned frequency must appear, for it to count as `Detected`: two
/// crystals of +-20 ppm at 5.8 GHz, plus margin.
const DETECT_CFO_HZ: f64 = 300_000.0;

/// The hub sighting for one burst that passed the reference-pattern check.
///
/// A CRC-valid burst is always reported. One that did not decode becomes
/// `Detected` only if its reference symbols carry positive SNR and it sits
/// within [`DETECT_CFO_HZ`] of the expected offset `expect_hz`: on the bench
/// (2026-11-14, 200 s, 1562 Wi-Fi frames of DroneID length decoded) nine Wi-Fi
/// frames passed the decoder's cyclic-prefix and Zadoff-Chu thresholds, all
/// with reference SNR below -6 dB and 3-6 MHz off; none passes this gate.
#[cfg(feature = "decode")]
fn sighting(b: &droneid::rx::Burst, freq: u32, expect_hz: i64, lvl_x10: i32, _snr_x10: i32) -> Option<DroneIdSighting> {
    if !b.crc24_ok && (b.snr_db < 0.0 || (b.cfo_hz - expect_hz as f64).abs() > DETECT_CFO_HZ) {
        return None;
    }
    use droneid::payload::Record;
    let record = match (&b.parsed, b.crc24_ok) {
        (Some(p), true) => match &p.record {
            Record::FlightInfo(f) => DroneIdRecord::Flight {
                serial: f.serial,
                lat_raw: f.lat_raw,
                lon_raw: f.lon_raw,
                altitude_raw: f.altitude_raw,
                height_raw: f.height_raw,
                v_north: f.v_north,
                v_east: f.v_east,
                v_up: f.v_up,
                yaw_raw: f.yaw_raw,
                pilot_lat_raw: f.app_lat_raw,
                pilot_lon_raw: f.app_lon_raw,
                home_lat_raw: f.home_lat_raw,
                home_lon_raw: f.home_lon_raw,
                product_type: f.product_type,
            },
            // O4: only records that pass every structural check of the
            // spec count; the raw logical object goes up with them.
            Record::Encrypted(e) if e.valid() => DroneIdRecord::O4 {
                kind: match e.kind {
                    droneid::payload::EncKind::Cryp => link::O4Kind::Cryp,
                    droneid::payload::EncKind::Infp10 => link::O4Kind::Infp10,
                    droneid::payload::EncKind::Infp11 => link::O4Kind::Infp11,
                },
                key_id: e.key_id,
                object: e.logical_object(&b.block).unwrap_or(&[]).iter().copied().collect(),
            },
            _ => DroneIdRecord::Detected,
        },
        // The reference pattern matched (that is what an `Ok` burst means)
        // but the block did not decode.
        _ => DroneIdRecord::Detected,
    };
    Some(DroneIdSighting {
        freq,
        // No calibrated dBm on this receiver: the burst level in dB below
        // the 4-bit window's full scale.
        rssi: (lvl_x10 / 10).clamp(-128, 127) as i8,
        snr_db: (b.snr_db as i32).clamp(-128, 127) as i8,
        uptime_us: now_us(),
        record,
    })
}

impl Radio {
    /// Restart the ring without forgetting the trigger floor (FPV, after a
    /// mid-dwell capture).
    fn restart_keep_floor(&mut self) {
        self.restart();
    }

    pub fn plan(&self) -> &[Hop] {
        &self.plan
    }

    /// Apply a hub assignment. Idempotent: the hub resends it every few
    /// seconds, and the same role and plan id change nothing. Returns false
    /// for a role this image cannot do (it keeps its current job).
    pub fn assign(&mut self, role: NodeRole, plan: &[Hop], id: u16) -> bool {
        if !matches!(role, NodeRole::DroneId | NodeRole::Fpv) {
            return false;
        }
        if role == self.role && id == self.plan_id && self.hopping {
            return true;
        }
        let mut p = Plan::new();
        for &(f, ms) in plan {
            let ok = match role {
                NodeRole::DroneId => droneid_freq_ok(f),
                _ => fpvscan::band::reachable(((f + 5) / 10) as u16),
            };
            if ok && ms > 0 {
                let _ = p.push((f, ms));
            }
        }
        if p.is_empty() {
            match role {
                NodeRole::DroneId => p.extend_from_slice(&DRONEID_PLAN).ok(),
                _ => {
                    // Both bands: 2.4 GHz analog video, then 5.8 GHz.
                    for &mhz in fpvscan::band::sweep() {
                        let _ = p.push((mhz as u32 * 10, fpvscan::channels::DWELL_MS));
                    }
                    Some(())
                }
            };
        }
        self.set_role(role);
        self.plan = p;
        self.plan_id = id;
        self.jam.set_plan(&self.plan, role == NodeRole::Fpv && self.edge_on);
        self.hopping = true;
        self.hop_idx = self.plan.len() - 1;
        self.hop_until_us = 0;
        true
    }

    /// Switch job, keeping the current plan only if it is the role's built-in one.
    pub fn set_role(&mut self, role: NodeRole) {
        if role == self.role {
            return;
        }
        self.role = role;
        self.trig.cfg = if role == NodeRole::Fpv {
            // Report every rise at once (no burst-length gate): the scanner
            // wants one look at whatever is loud, Wi-Fi frames included.
            trigger::Config { gate: false, ..self.s.trig }
        } else {
            self.s.trig
        };
        self.trig.reset();
        self.floors.reset();
        self.jam.watch.reset();
        self.sweep = SweepStats { t0_us: now_us(), ..SweepStats::default() };
        self.events.clear();
        self.visit = None;
    }

    /// The built-in plan of the current role (plan id 0).
    pub fn builtin_plan(&mut self) {
        let role = self.role;
        self.plan_id = u16::MAX; // force the reload
        self.assign(role, &[], 0);
    }

    /// Whether the current dwell is over (always false while parked).
    pub fn dwell_over(&self) -> bool {
        self.hopping && self.phy_ok && self.held == Held::Nothing && now_us() >= self.hop_until_us
    }

    /// End the current dwell (FPV: judge the channel) without retuning.
    pub fn end_dwell(&mut self) {
        ring::stop();
        if self.hop_until_us == 0 {
            return;
        }
        let link = if self.role == NodeRole::Fpv { self.fpv_judge() } else { None };
        self.jam_judge(link);
    }

    /// A valid O4 record of session `key_id` was decoded from the burst at
    /// `burst_us` on the current frequency.
    fn follow_observe(&mut self, key_id: [u8; 4]) {
        let id = u32::from_be_bytes(key_id);
        let r = self.follow.observe(id, self.burst_us, self.cur_freq);
        if let Some(v) = self.visit.as_mut()
            && v.a.id == id
        {
            v.heard = true;
        }
        if r == Some(droneid::follow::Observed::Restarted) {
            // The packet fits no hypothesis (another schedule, or lost
            // timing): the follower started over from it.
            println!(r#"{{"t":"follow","ev":"restart","f":{}}}"#, self.cur_freq);
        }
    }

    /// Close the predicted window just visited: a settled, uninterrupted
    /// window with nothing valid in it is a miss for the follower.
    fn close_visit(&mut self) {
        let Some(v) = self.visit.take() else { return };
        if v.heard {
            self.follow_stats.hits += 1;
        } else if v.settled && self.n.accepted == v.accepted {
            self.follow_stats.misses += 1;
            if !self.follow.missed(&v.a) {
                println!(r#"{{"t":"follow","ev":"lost","note":"back to scanning"}}"#);
            }
        }
    }

    /// The follower's next window, when one is due before the plan's next
    /// dwell would end: tune there and dwell over it. Returns the cap on
    /// the plan dwell otherwise (µs from now), if a window is coming.
    fn follow_hop(&mut self) -> Result<(), Option<u64>> {
        if self.role != NodeRole::DroneId || !self.follow_on {
            return Err(None);
        }
        let now = now_us();
        let Some(a) = self.follow.next(now) else { return Err(None) };
        let until_lead = a.start_us.saturating_sub(now + FOLLOW_LEAD_US);
        if until_lead >= FOLLOW_MIN_DWELL_US {
            return Err(Some(until_lead));
        }
        self.cur_freq = a.freq;
        self.tune_quiet((a.freq / 10) as u16, 0);
        let t = now_us();
        self.hop_until_us = a.end_us.max(t + 5_000);
        self.visit = Some(Visit { a, settled: t <= a.start_us, heard: false, accepted: self.n.accepted });
        self.follow_stats.visits += 1;
        Ok(())
    }

    /// Tune to the next hop of the plan and start its dwell (or, on the
    /// DroneID job, to a channel the O4 follower predicts).
    pub fn next_hop(&mut self) {
        if self.plan.is_empty() {
            self.hopping = false;
            return;
        }
        self.close_visit();
        let cap_us = match self.follow_hop() {
            Ok(()) => return,
            Err(c) => c,
        };
        // FPV: after the last channel of a round, one channel outside the
        // main sweep (rotating), before the next round starts.
        if self.role == NodeRole::Fpv && self.edge_on && !self.edge && self.hop_idx + 1 == self.plan.len() {
            self.edge = true;
            let mhz = jam::EDGE_MHZ[self.edge_idx % jam::EDGE_MHZ.len()];
            self.edge_idx = self.edge_idx.wrapping_add(1);
            self.cur_freq = mhz as u32 * 10;
            self.tune_quiet(mhz - fpvscan::channels::OFFSET_MHZ, FPV_DCAL_MHZ);
            self.hop_until_us = now_us() + fpvscan::channels::DWELL_MS as u64 * 1000;
            return;
        }
        self.edge = false;
        self.hop_idx = (self.hop_idx + 1) % self.plan.len();
        if self.hop_idx == 0 && self.role == NodeRole::Fpv && self.sweep.n > 0 {
            self.sweep_line();
        }
        let (f, ms) = self.plan[self.hop_idx];
        self.cur_freq = f;
        let (lo, tol) = match self.role {
            NodeRole::Fpv => (fpvscan::band::lo_for(((f + 5) / 10) as u16).0, FPV_DCAL_MHZ),
            // The whole MHz below the burst centre: +0.5 MHz off the DC spike.
            _ => ((f / 10) as u16, 0),
        };
        self.tune_quiet(lo, tol);
        let dwell = (ms as u64 * 1000).min(cap_us.unwrap_or(u64::MAX));
        self.hop_until_us = now_us() + dwell;
    }

    /// Retune without printing (one `phy` line per hop would flood the
    /// console). Re-measures the DC offset when the LO moved more than `tol`
    /// MHz from the last measurement or changed band.
    fn tune_quiet(&mut self, mhz: u16, tol: u16) {
        if !self.phy_ok {
            return;
        }
        ring::stop();
        self.s.mhz = mhz;
        let band_changed = (mhz < 4000) != (self.dc_for_mhz < 4000);
        if self.dc_auto && (band_changed || mhz.abs_diff(self.dc_for_mhz) > tol) {
            self.dc_cal(mhz);
        } else {
            phy::apply(self.tune());
        }
        self.trig.reset();
        self.meter.reset();
        self.jam.begin(self.cur_freq);
        self.fpv_cap = false;
        self.restart();
    }

    /// FPV: decimate the frozen window at `start` into the capture bank.
    fn fpv_take(&mut self, start: u64) {
        let cap = capture();
        decimate_nibbles(reader(start), cap);
        remove_dc(cap);
        self.c16_len = CAPTURE_LEN;
        self.fpv_cap = true;
    }

    /// FPV: the dwell is over; measure, analyse if loud, classify, report.
    /// Returns, when the capture was analysed, whether it held a recognised
    /// link (for the jamming watch).
    fn fpv_judge(&mut self) -> Option<bool> {
        ring::stop();
        if !self.fpv_cap {
            // Nothing rose: a quiet channel, or a continuous transmitter the
            // floor followed. Take the newest window.
            let (produced, _) = ring::produced();
            let need = (WINDOW_BYTES + NODE_BYTES) as u64;
            if produced >= need {
                self.fpv_take(produced - need);
            }
        }
        let mhz = ((self.cur_freq + 5) / 10) as u16;
        // One floor per band: the receiver's noise differs between them.
        let floor = self.floors.update(mhz, &self.meter);
        let d = self.meter.dwell(floor);
        let t0 = now_us();
        // Also analyse a dwell whose loudest probes are strong though its
        // 90th percentile is not: short bursts, e.g. a hopping controller.
        let bursty = self.dji.timeline.loud_x10(floor) >= fpvscan::classify::STRONG_DB as i32 * 10;
        let (a, extra) = match (&mut self.fpv_scratch, self.fpv_cap && (d.snr_db >= fpvscan::classify::STRONG_DB || bursty)) {
            (Some(sc), true) => {
                let x = &capture()[..self.c16_len];
                let a = fpvscan::analyse(x, sc);
                // 2.4 GHz only (DSSS and line-shape checks); zero on 5.8.
                let e = fpvscan::band::extra(x, sc, &a, mhz);
                (Some(a), e)
            }
            _ => (None, fpvscan::band::Extra::default()),
        };
        let us = (now_us() - t0) as u32;
        let kind = fpvscan::band::classify(&d, a.as_ref(), &extra, mhz);
        let dji = self.dji_judge(kind, floor, a.is_some());
        self.sweep.n += 1;
        self.sweep.kinds[kind_index(kind)] += 1;
        if a.is_some() {
            self.sweep.analysed += 1;
            self.sweep.analyse_us_max = self.sweep.analyse_us_max.max(us);
        }
        if kind != fpvscan::Kind::Quiet || self.fpv_verbose || dji.as_ref().is_some_and(|j| j.verdict.is_some()) {
            let band = fpvscan::band::Band::of(mhz);
            let mut line = Line::new();
            let _ = match fpvscan::band::label(mhz) {
                Some(l) => write!(line, r#"{{"t":"fpv","mhz":{mhz},"ch":"{l}","band":"{}""#, band.name()),
                None => write!(line, r#"{{"t":"fpv","mhz":{mhz},"ch":"?","band":"{}""#, band.name()),
            };
            let _ = write!(
                line,
                r#","lo":{},"edge":{},"snr_db":{},"duty":{},"floor":{},"probes":{},"kind":"{}""#,
                self.s.mhz,
                self.edge,
                d.snr_db,
                d.duty_pct,
                Dec1(floor),
                self.meter.count(),
                kind.name()
            );
            if let Some(a) = &a {
                let _ = write!(
                    line,
                    r#","line":{},"line_hz":{},"cp":[{},{},{}],"cp_thr":{},"width_khz":{},"flat":{},"centroid_khz":{},"us":{us}"#,
                    a.line,
                    a.line_hz,
                    a.cp_wifi,
                    a.cp_half,
                    a.cp_lte,
                    fpvscan::classify::cp_threshold(a),
                    a.width_khz,
                    Dec1(a.flat_x10 as i32),
                    a.centroid_khz
                );
                if band == fpvscan::band::Band::G24 {
                    let _ = write!(line, r#","dsss":{},"sub_line":{},"half_r":{}"#, extra.dsss, extra.sub_line, extra.half_r);
                }
            }
            if let Some(j) = &dji {
                j.log(&mut line);
            }
            println!("{}}}", line.as_str());
        }
        let link = a.map(|_| {
            use fpvscan::Kind::*;
            matches!(kind, AnalogVideo | LteStyle | Wifi | WifiNarrow)
                || (kind == Energy && extra.dsss >= fpvscan::dsss::DSSS_MIN)
                || dji.as_ref().is_some_and(|j| j.verdict.is_some())
        });
        if let Some((k, freq, width_khz, hopping)) = dji.and_then(|j| j.report) {
            let s = RfSighting { kind: k, freq, width_khz, snr_db: d.snr_db, duty_pct: d.duty_pct, line_hz: 0, uptime_us: now_us() };
            // A hopping controller is one emitter: rate-limit it as one
            // (channel 0), not once per channel it hops through.
            let _ = self.events.push(Event::Rf { s, mhz: if hopping { 0 } else { mhz } });
        } else if let Some(k) = rf_kind(kind).filter(|_| kind.reported()) {
            let s = RfSighting {
                kind: k,
                freq: self.cur_freq,
                width_khz: a.map(|a| a.width_khz).unwrap_or(0),
                snr_db: d.snr_db,
                duty_pct: d.duty_pct,
                line_hz: if k == RfKind::AnalogVideo { a.map(|a| a.line_hz).unwrap_or(0) } else { 0 },
                uptime_us: now_us(),
            };
            let _ = self.events.push(Event::Rf { s, mhz });
        }
        link
    }

    fn sweep_line(&mut self) {
        let sw = self.sweep;
        let mut line = Line::new();
        let _ = write!(
            line,
            r#"{{"t":"sweep","channels":{},"ms":{},"floor":{},"floor_2g4":{},"analysed":{},"analyse_us_max":{}"#,
            sw.n,
            (now_us() - sw.t0_us) / 1000,
            Dec1(self.floors.get(fpvscan::band::Band::G58).unwrap_or(0)),
            Dec1(self.floors.get(fpvscan::band::Band::G24).unwrap_or(0)),
            sw.analysed,
            sw.analyse_us_max
        );
        for (k, &c) in sw.kinds.iter().enumerate() {
            let _ = write!(line, r#","{}":{c}"#, KIND_NAMES[k]);
        }
        println!("{}}}", line.as_str());
        self.sweep = SweepStats { t0_us: now_us(), ..SweepStats::default() };
    }

    // ---------------------------------------------- hardware in the loop --

    /// Run the live capture path over recorded Path B bytes: they are copied
    /// into the stopped ring, probed by a fresh trigger exactly as the ring
    /// would be, and the first rise goes through [`Radio::window`] (length
    /// gate, half-band, DC removal, decoder, `burst` line, hub sightings,
    /// marked as HIL). Returns (rises, accepted, bursts matched, CRC-valid,
    /// decode ms, whether the ring still held exactly the fed bytes after
    /// the run).
    pub fn hil_run(&mut self, data: &[u8]) -> (u32, u32, u32, u32, u32, bool) {
        ring::stop();
        engine::stop();
        self.pending = None;
        // Let a DMA write already in flight land before we overwrite the ring.
        let t = now_us();
        while now_us() - t < 200 {}
        let n = data.len().min(GRAB_BYTES);
        // SAFETY: ring and engine stopped; single-threaded.
        unsafe { ring::bytes_mut()[..n].copy_from_slice(&data[..n]) };
        let (acc0, bursts0, crc0) = (self.n.accepted, self.n.bursts, self.n.crc_ok);
        let cfg = self.s.trig;
        let mut trig = Trigger::new(cfg);
        let mut rises = 0;
        let mut pos = 0usize;
        self.hil = true;
        self.n.last_decode_ms = 0;
        while pos + cfg.probe as usize <= n {
            let p = rawcap::iq::power_sum(&data[pos..pos + cfg.probe as usize]);
            if let Some(r) = trig.feed(pos as u64, p) {
                rises += 1;
                let start = r.pos.saturating_sub(PRE_BYTES);
                if start as usize + WINDOW_BYTES <= n {
                    self.window(start, r);
                }
                break;
            }
            pos += cfg.block as usize;
        }
        self.hil = false;
        // The ring must still hold the fed bytes, and the decoder input must
        // not have changed while it was decoded (nothing but the CPU may
        // write SRAM block 1 while the engine is stopped).
        let intact = ring::bytes()[..n] == data[..n] && (self.n.accepted == acc0 || cap_crc() == self.hil_cap_crc);

        let out = (
            rises,
            self.n.accepted - acc0,
            self.n.bursts - bursts0,
            self.n.crc_ok - crc0,
            self.n.last_decode_ms,
            intact,
        );
        self.restart();
        out
    }

    // ------------------------------------------------------------ uplink --

    #[cfg_attr(not(feature = "espnow"), allow(dead_code, reason = "used by the espnow uplink"))]
    /// Stop capturing for good (a failed re-lock): mode idle.
    pub fn halt(&mut self) {
        self.s.mode = Mode::Idle;
        self.hopping = false;
        self.restart();
    }

    #[cfg_attr(not(feature = "espnow"), allow(dead_code, reason = "used by the espnow uplink"))]
    /// Stop capturing before the uplink takes the radio.
    pub fn suspend(&mut self) {
        ring::stop();
        engine::stop();
        self.pending = None;
    }

    /// Take the radio back after the uplink: tune (the Wi-Fi driver left the
    /// PHY on 2.4 GHz channel 6 with its own gain and AGC), re-measure the DC
    /// offset, force gain and AGC off, stop and check the MAC transmit queues
    /// again, re-route the bus and restart the ring. Returns the transmit
    /// queues' read-back (true = all off, the receive-only state).
    #[cfg_attr(not(feature = "espnow"), allow(dead_code, reason = "used by the espnow uplink"))]
    pub fn resume(&mut self) -> bool {
        if !self.phy_ok {
            return false;
        }
        self.dc_for_mhz = 0;
        let rb = self.apply();
        route::set_lanes(&self.s.pins, self.lanes());
        self.trig.reset();
        self.meter.reset();
        self.jam.begin(self.cur_freq);
        self.fpv_cap = false;
        self.restart();
        rb.txq_off && rb.gain_forced && rb.agc_off
    }
}

/// Gain/window advice from a snapshot's level statistics. The decoder
/// research wants noise at about one 4-bit step in the clip window (64
/// codes RMS) and, for the fold windows, peaks inside the fold span.
fn advice(s: &word::Stats, w: Window) -> &'static str {
    let rms = s.rms_i_x10.max(s.rms_q_x10) / 10;
    if s.railed > s.n / 1000 {
        return "ADC railing: lower gain";
    }
    if s.gain_first != s.gain_last {
        return "gain changed during capture: AGC not disabled?";
    }
    if w == Window::CLIP {
        if rms < 24 {
            "clip: noise under 0.4 step, raise gain (or try win fold)"
        } else if rms > 160 {
            "clip: noise over 2.5 steps, lower gain"
        } else {
            "clip: noise in range"
        }
    } else if s.peak as i32 >= w.span() as i32 {
        "fold: peaks exceed the fold span, lower gain or use win clip"
    } else if rms * 4 < w.span() as u32 / 8 {
        "fold: noise very low, raise gain"
    } else {
        "fold: in range"
    }
}

fn ok(what: &str, v: &str) {
    println!(r#"{{"t":"ok","{what}":"{v}"}}"#);
}

fn err(what: &str) {
    println!(r#"{{"t":"err","what":"{what}"}}"#);
}

fn help() {
    for l in [
        "help | stats | phy",
        "freq <MHz> | gain <idx> | lpf <0-63>|off | win clip|fold5|fold | edge rise|fall | swap on|off",
        "dcal [ref | auto on|off] (receive DC-offset calibration)",
        "trig <dB> [min_us max_us] | mode detect|hold|idle | reveal on|off",
        "snap (Path A, 10-bit 20 MS/s) | grab (Path B ring) | level | decode",
        "dump [raw|c16] [b64|hex] | resend <first> [last] | resume",
        "role droneid|fpv | hop on|off | plan | assign droneid|fpv <id> [<f100k>:<ms> ...]",
        "fpv verbose on|off | uplink [now] | hil run <len> <crc32> (after @F lines)",
        "jam (jamming watch state) | edge on|off (FPV off-plan channel each round)",
    ] {
        println!(r#"{{"t":"help","l":"{l}"}}"#);
    }
}

/// Tenths as a decimal (`-123` -> `-12.3`).
pub struct Dec1(pub i32);

impl core::fmt::Display for Dec1 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let v = self.0;
        let s = if v < 0 { "-" } else { "" };
        write!(f, "{s}{}.{}", v.unsigned_abs() / 10, v.unsigned_abs() % 10)
    }
}

/// A fixed buffer for one output line (no heap).
pub struct Line {
    buf: [u8; 2048],
    n: usize,
}

impl Line {
    pub fn new() -> Self {
        Line { buf: [0; 2048], n: 0 }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.n]).unwrap_or("")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        if self.n + b.len() > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.n..self.n + b.len()].copy_from_slice(b);
        self.n += b.len();
        Ok(())
    }
}
