//! Everything that differs between Node A (ESP32-S3, 2.4 GHz) and Node C
//! (ESP32-C5, 5 GHz). The rest of the firmware is chip-agnostic.

use esp_radio::wifi::WifiController;
use serde::Serialize;

#[cfg(feature = "esp32c5")]
pub use esp_wifi_sys_esp32c5::include as sys;
#[cfg(feature = "esp32s3")]
pub use esp_wifi_sys_esp32s3::include as sys;

#[cfg(all(feature = "esp32s3", feature = "esp32c5"))]
compile_error!("enable exactly one of the `esp32s3` / `esp32c5` features");

/// One stop on the channel plan. Serializes as `[channel, dwell_ms]`.
#[derive(Clone, Copy, Serialize)]
pub struct Hop(pub u8, pub u64);

impl Hop {
    pub const fn channel(self) -> u8 {
        self.0
    }
    pub const fn dwell_ms(self) -> u64 {
        self.1
    }
}

/// Bytes esp-radio's `PromiscuousPkt::data` carries past the end of the MPDU.
///
/// `data` is `sig_len` bytes long, and `sig_len` is documented (both MAC
/// generations in esp-radio's `RxControlInfo`) as "length including FCS".
/// DRONK measured this on the C5 (IDF 5.5.4, MAC v3): the MPDU ends at
/// `sig_len - 4`, and the 4 trailing bytes are not a valid CRC, so they are
/// dropped, never checked. The S3 (MAC v1) was checked the same way on our
/// board: beacon element walks end exactly 4 bytes short of `sig_len` (the
/// `iefit` counters in the stats line keep re-checking this in the field).
pub const FCS_LEN: usize = 4;

// ---------------------------------------------------------------- ESP32-S3 --
#[cfg(feature = "esp32s3")]
mod imp {
    use super::Hop;

    pub const CHIP: &str = "esp32s3";
    pub const ROLE: link::NodeRole = link::NodeRole::Wifi24;
    pub const HEAP_BYTES: usize = 73_744;

    /// NAN Remote ID is always on channel 6 at 2.4 GHz, and most beacon
    /// transmitters follow it, so channel 6 gets ~75% of air time. Short visits
    /// to 1 and 11 catch beacon-only modules configured elsewhere; Remote ID
    /// repeats at least once a second, so a 50 ms visit every 400 ms still
    /// lands on several transmissions per second of exposure.
    const HOME_MS: u64 = 150;
    const VISIT_MS: u64 = 50;
    pub const PLAN: &[Hop] = &[Hop(6, HOME_MS), Hop(1, VISIT_MS), Hop(6, HOME_MS), Hop(11, VISIT_MS)];

    /// Hub link: UART1 routed to the pins labelled TX/RX (D6/D7) on a XIAO
    /// ESP32-S3. These are UART0's default pins, so the ROM bootloader's boot
    /// banner also appears here for a moment after reset; the hub's COBS
    /// decoder discards it as a garbled frame.
    macro_rules! hub_tx_pin {
        ($p:ident) => {
            $p.GPIO43
        };
    }
    pub(crate) use hub_tx_pin;
}

// ---------------------------------------------------------------- ESP32-C5 --
#[cfg(feature = "esp32c5")]
mod imp {
    use super::Hop;

    pub const CHIP: &str = "esp32c5";
    pub const ROLE: link::NodeRole = link::NodeRole::Wifi5;
    pub const HEAP_BYTES: usize = 65_536;

    /// Also listen on UNII-1 (36-48). Off by default: field reports (DRONK,
    /// drone-mesh-mapper) put 5 GHz Remote ID in UNII-3, and every extra
    /// channel dilutes time on 149.
    pub const INCLUDE_UNII1: bool = false;

    /// Channel 149 is the 5 GHz anchor (DJI Avata heard there by DRONK), so it
    /// gets ~75% of air time; 153-165 are visited in turn.
    const HOME_MS: u64 = 150;
    const VISIT_MS: u64 = 50;
    const UNII3: &[Hop] = &[
        Hop(149, HOME_MS),
        Hop(153, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(157, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(161, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(165, VISIT_MS),
    ];
    const UNII3_AND_UNII1: &[Hop] = &[
        Hop(149, HOME_MS),
        Hop(153, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(157, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(161, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(165, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(36, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(40, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(44, VISIT_MS),
        Hop(149, HOME_MS),
        Hop(48, VISIT_MS),
    ];
    pub const PLAN: &[Hop] = if INCLUDE_UNII1 { UNII3_AND_UNII1 } else { UNII3 };

    /// Hub link: UART1 on the C5's UART0 default TX pin (labelled TX on a XIAO
    /// ESP32-C5). Check against your board's pinout before wiring.
    macro_rules! hub_tx_pin {
        ($p:ident) => {
            $p.GPIO11
        };
    }
    pub(crate) use hub_tx_pin;
}

pub use imp::*;

/// Also receive data frames and report open FPV / telemetry links (wfb-ng,
/// OpenHD, RubyFPV, DroneBridge; `crates/openlink`). Costs one short callback
/// per data frame on the channel; Remote ID's queue is never used for them.
/// Turn off if the `drop` counter shows Remote ID losing frames on a busy
/// channel.
pub const OPEN_LINKS: bool = true;

/// Hub UART speed. 921600 leaves ample headroom: a full 9-message Detection is
/// ~250 bytes, under 3 ms on the wire.
pub const HUB_BAUD: u32 = 921_600;

/// The radio settings esp-radio 1.0.0-beta.1 does not cover by itself.
pub fn configure_radio(ctl: &mut WifiController<'_>) -> Result<(), &'static str> {
    // Country "US", set explicitly on both chips. esp-radio beta.1 applies its
    // CountryInfo through `esp_wifi_set_country` with a zero 5 GHz channel mask,
    // and upstream has since moved the default to "01" (world-safe), which
    // rejects channel 149. `esp_wifi_set_country_code` fills in the per-country
    // channel rules, 5 GHz included. Receive-only, so this only decides which
    // channels the driver lets us tune to.
    let code = *b"US\0";
    // SAFETY: NUL-terminated string that outlives the call; Wi-Fi is initialised.
    let err = unsafe { sys::esp_wifi_set_country_code(code.as_ptr().cast(), false) };
    if err != 0 {
        return Err("esp_wifi_set_country_code");
    }

    #[cfg(feature = "esp32c5")]
    {
        // The C5 radio is on one band at a time; AUTO would let the driver move
        // it. Pin it to 5 GHz before the first set_channel.
        ctl.set_band_mode(esp_radio::wifi::BandMode::_5G)
            .map_err(|_| "set_band_mode(5G)")?;
    }
    #[cfg(not(feature = "esp32c5"))]
    let _ = ctl;

    // Management frames for Remote ID, plus data frames when looking for
    // open FPV links (`OPEN_LINKS`). Data frames are most of the air time and
    // cost a callback each, so the callback rejects them on their first byte
    // or two and never queues one (see `rx::on_frame`). Control frames stay
    // off: an RTS-mode wfb-ng or DroneBridge link is not captured. The
    // callback re-checks the type anyway, because the C5's mask has been
    // reported to leak other types.
    let mask = if OPEN_LINKS {
        sys::WIFI_PROMIS_FILTER_MASK_MGMT | sys::WIFI_PROMIS_FILTER_MASK_DATA
    } else {
        sys::WIFI_PROMIS_FILTER_MASK_MGMT
    };
    let filter = sys::wifi_promiscuous_filter_t { filter_mask: mask };
    // SAFETY: plain struct passed by pointer; Wi-Fi is initialised.
    if unsafe { sys::esp_wifi_set_promiscuous_filter(&filter) } != 0 {
        return Err("esp_wifi_set_promiscuous_filter");
    }
    Ok(())
}
