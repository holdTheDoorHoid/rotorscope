//! Hardware-neutral receive configurations. The node maps each one onto
//! LR2021 commands (packet type, modulation and packet parameters, sync
//! word, frequency); nothing here knows about the chip's registers.

use crate::{Region, elrs::ElrsRate, mlrs::MlrsMode};

/// One receive configuration: where to listen, how, and for what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RxConfig {
    /// Centre frequency, Hz.
    pub freq_hz: u32,
    pub modulation: Modulation,
    /// What this configuration listens for. The recognisers use it to know
    /// which rate a packet came in on; the node only passes it through.
    pub probe: Probe,
    pub region: Region,
}

/// Modulation and packet-format settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Modulation {
    LoRa(LoRa),
    Flrc(Flrc),
    Gfsk(Gfsk),
    /// RSSI only (no packet engine): park or sweep with this receive
    /// bandwidth and report `(freq, rssi, time)` samples to
    /// [`crate::energy`].
    Energy {
        rx_bw_hz: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LoRa {
    /// Spreading factor, 5-12.
    pub sf: u8,
    pub bw: LoRaBw,
    pub cr: LoRaCr,
    /// Preamble length in symbols (what the transmitter sends; a receiver
    /// may use a shorter detector).
    pub preamble_symbols: u16,
    /// One-byte LoRa sync word: 0x12 = "private network" (SX127x value; the
    /// SX126x/SX128x/LR20xx equivalent is 0x1424). Every system here uses
    /// the chip default 0x12; LoRaWAN uses 0x34, Meshtastic 0x2B.
    pub sync_word: u8,
    /// Implicit header with this fixed payload length, or explicit header.
    pub header: LoRaHeader,
    /// LoRa PHY CRC. All the control links here turn it off and check
    /// their own CRC in software.
    pub crc_on: bool,
    pub invert_iq: bool,
    /// Low-data-rate optimisation (only relevant at SF11/12 / narrow BW).
    pub ldro: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LoRaHeader {
    Implicit { len: u8 },
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LoRaBw {
    Khz125,
    Khz250,
    Khz500,
    /// 812.5 kHz, the 2.4 GHz "800 kHz" setting (SX1280 `BW_0800`,
    /// LR2021 `BW_812`).
    Khz812,
}

impl LoRaBw {
    pub const fn hz(self) -> u32 {
        match self {
            LoRaBw::Khz125 => 125_000,
            LoRaBw::Khz250 => 250_000,
            LoRaBw::Khz500 => 500_000,
            LoRaBw::Khz812 => 812_500,
        }
    }
}

/// Coding rate. `Li*` are the SX1280/LR2021 long-interleaving rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LoRaCr {
    Cr4_5,
    Cr4_6,
    Cr4_7,
    Cr4_8,
    Li4_5,
    Li4_6,
    Li4_8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Flrc {
    pub bitrate_bps: u32,
    /// Coding rate numerator/denominator, e.g. (1, 2).
    pub cr: (u8, u8),
    /// Gaussian BT x10 (10 = BT 1.0).
    pub bt_x10: u8,
    /// The 32-bit sync word, when it is known. ExpressLRS and mLRS derive it
    /// from the binding phrase, so a node without the phrase cannot receive
    /// their FLRC packets; such configurations are energy-only.
    pub sync_word: Option<u32>,
    pub payload_len: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Gfsk {
    pub bitrate_bps: u32,
    pub fdev_hz: u32,
    /// Suggested receive filter bandwidth.
    pub rx_bw_hz: u32,
    pub preamble_bits: u16,
    /// Sync word, MSB first, left-aligned; `sync_len_bits` of it are used.
    pub sync_word: [u8; 4],
    pub sync_len_bits: u8,
    /// Read this many bytes after the sync word (fixed-length mode). The
    /// recognisers find the real length inside the packet.
    pub read_len: u8,
    /// Leave whitening and CRC OFF in the radio: the recognisers de-whiten
    /// and check CRCs in software (Si443x whitening and header layout are
    /// not what the LR2021 packet engine expects).
    pub radio_whitening: bool,
    pub radio_crc: bool,
}

/// What a configuration listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Probe {
    /// ExpressLRS at this air rate, parked on the sync channel of
    /// `version` (3.x and 4.x put it on different channels).
    Elrs {
        rate: ElrsRate,
        version: crate::elrs::OtaVersion,
    },
    Mlrs(MlrsMode),
    /// SiK / RFD900 at this air speed (kbit/s, the `AIR_SPEED` setting).
    Sik {
        kbps: u8,
    },
    /// OpenLRSng at `modem_params` index `rate` (0 = 4800 ... 4 = 125000
    /// bit/s); `bind` = listening on the binding frequency.
    OpenLrs {
        rate: u8,
        bind: bool,
    },
    Energy,
}

/// One step of a scan plan: a configuration and how long to stay on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ScanStep {
    pub config: RxConfig,
    /// Minimum dwell, ms.
    pub dwell_ms: u32,
    /// If anything is recognised during the dwell, stay this long in total
    /// (ms) to catch the rarer packet that confirms it (ELRS sync packets
    /// come every 5 s once a link is connected).
    pub extend_ms: u32,
}

/// What the radio reports with each packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RxMeta {
    pub rssi_dbm: i16,
    /// Packet SNR in dB (LoRa); 0 if the modem does not report one.
    pub snr_db: i8,
    /// Caller time, µs (node uptime).
    pub t_us: u64,
}
