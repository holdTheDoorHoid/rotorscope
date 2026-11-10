//! Driving the C5's PHY by hand, receive-only.
//!
//! The Wi-Fi driver is started only to power, clock and calibrate the radio
//! (as C5VRX and ESPARGOS esp-sdr both do). Then we take the receiver over:
//! stop the MAC's transmit queues, disable the AGC, force a fixed gain, set
//! the analog low-pass and tune with the PHY library's own channel setup.
//! Nothing here can make the radio transmit; [`lock_rx_only`] makes sure the
//! MAC cannot either.
//!
//! # ABI evidence
//!
//! Every function below is private to Espressif's closed `libphy.a` /
//! `libpp.a`; there is no header. Each signature was taken from (a) callers
//! in GPL source that run on this exact blob, and (b) a disassembly of the
//! blob we link (`esp-wifi-sys-esp32c5` 0.3.0, `libphy.a` SHA-256
//! `dbf33c41...04fffb`, version string `edb400d6`, built Mar 10 2026, which is
//! also the blob C5VRX pins). `build.rs` refuses any other blob and
//! [`abi_ok`] re-checks the version string at run time; if either changes,
//! re-derive every signature before trusting a register write here.
//!
//! RISC-V passes the first arguments in a0..a7, so an extra declared
//! argument is harmless garbage but a missing or reordered one is not. Each
//! entry notes which argument registers the disassembly actually reads.
//!
//! None of the code here is copied from C5VRX (GPL-3.0-only): the register
//! addresses and call order are facts read from it and from esp-sdr /
//! esp-sdr-zephyr (GPL-3.0-or-later), re-implemented independently.

use core::ffi::{CStr, c_char};

/// The libphy version this ABI was checked against (`get_phy_version_str`
/// prints `<rf_cal>,<this>,<date>,<time>`).
pub const PHY_VERSION: &str = "edb400d6";

unsafe extern "C" {
    /// `phy_rfpll.o`: full channel setup at any whole MHz above 14 (5 GHz
    /// path from about 3000 MHz up). Disassembly: a0 compared with 3001
    /// (band flag), a1 masked to a byte (`andi a1,a1,0xff`) and passed on,
    /// returns 0. esp-sdr `targets/esp32c5/tuning.h`: `phy_set_chanfreq(unsigned
    /// mhz, unsigned mode)`; esp-sdr-zephyr: "mode 1 the wide RX path", and
    /// `rx_bandwidth_phy_mode()` picks mode 0 for widths <= 23 MHz.
    fn phy_set_chanfreq(mhz: u32, mode: u32) -> i32;
    /// `phy_rfpll.o`: fine LO offset. Disassembly: one argument, rounded to a
    /// multiple of 4 (`(a0+2) & ~3`) and stored as an i16 in `phy_param`.
    /// C5VRX `main/rf.c` calls it with offsets in kHz (+-2500, its AFC).
    fn phy_chip_set_chan_offset(offset_khz: i32);
    /// `phy_reg.o`: writes `0x600a702c`: bit 23 = force (from a0), bits 31:24
    /// = gain index (from a1). C5VRX: `phy_force_rx_gain(bool, uint8_t)`.
    fn phy_force_rx_gain(enable: u32, gain: u32);
    /// `phy_reg.o`: sets bit 29 of `0x600a7030`. No arguments.
    fn phy_disable_agc();
    /// `phy_reg.o`: clears `0x600a705c` and the RF AGC fields of
    /// `0x600a284c`. No arguments.
    fn phy_rfagc_disable();
    /// `phy_reg.o`: baseband filter width select, a0 != 0 = 40 MHz.
    /// C5VRX calls it before every channel setup ("the analog filter branch
    /// follows the PHY bandwidth mode at the last channel setup").
    fn phy_wifi_fbw_sel(bw40: u32);
    /// `phy_i2c.o`: analog I2C read (block, host, reg) -> u8 (zero-extended).
    fn phy_chip_i2c_readReg(block: u32, host: u32, reg: u32) -> u32;
    /// `phy_i2c.o`: analog I2C write; the disassembly drops a1 (host) and
    /// passes (block, reg, value) on. Takes the PHY's own I2C lock.
    fn phy_i2c_writeReg(block: u32, host: u32, reg: u32, value: u32);
    /// `phy_pbus.o`: transmit chain power down. No arguments.
    fn phy_pbus_xpd_tx_off();
    /// `phy_pbus.o`: receive chain power up; a0 = enable.
    fn phy_pbus_xpd_rx_on(enable: u32);
    /// `phy_reg.o`: RX clock enable bits of `0x600a0890`; a0 = enable.
    fn phy_set_rxclk_en(enable: u32);
    /// `phy_debug.o`: sprintf's and returns a static version string.
    fn get_phy_version_str() -> *const c_char;
    /// `libpp.a lmac.o`: stops the MAC's hardware TX queues. Reads no
    /// argument registers (it loads its own), result unused.
    fn lmac_stop_hw_txq();
}

// ------------------------------------------------------------- registers --

/// AGC force: bit 23 force, 31:24 forced index, 14:8 largest calibrated
/// index (esp-sdr-zephyr `esp_sdr_priv.h`, `AGC_FORCE_REG` for the C5).
const AGC_FORCE: usize = 0x600a_702c;
/// Bit 29 set = AGC disabled (`phy_disable_agc` disassembly).
const AGC_CTRL: usize = 0x600a_7030;
/// MAC TX queue configs, queue q at `TXQ0 - 0x10 * q`, bit 31 = enabled
/// (C5VRX `main/rf.c` `lock_rx_only`).
const MAC_TXQ0_CONF: usize = 0x600a_4d6c;
const MAC_TXQ_COUNT: usize = 5;
const MAC_TXQ_ENABLE: u32 = 1 << 31;

/// Analog baseband (BBTOP) I2C block; regs 6/7 hold the I and Q low-pass
/// capacitor codes in bits 5:0 (esp-sdr `rx_bandwidth.h`: "C5: BBTOP 6/7";
/// C5VRX `phy.h`: "BBTOP 0x67 regs 6/7 hold the RX0 capacitor code").
const BBTOP_BLOCK: u32 = 0x67;
const BBTOP_HOST: u32 = 1;
const LPF_REG: u32 = 6;

/// Low-pass code for both capture paths: about 20 MHz full width in PHY
/// mode 0 (esp-sdr's C5 table: {12, 20 MHz}). That stops what would alias
/// at 40 MS/s (debug bus every other sample) and at 20 MS/s (Path A), and
/// keeps the 9 MHz DroneID burst at +0.5 MHz well inside.
pub const LPF_DEFAULT: u8 = 12;
/// Default forced gain. esp-sdr's index while the AGC rules is 40; the C5's
/// largest calibrated index is 77-89 depending on band. The bring-up
/// checklist sets it from `level`.
pub const GAIN_DEFAULT: u8 = 40;

#[inline(always)]
fn rd(a: usize) -> u32 {
    // SAFETY: a fixed, always-mapped modem register.
    unsafe { (a as *const u32).read_volatile() }
}
#[inline(always)]
fn wr(a: usize, v: u32) {
    // SAFETY: as `rd`.
    unsafe { (a as *mut u32).write_volatile(v) }
}

/// What the linked blob says it is.
pub fn version() -> &'static str {
    // SAFETY: the blob returns a pointer to its own static, NUL-terminated buffer.
    let p = unsafe { get_phy_version_str() };
    if p.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
}

/// The run-time half of the ABI guard (build.rs is the other).
pub fn abi_ok() -> bool {
    version().split(',').nth(1) == Some(PHY_VERSION)
}

/// Stop and lock the MAC's transmit queues; true if all read back off.
pub fn lock_rx_only() -> bool {
    // SAFETY: no arguments; the Wi-Fi driver is running.
    unsafe { lmac_stop_hw_txq() };
    for q in 0..MAC_TXQ_COUNT {
        let a = MAC_TXQ0_CONF - 0x10 * q;
        wr(a, rd(a) & !MAC_TXQ_ENABLE);
    }
    (0..MAC_TXQ_COUNT).all(|q| rd(MAC_TXQ0_CONF - 0x10 * q) & MAC_TXQ_ENABLE == 0)
}

/// Receiver settings we own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tune {
    pub mhz: u16,
    pub gain: u8,
    /// Low-pass capacitor code, or `None` for the PHY's own calibration.
    pub lpf: Option<u8>,
}

/// What the hardware reports after [`apply`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Readback {
    pub gain_forced: bool,
    pub gain: u8,
    pub gain_max: u8,
    pub agc_off: bool,
    pub lpf: [u8; 2],
    pub txq_off: bool,
}

pub fn readback() -> Readback {
    let g = rd(AGC_FORCE);
    let lpf = |r| {
        // SAFETY: analog I2C read; the blob takes its own lock.
        (unsafe { phy_chip_i2c_readReg(BBTOP_BLOCK, BBTOP_HOST, r) } & 0x3f) as u8
    };
    Readback {
        gain_forced: g & (1 << 23) != 0,
        gain: (g >> 24) as u8,
        gain_max: ((g >> 8) & 0x7f) as u8,
        agc_off: rd(AGC_CTRL) & (1 << 29) != 0,
        lpf: [lpf(LPF_REG), lpf(LPF_REG + 1)],
        txq_off: (0..MAC_TXQ_COUNT).all(|q| rd(MAC_TXQ0_CONF - 0x10 * q) & MAC_TXQ_ENABLE == 0),
    }
}

/// Force gain and AGC off again, cheaply. The forced gain is occasionally
/// lost (esp-sdr-zephyr measured it after thousands of transmissions; the
/// vendor's channel helpers re-enable the AGC), so the capture loop calls
/// this between captures. Returns true if it had been lost.
pub fn reassert(gain: u8) -> bool {
    let g = rd(AGC_FORCE);
    let lost = g & (1 << 23) == 0 || (g >> 24) as u8 != gain || rd(AGC_CTRL) & (1 << 29) == 0;
    if lost {
        // SAFETY: see the extern declarations.
        unsafe {
            phy_disable_agc();
            phy_rfagc_disable();
            phy_force_rx_gain(1, gain as u32);
        }
    }
    lost
}

/// Tune and lock the receiver. Order (from C5VRX `restore_lock` and esp-sdr
/// `prepare_rx`): bandwidth mode, channel setup (which reloads gain tables
/// and the filter), then RX power and clock, AGC off, forced gain, filter.
pub fn apply(t: Tune) -> Readback {
    // SAFETY: see the extern declarations; the Wi-Fi driver is up.
    unsafe {
        phy_wifi_fbw_sel(0);
        phy_set_chanfreq(t.mhz as u32, 0);
        phy_pbus_xpd_tx_off();
        phy_pbus_xpd_rx_on(1);
        phy_set_rxclk_en(1);
        phy_disable_agc();
        phy_rfagc_disable();
        phy_force_rx_gain(1, t.gain as u32);
    }
    if let Some(code) = t.lpf {
        for r in [LPF_REG, LPF_REG + 1] {
            // SAFETY: analog I2C read-modify-write of the capacitor code only.
            unsafe {
                let v = phy_chip_i2c_readReg(BBTOP_BLOCK, BBTOP_HOST, r) & 0xff;
                phy_i2c_writeReg(BBTOP_BLOCK, BBTOP_HOST, r, (v & !0x3f) | (code as u32 & 0x3f));
            }
        }
    }
    let _ = lock_rx_only();
    readback()
}

/// Fine LO offset in kHz (0 = none). Re-locks afterwards: C5VRX notes the
/// vendor helper re-enables the baseband AGC.
pub fn set_offset_khz(khz: i32, gain: u8) {
    // SAFETY: see the extern declarations.
    unsafe { phy_chip_set_chan_offset(khz) };
    let _ = reassert(gain);
}
