//! The aero roles as [`RxConfig`]s, with SoftRF's settings for the LR20xx
//! (`SoftRF/src/driver/radio/radiolib.cpp`, `lr20xx_setup`, and the
//! protocol descriptors in `SoftRF/src/protocol/radio/`).

use link::{AeroMode, AirProto};

use crate::cmd::{bw, shaping};
use crate::config::{Modem, RxConfig};

pub const ES1090_HZ: u32 = 1_090_000_000;
pub const UAT978_HZ: u32 = 978_000_000;

/// ES1090: Mode S long frame, 11 bytes + 3 parity = 14 bytes = 28 chip
/// bytes at 2 Mchip/s. SoftRF ("OPT_DF17 1") syncs on 0x95, the chips of
/// DF17's first nibble, and reads the 27 chip bytes after it.
pub const ES1090_CHIP_LEN: u16 = 27;
/// UAT: fixed 48 bytes (long ADS-B frame, 34 data + 14 Reed-Solomon).
pub const UAT978_LEN: u16 = 48;
/// FLARM ("Legacy") and OGNTP: 24+2 / 20+6 bytes, Manchester: 52 chips.
pub const FLARM_CHIP_LEN: u16 = 52;
pub const OGN_CHIP_LEN: u16 = 52;

/// SoftRF: `setSyncWord({0x95})`.
pub const ES1090_SYNC: u8 = 0x95;
/// `UAT978_SYNCWORD`.
pub const UAT978_SYNC: u32 = 0xACDD_A4E2;
/// `LEGACY_SYNCWORD`.
pub const FLARM_SYNC: u64 = 0x5599_A5A9_5566_6596;
/// `OGNTP_SYNCWORD`.
pub const OGN_SYNC: u64 = 0xAA66_55A5_9699_965A;

/// SoftRF's `setOokDetectionThreshold(-80)` ("TODO" in SoftRF itself). The
/// register encoding of that call is not public; this is the `lr2021`
/// crate's `set_ook_thr` encoding of -80. UNVERIFIED on hardware.
pub const ES1090_OOK_THRESHOLD: i8 = -80;

/// 1090 MHz extended squitter: OOK 2 Mbit/s (chips), 3076 kHz bandwidth,
/// NRZ, sync 0x95 (8 bits), 27 bytes, preamble detector 0x0285.
pub const fn es1090() -> RxConfig {
    RxConfig {
        freq_hz: ES1090_HZ,
        modem: Modem::Ook {
            bitrate: 2_000_000,
            rx_bw: bw::BW_3076,
            sync: ES1090_SYNC as u32,
            sync_bits: 8,
            len: ES1090_CHIP_LEN,
            // SoftRF: ookDetector(0x0285, 16, 0, false, false, 0). The
            // length field is 4 bits wide; the `lr2021` crate's ADS-B setup
            // writes 15 for the same pattern, i.e. the field holds length-1.
            // UNVERIFIED on hardware.
            detector: (0x0285, 15, 0),
            threshold: Some(ES1090_OOK_THRESHOLD),
            es1090_quirk: true,
        },
    }
}

/// 978 MHz UAT: 2-FSK 1041.667 kbit/s, ±312.5 kHz, 3076 kHz bandwidth,
/// 32-bit sync 0xACDDA4E2, 48 bytes, no preamble. (The bitrate is given in
/// whole bit/s; 0.33 bit/s off is far inside any receiver's tolerance.)
pub const fn uat978() -> RxConfig {
    RxConfig {
        freq_hz: UAT978_HZ,
        modem: Modem::Fsk {
            bitrate: 1_041_667,
            shaping: shaping::BT_0_5,
            rx_bw: bw::BW_3076,
            fdev: 312_500,
            preamble_tx: 0,
            sync: UAT978_SYNC as u64,
            sync_bits: 32,
            len: UAT978_LEN,
        },
    }
}

/// FLARM: GFSK 100 kchip/s, ±50 kHz, 307.7 kHz bandwidth, 64-bit sync, 52
/// chip bytes.
pub const fn flarm(freq_hz: u32) -> RxConfig {
    RxConfig {
        freq_hz,
        modem: Modem::Fsk {
            bitrate: 100_000,
            shaping: shaping::BT_0_5,
            rx_bw: bw::BW_307,
            fdev: 50_000,
            preamble_tx: 8,
            sync: FLARM_SYNC,
            sync_bits: 64,
            len: FLARM_CHIP_LEN,
        },
    }
}

/// OGN tracker (OGNTP): as FLARM, with OGN's sync word.
pub const fn ogn(freq_hz: u32) -> RxConfig {
    RxConfig {
        freq_hz,
        modem: Modem::Fsk {
            bitrate: 100_000,
            shaping: shaping::BT_0_5,
            rx_bw: bw::BW_307,
            fdev: 50_000,
            preamble_tx: 8,
            sync: OGN_SYNC,
            sync_bits: 64,
            len: OGN_CHIP_LEN,
        },
    }
}

/// The protocol a mode's frames are reported as; `None` for the control
/// modes (they report `Frame::Control`, see [`crate::ctrl`]).
pub const fn proto(mode: AeroMode) -> Option<AirProto> {
    match mode {
        AeroMode::Es1090 => Some(AirProto::Es1090),
        AeroMode::Uat978 => Some(AirProto::Uat978),
        AeroMode::FlarmHop { ogn: false, .. } => Some(AirProto::Flarm),
        AeroMode::FlarmHop { ogn: true, .. } => Some(AirProto::Ogn),
        AeroMode::Control { .. } | AeroMode::ControlFlags { .. } => None,
    }
}

/// The configuration for an aircraft `mode`, at `freq_hz` for the hopping
/// modes (the fixed modes ignore it). `None` for the control modes, whose
/// configurations come from the scan plan ([`crate::ctrl`]).
pub const fn config(mode: AeroMode, freq_hz: u32) -> Option<RxConfig> {
    Some(match mode {
        AeroMode::Es1090 => es1090(),
        AeroMode::Uat978 => uat978(),
        AeroMode::FlarmHop { ogn: false, .. } => flarm(freq_hz),
        AeroMode::FlarmHop { ogn: true, .. } => ogn(freq_hz),
        AeroMode::Control { .. } | AeroMode::ControlFlags { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_words_are_softrfs_bytes() {
        assert_eq!(FLARM_SYNC.to_be_bytes(), [0x55, 0x99, 0xA5, 0xA9, 0x55, 0x66, 0x65, 0x96]);
        assert_eq!(OGN_SYNC.to_be_bytes(), [0xAA, 0x66, 0x55, 0xA5, 0x96, 0x99, 0x96, 0x5A]);
        assert_eq!(UAT978_SYNC.to_be_bytes(), [0xAC, 0xDD, 0xA4, 0xE2]);
        // 0x95 = Manchester chips 10 01 01 01 = (inverted) 1000, DF17's first nibble.
        assert_eq!(crate::manchester::DECODE[0x95] & 0xF, 0b0111);
        // pkt_size = 2*(11+3) - 1 = 27, (24+2)*2 = 52, (20+6)*2 = 52.
        assert_eq!((ES1090_CHIP_LEN, FLARM_CHIP_LEN, OGN_CHIP_LEN), (27, 52, 52));
    }

    #[test]
    fn modes_map() {
        assert_eq!(proto(AeroMode::FlarmHop { band: 2, ogn: true }), Some(AirProto::Ogn));
        assert_eq!(config(AeroMode::FlarmHop { band: 2, ogn: false }, 910_200_000).unwrap().freq_hz, 910_200_000);
        assert_eq!(config(AeroMode::Es1090, 0).unwrap().freq_hz, ES1090_HZ);
        assert_eq!(proto(AeroMode::Control { regions: 1, energy: 0 }), None);
        assert_eq!(config(AeroMode::Control { regions: 1, energy: 0 }, 0), None);
    }
}
