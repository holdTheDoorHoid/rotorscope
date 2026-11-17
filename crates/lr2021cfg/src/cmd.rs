//! LR2021 SPI commands: receive and configuration only.
//!
//! Opcodes and field layouts are ported from the `lr2021` crate (MIT,
//! TheClams), itself generated from Semtech's datasheet, and checked
//! against what SoftRF sends through RadioLib where SoftRF shows the bytes
//! (`GetVersion` 0x0101, `ReadRegMem32` 0x0106, `WriteRegMemMask32` 0x0105).
//!
//! Wire protocol: wait for BUSY low, NSS low, clock the command bytes, NSS
//! high. A command with a response is followed (after BUSY low again) by a
//! second transaction clocking out the response, which starts with two
//! status bytes. A FIFO read is opcode `0x0001` followed, in the same
//! transaction, by the bytes to read.
//!
//! Receive only: nothing here can make the chip transmit.

use heapless::Vec;

/// One encoded command (the longest used here is 13 bytes).
pub type Cmd = Vec<u8, 16>;

fn cmd(bytes: &[u8]) -> Cmd {
    // Every caller passes at most 13 bytes.
    Vec::from_slice(bytes).unwrap_or_default()
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

// ---- system (0x01xx) -------------------------------------------------------

pub const GET_STATUS: [u8; 2] = [0x01, 0x00];
pub const GET_VERSION: [u8; 2] = [0x01, 0x01];
pub const GET_ERRORS: [u8; 2] = [0x01, 0x10];
pub const CLEAR_ERRORS: [u8; 2] = [0x01, 0x11];
pub const GET_AND_CLEAR_IRQ: [u8; 2] = [0x01, 0x17];
pub const GET_RX_FIFO_LEVEL: [u8; 2] = [0x01, 0x1C];
pub const CLEAR_RX_FIFO: [u8; 2] = [0x01, 0x1E];
/// ReadRxFifo: this opcode, then the bytes, in one transaction.
pub const READ_RX_FIFO: [u8; 2] = [0x00, 0x01];

/// Response lengths (status bytes included).
pub const VERSION_RSP: usize = 4;
pub const IRQ_RSP: usize = 6;
pub const PKT_LEN_RSP: usize = 4;
pub const PKT_STATUS_RSP: usize = 8;
pub const RSSI_INST_RSP: usize = 4;
pub const REG_RSP: usize = 6;

/// SetDioFunction: DIO 5-11, function, sleep pull.
pub mod dio_func {
    pub const NONE: u8 = 0;
    pub const IRQ: u8 = 1;
    pub const RF_SWITCH: u8 = 2;
}

pub fn set_dio_function(dio: u8, func: u8, pull: u8) -> Cmd {
    cmd(&[0x01, 0x12, dio & 0xF, ((func & 0xF) << 4) | (pull & 0xF)])
}

/// SetDioRfSwitchConfig, receive-only form: the TX bits (`tx_hf`, `tx_lf`)
/// are always 0, so the switch is never put in a transmit position.
pub fn set_dio_rf_switch_rx(dio: u8, rx_hf: bool, rx_lf: bool, standby: bool) -> Cmd {
    let mut v = 0u8;
    if rx_hf {
        v |= 8;
    }
    if rx_lf {
        v |= 2;
    }
    if standby {
        v |= 1;
    }
    cmd(&[0x01, 0x13, dio & 0xF, v])
}

pub fn set_dio_irq_config(dio: u8, irqs: u32) -> Cmd {
    let i = be32(irqs);
    cmd(&[0x01, 0x15, dio & 0xF, i[0], i[1], i[2], i[3]])
}

pub fn clear_irq(irqs: u32) -> Cmd {
    let i = be32(irqs);
    cmd(&[0x01, 0x16, i[0], i[1], i[2], i[3]])
}

/// TCXO supply voltage codes for [`set_tcxo_mode`]: 1.6, 1.7, 1.8, 2.2,
/// 2.4, 2.7, 3.0, 3.3 V.
pub const TCXO_MV: [u16; 8] = [1600, 1700, 1800, 2200, 2400, 2700, 3000, 3300];

/// SetTcxoMode: `voltage` is an index into [`TCXO_MV`], `start_time` in
/// 32.768 kHz ticks.
pub fn set_tcxo_mode(voltage: u8, start_time: u32) -> Cmd {
    let t = be32(start_time);
    cmd(&[0x01, 0x20, voltage & 7, t[0], t[1], t[2], t[3]])
}

/// Calibrate everything a receiver uses (measurement unit, AAF, PLL, HF
/// and LF RC oscillators); the PA offset is a transmitter calibration and
/// is left out. Leaves the chip in Standby RC.
pub fn calibrate_rx() -> Cmd {
    cmd(&[0x01, 0x22, 16 | 8 | 4 | 2 | 1])
}

/// CalibFE with no frequency: front-end calibration (ADC offset, PPF,
/// image) at the current RF frequency. Not in RX or TX.
pub fn calib_fe_current() -> Cmd {
    cmd(&[0x01, 0x23])
}

pub fn set_standby_rc() -> Cmd {
    cmd(&[0x01, 0x28, 0])
}

pub fn set_standby_xosc() -> Cmd {
    cmd(&[0x01, 0x28, 1])
}

// ---- register access -------------------------------------------------------

pub fn write_reg_mem_mask32(addr: u32, mask: u32, data: u32) -> Cmd {
    let a = be32(addr);
    let m = be32(mask);
    let d = be32(data);
    cmd(&[0x01, 0x05, a[1], a[2], a[3], m[0], m[1], m[2], m[3], d[0], d[1], d[2], d[3]])
}

/// ReadRegMem32 of one word; the response is status (2) + the word (4).
pub fn read_reg_mem32(addr: u32) -> Cmd {
    let a = be32(addr);
    cmd(&[0x01, 0x06, a[1], a[2], a[3], 1])
}

// ---- common radio (0x02xx) --------------------------------------------------

pub fn set_rf_frequency(hz: u32) -> Cmd {
    let f = be32(hz);
    cmd(&[0x02, 0x00, f[0], f[1], f[2], f[3]])
}

/// RX path: 0 = sub-GHz (LF), 1 = 2.4 GHz (HF); boost 0-7.
pub fn set_rx_path(hf: bool, boost: u8) -> Cmd {
    cmd(&[0x02, 0x01, hf as u8, boost & 7])
}

pub mod packet_type {
    pub const LORA: u8 = 0;
    pub const FSK_GENERIC: u8 = 1;
    /// The SX126x-compatible FSK packet engine, configured with the
    /// 0x0240-0x0247 commands.
    pub const FSK_LEGACY: u8 = 2;
    pub const FLRC: u8 = 5;
    pub const OOK: u8 = 10;
}

pub fn set_packet_type(t: u8) -> Cmd {
    cmd(&[0x02, 0x07, t])
}

/// SetRx with a timeout in 32.768 kHz ticks; [`RX_CONTINUOUS`] = keep
/// receiving (restart after every packet).
pub fn set_rx(timeout: u32) -> Cmd {
    let t = be32(timeout);
    cmd(&[0x02, 0x0C, t[1], t[2], t[3]])
}
pub const RX_CONTINUOUS: u32 = 0xFF_FFFF;

pub const GET_RSSI_INST: [u8; 2] = [0x02, 0x0B];
pub const GET_RX_PKT_LENGTH: [u8; 2] = [0x02, 0x12];

/// SetAgcGainManual: 0 = automatic gain (what SoftRF's `setGain(0)` asks).
pub fn set_agc_gain(step: u8) -> Cmd {
    cmd(&[0x02, 0x1A, step & 0xF])
}

// ---- FSK (0x024x) ----------------------------------------------------------

pub mod shaping {
    pub const NONE: u8 = 0;
    pub const BT_0_5: u8 = 5;
}

/// RX bandwidth codes (the `RxBw` table of the datasheet).
pub mod bw {
    pub const BW_3076: u8 = 0;
    pub const BW_2222: u8 = 192;
    pub const BW_1333: u8 = 136;
    pub const BW_555: u8 = 216;
    pub const BW_307: u8 = 33;
    pub const BW_153: u8 = 34;
    pub const BW_119: u8 = 83;

    /// (kHz, code), narrowest first: a subset of the datasheet table (as
    /// the `lr2021` crate's `RxBw`), enough to pick a filter for any
    /// control-link or energy-sweep setting.
    pub const TABLE: [(u16, u8); 24] = [
        (48, 28),
        (64, 20),
        (83, 155),
        (96, 27),
        (119, 83),
        (128, 19),
        (153, 34),
        (192, 26),
        (222, 225),
        (256, 18),
        (307, 33),
        (333, 153),
        (384, 25),
        (444, 224),
        (512, 17),
        (555, 216),
        (666, 152),
        (769, 24),
        (888, 144),
        (1111, 200),
        (1333, 136),
        (2222, 192),
        (2666, 128),
        (3076, 0),
    ];

    /// The narrowest filter at least `hz` wide (the widest if none is).
    pub fn at_least(hz: u32) -> u8 {
        TABLE.iter().find(|(k, _)| *k as u32 * 1000 >= hz).map_or(BW_3076, |e| e.1)
    }
}

/// SetFskModulationParams: bitrate in bit/s, shaping, RX bandwidth code,
/// deviation in Hz.
pub fn set_fsk_modulation(bitrate: u32, shaping: u8, rx_bw: u8, fdev: u32) -> Cmd {
    let b = be32(bitrate);
    let f = be32(fdev);
    cmd(&[0x02, 0x40, b[0], b[1], b[2], b[3], shaping & 0xF, rx_bw, f[1], f[2], f[3]])
}

/// SetFskPacketParams for a fixed-length packet: no preamble-length
/// detection, no address filtering, no CRC, no whitening (all done in
/// software, as SoftRF does). `preamble_tx` only matters to a transmitter
/// and is kept for fidelity with SoftRF's configuration.
pub fn set_fsk_packet_fixed(preamble_tx: u16, len: u16) -> Cmd {
    let p = preamble_tx.to_be_bytes();
    let l = len.to_be_bytes();
    cmd(&[0x02, 0x41, p[0], p[1], 0, 0, l[0], l[1], 0])
}

/// SetFskSyncWord: up to 64 bits, sent most significant bit first.
pub fn set_fsk_sync_word(sync: u64, bits: u8) -> Cmd {
    let s = sync.to_be_bytes();
    cmd(&[0x02, 0x44, s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7], 0x80 | (bits & 0x7F)])
}

pub const GET_FSK_PACKET_STATUS: [u8; 2] = [0x02, 0x47];

// ---- LoRa (0x022x), for the control-link scan --------------------------------

/// SetLoraModulationParams: SF 5-12, bandwidth code, coding-rate code,
/// low-data-rate optimisation, filter (0 = auto).
pub fn set_lora_modulation(sf: u8, bw: u8, cr: u8, ldro: bool) -> Cmd {
    cmd(&[0x02, 0x20, ((sf & 0xF) << 4) | (bw & 0xF), ((cr & 0xF) << 4) | ldro as u8])
}

/// SetLoraPacketParams: preamble symbols, payload length (implicit header
/// only), header type, CRC, inverted IQ.
pub fn set_lora_packet(preamble: u16, len: u8, implicit: bool, crc: bool, invert_iq: bool) -> Cmd {
    let p = preamble.to_be_bytes();
    cmd(&[0x02, 0x21, p[0], p[1], len, ((implicit as u8) << 2) | ((crc as u8) << 1) | invert_iq as u8])
}

pub fn set_lora_syncword(sync: u8) -> Cmd {
    cmd(&[0x02, 0x23, sync])
}

pub const GET_LORA_PACKET_STATUS: [u8; 2] = [0x02, 0x2A];

/// LoRa bandwidth codes for [`set_lora_modulation`] (the `lr2021` crate's
/// `LoraBw`).
pub mod lora_bw {
    pub const BW_125: u8 = 4;
    pub const BW_250: u8 = 5;
    pub const BW_500: u8 = 6;
    pub const BW_812: u8 = 15;
}

/// LoRa coding-rate codes for [`set_lora_modulation`] (the `lr2021`
/// crate's `LoraCr`): 1-4 short interleaving 4/5..4/8, 5-7 long
/// interleaving 4/5, 4/6, 4/8 (SX1280's `LI` rates).
pub mod lora_cr {
    pub const CR_4_5: u8 = 1;
    pub const CR_4_6: u8 = 2;
    pub const CR_4_7: u8 = 3;
    pub const CR_4_8: u8 = 4;
    pub const LI_4_5: u8 = 5;
    pub const LI_4_6: u8 = 6;
    pub const LI_4_8: u8 = 7;
}

/// SetLoraSideDetConfig: up to three extra detectors ("multi-SF"), each
/// `(sf << 4) | (ldro << 2) | invert_iq`. An empty list turns them off.
/// Sent after [`set_lora_modulation`] (the `lr2021` crate's
/// `set_lora_sidedet_cfg`).
pub fn set_lora_sidedet_cfg(cfgs: &[u8]) -> Cmd {
    let mut c = cmd(&[0x02, 0x24]);
    for &b in cfgs.iter().take(3) {
        let _ = c.push(b);
    }
    c
}

/// One side-detector configuration byte.
pub const fn sidedet(sf: u8, ldro: bool, invert_iq: bool) -> u8 {
    ((sf & 0xF) << 4) | ((ldro as u8) << 2) | invert_iq as u8
}

/// SetLoraSideDetSyncword, basic format: one sync word per side detector.
pub fn set_lora_sidedet_syncword(words: &[u8]) -> Cmd {
    let mut c = cmd(&[0x02, 0x25]);
    for &b in words.iter().take(3) {
        let _ = c.push(b);
    }
    c
}

/// GetLoraPacketStatus: (packet RSSI dBm, SNR dB, detector bits: 1 main,
/// 2/4/8 side detectors 1-3). Layout as the `lr2021` crate's
/// `LoraPacketStatusRsp`: SNR in quarter dB at byte 4, RSSI half-dB code
/// in byte 5 plus bit 1 of byte 7, detector in bits 2-5 of byte 7.
pub fn parse_lora_status(rsp: &[u8; PKT_STATUS_RSP]) -> (i8, i8, u8) {
    let rssi = rssi_dbm(((rsp[5] as u16) << 1) | ((rsp[7] >> 1) & 1) as u16);
    let snr = (rsp[4] as i8) / 4;
    (rssi, snr, (rsp[7] >> 2) & 0xF)
}

// ---- OOK (0x028x) ----------------------------------------------------------

pub fn set_ook_modulation(bitrate: u32, shaping: u8, rx_bw: u8) -> Cmd {
    let b = be32(bitrate);
    cmd(&[0x02, 0x81, b[0], b[1], b[2], b[3], shaping & 0xF, rx_bw])
}

/// SetOokPacketParams, fixed length, no address, no CRC, NRZ (no chip
/// Manchester: SoftRF decodes the chips in software).
pub fn set_ook_packet_fixed(preamble_tx: u16, len: u16) -> Cmd {
    let p = preamble_tx.to_be_bytes();
    let l = len.to_be_bytes();
    cmd(&[0x02, 0x82, p[0], p[1], 0, l[0], l[1], 0])
}

/// SetOokSyncWord: up to 32 bits, most significant bit first.
pub fn set_ook_sync_word(sync: u32, bits: u8) -> Cmd {
    let s = be32(sync);
    cmd(&[0x02, 0x84, s[0], s[1], s[2], s[3], 0x80 | (bits & 0x7F)])
}

/// SetOokDetector: preamble pattern (sent LSB first), pattern length field,
/// repeats, sync word is raw, start-of-frame delimiter edge and length.
pub fn set_ook_detector(pattern: u16, len_field: u8, repeats: u8, sw_raw: bool, sfd_rising: bool, sfd_len: u8) -> Cmd {
    let p = pattern.to_be_bytes();
    let mut b6 = sfd_len & 0xF;
    if sw_raw {
        b6 |= 32;
    }
    if sfd_rising {
        b6 |= 16;
    }
    cmd(&[0x02, 0x88, p[0], p[1], len_field & 0xF, repeats & 0x1F, b6])
}

pub const GET_OOK_PACKET_STATUS: [u8; 2] = [0x02, 0x87];

/// OOK detection threshold register (bits 26:20), from the `lr2021`
/// crate's `set_ook_thr`.
pub const REG_OOK_DETECT: u32 = 0xF3_0E14;
pub const OOK_THR_MASK: u32 = 0x07F0_0000;
/// SoftRF's "OOK quirk" register (`RADIOLIB_LR2021_REG_OOK_QUIRK`): low byte
/// written to 0x18 for 1090 reception.
pub const REG_OOK_QUIRK: u32 = 0xF3_0150;

/// The threshold write the `lr2021` crate's `set_ook_thr(level)` makes.
pub fn set_ook_threshold(level: i8) -> Cmd {
    write_reg_mem_mask32(REG_OOK_DETECT, OOK_THR_MASK, ((level as i32 as u32) << 20) & OOK_THR_MASK)
}

// ---- IRQ bits ----------------------------------------------------------------

pub mod irq {
    pub const PREAMBLE_DETECTED: u32 = 0x0000_0020;
    pub const SYNC_FAIL: u32 = 0x0000_2000;
    pub const ERROR: u32 = 0x0001_0000;
    pub const RX_DONE: u32 = 0x0004_0000;
    pub const TIMEOUT: u32 = 0x0020_0000;
    pub const CRC_ERROR: u32 = 0x0040_0000;
    pub const LEN_ERROR: u32 = 0x0080_0000;
    pub const ALL: u32 = 0xFFFF_FFFF;
}

// ---- responses -----------------------------------------------------------------

/// Command status bits of the first status byte: 0 fail, 1 permission
/// error, 2 OK, 3 data (for a read). The `lr2021` crate's `CmdStatus`.
pub fn cmd_status(rsp: &[u8]) -> u8 {
    rsp.first().map_or(0, |b| (b >> 1) & 7)
}

/// GetVersion: (major, minor). SoftRF accepts major 1 with minor >= 0x18
/// (LR2021) or 2.0 (LR2012/22).
pub fn parse_version(rsp: &[u8; VERSION_RSP]) -> (u8, u8) {
    (rsp[2], rsp[3])
}

pub fn version_ok(major: u8, minor: u8) -> bool {
    (major == 1 && minor >= 0x18) || (major == 2 && minor == 0)
}

pub fn parse_irq(rsp: &[u8; IRQ_RSP]) -> u32 {
    u32::from_be_bytes([rsp[2], rsp[3], rsp[4], rsp[5]])
}

pub fn parse_pkt_len(rsp: &[u8; PKT_LEN_RSP]) -> u16 {
    u16::from_be_bytes([rsp[2], rsp[3]])
}

/// Half-dB RSSI code (actual = -code/2 dBm) to whole dBm, saturating.
pub fn rssi_dbm(code: u16) -> i8 {
    (-((code as i32 + 1) / 2)).max(-128) as i8
}

/// GetOokPacketStatus: RSSI of the high level (the pulses), dBm.
pub fn parse_ook_rssi(rsp: &[u8; PKT_STATUS_RSP]) -> i8 {
    rssi_dbm(((rsp[5] as u16) << 1) | (rsp[6] & 1) as u16)
}

/// GetFskPacketStatus: RSSI latched at the sync word, dBm.
pub fn parse_fsk_rssi(rsp: &[u8; PKT_STATUS_RSP]) -> i8 {
    rssi_dbm(((rsp[5] as u16) << 1) | (rsp[6] & 1) as u16)
}

/// GetRssiInst: instantaneous RSSI, dBm.
pub fn parse_rssi_inst(rsp: &[u8; RSSI_INST_RSP]) -> i8 {
    rssi_dbm(((rsp[2] as u16) << 1) | (rsp[3] & 1) as u16)
}

pub fn parse_reg(rsp: &[u8; REG_RSP]) -> u32 {
    u32::from_be_bytes([rsp[2], rsp[3], rsp[4], rsp[5]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        assert_eq!(set_rf_frequency(1_090_000_000).as_slice(), [0x02, 0x00, 0x40, 0xF8, 0x14, 0x80]);
        assert_eq!(set_rf_frequency(915_000_000).as_slice(), [0x02, 0x00, 0x36, 0x89, 0xCA, 0xC0]);
        // SoftRF's quirk write: lr2021_writeRegMemMask32(0xF30150, 0xFF, 0x18).
        assert_eq!(
            write_reg_mem_mask32(REG_OOK_QUIRK, 0xFF, 0x18).as_slice(),
            [0x01, 0x05, 0xF3, 0x01, 0x50, 0, 0, 0, 0xFF, 0, 0, 0, 0x18]
        );
        // SoftRF's lr2021_readRegMem32: opcode, 3 address bytes, count 1.
        assert_eq!(read_reg_mem32(0xF3_0844).as_slice(), [0x01, 0x06, 0xF3, 0x08, 0x44, 1]);
        assert_eq!(set_rx(RX_CONTINUOUS).as_slice(), [0x02, 0x0C, 0xFF, 0xFF, 0xFF]);
        assert_eq!(set_ook_sync_word(0x95, 8).as_slice(), [0x02, 0x84, 0, 0, 0, 0x95, 0x88]);
        assert_eq!(set_ook_detector(0x0285, 15, 0, false, false, 0).as_slice(), [0x02, 0x88, 0x02, 0x85, 15, 0, 0]);
        assert_eq!(set_fsk_sync_word(0xACDD_A4E2, 32).as_slice(), [0x02, 0x44, 0, 0, 0, 0, 0xAC, 0xDD, 0xA4, 0xE2, 0xA0]);
        assert_eq!(
            set_fsk_modulation(100_000, shaping::BT_0_5, bw::BW_307, 50_000).as_slice(),
            [0x02, 0x40, 0x00, 0x01, 0x86, 0xA0, 5, 33, 0x00, 0xC3, 0x50]
        );
        assert_eq!(set_fsk_packet_fixed(8, 52).as_slice(), [0x02, 0x41, 0, 8, 0, 0, 0, 52, 0]);
        assert_eq!(set_dio_irq_config(8, irq::RX_DONE).as_slice(), [0x01, 0x15, 8, 0x00, 0x04, 0x00, 0x00]);
        assert_eq!(set_lora_modulation(7, 5, 1, false).as_slice(), [0x02, 0x20, 0x75, 0x10]);
        assert_eq!(set_lora_packet(8, 0, false, true, false).as_slice(), [0x02, 0x21, 0, 8, 0, 2]);
        assert_eq!(calibrate_rx().as_slice(), [0x01, 0x22, 0x1F]);
        // The lr2021 crate's set_lora_sidedet_cfg for SF7 and SF8, no LDRO,
        // normal IQ; SF6 inverted.
        assert_eq!(set_lora_sidedet_cfg(&[sidedet(7, false, false), sidedet(8, false, false)]).as_slice(), [0x02, 0x24, 0x70, 0x80]);
        assert_eq!(sidedet(6, true, true), 0x65);
        assert_eq!(set_lora_sidedet_cfg(&[]).as_slice(), [0x02, 0x24]);
        assert_eq!(set_lora_sidedet_syncword(&[0x12, 0x12]).as_slice(), [0x02, 0x25, 0x12, 0x12]);
        // SF6 BW500 CR4/7: ELRS 900 200 Hz.
        assert_eq!(set_lora_modulation(6, lora_bw::BW_500, lora_cr::CR_4_7, false).as_slice(), [0x02, 0x20, 0x66, 0x30]);
        assert_eq!(set_lora_modulation(7, lora_bw::BW_812, lora_cr::LI_4_6, false).as_slice(), [0x02, 0x20, 0x7F, 0x60]);
        assert_eq!(bw::at_least(25_000), 28);
        assert_eq!(bw::at_least(250_000), 18);
        assert_eq!(bw::at_least(1_000_000), 200);
        assert_eq!(bw::at_least(5_000_000), bw::BW_3076);
        assert!(bw::TABLE.windows(2).all(|w| w[0].0 < w[1].0));
    }

    /// No transmit opcode can come out of this module: SetTx (0x020D),
    /// SetTxParams (0x0203), SetPaConfig (0x0202), SelPa (0x020F), TX test
    /// (0x020E), auto RX/TX (0x0211), CAD with exit to TX (0x021B) and the TX
    /// FIFO write (0x0002) are absent, and the RF switch never gets a TX bit.
    #[test]
    fn receive_only() {
        let src = include_str!("cmd.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for op in ["0x02, 0x0D", "0x02, 0x03", "0x02, 0x02,", "0x02, 0x0F", "0x02, 0x0E", "0x02, 0x11", "0x02, 0x1B", "[0x00, 0x02"] {
            assert!(!code.contains(op), "transmit opcode {op} present");
        }
        for dio in 5..=11 {
            for b in 0..8u8 {
                let c = set_dio_rf_switch_rx(dio, b & 4 != 0, b & 2 != 0, b & 1 != 0);
                assert_eq!(c[3] & (16 | 4), 0);
            }
        }
    }

    #[test]
    fn responses() {
        assert_eq!(parse_version(&[0x04, 0x00, 0x01, 0x18]), (1, 0x18));
        assert!(version_ok(1, 0x18) && version_ok(1, 0x20) && version_ok(2, 0) && !version_ok(1, 0x17) && !version_ok(0xFF, 0xFF));
        assert_eq!(parse_irq(&[0, 0, 0x00, 0x04, 0x00, 0x20]), irq::RX_DONE | irq::PREAMBLE_DETECTED);
        assert_eq!(parse_pkt_len(&[0, 0, 0, 27]), 27);
        // 160 half-dB = -80 dBm; 161 = -80.5 -> -81.
        assert_eq!(parse_ook_rssi(&[0, 0, 0, 0, 0, 80, 0, 0]), -80);
        assert_eq!(parse_fsk_rssi(&[0, 0, 0, 0, 0, 80, 1, 0]), -81);
        assert_eq!(parse_rssi_inst(&[0, 0, 0xFF, 1]), -128);
        assert_eq!(cmd_status(&[0x04, 0]), 2);
        assert_eq!(set_ook_threshold(-80).as_slice()[9..], [0x03, 0x00, 0x00, 0x00]);
        // SNR -6 dB (-24 quarter dB), RSSI 181 half-dB = -90.5 -> -91, side
        // detector 2 (bit 2 of the detector field = 4 << 2).
        assert_eq!(parse_lora_status(&[0, 0, 0, 8, (-24i8) as u8, 90, 0, (4 << 2) | 2]), (-91, -6, 4));
    }
}
