//! ExpressLRS: air rates, sync channels, sync-packet recognition without
//! the binding phrase, RC and telemetry decoding, and follow mode.
//!
//! Ported from ExpressLRS (github.com/ExpressLRS/ExpressLRS, GPLv3; master
//! of 2026-11 and tags 3.6.3 / 4.1.0): `src/src/common.cpp` (air-rate
//! tables), `src/include/common.h` (rate enums), `src/lib/OTA/OTA.{h,cpp}`
//! (packet layouts, CRC, channel packing), `src/lib/FHSS/{FHSS.cpp,
//! random.cpp}` (domains, sync channel, hop sequence), `src/lib/
//! StubbornReceiver` (telemetry chunking), `src/python/binary_configurator.py`
//! (binding phrase -> UID).
//!
//! # How a link is recognised without the binding phrase
//!
//! Every ELRS transmitter returns to a fixed **sync channel** once per pass
//! through its hop table and, on it, now and then sends a SYNC packet (every
//! 5 s when connected, every 0.6 s on 900 MHz when not). The sync channel is
//! the middle of the domain's channel list: `count / 2` in 4.x, `count / 2
//! + 1` in 3.x. LoRa rates use the private sync word 0x12, implicit header,
//! a fixed 8-byte (OTA4) or 13-byte (OTA8) payload and no LoRa CRC; on
//! 2.4 GHz the IQ polarity is `UID[5] & 1`, so both are tried.
//!
//! A SYNC packet carries UID bytes 4 and 5 (and 3, in 3.x), and its CRC is
//! seeded from exactly those bytes: `init = (UID4 << 8 | UID5) ^ (4 << 8)`
//! in 4.x, `^ 3` in 3.x, with no nonce for SYNC. So a SYNC packet can be
//! checked using only what it carries. Model Match XORs the low 6 bits of
//! the *sent* UID5 with the model ID, so the recogniser tries the 64
//! possible XOR values (32 on 2.4 GHz, where the IQ polarity gives away the
//! real low bit), cheapest first. On top of the CRC: the sync's hop index
//! must be a multiple of the channel count (we are parked on the sync
//! channel, which only appears at the start of each block of the hop
//! table) and, in 4.x, the rate enum must be a real one with the spare bit
//! clear. A random 8-byte packet passes all that with odds of about 2^-17
//! (Model Match off) to 2^-13 (Model Match on).
//!
//! Other packets on the sync channel (RC data, telemetry) have the right
//! size but their CRC also needs the nonce (4.x) or cannot be told from
//! noise without the UID; they are reported as [`Confidence::Likely`], and
//! only when their SNR is above the demodulation floor.
//!
//! FLRC (2.4 GHz "F1000/F500/D500/D250") and GFSK ("K1000", 2.4 FSK) rates
//! cannot be received at all without the binding phrase: the radio's sync
//! word is `UID[2..6]` (FLRC) or `UID[5], UID[4]` (GFSK). They are energy
//! only (see [`crate::signatures`]).

use crate::content::{Channels, ControlContent, Opaque, Telemetry};
use crate::crc::{ELRS_CRC14, ELRS_CRC16};
use crate::rx::{LoRa, LoRaBw, LoRaCr, LoRaHeader, Modulation, Probe, RxConfig, RxMeta};
use crate::{Confidence, Config, Direction, LinkId, Recognised, Region, System};

pub const OTA4_LEN: usize = 8;
pub const OTA8_LEN: usize = 13;
const TYPE_RCDATA: u8 = 0b00; // uplink; downlink LINKSTATS
const TYPE_DATA: u8 = 0b01;
const TYPE_SYNC: u8 = 0b10;
const MODELMATCH_MASK: u8 = 0x3F;

/// ELRS OTA protocol generation. 3.x and 4.x put the sync channel in
/// different places and lay out the SYNC packet differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OtaVersion {
    /// ExpressLRS 3.x (`OTA_VERSION_ID` 3).
    V3,
    /// ExpressLRS 4.x and master (`OTA_VERSION_ID` 4).
    V4,
}

impl OtaVersion {
    pub const fn id(self) -> u8 {
        match self {
            OtaVersion::V3 => 3,
            OtaVersion::V4 => 4,
        }
    }

    /// The CRC initialiser `OtaUpdateCrcInitFromUid` computes.
    pub const fn crc_init(self, uid4: u8, uid5: u8) -> u16 {
        let base = ((uid4 as u16) << 8) | uid5 as u16;
        match self {
            OtaVersion::V3 => base ^ 3,
            OtaVersion::V4 => base ^ (4 << 8),
        }
    }

    /// `OtaGetUidSeed`: the FHSS seed (and the FLRC sync word).
    pub const fn seed(self, uid: &[u8; 6]) -> u32 {
        ((uid[2] as u32) << 24)
            .wrapping_add((uid[3] as u32) << 16)
            .wrapping_add((uid[4] as u32) << 8)
            .wrapping_add((uid[5] ^ self.id()) as u32)
    }
}

/// A regulatory domain's channel plan (`fhss_config_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain {
    pub start_hz: u32,
    pub stop_hz: u32,
    pub count: u8,
}

impl Domain {
    pub const fn spread_hz(&self) -> u32 {
        (self.stop_hz - self.start_hz) / (self.count as u32 - 1)
    }
    pub const fn freq(&self, ch: u8) -> u32 {
        self.start_hz + ch as u32 * self.spread_hz()
    }
    /// `(FHSS_SEQUENCE_LEN / count) * count`.
    pub const fn sequence_len(&self) -> u16 {
        (256 / self.count as u16) * self.count as u16
    }
}

/// ELRS's domain table for `version`. EU868 moved in 4.x (865.275 ->
/// 863.275 MHz start); TH920 exists only on master.
pub const fn domain(region: Region, version: OtaVersion) -> Domain {
    const fn d(start_hz: u32, stop_hz: u32, count: u8) -> Domain {
        Domain {
            start_hz,
            stop_hz,
            count,
        }
    }
    match region {
        Region::Au915 => d(915_500_000, 926_900_000, 20),
        Region::Fcc915 => d(903_500_000, 926_900_000, 40),
        Region::Eu868 => match version {
            OtaVersion::V3 => d(865_275_000, 869_575_000, 13),
            OtaVersion::V4 => d(863_275_000, 869_575_000, 13),
        },
        Region::In866 => d(865_375_000, 866_950_000, 4),
        Region::Au433 => d(433_420_000, 434_420_000, 3),
        Region::Eu433 => d(433_100_000, 434_450_000, 3),
        Region::Us433 => d(433_250_000, 438_000_000, 8),
        Region::Us433W => d(423_500_000, 438_000_000, 20),
        Region::Th920 => d(920_500_000, 924_700_000, 8),
        Region::Ism2g4 => d(2_400_400_000, 2_479_400_000, 80),
    }
}

/// The sync channel index (`sync_channel`).
pub const fn sync_channel(region: Region, version: OtaVersion) -> u8 {
    let c = domain(region, version).count / 2;
    match version {
        OtaVersion::V3 => c + 1,
        OtaVersion::V4 => c,
    }
}

pub const fn sync_freq(region: Region, version: OtaVersion) -> u32 {
    domain(region, version).freq(sync_channel(region, version))
}

/// Modulation family of a rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phy {
    /// (SF, BW, CR, preamble symbols).
    LoRa(u8, LoRaBw, LoRaCr, u16),
    /// FLRC 650 kbit/s, CR 1/2: needs the binding phrase.
    Flrc,
    /// GFSK 300 kbit/s, 100 kHz deviation: needs the binding phrase.
    Gfsk,
}

/// ExpressLRS air rates (every distinct over-the-air configuration in the
/// SX127x, SX1280, LR1121 and LR2021 tables). `Dual*` are the two halves of
/// the LR1121/LR2021 dual-band ("Gemini X") rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(non_camel_case_types)] // names follow ELRS's ("D50", "F1000", ...)
pub enum ElrsRate {
    L900_25,
    L900_50,
    L900_100,
    L900_100Full,
    L900_200,
    L900_200Full,
    L900_250,
    L900_D50,
    Dual900_150,
    Dual900_100Full,
    K900_1000Full,
    L2g4_50,
    L2g4_100Full,
    L2g4_150,
    L2g4_250,
    L2g4_333Full,
    L2g4_500,
    Dual2g4_150,
    Dual2g4_100Full,
    F2g4_1000,
    F2g4_500,
    F2g4_D500,
    F2g4_D250,
    K2g4_1000,
    K2g4_D500,
    K2g4_D250,
}

/// One row of the rate table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateInfo {
    pub rate: ElrsRate,
    pub name: &'static str,
    pub phy: Phy,
    pub sub_ghz: bool,
    /// Payload length: 8 (OTA4) or 13 (OTA8).
    pub len: u8,
    /// Packet interval, µs.
    pub interval_us: u32,
    /// Packets per hop (`FHSShopInterval`).
    pub hop_interval: u8,
    /// `expresslrs_RFrates_e` in 4.x.
    pub v4_enum: u8,
}

macro_rules! lora {
    ($sf:expr, $bw:ident, $cr:ident, $pre:expr) => {
        Phy::LoRa($sf, LoRaBw::$bw, LoRaCr::$cr, $pre)
    };
}

pub const RATES: [RateInfo; 26] = {
    use ElrsRate::*;
    #[allow(clippy::too_many_arguments)]
    const fn r(
        rate: ElrsRate,
        name: &'static str,
        phy: Phy,
        sub_ghz: bool,
        len: u8,
        interval_us: u32,
        hop_interval: u8,
        v4_enum: u8,
    ) -> RateInfo {
        RateInfo {
            rate,
            name,
            phy,
            sub_ghz,
            len,
            interval_us,
            hop_interval,
            v4_enum,
        }
    }
    [
        r(
            L900_25,
            "25Hz",
            lora!(9, Khz500, Cr4_7, 10),
            true,
            8,
            40_000,
            2,
            0,
        ),
        r(
            L900_50,
            "50Hz",
            lora!(8, Khz500, Cr4_7, 10),
            true,
            8,
            20_000,
            4,
            1,
        ),
        r(
            L900_100,
            "100Hz",
            lora!(7, Khz500, Cr4_7, 8),
            true,
            8,
            10_000,
            4,
            2,
        ),
        r(
            L900_100Full,
            "100Hz Full",
            lora!(6, Khz500, Cr4_8, 8),
            true,
            13,
            10_000,
            4,
            3,
        ),
        r(
            L900_200,
            "200Hz",
            lora!(6, Khz500, Cr4_7, 8),
            true,
            8,
            5_000,
            4,
            5,
        ),
        r(
            L900_200Full,
            "200Hz Full",
            lora!(5, Khz500, Cr4_7, 8),
            true,
            13,
            5_000,
            4,
            6,
        ),
        r(
            L900_250,
            "250Hz",
            lora!(5, Khz500, Cr4_8, 8),
            true,
            8,
            4_000,
            4,
            7,
        ),
        r(
            L900_D50,
            "D50",
            lora!(6, Khz500, Cr4_7, 8),
            true,
            8,
            5_000,
            2,
            10,
        ),
        r(
            Dual900_150,
            "X150 (900 half)",
            lora!(6, Khz500, Cr4_8, 12),
            true,
            8,
            6_666,
            4,
            101,
        ),
        r(
            Dual900_100Full,
            "X100 Full (900 half)",
            lora!(6, Khz500, Cr4_8, 18),
            true,
            13,
            10_000,
            4,
            100,
        ),
        r(
            K900_1000Full,
            "K1000 Full",
            Phy::Gfsk,
            true,
            13,
            1_000,
            2,
            11,
        ),
        r(
            L2g4_50,
            "50Hz",
            lora!(8, Khz812, Li4_8, 12),
            false,
            8,
            20_000,
            2,
            21,
        ),
        r(
            L2g4_100Full,
            "100Hz Full",
            lora!(7, Khz812, Li4_8, 12),
            false,
            13,
            10_000,
            4,
            23,
        ),
        r(
            L2g4_150,
            "150Hz",
            lora!(7, Khz812, Li4_8, 12),
            false,
            8,
            6_666,
            4,
            24,
        ),
        r(
            L2g4_250,
            "250Hz",
            lora!(6, Khz812, Li4_8, 14),
            false,
            8,
            4_000,
            4,
            27,
        ),
        r(
            L2g4_333Full,
            "333Hz Full",
            lora!(5, Khz812, Li4_8, 12),
            false,
            13,
            3_003,
            4,
            28,
        ),
        r(
            L2g4_500,
            "500Hz",
            lora!(5, Khz812, Li4_6, 12),
            false,
            8,
            2_000,
            4,
            29,
        ),
        r(
            Dual2g4_150,
            "X150 (2.4 half)",
            lora!(7, Khz812, Li4_6, 12),
            false,
            8,
            6_666,
            4,
            101,
        ),
        r(
            Dual2g4_100Full,
            "X100 Full (2.4 half)",
            lora!(7, Khz812, Li4_8, 14),
            false,
            13,
            10_000,
            4,
            100,
        ),
        r(F2g4_1000, "F1000", Phy::Flrc, false, 8, 1_000, 2, 33),
        r(F2g4_500, "F500", Phy::Flrc, false, 8, 2_000, 2, 32),
        r(F2g4_D500, "D500", Phy::Flrc, false, 8, 1_000, 2, 31),
        r(F2g4_D250, "D250", Phy::Flrc, false, 8, 1_000, 2, 30),
        r(K2g4_1000, "K1000 (2.4)", Phy::Gfsk, false, 8, 1_000, 2, 36),
        r(
            K2g4_D500,
            "K500 DVDA (2.4)",
            Phy::Gfsk,
            false,
            8,
            1_000,
            2,
            35,
        ),
        r(
            K2g4_D250,
            "K250 DVDA (2.4)",
            Phy::Gfsk,
            false,
            8,
            1_000,
            2,
            34,
        ),
    ]
};

impl ElrsRate {
    pub fn info(self) -> &'static RateInfo {
        RATES
            .iter()
            .find(|r| r.rate == self)
            .expect("every rate is in RATES")
    }

    /// The rate a 4.x SYNC packet announces (`rfRateEnum`), for the band
    /// the packet arrived on.
    pub fn from_v4_enum(e: u8, sub_ghz: bool) -> Option<ElrsRate> {
        RATES
            .iter()
            .find(|r| r.v4_enum == e && r.sub_ghz == sub_ghz)
            .map(|r| r.rate)
    }

    pub fn is_lora(self) -> bool {
        matches!(self.info().phy, Phy::LoRa(..))
    }
}

/// Is `e` a value of 4.x's `expresslrs_RFrates_e`?
const fn v4_enum_valid(e: u8, sub_ghz: bool) -> bool {
    match e {
        0..=11 => sub_ghz,
        20..=36 => !sub_ghz,
        100 | 101 => true,
        _ => false,
    }
}

/// The LoRa receive configuration for `rate` on `freq_hz`.
pub fn lora_rx(
    rate: ElrsRate,
    freq_hz: u32,
    invert_iq: bool,
    region: Region,
    version: OtaVersion,
) -> Option<RxConfig> {
    let info = rate.info();
    let Phy::LoRa(sf, bw, cr, pre) = info.phy else {
        return None;
    };
    Some(RxConfig {
        freq_hz,
        modulation: Modulation::LoRa(LoRa {
            sf,
            bw,
            cr,
            preamble_symbols: pre,
            sync_word: 0x12,
            header: LoRaHeader::Implicit { len: info.len },
            crc_on: false,
            // ExpressLRS forces standard IQ below 1 GHz.
            invert_iq: invert_iq && !info.sub_ghz,
            ldro: false,
        }),
        probe: Probe::Elrs { rate, version },
        region,
    })
}

/// Fields of a SYNC packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SyncInfo {
    pub version: OtaVersion,
    pub fhss_index: u8,
    pub nonce: u8,
    /// 4.x: `rfRateEnum`; 3.x: the 4-bit rate *index* into the sender's
    /// hardware table (meaning depends on its radio chip).
    pub rate_field: u8,
    pub switch_mode: u8,
    /// `newTlmRatio` (0 = no telemetry, 1 = 1:128 ... 7 = 1:2).
    pub tlm_ratio: u8,
    pub uid3: Option<u8>,
    pub uid4: u8,
    /// UID5 as sent (Model Match XOR included).
    pub uid5_sent: u8,
    /// UID5 the CRC check settled on.
    pub uid5: u8,
    pub model_match: bool,
    /// 4.x `otaProtocol` (1 = MAVLink link mode).
    pub ota_protocol: u8,
}

impl SyncInfo {
    pub const fn crc_init(&self) -> u16 {
        self.version.crc_init(self.uid4, self.uid5)
    }

    /// Telemetry denominator: every `n`th nonce is a downlink slot.
    pub const fn tlm_denominator(&self) -> Option<u16> {
        match self.tlm_ratio {
            1..=7 => Some(256 >> self.tlm_ratio),
            _ => None,
        }
    }
}

/// CRC of an OTA packet as sent (`GeneratePacketCrc*`). `crc_high_preset`
/// is what 3.x ORs into byte 0 before an OTA4 RC CRC in Wide mode.
fn ota_crc(pkt: &[u8], init: u16, crc_high_preset: u8) -> Option<u16> {
    match pkt.len() {
        OTA4_LEN => {
            let mut b = [0u8; 7];
            b.copy_from_slice(&pkt[..7]);
            b[0] = (b[0] & 0x03) | (crc_high_preset << 2);
            Some(ELRS_CRC14.calc(&b, init))
        }
        OTA8_LEN => Some(ELRS_CRC16.calc(&pkt[..11], init)),
        _ => None,
    }
}

fn stored_crc(pkt: &[u8]) -> Option<u16> {
    match pkt.len() {
        OTA4_LEN => Some(((pkt[0] as u16 >> 2) << 8) | pkt[7] as u16),
        OTA8_LEN => Some(u16::from_le_bytes([pkt[11], pkt[12]])),
        _ => None,
    }
}

/// Check a packet's CRC with a given initialiser (nonce already XORed in).
pub fn crc_ok(pkt: &[u8], init: u16) -> bool {
    stored_crc(pkt).is_some_and(|s| ota_crc(pkt, init, 0) == Some(s))
}

/// Try to read `pkt` as a SYNC packet of `version`, checking its CRC against
/// the UID bytes it carries. `iq_inverted` (2.4 GHz only) constrains the
/// real UID5's low bit; pass `None` on 900 MHz.
pub fn parse_sync(
    pkt: &[u8],
    version: OtaVersion,
    domain: &Domain,
    iq_inverted: Option<bool>,
    sub_ghz: bool,
) -> Option<SyncInfo> {
    if pkt.len() != OTA4_LEN && pkt.len() != OTA8_LEN {
        return None;
    }
    if pkt[0] & 0x03 != TYPE_SYNC {
        return None;
    }
    if pkt.len() == OTA8_LEN && pkt[0] & 0xFC != 0 {
        // OTA8: the first byte holds only the type ("only low 2 bits").
        return None;
    }
    let s = &pkt[1..7];
    let fhss_index = s[0];
    if fhss_index as u16 >= domain.sequence_len() || !fhss_index.is_multiple_of(domain.count) {
        return None;
    }
    let (rate_field, switch_mode, tlm_ratio, uid3, uid4, uid5_sent, ota_protocol) = match version {
        OtaVersion::V4 => {
            let f = s[3];
            if f & 0x80 != 0 || !v4_enum_valid(s[2], sub_ghz) {
                return None;
            }
            (s[2], f & 1, (f >> 1) & 7, None, s[4], s[5], (f >> 5) & 3)
        }
        OtaVersion::V3 => {
            let f = s[2];
            (f >> 4, f & 1, (f >> 1) & 7, Some(s[3]), s[4], s[5], 0)
        }
    };
    if pkt.len() == OTA8_LEN && pkt[7..11].iter().any(|&b| b != 0) {
        // OTA8 sync: the 4 spare bytes are zero (packet memset to 0).
        return None;
    }
    let stored = stored_crc(pkt)?;
    // Model Match off first (x = 0), then the other 63 XOR values.
    for x in 0..=MODELMATCH_MASK {
        let uid5 = uid5_sent ^ x;
        if iq_inverted.is_some_and(|iq| (uid5 & 1 == 1) != iq) {
            continue;
        }
        if ota_crc(pkt, version.crc_init(uid4, uid5), 0) == Some(stored) {
            return Some(SyncInfo {
                version,
                fhss_index,
                nonce: s[1],
                rate_field,
                switch_mode,
                tlm_ratio,
                uid3,
                uid4,
                uid5_sent,
                uid5,
                model_match: x != 0,
                ota_protocol,
            });
        }
    }
    None
}

/// Stateless recogniser (see the module docs).
pub fn recognise(pkt: &[u8], rx: &RxConfig, meta: RxMeta, cfg: &Config) -> Option<Recognised> {
    let Probe::Elrs { rate, version } = rx.probe else {
        return None;
    };
    let info = rate.info();
    if pkt.len() != info.len as usize {
        return None;
    }
    let (sf, iq) = match rx.modulation {
        Modulation::LoRa(l) => (l.sf, l.invert_iq),
        _ => return None,
    };
    let iq = (!info.sub_ghz).then_some(iq);
    let dom = domain(rx.region, version);
    if let Some(s) = parse_sync(pkt, version, &dom, iq, info.sub_ghz) {
        // Model Match widens the CRC search 64-fold. That is still strong
        // for OTA8 (16-bit CRC, zero spare bytes) and for 4.x (rate enum
        // and spare-bit checks), but a 3.x OTA4 sync that only checks with
        // Model Match is left at Likely.
        let weak = s.model_match && version == OtaVersion::V3 && pkt.len() == OTA4_LEN;
        let conf = if weak {
            Confidence::Likely
        } else {
            Confidence::Confirmed
        };
        let mut sighting = crate::sighting(System::ExpressLrs, rx, meta, conf, Direction::Uplink);
        sighting.version = Some(version.id());
        if cfg.read_ids {
            sighting.id = Some(LinkId::ElrsUid {
                uid3: s.uid3,
                uid4: s.uid4,
                uid5: s.uid5_sent,
                uid5_real: s.uid5,
            });
        }
        let mut r = Recognised::new(sighting);
        r.keep_raw(cfg, pkt);
        return Some(r);
    }
    // Not a checkable SYNC: plausibility only.
    let ty = pkt[0] & 0x03;
    if version == OtaVersion::V4 && ty == 0b11 {
        return None;
    }
    if meta.snr_db < crate::lora_snr_floor(sf) + 3 {
        return None;
    }
    let sighting = crate::sighting(
        System::ExpressLrs,
        rx,
        meta,
        Confidence::Likely,
        Direction::Unknown,
    );
    let mut r = Recognised::new(sighting);
    r.keep_raw(cfg, pkt);
    Some(r)
}

// ---------------------------------------------------------------- content

/// CRSF channel value (11-bit, 172..1811 = 988..2012 µs) to µs.
const fn crsf_to_us(v: u32) -> u16 {
    ((v as i32 - 992) * 5 / 8 + 1500) as u16
}

/// `UINT10_to_CRSF`: Hybrid/Wide 10-bit analog (0..1023 covers 988..2012 µs).
const fn uint10_to_us(v: u32) -> u16 {
    crsf_to_us(172 + v * (1811 - 172) / 1023)
}

/// `N_to_CRSF(v, max)`.
const fn n_to_us(v: u32, max: u32) -> u16 {
    crsf_to_us(172 + v * (1811 - 172) / max)
}

fn unpack_4x10(b: &[u8]) -> [u32; 4] {
    let mut out = [0u32; 4];
    let mut bits = 0u32;
    let mut val = 0u32;
    let mut i = 0;
    for o in out.iter_mut() {
        while bits < 10 {
            val |= (b[i] as u32) << bits;
            i += 1;
            bits += 8;
        }
        *o = val & 0x3FF;
        val >>= 10;
        bits -= 10;
    }
    out
}

/// `HybridWideNonceToSwitchIndex`.
pub const fn wide_switch_index(nonce: u8) -> u8 {
    ((nonce & 0b111) + ((nonce >> 3) & 1)) % 8
}

/// Decode an uplink RCDATA packet (CRC already checked). `switch_mode`
/// comes from the link's SYNC (0 = Wide / 8ch-12ch, 1 = Hybrid / 16ch).
pub fn decode_rc(pkt: &[u8], switch_mode: u8, nonce: u8) -> Option<Channels> {
    if pkt[0] & 3 != TYPE_RCDATA {
        return None;
    }
    let mut c = Channels::default();
    match pkt.len() {
        OTA4_LEN => {
            let analog = unpack_4x10(&pkt[1..6]);
            for (i, v) in analog.iter().enumerate() {
                c.set(i, uint10_to_us(*v));
            }
            let armed = pkt[6] & 0x80 != 0;
            c.armed = Some(armed);
            c.set(4, if armed { 2012 } else { 988 });
            let sw = pkt[6] & 0x7F;
            if switch_mode == 0 {
                let idx = wide_switch_index(nonce);
                if idx < 7 {
                    c.set(5 + idx as usize, n_to_us((sw & 0x3F) as u32, 63));
                }
            } else {
                let idx = (sw >> 3) & 0b111;
                if idx >= 6 {
                    c.set(11, n_to_us((sw & 0x0F) as u32, 15));
                } else {
                    // 3-bit switch: 0..5 positions (6, 7 unused) - approximate.
                    let v = (sw & 0b111).min(5) as u32;
                    c.set(5 + idx as usize, n_to_us(v, 5));
                }
            }
        }
        OTA8_LEN => {
            let b0 = pkt[0];
            let high_aux = b0 & 0x40 != 0;
            c.armed = Some(b0 & 0x80 != 0);
            let (lo, hi) = if switch_mode == 1 {
                if high_aux { (8, 12) } else { (0, 4) }
            } else {
                (0, if high_aux { 8 } else { 4 })
            };
            for (base, bytes) in [(lo, &pkt[1..6]), (hi, &pkt[6..11])] {
                for (i, v) in unpack_4x10(bytes).iter().enumerate() {
                    c.set(base + i, crsf_to_us(v << 1));
                }
            }
        }
        _ => return None,
    }
    Some(c)
}

/// Downlink telemetry reassembly (the passive half of ELRS's
/// `StubbornReceiver`): chunks with package index 1, 2, ... and 0 for the
/// last, carrying one CRSF frame (or, with addr 0x10, a MAVLink chunk).
#[derive(Debug, Clone)]
pub struct DlReassembler {
    buf: [u8; 64],
    offset: usize,
    current: u8,
    finished: bool,
}

impl Default for DlReassembler {
    fn default() -> Self {
        Self {
            buf: [0; 64],
            offset: 0,
            current: 1,
            finished: false,
        }
    }
}

impl DlReassembler {
    /// Feed one chunk; returns content decoded from a completed frame.
    pub fn push(
        &mut self,
        package_index: u8,
        max_index: u8,
        data: &[u8],
        mut out: impl FnMut(ControlContent),
    ) {
        if package_index == max_index {
            *self = Self::default();
            return;
        }
        if self.finished {
            *self = Self::default();
        }
        let accept = if package_index == 0 && self.current > 1 {
            self.finished = true;
            true
        } else if package_index == self.current {
            true
        } else if package_index == 1 {
            self.current = 1;
            self.offset = 0;
            true
        } else {
            // Missed a chunk: drop this frame.
            self.current = 1;
            self.offset = 0;
            false
        };
        if !accept {
            return;
        }
        let n = data.len().min(self.buf.len() - self.offset);
        self.buf[self.offset..self.offset + n].copy_from_slice(&data[..n]);
        self.offset += n;
        self.current = self.current.wrapping_add(1);
        // A frame is complete when its own length says so (this also covers
        // one-chunk frames, which never get a 0-index chunk).
        if let Some(total) = crate::crsf::frame_len(&self.buf[..self.offset])
            && self.offset >= total
        {
            let f = &self.buf[..total];
            if f[0] == crate::crsf::ADDR_USB {
                let count = (f[1] as usize).min(total - 2);
                crate::mavlink::decode_all(&f[2..2 + count], &mut out);
            } else if let Some(c) = crate::crsf::decode(f) {
                out(c);
            }
            *self = Self::default();
        }
    }
}

// ------------------------------------------------------------------- FHSS

/// ELRS's `rng()` (a 31-bit LCG).
#[derive(Debug, Clone, Copy)]
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u16 {
        self.0 = (214013u32.wrapping_mul(self.0).wrapping_add(2531011)) % 2_147_483_648;
        (self.0 >> 16) as u16
    }
    fn n(&mut self, max: u8) -> u8 {
        (self.next() % max as u16) as u8
    }
}

/// A link's hop table (`FHSSrandomiseFHSSsequenceBuild`), built block by
/// block so a candidate search can stop at the first contradiction.
#[derive(Debug, Clone)]
pub struct Sequence {
    pub seq: [u8; 256],
    pub len: u16,
    pub domain: Domain,
    pub sync_channel: u8,
}

struct SeqBuilder {
    seq: [u8; 256],
    rng: Rng,
    count: u8,
    len: u16,
    done_to: u16,
}

impl SeqBuilder {
    fn new(seed: u32, count: u8, sync: u8) -> Self {
        let len = (256 / count as u16) * count as u16;
        let mut seq = [0u8; 256];
        for i in 0..len {
            let r = (i % count as u16) as u8;
            seq[i as usize] = if r == 0 {
                sync
            } else if r == sync {
                0
            } else {
                r
            };
        }
        Self {
            seq,
            rng: Rng(seed),
            count,
            len,
            done_to: 0,
        }
    }

    /// Finish the block containing index `i` (and every block before it).
    fn build_to(&mut self, i: u16) {
        let count = self.count as u16;
        let end = ((i / count) + 1) * count;
        while self.done_to < end.min(self.len) {
            let k = self.done_to;
            if !k.is_multiple_of(count) {
                let offset = (k / count) * count;
                let r = self.rng.n(self.count - 1) as u16 + 1;
                self.seq.swap(k as usize, (offset + r) as usize);
            }
            self.done_to += 1;
        }
    }
}

impl Sequence {
    pub fn new(seed: u32, region: Region, version: OtaVersion) -> Self {
        let domain = domain(region, version);
        let sync_channel = sync_channel(region, version);
        let mut b = SeqBuilder::new(seed, domain.count, sync_channel);
        b.build_to(b.len - 1);
        Self {
            seq: b.seq,
            len: b.len,
            domain,
            sync_channel,
        }
    }

    pub fn channel(&self, index: u16) -> u8 {
        self.seq[(index % self.len) as usize]
    }

    pub fn freq(&self, index: u16) -> u32 {
        self.domain.freq(self.channel(index))
    }
}

/// UID from a binding phrase, as `binary_configurator.py` does:
/// `md5('-DMY_BINDING_PHRASE="' + phrase + '"')[0:6]`.
pub fn uid_from_phrase(phrase: &str) -> [u8; 6] {
    let mut h = crate::md5::Md5::new();
    h.update(b"-DMY_BINDING_PHRASE=\"");
    h.update(phrase.as_bytes());
    h.update(b"\"");
    let d = h.finish();
    [d[0], d[1], d[2], d[3], d[4], d[5]]
}

// ----------------------------------------------------------------- follow

/// One observation for UID recovery: at hop-table index `fhss_index` the
/// link was heard on channel `channel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub fhss_index: u16,
    pub channel: u8,
}

/// Outcome of a UID recovery search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Exactly one (UID2, UID3) fits every observation. UID2's top bit
    /// does not affect the hop table and is reported as 0 (it does affect
    /// the FLRC sync word, so FLRC still has two candidates).
    Unique { uid2: u8, uid3: u8 },
    /// More than one fits; observe more channels.
    Ambiguous(u32),
    /// None fits (an observation is wrong, or the version/domain is).
    NoMatch,
}

/// Search for the UID bytes a SYNC packet does not carry (UID2 in 3.x; UID2
/// and UID3 in 4.x) by rebuilding candidate hop tables and checking them
/// against observed (index, channel) pairs. 256 candidates in 3.x, 65,536 in
/// 4.x; each is abandoned at its first contradiction. About log(candidates)
/// / log(count - 1) observations make the answer unique: 2 for 3.x and 4
/// for 4.x on FCC915, 3 for 4.x on 2.4 GHz.
pub struct UidRecovery;

impl UidRecovery {
    pub fn search(
        version: OtaVersion,
        region: Region,
        uid3: Option<u8>,
        uid4: u8,
        uid5: u8,
        obs: &[Observation],
    ) -> Recovery {
        if obs.is_empty() {
            return Recovery::Ambiguous(u32::MAX);
        }
        let dom = domain(region, version);
        let sync = sync_channel(region, version);
        let max_idx = obs
            .iter()
            .map(|o| o.fhss_index % dom.sequence_len())
            .max()
            .unwrap_or(0);
        let mut found = 0u32;
        let mut first = (0u8, 0u8);
        let uid3s: (u16, u16) = match (version, uid3) {
            (OtaVersion::V3, Some(u)) => (u as u16, u as u16),
            _ => (0, 255),
        };
        for u3 in uid3s.0..=uid3s.1 {
            // UID2 bit 7 is seed bit 31, which ELRS's LCG drops (mod 2^31):
            // it does not change the hop table, and cannot be recovered.
            for u2 in 0..=127u8 {
                let uid = [0, 0, u2, u3 as u8, uid4, uid5];
                let mut b = SeqBuilder::new(version.seed(&uid), dom.count, sync);
                let mut ok = true;
                // Check observations in index order as blocks complete.
                b.build_to(max_idx);
                for o in obs {
                    if b.seq[(o.fhss_index % dom.sequence_len()) as usize] != o.channel {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    if found == 0 {
                        first = (u2, u3 as u8);
                    }
                    found += 1;
                }
            }
        }
        match found {
            0 => Recovery::NoMatch,
            1 => Recovery::Unique {
                uid2: first.0,
                uid3: first.1,
            },
            n => Recovery::Ambiguous(n),
        }
    }
}

/// Max observations a [`Follower`] keeps for recovery.
pub const MAX_OBS: usize = 8;

/// What [`Follower::process`] made of a packet.
#[derive(Debug, Clone, PartialEq)]
pub struct Followed {
    pub nonce: u8,
    pub fhss_index: u16,
    pub direction: Direction,
    pub is_sync: bool,
    pub content: heapless::Vec<ControlContent, { crate::MAX_CONTENT }>,
}

/// Follow mode for one ExpressLRS link. Requires
/// [`Config::decode_content`].
///
/// - With the full UID ([`Follower::from_uid`], e.g. from an operator's
///   settings line holding the binding phrase), the follower knows the hop
///   table, the CRC initialiser and the IQ polarity: after one SYNC packet
///   anchors nonce and hop index, [`Follower::next_rx`] says where and when
///   the next packet will be, and [`Follower::process`] decodes RC channels
///   and telemetry (CRSF: GPS, battery, link stats, attitude, flight mode,
///   baro; MAVLink in MAVLink link mode).
/// - From a SYNC packet alone ([`Follower::from_sync`]): the follower knows
///   UID4, UID5 (and UID3 in 3.x), so it can check and decode every packet
///   that lands on the channel the node is parked on. The hop table also
///   needs UID2 (and UID3 in 4.x), which no packet carries. The node parks
///   on other channels, the follower records where in the hop table each
///   CRC-valid packet was ([`Follower::observations`]), and
///   [`Follower::recover`] searches the missing byte(s). That is
///   [`Config::read_ids`] territory too, so recovery needs both flags.
#[derive(Debug, Clone)]
pub struct Follower {
    pub version: OtaVersion,
    pub region: Region,
    pub rate: ElrsRate,
    crc_init: u16,
    uid3: Option<u8>,
    uid4: u8,
    uid5: u8,
    seq: Option<Sequence>,
    /// (time µs, unwrapped nonce, hop index) of the last packet whose
    /// nonce is known.
    anchor: Option<(u64, i64, u16)>,
    switch_mode: u8,
    tlm_denom: Option<u16>,
    dl: DlReassembler,
    obs: heapless::Vec<Observation, MAX_OBS>,
    read_ids: bool,
}

impl Follower {
    /// Follow with a known UID (all six bytes; only 2-5 matter).
    pub fn from_uid(
        cfg: &Config,
        uid: [u8; 6],
        version: OtaVersion,
        region: Region,
        rate: ElrsRate,
    ) -> Option<Self> {
        if !cfg.decode_content {
            return None;
        }
        Some(Self {
            version,
            region,
            rate,
            crc_init: version.crc_init(uid[4], uid[5]),
            uid3: Some(uid[3]),
            uid4: uid[4],
            uid5: uid[5],
            seq: Some(Sequence::new(version.seed(&uid), region, version)),
            anchor: None,
            switch_mode: 0,
            tlm_denom: None,
            dl: DlReassembler::default(),
            obs: heapless::Vec::new(),
            read_ids: cfg.read_ids,
        })
    }

    /// Follow from a SYNC packet only (partial: no hop table yet).
    pub fn from_sync(
        cfg: &Config,
        s: &SyncInfo,
        region: Region,
        rate: ElrsRate,
        t_us: u64,
    ) -> Option<Self> {
        if !cfg.decode_content {
            return None;
        }
        let mut f = Self {
            version: s.version,
            region,
            rate,
            crc_init: s.crc_init(),
            uid3: s.uid3,
            uid4: s.uid4,
            uid5: s.uid5,
            seq: None,
            anchor: None,
            switch_mode: s.switch_mode,
            tlm_denom: s.tlm_denominator(),
            dl: DlReassembler::default(),
            obs: heapless::Vec::new(),
            read_ids: cfg.read_ids,
        };
        f.on_sync(s, t_us);
        Some(f)
    }

    pub fn has_sequence(&self) -> bool {
        self.seq.is_some()
    }

    /// 2.4 GHz IQ polarity of this link.
    pub fn invert_iq(&self) -> bool {
        self.uid5 & 1 == 1
    }

    /// Re-anchor on a SYNC packet received at `t_us`.
    pub fn on_sync(&mut self, s: &SyncInfo, t_us: u64) {
        self.anchor = Some((t_us, s.nonce as i64, s.fhss_index as u16));
        self.switch_mode = s.switch_mode;
        self.tlm_denom = s.tlm_denominator();
    }

    fn hop_len(&self) -> u16 {
        domain(self.region, self.version).sequence_len()
    }

    /// Predicted (unwrapped nonce, hop index) for a packet at `t_us`.
    pub fn predict(&self, t_us: u64) -> Option<(i64, u16)> {
        let (t0, n0, _) = self.anchor?;
        let iv = self.rate.info().interval_us as i64;
        let dt = t_us as i64 - t0 as i64;
        let k = (dt + dt.signum() * iv / 2) / iv;
        let n = n0 + k;
        Some((n, self.index_for(n)?))
    }

    fn index_for(&self, n: i64) -> Option<u16> {
        let (_, n0, f0) = self.anchor?;
        let h = self.rate.info().hop_interval as i64;
        let len = self.hop_len() as i64;
        Some((f0 as i64 + n.div_euclid(h) - n0.div_euclid(h)).rem_euclid(len) as u16)
    }

    /// When and where the next packet at or after `now_us` will be:
    /// (time µs, frequency Hz, receive configuration). Needs the hop table.
    pub fn next_rx(&self, now_us: u64) -> Option<(u64, u32, RxConfig)> {
        let (t0, n0, _) = self.anchor?;
        let iv = self.rate.info().interval_us as u64;
        let k = now_us.saturating_sub(t0).div_ceil(iv);
        let t = t0 + k * iv;
        let idx = self.index_for(n0 + k as i64)?;
        let f = self.seq.as_ref()?.freq(idx);
        let rx = lora_rx(self.rate, f, self.invert_iq(), self.region, self.version)?;
        Some((t, f, rx))
    }

    pub fn observations(&self) -> &[Observation] {
        &self.obs
    }

    /// Try to recover the missing UID byte(s) from the observations.
    /// Requires both opt-in flags (it reconstructs identifier bytes).
    pub fn recover(&mut self) -> Recovery {
        if !self.read_ids {
            return Recovery::NoMatch;
        }
        let r = UidRecovery::search(
            self.version,
            self.region,
            self.uid3,
            self.uid4,
            self.uid5,
            &self.obs,
        );
        if let Recovery::Unique { uid2, uid3 } = r {
            let uid = [0, 0, uid2, uid3, self.uid4, self.uid5];
            self.uid3 = Some(uid3);
            self.seq = Some(Sequence::new(
                self.version.seed(&uid),
                self.region,
                self.version,
            ));
        } else if r == Recovery::NoMatch {
            // Probably a bad observation: start over.
            self.obs.clear();
        }
        r
    }

    /// Check and decode one packet received at `meta.t_us` on hop-table
    /// channel `channel` (the index the node tuned to). Returns `None` if no
    /// nonce near the predicted one makes the CRC check.
    pub fn process(&mut self, pkt: &[u8], meta: RxMeta, channel: u8) -> Option<Followed> {
        let info = self.rate.info();
        if pkt.len() != info.len as usize {
            return None;
        }
        // SYNC packets re-anchor.
        let dom = domain(self.region, self.version);
        if pkt[0] & 3 == TYPE_SYNC
            && let Some(s) = parse_sync(
                pkt,
                self.version,
                &dom,
                (!info.sub_ghz).then_some(self.invert_iq()),
                info.sub_ghz,
            )
            && s.uid4 == self.uid4
            && s.uid5 == self.uid5
        {
            self.on_sync(&s, meta.t_us);
            return Some(Followed {
                nonce: s.nonce,
                fhss_index: s.fhss_index as u16,
                direction: Direction::Uplink,
                is_sync: true,
                content: heapless::Vec::new(),
            });
        }
        let (n_pred, _) = self.predict(meta.t_us)?;
        // Search outward from the prediction.
        let mut hit = None;
        'outer: for d in [0i64, -1, 1, -2, 2, -3, 3] {
            let n = n_pred + d;
            let nonce = n.rem_euclid(256) as u8;
            let init = match self.version {
                OtaVersion::V4 => self.crc_init ^ nonce as u16,
                OtaVersion::V3 => self.crc_init,
            };
            let stored = stored_crc(pkt)?;
            let presets: &[u8] = if self.version == OtaVersion::V3
                && pkt.len() == OTA4_LEN
                && pkt[0] & 3 == TYPE_RCDATA
            {
                &[0, 1, 2, 3, 4]
            } else {
                &[0]
            };
            for &p in presets {
                if p != 0 && p != (nonce % info.hop_interval) + 1 {
                    continue;
                }
                if ota_crc(pkt, init, p) == Some(stored) {
                    hit = Some(n);
                    break 'outer;
                }
            }
            if self.version == OtaVersion::V3 {
                // No nonce in the 3.x CRC: one check settles it.
                break;
            }
        }
        let n = hit?;
        let idx = self.index_for(n)?;
        // Re-anchor on this packet (corrects clock drift).
        self.anchor = Some((meta.t_us, n, idx));
        if self.seq.is_none()
            && channel != sync_channel(self.region, self.version)
            && !self.obs.iter().any(|o| o.fhss_index == idx)
        {
            if self.obs.is_full() {
                self.obs.remove(0);
            }
            let _ = self.obs.push(Observation {
                fhss_index: idx,
                channel,
            });
        }
        let nonce = n.rem_euclid(256) as u8;
        let downlink = self
            .tlm_denom
            .is_some_and(|d| (nonce as u16).is_multiple_of(d));
        let mut out = Followed {
            nonce,
            fhss_index: idx,
            direction: if downlink {
                Direction::Downlink
            } else {
                Direction::Uplink
            },
            is_sync: false,
            content: heapless::Vec::new(),
        };
        let ty = pkt[0] & 3;
        let full = pkt.len() == OTA8_LEN;
        if downlink {
            let (pi, max, stats_at, data): (u8, u8, Option<usize>, &[u8]) = if full {
                let pi = pkt[0] >> 3;
                if ty == TYPE_RCDATA {
                    (pi, 31, Some(1), &pkt[5..11])
                } else {
                    (pi, 31, None, &pkt[1..11])
                }
            } else {
                let pi = pkt[1] & 0x7F;
                if ty == TYPE_RCDATA {
                    (pi, 127, Some(2), &pkt[6..7])
                } else {
                    (pi, 127, None, &pkt[2..7])
                }
            };
            if let Some(o) = stats_at {
                let _ = out.content.push(ControlContent::Telemetry(Telemetry::Link {
                    uplink_rssi_dbm: -((pkt[o] & 0x7F) as i16),
                    uplink_lq: pkt[o + 2] & 0x7F,
                    uplink_snr_db: pkt[o + 3] as i8,
                    downlink_rssi_dbm: None,
                    downlink_lq: None,
                }));
            }
            let content = &mut out.content;
            self.dl.push(pi, max, data, |c| {
                let _ = content.push(c);
            });
        } else if ty == TYPE_RCDATA {
            if let Some(c) = decode_rc(pkt, self.switch_mode, nonce) {
                let _ = out.content.push(ControlContent::Channels(c));
            }
        } else if ty == TYPE_DATA {
            // Uplink data (MSP / MAVLink from the ground): not decoded.
            let _ = out.content.push(ControlContent::Opaque(Opaque::Unparsed));
        }
        Some(out)
    }
}

// ------------------------------------------------------------ test encoder

/// Test-only transmitter model, written from the ELRS source, used to
/// build packets bit-for-bit as a real transmitter would.
#[cfg(test)]
pub mod testenc {
    use super::*;

    pub fn crc_finish(pkt: &mut [u8], init: u16) {
        match pkt.len() {
            OTA4_LEN => {
                let c = ota_crc(pkt, init, (pkt[0] >> 2) & 0x3F).unwrap();
                pkt[0] = (pkt[0] & 3) | (((c >> 8) as u8) << 2);
                pkt[7] = c as u8;
            }
            OTA8_LEN => {
                let c = ota_crc(pkt, init, 0).unwrap();
                pkt[11..13].copy_from_slice(&c.to_le_bytes());
            }
            _ => unreachable!(),
        }
    }

    pub struct Tx {
        pub uid: [u8; 6],
        pub version: OtaVersion,
        pub model_id: Option<u8>,
    }

    impl Tx {
        pub fn sync(
            &self,
            full: bool,
            fhss_index: u8,
            nonce: u8,
            rate_field: u8,
            switch_mode: u8,
            tlm: u8,
        ) -> std::vec::Vec<u8> {
            let mut p = std::vec![0u8; if full { OTA8_LEN } else { OTA4_LEN }];
            p[0] = TYPE_SYNC;
            let mut uid5 = self.uid[5];
            if let Some(m) = self.model_id {
                uid5 ^= !m & MODELMATCH_MASK;
            }
            match self.version {
                OtaVersion::V4 => {
                    p[1..7].copy_from_slice(&[
                        fhss_index,
                        nonce,
                        rate_field,
                        switch_mode | (tlm << 1),
                        self.uid[4],
                        uid5,
                    ]);
                }
                OtaVersion::V3 => {
                    p[1..7].copy_from_slice(&[
                        fhss_index,
                        nonce,
                        switch_mode | (tlm << 1) | (rate_field << 4),
                        self.uid[3],
                        self.uid[4],
                        uid5,
                    ]);
                }
            }
            crc_finish(&mut p, self.version.crc_init(self.uid[4], self.uid[5]));
            p
        }

        fn init(&self, nonce: u8) -> u16 {
            let i = self.version.crc_init(self.uid[4], self.uid[5]);
            match self.version {
                OtaVersion::V4 => i ^ nonce as u16,
                OtaVersion::V3 => i,
            }
        }

        /// OTA4 Hybrid/Wide RC packet. `ch10` are 10-bit analog values.
        pub fn rc4(
            &self,
            nonce: u8,
            ch10: [u16; 4],
            armed: bool,
            switches: u8,
            hop_interval: u8,
            wide: bool,
        ) -> std::vec::Vec<u8> {
            let mut p = std::vec![0u8; OTA4_LEN];
            let mut bits = 0u64;
            for (i, v) in ch10.iter().enumerate() {
                bits |= (*v as u64 & 0x3FF) << (10 * i);
            }
            p[1..6].copy_from_slice(&bits.to_le_bytes()[..5]);
            p[6] = (switches & 0x7F) | if armed { 0x80 } else { 0 };
            if self.version == OtaVersion::V3 && wide {
                p[0] = ((nonce % hop_interval) + 1) << 2;
            }
            crc_finish(&mut p, self.init(nonce));
            p
        }

        /// OTA8 RC packet with 10-bit values (11-bit CRSF >> 1).
        pub fn rc8(
            &self,
            nonce: u8,
            low: [u16; 4],
            high: [u16; 4],
            high_aux: bool,
            armed: bool,
        ) -> std::vec::Vec<u8> {
            let mut p = std::vec![0u8; OTA8_LEN];
            p[0] = (if high_aux { 0x40 } else { 0 }) | (if armed { 0x80 } else { 0 });
            for (o, chans) in [(1usize, low), (6, high)] {
                let mut bits = 0u64;
                for (i, v) in chans.iter().enumerate() {
                    bits |= (*v as u64 & 0x3FF) << (10 * i);
                }
                p[o..o + 5].copy_from_slice(&bits.to_le_bytes()[..5]);
            }
            crc_finish(&mut p, self.init(nonce));
            p
        }

        /// The downlink packets an RX sends for one telemetry frame:
        /// LINKSTATS first (stats + 1 byte), then DATA chunks (OTA4).
        pub fn dl4(
            &self,
            nonces: &mut dyn Iterator<Item = u8>,
            frame: &[u8],
        ) -> std::vec::Vec<std::vec::Vec<u8>> {
            let mut out = std::vec::Vec::new();
            let mut off = 0;
            let mut pkg = 1u8;
            let mut first = true;
            while off < frame.len() {
                let cap = if first { 1 } else { 5 };
                let n = cap.min(frame.len() - off);
                let last = off + n >= frame.len();
                let idx = if pkg > 1 && last { 0 } else { pkg };
                let mut p = std::vec![0u8; OTA4_LEN];
                p[1] = idx;
                if first {
                    p[0] = TYPE_RCDATA; // LINKSTATS
                    p[2..6].copy_from_slice(&[70, 72, 100, 9]);
                    p[6..6 + n].copy_from_slice(&frame[off..off + n]);
                } else {
                    p[0] = TYPE_DATA;
                    p[2..2 + n].copy_from_slice(&frame[off..off + n]);
                }
                crc_finish(&mut p, self.init(nonces.next().unwrap()));
                out.push(p);
                off += n;
                pkg += 1;
                first = false;
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testenc::Tx;
    use super::*;

    fn meta(t_us: u64) -> RxMeta {
        RxMeta {
            rssi_dbm: -70,
            snr_db: 8,
            t_us,
        }
    }

    const PHRASE: &str = "rotorscope test";

    #[test]
    fn sync_channels_match_the_firmware() {
        // FCC915: 40 channels, 600 kHz apart from 903.5 MHz.
        assert_eq!(domain(Region::Fcc915, OtaVersion::V4).spread_hz(), 600_000);
        assert_eq!(sync_freq(Region::Fcc915, OtaVersion::V4), 915_500_000);
        assert_eq!(sync_freq(Region::Fcc915, OtaVersion::V3), 916_100_000);
        assert_eq!(sync_freq(Region::Ism2g4, OtaVersion::V4), 2_440_400_000);
        assert_eq!(sync_freq(Region::Ism2g4, OtaVersion::V3), 2_441_400_000);
        // EU868 3.x: 13 channels 358.33 kHz apart from 865.275, sync = 7.
        assert_eq!(sync_freq(Region::Eu868, OtaVersion::V3), 867_783_331);
    }

    #[test]
    fn hop_table_shape() {
        let uid = uid_from_phrase(PHRASE);
        let s = Sequence::new(OtaVersion::V4.seed(&uid), Region::Fcc915, OtaVersion::V4);
        assert_eq!(s.len, 240);
        for blk in 0..6 {
            let b = &s.seq[blk * 40..blk * 40 + 40];
            assert_eq!(b[0], 20, "sync channel starts every block");
            let mut seen = [false; 40];
            for &c in b {
                assert!(!seen[c as usize]);
                seen[c as usize] = true;
            }
        }
    }

    fn rx_for(rate: ElrsRate, region: Region, version: OtaVersion, iq: bool) -> RxConfig {
        lora_rx(rate, sync_freq(region, version), iq, region, version).unwrap()
    }

    #[test]
    fn v4_sync_confirmed_without_phrase_ids_opt_in() {
        let uid = uid_from_phrase(PHRASE);
        let tx = Tx {
            uid,
            version: OtaVersion::V4,
            model_id: None,
        };
        let p = tx.sync(false, 80, 17, 7, 0, 2);
        let rx = rx_for(ElrsRate::L900_250, Region::Fcc915, OtaVersion::V4, false);
        let r = crate::recognise(&p, &rx, meta(1), &Config::default()).unwrap();
        assert_eq!(r.sighting.confidence, Confidence::Confirmed);
        assert_eq!(r.sighting.version, Some(4));
        assert_eq!(r.sighting.id, None);
        assert!(r.raw.is_none() && r.content.is_empty());
        let r = crate::recognise(
            &p,
            &rx,
            meta(1),
            &Config {
                read_ids: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            r.sighting.id,
            Some(LinkId::ElrsUid {
                uid3: None,
                uid4: uid[4],
                uid5: uid[5],
                uid5_real: uid[5]
            })
        );
    }

    #[test]
    fn model_match_sync_and_v3_and_ota8() {
        let uid = uid_from_phrase("another phrase");
        for version in [OtaVersion::V3, OtaVersion::V4] {
            for full in [false, true] {
                let tx = Tx {
                    uid,
                    version,
                    model_id: Some(3),
                };
                let rate_field = if version == OtaVersion::V4 { 28 } else { 5 };
                let p = tx.sync(full, 160, 200, rate_field, 1, 3);
                let rate = if full {
                    ElrsRate::L2g4_333Full
                } else {
                    ElrsRate::L2g4_500
                };
                let iq = uid[5] & 1 == 1;
                let rx = rx_for(rate, Region::Ism2g4, version, iq);
                let r = crate::recognise(
                    &p,
                    &rx,
                    meta(1),
                    &Config {
                        read_ids: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                // A 3.x OTA4 sync that only checks with Model Match is Likely.
                let want = if version == OtaVersion::V3 && !full {
                    Confidence::Likely
                } else {
                    Confidence::Confirmed
                };
                assert_eq!(r.sighting.confidence, want, "{version:?} full={full}");
                let Some(LinkId::ElrsUid {
                    uid5_real, uid3, ..
                }) = r.sighting.id
                else {
                    panic!()
                };
                assert_eq!(uid5_real, uid[5]);
                assert_eq!(uid3, (version == OtaVersion::V3).then_some(uid[3]));
            }
        }
    }

    #[test]
    fn v3_sync_on_v3_channel_confirmed_and_serialises() {
        let uid = uid_from_phrase(PHRASE);
        let tx = Tx {
            uid,
            version: OtaVersion::V3,
            model_id: None,
        };
        let p = tx.sync(false, 120, 5, 2, 0, 1);
        let rx = rx_for(ElrsRate::L900_100, Region::Fcc915, OtaVersion::V3, false);
        assert_eq!(rx.freq_hz, 916_100_000);
        let cfg = Config {
            read_ids: true,
            decode_content: true,
        };
        let r = crate::recognise(&p, &rx, meta(9), &cfg).unwrap();
        assert_eq!(r.sighting.confidence, Confidence::Confirmed);
        assert_eq!(r.sighting.version, Some(3));
        assert_eq!(r.raw.as_deref(), Some(&p[..]));
        let json = serde_json::to_string(&r).unwrap();
        let back: Recognised = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        // The same packet on a 4.x probe is not a 4.x sync.
        let rx4 = rx_for(ElrsRate::L900_100, Region::Fcc915, OtaVersion::V4, false);
        let r4 = crate::recognise(&p, &rx4, meta(9), &cfg).unwrap();
        assert_eq!(r4.sighting.confidence, Confidence::Likely);
    }

    #[test]
    fn corrupted_or_random_packets_are_not_confirmed() {
        let uid = uid_from_phrase(PHRASE);
        let tx = Tx {
            uid,
            version: OtaVersion::V4,
            model_id: None,
        };
        let mut p = tx.sync(false, 40, 1, 7, 0, 2);
        p[3] ^= 0x01;
        let rx = rx_for(ElrsRate::L900_250, Region::Fcc915, OtaVersion::V4, false);
        let dom = domain(Region::Fcc915, OtaVersion::V4);
        assert!(parse_sync(&p, OtaVersion::V4, &dom, None, true).is_none());
        // 200k pseudo-random 8-byte packets with the SYNC type bits.
        let mut x: u64 = 0x1234_5678;
        let mut confirmed = 0;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let mut b = x.to_le_bytes();
            b[0] = (b[0] & 0xFC) | TYPE_SYNC;
            if let Some(r) = crate::recognise(&b, &rx, meta(0), &Config::default())
                && r.sighting.confidence == Confidence::Confirmed
            {
                confirmed += 1;
            }
        }
        // Expected about 0.5 (2^-18.6 per packet).
        assert!(confirmed <= 2, "{confirmed} random packets confirmed");
    }

    #[test]
    fn wrong_length_and_low_snr_rejected() {
        let rx = rx_for(ElrsRate::L900_250, Region::Fcc915, OtaVersion::V4, false);
        assert!(crate::recognise(&[0u8; 13], &rx, meta(0), &Config::default()).is_none());
        let weak = RxMeta {
            snr_db: -6,
            ..meta(0)
        };
        assert!(
            crate::recognise(&[0x01, 2, 3, 4, 5, 6, 7, 8], &rx, weak, &Config::default()).is_none()
        );
        let r = crate::recognise(
            &[0x01, 2, 3, 4, 5, 6, 7, 8],
            &rx,
            meta(0),
            &Config::default(),
        )
        .unwrap();
        assert_eq!(r.sighting.confidence, Confidence::Likely);
    }

    #[test]
    fn lorawan_and_meshtastic_shapes_do_not_match() {
        let rx = rx_for(ElrsRate::L900_100, Region::Fcc915, OtaVersion::V4, false);
        // A LoRaWAN uplink (MHDR 0x40 + DevAddr + ...) is never 8 bytes with
        // implicit header; a Meshtastic packet has a 16-byte header.
        let lorawan = [
            0x40, 0x11, 0x22, 0x33, 0x44, 0x80, 0x01, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD,
        ];
        assert!(crate::recognise(&lorawan, &rx, meta(0), &Config::default()).is_none());
        let mesh = [0xFFu8; 24];
        assert!(crate::recognise(&mesh, &rx, meta(0), &Config::default()).is_none());
    }

    #[test]
    fn decode_rc_hybrid_wide_and_full() {
        let uid = uid_from_phrase(PHRASE);
        let tx = Tx {
            uid,
            version: OtaVersion::V4,
            model_id: None,
        };
        let p = tx.rc4(9, [0, 511, 1023, 300], true, 63, 4, true);
        let c = decode_rc(&p, 0, 9).unwrap();
        assert_eq!(c.get(0), Some(988));
        assert_eq!(c.get(2), Some(2011));
        assert_eq!(c.armed, Some(true));
        assert_eq!(c.get(4), Some(2012));
        // nonce 9 -> switch index (1 + 1) % 8 = 2 -> channel 7, value 63 -> max.
        assert_eq!(c.get(7), Some(2011));
        let p = tx.rc8(0, [496, 100, 900, 512], [10, 20, 30, 40], true, false);
        let c = decode_rc(&p, 0, 0).unwrap();
        assert_eq!(c.get(0), Some(1500));
        assert!(c.get(8).is_some() && c.get(4).is_none());
    }

    #[test]
    fn follow_with_phrase_decodes_rc_and_gps_telemetry() {
        let cfg = Config {
            read_ids: false,
            decode_content: true,
        };
        assert!(
            Follower::from_uid(
                &Config::default(),
                [0; 6],
                OtaVersion::V4,
                Region::Fcc915,
                ElrsRate::L900_250
            )
            .is_none()
        );
        let uid = uid_from_phrase(PHRASE);
        let tx = Tx {
            uid,
            version: OtaVersion::V4,
            model_id: None,
        };
        let rate = ElrsRate::L900_250;
        let iv = rate.info().interval_us as u64;
        let mut f = Follower::from_uid(&cfg, uid, OtaVersion::V4, Region::Fcc915, rate).unwrap();
        let sync_ch = sync_channel(Region::Fcc915, OtaVersion::V4);
        // SYNC at nonce 8 (hop index 40, the sync channel), tlm 1:4 (k = 6).
        let s = tx.sync(false, 40, 8, 7, 0, 6);
        let t0 = 1_000_000;
        let got = f.process(&s, meta(t0), sync_ch).unwrap();
        assert!(got.is_sync);
        // Next packet: nonce 9 still on index 40; nonce 12 hops to 41.
        let (t, freq, _) = f.next_rx(t0 + 1).unwrap();
        assert_eq!(t, t0 + iv);
        assert_eq!(freq, sync_freq(Region::Fcc915, OtaVersion::V4));
        let (t12, f12, _) = f.next_rx(t0 + 4 * iv - 10).unwrap();
        assert_eq!(t12, t0 + 4 * iv);
        let seq = Sequence::new(OtaVersion::V4.seed(&uid), Region::Fcc915, OtaVersion::V4);
        assert_eq!(f12, seq.freq(41));
        // RC packet at nonce 13.
        let rc = tx.rc4(13, [100, 200, 300, 400], false, 10, 4, true);
        let got = f
            .process(&rc, meta(t0 + 5 * iv + 300), seq.channel(41))
            .unwrap();
        assert_eq!(got.direction, Direction::Uplink);
        let ControlContent::Channels(c) = &got.content[0] else {
            panic!()
        };
        assert_eq!(c.get(0), Some(uint10_to_us(100)));
        // Telemetry: a CRSF GPS frame on downlink slots (nonce % 4 == 0).
        let gps = crate::crsf::encode(
            0xEA,
            crate::crsf::TYPE_GPS,
            &crate::crsf::gps_payload(399_526_000, -751_652_000, 200, 1000, 30, 9),
        );
        let mut nonces = (16u8..).step_by(4);
        let pkts = tx.dl4(&mut nonces, &gps);
        let mut pos = None;
        for (i, p) in pkts.iter().enumerate() {
            let n = 16 + 4 * i as u64;
            let g = f.process(p, meta(t0 + (n - 8) * iv), 0).unwrap();
            assert_eq!(g.direction, Direction::Downlink);
            for c in &g.content {
                if let ControlContent::Position(p) = c {
                    pos = Some(*p);
                }
            }
        }
        let p = pos.expect("GPS decoded from reassembled telemetry");
        assert_eq!((p.lat_e7, p.lon_e7), (399_526_000, -751_652_000));
    }

    #[test]
    fn follow_from_sync_recovers_uid_from_observations() {
        let cfg = Config {
            read_ids: true,
            decode_content: true,
        };
        for version in [OtaVersion::V3, OtaVersion::V4] {
            let uid = uid_from_phrase("recover me");
            let tx = Tx {
                uid,
                version,
                model_id: None,
            };
            let rate = ElrsRate::L2g4_500;
            let region = Region::Ism2g4;
            let iv = rate.info().interval_us as u64;
            let dom = domain(region, version);
            let iq = Some(uid[5] & 1 == 1);
            let rf = if version == OtaVersion::V4 { 29 } else { 4 };
            let sp = tx.sync(false, 80, 0, rf, 0, 0);
            let s = parse_sync(&sp, version, &dom, iq, false).unwrap();
            let t0 = 10_000;
            let mut f = Follower::from_sync(&cfg, &s, region, rate, t0).unwrap();
            assert!(!f.has_sequence());
            let seq = Sequence::new(version.seed(&uid), region, version);
            // Park on a few channels; packets arrive at the hop indices
            // where the real table has that channel.
            let mut n = 0u64;
            let mut seen = 0;
            for idx in 81u16..160 {
                if seen >= 4 {
                    break;
                }
                if idx % 7 != 0 {
                    continue;
                }
                n = (idx as u64 - 80) * 4 + 1;
                let pkt = tx.rc4((n % 256) as u8, [1, 2, 3, 4], false, 0, 4, true);
                let got = f
                    .process(&pkt, meta(t0 + n * iv), seq.channel(idx))
                    .unwrap();
                assert_eq!(got.fhss_index, idx);
                seen += 1;
            }
            let _ = n;
            match f.recover() {
                Recovery::Unique { uid2, uid3 } => {
                    assert_eq!((uid2, uid3), (uid[2] & 0x7F, uid[3]));
                }
                other => panic!("{version:?}: {other:?}"),
            }
            assert!(f.has_sequence());
        }
    }

    #[test]
    fn binding_phrase_uid_is_md5_prefix() {
        // md5('-DMY_BINDING_PHRASE="test"') = 4f04fd822155a7e5... (md5sum).
        assert_eq!(
            uid_from_phrase("test"),
            [0x4f, 0x04, 0xfd, 0x82, 0x21, 0x55]
        );
    }
}
