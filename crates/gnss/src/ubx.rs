//! UBX framing and the handful of UBX messages this crate decodes.

use crate::fix::{Fix, FixType, Source, UtcTime};

/// UBX sync characters.
pub(crate) const SYNC1: u8 = 0xB5;
pub(crate) const SYNC2: u8 = 0x62;

pub(crate) const CLS_NAV: u8 = 0x01;
pub(crate) const ID_NAV_PVT: u8 = 0x07;
pub(crate) const ID_NAV_STATUS: u8 = 0x03;
pub(crate) const ID_NAV_CLOCK: u8 = 0x22;
pub(crate) const ID_NAV_SAT: u8 = 0x35;
pub(crate) const ID_MON_RF: u8 = 0x38;
pub(crate) const CLS_ACK: u8 = 0x05;
pub(crate) const ID_ACK_NAK: u8 = 0x00;
pub(crate) const ID_ACK_ACK: u8 = 0x01;
pub(crate) const CLS_CFG: u8 = 0x06;
pub(crate) const ID_CFG_VALSET: u8 = 0x8A;
pub(crate) const ID_CFG_VALGET: u8 = 0x8B;
pub(crate) const CLS_MON: u8 = 0x0A;
pub(crate) const ID_MON_VER: u8 = 0x04;

/// 8-bit Fletcher checksum used by UBX (RFC 1145 style), over class, id, length and payload.
/// Returns `(CK_A, CK_B)`.
pub fn fletcher(data: &[u8]) -> (u8, u8) {
    let (mut a, mut b) = (0u8, 0u8);
    for &x in data {
        a = a.wrapping_add(x);
        b = b.wrapping_add(a);
    }
    (a, b)
}

/// Write a complete UBX frame (`B5 62 class id len payload CK_A CK_B`) into `out`.
/// Returns the frame length, or `None` if `out` is too small or the payload exceeds 65535.
pub fn build_frame(out: &mut [u8], class: u8, id: u8, payload: &[u8]) -> Option<usize> {
    let n = payload.len();
    if n > u16::MAX as usize || out.len() < n + 8 {
        return None;
    }
    out[0] = SYNC1;
    out[1] = SYNC2;
    out[2] = class;
    out[3] = id;
    out[4..6].copy_from_slice(&(n as u16).to_le_bytes());
    out[6..6 + n].copy_from_slice(payload);
    let (a, b) = fletcher(&out[2..6 + n]);
    out[6 + n] = a;
    out[7 + n] = b;
    Some(n + 8)
}

fn u16le(p: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([p[o], p[o + 1]])
}
fn u32le(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}
fn i32le(p: &[u8], o: usize) -> i32 {
    u32le(p, o) as i32
}

/// Length of a NAV-PVT payload.
pub(crate) const NAV_PVT_LEN: usize = 92;

/// Decode a UBX-NAV-PVT payload (u-blox M10 SPG 5.10 interface description, 3.15.11).
pub(crate) fn nav_pvt_to_fix(p: &[u8]) -> Option<Fix> {
    if p.len() < NAV_PVT_LEN {
        return None;
    }
    let valid = p[11];
    let valid_date = valid & 0x01 != 0;
    let valid_time = valid & 0x02 != 0;
    let fully_resolved = valid & 0x04 != 0;
    let fix = FixType::from_ubx(p[20]);
    let fix_ok = p[21] & 0x01 != 0;
    let invalid_llh = u16le(p, 78) & 0x01 != 0;
    let position_valid = fix.has_position() && !invalid_llh;

    let mut f = Fix::empty(Source::UbxNavPvt);
    f.fix = fix;
    f.fix_ok = fix_ok;
    f.position_valid = position_valid;
    f.sats_used = p[23];
    f.itow_ms = Some(u32le(p, 0));
    f.utc_fully_resolved = fully_resolved;
    let t_acc = u32le(p, 12);
    f.time_acc_ns = if t_acc == u32::MAX { None } else { Some(t_acc) };
    f.pdop = Some(u16le(p, 76) as f32 * 0.01);

    if valid_date && valid_time {
        let nano = i32le(p, 16);
        let t = UtcTime {
            year: u16le(p, 4),
            month: p[6],
            day: p[7],
            hour: p[8],
            minute: p[9],
            second: p[10],
            millis: (nano.max(0) / 1_000_000).min(999) as u16,
        };
        if t.is_sane() {
            f.utc = Some(t);
        }
    }

    if position_valid {
        f.lon = i32le(p, 24) as f64 * 1e-7;
        f.lat = i32le(p, 28) as f64 * 1e-7;
        f.h_acc_m = Some(u32le(p, 40) as f32 / 1000.0);
        let g_speed = i32le(p, 60);
        f.speed_mps = Some(g_speed.max(0) as f32 / 1000.0);
        let mut h = i32le(p, 64) as f32 * 1e-5;
        if h < 0.0 {
            h += 360.0;
        }
        if h >= 360.0 {
            h -= 360.0;
        }
        f.heading_deg = Some(h);
        if fix.has_altitude() {
            f.alt_ellipsoid_m = Some(i32le(p, 32) as f32 / 1000.0);
            f.alt_msl_m = Some(i32le(p, 36) as f32 / 1000.0);
            f.v_acc_m = Some(u32le(p, 44) as f32 / 1000.0);
        }
    }
    Some(f)
}

/// Reply to a MON-VER poll (first two fields only; the extension strings are not kept).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonVer {
    /// NUL-terminated software version, e.g. `EXT CORE 5.10 (...)`.
    pub sw_version: [u8; 30],
    /// NUL-terminated hardware version, e.g. `000A0000`.
    pub hw_version: [u8; 10],
    /// `(major, minor)` from a `PROTVER=34.10` extension string, if present.
    pub protver: Option<(u8, u8)>,
}

fn cstr(b: &[u8]) -> &str {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    core::str::from_utf8(&b[..end]).unwrap_or("")
}

impl MonVer {
    /// Software version as text (empty if not valid UTF-8).
    pub fn sw_str(&self) -> &str {
        cstr(&self.sw_version)
    }
    /// Hardware version as text.
    pub fn hw_str(&self) -> &str {
        cstr(&self.hw_version)
    }
}

pub(crate) fn decode_mon_ver(p: &[u8]) -> Option<MonVer> {
    if p.len() < 40 {
        return None;
    }
    let mut sw = [0u8; 30];
    sw.copy_from_slice(&p[..30]);
    let mut hw = [0u8; 10];
    hw.copy_from_slice(&p[30..40]);
    let mut protver = None;
    for ext in p[40..].chunks(30) {
        let s = cstr(ext);
        if let Some(v) = s.strip_prefix("PROTVER=") {
            let mut it = v.split('.');
            let maj = it.next().and_then(|x| x.parse::<u8>().ok());
            let min = it.next().and_then(|x| x.parse::<u8>().ok());
            if let (Some(a), Some(b)) = (maj, min) {
                protver = Some((a, b));
            }
        }
    }
    Some(MonVer { sw_version: sw, hw_version: hw, protver })
}

/// One key/value pair from a CFG-VALGET reply. `value` holds the little-endian value
/// zero-extended (booleans are 0 or 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValGetItem {
    /// Configuration key ID.
    pub key: u32,
    /// Raw value.
    pub value: u64,
}

/// Maximum items kept from one CFG-VALGET reply.
pub const VALGET_MAX_ITEMS: usize = 8;

/// A CFG-VALGET reply (the first [`VALGET_MAX_ITEMS`] items).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValGet {
    /// Layer the values came from (0 RAM, 1 BBR, 2 flash, 7 default).
    pub layer: u8,
    /// Echo of the request's `position`.
    pub position: u16,
    /// Items; only `..count` are valid.
    pub items: [ValGetItem; VALGET_MAX_ITEMS],
    /// Number of valid entries in `items`.
    pub count: u8,
    /// More items were present than fit in `items`.
    pub truncated: bool,
}

impl ValGet {
    /// The valid items.
    pub fn items(&self) -> &[ValGetItem] {
        &self.items[..self.count as usize]
    }
    /// Look up one key.
    pub fn get(&self, key: u32) -> Option<u64> {
        self.items().iter().find(|i| i.key == key).map(|i| i.value)
    }
}

/// Value size in bytes implied by bits 30..28 of a key ID.
pub(crate) fn key_size(key: u32) -> Option<usize> {
    match (key >> 28) & 0x7 {
        1 | 2 => Some(1),
        3 => Some(2),
        4 => Some(4),
        5 => Some(8),
        _ => None,
    }
}

pub(crate) fn decode_val_get(p: &[u8]) -> Option<ValGet> {
    if p.len() < 4 || p[0] != 1 {
        return None;
    }
    let mut out = ValGet {
        layer: p[1],
        position: u16le(p, 2),
        items: [ValGetItem { key: 0, value: 0 }; VALGET_MAX_ITEMS],
        count: 0,
        truncated: false,
    };
    let mut o = 4;
    while o < p.len() {
        if o + 4 > p.len() {
            return None;
        }
        let key = u32le(p, o);
        let sz = key_size(key)?;
        o += 4;
        if o + sz > p.len() {
            return None;
        }
        let mut v = 0u64;
        for (i, &b) in p[o..o + sz].iter().enumerate() {
            v |= (b as u64) << (8 * i);
        }
        o += sz;
        if (out.count as usize) < VALGET_MAX_ITEMS {
            out.items[out.count as usize] = ValGetItem { key, value: v };
            out.count += 1;
        } else {
            out.truncated = true;
        }
    }
    Some(out)
}

/// UBX-NAV-STATUS, the parts the integrity monitor uses (UBX-21035062 R03, 3.15.17.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavStatus {
    /// GPS time of week, ms.
    pub itow_ms: u32,
    /// `flags2` bits 4..3, `spoofDetState`: 0 unknown or deactivated, 1 no spoofing indicated,
    /// 2 spoofing indicated, 3 multiple spoofing indications. The receiver's own detector,
    /// always on on the M10; it needs several constellations and cannot see spoofing that
    /// was already there at power-up (MIA-M10C integration manual UBX-23001616 R05, 3.5.2.2).
    pub spoof_det_state: u8,
    /// Milliseconds since receiver start or reset.
    pub msss: u32,
}

pub(crate) fn decode_nav_status(p: &[u8]) -> Option<NavStatus> {
    if p.len() < 16 {
        return None;
    }
    Some(NavStatus { itow_ms: u32le(p, 0), spoof_det_state: (p[7] >> 3) & 0x3, msss: u32le(p, 12) })
}

/// UBX-NAV-CLOCK: receiver clock bias and drift (UBX-21035062 R03, 3.15.2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavClock {
    /// GPS time of week, ms.
    pub itow_ms: u32,
    /// Clock bias, ns.
    pub bias_ns: i32,
    /// Clock drift, ns/s.
    pub drift_ns_s: i32,
    /// Time accuracy estimate, ns.
    pub t_acc_ns: u32,
    /// Frequency accuracy estimate, ps/s.
    pub f_acc_ps_s: u32,
}

pub(crate) fn decode_nav_clock(p: &[u8]) -> Option<NavClock> {
    if p.len() < 20 {
        return None;
    }
    Some(NavClock {
        itow_ms: u32le(p, 0),
        bias_ns: i32le(p, 4),
        drift_ns_s: i32le(p, 8),
        t_acc_ns: u32le(p, 12),
        f_acc_ps_s: u32le(p, 16),
    })
}

/// One RF block of UBX-MON-RF (UBX-21035062 R03, 3.14.6.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RfBlock {
    /// 0 = L1, 1 = L2 or L5.
    pub block_id: u8,
    /// `flags` bits 1..0, `jammingState` from the interference monitor (CFG-ITFM): 0 unknown
    /// or monitor off, 1 ok, 2 warning (interference, fix still OK), 3 critical
    /// (interference and no fix).
    pub jamming_state: u8,
    /// Antenna supervisor: 0 init, 1 don't know, 2 ok, 3 short, 4 open.
    pub ant_status: u8,
    /// Noise level as measured by the GPS core.
    pub noise_per_ms: u16,
    /// AGC monitor, 0..8191 (8191 = full gain). Falls when strong in-band power arrives.
    pub agc_cnt: u16,
    /// CW jamming indicator: 0 none, 255 strong (always on; relative to the unjammed baseline).
    pub cw_suppression: u8,
}

/// UBX-MON-RF: the first two RF blocks (the M10 has one, L1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonRf {
    /// Blocks; only `..count` are valid.
    pub blocks: [RfBlock; 2],
    /// Number of valid blocks.
    pub count: u8,
}

impl MonRf {
    /// The valid blocks.
    pub fn blocks(&self) -> &[RfBlock] {
        &self.blocks[..usize::from(self.count)]
    }
}

pub(crate) fn decode_mon_rf(p: &[u8]) -> Option<MonRf> {
    if p.len() < 4 || p[0] != 0 {
        return None;
    }
    let n = usize::from(p[1]);
    if p.len() < 4 + 24 * n {
        return None;
    }
    let mut out = MonRf::default();
    for i in 0..n.min(2) {
        let b = &p[4 + 24 * i..28 + 24 * i];
        out.blocks[i] = RfBlock {
            block_id: b[0],
            jamming_state: b[1] & 0x3,
            ant_status: b[2],
            noise_per_ms: u16le(b, 12),
            agc_cnt: u16le(b, 14),
            cw_suppression: b[16],
        };
        out.count += 1;
    }
    Some(out)
}
