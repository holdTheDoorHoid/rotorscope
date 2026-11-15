//! ADS-B from an ADSBee 1090 on the P4 hub (PLAN.md Phase 6): its
//! COMMS_UART sends CSBee text at 115200 baud (`crates/adsb`, set up once on
//! the ADSBee's own USB console with `AT+PROTOCOL_OUT=COMMS_UART,CSBEE`; see
//! docs/ADSBEE.md). Only the ADSBee's TX is wired, to GPIO30 here (UART4 RX,
//! a wireless-module socket pin). Aircraft go to the tracker's own aircraft
//! table: never a drone track, never an alert.
//!
//! Unlike the S3 (three UARTs, all taken by nodes), the P4 has a UART to
//! spare, so this is always built; with nothing wired the pin is pulled up,
//! no bytes arrive and the ADSBee shows as not alive.
//!
//! **Not tested with a real ADSBee on this hub** (it is not wired to the P4):
//! built, host-tested in `crates/adsb`, and loop-back tested on the chip
//! (UART4 TX drives its own RX pad, `--features gpsloop`).

use core::sync::atomic::Ordering::Relaxed;

use tracker::json::AdsbStatus;

use crate::app::HubTracker;
use crate::uartring::adsb_rx;

/// The ADSBee reports once a second; quiet for this long means it is gone.
pub const ALIVE_MS: u64 = 3_000;

#[derive(Default)]
pub struct Adsbee {
    split: adsb::LineSplitter,
    /// Lines parsed (aircraft and statistics), refused (bad CRC, malformed),
    /// and text that is not CSBee at all.
    pub lines: u32,
    pub errors: u32,
    pub other: u32,
    pub last_ms: Option<u64>,
    pub stats: Option<adsb::Stats>,
}

impl Adsbee {
    pub fn tick(&mut self, tracker: &mut HubTracker, now: u64) {
        let mut buf = [0u8; 256];
        loop {
            let n = adsb_rx::take(&mut buf);
            if n == 0 {
                break;
            }
            let (lines, errors, other, last, stats) =
                (&mut self.lines, &mut self.errors, &mut self.other, &mut self.last_ms, &mut self.stats);
            self.split.feed(&buf[..n], |r| match r {
                Ok(report) => {
                    *lines += 1;
                    *last = Some(now);
                    match report {
                        adsb::Report::Aircraft(a) => {
                            tracker.ingest_aircraft(&a, now);
                        }
                        adsb::Report::Stats(s) => *stats = Some(s),
                    }
                }
                Err(adsb::Error::NotCsbee) => *other += 1,
                Err(_) => *errors += 1,
            });
        }
    }

    pub fn status(&self, now: u64) -> AdsbStatus {
        let age = self.last_ms.map(|t| now.saturating_sub(t));
        AdsbStatus {
            alive: age.is_some_and(|a| a <= ALIVE_MS),
            age_ms: age,
            frames_per_s: self.stats.map(|s| s.valid_per_s()),
            aircraft: self.stats.map(|s| s.aircraft),
            errors: self.errors,
        }
    }

    pub fn bytes(&self) -> u32 {
        adsb_rx::BYTES.load(Relaxed)
    }
}
