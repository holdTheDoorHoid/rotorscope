//! NMEA 0183 GGA + RMC handling (any talker ID), plus GSA/GSV into the sky view.
//!
//! GGA and RMC arrive as separate sentences of the same epoch. They are paired by their
//! UTC time-of-day field and merged into one [`Fix`]. If the partner never arrives, the lone
//! sentence is flushed as a `Fix` when the first sentence of the next epoch shows up.

use crate::fix::{Fix, FixType, Source, UtcTime};
use crate::sats::Collector;
use crate::sky::SkyView;

/// XOR of every byte between `$` and `*`.
pub(crate) fn xor_checksum(body: &[u8]) -> u8 {
    body.iter().fold(0, |a, &b| a ^ b)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Check a complete line (`$body*HH\r\n` or `$body*HH\n`) and return `body`.
pub(crate) fn verify(line: &[u8]) -> Option<&[u8]> {
    let mut l = line;
    if let [rest @ .., b'\n'] = l {
        l = rest;
    }
    if let [rest @ .., b'\r'] = l {
        l = rest;
    }
    if l.first() != Some(&b'$') || l.len() < 4 {
        return None;
    }
    let star = l.len() - 3;
    if l[star] != b'*' {
        return None;
    }
    let want = hex(l[star + 1])? << 4 | hex(l[star + 2])?;
    let body = &l[1..star];
    if body.contains(&b'*') || xor_checksum(body) != want {
        return None;
    }
    Some(body)
}

type R<T> = Result<T, ()>;

/// Parse an unsigned/signed decimal such as `4807.038`, `-12.5`, `7`. Empty -> `Ok(None)`.
fn dec(f: &[u8]) -> R<Option<f64>> {
    if f.is_empty() {
        return Ok(None);
    }
    let (neg, f) = match f {
        [b'-', rest @ ..] => (true, rest),
        _ => (false, f),
    };
    let mut mant: u64 = 0;
    let mut scale = 1.0f64;
    let mut digits = 0;
    let mut seen_dot = false;
    for &c in f {
        match c {
            b'0'..=b'9' => {
                digits += 1;
                if digits <= 18 {
                    mant = mant * 10 + (c - b'0') as u64;
                    if seen_dot {
                        scale *= 10.0;
                    }
                } else if !seen_dot {
                    return Err(()); // absurdly long integer part
                }
            }
            b'.' if !seen_dot => seen_dot = true,
            _ => return Err(()),
        }
    }
    if digits == 0 {
        return Err(());
    }
    let v = mant as f64 / scale;
    Ok(Some(if neg { -v } else { v }))
}

fn uint(f: &[u8]) -> R<Option<u32>> {
    if f.is_empty() {
        return Ok(None);
    }
    let mut v: u32 = 0;
    for &c in f {
        if !c.is_ascii_digit() {
            return Err(());
        }
        v = v.checked_mul(10).and_then(|x| x.checked_add((c - b'0') as u32)).ok_or(())?;
    }
    Ok(Some(v))
}

/// `ddmm.mmmm` (or `dddmm.mmmm`) plus hemisphere to signed degrees.
fn coord(v: &[u8], hemi: &[u8], neg_hemi: u8, max_deg: f64) -> R<Option<f64>> {
    let (Some(raw), h) = (dec(v)?, hemi) else {
        return Ok(None);
    };
    if h.len() != 1 || (h[0] != neg_hemi && h[0] != opposite(neg_hemi)) || raw < 0.0 {
        return Err(());
    }
    let deg = (raw / 100.0) as u32 as f64;
    let min = raw - deg * 100.0;
    if min >= 60.0 {
        return Err(());
    }
    let mut d = deg + min / 60.0;
    if d > max_deg {
        return Err(());
    }
    if h[0] == neg_hemi {
        d = -d;
    }
    Ok(Some(d))
}

fn opposite(h: u8) -> u8 {
    match h {
        b'S' => b'N',
        b'W' => b'E',
        _ => 0,
    }
}

/// Time of day in milliseconds since midnight, from `hhmmss[.sss]`.
fn tod(f: &[u8]) -> R<Option<u32>> {
    if f.is_empty() {
        return Ok(None);
    }
    if f.len() < 6 || !f[..6].iter().all(u8::is_ascii_digit) {
        return Err(());
    }
    let d = |i: usize| ((f[i] - b'0') * 10 + (f[i + 1] - b'0')) as u32;
    let (h, m, s) = (d(0), d(2), d(4));
    if h > 23 || m > 59 || s > 60 {
        return Err(());
    }
    let mut ms = 0u32;
    match &f[6..] {
        [] => {}
        [b'.', frac @ ..] => {
            let mut place = 100;
            for &c in frac {
                if !c.is_ascii_digit() {
                    return Err(());
                }
                ms += (c - b'0') as u32 * place;
                place /= 10;
                if place == 0 {
                    break;
                }
            }
        }
        _ => return Err(()),
    }
    Ok(Some(((h * 60 + m) * 60 + s) * 1000 + ms))
}

#[derive(Clone, Copy)]
struct Gga {
    tod: Option<u32>,
    lat: Option<f64>,
    lon: Option<f64>,
    quality: u32,
    sats: u8,
    hdop: Option<f32>,
    alt: Option<f32>,
    sep: Option<f32>,
}

#[derive(Clone, Copy)]
struct Rmc {
    tod: Option<u32>,
    valid: bool,
    lat: Option<f64>,
    lon: Option<f64>,
    speed_mps: Option<f32>,
    course: Option<f32>,
    date: Option<(u16, u8, u8)>,
}

/// GSV with four satellites and a signal ID has 21 fields.
const MAX_FIELDS: usize = 24;

fn split<'a>(body: &'a [u8], out: &mut [&'a [u8]; MAX_FIELDS]) -> usize {
    let mut n = 0;
    for f in body.split(|&c| c == b',') {
        if n == MAX_FIELDS {
            break;
        }
        out[n] = f;
        n += 1;
    }
    n
}

fn parse_gga(f: &[&[u8]]) -> R<Gga> {
    if f.len() < 10 {
        return Err(());
    }
    Ok(Gga {
        tod: tod(f[1])?,
        lat: coord(f[2], f[3], b'S', 90.0)?,
        lon: coord(f[4], f[5], b'W', 180.0)?,
        quality: uint(f[6])?.unwrap_or(0),
        sats: uint(f[7])?.unwrap_or(0).min(255) as u8,
        hdop: dec(f[8])?.map(|v| v as f32),
        alt: dec(f[9])?.map(|v| v as f32),
        sep: if f.len() > 11 { dec(f[11])?.map(|v| v as f32) } else { None },
    })
}

fn parse_rmc(f: &[&[u8]]) -> R<Rmc> {
    if f.len() < 10 {
        return Err(());
    }
    let mut valid = f[2] == b"A";
    if !valid && f[2] != b"V" && !f[2].is_empty() {
        return Err(());
    }
    // NMEA 2.3+ mode indicator: N = not valid, E = estimated (still a fix).
    if f.len() > 12 && f[12] == b"N" {
        valid = false;
    }
    let date = match f[9] {
        [] => None,
        d if d.len() == 6 && d.iter().all(u8::is_ascii_digit) => {
            let n = |i: usize| (d[i] - b'0') * 10 + (d[i + 1] - b'0');
            let (day, mon, yy) = (n(0), n(2), n(4));
            if !(1..=31).contains(&day) || !(1..=12).contains(&mon) {
                return Err(());
            }
            let year = if yy < 80 { 2000 + yy as u16 } else { 1900 + yy as u16 };
            Some((year, mon, day))
        }
        _ => return Err(()),
    };
    Ok(Rmc {
        tod: tod(f[1])?,
        valid,
        lat: coord(f[3], f[4], b'S', 90.0)?,
        lon: coord(f[5], f[6], b'W', 180.0)?,
        speed_mps: dec(f[7])?.map(|k| (k * 0.514_444_4).max(0.0) as f32),
        course: dec(f[8])?.map(|c| {
            let c = c as f32;
            if c >= 360.0 { c - 360.0 } else { c }
        }),
        date,
    })
}

/// Pairs GGA and RMC of one epoch.
pub(crate) struct Pending {
    /// Epoch key: time of day in ms, or `u32::MAX` when the sentence carried no time.
    key: u32,
    gga: Option<Gga>,
    rmc: Option<Rmc>,
    /// Satellites per constellation and band, from GSV and GSA.
    pub(crate) sky: SkyView,
    /// Satellites one by one, from GSV (and from UBX NAV-SAT, written by the parser).
    pub(crate) sats: Collector,
}

impl Pending {
    pub(crate) const fn new() -> Self {
        Pending { key: 0, gga: None, rmc: None, sky: SkyView::new(), sats: Collector::new() }
    }

    /// Feed a checksum-verified sentence body. Returns a fix when an epoch completes (or a
    /// previous, half-received epoch is flushed).
    pub(crate) fn accept(&mut self, body: &[u8]) -> Option<Fix> {
        let mut fields: [&[u8]; MAX_FIELDS] = [&[]; MAX_FIELDS];
        let n = split(body, &mut fields);
        let f = &fields[..n];
        let head = f[0];
        // Talker (2 letters, any) + type. Proprietary $P... sentences are ignored.
        if head.len() != 5 || !head[..2].iter().all(u8::is_ascii_uppercase) || head[0] == b'P' {
            return None;
        }
        match &head[2..] {
            b"GSV" => {
                self.sky.gsv(f);
                self.sats.gsv(f);
                return None;
            }
            b"GSA" => {
                self.sky.gsa(f);
                return None;
            }
            _ => {}
        }
        let is_gga = &head[2..] == b"GGA";
        let is_rmc = &head[2..] == b"RMC";
        let (gga, rmc, tod) = if is_gga {
            let g = parse_gga(f).ok()?;
            (Some(g), None, g.tod)
        } else if is_rmc {
            let r = parse_rmc(f).ok()?;
            (None, Some(r), r.tod)
        } else {
            return None;
        };
        let key = tod.unwrap_or(u32::MAX);
        let mut out = None;
        let have_any = self.gga.is_some() || self.rmc.is_some();
        let duplicate = (is_gga && self.gga.is_some()) || (is_rmc && self.rmc.is_some());
        if have_any && (self.key != key || duplicate) {
            out = Some(self.take());
        }
        self.key = key;
        if gga.is_some() {
            self.gga = gga;
        }
        if rmc.is_some() {
            self.rmc = rmc;
        }
        if out.is_none() && self.gga.is_some() && self.rmc.is_some() {
            out = Some(self.take());
        }
        out
    }

    fn take(&mut self) -> Fix {
        let mut fix = merge(self.gga.as_ref(), self.rmc.as_ref());
        // GSA's fix mode (1 none, 2 2D, 3 3D) is the only 2D/3D flag NMEA has: a receiver
        // that says 2D still prints an altitude in GGA (the one it assumed).
        if fix.fix == FixType::ThreeD && self.sky.mode() == Some(2) {
            fix.fix = FixType::TwoD;
            fix.alt_msl_m = None;
            fix.alt_ellipsoid_m = None;
        }
        if fix.position_valid {
            fix.pdop = self.sky.gsa_pdop;
        }
        self.gga = None;
        self.rmc = None;
        self.sky.next_epoch();
        self.sats.publish();
        fix
    }
}

fn merge(gga: Option<&Gga>, rmc: Option<&Rmc>) -> Fix {
    let mut f = Fix::empty(Source::Nmea);

    // GGA quality: 0 invalid, 1 GPS, 2 DGPS, 3 PPS, 4/5 RTK, 6 dead reckoning, 7 manual, 8 sim.
    let gga_ok = gga.map(|g| matches!(g.quality, 1..=7));
    let rmc_ok = rmc.map(|r| r.valid);
    let ok = gga_ok.unwrap_or(true) && rmc_ok.unwrap_or(true);

    if let Some(g) = gga {
        f.sats_used = g.sats;
        f.hdop = g.hdop;
    }
    f.fix_ok = ok;

    // Position source: GGA first (more digits), else RMC.
    let pos = if !ok {
        None
    } else if let Some((la, lo)) = gga.and_then(|g| g.lat.zip(g.lon)) {
        Some((la, lo))
    } else {
        rmc.and_then(|r| r.lat.zip(r.lon))
    };

    if let Some((la, lo)) = pos {
        f.position_valid = true;
        f.lat = la;
        f.lon = lo;
        let dead_reckoning = gga.is_some_and(|g| g.quality == 6);
        let alt = gga.and_then(|g| g.alt);
        f.fix = if dead_reckoning {
            FixType::DeadReckoning
        } else if alt.is_some() {
            FixType::ThreeD
        } else {
            FixType::TwoD
        };
        if f.fix == FixType::ThreeD {
            f.alt_msl_m = alt;
            f.alt_ellipsoid_m = alt.zip(gga.and_then(|g| g.sep)).map(|(a, s)| a + s);
        }
        if let Some(r) = rmc {
            f.speed_mps = r.speed_mps;
            f.heading_deg = r.course;
        }
    }

    // UTC needs a date (RMC only) and a trustworthy time, i.e. a valid RMC.
    if let Some(r) = rmc {
        if let (true, Some((year, month, day)), Some(t)) = (r.valid, r.date, r.tod.or(gga.and_then(|g| g.tod))) {
            let secs = t / 1000;
            let u = UtcTime {
                year,
                month,
                day,
                hour: (secs / 3600) as u8,
                minute: (secs / 60 % 60) as u8,
                second: (secs % 60) as u8,
                millis: (t % 1000) as u16,
            };
            if u.is_sane() {
                f.utc = Some(u);
            }
        }
    }
    f
}
