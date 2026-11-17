//! A generic receive configuration and the command sequences that apply it.
//!
//! The aero roles ([`crate::roles`]) are four [`RxConfig`]s; a control-link
//! scan (LoRa / FLRC / GFSK configurations, RSSI sweeps across 150-960 MHz
//! and 2.4 GHz) is more of them, applied in turn. Nothing here knows what a
//! config is for.

use heapless::Vec;

use crate::cmd::{self, Cmd};

/// What differs between LR2021 modules. Values for a module come from its
/// schematic (or SoftRF's per-board table); see `firmware/node-lr/src/board.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Board {
    /// TCXO supply voltage index ([`cmd::TCXO_MV`]) and start time in
    /// 32.768 kHz ticks; `None` for a plain crystal (or a TCXO powered
    /// from elsewhere).
    pub tcxo: Option<(u8, u32)>,
    /// The LR2021 DIO (5-11) wired to the MCU as the interrupt line.
    pub irq_dio: u8,
    /// RF switch DIOs driven by the chip itself, with their level in
    /// sub-GHz RX, 2.4 GHz RX and standby. Transmit levels are never set.
    pub rf_switch: &'static [RfSwitchDio],
    /// RX boost level 0-7 for the low-noise amplifier.
    pub rx_boost: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfSwitchDio {
    pub dio: u8,
    pub rx_lf: bool,
    pub rx_hf: bool,
    pub standby: bool,
}

/// Which RF path: sub-GHz (150-1100 MHz) or 2.4 GHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    Lf,
    Hf,
}

impl Path {
    /// The path for a frequency. SoftRF calls anything at or above 1.5 GHz
    /// "high"; 1090 MHz is on the sub-GHz path.
    pub const fn for_hz(hz: u32) -> Path {
        if hz >= 1_500_000_000 { Path::Hf } else { Path::Lf }
    }
}

/// The modem and its packet settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modem {
    /// On-off keying, fixed-length NRZ packets after a sync word.
    Ook {
        bitrate: u32,
        rx_bw: u8,
        /// Sync word (MSB first) and its length in bits.
        sync: u32,
        sync_bits: u8,
        len: u16,
        /// Preamble pattern detector: pattern (LSB first), length field,
        /// repeats.
        detector: (u16, u8, u8),
        /// Detection threshold, as `set_ook_thr` takes it; `None` leaves
        /// the chip's default.
        threshold: Option<i8>,
        /// SoftRF's 1090 register quirk (0xF30150 low byte = 0x18).
        es1090_quirk: bool,
    },
    /// (G)FSK, fixed-length packets after a sync word of up to 64 bits.
    Fsk { bitrate: u32, shaping: u8, rx_bw: u8, fdev: u32, preamble_tx: u16, sync: u64, sync_bits: u8, len: u16 },
    /// LoRa. Codes as in [`cmd::set_lora_modulation`].
    Lora { sf: u8, bw: u8, cr: u8, ldro: bool, sync: u8, preamble: u16, implicit_len: Option<u8>, crc: bool, invert_iq: bool },
    /// No packets: the receiver runs (FSK engine, a sync word that never
    /// matches) only so the instantaneous RSSI can be read during a dwell.
    RssiOnly { rx_bw: u8 },
}

impl Modem {
    pub const fn packet_type(&self) -> u8 {
        match self {
            Modem::Ook { .. } => cmd::packet_type::OOK,
            Modem::Fsk { .. } | Modem::RssiOnly { .. } => cmd::packet_type::FSK_LEGACY,
            Modem::Lora { .. } => cmd::packet_type::LORA,
        }
    }

    /// The fixed packet length the FIFO delivers, if any.
    pub const fn fixed_len(&self) -> Option<u16> {
        match self {
            Modem::Ook { len, .. } | Modem::Fsk { len, .. } => Some(*len),
            Modem::Lora { implicit_len: Some(n), .. } => Some(*n as u16),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxConfig {
    pub freq_hz: u32,
    pub modem: Modem,
}

impl RxConfig {
    pub const fn path(&self) -> Path {
        Path::for_hz(self.freq_hz)
    }
}

/// Room for the longest sequence.
pub type Seq = Vec<Cmd, 32>;

fn push(s: &mut Seq, c: Cmd) {
    // Sized for the longest sequence (tested).
    let _ = s.push(c);
}

/// Once after reset: oscillator, calibration, the RF switch and the IRQ
/// line. Ends in Standby RC.
pub fn init(board: &Board) -> Seq {
    let mut s = Seq::new();
    push(&mut s, cmd::set_standby_rc());
    if let Some((v, t)) = board.tcxo {
        push(&mut s, cmd::set_tcxo_mode(v, t));
    }
    push(&mut s, cmd::calibrate_rx());
    for sw in board.rf_switch {
        push(&mut s, cmd::set_dio_function(sw.dio, cmd::dio_func::RF_SWITCH, 0));
        push(&mut s, cmd::set_dio_rf_switch_rx(sw.dio, sw.rx_hf, sw.rx_lf, sw.standby));
    }
    push(&mut s, cmd::set_dio_function(board.irq_dio, cmd::dio_func::IRQ, 0));
    push(&mut s, cmd::set_dio_irq_config(board.irq_dio, cmd::irq::RX_DONE));
    push(&mut s, cmd::clear_irq(cmd::irq::ALL));
    s
}

/// Apply `cfg` from standby and start continuous reception.
pub fn apply(board: &Board, cfg: &RxConfig) -> Seq {
    let mut s = Seq::new();
    push(&mut s, cmd::set_standby_rc());
    push(&mut s, cmd::set_packet_type(cfg.modem.packet_type()));
    push(&mut s, cmd::set_rf_frequency(cfg.freq_hz));
    push(&mut s, cmd::calib_fe_current());
    push(&mut s, cmd::set_rx_path(cfg.path() == Path::Hf, board.rx_boost));
    push(&mut s, cmd::set_agc_gain(0));
    match cfg.modem {
        Modem::Ook { bitrate, rx_bw, sync, sync_bits, len, detector, threshold, es1090_quirk } => {
            push(&mut s, cmd::set_ook_modulation(bitrate, cmd::shaping::NONE, rx_bw));
            push(&mut s, cmd::set_ook_packet_fixed(0, len));
            push(&mut s, cmd::set_ook_sync_word(sync, sync_bits));
            push(&mut s, cmd::set_ook_detector(detector.0, detector.1, detector.2, false, false, 0));
            if let Some(t) = threshold {
                push(&mut s, cmd::set_ook_threshold(t));
            }
            if es1090_quirk {
                push(&mut s, cmd::write_reg_mem_mask32(cmd::REG_OOK_QUIRK, 0xFF, 0x18));
            }
        }
        Modem::Fsk { bitrate, shaping, rx_bw, fdev, preamble_tx, sync, sync_bits, len } => {
            push(&mut s, cmd::set_fsk_modulation(bitrate, shaping, rx_bw, fdev));
            push(&mut s, cmd::set_fsk_packet_fixed(preamble_tx, len));
            push(&mut s, cmd::set_fsk_sync_word(sync, sync_bits));
        }
        Modem::Lora { sf, bw, cr, ldro, sync, preamble, implicit_len, crc, invert_iq } => {
            push(&mut s, cmd::set_lora_modulation(sf, bw, cr, ldro));
            push(&mut s, cmd::set_lora_packet(preamble, implicit_len.unwrap_or(0), implicit_len.is_some(), crc, invert_iq));
            push(&mut s, cmd::set_lora_syncword(sync));
        }
        Modem::RssiOnly { rx_bw } => {
            push(&mut s, cmd::set_fsk_modulation(50_000, cmd::shaping::NONE, rx_bw, 25_000));
            push(&mut s, cmd::set_fsk_packet_fixed(0, 255));
            // 64 bits no transmitter sends.
            push(&mut s, cmd::set_fsk_sync_word(0x2D5A_B4C3_96E1_87F0, 64));
        }
    }
    push(&mut s, cmd::clear_irq(cmd::irq::ALL));
    push(&mut s, cmd::CLEAR_RX_FIFO.into_iter().collect());
    push(&mut s, cmd::set_rx(cmd::RX_CONTINUOUS));
    s
}

/// Move the running configuration to another frequency on the same path
/// and modem (a hop): standby (oscillator kept), frequency, restart RX.
/// No front-end calibration: it was done at `apply` inside the same band.
pub fn retune(freq_hz: u32) -> Seq {
    let mut s = Seq::new();
    push(&mut s, cmd::set_standby_xosc());
    push(&mut s, cmd::set_rf_frequency(freq_hz));
    push(&mut s, cmd::clear_irq(cmd::irq::ALL));
    push(&mut s, cmd::CLEAR_RX_FIFO.into_iter().collect());
    push(&mut s, cmd::set_rx(cmd::RX_CONTINUOUS));
    s
}

/// Restart reception in place (the 1090 watchdog).
pub fn restart_rx() -> Seq {
    let mut s = Seq::new();
    push(&mut s, cmd::set_standby_xosc());
    push(&mut s, cmd::clear_irq(cmd::irq::ALL));
    push(&mut s, cmd::CLEAR_RX_FIFO.into_iter().collect());
    push(&mut s, cmd::set_rx(cmd::RX_CONTINUOUS));
    s
}

/// The packet-status request for a modem (RSSI of the last packet).
pub fn packet_status_req(m: &Modem) -> [u8; 2] {
    match m {
        Modem::Ook { .. } => cmd::GET_OOK_PACKET_STATUS,
        Modem::Lora { .. } => cmd::GET_LORA_PACKET_STATUS,
        _ => cmd::GET_FSK_PACKET_STATUS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SW: [RfSwitchDio; 2] = [
        RfSwitchDio { dio: 5, rx_lf: false, rx_hf: true, standby: false },
        RfSwitchDio { dio: 8, rx_lf: true, rx_hf: false, standby: false },
    ];
    const BOARD: Board = Board { tcxo: Some((3, 164)), irq_dio: 9, rf_switch: &SW, rx_boost: 7 };

    #[test]
    fn init_sequence() {
        let s = init(&BOARD);
        let ops: std::vec::Vec<[u8; 2]> = s.iter().map(|c| [c[0], c[1]]).collect();
        assert_eq!(ops, [[1, 0x28], [1, 0x20], [1, 0x22], [1, 0x12], [1, 0x13], [1, 0x12], [1, 0x13], [1, 0x12], [1, 0x15], [1, 0x16]]);
        assert_eq!(s[8].as_slice(), [1, 0x15, 9, 0, 4, 0, 0]);
    }

    #[test]
    fn every_role_fits_and_ends_in_rx() {
        for cfg in [
            crate::roles::es1090(),
            crate::roles::uat978(),
            crate::roles::flarm(915_000_000),
            crate::roles::ogn(868_200_000),
            RxConfig { freq_hz: 2_440_000_000, modem: Modem::RssiOnly { rx_bw: cmd::bw::BW_2222 } },
            RxConfig {
                freq_hz: 915_000_000,
                modem: Modem::Lora {
                    sf: 6,
                    bw: 6,
                    cr: 1,
                    ldro: false,
                    sync: 0x12,
                    preamble: 6,
                    implicit_len: Some(8),
                    crc: true,
                    invert_iq: false,
                },
            },
        ] {
            let s = apply(&BOARD, &cfg);
            assert!(s.len() < s.capacity());
            assert_eq!(s.last().unwrap().as_slice(), [2, 0x0C, 0xFF, 0xFF, 0xFF]);
            assert_eq!(s[2].as_slice(), cmd::set_rf_frequency(cfg.freq_hz).as_slice());
            let hf = cfg.freq_hz > 2_000_000_000;
            assert_eq!(s[4].as_slice(), [2, 0x01, hf as u8, 7]);
        }
    }

    #[test]
    fn retune_is_short() {
        let s = retune(910_200_000);
        assert_eq!(s.len(), 5);
        assert_eq!(s[1].as_slice(), cmd::set_rf_frequency(910_200_000).as_slice());
    }
}
