//! Buzzer and battery (PLAN.md O6, H2). The buttons are read by the screen
//! task, which owns the menu state machine they drive (`screen.rs`).
//!
//! - **Buzzer**: a passive piezo on LEDC, a 2.7 kHz square wave (the usual
//!   resonance of small piezo discs) switched on and off for a short double
//!   chirp. `screen::Ui` already rate-limits requests (one per 3 s, none when
//!   muted in Settings), so a swarm never becomes a siren.
//! - **Battery**: ADC1 on the sense pin, behind a resistor divider. The
//!   reading is converted to a charge estimate with a resting-voltage curve
//!   for one LiPo cell. **Placeholder, uncalibrated**: the divider ratio is a
//!   guess until the PCB exists, and with nothing wired the pin floats and
//!   reads nonsense. Below [`NO_BATTERY_MV`] the hub assumes it has no cell
//!   and is on USB, which is also what a bench board reports. Telling "on USB
//!   with a full cell" from "on battery" needs a VBUS sense pin on the PCB.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Timer;

use crate::pwm::Pwm;
use tracker::TrackId;

/// Battery sense divider: cell voltage = pin voltage x this. PLACEHOLDER
/// (two equal resistors). Set it from the PCB.
pub const BATTERY_DIVIDER: f32 = 2.0;
/// Below this the "cell" is not a cell: nothing attached, so on USB.
pub const NO_BATTERY_MV: u32 = 2_500;
/// Above this no LiPo rests: a charger (USB) is holding the line up.
pub const CHARGER_MV: u32 = 4_300;

/// Last battery reading, millivolts at the cell (0 = not measured yet).
static BATT_MV: AtomicU32 = AtomicU32::new(0);
static BUZZ: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static BUZZES: AtomicU32 = AtomicU32::new(0);

/// A drone was seen for the first time: banner on the screen (and its
/// buzzer request, rate-limited there).
pub fn new_drone(id: TrackId) {
    crate::screen::new_drone(id.0);
}

/// Ask for one chirp (from the screen's `Ui::take_buzz`).
pub fn buzz() {
    BUZZ.signal(());
}

/// Charge from cell voltage: an approximate resting curve for one LiPo cell
/// (piecewise linear). Under load the voltage sags, so this reads low while
/// all four radios are busy; good enough for a five-step icon.
pub fn lipo_pct(mv: u32) -> u8 {
    const CURVE: [(u32, u8); 11] = [
        (3300, 0),
        (3500, 5),
        (3600, 10),
        (3700, 20),
        (3750, 30),
        (3800, 40),
        (3850, 50),
        (3900, 60),
        (4000, 75),
        (4100, 90),
        (4200, 100),
    ];
    if mv <= CURVE[0].0 {
        return 0;
    }
    for w in CURVE.windows(2) {
        let ((v0, p0), (v1, p1)) = (w[0], w[1]);
        if mv <= v1 {
            return p0 + ((mv - v0) * u32::from(p1 - p0) / (v1 - v0)) as u8;
        }
    }
    100
}

/// What the screen's status bar shows.
pub fn power() -> screen::Power {
    let mv = BATT_MV.load(Relaxed);
    if !(NO_BATTERY_MV..CHARGER_MV).contains(&mv) {
        screen::Power::Usb { battery_pct: None }
    } else {
        screen::Power::Battery { pct: lipo_pct(mv) }
    }
}

/// For the `hub` line.
#[derive(serde::Serialize)]
pub struct BattLine {
    /// Cell millivolts as measured (uncalibrated placeholder divider).
    mv: u32,
    /// `"usb"` (no cell, or a charger holding it up) or `"battery"`.
    src: &'static str,
    pct: Option<u8>,
    note: &'static str,
}

impl BattLine {
    pub fn now() -> Self {
        let mv = BATT_MV.load(Relaxed);
        let (src, pct) = match power() {
            screen::Power::Battery { pct } => ("battery", Some(pct)),
            screen::Power::Usb { .. } => ("usb", None),
        };
        Self { mv, src, pct, note: "uncalibrated" }
    }
}

/// Plays a chirp per request: two 60 ms beeps. LEDC keeps the tone running
/// by itself; this only switches the duty between 50% and 0.
#[embassy_executor::task]
pub async fn buzzer(mut ch: Pwm) {
    ch.set_pct(0);
    loop {
        BUZZ.wait().await;
        BUZZES.fetch_add(1, Relaxed);
        for _ in 0..2 {
            ch.set_pct(50);
            Timer::after_millis(60).await;
            ch.set_pct(0);
            Timer::after_millis(60).await;
        }
    }
}

pub type BattAdc = esp_hal::analog::adc::Adc<'static, esp_hal::peripherals::ADC1<'static>, esp_hal::Blocking>;
pub type BattPin = esp_hal::analog::adc::AdcPin<
    crate::pins::BatteryPin,
    esp_hal::peripherals::ADC1<'static>,
    esp_hal::analog::adc::AdcCalCurve<esp_hal::peripherals::ADC1<'static>>,
>;

/// Battery sense every 5 s, averaging 16 one-shot reads (the ADC is noisy
/// and the radios cause supply ripple). Curve calibration (factory eFuse
/// values) turns the raw code into millivolts at the pin.
#[embassy_executor::task]
pub async fn battery(mut adc: BattAdc, mut pin: BattPin) {
    loop {
        let mut sum = 0u32;
        let mut n = 0u32;
        for _ in 0..16 {
            // A one-shot conversion takes tens of microseconds.
            let mv = loop {
                match adc.read_oneshot(&mut pin) {
                    Ok(v) => break Some(v),
                    Err(nb::Error::WouldBlock) => {}
                    Err(_) => break None,
                }
            };
            if let Some(mv) = mv {
                sum += u32::from(mv);
                n += 1;
            }
        }
        if n > 0 {
            let pin_mv = sum / n;
            BATT_MV.store((pin_mv as f32 * BATTERY_DIVIDER) as u32, Relaxed);
        }
        Timer::after_secs(5).await;
    }
}
