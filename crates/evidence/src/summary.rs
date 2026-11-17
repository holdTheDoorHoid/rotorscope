//! The detection record's raw bytes and decode summary.
//!
//! The raw bytes are one `link::Frame` serialized with postcard (link
//! protocol 6). Postcard's encoding of these types is canonical, so for a
//! frame that decoded cleanly this is byte-for-byte the payload the node
//! sent (the COBS framing and the zero delimiter removed). For Remote ID the
//! frame carries the 25-byte ASTM F3411 messages exactly as the node took
//! them out of the Wi-Fi or Bluetooth frame.
//!
//! The summary is a short line of text the hub writes next to the raw bytes
//! (what *it* made of them). The verifier recomputes it from the raw bytes
//! and reports any difference. Both use this code, so this is a check that
//! the summary matches the bytes, not an independent second decoder (that is
//! future work; see docs/EVIDENCE.md).
//!
//! Open FPV link identifiers stay out of the summary (project policy, as on
//! every other hub output); they are inside the raw bytes.

use core::fmt::Write;

use link::Frame;

use crate::format::{band, frame_kind, MAX_SUMMARY, RAW_LINK_FRAME_V6};

/// The detection fields that come from the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub frame_kind: u8,
    pub band: u8,
    pub channel: u16,
    pub freq_100khz: u32,
    pub rssi: Option<i8>,
}

fn wifi_band(channel: u8) -> u8 {
    match channel {
        0 => band::UNKNOWN,
        1..=14 => band::WIFI_24,
        _ => band::WIFI_5,
    }
}

fn rf_band(freq_100khz: u32) -> u8 {
    if freq_100khz == 0 { band::UNKNOWN } else { band::RF }
}

/// Band, channel and signal strength of a data frame; `None` for link
/// bookkeeping frames (Hello, Stats, Via, Status, hub-to-web-head frames).
pub fn info(f: &Frame) -> Option<FrameInfo> {
    Some(match f {
        Frame::Detection(d) => {
            let ble = matches!(d.transport, rid::Transport::BleLegacy | rid::Transport::BleExtended | rid::Transport::BleLongRange);
            FrameInfo {
                frame_kind: frame_kind::REMOTE_ID,
                band: if ble { band::BLE } else { wifi_band(d.channel) },
                channel: d.channel.into(),
                freq_100khz: 0,
                rssi: Some(d.rssi),
            }
        }
        Frame::OpenLink(o) => FrameInfo {
            frame_kind: frame_kind::OPEN_LINK,
            band: wifi_band(o.channel),
            channel: o.channel.into(),
            freq_100khz: 0,
            rssi: Some(o.rssi),
        },
        Frame::DroneId(s) => FrameInfo {
            frame_kind: frame_kind::DJI_DRONEID,
            band: rf_band(s.freq),
            channel: 0,
            freq_100khz: s.freq,
            rssi: Some(s.rssi),
        },
        Frame::Rf(r) => FrameInfo { frame_kind: frame_kind::RF, band: rf_band(r.freq), channel: 0, freq_100khz: r.freq, rssi: None },
        Frame::DjiWifi { channel, rssi, .. } => FrameInfo {
            frame_kind: frame_kind::DJI_WIFI,
            band: wifi_band(*channel),
            channel: (*channel).into(),
            freq_100khz: 0,
            rssi: Some(*rssi),
        },
        Frame::WifiDrone(w) => FrameInfo {
            frame_kind: frame_kind::WIFI_DRONE,
            band: wifi_band(w.channel),
            channel: w.channel.into(),
            freq_100khz: 0,
            rssi: Some(w.rssi),
        },
        // Protocol 11: an aero node's raw aircraft frame. Frequency in
        // 100 kHz units, as the other RF kinds.
        Frame::Air(a) => FrameInfo {
            frame_kind: frame_kind::AIRCRAFT,
            band: rf_band(a.freq_khz / 100),
            channel: 0,
            freq_100khz: a.freq_khz / 100,
            rssi: Some(a.rssi),
        },
        // Protocol 12: a control link. Frequency in 100 kHz units.
        Frame::Control(c) => FrameInfo {
            frame_kind: frame_kind::CONTROL,
            band: rf_band(c.sighting.freq_hz / 100_000),
            channel: 0,
            freq_100khz: c.sighting.freq_hz / 100_000,
            rssi: Some(c.sighting.rssi_dbm.clamp(-128, 127) as i8),
        },
        // Protocol 12: a control band's energy window, at the band's start.
        Frame::ControlEnergy(e) => {
            let f = link::ctrlwatch::scan::band(e.region).0 / 100_000;
            FrameInfo { frame_kind: frame_kind::CONTROL_ENERGY, band: rf_band(f), channel: 0, freq_100khz: f, rssi: None }
        }
        _ => return None,
    })
}

/// A `fmt::Write` that keeps the first `N` bytes and drops the rest.
pub struct Cut<const N: usize> {
    pub buf: [u8; N],
    pub len: usize,
}

impl<const N: usize> Cut<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl<const N: usize> Default for Cut<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for Cut<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = N - self.len;
        let mut take = s.len().min(room);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

pub type Summary = Cut<MAX_SUMMARY>;

fn mac(w: &mut impl Write, m: &[u8; 6]) -> core::fmt::Result {
    write!(w, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

fn transport_name(t: rid::Transport) -> &'static str {
    match t {
        rid::Transport::WifiBeacon => "wifi_beacon",
        rid::Transport::WifiNan => "wifi_nan",
        rid::Transport::BleLegacy => "ble4",
        rid::Transport::BleExtended => "ble5",
        rid::Transport::BleLongRange => "ble_long_range",
    }
}

/// The summary line for a data frame.
pub fn write_summary(f: &Frame, w: &mut impl Write) -> core::fmt::Result {
    use rid::message::Message;
    match f {
        Frame::Detection(d) => {
            write!(w, "remote_id {} ch{} rssi{} tx=", transport_name(d.transport), d.channel, d.rssi)?;
            mac(w, &d.mac)?;
            write!(w, " ctr={}", d.counter)?;
            for (i, raw) in d.messages.iter().enumerate() {
                match rid::message::decode(raw) {
                    Ok(Message::BasicId(b)) => match b.uas_id.as_str() {
                        Some(s) => write!(w, " id={s}")?,
                        None => write!(w, " id=(binary)")?,
                    },
                    Ok(Message::Location(l)) => {
                        match l.position {
                            Some(p) => write!(w, " pos={:.7},{:.7}", p.lat, p.lon)?,
                            None => write!(w, " pos=none")?,
                        }
                        if let Some(a) = l.altitude_geodetic {
                            write!(w, " alt_hae={a:.1}")?;
                        }
                        if let Some(h) = l.height {
                            write!(w, " h={h:.1}")?;
                        }
                    }
                    Ok(Message::System(s)) => match s.operator_position {
                        Some(p) => write!(w, " op={:.7},{:.7}", p.lat, p.lon)?,
                        None => write!(w, " op=none")?,
                    },
                    Ok(Message::OperatorId(o)) => match o.operator_id.as_str() {
                        Some(s) => write!(w, " operator={s}")?,
                        None => write!(w, " operator=(binary)")?,
                    },
                    Ok(Message::SelfId(_)) => write!(w, " self_id")?,
                    Ok(Message::Auth(_)) => write!(w, " auth")?,
                    Err(e) => write!(w, " msg{i}=error:{e:?}")?,
                }
            }
            Ok(())
        }
        Frame::OpenLink(o) => {
            write!(w, "open_link {:?} ch{} rssi{} port={} dir={:?} tx=", o.link.system, o.channel, o.rssi, o.link.port, o.link.direction)?;
            mac(w, &o.link.tx)?;
            write!(w, " packets={}", o.packets)
        }
        Frame::DroneId(s) => {
            write!(w, "dji_droneid f={}.{}MHz rssi{} snr{}", s.freq / 10, s.freq % 10, s.rssi, s.snr_db)?;
            match &s.record {
                link::DroneIdRecord::Flight { serial, lat_raw, lon_raw, pilot_lat_raw, pilot_lon_raw, .. } => {
                    let n = serial.iter().position(|&b| b == 0).unwrap_or(serial.len());
                    match core::str::from_utf8(&serial[..n]) {
                        Ok(s) => write!(w, " serial={s}")?,
                        Err(_) => write!(w, " serial=(binary)")?,
                    }
                    write!(w, " lat_raw={lat_raw} lon_raw={lon_raw} pilot_raw={pilot_lat_raw},{pilot_lon_raw}")
                }
                link::DroneIdRecord::Encrypted { key_id } => write!(w, " encrypted key_id={:02x}{:02x}{:02x}{:02x}", key_id[0], key_id[1], key_id[2], key_id[3]),
                link::DroneIdRecord::Detected => write!(w, " detected"),
                // The raw object stays in the evidence record (the frame
                // itself); the summary names it only.
                link::DroneIdRecord::O4 { kind, key_id, object } => write!(
                    w,
                    " o4 {} key_id={:02x}{:02x}{:02x}{:02x} object={}B",
                    kind.name(),
                    key_id[0],
                    key_id[1],
                    key_id[2],
                    key_id[3],
                    object.len()
                ),
            }
        }
        Frame::Rf(r) => write!(
            w,
            "rf {:?} f={}.{}MHz width={}kHz snr{} duty={}% line={}Hz",
            r.kind,
            r.freq / 10,
            r.freq % 10,
            r.width_khz,
            r.snr_db,
            r.duty_pct,
            r.line_hz
        ),
        Frame::DjiWifi { channel, rssi, mac: m, element, .. } => {
            write!(w, "dji_wifi ch{channel} rssi{rssi} tx=")?;
            mac(w, m)?;
            write!(w, " element={}B", element.len())
        }
        Frame::WifiDrone(d) => {
            write!(w, "wifi_drone maker={} ch{} rssi{} ssid={:?} tx=", d.maker, d.channel, d.rssi, d.ssid.as_str())?;
            mac(w, &d.mac)
        }
        // The raw radio bytes are in the record (the frame itself); the
        // summary names the protocol, so a reader knows how to decode them.
        Frame::Air(a) => {
            let proto = match a.proto {
                link::AirProto::Es1090 => "es1090",
                link::AirProto::Uat978 => "uat978",
                link::AirProto::Flarm => "flarm",
                link::AirProto::Ogn => "ogn",
            };
            write!(w, "aircraft {proto} f={}.{:03}MHz rssi{} raw={}B", a.freq_khz / 1000, a.freq_khz % 1000, a.rssi, a.raw.len())?;
            match a.utc_ms {
                Some(t) => write!(w, " node_utc_ms={t}"),
                None => write!(w, " node_utc=none"),
            }
        }
        // Protocol 12. The link's identifier (present only with the
        // owner's ID opt-in) stays out of the summary, as open-link IDs do;
        // it is in the raw bytes.
        Frame::Control(c) => {
            use link::ctrlwatch::ControlContent as C;
            let s = &c.sighting;
            write!(
                w,
                "control {:?} {:?} f={}.{:03}MHz rssi{} snr{} {:?} best={:?} part={} pkts={}",
                s.system,
                s.region,
                s.freq_hz / 1_000_000,
                s.freq_hz / 1000 % 1000,
                s.rssi_dbm,
                s.snr_db,
                s.confidence,
                c.best,
                c.part,
                c.packets
            )?;
            if s.id.is_some() {
                write!(w, " id=yes")?;
            }
            for item in &c.content {
                match item {
                    C::Channels(ch) => write!(w, " channels={}", ch.present.count_ones())?,
                    C::Telemetry(_) => write!(w, " telemetry")?,
                    C::Position(p) => write!(w, " pos={:.7},{:.7}", p.lat_e7 as f64 / 1e7, p.lon_e7 as f64 / 1e7)?,
                    C::Opaque(o) => write!(w, " opaque={o:?}")?,
                }
            }
            match &c.raw {
                Some(r) => write!(w, " raw={}B", r.len()),
                None => Ok(()),
            }
        }
        Frame::ControlEnergy(e) => {
            use link::EnergyActivity as E;
            write!(w, "control_energy {:?} windows={} steady={}", e.region, e.windows, e.steady.len())?;
            match &e.activity {
                E::Quiet => write!(w, " quiet"),
                E::Steady => write!(w, " steady"),
                E::Background => write!(w, " background"),
                E::Hopper(h) => {
                    write!(w, " hopper {}-{}kHz ch{} rssi{}", h.low_khz, h.high_khz, h.channels, h.rssi)?;
                    match h.candidates.first() {
                        Some((sys, score)) => write!(w, " best={sys:?}:{score}"),
                        None => Ok(()),
                    }
                }
            }
        }
        _ => write!(w, "not a data frame"),
    }
}

/// Postcard bytes of `f` (the detection record's raw bytes).
pub fn raw_bytes<'a>(f: &Frame, out: &'a mut [u8]) -> Option<&'a mut [u8]> {
    postcard::to_slice(f, out).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawError {
    UnknownFormat(u8),
    /// The bytes are not a link frame.
    NotAFrame,
    /// Bytes left over after the frame.
    TrailingBytes,
    /// A frame, but not a data frame.
    NotData,
    /// Decodes, but re-encoding gives different bytes (non-canonical input).
    NotCanonical,
}

impl core::fmt::Display for RawError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownFormat(v) => write!(f, "unknown raw format {v}"),
            Self::NotAFrame => write!(f, "raw bytes are not a link protocol 6 frame"),
            Self::TrailingBytes => write!(f, "bytes left over after the frame"),
            Self::NotData => write!(f, "raw bytes are a link bookkeeping frame, not a detection"),
            Self::NotCanonical => write!(f, "raw bytes are not in canonical encoding"),
        }
    }
}

/// Decode stored raw bytes: the frame's info and summary.
pub fn decode_raw(format: u8, raw: &[u8]) -> Result<(FrameInfo, Summary, Frame), RawError> {
    if format != RAW_LINK_FRAME_V6 {
        return Err(RawError::UnknownFormat(format));
    }
    let (f, rest): (Frame, &[u8]) = postcard::take_from_bytes(raw).map_err(|_| RawError::NotAFrame)?;
    if !rest.is_empty() {
        return Err(RawError::TrailingBytes);
    }
    let mut again = [0u8; link::MAX_FRAME];
    if raw_bytes(&f, &mut again).is_none_or(|b| &b[..] != raw) {
        return Err(RawError::NotCanonical);
    }
    let i = info(&f).ok_or(RawError::NotData)?;
    let mut s = Summary::new();
    let _ = write_summary(&f, &mut s);
    Ok((i, s, f))
}
