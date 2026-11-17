//! Just enough 802.11 to read management frame headers and elements.
//! Every function takes untrusted bytes and returns `None` rather than
//! panicking on anything short or odd.

/// Management subtypes (frame control bits 4-7, type 0).
pub const ASSOC_REQ: u8 = 0;
pub const REASSOC_REQ: u8 = 2;
pub const PROBE_REQ: u8 = 4;
pub const PROBE_RESP: u8 = 5;
pub const BEACON: u8 = 8;
pub const DISASSOC: u8 = 10;
pub const AUTH: u8 = 11;
pub const DEAUTH: u8 = 12;
pub const ACTION: u8 = 13;

/// Element IDs used here.
pub const EID_SSID: u8 = 0;
pub const EID_CSA: u8 = 37;
pub const EID_RSN: u8 = 48;
pub const EID_ECSA: u8 = 60;
pub const EID_VENDOR: u8 = 221;

/// Management subtypes the flood counters handle from the header alone
/// (deauth, disassoc, authentication, (re)association and probe requests).
/// A node can count these in its receive callback and never queue them.
pub const fn header_only(subtype: u8) -> bool {
    matches!(subtype, ASSOC_REQ | REASSOC_REQ | PROBE_REQ | DISASSOC | AUTH | DEAUTH)
}

/// A management frame's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub subtype: u8,
    /// Retry bit: a retransmission of an unacknowledged frame.
    pub retry: bool,
    /// Protected bit: the body is encrypted (802.11w protected management
    /// frames for deauth / disassoc / robust action frames).
    pub protected: bool,
    /// Receiver, transmitter, BSSID.
    pub addr1: [u8; 6],
    pub addr2: [u8; 6],
    pub addr3: [u8; 6],
    /// Offset of the frame body (24, or 28 with an HT Control field).
    pub body_at: usize,
}

/// Parse a management frame header (MPDU without FCS). `None` if the frame
/// is not version 0 / type management or is shorter than a header.
pub fn header(f: &[u8]) -> Option<Header> {
    let fc0 = *f.first()?;
    let fc1 = *f.get(1)?;
    if fc0 & 0x0F != 0 {
        return None;
    }
    // Order bit on a management frame: a 4-byte HT Control field follows.
    let body_at = if fc1 & 0x80 != 0 { 28 } else { 24 };
    if f.len() < body_at {
        return None;
    }
    Some(Header {
        subtype: fc0 >> 4,
        retry: fc1 & 0x08 != 0,
        protected: fc1 & 0x40 != 0,
        addr1: f[4..10].try_into().ok()?,
        addr2: f[10..16].try_into().ok()?,
        addr3: f[16..22].try_into().ok()?,
        body_at,
    })
}

/// Iterate `(id, body)` over an element list; stops at the first element
/// that runs past the end.
pub fn elements(mut ies: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    core::iter::from_fn(move || {
        let [id, len, rest @ ..] = ies else { return None };
        let body = rest.get(..usize::from(*len))?;
        let (id, n) = (*id, usize::from(*len));
        ies = &rest[n..];
        Some((id, body))
    })
}

/// Security of a network, as far as a beacon says. Ordered roughly from
/// weakest; the numbers travel in `ThreatSighting::detail` (evil twin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Security {
    /// No privacy bit, no RSN/WPA element: anyone can join, nothing is
    /// encrypted. The only class the evil-twin check treats as "weaker".
    Open = 0,
    /// Privacy bit without an RSN or WPA element.
    Wep = 1,
    /// RSN or WPA with a pre-shared key (incl. FT-PSK, PSK-SHA256).
    Psk = 2,
    /// WPA3-Personal (SAE, FT-SAE, SAE-EXT-KEY).
    Sae = 3,
    /// 802.1X (WPA2/WPA3-Enterprise).
    Enterprise = 4,
    /// Opportunistic Wireless Encryption, or the open half of an OWE
    /// transition pair (carries the OWE Transition Mode element): open to
    /// join but encrypted, and a normal deployment, so never "open".
    Owe = 5,
    /// RSN with an AKM not listed above.
    Other = 6,
}

impl Security {
    pub const fn protected(self) -> bool {
        !matches!(self, Security::Open)
    }
}

/// Everything the beacon detectors want from one beacon / probe response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BssInfo<'a> {
    pub ssid: &'a [u8],
    pub security: Security,
    /// Channel Switch Announcement (element 37) or Extended CSA (60):
    /// (new channel, switch count).
    pub csa: Option<(u8, u8)>,
    /// Elements 222-226 (Pwnagotchi / pwngrid "whisper" elements): bit n
    /// set for element 222 + n; plus the first payload byte of 222.
    pub whisper: u8,
    pub whisper_first: Option<u8>,
    /// All elements parsed to exactly the end (a sane frame).
    pub clean: bool,
}

/// OUI and type of the Wi-Fi Alliance OWE Transition Mode element.
const OWE_TRANSITION: [u8; 4] = [0x50, 0x6F, 0x9A, 0x1C];
/// Microsoft WPA (v1) element.
const WPA_V1: [u8; 4] = [0x00, 0x50, 0xF2, 0x01];

/// Beacon or probe response: the network's name, security and the
/// elements the detectors care about. `None` for other frames.
pub fn bss_info<'a>(h: &Header, f: &'a [u8]) -> Option<BssInfo<'a>> {
    if h.subtype != BEACON && h.subtype != PROBE_RESP || h.protected {
        return None;
    }
    // Timestamp (8), beacon interval (2), capability information (2).
    let fixed = f.get(h.body_at..h.body_at + 12)?;
    let privacy = fixed[10] & 0x10 != 0;
    let ies = &f[h.body_at + 12..];
    let mut info = BssInfo { ssid: &[], security: Security::Open, csa: None, whisper: 0, whisper_first: None, clean: false };
    let mut rsn: Option<Security> = None;
    let (mut wpa, mut owe_transition) = (false, false);
    let mut consumed = 0usize;
    for (id, body) in elements(ies) {
        consumed += 2 + body.len();
        match id {
            EID_SSID => info.ssid = body,
            EID_RSN => rsn = Some(rsn_akm(body)),
            EID_CSA if body.len() >= 3 => info.csa = Some((body[1], body[2])),
            EID_ECSA if body.len() >= 4 => info.csa = Some((body[2], body[3])),
            EID_VENDOR if body.starts_with(&OWE_TRANSITION) => owe_transition = true,
            EID_VENDOR if body.starts_with(&WPA_V1) => wpa = true,
            222..=226 => {
                info.whisper |= 1 << (id - 222);
                if id == 222 && info.whisper_first.is_none() {
                    info.whisper_first = body.first().copied();
                }
            }
            _ => {}
        }
    }
    info.clean = consumed == ies.len();
    info.security = match (rsn, wpa, privacy, owe_transition) {
        (Some(s), _, _, _) => s,
        (None, true, _, _) => Security::Psk,
        (None, false, _, true) => Security::Owe,
        (None, false, true, false) => Security::Wep,
        (None, false, false, false) => Security::Open,
    };
    Some(info)
}

/// The strongest-sounding AKM in an RSN element (enterprise beats SAE beats
/// PSK; we only need a class). Malformed: `Other`.
fn rsn_akm(rsn: &[u8]) -> Security {
    // Version (2), group cipher (4), pairwise count (2) + 4 each, AKM count (2) + 4 each.
    let Some(pc) = rsn.get(6..8).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]]))) else {
        return Security::Other;
    };
    let akm_at = 8 + 4 * pc;
    let Some(n) = rsn.get(akm_at..akm_at + 2).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]]))) else {
        return Security::Other;
    };
    let mut best: Option<Security> = None;
    for i in 0..n.min(8) {
        let Some(s) = rsn.get(akm_at + 2 + 4 * i..akm_at + 6 + 4 * i) else { break };
        let class = if s[..3] == [0x00, 0x0F, 0xAC] {
            match s[3] {
                1 | 3 | 5 | 11 | 12 | 13 => Security::Enterprise,
                2 | 4 | 6 => Security::Psk,
                8 | 9 | 24 | 25 => Security::Sae,
                18 => Security::Owe,
                _ => Security::Other,
            }
        } else {
            Security::Other
        };
        let rank = |s: Security| match s {
            Security::Enterprise => 4,
            Security::Sae => 3,
            Security::Owe => 2,
            Security::Psk => 1,
            _ => 0,
        };
        if best.is_none_or(|b| rank(class) > rank(b)) {
            best = Some(class);
        }
    }
    best.unwrap_or(Security::Other)
}

/// Channel switch carried by an action frame: Spectrum Management (category
/// 0) Channel Switch Announcement (action 4, CSA element in the body), or
/// Public (category 4) Extended Channel Switch Announcement (action 4,
/// fields inline). `(new channel, count)`.
pub fn action_csa(h: &Header, f: &[u8]) -> Option<(u8, u8)> {
    if h.subtype != ACTION || h.protected {
        return None;
    }
    let body = f.get(h.body_at..)?;
    match (body.first()?, body.get(1)?) {
        (0, 4) => elements(body.get(2..)?).find_map(|(id, b)| (id == EID_CSA && b.len() >= 3).then(|| (b[1], b[2]))),
        // Mode, new operating class, new channel, count.
        (4, 4) => Some((*body.get(4)?, *body.get(5)?)),
        _ => None,
    }
}

/// A channel number a real network could move to.
pub const fn valid_channel(ch: u8) -> bool {
    // 6 GHz channels (1-233, odd) are also legal targets of an ECSA, so
    // anything 1-233 passes; 0 and 234+ never name a channel.
    matches!(ch, 1..=233)
}

#[cfg(test)]
pub(crate) mod build {
    //! Frame builders for tests.
    use std::vec::Vec;

    pub fn mgmt(subtype: u8, addr1: [u8; 6], addr2: [u8; 6], addr3: [u8; 6], body: &[u8]) -> Vec<u8> {
        let mut f = std::vec![subtype << 4, 0, 0, 0];
        f.extend_from_slice(&addr1);
        f.extend_from_slice(&addr2);
        f.extend_from_slice(&addr3);
        f.extend_from_slice(&[0x10, 0x00]);
        f.extend_from_slice(body);
        f
    }

    pub fn ie(id: u8, body: &[u8]) -> Vec<u8> {
        let mut v = std::vec![id, body.len() as u8];
        v.extend_from_slice(body);
        v
    }

    /// RSN with one AKM suite (00-0F-AC:akm), CCMP.
    pub fn rsn(akm: u8) -> Vec<u8> {
        ie(48, &[1, 0, 0, 0x0F, 0xAC, 4, 1, 0, 0, 0x0F, 0xAC, 4, 1, 0, 0, 0x0F, 0xAC, akm, 0, 0])
    }

    /// Beacon (or probe response) from `bssid` with these elements after
    /// the SSID. `privacy` sets the capability bit.
    pub fn beacon_from(subtype: u8, bssid: [u8; 6], ssid: &[u8], privacy: bool, extra: &[u8]) -> Vec<u8> {
        let mut body = std::vec![0u8; 8];
        body.extend_from_slice(&[0x64, 0x00]);
        body.extend_from_slice(&[if privacy { 0x11 } else { 0x01 }, 0x00]);
        body.extend(ie(0, ssid));
        body.extend_from_slice(extra);
        mgmt(subtype, [0xFF; 6], bssid, bssid, &body)
    }

    pub fn beacon(bssid: [u8; 6], ssid: &[u8], privacy: bool, extra: &[u8]) -> Vec<u8> {
        beacon_from(super::BEACON, bssid, ssid, privacy, extra)
    }

    pub fn wpa2(bssid: [u8; 6], ssid: &[u8]) -> Vec<u8> {
        beacon(bssid, ssid, true, &rsn(2))
    }

    pub fn open(bssid: [u8; 6], ssid: &[u8]) -> Vec<u8> {
        beacon(bssid, ssid, false, &[])
    }

    /// Deauth / disassoc with a reason code.
    pub fn kill(subtype: u8, dst: [u8; 6], src: [u8; 6], bssid: [u8; 6], reason: u16) -> Vec<u8> {
        mgmt(subtype, dst, src, bssid, &reason.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    const AP: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];

    #[test]
    fn header_fields() {
        let mut f = kill(DEAUTH, [0xFF; 6], AP, AP, 7);
        let h = header(&f).unwrap();
        assert_eq!((h.subtype, h.retry, h.protected, h.body_at), (DEAUTH, false, false, 24));
        assert_eq!((h.addr1, h.addr2, h.addr3), ([0xFF; 6], AP, AP));
        f[1] = 0x08 | 0x40 | 0x80;
        f.extend_from_slice(&[0; 4]);
        let h = header(&f).unwrap();
        assert!(h.retry && h.protected);
        assert_eq!(h.body_at, 28);
        assert!(header(&f[..23]).is_none());
        // Data frame (type 2).
        f[0] = 0x08;
        assert!(header(&f).is_none());
        assert!(header(&[]).is_none());
    }

    #[test]
    fn element_walk_stops_on_overrun() {
        let ies = [0, 2, b'h', b'i', 37, 3, 0, 11, 5, 48, 9];
        let v: std::vec::Vec<_> = elements(&ies).collect();
        assert_eq!(v, [(0, &b"hi"[..]), (37, &[0, 11, 5][..])]);
    }

    #[test]
    fn security_classes() {
        let s = |f: &[u8]| bss_info(&header(f).unwrap(), f).unwrap().security;
        assert_eq!(s(&open(AP, b"x")), Security::Open);
        assert_eq!(s(&beacon(AP, b"x", true, &[])), Security::Wep);
        assert_eq!(s(&wpa2(AP, b"x")), Security::Psk);
        assert_eq!(s(&beacon(AP, b"x", true, &rsn(8))), Security::Sae);
        assert_eq!(s(&beacon(AP, b"x", true, &rsn(1))), Security::Enterprise);
        assert_eq!(s(&beacon(AP, b"x", true, &rsn(18))), Security::Owe);
        assert_eq!(s(&beacon(AP, b"x", true, &ie(221, &[0x00, 0x50, 0xF2, 0x01, 1, 0]))), Security::Psk);
        // Open half of an OWE transition pair.
        assert_eq!(s(&beacon(AP, b"x", false, &ie(221, &[0x50, 0x6F, 0x9A, 0x1C, 0, 0]))), Security::Owe);
        // Malformed RSN.
        assert_eq!(s(&beacon(AP, b"x", true, &ie(48, &[1, 0]))), Security::Other);
    }

    #[test]
    fn rsn_mixed_akms_takes_strongest() {
        // PSK + SAE (WPA3 transition) -> Sae.
        let body = [1, 0, 0, 0x0F, 0xAC, 4, 1, 0, 0, 0x0F, 0xAC, 4, 2, 0, 0, 0x0F, 0xAC, 2, 0, 0x0F, 0xAC, 8, 0, 0];
        assert_eq!(rsn_akm(&body), Security::Sae);
    }

    #[test]
    fn csa_in_beacon_and_action() {
        let f = beacon(AP, b"x", true, &ie(37, &[1, 44, 5]));
        let i = bss_info(&header(&f).unwrap(), &f).unwrap();
        assert_eq!(i.csa, Some((44, 5)));
        assert!(i.clean);
        let f = beacon(AP, b"x", true, &ie(60, &[1, 115, 36, 3]));
        assert_eq!(bss_info(&header(&f).unwrap(), &f).unwrap().csa, Some((36, 3)));

        let mut body = std::vec![0, 4];
        body.extend(ie(37, &[1, 11, 0]));
        let f = mgmt(ACTION, [0xFF; 6], AP, AP, &body);
        assert_eq!(action_csa(&header(&f).unwrap(), &f), Some((11, 0)));
        let f = mgmt(ACTION, [0xFF; 6], AP, AP, &[4, 4, 1, 81, 6, 2]);
        assert_eq!(action_csa(&header(&f).unwrap(), &f), Some((6, 2)));
        // Some other action (block ack).
        let f = mgmt(ACTION, [0xFF; 6], AP, AP, &[3, 0, 1]);
        assert_eq!(action_csa(&header(&f).unwrap(), &f), None);
    }

    #[test]
    fn whisper_elements() {
        let mut extra = ie(222, b"{\"name\":\"x\"}");
        extra.extend(ie(223, &[1]));
        let f = beacon(AP, b"", false, &extra);
        let i = bss_info(&header(&f).unwrap(), &f).unwrap();
        assert_eq!(i.whisper, 0b11);
        assert_eq!(i.whisper_first, Some(b'{'));
    }

    #[test]
    fn short_or_wrong_frames() {
        let f = kill(DEAUTH, [0xFF; 6], AP, AP, 7);
        assert!(bss_info(&header(&f).unwrap(), &f).is_none());
        let f = mgmt(BEACON, [0xFF; 6], AP, AP, &[0; 5]);
        assert!(bss_info(&header(&f).unwrap(), &f).is_none());
        let f = mgmt(ACTION, [0xFF; 6], AP, AP, &[]);
        assert!(action_csa(&header(&f).unwrap(), &f).is_none());
        assert!(valid_channel(1) && valid_channel(177) && !valid_channel(0) && !valid_channel(250));
    }

    #[test]
    fn never_panics_on_garbage() {
        // Every prefix of a few frames, and random-ish bytes.
        let frames = [wpa2(AP, b"net"), kill(DEAUTH, [0xFF; 6], AP, AP, 3), mgmt(ACTION, AP, AP, AP, &[4, 4, 1])];
        for f in &frames {
            for n in 0..=f.len() {
                if let Some(h) = header(&f[..n]) {
                    let _ = bss_info(&h, &f[..n]);
                    let _ = action_csa(&h, &f[..n]);
                }
            }
        }
        let mut x: u32 = 1;
        for _ in 0..2000 {
            let mut f = [0u8; 64];
            for b in f.iter_mut() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = x as u8;
            }
            f[0] &= 0xF0;
            if let Some(h) = header(&f) {
                let _ = bss_info(&h, &f);
                let _ = action_csa(&h, &f);
            }
        }
    }
}
