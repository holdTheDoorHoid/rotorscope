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
//!
//! **Our own decoder (`--features modes`, PLAN.md Phase 14a).** The ADSBee
//! is set to `AT+PROTOCOL_OUT=COMMS_UART,BEAST` instead and the hub decodes
//! the raw Mode S frames itself with `crates/modes`. Once a second the
//! decoder's table is reported as the same `adsb::Aircraft` records CSBee
//! gives, so the tracker, screen, SD log, JSON lines, FAA-rule checks and
//! the web-head feed (as `Csbee` frames: no link change, no decoder on the
//! S3) are unchanged. CSBee lines are still accepted in this build, so a
//! unit whose ADSBee was left in CSBee mode keeps working. CSBee stays the
//! default build until the raw path has run with a real ADSBee on the hub;
//! on the laptop it agreed with the ADSBee's own decode (docs/ADSBEE.md).

use core::sync::atomic::Ordering::Relaxed;

use tracker::json::AdsbStatus;

use crate::app::HubTracker;
use crate::uartring::adsb_rx;
use crate::webhead::Feed;

/// The ADSBee reports once a second; quiet for this long means it is gone.
pub const ALIVE_MS: u64 = 3_000;

/// Our decoder: Beast frames in, an aircraft table out.
#[cfg(feature = "modes")]
pub struct Raw {
    pub beast: modes::BeastReader,
    pub table: modes::Table<{ crate::app::MAX_AIRCRAFT }>,
    last_report_ms: u64,
}

#[cfg(feature = "modes")]
impl Default for Raw {
    fn default() -> Self {
        Self { beast: modes::BeastReader::new(), table: modes::Table::default(), last_report_ms: 0 }
    }
}

#[derive(Default)]
pub struct Adsbee {
    split: adsb::LineSplitter,
    #[cfg(feature = "modes")]
    pub raw: Raw,
    /// Lines parsed (aircraft and statistics), refused (bad CRC, malformed),
    /// and text that is not CSBee at all.
    pub lines: u32,
    pub errors: u32,
    pub other: u32,
    pub last_ms: Option<u64>,
    pub stats: Option<adsb::Stats>,
}

impl Adsbee {
    /// Take what UART4 received. Each report also goes to the web head
    /// (`feed`), as the CSBee line it came from.
    #[cfg_attr(feature = "webheadloop", allow(dead_code))]
    pub fn tick(&mut self, tracker: &mut HubTracker, now: u64, mut feed: Option<&mut Feed>) {
        let mut buf = [0u8; 256];
        loop {
            let n = adsb_rx::take(&mut buf);
            if n == 0 {
                break;
            }
            self.feed(&buf[..n], tracker, now, feed.as_deref_mut());
        }
        #[cfg(feature = "modes")]
        self.report_raw(tracker, now, feed);
    }

    /// Once a second: our table's aircraft to the tracker and the web head,
    /// like the ADSBee's own once-a-second CSBee burst.
    #[cfg(feature = "modes")]
    pub fn report_raw(&mut self, tracker: &mut HubTracker, now: u64, mut feed: Option<&mut Feed>) {
        let raw = &mut self.raw;
        if now.saturating_sub(raw.last_report_ms) < 1000 {
            return;
        }
        raw.last_report_ms = now;
        if raw.table.stats.frames == 0 {
            return;
        }
        // The hub's own position (GPS or manual) is the local-CPR and
        // surface reference and the range check's centre.
        raw.table.set_reference(tracker.own_position(now).map(|f| (f.lat, f.lon)));
        let stats = raw.table.report(now, |a| {
            tracker.ingest_aircraft(&a, now);
            if let Some(f) = feed.as_deref_mut() {
                f.adsb(&adsb::Report::Aircraft(a));
            }
        });
        if let Some(f) = feed.as_deref_mut() {
            f.adsb(&adsb::Report::Stats(stats));
        }
        self.stats = Some(stats);
    }

    /// CSBee bytes, from UART4 or (bench) from a test.
    pub fn feed(&mut self, bytes: &[u8], tracker: &mut HubTracker, now: u64, mut feed: Option<&mut Feed>) {
        // Beast: every byte also goes to the frame reader. CSBee text never
        // contains its start byte (0x1A), so each parser ignores the other.
        #[cfg(feature = "modes")]
        {
            let (table, last) = (&mut self.raw.table, &mut self.last_ms);
            self.raw.beast.feed(bytes, |rx| {
                *last = Some(now);
                let _ = table.ingest(&rx, now);
            });
        }
        {
            let (lines, errors, other, last, stats) =
                (&mut self.lines, &mut self.errors, &mut self.other, &mut self.last_ms, &mut self.stats);
            self.split.feed(bytes, |r| match r {
                Ok(report) => {
                    *lines += 1;
                    *last = Some(now);
                    if let Some(f) = feed.as_deref_mut() {
                        f.adsb(&report);
                    }
                    match report {
                        adsb::Report::Aircraft(a) => {
                            tracker.ingest_aircraft(&a, now);
                        }
                        adsb::Report::Stats(s) => *stats = Some(s),
                    }
                }
                Err(adsb::Error::NotCsbee) => *other += 1,
                // Beast binary through the line splitter (a stray '#').
                #[cfg(feature = "modes")]
                Err(adsb::Error::TooLong | adsb::Error::NotText) => *other += 1,
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
