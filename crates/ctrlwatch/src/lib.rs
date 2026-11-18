//! Recognising drone and RC **control links** on the air, for the Phase 9
//! LR2021 node. Receive-only: there is no transmit code in this crate.
//!
//! # What it does
//!
//! - [`scan`]: scan plans as data. For each system and region, the receive
//!   configurations ([`RxConfig`]) the node steps through, with dwell times.
//!   The node applies each one to the LR2021 and hands every received packet
//!   to [`recognise`].
//! - [`recognise`]: one packet in, a [`Recognised`] out (or `None`): which
//!   system, which rate, how sure ([`Confidence`]), and, only when the
//!   operator has opted in, the link's identifier and decoded content.
//! - [`energy`]: an energy map built from wide receive-bandwidth sweeps, a
//!   noise-floor and background-occupancy baseline, and a hop-pattern
//!   classifier that names candidate systems from channel spacing, band width
//!   and burst timing. This is how closed systems (Crossfire, Tracer, Ghost,
//!   FrSky, FlySky) are seen: by energy and timing only.
//! - [`Sightings`]: the per-link count and report rate limit, like
//!   `openlink`'s.
//! - [`elrs::Follower`]: ExpressLRS "follow" mode, which tracks one link's
//!   hop sequence once its UID is known (operator-supplied binding phrase) or
//!   recovered (see [`elrs::UidRecovery`]).
//!
//! # Which systems, and how
//!
//! | System | Packet recognition | Without any key | Identifier (opt-in) | Content (opt-in) |
//! |---|---|---|---|---|
//! | ExpressLRS 900 / 2.4 GHz, LoRa rates | yes | **Confirmed** from a sync packet (CRC checks against UID bytes the packet itself carries); **Likely** from other packets on the sync channel | UID bytes 3/4/5 (3.x) or 4/5 (4.x) | RC channels, CRSF telemetry incl. GPS, in follow mode |
//! | ExpressLRS FLRC / FSK rates (2.4 "F1000/F500", 900 "K1000") | no: the radio's sync word is derived from the binding phrase | energy/timing only | - | only with the binding phrase |
//! | mLRS LoRa modes | yes | **Confirmed** (91-byte frame with its own CRC) | frame sync word | MAVLink if not encrypted |
//! | mLRS FLRC / FSK modes | no (sync word from bind phrase) | energy only | - | - |
//! | SiK / RFD900 | yes (Si443x sync 0x2DD4, header, CRC) | **Confirmed** | NET ID | MAVLink if not AES-encrypted |
//! | OpenLRSng | yes (Si443x sync, 4-byte rf_magic header, packet size) | **Likely**, **Confirmed** over several packets | rf_magic | RC channels (format follows from packet size) |
//! | Crossfire, Tracer, Ghost, FrSky R9/ACCESS, FlySky AFHDS 2A, FrSky D16 | no | energy/timing candidates only | - | - |
//!
//! # The legal line
//!
//! A 2020 DOJ/FAA/FCC/DHS advisory warns that monitoring drone control
//! links can fall under the Wiretap Act and the Pen/Trap statute. This crate
//! is built so the default is the least intrusive thing that still answers
//! "is a drone control link on the air here?":
//!
//! - **Presence and energy are the default.** With [`Config::default`] a
//!   [`Recognised`] says which system, band, rate, frequency, signal
//!   strength and how sure. `id` is `None`, `content` is empty and `raw` is
//!   `None`.
//! - **Identifiers are opt-in** ([`Config::read_ids`]). ExpressLRS UID
//!   bytes, the SiK NET ID, the mLRS frame sync word and the OpenLRSng
//!   rf_magic are only copied out when it is set. When it is not, the
//!   recogniser may still *use* those bytes transiently to check a CRC (an
//!   ELRS sync packet's CRC is seeded from them), but it does not return or
//!   keep them.
//! - **Content is opt-in** ([`Config::decode_content`]), separately. The
//!   owner enabled content decoding on their stated permission on
//!   2026-11-17. When set, the crate decodes RC channel values, CRSF
//!   telemetry and the MAVLink messages listed in [`content`], and keeps the
//!   raw packet for the evidence log. When not set, none of that is computed.
//! - **No encryption is broken.** SiK AES and mLRS encrypted links are
//!   reported as [`content::ControlContent::Opaque`]; nothing tries keys.
//! - **Receive-only.** There is no transmit code of any kind.
//!
//! `no_std`, no allocator (heapless), serde behind a feature.
#![cfg_attr(not(test), no_std)]

pub mod content;
pub mod crc;
pub mod crsf;
pub mod elrs;
pub mod energy;
pub mod mavlink;
pub mod md5;
pub mod mlrs;
pub mod openlrs;
pub mod rx;
pub mod scan;
pub mod sightings;
pub mod signatures;
pub mod sik;

pub use content::ControlContent;
pub use rx::{Modulation, Probe, RxConfig, RxMeta, ScanStep};
pub use sightings::{Sighting, Sightings};

/// What the operator has opted in to. Both flags are off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Config {
    /// Copy link identifiers (ELRS UID bytes, SiK NET ID, mLRS sync word,
    /// OpenLRSng rf_magic) into [`ControlSighting::id`].
    pub read_ids: bool,
    /// Decode control content (RC channels, telemetry, MAVLink positions)
    /// and keep the raw packet bytes.
    pub decode_content: bool,
}

/// A control-link system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum System {
    ExpressLrs,
    Mlrs,
    Sik,
    OpenLrsNg,
    /// TBS Crossfire (900 MHz). Closed; energy only.
    Crossfire,
    /// TBS Tracer (2.4 GHz). Closed; energy only.
    Tracer,
    /// ImmersionRC Ghost (2.4 GHz). Closed; energy only.
    Ghost,
    /// FrSky R9 / ACCESS 900 MHz. Closed; energy only.
    FrskyR9,
    /// FrSky D16 / ACCESS 2.4 GHz (CC2500 family). Energy only.
    FrskyX,
    /// FlySky AFHDS 2A (2.4 GHz, A7105). Energy only.
    FlyskyAfhds2a,
}

impl System {
    /// Display names, in declaration order. (Tables indexed by the
    /// discriminant rather than `match`es: on the ESP32 targets the
    /// compiler's string switch tables land in RAM.)
    const LABELS: [&'static str; 10] = [
        "ExpressLRS",
        "mLRS",
        "SiK/RFD900",
        "OpenLRSng",
        "TBS Crossfire",
        "TBS Tracer",
        "ImmersionRC Ghost",
        "FrSky R9/ACCESS 900",
        "FrSky D16/ACCESS 2.4",
        "FlySky AFHDS 2A",
    ];
    /// Identifier-style names (as the variants and their serde form).
    const NAMES: [&'static str; 10] = [
        "ExpressLrs",
        "Mlrs",
        "Sik",
        "OpenLrsNg",
        "Crossfire",
        "Tracer",
        "Ghost",
        "FrskyR9",
        "FrskyX",
        "FlyskyAfhds2a",
    ];

    pub const fn label(self) -> &'static str {
        Self::LABELS[self as usize]
    }

    /// The variant's name (`"ExpressLrs"`), as serde writes it.
    pub const fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }
}

/// Regulatory domain / band plan. Names follow ExpressLRS's domains where
/// they exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Region {
    Fcc915,
    Au915,
    Eu868,
    In866,
    Au433,
    Eu433,
    Us433,
    Us433W,
    Th920,
    Ism2g4,
}

impl Region {
    pub const ALL: [Region; 10] = [
        Region::Fcc915,
        Region::Au915,
        Region::Eu868,
        Region::In866,
        Region::Au433,
        Region::Eu433,
        Region::Us433,
        Region::Us433W,
        Region::Th920,
        Region::Ism2g4,
    ];

    pub const fn is_2g4(self) -> bool {
        matches!(self, Region::Ism2g4)
    }

    /// The variant's name (`"Fcc915"`), as serde writes it.
    pub const fn name(self) -> &'static str {
        const NAMES: [&str; 10] = ["Fcc915", "Au915", "Eu868", "In866", "Au433", "Eu433", "Us433", "Us433W", "Th920", "Ism2g4"];
        NAMES[self as usize]
    }
}

/// How sure a sighting is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Confidence {
    /// Energy and timing consistent with the system; other emitters could
    /// look the same.
    Energy,
    /// A packet demodulated with exactly this system's radio settings and
    /// the right size and shape, but nothing in it could be checked. A single
    /// one is weak evidence; [`Sightings`] only reports Likely links after
    /// several packets.
    Likely,
    /// The packet passed the system's own CRC (or equivalent) check.
    Confirmed,
}

/// An identifier a link carries in the clear. Only filled in when
/// [`Config::read_ids`] is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LinkId {
    /// ExpressLRS UID bytes from a sync packet. `uid3` is only sent by 3.x.
    /// `uid5` is as sent: with Model Match on, its low 6 bits are XORed with
    /// the model ID; `uid5_real` is the value the CRC check settled on.
    ElrsUid {
        uid3: Option<u8>,
        uid4: u8,
        uid5: u8,
        uid5_real: u8,
    },
    /// mLRS frame sync word (a hash of the bind phrase, plus offsets).
    MlrsSyncWord(u16),
    /// SiK NET ID.
    SikNetId(u16),
    /// OpenLRSng rf_magic (the top three bytes; the low byte is XORed with
    /// the hop index, so it is reported with the hop index removed when
    /// known, else as received).
    OpenLrsMagic(u32),
}

/// Which way a packet was going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Direction {
    /// Ground to aircraft (RC, uplink).
    Uplink,
    /// Aircraft to ground (telemetry).
    Downlink,
    Unknown,
}

/// One packet recognised as a control link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ControlSighting {
    pub system: System,
    pub region: Region,
    /// The probe (rate / mode) the packet arrived on.
    pub probe: Probe,
    pub freq_hz: u32,
    pub rssi_dbm: i16,
    pub snr_db: i8,
    pub t_us: u64,
    pub confidence: Confidence,
    pub direction: Direction,
    /// `None` unless [`Config::read_ids`] is set (and the packet carries one).
    pub id: Option<LinkId>,
    /// Protocol generation, when the packet says (ELRS 3 or 4).
    pub version: Option<u8>,
}

/// Max decoded content items from one packet (an mLRS frame can carry
/// several MAVLink messages).
pub const MAX_CONTENT: usize = 6;
/// Raw packets up to this size are kept for the evidence log.
pub const MAX_RAW: usize = 96;

/// The result of [`recognise`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Recognised {
    pub sighting: ControlSighting,
    /// Decoded content; always empty unless [`Config::decode_content`].
    pub content: heapless::Vec<ControlContent, MAX_CONTENT>,
    /// The packet as received; `None` unless [`Config::decode_content`].
    pub raw: Option<heapless::Vec<u8, MAX_RAW>>,
}

impl Recognised {
    pub(crate) fn new(sighting: ControlSighting) -> Self {
        Self {
            sighting,
            content: heapless::Vec::new(),
            raw: None,
        }
    }

    pub(crate) fn keep_raw(&mut self, cfg: &Config, bytes: &[u8]) {
        if cfg.decode_content {
            self.raw = heapless::Vec::from_slice(&bytes[..bytes.len().min(MAX_RAW)]).ok();
        }
    }

    pub(crate) fn push(&mut self, c: ControlContent) {
        let _ = self.content.push(c);
    }
}

/// Recognise one received packet. `bytes` is the payload as the radio
/// delivered it (for GFSK probes: the fixed-length read after the sync
/// word), `rx` is the configuration it was received on.
pub fn recognise(bytes: &[u8], rx: &RxConfig, meta: RxMeta, cfg: &Config) -> Option<Recognised> {
    match rx.probe {
        Probe::Elrs { .. } => elrs::recognise(bytes, rx, meta, cfg),
        Probe::Mlrs(_) => mlrs::recognise(bytes, rx, meta, cfg),
        Probe::Sik { .. } => sik::recognise(bytes, rx, meta, cfg),
        Probe::OpenLrs { .. } => openlrs::recognise(bytes, rx, meta, cfg),
        Probe::Energy => None,
    }
}

/// The bytes a packet carries for a MAVLink stream (SiK serial data, an
/// mLRS payload), with the direction when the system tells, for
/// `mavwatch`'s hostile-MAVLink scan. `None` for other systems, packets that
/// do not check, command packets and encrypted payloads. Call it only when
/// [`Config::decode_content`] is set: it reads content.
pub fn serial_bytes<'a>(bytes: &[u8], rx: &RxConfig, out: &'a mut [u8; 258]) -> Option<(&'a [u8], Direction)> {
    match rx.probe {
        Probe::Sik { .. } => sik::serial_bytes(bytes, out).map(|b| (b, Direction::Unknown)),
        Probe::Mlrs(_) => {
            let (p, d) = mlrs::serial_bytes(bytes)?;
            let n = p.len().min(out.len());
            out[..n].copy_from_slice(&p[..n]);
            Some((&out[..n], d))
        }
        _ => None,
    }
}

pub(crate) fn sighting(
    system: System,
    rx: &RxConfig,
    meta: RxMeta,
    confidence: Confidence,
    direction: Direction,
) -> ControlSighting {
    ControlSighting {
        system,
        region: rx.region,
        probe: rx.probe,
        freq_hz: rx.freq_hz,
        rssi_dbm: meta.rssi_dbm,
        snr_db: meta.snr_db,
        t_us: meta.t_us,
        confidence,
        direction,
        id: None,
        version: None,
    }
}

/// LoRa demodulation floor (dB SNR) by spreading factor; packets well below
/// this are usually false preamble detections, whose payloads are noise.
pub(crate) const fn lora_snr_floor(sf: u8) -> i8 {
    match sf {
        5 => -3,
        6 => -5,
        7 => -8,
        8 => -10,
        9 => -13,
        10 => -15,
        11 => -18,
        _ => -20,
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn names_match_variants_and_serde() {
        for (i, r) in Region::ALL.iter().enumerate() {
            assert_eq!(*r as usize, i);
            assert_eq!(std::format!("{r:?}"), r.name());
            assert_eq!(serde_json::to_string(r).unwrap(), std::format!("\"{}\"", r.name()));
        }
        let all = [
            System::ExpressLrs,
            System::Mlrs,
            System::Sik,
            System::OpenLrsNg,
            System::Crossfire,
            System::Tracer,
            System::Ghost,
            System::FrskyR9,
            System::FrskyX,
            System::FlyskyAfhds2a,
        ];
        for s in all {
            assert_eq!(std::format!("{s:?}"), s.name());
            assert_eq!(serde_json::to_string(&s).unwrap(), std::format!("\"{}\"", s.name()));
        }
        assert_eq!(System::Ghost.label(), "ImmersionRC Ghost");
    }
}
