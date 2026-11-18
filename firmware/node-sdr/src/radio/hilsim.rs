//! Bench scenarios for "node-sdr under hostile signals" (docs/BENCH.md):
//! the decode budget against a pulse train and the O4 follower against
//! forged sessions, run on the chip without transmitting anything.
//! `bringup` builds only (a bench feature: the build needs
//! `ROTORSCOPE_BENCH=1`).
//!
//! - **`hil train <n> <gap_ms> [nobudget]`**: the staged `@F` grab (one
//!   burst, as for `hil run`) is replayed as a train of `n` identical pulses
//!   `gap_ms` apart on a simulated clock. Every pulse the budget lets
//!   through is really decoded on the chip (its measured decode time
//!   advances the simulated clock); pulses that arrive while the decoder is
//!   busy are counted as missed, pulses the budget refuses as skipped. Each
//!   simulated 700 ms dwell's pressure goes through the jamming watch's own
//!   rule (`jamwatch::ProbeStats::pressure`). `nobudget` shows the loop as it
//!   was before the budget.
//! - **`hil forge <secs>`**: a virtual attacker and a virtual drone inside
//!   the live radio. The node hops its real plan and follows on the real
//!   follower; a publication counts as heard only if the node is tuned to
//!   its channel when it falls due (checked every pass of the capture loop,
//!   so within ~1 ms). Streams: one "real" O4 drone; four forged sessions
//!   that follow the published schedule exactly (the strongest forger, who
//!   read `droneid::follow`); a new forged key ID every 700 ms on whatever
//!   O4 channel the node is on; and, once the real session is followed, a
//!   packet with the real key ID every 1.3 s on the node's channel at an
//!   arbitrary time (a contradiction). At the end (or on `hil forge` with no
//!   argument) one `forge` line says what the follower did.

use core::fmt::Write as _;

use droneid::follow::{CHANNELS, PERIOD_US, channel};
use esp_println::println;
use link::NodeRole;
use rawcap::budget::{self, Budget, Pressure};
use rawcap::trigger::Trigger;

use super::{GRAB_BYTES, Line, PRE_BYTES, Radio, WINDOW_BYTES, now_us};
use crate::{engine, ring};

/// The DroneID plan's dwell, for the simulated train's dwells.
const DWELL_US: u64 = 700_000;

/// What a `hil train` did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Train {
    /// The staged grab held a DroneID-length rise to replay.
    pub found: bool,
    pub pulses: u32,
    pub decoded: u32,
    pub full: u32,
    pub valid: u32,
    /// Refused by the budget, and arrived while the decoder was busy.
    pub skipped: u32,
    pub missed: u32,
    /// Simulated time the decoder held the loop, and the train's length.
    pub busy_ms: u32,
    pub span_ms: u32,
    /// Simulated dwells, and those the jamming watch would count as pulsed.
    pub dwells: u32,
    pub flagged: u32,
    pub ms_min: u32,
    pub ms_max: u32,
}

/// The jamming watch's rule for one dwell's decoder pressure. Not inlined:
/// its `ProbeStats` must not sit in the frame under the decoder.
#[inline(never)]
fn flagged(p: &Pressure) -> bool {
    let mut ps = jamwatch::ProbeStats::new(51_200);
    ps.decode_pressure(p.skipped, p.full, p.valid, p.busy_pct());
    ps.pressure().is_some()
}

/// Key IDs of the scenario (never real ones).
const REAL_ID: u32 = 0x5EA1_0001;
const FORGED_ID: u32 = 0xF0F0_0001;
const NEW_ID: u32 = 0xF1E5_0000;
const NEW_EVERY_US: u64 = 700_000;
const CONTRA_EVERY_US: u64 = 1_300_000;

#[derive(Clone, Copy, Debug)]
struct Stream {
    id: u32,
    c0: u8,
    t0: u64,
    /// Next publication.
    k: u64,
    heard: u32,
}

/// Counters at the start, to report the scenario's own share.
#[derive(Clone, Copy, Debug, Default)]
struct Base {
    visits: u32,
    hits: u32,
    misses: u32,
    granted: u32,
    declined: u32,
    used_us: u64,
    contradictions: u32,
    refused: u32,
}

/// The forged-session scenario's state (see the module docs).
#[derive(Clone, Copy, Debug)]
pub struct Forge {
    t0_us: u64,
    until_us: u64,
    /// 0: the real drone; 1..=4: forged, schedule-perfect.
    streams: [Stream; 5],
    next_new_us: u64,
    new_ids: u32,
    next_contra_us: u64,
    contra: u32,
    base: Base,
}

impl Radio {
    fn follow_base(&self) -> Base {
        let f = &self.follow;
        Base {
            visits: self.follow_stats.visits,
            hits: self.follow_stats.hits,
            misses: self.follow_stats.misses,
            granted: f.share.granted,
            declined: f.share.declined,
            used_us: f.share.used_us,
            contradictions: f.contradictions,
            refused: f.refused,
        }
    }

    /// `hil train` (see the module docs).
    #[inline(never)]
    pub fn hil_train(&mut self, data: &[u8], n: u32, gap_us: u64, budgeted: bool) -> Train {
        ring::stop();
        engine::stop();
        self.pending = None;
        // Let a DMA write already in flight land before we overwrite the ring.
        let t = now_us();
        while now_us() - t < 200 {}
        let len = data.len().min(GRAB_BYTES);
        // SAFETY: ring and engine stopped; single-threaded.
        unsafe { ring::bytes_mut()[..len].copy_from_slice(&data[..len]) };
        let mut out = Train { pulses: n, ms_min: u32::MAX, ..Train::default() };
        let cfg = self.s.trig;
        let mut trig = Trigger::new(cfg);
        let mut pos = 0usize;
        let mut rise = None;
        while pos + cfg.probe as usize <= len {
            if let Some(r) = trig.feed(pos as u64, rawcap::iq::power_sum(&data[pos..pos + cfg.probe as usize])) {
                rise = Some(r);
                break;
            }
            pos += cfg.block as usize;
        }
        let Some(r) = rise.filter(|r| r.pos.saturating_sub(PRE_BYTES) as usize + WINDOW_BYTES <= len) else {
            self.restart();
            return out;
        };
        out.found = true;
        let start = r.pos.saturating_sub(PRE_BYTES);
        let mut b = Budget::new(budget::Config::default());
        // Simulated clock from 1 s (the budget starts with a full credit).
        let t0 = 1_000_000u64;
        let mut busy_until = 0u64;
        let mut busy = 0u64;
        let mut dwell_end = t0 + DWELL_US;
        b.begin_dwell(t0);
        self.hil = true;
        for k in 0..n as u64 {
            node_common::fault::feed();
            let t = t0 + k * gap_us;
            while t >= dwell_end {
                out.dwells += 1;
                out.flagged += flagged(&b.dwell(dwell_end)) as u32;
                b.begin_dwell(dwell_end);
                dwell_end += DWELL_US;
            }
            if t < busy_until {
                out.missed += 1;
                continue;
            }
            if budgeted && !b.allow(t) {
                out.skipped += 1;
                continue;
            }
            let (_, dec) = self.window(start, r);
            let Some(d) = dec else { continue };
            let us = d.t1.saturating_sub(d.t0);
            b.done(t, t + us, d.full > 0, d.valid);
            busy_until = t + us;
            busy += us;
            out.decoded += 1;
            out.full += (d.full > 0) as u32;
            out.valid += d.valid;
            let ms = (us / 1000) as u32;
            out.ms_min = out.ms_min.min(ms);
            out.ms_max = out.ms_max.max(ms);
        }
        let end = (t0 + n as u64 * gap_us).max(busy_until);
        if end > dwell_end - DWELL_US {
            out.dwells += 1;
            out.flagged += flagged(&b.dwell(end)) as u32;
        }
        self.hil = false;
        out.busy_ms = (busy / 1000) as u32;
        out.span_ms = ((end - t0) / 1000) as u32;
        if out.ms_min == u32::MAX {
            out.ms_min = 0;
        }
        self.restart();
        out
    }

    /// `hil forge <secs>`: start the forged-session scenario.
    pub fn forge_start(&mut self, secs: u32) {
        let now = now_us();
        let mut streams = [Stream { id: REAL_ID, c0: 17, t0: now + 1_000_000, k: 0, heard: 0 }; 5];
        for (j, s) in streams.iter_mut().enumerate().skip(1) {
            *s = Stream {
                id: FORGED_ID + j as u32 - 1,
                c0: (60 * j as u32 + 3) as u8,
                t0: now + 1_000_000 + j as u64 * 160_000,
                k: 0,
                heard: 0,
            };
        }
        self.forge = Some(Forge {
            t0_us: now,
            until_us: now + secs as u64 * 1_000_000,
            streams,
            next_new_us: now + 2_000_000,
            new_ids: 0,
            next_contra_us: 0,
            contra: 0,
            base: self.follow_base(),
        });
        println!(
            r#"{{"t":"ok","forge":{secs},"role":"{}","hop":{},"follow":{},"note":"virtual streams only; nothing is transmitted"}}"#,
            crate::hub::role_name(self.role),
            self.hopping,
            self.follow_on
        );
    }

    /// Advance the scenario to now (called from `poll`). Not inlined: its
    /// frame must not sit under the decoder in `poll`.
    #[inline(never)]
    pub(super) fn forge_tick(&mut self) {
        let Some(mut f) = self.forge.take() else { return };
        let now = now_us();
        let listening = self.role == NodeRole::DroneId && self.phy_ok;
        for j in 0..f.streams.len() {
            loop {
                let s = f.streams[j];
                let at = s.t0 + s.k * PERIOD_US;
                if at > now {
                    break;
                }
                let ch = channel(s.c0.wrapping_add(s.k as u8));
                f.streams[j].k += 1;
                if listening && self.cur_freq == ch {
                    f.streams[j].heard += 1;
                    self.follow_observe_at(s.id, at);
                }
            }
        }
        let on_o4 = listening && CHANNELS.contains(&self.cur_freq);
        if now >= f.next_new_us {
            f.next_new_us = now + NEW_EVERY_US;
            if on_o4 {
                self.follow_observe_at(NEW_ID + f.new_ids, now);
                f.new_ids += 1;
            }
        }
        if now >= f.next_contra_us && self.follow.session(REAL_ID).is_some_and(|s| s.confirmed) {
            f.next_contra_us = now + CONTRA_EVERY_US;
            if on_o4 {
                self.follow_observe_at(REAL_ID, now);
                f.contra += 1;
            }
        }
        if now >= f.until_us {
            self.forge_report(&f, true);
            return;
        }
        self.forge = Some(f);
    }

    /// `hil forge` without a number: the running scenario's report.
    pub fn forge_status(&mut self) {
        match self.forge {
            Some(f) => self.forge_report(&f, false),
            None => println!(r#"{{"t":"forge","running":false}}"#),
        }
    }

    fn forge_report(&mut self, f: &Forge, done: bool) {
        let now = now_us();
        let b = self.follow_base();
        let elapsed = now.saturating_sub(f.t0_us).max(1);
        let used = b.used_us - f.base.used_us;
        let real = self.follow.session(REAL_ID);
        let forged = (1..=4u32).filter(|j| self.follow.session(FORGED_ID + j - 1).is_some_and(|s| s.confirmed)).count();
        let mut line = Line::new();
        let _ = write!(
            line,
            r#"{{"t":"forge","done":{done},"s":{},"real":[{},{},{}],"heard":[{},{},{},{},{}],"forged_followed":{forged},"active":{},"tentative":{},"visits":{},"hits":{},"misses":{},"granted":{},"declined":{},"share_permille":{},"contra":[{},{}],"refused":{},"new_ids":{}}}"#,
            elapsed / 1_000_000,
            real.is_some_and(|s| s.confirmed),
            real.map_or(0, |s| s.hits),
            real.map_or(0, |s| s.contradictions),
            f.streams[0].heard,
            f.streams[1].heard,
            f.streams[2].heard,
            f.streams[3].heard,
            f.streams[4].heard,
            self.follow.active(),
            self.follow.tentative(),
            b.visits - f.base.visits,
            b.hits - f.base.hits,
            b.misses - f.base.misses,
            b.granted - f.base.granted,
            b.declined - f.base.declined,
            used * 1000 / elapsed,
            f.contra,
            b.contradictions - f.base.contradictions,
            b.refused - f.base.refused,
            f.new_ids
        );
        // Over the ~300 bytes the ring tolerates in one print: in pieces.
        crate::out::paced(line.as_str(), 128, || self.probe());
    }
}
