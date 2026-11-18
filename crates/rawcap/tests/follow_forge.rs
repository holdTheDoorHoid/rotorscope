//! Host model of node-sdr's DroneID job under forged O4 sessions: the plan
//! walk, the O4 follower's visits (`firmware/node-sdr/src/radio.rs`
//! `follow_hop` / `next_hop` / `close_visit`) and the bench's forged-session
//! scenario (`radio/hilsim.rs`, docs/BENCH.md "node-sdr under hostile
//! signals" step 4), driven by the real `droneid::follow::Follower` and,
//! for the decoded runs, the real `rawcap::budget::Budget`.
//!
//! Event driven with exact times. Two modes:
//! - **virtual** (the bench scenario as the chip runs it): a publication
//!   counts as heard when the node is tuned to its channel when it falls
//!   due, retunes included, and costs nothing.
//! - **decoded** (a real transmitter): every packet the node hears must be
//!   let through by the decode budget and holds the loop for one decode
//!   (`decode_us`); the receiver hears nothing during a decode or a retune.
//!
//! `print_table` (ignored; `--ignored --nocapture`) prints the numbers
//! docs/DRONEID-O4.md quotes. The other tests hold the targets.

use droneid::follow::{CANDIDATES, CHANNELS, Follower, Observed, PERIOD_US, TENTATIVE_LOST_US, channel};
use rawcap::budget::{Budget, Config};

/// `radio::DRONEID_PLAN`, 700 ms each.
const PLAN: [u32; 13] = [57465, 57565, 57665, 57765, 57865, 57965, 58065, 58165, 24145, 24295, 24445, 24595, 23995];
const DWELL_US: u64 = 700_000;
/// `radio::FOLLOW_LEAD_US`, `radio::FOLLOW_MIN_DWELL_US`.
const LEAD_US: u64 = 30_000;
const MIN_DWELL_US: u64 = 120_000;
/// A DroneID retune with its DC calibration (~22 ms on 5 GHz).
const RETUNE_US: u64 = 22_000;
/// The fastest full decode measured on the C5 (BRINGUP-RESULTS.md "HIL",
/// 144-158 ms; 173 ms in the 2026-11-18 burst train).
const FASTEST_DECODE_US: u64 = 144_000;

/// `radio/hilsim.rs`'s key IDs.
const REAL_ID: u32 = 0x5EA1_0001;
const FORGED_ID: u32 = 0xF0F0_0001;
const NEW_ID: u32 = 0xF1E5_0000;

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.step() % n
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    pub secs: u64,
    /// Schedule-perfect forged sessions (0..=4) and when they start.
    pub forged: usize,
    pub forged_from_us: u64,
    /// A new forged key ID this often (on the node's channel when it is an
    /// O4 one), from `new_from_us`.
    pub new_every_us: Option<u64>,
    pub new_from_us: u64,
    /// The forgers (sessions, new IDs, wrong-time packets) start only once
    /// the real drone is followed.
    pub late: bool,
    /// Wrong-time packets with the real key ID every 1.3 s once followed.
    pub contra: bool,
    pub real: bool,
    pub follow_on: bool,
    /// `Some(us)`: decoded mode (see the module docs).
    pub decode_us: Option<u64>,
    /// The bench's fixed phases (hilsim::forge_start) instead of random:
    /// forged session 4 publishes at the real drone's moments.
    pub bench_phases: bool,
}

impl Scenario {
    /// docs/BENCH.md step 4 as the chip ran it on 2026-11-18.
    pub fn bench(secs: u64) -> Self {
        Scenario {
            secs,
            forged: 4,
            forged_from_us: 1_000_000,
            new_every_us: Some(700_000),
            new_from_us: 2_000_000,
            late: false,
            contra: true,
            real: true,
            follow_on: true,
            decode_us: None,
            bench_phases: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Outcome {
    /// When the real drone was followed (µs from the start).
    pub confirmed_at: Option<u64>,
    pub followed_at_end: bool,
    /// Its candidate held CONFIRM_HITS consistent packets with every
    /// followed slot taken.
    pub blocked_by_slots: bool,
    /// The real drone's own packets heard on plan dwells, during visits,
    /// and before it was followed.
    pub real_heard_plan: u32,
    pub real_heard_visit: u32,
    pub real_heard_unfollowed: u32,
    /// What the follower did with them, and real-drone candidates lost to
    /// a new key ID.
    pub real_acquired: u32,
    pub real_restarted: u32,
    pub real_replaced: u32,
    /// Two passive drones nobody follows (never fed to the follower): one
    /// at a random phase, one publishing at forged session 1's moments.
    /// Publications each heard on the plan.
    pub probe_heard: u32,
    pub probe_inphase_heard: u32,
    pub forged_followed: usize,
    pub plan_us: u64,
    pub follow_us: u64,
    pub new_ids: u32,
    pub evicted: u32,
    pub contra_sent: u32,
    pub contradicted: u32,
    pub skipped: u32,
    pub visits: u32,
    pub declined: u32,
}

#[derive(Clone, Copy)]
struct Stream {
    id: u32,
    c0: u8,
    t0: u64,
    k: u64,
}

impl Stream {
    fn at(&self) -> u64 {
        self.t0.saturating_add(self.k * PERIOD_US)
    }
    fn ch(&self) -> u32 {
        channel(self.c0.wrapping_add(self.k as u8))
    }
}

#[derive(Clone, Copy)]
struct Visit {
    a: droneid::follow::Appointment,
    settled: bool,
    heard: bool,
    decoded: bool,
}

const PROBE: u32 = 0xEEEE_0000;

pub fn run(sc: &Scenario, seed: u64) -> Outcome {
    let mut rng = Rng::new(seed);
    let mut o = Outcome::default();
    let mut f = Follower::new();
    let mut budget = Budget::new(Config::default());
    let t_start = 10_000_000u64;
    let end = t_start + sc.secs * 1_000_000;
    let never = u64::MAX / 2;
    // Streams: the real drone first (if any), then forged ones, then the
    // two passive probes.
    let mut streams: Vec<Stream> = Vec::new();
    let (real_c0, real_t0) = if sc.bench_phases { (17, t_start + 1_000_000) } else { (rng.below(256) as u8, t_start + 1_000_000 + rng.below(PERIOD_US)) };
    if sc.real {
        streams.push(Stream { id: REAL_ID, c0: real_c0, t0: real_t0, k: 0 });
    }
    let mut forged_phase = Vec::new();
    for j in 1..=sc.forged {
        let (c0, off) = if sc.bench_phases { ((60 * j as u32 + 3) as u8, j as u64 * 160_000) } else { (rng.below(256) as u8, rng.below(PERIOD_US)) };
        forged_phase.push(off);
        let t0 = if sc.late { never } else { t_start + sc.forged_from_us + off };
        streams.push(Stream { id: FORGED_ID + j as u32 - 1, c0, t0, k: 0 });
    }
    let probe_t0 = t_start + rng.below(PERIOD_US);
    streams.push(Stream { id: PROBE, c0: rng.below(256) as u8, t0: probe_t0, k: 0 });
    let inphase_t0 = t_start + sc.forged_from_us + forged_phase.first().copied().unwrap_or(0);
    streams.push(Stream { id: PROBE + 1, c0: rng.below(256) as u8, t0: inphase_t0, k: 0 });
    // The plan: random starting hop and phase.
    let mut hop_idx = rng.below(PLAN.len() as u64) as usize;
    let mut cur = PLAN[hop_idx];
    let mut hop_start = t_start;
    let mut hop_until = t_start + rng.below(DWELL_US) + 1;
    let mut on_plan = true;
    let mut visit: Option<Visit> = None;
    let mut busy_until = 0u64;
    let new_phase = if sc.bench_phases { 0 } else { rng.below(sc.new_every_us.unwrap_or(1)) };
    let mut next_new = sc.new_every_us.map(|_| if sc.late { never } else { t_start + sc.new_from_us + new_phase });
    let mut next_contra: Option<u64> = None;
    let mut new_n = 0u32;

    // Hear key ID `id`'s packet at `t` on `cur`; false if the node could not.
    let deliver = |f: &mut Follower, budget: &mut Budget, busy_until: &mut u64, visit: &mut Option<Visit>, o: &mut Outcome, id: u32, t: u64, cur: u32, on_plan: bool, own: bool| -> bool {
        if t < *busy_until {
            return false;
        }
        if let Some(d) = sc.decode_us {
            if !budget.allow(t) {
                o.skipped += 1;
                return false;
            }
            budget.done(t, t + d, true, 1);
            *busy_until = t + d;
            if let Some(v) = visit.as_mut() {
                v.decoded = true;
            }
        }
        let had = f.candidate(REAL_ID).is_some();
        let ev = f.evicted;
        let r = f.observe(id, t, cur);
        if had && f.evicted > ev && f.candidate(REAL_ID).is_none() && f.session(REAL_ID).is_none() {
            o.real_replaced += 1;
        }
        if let Some(v) = visit.as_mut()
            && v.a.id == id
            && matches!(r, Some(Observed::Matched { .. } | Observed::Confirmed { .. } | Observed::Duplicate))
        {
            v.heard = true;
        }
        if r == Some(Observed::Contradicted) && id == REAL_ID {
            o.contradicted += 1;
        }
        if own {
            if f.session(REAL_ID).is_none() || matches!(r, Some(Observed::Confirmed { .. })) {
                o.real_heard_unfollowed += 1;
            }
            match r {
                Some(Observed::Acquired) => o.real_acquired += 1,
                Some(Observed::Restarted) => o.real_restarted += 1,
                _ => {}
            }
            if on_plan {
                o.real_heard_plan += 1;
            } else {
                o.real_heard_visit += 1;
            }
        }
        true
    };

    let mut now = t_start;
    loop {
        // Earliest event: usize::MAX the end of the dwell, 100 a new key
        // ID, 101 a wrong-time packet, else a stream's publication.
        let mut t_ev = hop_until.max(busy_until);
        let mut which = usize::MAX;
        for (j, s) in streams.iter().enumerate() {
            if s.at() < t_ev {
                t_ev = s.at();
                which = j;
            }
        }
        if let Some(n) = next_new
            && n < t_ev
        {
            t_ev = n;
            which = 100;
        }
        if let Some(c) = next_contra
            && c < t_ev
        {
            t_ev = c;
            which = 101;
        }
        if t_ev >= end {
            break;
        }
        now = now.max(t_ev);
        match which {
            100 => {
                next_new = Some(t_ev + sc.new_every_us.unwrap());
                if CHANNELS.contains(&cur) && deliver(&mut f, &mut budget, &mut busy_until, &mut visit, &mut o, NEW_ID + new_n, t_ev, cur, on_plan, false) {
                    new_n += 1;
                }
            }
            101 => {
                next_contra = Some(t_ev + 1_300_000);
                if CHANNELS.contains(&cur) && deliver(&mut f, &mut budget, &mut busy_until, &mut visit, &mut o, REAL_ID, t_ev, cur, on_plan, false) {
                    o.contra_sent += 1;
                }
            }
            usize::MAX => {
                // The dwell is over: close a visit, then follow or plan.
                let t = t_ev;
                if on_plan {
                    o.plan_us += t - hop_start;
                } else {
                    o.follow_us += t - hop_start;
                }
                budget.begin_dwell(t);
                if let Some(v) = visit.take()
                    && !v.heard
                    && v.settled
                    && !v.decoded
                {
                    f.missed(&v.a);
                }
                let mut cap = None;
                let mut went = false;
                if sc.follow_on
                    && let Some(a) = f.next(t)
                {
                    let until_lead = a.start_us.saturating_sub(t + LEAD_US);
                    if until_lead >= MIN_DWELL_US {
                        cap = f.affordable(&a, t + until_lead).then_some(until_lead);
                    } else if f.grant(t, a.end_us.max(t + 5_000)) {
                        cur = a.freq;
                        let tuned = t + RETUNE_US;
                        hop_until = a.end_us.max(tuned + 5_000);
                        visit = Some(Visit { a, settled: tuned <= a.start_us, heard: false, decoded: false });
                        o.visits += 1;
                        on_plan = false;
                        went = true;
                    }
                }
                if !went {
                    hop_idx = (hop_idx + 1) % PLAN.len();
                    cur = PLAN[hop_idx];
                    on_plan = true;
                    // A dwell cut short for a window ends FOLLOW_LEAD_US
                    // before it opens, counted from before the retune
                    // (radio.rs `next_hop`; before 2026-11-18 it was
                    // counted from after, and the visit then started
                    // RETUNE_US - LEAD_US late: never settled).
                    hop_until = (t + RETUNE_US + DWELL_US).min(cap.map_or(u64::MAX, |c| t + c));
                }
                hop_start = t;
                if sc.decode_us.is_some() {
                    // Deaf while retuning.
                    busy_until = busy_until.max(t + RETUNE_US);
                }
            }
            j => {
                let s = streams[j];
                streams[j].k += 1;
                if cur != s.ch() {
                    continue;
                }
                if s.id == PROBE || s.id == PROBE + 1 {
                    // Passive: heard on the plan when the node is free.
                    if on_plan && s.at() >= busy_until {
                        if s.id == PROBE {
                            o.probe_heard += 1;
                        } else {
                            o.probe_inphase_heard += 1;
                        }
                    }
                    continue;
                }
                deliver(&mut f, &mut budget, &mut busy_until, &mut visit, &mut o, s.id, s.at(), cur, on_plan, s.id == REAL_ID);
            }
        }
        if sc.real && f.candidate(REAL_ID).is_some_and(|c| c.hits as u32 >= droneid::follow::CONFIRM_HITS) {
            o.blocked_by_slots = true;
        }
        if sc.real && o.confirmed_at.is_none() && f.session(REAL_ID).is_some() {
            o.confirmed_at = Some(now - t_start);
            if sc.contra {
                next_contra = Some(now);
            }
            if sc.late {
                for (j, s) in streams.iter_mut().filter(|s| s.id & 0xFFFF_FF00 == FORGED_ID & 0xFFFF_FF00).enumerate() {
                    s.t0 = now + 100_000 + forged_phase[j];
                }
                next_new = sc.new_every_us.map(|e| now + 100_000 + new_phase % e);
            }
        }
    }
    o.followed_at_end = f.session(REAL_ID).is_some();
    o.forged_followed = (0..4).filter(|j| f.session(FORGED_ID + j).is_some()).count();
    o.new_ids = new_n;
    o.evicted = f.evicted;
    o.declined = f.share.declined;
    o
}

/// Totals over seeds 1..=`seeds`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sweep {
    pub runs: u64,
    pub confirmed_60s: u64,
    pub followed_at_end: u64,
    pub blocked_by_slots: u64,
    pub evicted: u64,
    pub real_replaced: u64,
    /// Means: the real drone heard per minute while unfollowed; the probes
    /// heard per minute of wall time; the follower's share, per mille.
    pub unfollowed_per_min: f64,
    pub probe_per_min: f64,
    pub inphase_per_min: f64,
    pub share_permille: f64,
    pub forged_followed: f64,
    /// Seconds by which 90 % of runs had followed the drone (None: fewer).
    pub p90_s: Option<u64>,
}

pub fn sweep(sc: &Scenario, seeds: u64) -> Sweep {
    let mut w = Sweep { runs: seeds, ..Sweep::default() };
    let n = seeds as f64;
    let mins = sc.secs as f64 / 60.0;
    let mut times = Vec::new();
    for seed in 1..=seeds {
        let o = run(sc, seed);
        times.push(o.confirmed_at.unwrap_or(u64::MAX));
        w.confirmed_60s += o.confirmed_at.is_some_and(|t| t <= 60_000_000) as u64;
        w.followed_at_end += o.followed_at_end as u64;
        w.blocked_by_slots += (o.blocked_by_slots && o.confirmed_at.is_none()) as u64;
        w.evicted += o.evicted as u64;
        w.real_replaced += o.real_replaced as u64;
        let unf_min = o.confirmed_at.unwrap_or(sc.secs * 1_000_000) as f64 / 60e6;
        w.unfollowed_per_min += o.real_heard_unfollowed as f64 / unf_min.max(1.0 / 60.0) / n;
        w.probe_per_min += o.probe_heard as f64 / mins / n;
        w.inphase_per_min += o.probe_inphase_heard as f64 / mins / n;
        w.share_permille += (o.follow_us * 1000 / (o.follow_us + o.plan_us).max(1)) as f64 / n;
        w.forged_followed += o.forged_followed as f64 / n;
    }
    times.sort();
    let t90 = times[(seeds as usize * 9).div_ceil(10) - 1];
    w.p90_s = (t90 != u64::MAX).then_some(t90.div_ceil(1_000_000));
    w
}

fn report(name: &str, sc: &Scenario, seeds: u64) -> Sweep {
    let w = sweep(sc, seeds);
    println!(
        "{name}: followed within 60 s {}/{seeds} (90 % by {:?} s), at the end {}/{seeds}, never followed for want of a slot {}; evicted {} replaced {}; plan hears a passive drone {:.2}/min (one in phase with forged session 1: {:.2}/min); follower share {:.0} permille; forged sessions followed at the end {:.2}",
        w.confirmed_60s, w.p90_s, w.followed_at_end, w.blocked_by_slots, w.evicted, w.real_replaced, w.probe_per_min, w.inphase_per_min, w.share_permille, w.forged_followed
    );
    w
}

/// The new forged key IDs the decode budget can let through at most in
/// `span_us`: a burst decodes only when the budget allows it, and each
/// valid one holds the loop for `decode_us`.
fn max_new_ids(span_us: u64, decode_us: u64) -> u32 {
    let mut b = Budget::new(Config::default());
    let (t0, mut t, mut n) = (1_000_000u64, 1_000_000u64, 0);
    let mut dwell_end = t0;
    while t < t0 + span_us {
        while dwell_end <= t {
            b.begin_dwell(dwell_end);
            dwell_end += DWELL_US;
        }
        if b.allow(t) {
            b.done(t, t + decode_us, true, 1);
            n += 1;
            t += decode_us;
        } else {
            t += 1_000;
        }
    }
    n
}

#[test]
fn the_decode_budget_cannot_cycle_the_candidate_table() {
    // A flood can only displace candidates older than TENTATIVE_LOST_US
    // (which would have expired anyway) if the budget lets fewer new key
    // IDs through in that time than the table holds.
    let n = max_new_ids(TENTATIVE_LOST_US, FASTEST_DECODE_US);
    assert!((80..=95).contains(&n), "{n}");
    assert!((n as usize) < CANDIDATES, "{n} new key IDs in {TENTATIVE_LOST_US} us, {CANDIDATES} candidates");
    // Margin: decodes would have to get ~1.5x faster before it no longer holds.
    assert!((max_new_ids(TENTATIVE_LOST_US, FASTEST_DECODE_US * 2 / 3) as usize) < CANDIDATES);
}

/// The fastest steady rate of new key IDs the budget passes, as a gap.
fn budget_rate_gap_us(decode_us: u64) -> u64 {
    // Credit earns busy_permille of wall time; each decode spends decode_us.
    decode_us * 1000 / Config::default().busy_permille as u64
}

const SEEDS: u64 = 200;

#[test]
fn a_new_key_id_flood_does_not_keep_the_real_drone_unfollowed() {
    // 2026-11-18 bench: with a new forged key ID every 0.7 s the drone was
    // never followed. Now the flood displaces nothing and the drone is
    // followed as soon as the plan has heard it three times.
    let flood = Scenario { forged: 0, bench_phases: false, ..Scenario::bench(180) };
    let alone = Scenario { new_every_us: None, contra: false, ..flood };
    let base = sweep(&alone, SEEDS);
    assert!(base.confirmed_60s * 100 >= SEEDS * 90, "{base:?}");
    // Virtual (the bench scenario): at 0.7 s and at twice the most the
    // decode budget can pass, no different from no flood at all.
    for every in [700_000, budget_rate_gap_us(FASTEST_DECODE_US) / 2] {
        let w = sweep(&Scenario { new_every_us: Some(every), ..flood }, SEEDS);
        assert_eq!((w.evicted, w.real_replaced), (0, 0), "{every}: {w:?}");
        assert!(w.confirmed_60s * 100 >= SEEDS * 90, "{every}: {w:?}");
        assert!(w.confirmed_60s + SEEDS / 50 >= base.confirmed_60s, "{every}: {w:?} vs {base:?}");
    }
    // Decoded at the budget's maximum: still nothing displaced; each forged
    // packet costs a decode the plan cannot listen through, so the drone is
    // heard less often and followed later (90 % within 2 minutes).
    let d = Scenario { decode_us: Some(FASTEST_DECODE_US), new_every_us: Some(budget_rate_gap_us(FASTEST_DECODE_US)), ..flood };
    let w = sweep(&d, SEEDS);
    assert_eq!((w.evicted, w.real_replaced), (0, 0), "{w:?}");
    assert!(w.p90_s.is_some_and(|s| s <= 120), "{w:?}");
}

#[test]
fn forgers_arriving_later_do_not_take_a_followed_drone() {
    // Four schedule-perfect forged sessions, the flood and wrong-time
    // packets start once the drone is followed. Before the starvation rule
    // (Follower::next) 3.6 % of runs lost the drone to declined visits.
    let late = Scenario { late: true, bench_phases: false, ..Scenario::bench(240) };
    let mut lost = 0;
    for seed in 1..=SEEDS {
        let o = run(&late, seed);
        lost += (o.confirmed_at.is_some() && !o.followed_at_end) as u64;
    }
    assert!(lost * 100 <= SEEDS, "{lost} of {SEEDS} runs lost the followed drone");
}

#[test]
fn a_followed_drone_survives_the_counter_wrap() {
    // Every followed drone crosses a counter wrap each 164 s, where its
    // hypotheses disagree and only a settled miss prunes the wrong ones.
    // Before 2026-11-18 a plan dwell cut short for a window ended
    // FOLLOW_LEAD_US before it counted from *after* the retune, so the
    // visit arrived ~8 ms late, was never settled, its misses never
    // counted: 10.5 % of drones alone were lost within 3 minutes.
    let alone = Scenario { forged: 0, new_every_us: None, contra: false, bench_phases: false, ..Scenario::bench(240) };
    let mut lost = 0;
    for seed in 1..=SEEDS {
        let o = run(&alone, seed);
        lost += (o.confirmed_at.is_some() && !o.followed_at_end) as u64;
    }
    assert_eq!(lost, 0);
}

#[test]
fn every_followed_slot_taken_first_is_the_documented_limit() {
    // Four forged sessions from the start: the drone is followed only if
    // the plan confirms it before the fourth forger (docs/DRONEID-O4.md).
    // It is still heard by the plan, and nothing displaces its candidate.
    let w = sweep(&Scenario { bench_phases: false, ..Scenario::bench(180) }, SEEDS);
    assert_eq!((w.evicted, w.real_replaced), (0, 0), "{w:?}");
    assert!(w.blocked_by_slots > 0 && w.confirmed_60s * 100 >= SEEDS * 60, "{w:?}");
    assert!(w.probe_per_min > 3.5, "{w:?}");
}

#[test]
#[ignore = "prints the simulation table (cargo test --release -p rawcap --test follow_forge -- --ignored --nocapture)"]
fn print_table() {
    let seeds = 1000;
    println!("max new key IDs the budget passes in 30 s: {} at 144 ms, {} at 173 ms; steady gap {} ms", max_new_ids(30_000_000, 144_000), max_new_ids(30_000_000, 173_000), budget_rate_gap_us(144_000) / 1000);
    let b = Scenario::bench(180);
    report("bench step 4 as run (4 forged sessions from the start, bench phases)", &b, seeds);
    let r = Scenario { bench_phases: false, ..b };
    report("4 forged sessions from the start, random phases", &r, seeds);
    report("3 forged sessions + new IDs + wrong-time packets", &Scenario { forged: 3, ..r }, seeds);
    report("new IDs only", &Scenario { forged: 0, ..r }, seeds);
    report("forgers arrive once the drone is followed", &Scenario { late: true, secs: 240, ..r }, seeds);
    let alone = Scenario { forged: 0, new_every_us: None, contra: false, ..r };
    report("drone alone", &alone, seeds);
    report("drone alone, follower off", &Scenario { follow_on: false, ..alone }, seeds);
    report("4 forged sessions, follower off", &Scenario { follow_on: false, ..r }, seeds);
    let gap = budget_rate_gap_us(FASTEST_DECODE_US);
    report("decoded: 3 forged + new IDs every 0.7 s", &Scenario { forged: 3, decode_us: Some(165_000), ..r }, seeds);
    report("decoded: 3 forged + new IDs at the budget's max", &Scenario { forged: 3, decode_us: Some(FASTEST_DECODE_US), new_every_us: Some(gap), ..r }, seeds);
    report("decoded: new IDs every 20 ms (budget-starving flood)", &Scenario { forged: 0, decode_us: Some(165_000), new_every_us: Some(20_000), ..r }, seeds);
    report("decoded: new IDs only every 0.7 s", &Scenario { forged: 0, decode_us: Some(165_000), ..r }, seeds);
    report("decoded: new IDs only at the budget's max", &Scenario { forged: 0, decode_us: Some(FASTEST_DECODE_US), new_every_us: Some(gap), ..r }, seeds);
    report("decoded: forgers arrive once the drone is followed", &Scenario { late: true, secs: 240, decode_us: Some(165_000), ..r }, seeds);
    report("decoded: drone alone", &Scenario { decode_us: Some(165_000), ..alone }, seeds);
}

