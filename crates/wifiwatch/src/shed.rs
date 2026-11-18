//! What a Wi-Fi node sends the hub when it cannot send everything
//! (docs/BENCH.md "Frame flood").
//!
//! A node's hub queue is short (eight frames on node-wifi) and the link
//! behind it can be slow (ESP-NOW only drains while the radio sits on the
//! gateway's channel). Anyone in radio range can forge cheap frames that each
//! become a hub frame: a beacon with an attack tool's network name is a
//! `Threat`, a drone maker's address prefix a `WifiDrone`, a DJI vendor
//! element a `DjiWifi`, a wfb-ng frame with a new link id an `OpenLink`, a
//! Remote ID beacon from a new random address a `Detection`. Per-address
//! rate limits do not bound that: the attacker uses a fresh address each
//! time. Without more, the queue fills with forgeries and a real drone's
//! Remote ID is lost.
//!
//! [`Shed`] decides, before a frame is formatted or queued:
//!
//! - **Room** ([`Kind::room`]): with `cap` slots, a Remote ID frame from a
//!   known source may take any slot; node health (`Hello`, `Status`,
//!   `Stats`) all but 2; Remote ID from a new source all but 3; everything
//!   else (alarms) all but 4. So 2 slots are only ever used by known Remote
//!   ID, and no flood of alarms or of new-source Remote ID can fill them.
//! - **Budgets** ([`Kind::budget`]): a token bucket per alarm kind (and
//!   one for new-source Remote ID), so a flood of one kind is capped at its
//!   rate and cannot take another kind's share of the alarm slots.
//! - **Known sources** ([`RidSources`]): a Remote ID source is *known* once
//!   it has been heard twice within [`PROBATION_MS`]. A flood from random
//!   addresses is heard once per address and never becomes known. The table
//!   is indexed by a salted hash of the address (salt from the node's random
//!   number generator, so an attacker cannot aim at a slot), and a slot held
//!   by a known source is not given up while that source keeps sending: a
//!   flood cannot evict a drone the node already follows. A known source is
//!   limited to one frame per [`KNOWN_GAP_MS`] (12.5 a second; Wi-Fi Remote
//!   ID beacons come at 1 to 10 a second), so a forger spoofing a known
//!   address cannot use the reserved slots any faster than that.
//!
//! Every frame turned away is counted per kind ([`Shed::dropped`]), so the
//! node can say what it shed.
//!
//! Limits, by design: a drone that first appears *during* a random-address
//! Remote ID flood goes through the new-source budget until its slot frees
//! (one-off addresses hold a slot for [`PROBATION_MS`]); the higher the flood
//! rate, the longer that takes (see the tests). An attacker who repeats each
//! forged address can make forged sources known; they then share the known
//! class with real drones but cannot evict them.

use link::Frame;

use crate::util::{Ms, elapsed, hash};

/// What a hub frame is, for room and budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Remote ID from a source heard before ([`RidSources`]).
    RidKnown,
    /// Remote ID from a new (or long silent) source.
    RidNew,
    /// `Hello`, `Status`, `Stats`: the node's own health.
    Health,
    Threat,
    WifiDrone,
    DjiWifi,
    OpenLink,
    /// Anything else (MAVLink evidence after its threat, bench frames).
    Other,
}

impl Kind {
    pub const COUNT: usize = 8;
    pub const ALL: [Kind; Kind::COUNT] = [
        Kind::RidKnown,
        Kind::RidNew,
        Kind::Health,
        Kind::Threat,
        Kind::WifiDrone,
        Kind::DjiWifi,
        Kind::OpenLink,
        Kind::Other,
    ];

    /// Short name, for the node's JSON stats line.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::RidKnown => "rid_known",
            Kind::RidNew => "rid_new",
            Kind::Health => "health",
            Kind::Threat => "threat",
            Kind::WifiDrone => "wdrone",
            Kind::DjiWifi => "dji",
            Kind::OpenLink => "open",
            Kind::Other => "other",
        }
    }

    /// The kind of a frame. A `Detection` is [`Kind::RidNew`] here; the
    /// node asks [`Shed::rid`] for the real answer before queuing one.
    pub fn of(f: &Frame) -> Kind {
        match f {
            Frame::Detection(_) => Kind::RidNew,
            Frame::Hello { .. } | Frame::Status { .. } | Frame::Stats { .. } => Kind::Health,
            Frame::Threat(_) => Kind::Threat,
            Frame::WifiDrone(_) => Kind::WifiDrone,
            Frame::DjiWifi { .. } => Kind::DjiWifi,
            Frame::OpenLink(_) => Kind::OpenLink,
            _ => Kind::Other,
        }
    }

    /// Slots kept free of everything below this kind, in a queue of `cap`.
    const fn reserve(self) -> usize {
        match self {
            Kind::RidKnown => 0,
            Kind::Health => 2,
            Kind::RidNew => 3,
            _ => 4,
        }
    }

    /// How full a queue of `cap` slots may be for a frame of this kind to
    /// still go in.
    pub const fn room(self, cap: usize) -> usize {
        cap.saturating_sub(self.reserve())
    }

    /// May a frame of this kind join a queue holding `queued` of `cap`?
    pub const fn fits(self, queued: usize, cap: usize) -> bool {
        queued < self.room(cap)
    }

    /// `(per second, burst)` for kinds with a budget. Known Remote ID is
    /// limited per source instead ([`KNOWN_GAP_MS`]); health is the node's
    /// own, a few frames a second; `Other` follows a budgeted frame or is a
    /// bench frame.
    ///
    /// Alarms: each detector already reports a given transmitter or target
    /// at most about once a second, so 8 a second is several attacks or
    /// drones at once; open links are 16 streams at most once a second each
    /// (`rx::OPEN_STREAMS` on node-wifi). Together at most 56 frames a
    /// second, a few kilobytes: well inside the 921 600 baud wire, and the
    /// room rule keeps the slow ESP-NOW link's slots for Remote ID anyway.
    pub const fn budget(self) -> Option<(u16, u16)> {
        match self {
            Kind::RidNew => Some((8, 8)),
            Kind::Threat => Some((8, 8)),
            Kind::WifiDrone => Some((8, 8)),
            Kind::DjiWifi => Some((8, 8)),
            Kind::OpenLink => Some((16, 16)),
            Kind::RidKnown | Kind::Health | Kind::Other => None,
        }
    }
}

/// A token bucket on the millisecond clock: `rate` tokens a second, at most
/// `burst` saved up. Starts full.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    /// Thousandths of a token.
    milli: u32,
    last: Ms,
    rate: u16,
    burst: u16,
}

impl Bucket {
    pub const fn new(rate: u16, burst: u16) -> Self {
        Self { milli: burst as u32 * 1000, last: 0, rate, burst }
    }

    /// Take a token at `now` if there is one.
    pub fn take(&mut self, now: Ms) -> bool {
        let cap = u32::from(self.burst) * 1000;
        // ms * tokens/s = thousandths of a token.
        let add = elapsed(now, self.last).saturating_mul(u32::from(self.rate));
        self.milli = self.milli.saturating_add(add).min(cap);
        self.last = now;
        if self.milli >= 1000 {
            self.milli -= 1000;
            true
        } else {
            false
        }
    }
}

/// A new Remote ID source becomes known if heard again within this; a
/// one-off address holds its slot this long.
pub const PROBATION_MS: Ms = 2_500;
/// A known source silent this long is forgotten (its slot is free again).
pub const FORGET_MS: Ms = 30_000;
/// Shortest gap between two frames of one known source (12.5 a second).
pub const KNOWN_GAP_MS: Ms = 80;
/// Slots in the node's source table (16 bytes each).
pub const RID_SOURCES: usize = 32;

#[derive(Clone, Copy, Debug)]
struct Source {
    mac: [u8; 6],
    /// 0: empty slot; 1: heard once (probation); 2: known.
    hits: u8,
    heard: Ms,
    sent: Ms,
}

const EMPTY: Source = Source { mac: [0; 6], hits: 0, heard: 0, sent: 0 };

/// Remote ID sources by address: known (heard twice within
/// [`PROBATION_MS`]) or new. See the module doc.
#[derive(Clone, Debug)]
pub struct RidSources<const N: usize> {
    slots: [Source; N],
    salt: u32,
}

/// What [`RidSources::classify`] says about one Remote ID frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Known,
    New,
    /// A known source sending faster than one frame per [`KNOWN_GAP_MS`].
    TooFast,
}

impl<const N: usize> RidSources<N> {
    /// `salt`: a random number from the node, so the slot an address lands
    /// in cannot be predicted.
    pub const fn new(salt: u32) -> Self {
        Self { slots: [EMPTY; N], salt }
    }

    fn slot(&self, mac: &[u8; 6]) -> usize {
        let s = self.salt.to_le_bytes();
        let key = [s[0], s[1], s[2], s[3], mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]];
        hash(&key) as usize % N.max(1)
    }

    /// One Remote ID frame from `mac` at `now`.
    pub fn classify(&mut self, mac: [u8; 6], now: Ms) -> Class {
        if N == 0 {
            return Class::New;
        }
        let i = self.slot(&mac);
        let s = &mut self.slots[i];
        if s.hits > 0 && s.mac == mac {
            let gap = elapsed(now, s.heard);
            s.heard = now;
            if s.hits >= 2 {
                if gap > FORGET_MS {
                    // Back after a long silence: on probation again.
                    s.hits = 1;
                    return Class::New;
                }
                if elapsed(now, s.sent) < KNOWN_GAP_MS {
                    return Class::TooFast;
                }
                s.sent = now;
                return Class::Known;
            }
            if gap <= PROBATION_MS {
                s.hits = 2;
                s.sent = now;
                return Class::Known;
            }
            return Class::New;
        }
        let free = match s.hits {
            0 => true,
            1 => elapsed(now, s.heard) > PROBATION_MS,
            _ => elapsed(now, s.heard) > FORGET_MS,
        };
        if free {
            *s = Source { mac, hits: 1, heard: now, sent: now };
        }
        Class::New
    }

    /// Sources known now (heard twice, not yet forgotten).
    pub fn known(&self, now: Ms) -> usize {
        self.slots.iter().filter(|s| s.hits >= 2 && elapsed(now, s.heard) <= FORGET_MS).count()
    }
}

/// The node's admission state: budgets, Remote ID sources, drop counts.
/// About 650 bytes.
#[derive(Clone, Debug)]
pub struct Shed {
    pub rid: RidSources<RID_SOURCES>,
    buckets: [Bucket; Kind::COUNT],
    /// Frames turned away, per [`Kind`] (index `kind as usize`): over
    /// budget, over a known source's rate, or no room in the queue. Totals.
    pub dropped: [u32; Kind::COUNT],
}

impl Shed {
    pub const fn new(salt: u32) -> Self {
        let mut buckets = [Bucket::new(0, 0); Kind::COUNT];
        let mut i = 0;
        while i < Kind::COUNT {
            if let Some((rate, burst)) = Kind::ALL[i].budget() {
                buckets[i] = Bucket::new(rate, burst);
            }
            i += 1;
        }
        Self { rid: RidSources::new(salt), buckets, dropped: [0; Kind::COUNT] }
    }

    fn count(&mut self, kind: Kind) {
        let d = &mut self.dropped[kind as usize];
        *d = d.wrapping_add(1);
    }

    /// Budget check for one frame of `kind` (always true for kinds without
    /// a budget). A refusal is counted.
    pub fn admit(&mut self, kind: Kind, now: Ms) -> bool {
        if kind.budget().is_none() || self.buckets[kind as usize].take(now) {
            return true;
        }
        self.count(kind);
        false
    }

    /// A Remote ID frame from `mac`: its kind if it may go on (a known
    /// source within its rate, or a new source within the new-source
    /// budget), else `None` (counted).
    pub fn rid(&mut self, mac: [u8; 6], now: Ms) -> Option<Kind> {
        match self.rid.classify(mac, now) {
            Class::Known => Some(Kind::RidKnown),
            Class::TooFast => {
                self.count(Kind::RidKnown);
                None
            }
            Class::New => self.admit(Kind::RidNew, now).then_some(Kind::RidNew),
        }
    }

    /// Room check at the queue (`queued` of `cap` in use). A refusal is
    /// counted.
    pub fn room(&mut self, kind: Kind, queued: usize, cap: usize) -> bool {
        if kind.fits(queued, cap) {
            return true;
        }
        self.count(kind);
        false
    }

    /// Everything turned away, all kinds.
    pub fn total(&self) -> u32 {
        self.dropped.iter().fold(0u32, |a, &d| a.wrapping_add(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 8;
    const REAL: [u8; 6] = [0x60, 0x60, 0x1F, 0x11, 0x22, 0x33];

    /// A hub queue drained by a consumer, frames tagged with their kind and
    /// whether they are the real drone's.
    struct Sim {
        queue: std::collections::VecDeque<(Kind, bool)>,
        delivered_real: u32,
        delivered: [u32; Kind::COUNT],
    }

    impl Sim {
        fn new() -> Self {
            Self { queue: Default::default(), delivered_real: 0, delivered: [0; Kind::COUNT] }
        }
        fn drain(&mut self, n: usize) {
            for _ in 0..n {
                let Some((k, real)) = self.queue.pop_front() else { break };
                self.delivered[k as usize] += 1;
                if real {
                    self.delivered_real += 1;
                }
            }
        }
    }

    /// What a flood looks like: every frame of the flood is one hub frame of
    /// `kind` (a fresh random address each, so no per-address limit helps).
    #[derive(Clone, Copy)]
    enum Flood {
        Alarm(Kind),
        RidRandom,
    }

    fn rand_mac(x: &mut u32) -> [u8; 6] {
        *x ^= *x << 13;
        *x ^= *x >> 17;
        *x ^= *x << 5;
        let b = x.to_le_bytes();
        [0x02, b[0], b[1], b[2], b[3], 0x5A]
    }

    /// Run `secs` seconds in 1 ms steps: the real drone sends one Remote ID
    /// frame a second (heard twice before the flood so it is known, as on
    /// the bench), Stats goes once a second, the flood sends `rate` frames
    /// a second. The consumer takes `drain_per_s` frames a second, but only
    /// during the first `on_ms` of every second (ESP-NOW on a 5 GHz plan:
    /// 40 ms a second on channel 6). `legacy`: the old node (one queue,
    /// newest dropped when full, no budgets, no classes).
    fn run(flood: Flood, rate: u32, drain_per_s: u32, on_ms: u32, secs: u32, legacy: bool) -> (u32, u32, Shed, Sim) {
        let mut shed = Shed::new(0xC0FF_EE11);
        let mut sim = Sim::new();
        let mut rng = 0x1234_5678u32;
        let mut real_sent = 0;
        let mut flood_acc = 0u32;
        let mut drain_acc = 0u32;
        let mut push = |shed: &mut Shed, sim: &mut Sim, kind: Option<Kind>, real: bool| {
            let Some(kind) = kind else { return };
            if legacy {
                if sim.queue.len() < CAP {
                    sim.queue.push_back((kind, real));
                }
            } else if shed.room(kind, sim.queue.len(), CAP) {
                sim.queue.push_back((kind, real));
            }
        };
        // Learn the real drone (two beacons a second apart), queue drained.
        for t in [0u32, 1000] {
            let k = if legacy { Some(Kind::RidNew) } else { shed.rid(REAL, t) };
            push(&mut shed, &mut sim, k, false);
            sim.drain(CAP);
        }
        let start = 2000u32;
        for ms in 0..secs * 1000 {
            let now = start + ms;
            let in_sec = ms % 1000;
            if in_sec == 500 {
                let k = if legacy { Some(Kind::RidNew) } else { shed.rid(REAL, now) };
                real_sent += 1;
                push(&mut shed, &mut sim, k, true);
            }
            if in_sec == 0 {
                push(&mut shed, &mut sim, Some(Kind::Health), false);
            }
            flood_acc += rate;
            while flood_acc >= 1000 {
                flood_acc -= 1000;
                let k = match flood {
                    Flood::Alarm(k) => {
                        if legacy || shed.admit(k, now) { Some(k) } else { None }
                    }
                    Flood::RidRandom => {
                        let mac = rand_mac(&mut rng);
                        if legacy { Some(Kind::RidNew) } else { shed.rid(mac, now) }
                    }
                };
                push(&mut shed, &mut sim, k, false);
            }
            if in_sec < on_ms {
                drain_acc += drain_per_s;
                while drain_acc >= 1000 {
                    drain_acc -= 1000;
                    sim.drain(1);
                }
            }
        }
        sim.drain(usize::MAX);
        (real_sent, sim.delivered_real, shed, sim)
    }

    const FLOODS: [Flood; 5] = [
        Flood::Alarm(Kind::Threat),
        Flood::Alarm(Kind::WifiDrone),
        Flood::Alarm(Kind::DjiWifi),
        Flood::Alarm(Kind::OpenLink),
        Flood::RidRandom,
    ];

    #[test]
    fn no_flood_reduces_delivered_remote_id() {
        // Wired (300 frames/s, always draining) and ESP-NOW on a 5 GHz plan
        // (40 ms a second on channel 6, ~100 frames/s while there).
        for flood in FLOODS {
            for &(drain, on) in &[(300u32, 1000u32), (2_500, 40)] {
                for rate in [50u32, 500, 2000] {
                    let (sent, got, shed, _) = run(flood, rate, drain, on, 20, false);
                    assert_eq!(got, sent, "rate {rate} drain {drain}/{on}: real Remote ID lost");
                    assert_eq!(shed.dropped[Kind::RidKnown as usize], 0);
                }
            }
        }
    }

    #[test]
    fn the_old_queue_lost_remote_id_under_the_same_floods() {
        // The scenario above discriminates: with one shared drop-newest queue
        // and no budgets, every flood costs real Remote ID.
        for flood in FLOODS {
            let (sent, got, _, _) = run(flood, 500, 2_500, 40, 20, true);
            assert!(got < sent / 2, "legacy delivered {got} of {sent}");
        }
    }

    #[test]
    fn health_survives_alarm_floods() {
        for flood in FLOODS {
            let (_, _, shed, sim) = run(flood, 2000, 2_500, 40, 20, false);
            assert_eq!(shed.dropped[Kind::Health as usize], 0);
            assert!(sim.delivered[Kind::Health as usize] >= 20);
        }
    }

    #[test]
    fn drops_are_counted_per_kind() {
        let (_, _, shed, _) = run(Flood::Alarm(Kind::Threat), 2000, 300, 1000, 10, false);
        let threat = shed.dropped[Kind::Threat as usize];
        // 2000/s for 10 s, about 8/s (+8 burst) admitted.
        assert!(threat >= 20_000 - 100 && threat <= 20_000 - 80, "{threat}");
        for k in [Kind::RidKnown, Kind::Health, Kind::WifiDrone, Kind::DjiWifi, Kind::OpenLink] {
            assert_eq!(shed.dropped[k as usize], 0, "{}", k.name());
        }
        assert_eq!(shed.total(), threat + shed.dropped[Kind::RidNew as usize]);
    }

    #[test]
    fn a_kind_is_capped_at_its_rate_and_refills() {
        for k in Kind::ALL {
            let Some((rate, burst)) = k.budget() else { continue };
            let mut shed = Shed::new(1);
            // 1000 a second for 10 s.
            let ok = (0..10_000u32).filter(|&ms| shed.admit(k, 5_000 + ms)).count() as u32;
            let max = u32::from(burst) + 10 * u32::from(rate);
            assert!(ok <= max && ok + 2 >= max, "{}: {ok} vs {max}", k.name());
            // Quiet for a second: a full burst again.
            let ok = (0..u32::from(burst) * 2).filter(|_| shed.admit(k, 16_000)).count() as u32;
            assert_eq!(ok, u32::from(burst), "{}", k.name());
        }
        // Kinds without a budget are never refused by it.
        let mut shed = Shed::new(1);
        assert!((0..10_000).all(|_| shed.admit(Kind::Health, 0) && shed.admit(Kind::RidKnown, 0)));
    }

    #[test]
    fn one_kinds_flood_leaves_the_others_their_budget() {
        let mut shed = Shed::new(1);
        for ms in 0..1000u32 {
            for _ in 0..5 {
                shed.admit(Kind::Threat, 10_000 + ms);
            }
        }
        assert!((0..8).all(|_| shed.admit(Kind::WifiDrone, 11_000)));
        assert!((0..16).all(|_| shed.admit(Kind::OpenLink, 11_000)));
    }

    #[test]
    fn room_rule() {
        assert_eq!(Kind::RidKnown.room(CAP), 8);
        assert_eq!(Kind::Health.room(CAP), 6);
        assert_eq!(Kind::RidNew.room(CAP), 5);
        for k in [Kind::Threat, Kind::WifiDrone, Kind::DjiWifi, Kind::OpenLink, Kind::Other] {
            assert_eq!(k.room(CAP), 4);
            assert!(k.fits(3, CAP) && !k.fits(4, CAP));
        }
        assert!(Kind::RidKnown.fits(7, CAP) && !Kind::RidKnown.fits(8, CAP));
    }

    #[test]
    fn a_source_becomes_known_on_its_second_frame() {
        let mut r = RidSources::<32>::new(7);
        assert_eq!(r.classify(REAL, 0), Class::New);
        assert_eq!(r.classify(REAL, 1000), Class::Known);
        assert_eq!(r.classify(REAL, 1050), Class::TooFast);
        assert_eq!(r.classify(REAL, 1100), Class::Known);
        // Silent past FORGET_MS: probation again.
        assert_eq!(r.classify(REAL, 1100 + FORGET_MS + 1), Class::New);
        // Heard once, then again too late: still new.
        let mut r = RidSources::<32>::new(7);
        assert_eq!(r.classify(REAL, 0), Class::New);
        assert_eq!(r.classify(REAL, PROBATION_MS + 1), Class::New);
        assert_eq!(r.classify(REAL, PROBATION_MS + 500), Class::Known);
    }

    #[test]
    fn random_addresses_never_become_known_or_evict_a_known_source() {
        let mut r = RidSources::<RID_SOURCES>::new(0xABCD);
        r.classify(REAL, 0);
        assert_eq!(r.classify(REAL, 1000), Class::Known);
        let mut x = 99u32;
        let mut real_known = 0;
        // 2000 random-address frames a second for 30 s, real drone at 1 Hz.
        for ms in 1001..=31_000u32 {
            for _ in 0..2 {
                assert_eq!(r.classify(rand_mac(&mut x), ms), Class::New);
            }
            if ms % 1000 == 0 && r.classify(REAL, ms) == Class::Known {
                real_known += 1;
            }
        }
        assert_eq!(real_known, 30);
        assert_eq!(r.known(31_000), 1);
    }

    #[test]
    fn a_drone_that_starts_during_a_flood_becomes_known_eventually() {
        // 200 random-address frames a second; the drone beacons at 1 Hz.
        // Its slot is free one time in ~(1 + 200/32) when it asks, so it is
        // known within some tens of seconds (documented in BENCH.md).
        for salt in [1u32, 2, 3, 4, 5] {
            let mut r = RidSources::<RID_SOURCES>::new(salt);
            let mut x = salt.wrapping_mul(7919) | 1;
            let mut known_at = None;
            for ms in 0..120_000u32 {
                if ms % 5 == 0 {
                    r.classify(rand_mac(&mut x), ms);
                }
                if ms % 1000 == 999 && r.classify(REAL, ms) == Class::Known && known_at.is_none() {
                    known_at = Some(ms);
                }
            }
            assert!(known_at.is_some_and(|t| t < 90_000), "salt {salt}: {known_at:?}");
        }
    }

    #[test]
    fn kind_of_frames() {
        assert_eq!(Kind::of(&Frame::Status { role: link::NodeRole::Wifi24, can: 0, plan: 0 }), Kind::Health);
        assert_eq!(
            Kind::of(&Frame::Stats { uptime_us: 0, frames_seen: 0, rid_found: 0, dropped: 0, by_transport: heapless::Vec::new() }),
            Kind::Health
        );
        assert_eq!(Kind::of(&Frame::Via { node: [0; 6], rssi: 0 }), Kind::Other);
    }
}
