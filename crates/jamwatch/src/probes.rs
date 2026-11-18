//! One dwell's trigger probes: level percentiles and loud on/off runs.
//!
//! The level histogram is `fpvscan::Meter` (half-dB steps over the whole
//! range a 256-byte 4-bit probe can take, 27-51 dB). On top of it this
//! counts the runs of consecutive probes at or above a "loud" level set at
//! the start of the dwell from the hop's learnt floor: a pulsed jammer, a
//! microwave oven (on for half of every mains cycle) and Wi-Fi traffic all
//! show as runs, of very different lengths.
//!
//! The node also hands over its DroneID decoder's pressure on the dwell
//! ([`ProbeStats::decode_pressure`], from node-sdr's `rawcap::budget`): a
//! train of short DroneID-length pulses costs the node its decode time
//! rather than probe levels, which are hardly touched while the loop
//! decodes (nothing is probed then).

use fpvscan::{Meter, db_x10};

#[derive(Clone, Debug)]
pub struct ProbeStats {
    meter: Meter,
    /// Loud threshold, in probe power (the smallest power at or above the
    /// threshold in dB).
    loud_pow: u32,
    loud_x10: i32,
    loud: u32,
    runs: u32,
    on: bool,
    max: u32,
    /// Spacing of the probes, ns (the trigger's block: 51,200).
    block_ns: u32,
    /// The decoder on this dwell: windows the budget refused, full decodes,
    /// CRC-valid bursts, percent of the dwell spent decoding.
    dec_skipped: u32,
    dec_full: u32,
    dec_valid: u32,
    dec_busy_pct: u8,
}

/// The smallest probe power whose `db_x10` is at least `x10`.
fn pow_at(x10: i32) -> u32 {
    let (mut lo, mut hi) = (1u32, u32::MAX / 2);
    if db_x10(hi as u64) < x10 {
        return hi;
    }
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if db_x10(mid as u64) >= x10 { hi = mid } else { lo = mid + 1 }
    }
    lo
}

impl ProbeStats {
    pub const fn new(block_ns: u32) -> Self {
        ProbeStats {
            meter: Meter::new(),
            loud_pow: u32::MAX,
            loud_x10: i32::MAX,
            loud: 0,
            runs: 0,
            on: false,
            max: 0,
            block_ns,
            dec_skipped: 0,
            dec_full: 0,
            dec_valid: 0,
            dec_busy_pct: 0,
        }
    }

    /// Start a dwell; probes at or above `loud_x10` (tenths of a dB, probe
    /// units) count as loud.
    pub fn begin(&mut self, loud_x10: i32) {
        let block_ns = self.block_ns;
        *self = ProbeStats::new(block_ns);
        self.loud_x10 = loud_x10;
        self.loud_pow = pow_at(loud_x10);
    }

    /// One probe's power sum.
    #[inline]
    pub fn feed(&mut self, power: u32) {
        self.meter.feed(power);
        let l = power >= self.loud_pow;
        if l {
            self.loud += 1;
            if !self.on {
                self.runs += 1;
            }
        }
        self.on = l;
        self.max = self.max.max(power);
    }

    /// A gap in the probe sequence (the ring was stopped): the current run
    /// ends here.
    pub fn gap(&mut self) {
        self.on = false;
    }

    pub fn count(&self) -> u32 {
        self.meter.count()
    }

    pub fn percentile_x10(&self, pct: u32) -> i32 {
        self.meter.percentile_x10(pct)
    }

    pub fn max_x10(&self) -> i32 {
        db_x10(self.max.max(1) as u64)
    }

    pub fn loud_x10(&self) -> i32 {
        self.loud_x10
    }

    /// Percent of probes at or above the loud level.
    pub fn loud_pct(&self) -> u8 {
        let n = self.count();
        if n == 0 { 0 } else { (self.loud as u64 * 100 / n as u64) as u8 }
    }

    /// Loud runs (each a stretch of consecutive loud probes).
    pub fn runs(&self) -> u32 {
        self.runs
    }

    /// Mean length of a loud run, µs.
    pub fn mean_on_us(&self) -> u32 {
        if self.runs == 0 { 0 } else { (self.loud as u64 * self.block_ns as u64 / 1000 / self.runs as u64) as u32 }
    }

    /// Mean gap between loud runs, µs.
    pub fn mean_off_us(&self) -> u32 {
        let off = self.count().saturating_sub(self.loud);
        if self.runs == 0 { 0 } else { (off as u64 * self.block_ns as u64 / 1000 / self.runs as u64) as u32 }
    }

    /// The DroneID decoder's load on this dwell (node-sdr: the decode
    /// budget's per-dwell counts, fed once at the end of the dwell).
    pub fn decode_pressure(&mut self, skipped: u32, full: u32, valid: u32, busy_pct: u8) {
        self.dec_skipped = skipped;
        self.dec_full = full;
        self.dec_valid = valid;
        self.dec_busy_pct = busy_pct;
    }

    /// Decoder pressure that counts as interference: the budget had to
    /// refuse windows, and most of what the decoder worked on was not a
    /// valid burst (a drone's bursts decode; several drones on one channel
    /// fill the budget with valid ones). The decoder's busy percent.
    pub fn pressure(&self) -> Option<u8> {
        (self.dec_skipped > 0 && 2 * self.dec_valid < self.dec_full.max(1)).then_some(self.dec_busy_pct)
    }

    /// Length of the dwell's probe record, µs.
    pub fn span_us(&self) -> u32 {
        (self.count() as u64 * self.block_ns as u64 / 1000) as u32
    }
}
