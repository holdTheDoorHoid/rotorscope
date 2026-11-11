//! `--sniffle PORT`: Bluetooth 5 Remote ID from a TI CC26x2 dongle running
//! NCC Group's Sniffle firmware (e.g. a Sonoff Zigbee 3.0 USB Dongle Plus
//! flashed with `tools/cc26xx-flash`). Unlike a laptop BT4 adapter it hears
//! legacy adverts, BT5 extended adverts (it follows the AUX_ADV_IND
//! pointer), and Long Range (coded PHY) — and it reports the PHY, channel,
//! PDU type and RSSI of every packet, so the transport label is exact.
//!
//! Sniffle's host protocol, as its Python host speaks it (written from that
//! description; no code shared):
//!
//! - UART 8N1 at 2 Mbaud (the `_1M` firmware builds use 921600).
//! - Each message, both ways, is one line: standard base64 with `=`
//!   padding, then CR LF. Decoded, byte 0 is the line's length in 4-char
//!   base64 words and byte 1 the opcode or message type.
//! - Host to sniffer: `0x10` channel/access address/PHY/CRC init, `0x11`
//!   pause when done, `0x12` minimum RSSI, `0x13` MAC filter (none = off),
//!   `0x15` follow connections, `0x16` follow aux pointers, `0x18` marker
//!   (echoed back, for sync), `0x24` version, `0x26` CRC validation.
//!   Nothing here makes the sniffer transmit: no scan (`0x22`), advertise
//!   or connect command is ever sent. The scan is passive.
//! - Sniffer to host: `0x10` packet, `0x11` debug text, `0x12` marker,
//!   `0x13` state change, `0x14` measurement (`05` = firmware version).
//! - A packet message: timestamp µs (u32 LE), length (u16 LE; bit 14 =
//!   CRC error, bit 15 = direction), connection event (u16), RSSI (i8),
//!   channel | PHY << 6 (PHY 0 = 1M, 1 = 2M, 2 = coded S=8, 3 = coded
//!   S=2), then the link-layer PDU (2-byte header + payload).
//!
//! One radio listens on one primary channel with one PHY at a time, so the
//! mode is a choice ([`Primary`]): 1M primary (legacy + extended), coded
//! primary (Long Range), or alternating between the two.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use rid::Transport;

use crate::ble::rid_line_from_adv;
use crate::input::{Clock, Event};
use crate::parse::{Line, RidLine};

pub const BAUD: u32 = 2_000_000;
/// The advertising access address and CRC init.
pub const ADV_AA: u32 = 0x8E89_BED6;
pub const ADV_CRCI: u32 = 0x55_5555;
/// Remote ID adverts go out on all three primary channels; one is enough.
pub const PRIMARY_CHANNEL: u8 = 37;

pub mod op {
    pub const CHAN_AA_PHY: u8 = 0x10;
    pub const PAUSE_DONE: u8 = 0x11;
    pub const RSSI_MIN: u8 = 0x12;
    pub const MAC_FILTER: u8 = 0x13;
    pub const FOLLOW: u8 = 0x15;
    pub const AUX_ADV: u8 = 0x16;
    pub const MARKER: u8 = 0x18;
    pub const VERSION: u8 = 0x24;
    pub const CRC_VALID: u8 = 0x26;
}

pub mod msg {
    pub const PACKET: u8 = 0x10;
    pub const DEBUG: u8 = 0x11;
    pub const MARKER: u8 = 0x12;
    pub const STATE: u8 = 0x13;
    pub const MEASURE: u8 = 0x14;
}

pub const PHY_1M: u8 = 0;
#[allow(dead_code)] // reported, never requested
pub const PHY_2M: u8 = 1;
pub const PHY_CODED_S8: u8 = 2;
pub const PHY_CODED_S2: u8 = 3;

/// Which primary PHY to listen on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primary {
    /// 1M: legacy adverts and extended adverts (BT4 legacy, BT5 extended).
    OneM,
    /// Coded S=8: Long Range extended adverts only.
    Coded,
    /// Switch between the two, `dwell` on each.
    Alternate { dwell: Duration },
}

impl Primary {
    pub fn parse(s: &str) -> Option<Primary> {
        match s.to_ascii_lowercase().as_str() {
            "1m" | "legacy" | "extended" => Some(Primary::OneM),
            "coded" | "long-range" | "lr" => Some(Primary::Coded),
            "alt" | "alternate" | "both" => Some(Primary::Alternate {
                dwell: Duration::from_secs(3),
            }),
            _ => None,
        }
    }
}

// ------------------------------------------------------------- base64 --

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn b64_decode(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let quads = text.len() / 4;
    for (qi, q) in text.chunks(4).enumerate() {
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && qi + 1 != quads) {
            return None;
        }
        let mut n = 0u32;
        for &c in &q[..4 - pad] {
            n = n << 6 | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

// ------------------------------------------------------------ commands --

/// One command line: `[word count][op][args]`, base64, CR LF.
pub fn command(op: u8, args: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(args.len() + 2);
    raw.push(((args.len() + 1 + 3) / 3) as u8);
    raw.push(op);
    raw.extend_from_slice(args);
    let mut line = b64_encode(&raw).into_bytes();
    line.extend_from_slice(b"\r\n");
    line
}

/// Listen on the advertising access address on `chan` with `phy`.
pub fn chan_aa_phy(chan: u8, phy: u8) -> Vec<u8> {
    let mut a = vec![chan];
    a.extend_from_slice(&ADV_AA.to_le_bytes());
    a.push(phy);
    a.extend_from_slice(&ADV_CRCI.to_le_bytes());
    command(op::CHAN_AA_PHY, &a)
}

/// Passive advertising capture: no RSSI floor, never pause, never follow a
/// connection, follow aux pointers (extended adverts), no MAC filter, drop
/// bad-CRC packets. The sequence Sniffle's own host sends for a passive
/// scan, minus the connection-sniffing preloads.
pub fn passive_setup(phy: u8) -> Vec<Vec<u8>> {
    vec![
        chan_aa_phy(PRIMARY_CHANNEL, phy),
        command(op::RSSI_MIN, &[(-128i8) as u8]),
        command(op::PAUSE_DONE, &[0]),
        command(op::FOLLOW, &[0]),
        command(op::AUX_ADV, &[1]),
        command(op::MAC_FILTER, &[]),
        command(op::CRC_VALID, &[1]),
    ]
}

// ------------------------------------------------------------ messages --

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub ts_us: u32,
    pub crc_err: bool,
    pub rssi: i8,
    pub chan: u8,
    pub phy: u8,
    /// The PDU: 2-byte header, then payload.
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Packet(Packet),
    Debug(String),
    Marker(Vec<u8>),
    State(u8),
    Version {
        major: u8,
        minor: u8,
        rev: u8,
        api: u8,
    },
    Measurement(Vec<u8>),
    Unknown(u8),
}

/// One line from the sniffer, without its CR LF.
pub fn decode_line(line: &[u8]) -> Result<Msg, String> {
    let data = b64_decode(line).ok_or("not base64")?;
    if data.len() < 2 {
        return Err("too short".into());
    }
    if usize::from(data[0]) * 4 != line.len() {
        return Err(format!("word count {} but {} chars", data[0], line.len()));
    }
    let body = &data[2..];
    Ok(match data[1] {
        msg::PACKET => {
            if body.len() < 10 {
                return Err("packet header short".into());
            }
            let len_dir = u16::from_le_bytes([body[4], body[5]]);
            let len = usize::from(len_dir & 0x3FFF);
            let pdu = &body[10..];
            if pdu.len() != len {
                return Err(format!("packet length {len} but {} bytes", pdu.len()));
            }
            Msg::Packet(Packet {
                ts_us: u32::from_le_bytes(body[..4].try_into().unwrap()),
                crc_err: len_dir & 0x4000 != 0,
                rssi: body[8] as i8,
                chan: body[9] & 0x3F,
                phy: body[9] >> 6,
                body: pdu.to_vec(),
            })
        }
        msg::DEBUG => Msg::Debug(body.iter().map(|&b| b as char).collect()),
        msg::MARKER => Msg::Marker(body.get(4..).unwrap_or_default().to_vec()),
        msg::STATE => Msg::State(*body.first().ok_or("empty state")?),
        msg::MEASURE => match body {
            [5, 5, major, minor, rev, api] => Msg::Version {
                major: *major,
                minor: *minor,
                rev: *rev,
                api: *api,
            },
            _ => Msg::Measurement(body.to_vec()),
        },
        t => Msg::Unknown(t),
    })
}

// ------------------------------------------------------------- adverts --

/// Legacy advertising PDU types that carry AdvA + AdvData.
const ADV_IND: u8 = 0;
const ADV_NONCONN_IND: u8 = 2;
const SCAN_RSP: u8 = 4;
const ADV_SCAN_IND: u8 = 6;
/// ADV_EXT_IND on a primary channel; AUX_ADV_IND / AUX_CHAIN_IND /
/// AUX_SCAN_RSP on a secondary one.
const ADV_EXT: u8 = 7;

/// What one advertising packet carries, before Remote ID parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advert<'a> {
    pub transport: Transport,
    /// Address as printed (most significant byte first), if the PDU has it.
    pub adv_a: Option<[u8; 6]>,
    /// Extended adverts: AdvDataInfo (DID | SID << 12), if present.
    pub adi: Option<u16>,
    pub data: &'a [u8],
}

fn mac(le: &[u8]) -> [u8; 6] {
    let mut m: [u8; 6] = le[..6].try_into().unwrap();
    m.reverse();
    m
}

/// Split an advertising-channel PDU. `coded_primary` says the sniffer is
/// listening on a coded primary channel, i.e. Long Range.
pub fn advert(p: &Packet, coded_primary: bool) -> Option<Advert<'_>> {
    let b = &p.body;
    let len = usize::from(*b.get(1)?);
    let payload = b.get(2..2 + len)?;
    let pdu_type = b[0] & 0x0F;
    let primary = p.chan >= 37;
    if primary && pdu_type != ADV_EXT {
        if p.phy != PHY_1M
            || !matches!(
                pdu_type,
                ADV_IND | ADV_NONCONN_IND | ADV_SCAN_IND | SCAN_RSP
            )
        {
            return None;
        }
        return Some(Advert {
            transport: Transport::BleLegacy,
            adv_a: Some(mac(payload.get(..6)?)),
            adi: None,
            data: payload.get(6..)?,
        });
    }
    if pdu_type != ADV_EXT {
        return None;
    }
    // Common extended advertising payload: ext header length (6 bits) and
    // AdvMode, then the ext header (flags byte + fields), then AdvData.
    let hdr_len = usize::from(*payload.first()? & 0x3F);
    let hdr = payload.get(1..1 + hdr_len)?;
    let data = payload.get(1 + hdr_len..)?;
    let (mut adv_a, mut adi) = (None, None);
    if let Some((&flags, fields)) = hdr.split_first() {
        let mut at = 0;
        if flags & 0x01 != 0 {
            adv_a = Some(mac(fields.get(at..at + 6)?));
            at += 6;
        }
        if flags & 0x02 != 0 {
            at += 6; // TargetA
        }
        if flags & 0x04 != 0 {
            at += 1; // CTEInfo
        }
        if flags & 0x08 != 0 {
            let d = fields.get(at..at + 2)?;
            adi = Some(u16::from_le_bytes([d[0], d[1]]));
        }
    }
    let long_range = coded_primary || matches!(p.phy, PHY_CODED_S8 | PHY_CODED_S2);
    Some(Advert {
        transport: if long_range {
            Transport::BleLongRange
        } else {
            Transport::BleExtended
        },
        adv_a,
        adi,
        data,
    })
}

/// Turns Sniffle packets into Remote ID lines. Remembers the AdvA each
/// ADV_EXT_IND announced, by AdvDataInfo, for AUX_ADV_INDs that leave it
/// out.
#[derive(Default)]
pub struct Decoder {
    adv_a_by_adi: HashMap<u16, [u8; 6]>,
}

impl Decoder {
    pub fn rid(&mut self, p: &Packet, coded_primary: bool) -> Option<RidLine> {
        if p.crc_err {
            return None;
        }
        let a = advert(p, coded_primary)?;
        if let (Some(adi), Some(m)) = (a.adi, a.adv_a) {
            if self.adv_a_by_adi.len() > 256 {
                self.adv_a_by_adi.clear();
            }
            self.adv_a_by_adi.insert(adi, m);
        }
        let mac = a
            .adv_a
            .or_else(|| self.adv_a_by_adi.get(&a.adi?).copied())
            .unwrap_or_default();
        rid_line_from_adv(a.transport, p.chan, mac, i16::from(p.rssi), a.data)
    }
}

// ------------------------------------------------------------------ io --

fn open_port(path: &str) -> io::Result<File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(path)?;
    let fd = f.as_raw_fd();
    // SAFETY: termios calls on a descriptor we own; `t` is filled by
    // tcgetattr before use.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        t.c_cflag &= !libc::CRTSCTS;
        // Return what has arrived within 0.1 s, so the loop can keep time.
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 1;
        libc::cfsetispeed(&mut t, libc::B2000000);
        libc::cfsetospeed(&mut t, libc::B2000000);
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            return Err(io::Error::last_os_error());
        }
        libc::tcflush(fd, libc::TCIOFLUSH);
    }
    Ok(f)
}

/// Splits the byte stream into lines (CR LF stripped).
#[derive(Default)]
pub struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    pub fn feed(&mut self, data: &[u8], mut each: impl FnMut(&[u8])) {
        self.buf.extend_from_slice(data);
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            each(line);
        }
        // A sniffer that never sends a newline is not Sniffle.
        if self.buf.len() > 4096 {
            self.buf.clear();
        }
    }
}

fn phy_name(coded: bool) -> &'static str {
    if coded { "coded" } else { "1M" }
}

/// Configure the sniffer and wait for it to echo a marker. Returns the
/// firmware version if it reported one.
fn handshake(port: &mut File, coded: bool) -> Result<Option<String>, String> {
    let mut out = b"\r\n".to_vec(); // end any half-written line
    out.extend(command(op::MARKER, b"@"));
    for c in passive_setup(if coded { PHY_CODED_S8 } else { PHY_1M }) {
        out.extend(c);
    }
    out.extend(command(op::VERSION, &[]));
    let mark = (std::process::id() ^ 0x5249_4456).to_le_bytes();
    out.extend(command(op::MARKER, &mark));
    port.write_all(&out).map_err(|e| e.to_string())?;

    let began = Instant::now();
    let mut lines = Lines::default();
    let (mut version, mut synced, mut any) = (None, false, false);
    let mut buf = [0u8; 4096];
    while !synced && began.elapsed() < Duration::from_secs(3) {
        let n = port.read(&mut buf).map_err(|e| e.to_string())?;
        lines.feed(&buf[..n], |l| match decode_line(l) {
            Ok(Msg::Version {
                major,
                minor,
                rev,
                api,
            }) => {
                any = true;
                version = Some(format!("{major}.{minor}.{rev} (API {api})"));
            }
            Ok(Msg::Marker(d)) => {
                any = true;
                synced |= d == mark;
            }
            Ok(_) => any = true,
            Err(_) => {}
        });
    }
    if !synced {
        return Err(if any {
            "Sniffle answered but never echoed the sync marker".into()
        } else {
            format!(
                "no Sniffle messages at {BAUD} baud. Is the dongle flashed with Sniffle \
                 (tools/cc26xx-flash)? A `_1M` firmware build talks at 921600 instead"
            )
        });
    }
    Ok(version)
}

pub fn spawn(
    node: u8,
    path: &str,
    mode: Primary,
    stop_after: Option<Duration>,
    start: Instant,
    tx: Sender<Event>,
) -> Result<String, String> {
    let mut port = open_port(path).map_err(|e| format!("{path}: {e}"))?;
    let mut coded = mode == Primary::Coded;
    let version = handshake(&mut port, coded)?;
    let ver = version.clone().unwrap_or_else(|| "unknown version".into());
    let hello = serde_json::json!({
        "t": "hello", "node": "ble", "via": "sniffle", "fw": ver,
        "scan": match mode {
            Primary::OneM => "passive, 1M primary ch 37 (legacy + extended)".to_string(),
            Primary::Coded => "passive, coded primary ch 37 (Long Range)".to_string(),
            Primary::Alternate { dwell } =>
                format!("passive, ch 37, 1M and coded primary alternating every {} s", dwell.as_secs()),
        },
    });
    let source = format!("Sniffle {ver} {path}");
    std::thread::spawn(move || {
        let clock = Clock(start);
        let began = Instant::now();
        let send = |line: Line| {
            tx.send(Event::Line {
                node,
                line,
                at_ms: clock.ms(),
            })
            .is_ok()
        };
        if !send(Line::Hello(hello)) {
            return;
        }
        let mut dec = Decoder::default();
        let mut lines = Lines::default();
        let (mut adv, mut rid, mut tot_adv, mut tot_rid, mut bad) = (0u64, 0u64, 0u64, 0u64, 0u64);
        let mut by_transport: HashMap<&'static str, u64> = HashMap::new();
        let mut window = Instant::now();
        let mut switched = Instant::now();
        let mut buf = [0u8; 8192];
        let why = loop {
            if stop_after.is_some_and(|d| began.elapsed() >= d) {
                break "scan time over".to_string();
            }
            if let Primary::Alternate { dwell } = mode
                && switched.elapsed() >= dwell
            {
                coded = !coded;
                let phy = if coded { PHY_CODED_S8 } else { PHY_1M };
                if let Err(e) = port.write_all(&chan_aa_phy(PRIMARY_CHANNEL, phy)) {
                    break e.to_string();
                }
                switched = Instant::now();
            }
            let n = match port.read(&mut buf) {
                Ok(n) => n,
                Err(e) => break e.to_string(),
            };
            let mut out: Vec<RidLine> = Vec::new();
            lines.feed(&buf[..n], |l| match decode_line(l) {
                Ok(Msg::Packet(p)) => {
                    adv += 1;
                    if let Some(r) = dec.rid(&p, coded) {
                        *by_transport.entry(transport_name(r.transport)).or_default() += 1;
                        out.push(r);
                    }
                }
                Ok(_) => {}
                Err(_) => bad += 1,
            });
            for r in out {
                rid += 1;
                if !send(Line::Rid(r)) {
                    return;
                }
            }
            let w = window.elapsed();
            if w >= Duration::from_secs(1) {
                tot_adv += adv;
                tot_rid += rid;
                let stats = serde_json::json!({
                    "t": "stats", "adv": adv, "rid": rid, "win_ms": w.as_millis() as u64,
                    "phy": phy_name(coded), "tot_adv": tot_adv, "tot_rid": tot_rid,
                    "rid_by": by_transport, "bad_lines": bad,
                });
                (adv, rid) = (0, 0);
                window = Instant::now();
                if !send(Line::Stats(stats)) {
                    return;
                }
            }
        };
        let _ = tx.send(Event::End { node, why });
    });
    Ok(source)
}

fn transport_name(t: Transport) -> &'static str {
    match t {
        Transport::BleLegacy => "legacy",
        Transport::BleExtended => "extended",
        Transport::BleLongRange => "long_range",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rid::message::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..20 {
            let d: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(b64_decode(b64_encode(&d).as_bytes()).unwrap(), d);
        }
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert_eq!(b64_decode(b"TWE="), Some(b"Ma".to_vec()));
        assert_eq!(b64_decode(b"TW=="), Some(b"M".to_vec()));
        assert!(b64_decode(b"TWE").is_none());
        assert!(b64_decode(b"TW=uTWFu").is_none(), "padding mid-stream");
        assert!(b64_decode(b"T!Fu").is_none());
    }

    #[test]
    fn command_framing() {
        // VERSION: [1, 0x24] -> word count (1 + 1 + 3) / 3 = 1.
        assert_eq!(command(op::VERSION, &[]), b"ASQ=\r\n");
        // MARKER '@': [1, 0x18, 0x40].
        assert_eq!(command(op::MARKER, b"@"), b"ARhA\r\n");
        // CHAN_AA_PHY: 11 bytes after the count byte -> 4 words, and the
        // firmware checks the decoded length is exactly 12.
        let l = chan_aa_phy(37, PHY_CODED_S8);
        let raw = b64_decode(&l[..l.len() - 2]).unwrap();
        assert_eq!(raw.len(), 12);
        assert_eq!(raw[0], 4);
        assert_eq!(
            &raw[1..],
            &[0x10, 37, 0xD6, 0xBE, 0x89, 0x8E, 2, 0x55, 0x55, 0x55, 0x00]
        );
        // MAC filter off is the bare opcode (decoded length 2).
        let l = command(op::MAC_FILTER, &[]);
        assert_eq!(b64_decode(&l[..l.len() - 2]).unwrap(), vec![1, 0x13]);
        // Nothing in the setup asks the radio to transmit.
        for c in passive_setup(PHY_1M) {
            let raw = b64_decode(&c[..c.len() - 2]).unwrap();
            assert!(![0x19, 0x1A, 0x1C, 0x22, 0x25].contains(&raw[1]));
        }
    }

    /// A sniffer->host line, as the firmware frames it.
    pub fn line(mtype: u8, body: &[u8]) -> Vec<u8> {
        let mut raw = vec![0, mtype];
        raw.extend_from_slice(body);
        raw[0] = raw.len().div_ceil(3) as u8;
        b64_encode(&raw).into_bytes()
    }

    fn packet_line(rssi: i8, chan: u8, phy: u8, crc_err: bool, pdu: &[u8]) -> Vec<u8> {
        let mut b = 123_456u32.to_le_bytes().to_vec();
        let len = pdu.len() as u16 | if crc_err { 0x4000 } else { 0 };
        b.extend_from_slice(&len.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.push(rssi as u8);
        b.push(chan | phy << 6);
        b.extend_from_slice(pdu);
        line(msg::PACKET, &b)
    }

    fn basic() -> rid::RawMessage {
        rid::encode::message(&Message::BasicId(BasicId {
            id_type: IdType::SerialNumber,
            ua_type: UaType::HelicopterOrMultirotor,
            uas_id: FixedStr::new(b"TESTDRONE0000000001").unwrap(),
        }))
        .unwrap()
    }

    fn rid_ad(counter: u8, payload: &[u8]) -> Vec<u8> {
        let mut ad = [0u8; 256];
        let n = rid::encode::ble_service_data(counter, payload, &mut ad).unwrap();
        ad[..n].to_vec()
    }

    const ADDR_LE: [u8; 6] = [0x53, 0x00, 0x44, 0x49, 0x52, 0xC2];
    const ADDR: [u8; 6] = [0xC2, 0x52, 0x49, 0x44, 0x00, 0x53];

    /// ADV_NONCONN_IND, random address, as a BT4 Remote ID broadcaster.
    fn legacy_pdu() -> Vec<u8> {
        let ad = rid_ad(7, &basic());
        let mut p = vec![0x40 | ADV_NONCONN_IND, (6 + ad.len()) as u8];
        p.extend_from_slice(&ADDR_LE);
        p.extend_from_slice(&ad);
        p
    }

    /// AUX_ADV_IND carrying a Message Pack: ext header with AdvA and ADI.
    fn aux_pdu(with_adv_a: bool) -> Vec<u8> {
        let raws = [basic(), basic()];
        let mut pack = [0u8; 64];
        let n = rid::encode::pack(&raws, &mut pack).unwrap();
        let ad = rid_ad(9, &pack[..n]);
        let mut hdr = vec![if with_adv_a { 0x09 } else { 0x08 }];
        if with_adv_a {
            hdr.extend_from_slice(&ADDR_LE);
        }
        hdr.extend_from_slice(&[0x34, 0x12]); // ADI
        let mut payload = vec![hdr.len() as u8];
        payload.extend(hdr);
        payload.extend(ad);
        let mut p = vec![0x40 | ADV_EXT, payload.len() as u8];
        p.extend(payload);
        p
    }

    fn packet(l: &[u8]) -> Packet {
        match decode_line(l).unwrap() {
            Msg::Packet(p) => p,
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn legacy_advert() {
        let p = packet(&packet_line(-61, 37, PHY_1M, false, &legacy_pdu()));
        assert_eq!((p.rssi, p.chan, p.phy, p.ts_us), (-61, 37, 0, 123_456));
        let r = Decoder::default().rid(&p, false).unwrap();
        assert_eq!(r.transport, Transport::BleLegacy);
        assert_eq!((r.mac, r.rssi, r.counter, r.channel), (ADDR, -61, 7, 37));
        assert_eq!(r.msgs.len(), 1);
        assert_eq!(r.errors, 0);
    }

    #[test]
    fn extended_advert_on_1m_and_2m() {
        for phy in [PHY_1M, PHY_2M] {
            let p = packet(&packet_line(-70, 12, phy, false, &aux_pdu(true)));
            let r = Decoder::default().rid(&p, false).unwrap();
            assert_eq!(r.transport, Transport::BleExtended);
            assert_eq!(r.mac, ADDR);
            assert_eq!(r.msgs.len(), 2);
            assert_eq!(r.channel, 12);
        }
    }

    #[test]
    fn long_range_by_phy_or_mode() {
        for phy in [PHY_CODED_S8, PHY_CODED_S2] {
            let p = packet(&packet_line(-90, 20, phy, false, &aux_pdu(true)));
            let r = Decoder::default().rid(&p, false).unwrap();
            assert_eq!(r.transport, Transport::BleLongRange);
        }
        // Coded primary: Long Range even if the aux packet is on 1M.
        let p = packet(&packet_line(-90, 20, PHY_1M, false, &aux_pdu(true)));
        assert_eq!(
            Decoder::default().rid(&p, true).unwrap().transport,
            Transport::BleLongRange
        );
    }

    #[test]
    fn aux_without_adv_a_borrows_it_from_adv_ext_ind() {
        let mut d = Decoder::default();
        // ADV_EXT_IND on ch 37: AdvA + ADI + AuxPtr, no data.
        let mut payload = vec![0x0C, 0x19];
        payload.extend_from_slice(&ADDR_LE);
        payload.extend_from_slice(&[0x34, 0x12, 12, 0x10, 0x00]);
        let mut pdu = vec![ADV_EXT, payload.len() as u8];
        pdu.extend(payload);
        let p = packet(&packet_line(-50, 37, PHY_1M, false, &pdu));
        assert!(d.rid(&p, false).is_none(), "no data on the primary");
        let p = packet(&packet_line(-50, 12, PHY_1M, false, &aux_pdu(false)));
        assert_eq!(d.rid(&p, false).unwrap().mac, ADDR);
        // Unknown ADI: no address.
        let p = packet(&packet_line(-50, 12, PHY_1M, false, &aux_pdu(false)));
        assert_eq!(Decoder::default().rid(&p, false).unwrap().mac, [0; 6]);
    }

    #[test]
    fn ignored_packets() {
        // Bad CRC.
        let p = packet(&packet_line(-50, 37, PHY_1M, true, &legacy_pdu()));
        assert!(Decoder::default().rid(&p, false).is_none());
        // SCAN_REQ (type 3) and a legacy PDU type on a data channel.
        let mut pdu = legacy_pdu();
        pdu[0] = 0x40 | 3;
        let p = packet(&packet_line(-50, 37, PHY_1M, false, &pdu));
        assert!(Decoder::default().rid(&p, false).is_none());
        let p = packet(&packet_line(-50, 5, PHY_1M, false, &legacy_pdu()));
        assert!(Decoder::default().rid(&p, false).is_none());
        // Not Remote ID: Flags only.
        let p = packet(&packet_line(
            -50,
            37,
            PHY_1M,
            false,
            &[0x42, 9, 1, 2, 3, 4, 5, 6, 2, 1, 6],
        ));
        assert!(Decoder::default().rid(&p, false).is_none());
        // Truncated PDU.
        let mut pdu = legacy_pdu();
        pdu[1] = 200;
        let p = packet(&packet_line(-50, 37, PHY_1M, false, &pdu));
        assert!(Decoder::default().rid(&p, false).is_none());
    }

    #[test]
    fn other_messages() {
        assert_eq!(
            decode_line(&line(msg::MEASURE, &[5, 5, 1, 11, 0, 3])).unwrap(),
            Msg::Version {
                major: 1,
                minor: 11,
                rev: 0,
                api: 3
            }
        );
        assert_eq!(
            decode_line(&line(msg::MARKER, &[1, 2, 3, 4, b'@'])).unwrap(),
            Msg::Marker(b"@".to_vec())
        );
        assert_eq!(decode_line(&line(msg::STATE, &[1])).unwrap(), Msg::State(1));
        assert_eq!(
            decode_line(&line(msg::DEBUG, b"hi")).unwrap(),
            Msg::Debug("hi".into())
        );
        // Length field disagreeing with the line.
        let mut l = line(msg::STATE, &[1]);
        l.extend_from_slice(b"AAAA");
        assert!(decode_line(&l).is_err());
        // Packet length field disagreeing with the body.
        let mut b = vec![0u8; 10];
        b[4] = 5;
        assert!(decode_line(&line(msg::PACKET, &b)).is_err());
    }

    #[test]
    fn line_splitter() {
        let mut ls = Lines::default();
        let mut got = Vec::new();
        ls.feed(b"ASQ=\r\nAR", |l| got.push(l.to_vec()));
        ls.feed(b"hA\r\n\r\n", |l| got.push(l.to_vec()));
        assert_eq!(got, vec![b"ASQ=".to_vec(), b"ARhA".to_vec(), Vec::new()]);
    }

    #[test]
    fn modes() {
        assert_eq!(Primary::parse("1M"), Some(Primary::OneM));
        assert_eq!(Primary::parse("coded"), Some(Primary::Coded));
        assert!(matches!(
            Primary::parse("alt"),
            Some(Primary::Alternate { .. })
        ));
        assert_eq!(Primary::parse("2m"), None);
    }
}
