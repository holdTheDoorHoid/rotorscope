//! Crash recovery shared by the nodes and the gateway: reboot after a panic,
//! a hang watchdog, and why the chip last reset (docs/BENCH.md "Crash
//! recovery").
//!
//! `esp_hal::init` turns every watchdog off. [`arm`] turns the RTC watchdog
//! (RWDT) back on with stage 0 = reset the whole chip; the board's main
//! piece of work then calls [`feed`] as it goes round. If that piece stops
//! (a task that never yields, a deadlock, interrupts left off), the chip
//! resets after the timeout and boots as if powered up, which is better for
//! an unattended box than staying blind. The RWDT runs from the RTC slow
//! clock, so it still fires when the CPU or its clocks are wedged.
//!
//! [`feed`] is a few register writes (no printing, no locking), cheap enough
//! for node-sdr's capture loop. Call it from thread mode only, not from an
//! interrupt (two feeds interleaving their write-protect writes could leave
//! the protection off; harmless, but untidy).

use esp_hal::rtc_cntl::{Rtc, RwdtStage};
use esp_hal::time::Duration;

/// The RWDT driver, made on the spot. `Rtc::new` only wraps the peripheral
/// token; the registers are the same whoever holds it.
fn rtc() -> Rtc<'static> {
    // SAFETY: the board's firmware never constructs an `Rtc` of its own
    // (none of node-wifi, node-ble, node-sdr, node-lr or the gateway use
    // RTC_TIMER), and every use here is a short register access.
    Rtc::new(unsafe { esp_hal::peripherals::RTC_TIMER::steal() })
}

/// Turn the hang watchdog on: the chip resets if [`feed`] is not called for
/// `timeout`. Call once, after the slow parts of boot (radio start-up).
pub fn arm(timeout: Duration) {
    let mut rtc = rtc();
    rtc.rwdt.set_timeout(RwdtStage::Stage0, timeout);
    rtc.rwdt.enable();
    rtc.rwdt.feed();
}

/// Restart the hang watchdog's count.
#[inline]
pub fn feed() {
    rtc().rwdt.feed();
}

/// After a panic (the message and backtrace are already on USB): wait a
/// moment so the host can read them, then reset the chip. Called from each
/// firmware's `custom_halt` (esp-backtrace's `custom-halt` feature).
pub fn reboot_after_panic() -> ! {
    esp_hal::delay::Delay::new().delay_millis(500);
    esp_hal::system::software_reset()
}

/// Why the chip last reset, as one word for the boot line (see [`name`]).
pub fn reset_reason() -> &'static str {
    esp_hal::system::reset_reason().map_or("unknown", |r| name(r as usize))
}

/// The ROM's reset cause code (`SocResetReason` as a number; the numbers are
/// the same on the S3, C3 and C5 even where the variant names differ) as one
/// word:
/// - `power_on`: power applied, or the EN / reset button;
/// - `software`: a reboot by the firmware itself: after a panic, an image
///   switch, a gateway epoch roll-over, or a BLE host restart;
/// - `watchdog`: the hang watchdog (or another watchdog) fired;
/// - `brownout`: the supply dipped;
/// - `usb`: reset over USB (a flasher or a serial monitor opening the port);
/// - `deep_sleep`, or `other` for the rarer causes.
pub fn name(code: usize) -> &'static str {
    match code {
        0x01 => "power_on",
        0x03 | 0x0C => "software",
        // Main watchdogs, RTC watchdog (core, CPU or system), super watchdog.
        0x07 | 0x08 | 0x0B | 0x11 | 0x09 | 0x0D | 0x10 | 0x12 => "watchdog",
        0x0F => "brownout",
        0x05 => "deep_sleep",
        0x15 | 0x16 => "usb",
        _ => "other",
    }
}

/// Bench only (`faulttest`): read console lines with `read` (e.g. a
/// USB-Serial-JTAG `drain_rx_fifo`), polled every 50 ms, and on
/// `fault panic` panic, on `fault hang` spin forever without yielding, which
/// stops every task on this executor (the watchdog feeder included) the way
/// a real hang would. `say` prints one line (each firmware has its own
/// output path); it gets 200 ms to reach the host before the fault.
#[cfg(feature = "faulttest")]
pub async fn console(mut read: impl FnMut(&mut [u8]) -> usize, say: impl Fn(&str)) -> ! {
    use embassy_time::{Duration, Timer};
    let mut line: heapless::Vec<u8, 32> = heapless::Vec::new();
    let mut buf = [0u8; 32];
    say(r#"{"t":"faulttest","note":"FAULT TEST BUILD: type 'fault panic' or 'fault hang' + Enter"}"#);
    loop {
        Timer::after(Duration::from_millis(50)).await;
        let n = read(&mut buf);
        for &b in &buf[..n] {
            if b != b'\r' && b != b'\n' {
                if line.push(b).is_err() {
                    line.clear();
                }
                continue;
            }
            let what = core::str::from_utf8(&line).unwrap_or("").trim();
            match what {
                "fault panic" => {
                    say(r#"{"t":"faulttest","what":"panic"}"#);
                    Timer::after(Duration::from_millis(200)).await;
                    panic!("faulttest: panic requested on the console");
                }
                "fault hang" => {
                    say(r#"{"t":"faulttest","what":"hang","note":"spinning; the watchdog should reset the chip"}"#);
                    Timer::after(Duration::from_millis(200)).await;
                    #[allow(clippy::empty_loop)]
                    loop {
                        core::hint::spin_loop();
                    }
                }
                "" => {}
                _ => say(r#"{"t":"faulttest","err":"unknown; type 'fault panic' or 'fault hang'"}"#),
            }
            line.clear();
        }
    }
}
