//! Bluetooth LE pairing pop-up spam ("BLE spam"): a Flipper Zero, an ESP32
//! running Marauder / AppleJuice-style firmware or a phone app sending
//! fake pairing adverts, each from a new random address, so every phone
//! nearby keeps showing "connect your new earbuds?" dialogs.
//!
//! Recognised advert families ([`classify`]):
//!
//! | Family | What | Source |
//! |---|---|---|
//! | Apple | Manufacturer data, company `0x004C`, Continuity type `0x07` (Proximity Pairing, AirPods-style) or `0x0F` (Nearby Action, Apple TV / setup pop-ups) | Celosia & Cunche, "Discontinued Privacy", PoPETs 2020; Martin et al., "Handoff All Your Privacy", PoPETs 2019 |
//! | Samsung | Manufacturer data, company `0x0075` (Galaxy Buds / Watch easy-pairing pop-ups) | Bluetooth SIG company identifiers |
//! | Microsoft | Manufacturer data, company `0x0006`, Microsoft Beacon ID `0x03` (Swift Pair) | Microsoft Learn, "Swift Pair" |
//! | Google | Service data for UUID `0xFE2C` (Fast Pair) | Google Fast Pair specification |
//!
//! Real devices of these families exist everywhere (phones, earbuds, watches),
//! but each keeps its address for many minutes (LE privacy rotates about every
//! 15 min, and Swift Pair asks devices not to rotate during pairing). Spam
//! uses a fresh random address for nearly every advert. So the detector
//! counts **addresses it has not heard in the last 5-10 s**, per family,
//! over a five-second window ([`SPAM_NEW`]).

use crate::util::{Ms, NONE, Recent, elapsed, hash, ms};
use crate::{ThreatKind, ThreatSighting};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Family {
    Apple = 0,
    Samsung = 1,
    Microsoft = 2,
    Google = 3,
}

impl Family {
    pub const ALL: [Family; 4] = [Family::Apple, Family::Samsung, Family::Microsoft, Family::Google];

    /// Short name, sent as `ThreatSighting::ssid` of a `BleSpam` sighting.
    pub const fn name(self) -> &'static str {
        match self {
            Family::Apple => "apple",
            Family::Samsung => "samsung",
            Family::Microsoft => "microsoft",
            Family::Google => "google",
        }
    }
}

const AD_SERVICE_DATA_16: u8 = 0x16;
const AD_MANUFACTURER: u8 = 0xFF;
const APPLE: u16 = 0x004C;
const SAMSUNG: u16 = 0x0075;
const MICROSOFT: u16 = 0x0006;
const FAST_PAIR: u16 = 0xFE2C;

/// The pairing pop-up family an advert belongs to, if any. Walks the AD
/// structures; never panics; stops at a zero length (padding).
pub fn classify(ad: &[u8]) -> Option<Family> {
    let mut rest = ad;
    while let [len, tail @ ..] = rest {
        let len = usize::from(*len);
        if len == 0 {
            return None;
        }
        let structure = tail.get(..len)?;
        rest = &tail[len..];
        let (&kind, data) = structure.split_first()?;
        let id = |d: &[u8]| d.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        match (kind, id(data)) {
            (AD_MANUFACTURER, Some(APPLE)) if matches!(data.get(2), Some(0x07 | 0x0F)) => return Some(Family::Apple),
            (AD_MANUFACTURER, Some(SAMSUNG)) => return Some(Family::Samsung),
            (AD_MANUFACTURER, Some(MICROSOFT)) if data.get(2) == Some(&0x03) => return Some(Family::Microsoft),
            (AD_SERVICE_DATA_16, Some(FAST_PAIR)) => return Some(Family::Google),
            _ => {}
        }
    }
    None
}

/// New addresses of one family in the window that make it spam. Earbud
/// cases being opened in a crowded room give a handful a minute; spam
/// tools give several a second (Flipper Zero's app cycles adverts every
/// few tens of ms).
pub const SPAM_NEW: u32 = 20;
/// While spam is on, keep reporting down to this level.
pub const SPAM_HOLD: u32 = 10;
/// Window length = buckets x one second.
pub const BUCKETS: usize = 5;
const BUCKET_MS: Ms = 1000;
/// An address is "known" for one to two of these after it was last heard.
pub const KNOWN_GEN_MS: Ms = 5000;

/// Running totals since boot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Adverts in one of the pairing families.
    pub pairing: u32,
    /// Of those, from an address not heard recently.
    pub new: u32,
    pub threats: u32,
}

pub struct BleWatch {
    known: Recent<16>,
    /// Adverts before this time teach addresses but never count as new
    /// (at boot, or after a gap, every device nearby is "new").
    warm_until: Ms,
    new: [[u16; 4]; BUCKETS],
    adverts: [u16; BUCKETS],
    rssi: [i8; BUCKETS],
    cur: usize,
    start: Ms,
    started: bool,
    alarmed: bool,
    pub totals: Totals,
}

impl Default for BleWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl BleWatch {
    pub const fn new() -> Self {
        Self {
            known: Recent::new(KNOWN_GEN_MS),
            warm_until: 0,
            new: [[0; 4]; BUCKETS],
            adverts: [0; BUCKETS],
            rssi: [i8::MIN; BUCKETS],
            cur: 0,
            start: 0,
            started: false,
            alarmed: false,
            totals: Totals { pairing: 0, new: 0, threats: 0 },
        }
    }

    /// One advert: address (any byte order, as long as it is consistent),
    /// RSSI, AD data. Cheap enough for the BLE host's receive loop; never
    /// reports (see [`BleWatch::poll`]). Returns the family if it was one.
    pub fn advert(&mut self, addr: &[u8; 6], rssi: i8, ad: &[u8], now_us: u64) -> Option<Family> {
        let fam = classify(ad)?;
        self.record(addr, rssi, fam, now_us);
        Some(fam)
    }

    /// Same, for a caller that already ran [`classify`].
    pub fn record(&mut self, addr: &[u8; 6], rssi: i8, fam: Family, now_us: u64) {
        let now = ms(now_us);
        if !self.started {
            self.started = true;
            self.start = now;
            self.warm_until = now.wrapping_add(KNOWN_GEN_MS);
        }
        self.totals.pairing = self.totals.pairing.wrapping_add(1);
        let i = self.cur;
        self.adverts[i] = self.adverts[i].saturating_add(1);
        self.rssi[i] = self.rssi[i].max(rssi);
        let warming = (now.wrapping_sub(self.warm_until) as i32) < 0;
        if !self.known.seen_before(hash(addr), now) && !warming {
            self.totals.new = self.totals.new.wrapping_add(1);
            let n = &mut self.new[i][fam as usize];
            *n = n.saturating_add(1);
        }
    }

    /// Advance the one-second buckets; at each step, report if the last
    /// five seconds look like spam. Call about once a second.
    pub fn poll(&mut self, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        let now = ms(now_us);
        if !self.started {
            self.started = true;
            self.start = now;
            self.warm_until = now.wrapping_add(KNOWN_GEN_MS);
            return;
        }
        let mut steps = 0;
        while elapsed(now, self.start) >= BUCKET_MS {
            if steps < BUCKETS {
                self.judge(now_us, emit);
            }
            steps += 1;
            self.start = self.start.wrapping_add(BUCKET_MS);
            self.cur = (self.cur + 1) % BUCKETS;
            self.new[self.cur] = [0; 4];
            self.adverts[self.cur] = 0;
            self.rssi[self.cur] = i8::MIN;
            if steps > BUCKETS {
                // A long gap (scanning stopped?): everything is stale;
                // restart the clock and learn the room again.
                self.start = now;
                self.warm_until = now.wrapping_add(KNOWN_GEN_MS);
                break;
            }
        }
    }

    fn judge(&mut self, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        let per = Family::ALL.map(|f| self.new.iter().map(|b| u32::from(b[f as usize])).sum::<u32>());
        let (top, &most) = per.iter().enumerate().max_by_key(|&(_, n)| *n).unwrap_or((0, &0));
        let alarm = most >= SPAM_NEW || (self.alarmed && most >= SPAM_HOLD);
        self.alarmed = alarm;
        if !alarm {
            return;
        }
        self.totals.threats = self.totals.threats.wrapping_add(1);
        let mut ssid = heapless::String::new();
        let _ = ssid.push_str(Family::ALL[top].name());
        emit(ThreatSighting {
            kind: ThreatKind::BleSpam,
            // HCI advertising reports do not say which channel.
            channel: 0,
            freq: 0,
            width_khz: 0,
            level: self.rssi.iter().copied().max().unwrap_or(i8::MIN),
            count: self.adverts.iter().map(|&n| u32::from(n)).sum(),
            window_ms: BUCKET_MS * BUCKETS as u32,
            source: NONE,
            target: NONE,
            detail: per.iter().sum(),
            ssid,
            uptime_us: now_us,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn apple(kind: u8) -> Vec<u8> {
        // Flags, then manufacturer data 4C 00 <type> <len> ...
        let mut v = std::vec![2, 0x01, 0x06, 0x1E, 0xFF, 0x4C, 0x00, kind, 0x19];
        v.extend_from_slice(&[0x01; 25]);
        v
    }

    fn rnd(i: u32) -> [u8; 6] {
        let h = hash(&i.to_le_bytes()).to_le_bytes();
        [h[0], h[1], h[2], h[3], i as u8, 0xC0 | (i >> 8) as u8]
    }

    #[test]
    fn families() {
        assert_eq!(classify(&apple(0x07)), Some(Family::Apple));
        assert_eq!(classify(&apple(0x0F)), Some(Family::Apple));
        // iPhone "Nearby Info" (0x10): every iPhone, all the time. Not a pop-up.
        assert_eq!(classify(&apple(0x10)), None);
        assert_eq!(classify(&[5, 0xFF, 0x75, 0x00, 0x01, 0x00]), Some(Family::Samsung));
        assert_eq!(classify(&[6, 0xFF, 0x06, 0x00, 0x03, 0x00, 0x80]), Some(Family::Microsoft));
        // Microsoft CDP beacon (ID 0x01): Windows PCs, not Swift Pair.
        assert_eq!(classify(&[6, 0xFF, 0x06, 0x00, 0x01, 0x09, 0x20]), None);
        assert_eq!(classify(&[6, 0x16, 0x2C, 0xFE, 0x00, 0x12, 0x34]), Some(Family::Google));
        // Remote ID service data (0xFFFA).
        assert_eq!(classify(&[6, 0x16, 0xFA, 0xFF, 0x0D, 0x00, 0x00]), None);
        // Malformed: length past the end, zero length, empty.
        assert_eq!(classify(&[30, 0xFF, 0x4C, 0x00, 0x07]), None);
        assert_eq!(classify(&[0, 0xFF, 0x4C]), None);
        assert_eq!(classify(&[]), None);
        assert_eq!(classify(&[1]), None);
        assert_eq!(Family::Google.name(), "google");
    }

    #[test]
    fn classify_never_panics() {
        let mut x: u32 = 7;
        for _ in 0..5000 {
            let mut ad = [0u8; 31];
            for b in ad.iter_mut() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = (x % 40) as u8;
            }
            let _ = classify(&ad);
        }
    }

    /// Run `secs` seconds with `per_sec` adverts a second from `addr(i)`.
    fn run(w: &mut BleWatch, t0: u64, secs: u64, per_sec: u64, ad: &[u8], mut addr: impl FnMut(u64) -> [u8; 6]) -> Vec<ThreatSighting> {
        let mut out = Vec::new();
        for s in 0..secs {
            for k in 0..per_sec {
                let t = (t0 + s * 1000) * 1000 + k * 1_000_000 / per_sec;
                w.advert(&addr(s * per_sec + k), -60, ad, t);
            }
            w.poll((t0 + s * 1000 + 1000) * 1000, &mut |x| out.push(x));
        }
        out
    }

    #[test]
    fn real_devices_are_quiet() {
        let mut w = BleWatch::new();
        w.poll(0, &mut |_| {});
        // 40 Samsung phones and 10 AirPods cases, each with a fixed address,
        // advertising several times a second for a minute.
        let samsung = [5, 0xFF, 0x75, 0x00, 0x01, 0x00];
        let mut out = run(&mut w, 0, 60, 200, &samsung, |i| rnd((i % 40) as u32));
        out.extend(run(&mut w, 60_000, 60, 50, &apple(0x07), |i| rnd(1000 + (i % 10) as u32)));
        assert!(out.is_empty(), "{out:?}");
        // A few earbud cases opened (new addresses) now and then.
        out.extend(run(&mut w, 120_000, 30, 3, &apple(0x07), |i| rnd(2000 + (i / 30) as u32)));
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn spam_is_reported_each_second() {
        let mut w = BleWatch::new();
        w.poll(0, &mut |_| {});
        // A quiet room while the node learns it.
        assert!(run(&mut w, 0, 6, 0, &[], |_| [0; 6]).is_empty());
        // Flipper-style: 20 adverts a second, every one from a new address.
        let out = run(&mut w, 6000, 8, 20, &apple(0x0F), |i| rnd(5000 + i as u32));
        assert_eq!(out.len(), 8, "{out:?}");
        let s = &out[7];
        assert_eq!(s.kind, ThreatKind::BleSpam);
        assert_eq!(s.ssid.as_str(), "apple");
        assert!(s.detail >= 90, "{}", s.detail);
        assert_eq!(s.window_ms, 5000);
        assert_eq!(s.level, -60);
        // Stops a few seconds after the spam does (the window drains).
        let after = run(&mut w, 14_000, 10, 0, &apple(0x0F), |_| [0; 6]);
        assert!(after.len() < 5, "{}", after.len());
        assert!(w.totals.threats >= 8);
    }

    #[test]
    fn slower_spam_and_mixed_families() {
        let mut w = BleWatch::new();
        w.poll(0, &mut |_| {});
        let fp = [6, 0x16, 0x2C, 0xFE, 0x00, 0x12, 0x34];
        assert!(run(&mut w, 0, 6, 0, &[], |_| [0; 6]).is_empty());
        // 5 new Fast Pair addresses a second: 25 in the window.
        let out = run(&mut w, 6000, 6, 5, &fp, |i| rnd(9000 + i as u32));
        assert!(!out.is_empty());
        assert_eq!(out.last().unwrap().ssid.as_str(), "google");
    }

    #[test]
    fn long_gap_between_polls() {
        let mut w = BleWatch::new();
        w.poll(0, &mut |_| {});
        let mut n = 0;
        w.poll(3_600_000_000, &mut |_| n += 1);
        assert_eq!(n, 0);
        w.poll(3_601_000_000, &mut |_| n += 1);
        assert_eq!(n, 0);
    }

    #[test]
    fn boot_into_a_crowd_is_quiet() {
        let mut w = BleWatch::new();
        w.poll(0, &mut |_| {});
        // 60 devices with fixed addresses heard from the first second.
        let out = run(&mut w, 0, 20, 120, &apple(0x07), |i| rnd((i % 60) as u32));
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn non_pairing_adverts_ignored() {
        let mut w = BleWatch::new();
        assert_eq!(w.advert(&[1; 6], -50, &apple(0x10), 0), None);
        assert_eq!(w.totals.pairing, 0);
    }
}
