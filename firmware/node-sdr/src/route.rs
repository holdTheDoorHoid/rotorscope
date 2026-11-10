//! Looping eight modem debug-bus bits through the GPIO matrix into PARL_IO.
//!
//! Each pad is an output driven by one `MODEM_DIAG` signal and, at the same
//! time, the input of one `PARL_RX_DATA` line: the pad's own input buffer
//! reads back what its output drives. **No wire or jumper is needed**
//! (esp-sdr-zephyr: "No wire; the pads really toggle at up to 40 MHz";
//! C5VRX `main/rf.h`: "These are GPIO-matrix loopbacks; no external IQ wiring
//! is required"). Because the pads really toggle, nothing may be connected
//! to them.
//!
//! esp-hal's C5 output-signal table stops at 105 (`FSPICS5`), and the debug
//! bus is signals 106 (`MODEM_DIAG0_IDX`) to 137, so the output side is
//! written straight into `GPIO_FUNCn_OUT_SEL_CFG`. Signal numbers: ESP-IDF
//! `components/soc/esp32c5/include/soc/gpio_sig_map.h` (`MODEM_DIAG0_IDX`
//! 106, `PARL_RX_DATA0_IDX` 48, `SIG_GPIO_OUT_IDX` 256).

use esp_hal::peripherals::{GPIO, IO_MUX};

pub const MODEM_DIAG0_IDX: u16 = 106;
pub const PARL_RX_DATA0_IDX: usize = 48;
pub const SIG_GPIO_OUT_IDX: u16 = 256;
/// IO MUX function 1 is "GPIO" (matrix) on every C5 pad.
const FUNC_GPIO: u8 = 1;

/// Default pads, PARL_IO data line order (Q LSB..sign, I LSB..sign): C5VRX's
/// set, validated on a XIAO ESP32-C5 at 40 MS/s (its `RF_IQ_GPIO_NUMS`).
/// On a XIAO they are D1 (GPIO0), D0 (GPIO1), D2 (GPIO25), the user LED
/// (GPIO27) and GPIO2-5 (not on the headers). They avoid the PSRAM/flash
/// pads (15-22), USB (13, 14), the UART0 header pins (11, 12) and GPIO26.
pub const PINS_XIAO: [u8; 8] = [1, 0, 25, 2, 27, 5, 3, 4];
/// esp-sdr-zephyr's set (ESPsoup's pads), for boards where the above clash.
pub const PINS_ALT: [u8; 8] = [0, 1, 3, 4, 5, 7, 10, 23];

/// Pad drive strength 0..3. C5VRX uses ESP-IDF's default (2) with the XIAO's
/// LED load on GPIO27; esp-sdr-zephyr used 0 with nothing attached.
pub const DRIVE: u8 = 2;

/// Route `lanes[k]` (a `MODEM_DIAG` bit) out of `pins[k]` and back into
/// `PARL_RX_DATAk`.
pub fn connect(pins: &[u8; 8], lanes: [u8; 8]) {
    let gpio = GPIO::regs();
    let iomux = IO_MUX::regs();
    for k in 0..8 {
        let p = pins[k] as usize;
        // SAFETY (all register writes): plain field values within their widths.
        iomux.gpio(p).modify(|_, w| unsafe {
            w.mcu_sel().bits(FUNC_GPIO);
            w.fun_ie().set_bit();
            w.fun_wpu().clear_bit();
            w.fun_wpd().clear_bit();
            w.fun_drv().bits(DRIVE)
        });
        gpio.func_out_sel_cfg(p).write(|w| unsafe {
            w.out_sel().bits(MODEM_DIAG0_IDX + lanes[k] as u16);
            w.inv_sel().clear_bit();
            // Output enable from GPIO_ENABLE, not from the (absent) peripheral.
            w.oen_sel().set_bit();
            w.oen_inv_sel().clear_bit()
        });
        gpio.enable_w1ts().write(|w| unsafe { w.enable_w1ts().bits(1 << p) });
        gpio.func_in_sel_cfg(PARL_RX_DATA0_IDX + k).write(|w| unsafe {
            w.sel().set_bit();
            w.in_inv_sel().clear_bit();
            w.in_sel().bits(p as u8)
        });
    }
}

/// Give the pads back: output driven by plain GPIO (low) and disabled,
/// input buffer off, PARL_IO inputs tied to constant 0.
pub fn release(pins: &[u8; 8]) {
    let gpio = GPIO::regs();
    let iomux = IO_MUX::regs();
    for (k, &p) in pins.iter().enumerate() {
        let p = p as usize;
        // SAFETY: plain field values within their widths.
        gpio.enable_w1tc().write(|w| unsafe { w.enable_w1tc().bits(1 << p) });
        gpio.func_out_sel_cfg(p).write(|w| unsafe { w.out_sel().bits(SIG_GPIO_OUT_IDX).oen_sel().set_bit() });
        iomux.gpio(p).modify(|_, w| w.fun_ie().clear_bit());
        // 0x60 = constant low input (esp-metadata `gpio.constant_0_input`).
        gpio.func_in_sel_cfg(PARL_RX_DATA0_IDX + k).write(|w| unsafe { w.sel().set_bit().in_sel().bits(0x60) });
    }
}

/// Change only the output lanes (a new window) on already-connected pads.
pub fn set_lanes(pins: &[u8; 8], lanes: [u8; 8]) {
    let gpio = GPIO::regs();
    for k in 0..8 {
        gpio.func_out_sel_cfg(pins[k] as usize)
            .modify(|_, w| unsafe { w.out_sel().bits(MODEM_DIAG0_IDX + lanes[k] as u16) });
    }
}

/// The `out_sel` each pad currently has (for the `phy` read-back).
pub fn read_lanes(pins: &[u8; 8]) -> [u16; 8] {
    let gpio = GPIO::regs();
    core::array::from_fn(|k| gpio.func_out_sel_cfg(pins[k] as usize).read().out_sel().bits())
}

/// The pads' current input levels, bit k = pin k (for bring-up: with the bus
/// running at 40 MHz, repeated reads should differ).
pub fn sample_inputs(pins: &[u8; 8]) -> u8 {
    let v = GPIO::regs().in_().read().bits();
    let mut b = 0;
    for (k, &p) in pins.iter().enumerate() {
        b |= (((v >> p) & 1) as u8) << k;
    }
    b
}
