//! DJI channel plans (sources in `docs/DJI-LINKS.md`).
//!
//! All frequencies in kHz. Three facts carry the classification:
//!
//! - **OcuSync drones tune on a 1 MHz raster at x.5 MHz** (filed: Mavic 2's
//!   FCC SRD report lists 10 MHz mode on 5732.5-5844.5 and 20 MHz on
//!   5735.5-5839.5 in 1 MHz steps; the O3/O4 air units' manual channels are
//!   x.5 too). The uplink's hopping "1.4 MHz-CA" mode sits on a 2 MHz
//!   raster at x.12 MHz (5730.12-5846.12, 59 channels).
//! - **DJI's first FPV system (DJI FPV goggles V1/V2, Air Unit, Caddx
//!   Vista, DJI FPV drone) uses whole-MHz channels**: 5660, 5695, 5735,
//!   5770, 5805, 5839 (public channel 8), 5878, 5914.
//! - **Only O4 products list 5.170-5.250 GHz for video** (O4 Air Unit,
//!   Air 3, Mini 4 Pro, Avata 2 spec pages); O3/O3+ products (Mini 3 Pro,
//!   Mavic 3 Pro, O3 Air Unit) list 2.4 and 5.8 GHz only. Autel's EVO Lite
//!   and EVO Max file 5.15-5.25 GHz too, with LTE-like OFDM of the same
//!   widths, so "O4" there is a likelihood, not proof.

/// DJI FPV (V1) channels 1-8 (FCC; CE/SRRC allow 1-3 and 8).
pub const FPV_V1_KHZ: [u32; 8] = [5_660_000, 5_695_000, 5_735_000, 5_770_000, 5_805_000, 5_878_000, 5_914_000, 5_839_000];
/// O3 Air Unit manual channels: 10/20 MHz and 40 MHz.
pub const O3_AIR_KHZ: [u32; 3] = [5_768_500, 5_804_500, 5_839_500];
pub const O3_AIR_40_KHZ: u32 = 5_794_500;
/// O4 Air Unit (and Pro) manual channels: 10/20 MHz and 40/60 MHz.
pub const O4_AIR_KHZ: [u32; 3] = [5_768_500, 5_789_500, 5_814_500];
pub const O4_AIR_WIDE_KHZ: u32 = 5_794_500;
/// The 5.1 GHz video band only O4 products list.
pub const O4_BAND_KHZ: core::ops::RangeInclusive<u32> = 5_170_000..=5_250_000;
/// OcuSync DroneID burst centres (proto17 for OcuSync 2; EdwardBlair adds
/// 5816.5 and the hop order for O4).
pub const DRONEID_KHZ: [u32; 9] =
    [2_399_500, 2_414_500, 2_429_500, 2_444_500, 2_459_500, 5_741_500, 5_756_500, 5_776_500, 5_796_500];
pub const DRONEID_O4_EXTRA_KHZ: u32 = 5_816_500;

/// How close (kHz) a measured centre must be to a channel to count as on
/// it. Budget: C5 crystal +-10 ppm (+-58 kHz at 5.8 GHz), the transmitter's
/// own, and the edge interpolation (+-30 kHz on a clean capture). Leaves
/// 50 kHz between a whole-MHz channel and the x.5 raster's nearest step.
pub const TOL_KHZ: u32 = 200;

/// The whole-MHz DJI FPV (V1) channel `centre` is on, if any.
pub fn fpv_v1_channel(centre_khz: u32) -> Option<u8> {
    FPV_V1_KHZ.iter().position(|&c| c.abs_diff(centre_khz) <= TOL_KHZ).map(|i| i as u8 + 1)
}

/// Distance (kHz) from `centre` to the nearest x.5 MHz raster point.
pub fn half_raster_err(centre_khz: u32) -> u32 {
    let r = (centre_khz + 500) % 1000; // 0 on x.5
    r.min(1000 - r)
}

/// Whether `centre` sits on the OcuSync x.5 MHz raster.
pub fn on_half_raster(centre_khz: u32) -> bool {
    half_raster_err(centre_khz) <= TOL_KHZ
}

pub fn in_o4_band(centre_khz: u32) -> bool {
    O4_BAND_KHZ.contains(&centre_khz)
}

/// Extra FPV-sweep centres (MHz) that would cover the O4-only 5.1 GHz band
/// with the C5's 20 MHz capture (LO = centre - 1 MHz, from 5180 MHz, its
/// lowest measured LO). Not in the built-in sweep yet: adding them is the
/// sweep's decision (`fpvscan::channels`), and they cost 4 x 60 ms a sweep.
pub const O4_BAND_SWEEP_MHZ: [u16; 4] = [5181, 5201, 5221, 5241];
