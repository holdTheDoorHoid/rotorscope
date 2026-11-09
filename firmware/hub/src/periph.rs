//! The slow, blocking peripherals: screen, SD card, buttons, buzzer, battery.
//!
//! On the S3 they run on the **second core**, on their own thread-mode
//! executor. Everything here is blocking by nature (mipidsi and
//! embedded-sdmmc are blocking drivers; an SD card can sit busy for hundreds
//! of milliseconds; a frame takes tens of milliseconds to draw), and on core
//! 0 that time would come straight out of USB, HTTP, CoT and the tick. On
//! core 1 it costs nothing but itself. The tasks talk to core 0 only through
//! critical-section-guarded statics (`screen::UI_STATE`, the SD row pipe,
//! atomics), which work across cores.
//!
//! The C5 has one core: there the same tasks are spawned on the main
//! executor, and a slow card or frame delays everything else. The C5 hub is
//! a fallback build, so that is accepted.

use embassy_executor::Spawner;
use embedded_graphics::pixelcolor::{Rgb565, RgbColor};
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
#[cfg(feature = "esp32s3")]
use esp_hal::gpio::DriveMode;
use esp_hal::gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, Pull};
#[cfg(feature = "esp32s3")]
use esp_hal::ledc::channel::{self, ChannelIFace};
#[cfg(feature = "esp32s3")]
use esp_hal::ledc::timer::{self, TimerIFace};
#[cfg(feature = "esp32s3")]
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
#[cfg(feature = "esp32s3")]
use esp_hal::peripherals::LEDC;
use esp_hal::peripherals::{ADC1, SPI2};
use esp_hal::spi::master::Spi;
#[cfg(feature = "esp32s3")]
use esp_hal::time::Rate;
use static_cell::StaticCell;

use crate::pwm::Pwm;

use crate::bus::{Bus, Speed};
use crate::pins::BatteryPin;
use crate::{panel, screen, sd, usb};

/// Everything the second core needs, moved over in one piece.
pub struct Parts {
    pub spi: SPI2<'static>,
    #[cfg(feature = "esp32s3")]
    pub ledc: LEDC<'static>,
    pub adc: ADC1<'static>,
    pub sck: AnyPin<'static>,
    pub mosi: AnyPin<'static>,
    pub miso: AnyPin<'static>,
    pub screen_cs: AnyPin<'static>,
    pub screen_dc: AnyPin<'static>,
    pub backlight: AnyPin<'static>,
    pub sd_cs: AnyPin<'static>,
    pub buttons: [AnyPin<'static>; 3],
    pub buzzer: AnyPin<'static>,
    pub battery: BatteryPin,
}

static BUS: StaticCell<Bus> = StaticCell::new();
#[cfg(feature = "esp32s3")]
static LEDC_DRV: StaticCell<Ledc<'static>> = StaticCell::new();
#[cfg(feature = "esp32s3")]
static BL_TIMER: StaticCell<timer::Timer<'static, LowSpeed>> = StaticCell::new();
#[cfg(feature = "esp32s3")]
static BUZZ_TIMER: StaticCell<timer::Timer<'static, LowSpeed>> = StaticCell::new();
static SPI_BUF: StaticCell<[u8; 512]> = StaticCell::new();

/// Bring up the peripherals and spawn their tasks on `spawner`.
pub fn spawn(spawner: Spawner, p: Parts) {
    // One SPI bus, starting at the SD card's identification speed (see bus.rs).
    let spi = match Spi::new(p.spi, Bus::config(Speed::SdInit)) {
        Ok(s) => s.with_sck(p.sck).with_mosi(p.mosi).with_miso(p.miso),
        Err(_) => {
            usb::msg("err", "spi config; no screen or SD");
            return;
        }
    };
    let bus: &'static Bus = BUS.init(Bus::new(spi));
    // Both chip selects high (idle) before any transfer.
    let screen_cs = Output::new(p.screen_cs, Level::High, OutputConfig::default());
    let sd_cs = Output::new(p.sd_cs, Level::High, OutputConfig::default());
    let dc = Output::new(p.screen_dc, Level::Low, OutputConfig::default());

    #[cfg(feature = "esp32s3")]
    let (backlight, buzzer) = pwm_outputs(p.ledc, p.backlight, p.buzzer);
    #[cfg(not(feature = "esp32s3"))]
    let (backlight, buzzer) = pwm_outputs(p.backlight, p.buzzer);

    let buttons = p.buttons.map(|pin| Input::new(pin, InputConfig::default().with_pull(Pull::Up)));

    // ADC1 (ADC2 is unusable while Wi-Fi runs), 11 dB attenuation for the
    // full ~0-3.1 V range, curve calibration from the chip's eFuses.
    let mut adc_cfg = AdcConfig::new();
    let batt_pin = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1<'static>>>(p.battery, Attenuation::_11dB);
    let adc = Adc::new(p.adc, adc_cfg);

    // The band buffer (15 KB) comes from the heap: allocated once, never
    // freed, and kept out of .bss where it would shrink core 0's stack.
    let band: &'static mut [Rgb565] =
        alloc::vec![Rgb565::BLACK; ::screen::WIDTH as usize * ::screen::BAND_ROWS].leak();

    spawner.spawn(sd::task(bus, sd_cs).expect("spawn sd"));
    spawner.spawn(
        screen::display(screen::DisplayParts {
            bus,
            cs: screen_cs,
            dc,
            backlight,
            buttons,
            band,
            spi_buf: SPI_BUF.init([0; 512]),
        })
        .expect("spawn display"),
    );
    spawner.spawn(panel::buzzer(buzzer).expect("spawn buzzer"));
    spawner.spawn(panel::battery(adc, batt_pin).expect("spawn battery"));
}

/// LEDC: timer 0 for the backlight (5 kHz, above audible and flicker),
/// timer 1 for the buzzer tone. Both from the 80 MHz APB clock, 8-bit duty.
#[cfg(feature = "esp32s3")]
fn pwm_outputs(ledc: LEDC<'static>, backlight: AnyPin<'static>, buzzer: AnyPin<'static>) -> (Pwm, Pwm) {
    let ledc = LEDC_DRV.init(Ledc::new(ledc));
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    let bl_timer = BL_TIMER.init(ledc.timer::<LowSpeed>(timer::Number::Timer0));
    let _ = bl_timer.configure(timer::config::Config {
        duty: timer::config::Duty::Duty8Bit,
        clock_source: timer::LSClockSource::APBClk,
        frequency: Rate::from_khz(5),
    });
    let buzz_timer = BUZZ_TIMER.init(ledc.timer::<LowSpeed>(timer::Number::Timer1));
    let _ = buzz_timer.configure(timer::config::Config {
        duty: timer::config::Duty::Duty8Bit,
        clock_source: timer::LSClockSource::APBClk,
        frequency: Rate::from_hz(2_700),
    });
    let mut bl = ledc.channel(channel::Number::Channel0, backlight);
    let _ = bl.configure(channel::config::Config {
        timer: &*bl_timer,
        duty_pct: 0, // dark until the first frame is on the glass
        drive_mode: DriveMode::PushPull,
    });
    let mut bz = ledc.channel(channel::Number::Channel1, buzzer);
    let _ = bz.configure(channel::config::Config {
        timer: &*buzz_timer,
        duty_pct: 0,
        drive_mode: DriveMode::PushPull,
    });
    (Pwm(bl), Pwm(bz))
}

/// No LEDC driver for the C5 in esp-hal 1.2: plain on/off pins (see pwm.rs).
#[cfg(not(feature = "esp32s3"))]
fn pwm_outputs(backlight: AnyPin<'static>, buzzer: AnyPin<'static>) -> (Pwm, Pwm) {
    (
        Pwm(Output::new(backlight, Level::Low, OutputConfig::default())),
        Pwm(Output::new(buzzer, Level::Low, OutputConfig::default())),
    )
}

// ------------------------------------------------------------ S3 only --

/// Stack for the second core. Its deepest paths are a frame render (the
/// layout code and embedded-graphics) and embedded-sdmmc directory scans.
/// Measured with the demo and no card: ~3.7 KB used (`core1_stack_free` in
/// the `hub` line); 12 KB leaves room for the SD paths not yet exercised.
#[cfg(feature = "esp32s3")]
pub const CORE1_STACK: usize = 12 * 1024;

#[cfg(feature = "esp32s3")]
static EXEC1: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
/// Bottom of the second core's stack, for [`core1_stack_free`].
#[cfg(feature = "esp32s3")]
static STACK1_BASE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(feature = "esp32s3")]
const PAINT: u8 = 0xA5;

/// Bytes of the second core's stack never touched since boot (its headroom),
/// found by scanning the paint from the bottom up.
#[cfg(feature = "esp32s3")]
pub fn core1_stack_free() -> Option<usize> {
    let base = STACK1_BASE.load(core::sync::atomic::Ordering::Relaxed);
    if base == 0 {
        return None;
    }
    // SAFETY: `base` points at the start of the leaked, CORE1_STACK-byte heap
    // block that is the second core's stack. It is never freed; bytes the
    // stack has not reached yet are only read here, and a racing write just
    // makes the answer slightly conservative.
    let mem = unsafe { core::slice::from_raw_parts(base as *const u8, CORE1_STACK) };
    // esp-rtos keeps a guard word at offset 60 and a few bytes of its own
    // just above it; the headroom is the painted run after those.
    let first = mem[GUARD_END..GUARD_END + 64].iter().position(|&b| b == PAINT)? + GUARD_END;
    Some(mem[first..].iter().take_while(|&&b| b == PAINT).count())
}

/// End of esp-hal's stack guard word (offset 60, 4 bytes).
#[cfg(feature = "esp32s3")]
const GUARD_END: usize = 64;

#[cfg(not(feature = "esp32s3"))]
pub fn core1_stack_free() -> Option<usize> {
    None
}

/// Start the second core with its own executor running [`spawn`].
///
/// Its stack comes from the heap rather than `.bss`: `.bss` is carved out of
/// the same RAM as core 0's stack, which is already the tightest budget on a
/// chip without PSRAM. The stack is painted so its headroom can be measured.
#[cfg(feature = "esp32s3")]
pub fn start_core1(
    cpu: esp_hal::peripherals::CPU_CTRL<'static>,
    int1: esp_hal::peripherals::FROM_CPU_INTR1<'static>,
    parts: Parts,
) {
    use esp_hal::system::Stack;
    let mut boxed = alloc::boxed::Box::<Stack<CORE1_STACK>>::new_uninit();
    // SAFETY: `Stack` is only a `MaybeUninit` byte array, so every bit
    // pattern (including uninitialised) is a valid value. Painting first
    // makes the headroom measurable.
    let stack: &'static mut Stack<CORE1_STACK> = unsafe {
        core::ptr::write_bytes(boxed.as_mut_ptr().cast::<u8>(), PAINT, CORE1_STACK);
        alloc::boxed::Box::leak(boxed.assume_init())
    };
    STACK1_BASE.store(stack.mem.as_ptr() as usize, core::sync::atomic::Ordering::Relaxed);
    esp_rtos::start_second_core(cpu, int1, stack, move || {
        let exec = EXEC1.init(esp_rtos::embassy::Executor::new());
        exec.run(move |spawner| spawn(spawner, parts));
    });
}
