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

    // ---- receive DC-offset calibration (see [`dc_recal`]) ----

    /// `phy_init.o`: the PHY's calibration state, 0x438 bytes (`nm -S`).
    /// Offsets used here, each read from the disassembly of `phy_bb_init`
    /// and `phy_cal_param_track` (the PHY's own temperature re-cal):
    /// 0x2a band byte (1 = 5 GHz), 0x94 calibration flags (0x80 = RX DC
    /// measured, 0x400 = RX IQ measured), RX DC tables 0x138/0x380 and
    /// 0x15c/0x388 (2.4 GHz) and 0x180/0x390 (5 GHz).
    static mut phy_param: [u8; 0x438];
    /// `phy_rx_cal.o`: measures the receive DC offset for every RF gain
    /// step and stores it in (`rf_dc`, `bb_dc`). Disassembly: a0 (table
    /// variant) and a2/a3 passed on to `phy_rfrx_rxdc_cal_new`; a1 != 0 = 9
    /// RF gain steps (else 7). On 5 GHz it visits seven reference LOs (5210,
    /// 5290, 5530, 5610, 5690, 5775, 5855 MHz, `.rodata+0x38`) through
    /// `phy_chip_set_chan_ana`, on 2.4 GHz one (2432). Its callees are RX
    /// only: pbus test-mode forces, `phy_pbus_set_rxgain`, the DCO DAC
    /// search (`phy_rx_dco_cal_1step_new`), the DC estimator, and the
    /// digital TX *clock* gate `phy_set_txclk_en` (0x600a0890 bits 17:16,
    /// on then off). Nothing powers the TX chain or starts a tone.
    fn phy_set_rx_gain_cal_dc(variant: u32, all_steps: u32, rf_dc: *mut u8, bb_dc: *mut u8);
    /// `phy_rx_cal.o`: 2.4 GHz ADC-rate DC step, no arguments; same RX-only
    /// callees (`phy_adc_rate_set`, `phy_dc_iq_est_new`, `phy_pbus_set_dco`).
    fn phy_adc_rate_cal_rxdc();
    /// `phy_rx_gain.o`: rebuilds the 90-entry RX gain memory (0x600a0844..
    /// 0x600a0850, `phy_write_gain_mem`) for `mhz`; each entry carries the
    /// RF and baseband DC trims interpolated from the tables above.
    /// `phy_chip_set_chan` calls it as (mhz, 0).
    fn phy_set_rx_gain_table(mhz: u32, mode: u32);
    /// The original `phy_chip_set_chan_ana` (`phy_rfpll.o`), reached through
    /// `--wrap` (build.rs). One argument, the LO in MHz, passed on to
    /// `phy_set_channel_rfpll_freq_new`.
    fn __real_phy_chip_set_chan_ana(mhz: u32);
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
/// Default forced gain, from the bring-up gain sweep (BRINGUP-RESULTS.md,
/// step 6, after the DC fix): at 5756 MHz index 83 (`gain_max` on 5 GHz)
/// puts the noise at 43-51 codes RMS on two boards, the nearest any index
/// gets to the clip window's 48-80, with DC within a few tens of codes. The
/// gain steps are about 1 dB (RMS 20 at 75, 32 at 79, 48 at 83).
pub const GAIN_DEFAULT: u8 = 83;

// ------------------------------------------------- receive DC calibration --

/// Band byte, flags word and table offsets inside `phy_param` (see the
/// extern block).
const PP_BAND: usize = 0x2a;
const PP_FLAGS: usize = 0x94;
const PP_DC_24: [(usize, usize); 2] = [(0x138, 0x380), (0x15c, 0x388)];
const PP_DC_5: (usize, usize) = (0x180, 0x390);
/// `phy_cal_param_track` clears 0x281 before re-measuring and `phy_bb_init`
/// sets 0x80 after. 0x400 (IQ valid) is left alone: the loopback IQ step
/// (`phy_set_rx_gain_cal_iq`, which does send a tone) is reached only from
/// `phy_bb_init` and `phy_cal_param_track`, neither of which we call.
const FLAG_RXDC: u32 = 0x80;
const FLAG_EXPIRE: u32 = 0x280;
const FLAG_IQ: u32 = 0x400;

/// The LO the DC measurement must use instead of the PHY's reference
/// channels; 0 = pass through. Only [`dc_recal`] sets it, synchronously.
static MEASURE_MHZ: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Every `phy_chip_set_chan_ana` call goes through here (`--wrap`, build.rs).
/// Outside [`dc_recal`] it passes the request through unchanged.
#[unsafe(no_mangle)]
extern "C" fn __wrap_phy_chip_set_chan_ana(mhz: u32) {
    let m = MEASURE_MHZ.load(core::sync::atomic::Ordering::Relaxed);
    // SAFETY: the original, with its own argument or our LO.
    unsafe { __real_phy_chip_set_chan_ana(if m != 0 { m } else { mhz }) }
}

fn pp() -> *mut u8 {
    core::ptr::addr_of_mut!(phy_param) as *mut u8
}
fn pp_u32(off: usize) -> u32 {
    // SAFETY: inside the 0x438-byte object, 4-aligned offsets only.
    unsafe { (pp().add(off) as *const u32).read_volatile() }
}
fn pp_set_u32(off: usize, v: u32) {
    // SAFETY: as `pp_u32`.
    unsafe { (pp().add(off) as *mut u32).write_volatile(v) }
}

/// Result of the last receive DC calibration.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcCal {
    /// Calibrations run since boot (ours, not the PHY's boot one).
    pub n: u32,
    /// LO it was measured at; 0 = the PHY's reference channels (`dcal ref`).
    pub mhz: u16,
    /// 5 GHz tables (else 2.4 GHz).
    pub band5: bool,
    /// Duration in microseconds.
    pub us: u32,
    /// `phy_param` flag 0x80 (RX DC measured) after the run.
    pub valid: bool,
    /// IQ flag still set (the loopback IQ step was not, and will not be, run).
    pub iq_kept: bool,
    /// First RF DC table row of the active band: (I, Q) trim per RF gain step.
    pub rf_dc: [[u16; 2]; 9],
}

/// Re-measure the receiver's DC offset at `mhz` (or, with `mhz` = 0, at the
/// PHY's own reference channels, as at boot) and rebuild the gain memory.
///
/// Why: every gain-memory entry carries analog DC trims, measured once at
/// boot at seven 5 GHz reference LOs (one at 2.4 GHz) and interpolated at
/// channel setup. Forcing a gain selects an entry but nothing re-measures
/// it; at high gain the residual rails the ADC (BRINGUP-RESULTS step 6).
/// The sequence follows ESPARGOS esp-sdr `rx_recalibrate_locked` (C5) and
/// the PHY's own `phy_cal_param_track`, minus the latter's loopback IQ step.
///
/// Receive-only: see the extern notes. The manual gain is released for the
/// measurement (it sweeps its own gains); the caller re-applies [`apply`].
pub fn dc_recal(mhz: u16, n: u32) -> DcCal {
    let t0 = esp_hal::time::Instant::now();
    // The measurement's gain sweep must not be overridden by our force.
    wr(AGC_FORCE, rd(AGC_FORCE) & !(1 << 23));
    let flags = pp_u32(PP_FLAGS);
    MEASURE_MHZ.store(mhz as u32, core::sync::atomic::Ordering::Relaxed);
    // SAFETY: see the extern declarations; PHY up, nothing else drives it
    // (esp-radio runs no PHY tracking task: no caller of
    // `phy_param_track_tot` in any linked archive).
    unsafe {
        if mhz != 0 {
            __real_phy_chip_set_chan_ana(mhz as u32);
        }
        pp_set_u32(PP_FLAGS, flags & !FLAG_EXPIRE);
        let band5 = *pp().add(PP_BAND) != 0;
        if band5 {
            phy_set_rx_gain_cal_dc(0, 1, pp().add(PP_DC_5.0), pp().add(PP_DC_5.1));
        } else {
            phy_set_rx_gain_cal_dc(0, 1, pp().add(PP_DC_24[0].0), pp().add(PP_DC_24[0].1));
            phy_adc_rate_cal_rxdc();
            phy_set_rx_gain_cal_dc(1, 1, pp().add(PP_DC_24[1].0), pp().add(PP_DC_24[1].1));
        }
        pp_set_u32(PP_FLAGS, pp_u32(PP_FLAGS) | FLAG_RXDC);
        MEASURE_MHZ.store(0, core::sync::atomic::Ordering::Relaxed);
        if mhz != 0 {
            phy_set_rx_gain_table(mhz as u32, 0);
        }
    }
    let us = t0.elapsed().as_micros() as u32;
    let mut c = dc_readback();
    c.n = n;
    c.mhz = mhz;
    c.us = us;
    c
}

/// The DC calibration state as `phy_param` holds it now (n/mhz/us zero).
pub fn dc_readback() -> DcCal {
    let flags = pp_u32(PP_FLAGS);
    // SAFETY: byte read inside the object.
    let band5 = unsafe { *pp().add(PP_BAND) } != 0;
    let base = if band5 { PP_DC_5.0 } else { PP_DC_24[0].0 };
    let mut rf_dc = [[0u16; 2]; 9];
    for (k, e) in rf_dc.iter_mut().enumerate() {
        // SAFETY: 9 x 4 bytes from a table start, inside the object.
        unsafe {
            let p = pp().add(base + 4 * k) as *const u16;
            *e = [p.read_volatile(), p.add(1).read_volatile()];
        }
    }
    DcCal { band5, valid: flags & FLAG_RXDC != 0, iq_kept: flags & FLAG_IQ != 0, rf_dc, ..DcCal::default() }
}

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
