//! Scan plans as data: what the node should listen to, in what order, for
//! how long. A plan is an iterator of [`ScanStep`]s computed on the fly (no
//! storage). The node applies `step.config` to the LR2021, listens for
//! `step.dwell_ms` (stretching to `step.extend_ms` if anything was
//! recognised), feeds every packet to [`crate::recognise`], and moves on.
//!
//! ```
//! use ctrlwatch::{scan, System, Region};
//! for step in scan::plan(System::ExpressLrs, Region::Fcc915) {
//!     // radio.apply(&step.config); listen(step.dwell_ms); ...
//!     assert!(step.dwell_ms >= 1000);
//! }
//! ```
//!
//! Multi-SF receive: the LR2021 can listen for up to four spreading
//! factors at once when everything else matches. Plans put such steps next
//! to each other; [`multi_sf_compatible`] says whether two steps can share
//! one receive.

use crate::elrs::{ElrsRate, OtaVersion};
use crate::mlrs::MlrsMode;
use crate::rx::{Modulation, Probe, RxConfig, ScanStep};
use crate::{Region, System};

/// ExpressLRS LoRa rates with distinct receive settings, sub-GHz, grouped
/// for multi-SF (same CR and length next to each other).
pub const ELRS_SUB_GHZ: [ElrsRate; 8] = [
    ElrsRate::L900_200,
    ElrsRate::L900_100,
    ElrsRate::L900_50,
    ElrsRate::L900_25,
    ElrsRate::L900_250,
    ElrsRate::Dual900_150,
    ElrsRate::L900_200Full,
    ElrsRate::L900_100Full,
];

pub const ELRS_2G4: [ElrsRate; 7] = [
    ElrsRate::L2g4_250,
    ElrsRate::L2g4_150,
    ElrsRate::L2g4_50,
    ElrsRate::L2g4_500,
    ElrsRate::Dual2g4_150,
    ElrsRate::L2g4_333Full,
    ElrsRate::L2g4_100Full,
];

pub const SIK_RATES: [u8; 4] = [64, 128, 250, 32];
pub const OPENLRS_RATES: [u8; 5] = [2, 1, 0, 3, 4];

/// ELRS sync packets come every 5 s on a connected link: stay this long
/// once anything was heard.
pub const ELRS_EXTEND_MS: u32 = 5_500;

/// The band an energy sweep covers for a region (Hz).
pub const fn band(region: Region) -> (u32, u32) {
    match region {
        Region::Fcc915 | Region::Au915 | Region::Th920 => (902_000_000, 928_000_000),
        Region::Eu868 | Region::In866 => (863_000_000, 870_000_000),
        Region::Au433 | Region::Eu433 | Region::Us433 | Region::Us433W => {
            (423_000_000, 440_000_000)
        }
        Region::Ism2g4 => (2_400_000_000, 2_483_500_000),
    }
}

/// Can `a` and `b` be received together with LR2021 multi-SF (same
/// frequency, bandwidth, coding rate, header, length and IQ; different SF)?
pub fn multi_sf_compatible(a: &RxConfig, b: &RxConfig) -> bool {
    match (a.modulation, b.modulation) {
        (Modulation::LoRa(x), Modulation::LoRa(y)) => {
            a.freq_hz == b.freq_hz
                && x.bw == y.bw
                && x.cr == y.cr
                && x.header == y.header
                && x.invert_iq == y.invert_iq
                && x.sync_word == y.sync_word
                && x.sf != y.sf
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Elrs,
    Mlrs,
    Sik,
    OpenLrs,
    Energy { bin_hz: u32 },
    None,
}

/// A scan plan (iterator of [`ScanStep`]).
#[derive(Debug, Clone)]
pub struct Plan {
    kind: Kind,
    region: Region,
    i: u32,
}

/// The packet-recognition plan for `system` in `region`. Closed systems
/// (Crossfire, Ghost, ...) have no packet plan: use [`energy_plan`].
pub fn plan(system: System, region: Region) -> Plan {
    let kind = match system {
        System::ExpressLrs => Kind::Elrs,
        System::Mlrs => Kind::Mlrs,
        System::Sik => Kind::Sik,
        System::OpenLrsNg => Kind::OpenLrs,
        _ => Kind::None,
    };
    Plan { kind, region, i: 0 }
}

/// An energy sweep of the region's band in `bin_hz` steps (receive
/// bandwidth = `bin_hz`; up to ~3 MHz on the LR2021). Use a bin no wider
/// than half the channel spacing you want to resolve (300 kHz for ELRS
/// 900, 125 kHz for SiK) for hop-pattern work; wide bins for a quick look.
pub fn energy_plan(region: Region, bin_hz: u32) -> Plan {
    Plan {
        kind: Kind::Energy {
            bin_hz: bin_hz.max(10_000),
        },
        region,
        i: 0,
    }
}

impl Iterator for Plan {
    type Item = ScanStep;

    fn next(&mut self) -> Option<ScanStep> {
        let s = step(self.kind, self.region, self.i);
        if s.is_some() {
            self.i += 1;
        }
        s
    }
}

fn step(kind: Kind, region: Region, i: u32) -> Option<ScanStep> {
    match kind {
        Kind::None => None,
        Kind::Elrs => elrs_step(region, i),
        Kind::Mlrs => {
            let modes: &[MlrsMode] = if region.is_2g4() {
                &[MlrsMode::L2g4_50, MlrsMode::L2g4_31, MlrsMode::L2g4_19]
            } else {
                &[MlrsMode::L900_31, MlrsMode::L900_19]
            };
            let mode = *modes.get(i as usize)?;
            let freq = match crate::mlrs::channels(region) {
                Some((f0, step, n)) => f0 + step * (n as u32 / 2),
                None if matches!(
                    region,
                    Region::Eu433 | Region::Au433 | Region::Us433 | Region::Us433W
                ) =>
                {
                    crate::mlrs::FREQS_433[1]
                }
                None => return None,
            };
            let config = crate::mlrs::lora_rx(mode, freq, region)?;
            Some(ScanStep {
                config,
                dwell_ms: 3_000,
                extend_ms: 6_000,
            })
        }
        Kind::Sik => {
            let kbps = *SIK_RATES.get(i as usize)?;
            let config = crate::sik::rx_config(region, crate::sik::DEFAULT_NETID, 0, kbps)?;
            Some(ScanStep {
                config,
                dwell_ms: 3_000,
                extend_ms: 6_000,
            })
        }
        Kind::OpenLrs => {
            let base = crate::openlrs::base_freq(region)?;
            let n = OPENLRS_RATES.len() as u32;
            if i < n {
                let f = base
                    + crate::openlrs::DEFAULT_HOPS[0] as u32 * crate::openlrs::DEFAULT_SPACING_HZ;
                let config =
                    crate::openlrs::rx_config(region, f, OPENLRS_RATES[i as usize], false)?;
                Some(ScanStep {
                    config,
                    dwell_ms: 2_000,
                    extend_ms: 4_000,
                })
            } else if i == n {
                // Binding: 9600 bit/s on the base frequency.
                let config = crate::openlrs::rx_config(region, base, 1, true)?;
                Some(ScanStep {
                    config,
                    dwell_ms: 2_000,
                    extend_ms: 4_000,
                })
            } else {
                None
            }
        }
        Kind::Energy { bin_hz } => {
            let (lo, hi) = band(region);
            let f = lo + bin_hz / 2 + i * bin_hz;
            (f < hi).then_some(ScanStep {
                config: RxConfig {
                    freq_hz: f,
                    modulation: Modulation::Energy { rx_bw_hz: bin_hz },
                    probe: Probe::Energy,
                    region,
                },
                dwell_ms: 2,
                extend_ms: 2,
            })
        }
    }
}

fn elrs_step(region: Region, i: u32) -> Option<ScanStep> {
    let rates: &[ElrsRate] = if region.is_2g4() {
        &ELRS_2G4
    } else {
        &ELRS_SUB_GHZ
    };
    // Versions whose sync channels differ; when they coincide only one.
    let v3 = crate::elrs::sync_freq(region, OtaVersion::V3);
    let v4 = crate::elrs::sync_freq(region, OtaVersion::V4);
    let versions: &[OtaVersion] = if v3 == v4 {
        &[OtaVersion::V4]
    } else {
        &[OtaVersion::V4, OtaVersion::V3]
    };
    let iqs: &[bool] = if region.is_2g4() {
        &[false, true]
    } else {
        &[false]
    };
    let per_version = (rates.len() * iqs.len()) as u32;
    let v = *versions.get((i / per_version) as usize)?;
    let j = i % per_version;
    let rate = rates[(j / iqs.len() as u32) as usize];
    let iq = iqs[(j % iqs.len() as u32) as usize];
    let config = crate::elrs::lora_rx(rate, crate::elrs::sync_freq(region, v), iq, region, v)?;
    let info = rate.info();
    let dom = crate::elrs::domain(region, v);
    // Time between visits to the sync channel.
    let period_ms = dom.count as u32 * info.hop_interval as u32 * info.interval_us / 1000;
    Some(ScanStep {
        config,
        dwell_ms: (2 * period_ms).clamp(1_000, 7_000),
        extend_ms: ELRS_EXTEND_MS.max(2 * period_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rx::LoRaHeader;

    #[test]
    fn elrs_fcc_plan() {
        let steps: std::vec::Vec<_> = plan(System::ExpressLrs, Region::Fcc915).collect();
        assert_eq!(steps.len(), 16); // 8 rates x 2 sync channels
        assert!(steps[..8].iter().all(|s| s.config.freq_hz == 915_500_000));
        assert!(steps[8..].iter().all(|s| s.config.freq_hz == 916_100_000));
        let Modulation::LoRa(l) = steps[0].config.modulation else {
            panic!()
        };
        assert_eq!(l.sync_word, 0x12);
        assert_eq!(l.header, LoRaHeader::Implicit { len: 8 });
        assert!(!l.crc_on && !l.invert_iq);
        // 200/100/50/25 Hz share CR 4/7 and length 8: one multi-SF group.
        assert!(multi_sf_compatible(&steps[0].config, &steps[1].config));
        assert!(multi_sf_compatible(&steps[0].config, &steps[3].config));
        assert!(!multi_sf_compatible(&steps[3].config, &steps[4].config));
    }

    #[test]
    fn elrs_2g4_plan_tries_both_iq() {
        let steps: std::vec::Vec<_> = plan(System::ExpressLrs, Region::Ism2g4).collect();
        assert_eq!(steps.len(), 28);
        let iq: std::vec::Vec<bool> = steps
            .iter()
            .map(|s| match s.config.modulation {
                Modulation::LoRa(l) => l.invert_iq,
                _ => unreachable!(),
            })
            .collect();
        assert!(iq.contains(&true) && iq.contains(&false));
    }

    #[test]
    fn other_plans() {
        assert_eq!(plan(System::Mlrs, Region::Fcc915).count(), 2);
        assert_eq!(plan(System::Mlrs, Region::Ism2g4).count(), 3);
        assert_eq!(plan(System::Sik, Region::Fcc915).count(), 4);
        assert_eq!(plan(System::OpenLrsNg, Region::Eu433).count(), 6);
        assert_eq!(plan(System::Crossfire, Region::Fcc915).count(), 0);
        let e: std::vec::Vec<_> = energy_plan(Region::Fcc915, 200_000).collect();
        assert_eq!(e.len(), 130);
        assert!(e.iter().all(|s| s.config.probe == Probe::Energy));
    }
}
