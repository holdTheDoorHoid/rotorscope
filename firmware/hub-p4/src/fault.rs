//! Crash recovery on the P4 hub (docs/BENCH.md "Crash recovery"): reboot
//! after a panic, a hang watchdog fed by the main loop, and why the chip last
//! reset.
//!
//! The patched esp-hal (firmware/patches/esp-hal-p4-pre-v3) has a P4 port of
//! the RTC watchdog driver (`rtc_cntl::Rwdt`, LP_WDT) and of
//! `system::software_reset` (which parks core 1 first, as IDF's
//! `esp_restart` does); both are used here as they are. Neither had run on
//! this chip before this change, so the timeout is checked twice before the
//! watchdog is trusted (see [`arm`]): a watchdog that fires too early means a
//! reboot loop, which is worse than no watchdog.
//!
//! Register facts used directly (read-only diagnostics), each from the
//! esp32p4 PAC the HAL links (esp-pacs rev 5a07030, `src/lib.rs` base
//! addresses and the per-peripheral register offsets) and the same in ESP-IDF
//! release/v5.4 (`soc/esp32p4/register/soc/reg_base.h`, `lp_wdt_reg.h`,
//! `lp_clkrst_reg.h`, `lp_system_reg.h`), which supports pre-v3 silicon:
//! - LP_SYS base 0x5011_0000; LP_STORE1 at +0x30: the calibrated RTC slow
//!   clock period, microseconds with 19 fraction bits, written by esp-hal's
//!   clock init (`clock::calibrate_rtc_slow_clock`) and read by the RWDT
//!   driver to turn seconds into slow-clock ticks.
//! - LP_AON_CLKRST (IDF: LP_CLKRST) base 0x5011_1000; RESET_CAUSE at +0x10,
//!   bits [12:7] = HP core 0 reset cause (the ROM's `rtc_get_reset_reason`
//!   codes, `SocResetReason`).
//! - LP_WDT base 0x5011_6000; CONFIG1 at +0x04 = stage 0 hold (ticks, before
//!   the eFuse `WDT_DELAY_SEL` multiplier). IDF's `lpwdt_ll_config_stage`
//!   writes `ticks >> (1 + WDT_DELAY_SEL)` there, as the HAL does.

use esp_hal::rtc_cntl::{Rtc, RwdtStage};
use esp_hal::time::{Duration, Instant};

/// Hang watchdog timeout. The main loop goes round every ~7 ms (the screen
/// band); the longest legitimate stops measured or estimated on this hub:
/// SD mount 93-166 ms (BRINGUP.md step 14) plus up to ~200 ms to count the
/// previous log (16 KB at ~6 ms a block); a flush with read-back 37 ms; the
/// airspace pack's check when its last chunk arrives (a bitwise CRC-32 over
/// up to 8 MB in PSRAM, ~1-2 s at 360 MHz; the pack itself is copied in 1 KB
/// steps). 30 s is more than ten times the worst of these, and still turns a
/// frozen hub into a working one within half a minute. `benchlog`'s
/// whole-file count feeds as it reads (`sd::bench_count`).
pub const WATCHDOG: Duration = Duration::from_secs(30);

const LP_SYS_STORE1: *const u32 = 0x5011_0030 as *const u32;
const LP_CLKRST_RESET_CAUSE: *const u32 = 0x5011_1010 as *const u32;
const LP_WDT_CONFIG1: *const u32 = 0x5011_6004 as *const u32;
/// `RtcClock::CAL_FRACT` in the HAL.
const CAL_FRACT: u32 = 19;

fn rtc() -> Rtc<'static> {
    // SAFETY: `main` never takes LPWR for itself (only `esp_hal::init` does,
    // briefly, to turn the watchdogs off); `Rtc::new` only wraps the token,
    // and each use here is a short register access from the main loop.
    Rtc::new(unsafe { esp_hal::peripherals::LPWR::steal() })
}

/// The calibrated slow clock period, microseconds (0.0 if unset).
fn slow_period_us() -> f32 {
    // SAFETY: read-only access to an always-on LP register (see module docs).
    let raw = unsafe { LP_SYS_STORE1.read_volatile() };
    raw as f32 / (1u32 << CAL_FRACT) as f32
}

/// Turn the hang watchdog on, if the numbers it would run on make sense.
/// Returns what to print: the effective timeout read back from the chip, or
/// why it was left off.
///
/// Two checks guard against a reboot loop from a wrong conversion: the slow
/// clock period must be plausible (RC_SLOW ~136 kHz = ~7.4 us, a 32 kHz
/// crystal ~30.5 us; anything outside 2..=100 us means the calibration did
/// not run), and the timeout read back from LP_WDT CONFIG1 must come out
/// within a factor of two of [`WATCHDOG`]. Otherwise the watchdog stays off.
pub fn arm() -> Result<f32, (&'static str, f32)> {
    let period = slow_period_us();
    if !(2.0..=100.0).contains(&period) {
        return Err(("slow clock calibration missing or implausible", period));
    }
    let mut rtc = rtc();
    rtc.rwdt.set_timeout(RwdtStage::Stage0, WATCHDOG);
    // SAFETY: read-only access to LP_WDT CONFIG1 (see module docs).
    let hold = unsafe { LP_WDT_CONFIG1.read_volatile() };
    let ticks = (hold as u64) << (1 + u32::from(esp_hal::efuse::rwdt_multiplier()));
    let secs = ticks as f32 * period / 1e6;
    let want = WATCHDOG.as_secs() as f32;
    if !(want / 2.0..=want * 2.0).contains(&secs) {
        return Err(("timeout read back from LP_WDT does not match", secs));
    }
    rtc.rwdt.enable();
    rtc.rwdt.feed();
    Ok(secs)
}

/// Restart the hang watchdog's count. A few register writes; main loop only.
#[inline]
pub fn feed() {
    rtc().rwdt.feed();
}

/// After a panic (message and backtrace already printed on UART0 by
/// esp-backtrace): reset the chip. A panic in the first seconds after boot
/// is likely to repeat on every boot (no PSRAM, an assert at start-up), so
/// it waits longer there, which keeps such a loop slow and its message
/// readable instead of a blur of boots.
pub fn reboot_after_panic() -> ! {
    let up = Instant::now().duration_since_epoch();
    let wait_ms = if up < Duration::from_secs(10) { 5_000 } else { 500 };
    esp_hal::delay::Delay::new().delay_millis(wait_ms);
    esp_hal::system::software_reset()
}

/// Why the chip last reset, as one word (same words as the nodes,
/// `node_common::fault::name`): `power_on`, `software` (after a panic, or
/// any other restart by the firmware), `watchdog`, `brownout`, `usb`,
/// `deep_sleep` or `other`.
pub fn reset_reason() -> &'static str {
    match reset_code() {
        0x01 => "power_on",
        0x03 | 0x0C => "software",
        0x07 | 0x08 | 0x0B | 0x11 | 0x09 | 0x0D | 0x10 | 0x12 => "watchdog",
        0x0F => "brownout",
        0x05 => "deep_sleep",
        0x15 | 0x16 => "usb",
        _ => "other",
    }
}

/// The raw cause code (`SocResetReason` as a number), for the boot line.
pub fn reset_code() -> u32 {
    // SAFETY: read-only access to LP_CLKRST RESET_CAUSE (see module docs).
    (unsafe { LP_CLKRST_RESET_CAUSE.read_volatile() } >> 7) & 0x3f
}
