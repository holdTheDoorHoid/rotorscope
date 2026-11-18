//! Small fixed-size building blocks: hashing, bitmaps, "seen recently"
//! sets, per-channel warm-up and a per-(kind, key) rate limit. All of them
//! are a few hundred bytes at most and O(1) or O(small table) per call.

/// Milliseconds on a wrapping 32-bit clock (49.7 days). Every comparison
/// uses [`elapsed`], so the wrap is harmless for gaps under 24 days.
pub type Ms = u32;

pub const fn ms(now_us: u64) -> Ms {
    (now_us / 1000) as Ms
}

/// `now - then` on the wrapping clock.
pub const fn elapsed(now: Ms, then: Ms) -> Ms {
    now.wrapping_sub(then)
}

/// FNV-1a, 32 bits, finished with a multiply-xorshift so the low bits (used
/// to index bitmaps) depend on every input byte.
pub fn hash(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h
}

/// A bitmap of `64 * W` bits indexed by a hash: a one-hash Bloom filter
/// when asked "contained?", and a distinct-value counter when asked
/// "how many bits are set?" (`count` is a lower bound on the number of
/// distinct values inserted; with `n` values in `m` bits it averages
/// `m * (1 - e^(-n/m))`, e.g. 40 values in 512 bits read as ~38).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bits<const W: usize> {
    w: [u64; W],
}

impl<const W: usize> Default for Bits<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize> Bits<W> {
    pub const fn new() -> Self {
        Self { w: [0; W] }
    }

    const fn at(h: u32) -> (usize, u64) {
        let bit = (h as usize) % (64 * W);
        (bit / 64, 1u64 << (bit % 64))
    }

    /// Set the bit for `h`; true if it was clear.
    pub fn insert(&mut self, h: u32) -> bool {
        let (i, m) = Self::at(h);
        let was = self.w[i] & m != 0;
        self.w[i] |= m;
        !was
    }

    pub fn contains(&self, h: u32) -> bool {
        let (i, m) = Self::at(h);
        self.w[i] & m != 0
    }

    pub fn count(&self) -> u32 {
        self.w.iter().map(|w| w.count_ones()).sum()
    }

    pub fn clear(&mut self) {
        self.w = [0; W];
    }
}

/// "Was this value seen in the last one to two generations?" Two bitmaps
/// swap every `gen_ms`: a value is remembered for at least `gen_ms` and at
/// most `2 * gen_ms` after it was last seen.
#[derive(Clone, Debug)]
pub struct Recent<const W: usize> {
    cur: Bits<W>,
    prev: Bits<W>,
    since: Ms,
    gen_ms: Ms,
    started: bool,
}

impl<const W: usize> Recent<W> {
    pub const fn new(gen_ms: Ms) -> Self {
        Self { cur: Bits::new(), prev: Bits::new(), since: 0, gen_ms, started: false }
    }

    fn roll(&mut self, now: Ms) {
        if !self.started {
            self.started = true;
            self.since = now;
            return;
        }
        let age = elapsed(now, self.since);
        if age >= 2 * self.gen_ms {
            self.cur.clear();
            self.prev.clear();
            self.since = now;
        } else if age >= self.gen_ms {
            self.prev = self.cur;
            self.cur.clear();
            self.since = now;
        }
    }

    /// Record `h`; true if it had been seen recently already.
    pub fn seen_before(&mut self, h: u32, now: Ms) -> bool {
        self.roll(now);
        let old = self.cur.contains(h) || self.prev.contains(h);
        self.cur.insert(h);
        old
    }
}

/// Wi-Fi channel number to a compact index: 2.4 GHz 1-14 -> 0-13,
/// 5 GHz 32-144 (multiples of 4) -> 14-42, 149-177 (4n+1) -> 43-50.
pub const CHANNEL_SLOTS: usize = 51;

pub const fn channel_slot(ch: u8) -> Option<usize> {
    match ch {
        1..=14 => Some(ch as usize - 1),
        32..=144 if ch.is_multiple_of(4) => Some(14 + (ch as usize - 32) / 4),
        149..=177 if ch % 4 == 1 => Some(43 + (ch as usize - 149) / 4),
        _ => None,
    }
}

/// Centre frequency of a Wi-Fi channel, MHz (0 if not a channel number).
pub const fn channel_mhz(ch: u8) -> u32 {
    match ch {
        1..=13 => 2407 + 5 * ch as u32,
        14 => 2484,
        32..=177 => 5000 + 5 * ch as u32,
        _ => 0,
    }
}

/// Per-channel warm-up. A node hops: when it lands on a channel it has not
/// heard for a while (a short scouting visit, the first seconds after boot
/// or a new plan), every network there looks new. Detectors that count
/// "new" things ignore a channel until it has been heard continuously for
/// [`WARM_MS`](crate::beacons::WARM_MS).
#[derive(Clone, Debug)]
pub struct Warmth {
    last: [Ms; CHANNEL_SLOTS],
    warm_at: [Ms; CHANNEL_SLOTS],
    heard: u64,
    cold_gap: Ms,
    warm_ms: Ms,
}

impl Warmth {
    pub const fn new(cold_gap: Ms, warm_ms: Ms) -> Self {
        Self { last: [0; CHANNEL_SLOTS], warm_at: [0; CHANNEL_SLOTS], heard: 0, cold_gap, warm_ms }
    }

    /// A frame was heard on `ch`: is the channel warm? Channels without a
    /// slot (odd numbers outside the plan grid) are never warm.
    pub fn note(&mut self, ch: u8, now: Ms) -> bool {
        let Some(i) = channel_slot(ch) else { return false };
        let bit = 1u64 << i;
        if self.heard & bit == 0 || elapsed(now, self.last[i]) > self.cold_gap {
            self.heard |= bit;
            self.warm_at[i] = now.wrapping_add(self.warm_ms);
        }
        self.last[i] = now;
        // "now >= warm_at" on the wrapping clock.
        (now.wrapping_sub(self.warm_at[i]) as i32) >= 0
    }
}

/// At most one report per (kind, key) every `gap_ms`. The stalest entry is
/// replaced when the table is full, so a forgotten key may report early:
/// harmless (one extra line), never a missed attack.
#[derive(Clone, Debug)]
pub struct RateLimit<const N: usize> {
    seen: heapless::Vec<(u8, [u8; 6], Ms), N>,
    gap_ms: Ms,
}

impl<const N: usize> RateLimit<N> {
    pub const fn new(gap_ms: Ms) -> Self {
        Self { seen: heapless::Vec::new(), gap_ms }
    }

    pub fn allow(&mut self, kind: u8, key: [u8; 6], now: Ms) -> bool {
        if let Some(e) = self.seen.iter_mut().find(|e| e.0 == kind && e.1 == key) {
            if elapsed(now, e.2) < self.gap_ms {
                return false;
            }
            e.2 = now;
            return true;
        }
        if self.seen.is_full()
            && let Some(i) = (0..self.seen.len()).max_by_key(|&i| elapsed(now, self.seen[i].2))
        {
            self.seen.swap_remove(i);
        }
        let _ = self.seen.push((kind, key, now));
        true
    }
}

/// "Is this retry a copy of a frame already heard?" The receiver's own
/// duplicate filter (IEEE 802.11-2020 10.3.2.14): a frame with the Retry bit
/// whose (transmitter, sequence number) equals the last one heard from that
/// transmitter is a retransmission and is discarded; any other frame,
/// Retry bit or not, is acted on. So a retry only counts as "nothing new"
/// when it really repeats the previous frame.
///
/// `N` transmitters are remembered, one 4-byte entry each (20 bits of a
/// hash of the address, the 12-bit sequence number); a new transmitter
/// takes the slots in turn. An address that falls out simply makes its next retry count, the
/// safe direction (a flood is never hidden; a genuine retransmission storm
/// from more than `N` transmitters at one target may be counted once more).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dups<const N: usize> {
    last: [u32; N],
    next: u8,
}

impl<const N: usize> Default for Dups<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Dups<N> {
    pub const fn new() -> Self {
        Self { last: [0; N], next: 0 }
    }

    /// Record a frame from `tx` with sequence number `seq`; true when it
    /// is a retry of the frame last heard from `tx` (a real duplicate).
    pub fn duplicate(&mut self, tx: &[u8; 6], seq: u16, retry: bool) -> bool {
        // Bit 27 always set, so an empty slot (0) matches nothing.
        let me = (hash(tx) & 0xFFFF_F000) | 0x0800_0000;
        let key = me | u32::from(seq & 0x0FFF);
        let slot = self.last.iter().position(|&e| e & 0xFFFF_F000 == me);
        let dup = retry && slot.is_some_and(|i| self.last[i] == key);
        let i = slot.unwrap_or_else(|| {
            let i = usize::from(self.next) % N;
            self.next = ((i + 1) % N) as u8;
            i
        });
        self.last[i] = key;
        dup
    }
}

/// Locally administered (bit 1 of the first octet): random and virtual
/// addresses. Beacon spam tools almost always use them, but so do many
/// real access points for their second and third networks.
pub const fn locally_administered(mac: &[u8; 6]) -> bool {
    mac[0] & 0x02 != 0
}

pub const BROADCAST: [u8; 6] = [0xFF; 6];
pub const NONE: [u8; 6] = [0; 6];

/// Printable ASCII copy of a network name for `ThreatSighting::ssid`
/// (other bytes become `?`; at most 32).
pub fn ssid_string(ssid: &[u8]) -> heapless::String<32> {
    let mut s = heapless::String::new();
    for &b in ssid.iter().take(32) {
        let _ = s.push(if (0x20..0x7F).contains(&b) { b as char } else { '?' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_spreads() {
        assert_ne!(hash(b"a"), hash(b"b"));
        assert_ne!(hash(&[0, 0, 0, 0, 0, 1]), hash(&[0, 0, 0, 0, 0, 2]));
        // Low bits differ for MACs differing only in the last octet.
        let mut b = Bits::<8>::new();
        let mut set = 0;
        for i in 0..40u8 {
            if b.insert(hash(&[2, 0, 0, 0, 0, i])) {
                set += 1;
            }
        }
        assert!(set >= 36, "{set}");
        assert_eq!(b.count(), set);
    }

    #[test]
    fn bits_basics() {
        let mut b = Bits::<1>::new();
        assert!(b.insert(5));
        assert!(!b.insert(5));
        assert!(b.contains(5) && !b.contains(6));
        assert!(b.contains(5 + 64), "same bit modulo 64");
        b.clear();
        assert_eq!(b.count(), 0);
    }

    #[test]
    fn recent_remembers_one_to_two_generations() {
        let mut r = Recent::<4>::new(1000);
        assert!(!r.seen_before(1, 0));
        assert!(r.seen_before(1, 500));
        assert!(r.seen_before(1, 1500), "previous generation");
        // Last seen at 1500 (generation started at 1000): still known at 2999.
        assert!(r.seen_before(1, 2999));
        assert!(!r.seen_before(2, 3000));
        // Gap of two generations: everything forgotten.
        assert!(!r.seen_before(1, 9000));
    }

    #[test]
    fn recent_survives_clock_wrap() {
        let mut r = Recent::<4>::new(1000);
        let t = u32::MAX - 100;
        assert!(!r.seen_before(7, t));
        assert!(r.seen_before(7, t.wrapping_add(500)));
    }

    #[test]
    fn channel_slots_are_unique() {
        let mut seen = [false; CHANNEL_SLOTS];
        for ch in 0..=255u8 {
            if let Some(i) = channel_slot(ch) {
                assert!(!seen[i], "{ch}");
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(channel_slot(149), Some(43));
        assert_eq!(channel_slot(177), Some(50));
        assert_eq!(channel_slot(165), Some(47));
        assert_eq!(channel_slot(15), None);
        assert_eq!(channel_mhz(1), 2412);
        assert_eq!(channel_mhz(14), 2484);
        assert_eq!(channel_mhz(149), 5745);
        assert_eq!(channel_mhz(0), 0);
    }

    #[test]
    fn warmth() {
        let mut w = Warmth::new(5000, 3000);
        assert!(!w.note(6, 0));
        assert!(!w.note(6, 2999));
        assert!(w.note(6, 3000));
        assert!(w.note(6, 7000), "gap 4000 < cold gap");
        assert!(!w.note(6, 13000), "cold again after a 6 s gap");
        assert!(!w.note(15, 13000), "no slot: never warm");
        assert!(!w.note(1, 13000), "channels are independent");
    }

    #[test]
    fn rate_limit() {
        let mut r = RateLimit::<2>::new(1000);
        assert!(r.allow(1, [1; 6], 0));
        assert!(!r.allow(1, [1; 6], 999));
        assert!(r.allow(2, [1; 6], 999), "other kind");
        assert!(r.allow(1, [1; 6], 1000));
        assert!(r.allow(1, [2; 6], 1000), "full: stalest replaced");
        assert!(!r.allow(1, [1; 6], 1500), "the fresh entry was kept");
    }

    #[test]
    fn dups_only_repeat_the_last_frame() {
        let (a, b, c) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2], [2, 0, 0, 0, 0, 3]);
        let mut d = Dups::<2>::new();
        assert!(!d.duplicate(&a, 7, false));
        assert!(d.duplicate(&a, 7, true), "a retransmission");
        assert!(d.duplicate(&a, 7, true), "and another");
        assert!(!d.duplicate(&a, 8, true), "Retry bit with a new number: a new frame");
        assert!(!d.duplicate(&a, 8, false), "same number without the Retry bit: acted on");
        assert!(!d.duplicate(&b, 8, true), "another transmitter's");
        assert!(d.duplicate(&a, 8, true));
        // A third transmitter takes a's slot; a's next retry then counts.
        assert!(!d.duplicate(&c, 1, false));
        assert!(!d.duplicate(&a, 8, true));
    }

    #[test]
    fn ssid_printable() {
        assert_eq!(ssid_string(b"Cafe \"x\"").as_str(), "Cafe \"x\"");
        assert_eq!(ssid_string(&[0x41, 0x00, 0xFF]).as_str(), "A??");
        assert_eq!(ssid_string(&[b'a'; 40]).len(), 32);
    }
}
