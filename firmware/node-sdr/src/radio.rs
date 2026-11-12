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

use crate::{engine, phy, ring, route};

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
    /// Last receive DC calibration (ours), and the setting it was made for.
    dc: phy::DcCal,
    dc_for_mhz: u16,
    dc_auto: bool,
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

fn capture() -> &'static mut [C16] {
    // SAFETY: the upper half of SRAM block 1 is reserved for us
    // (ld/sdr_bank.x); single-threaded use, never across a snapshot.
    unsafe { core::slice::from_raw_parts_mut(CAPTURE_ADDR as *mut C16, CAPTURE_LEN) }
}

impl Radio {
    pub fn new(s: Settings) -> Self {
        Radio {
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
            dc: phy::DcCal::default(),
            dc_for_mhz: 0,
            dc_auto: true,
        }
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

    /// Called after the ring signalled new data (and periodically).
    pub fn poll(&mut self) {
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
            for &b in &bytes[off..off + cfg.probe as usize] {
                p += POWER[b as usize] as u32;
            }
            // The probe may have been overwritten while summed: re-check.
            let (now, _) = ring::produced();
            if window_ok(self.next_block, self.next_block + cfg.probe as u64, now, NODE_BYTES) {
                self.pending = self.trig.feed(self.next_block, p);
            }
            self.next_block += cfg.block as u64;
        }
        let Some(r) = self.pending else { return };
        let start = r.pos.saturating_sub(PRE_BYTES);
        let end = start + WINDOW_BYTES as u64;
        if produced < end {
            return;
        }
        ring::stop();
        self.pending = None;
        let (produced, _) = ring::produced();
        if !window_ok(start, end, produced, NODE_BYTES) {
            self.n.lapped += 1;
            self.restart();
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
        let seg_power = |k: usize| -> u32 { (k * SEG..(k + 1) * SEG).map(|j| POWER[get(j) as usize] as u32).sum() };
        let segs = WINDOW_BYTES / SEG;
        // Floor per segment, from the trigger's per-probe floor.
        let floor_seg = (r.floor as u64 * SEG as u64 / cfg.probe as u64).max(1);
        let thr = (floor_seg * cfg.ratio_q8 as u64 / 256) as u32;
        let (first, n) = extent((0..segs).map(seg_power), thr);
        let dur_us = (n * SEG) as u32 * 1_000_000 / RATE_B;
        let mean_seg: u64 = if n > 0 { (first..first + n).map(|k| seg_power(k) as u64).sum::<u64>() / n as u64 } else { 0 };
        // 0 dBFS = both axes on the rails (POWER 450 per sample).
        let lvl = db10_x10(mean_seg.max(1)) - db10_x10(450 * SEG as u64);
        let snr = db10_x10(mean_seg.max(1)) - db10_x10(floor_seg);
        let ok = (cfg.min_us..=cfg.max_us).contains(&dur_us);
        if !ok {
            self.n.rejected += 1;
            let t = now_us();
            if t >= self.ev_quiet_until {
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
        self.decode_into(&mut line, CAPTURE_LEN);
        println!("{}}}", line.as_str());
        true
    }

    /// Decode the first `n` capture samples, appending `,"dec":[...]`.
    fn decode_into(&mut self, line: &mut Line, n: usize) {
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
            rx.decode_all(&capture()[..n], &mut |res| {
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
            let _ = n;
            let _ = write!(line, r#","dec":null"#);
        }
    }

    pub fn stats(&self) {
        let (produced, overrun) = ring::produced();
        println!(
            r#"{{"t":"stats","up_us":{},"mode":"{}","mhz":{},"gain":{},"win":"{}","produced":{produced},"overrun":{overrun},"isrs":{},"dma_err":{},"probes":{},"rises":{},"floor":{},"accepted":{},"rejected":{},"lapped":{},"skipped":{},"bursts":{},"crc_ok":{},"decode_ms":{},"gain_lost":{},"held":{},"heap_free":{}}}"#,
            now_us(),
            self.s.mode.name(),
            self.s.mhz,
            self.s.gain,
            self.s.window.name(),
            ring::ISRS.load(core::sync::atomic::Ordering::Relaxed),
            ring::DMA_ERRORS.load(core::sync::atomic::Ordering::Relaxed),
            self.trig.probes,
            self.trig.rises,
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
            esp_alloc::HEAP.free()
        );
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
        self.decode_into(&mut line, self.c16_len);
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
