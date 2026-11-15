//! Bench only (`--features webheadloop`): on-chip loop-back test of the
//! web-head feed, like the gateway self-test (BRINGUP step 7). Nothing
//! outside the chip is driven or needed; **leave GPIO30 and GPIO31
//! unconnected while it runs**. The gateway wire stays connected: its real
//! traffic is what gets forwarded.
//!
//! - UART2 sends the feed on GPIO31 as in the normal build; UART4's RX is
//!   routed to the same pad (instead of the ADSBee's GPIO30) and everything
//!   it hears is decoded here with `link::Decoder`, counted by kind.
//! - GPS: once a second a GGA and an RMC sentence (fixed test point, 3D fix)
//!   are put into the GPS receive ring, so the hub's GPS code finds a plain
//!   NMEA receiver, gets a fix and UTC, and `HubFix` carries them.
//! - ADS-B: once a second a CSBee aircraft line (callsign `TESTWH`, address
//!   DE0003, made up) and a statistics line are fed to the ADSBee parser,
//!   which forwards them as `Csbee` frames.
//!
//! Checked on the receiving side: 0 corrupt, every `Via` followed by the
//! frame it names, `HubFix` with the test point and UTC, every `Csbee` line
//! parsing with `adsb::parse` (CRC included).

use core::fmt::Write;

use link::{Decoder, Frame, LinkError};

use crate::adsbee::Adsbee;
use crate::app::HubTracker;
use crate::links::GatewayLink;
use crate::uartring::{adsb_rx, gps_rx};

const LAT: f64 = 39.952_6;
const LON: f64 = -75.165_2;
const HAE: f64 = 12.5;
const START_UTC_S: u64 = 1_794_787_200; // 2026-11-16T00:00:00Z

#[derive(Default)]
struct Counts {
    bytes: u32,
    via: u32,
    hello: u32,
    stats: u32,
    status: u32,
    detection: u32,
    open_link: u32,
    other_data: u32,
    hub_fix: u32,
    csbee_ok: u32,
    csbee_bad: u32,
    corrupt: u32,
    overflow: u32,
    /// A `Via` followed by another `Via` (its frame went missing).
    orphan_via: u32,
}

pub struct Loop {
    dec: Decoder,
    n: Counts,
    after_via: bool,
    last_fix: Option<link::HubFix>,
    second: u64,
    next_ms: u64,
    last_print_ms: u64,
}

fn xor(b: &[u8]) -> u8 {
    b.iter().fold(0, |a, &c| a ^ c)
}

fn coord(v: f64, deg_digits: usize, pos: char, neg: char) -> (heapless::String<16>, char) {
    let a = if v < 0.0 { -v } else { v };
    let deg = a as u32;
    let min = (a - f64::from(deg)) * 60.0;
    let mut s = heapless::String::new();
    let _ = if deg_digits == 2 { write!(s, "{deg:02}{min:09.6}") } else { write!(s, "{deg:03}{min:09.6}") };
    (s, if v < 0.0 { neg } else { pos })
}

fn sentence(body: &str) {
    let mut out: heapless::String<128> = heapless::String::new();
    let _ = write!(out, "${body}*{:02X}\r\n", xor(body.as_bytes()));
    gps_rx::inject(out.as_bytes());
}

impl Loop {
    pub fn new() -> Self {
        crate::println!(
            "hub-p4: webheadloop: UART4 RX moved to GPIO{} (the feed's TX pad); NMEA + CSBee fed from inside the chip",
            crate::webhead::TX_PIN
        );
        Self { dec: Decoder::new(), n: Counts::default(), after_via: false, last_fix: None, second: 0, next_ms: 1000, last_print_ms: 0 }
    }

    pub fn tick(&mut self, adsbee: &mut Adsbee, tracker: &mut HubTracker, gw: &mut GatewayLink<'_>, now: u64) {
        self.receive();
        if now < self.next_ms {
            return;
        }
        self.next_ms += 1000;
        self.second += 1;
        self.gps_epoch();
        let mut line = [0u8; adsb::MAX_LINE];
        if let Some(n) = adsb::encode::aircraft(&test_aircraft(), &mut line) {
            adsbee.feed(&line[..n], tracker, now, gw.feed.as_mut());
        }
        if let Some(n) = adsb::encode::stats(&test_stats(self.second as u32), &mut line) {
            adsbee.feed(&line[..n], tracker, now, gw.feed.as_mut());
        }
    }

    fn gps_epoch(&self) {
        let c = crate::civil::civil((START_UTC_S + self.second) * 1000);
        let (lat, ns) = coord(LAT, 2, 'N', 'S');
        let (lon, ew) = coord(LON, 3, 'E', 'W');
        let mut b: heapless::String<128> = heapless::String::new();
        let t = (c.hour, c.minute, c.second);
        let _ = write!(
            b,
            "GPGGA,{:02}{:02}{:02}.000,{lat},{ns},{lon},{ew},1,09,0.90,{:.3},M,-33.900,M,,",
            t.0, t.1, t.2, HAE + 33.9
        );
        sentence(&b);
        b.clear();
        let _ = write!(
            b,
            "GPRMC,{:02}{:02}{:02}.000,A,{lat},{ns},{lon},{ew},0.00,0.00,{:02}{:02}{:02},,,A",
            t.0, t.1, t.2, c.day, c.month, c.year % 100
        );
        sentence(&b);
    }

    fn receive(&mut self) {
        let mut buf = [0u8; 256];
        loop {
            let n = adsb_rx::take(&mut buf);
            if n == 0 {
                break;
            }
            self.n.bytes += n as u32;
            let (c, after_via, last_fix) = (&mut self.n, &mut self.after_via, &mut self.last_fix);
            self.dec.feed(&buf[..n], |r| {
                let f = match r {
                    Ok(f) => f,
                    Err(LinkError::Corrupt) => {
                        c.corrupt += 1;
                        *after_via = false;
                        return;
                    }
                    Err(LinkError::Overflow) => {
                        c.overflow += 1;
                        *after_via = false;
                        return;
                    }
                };
                if matches!(f, Frame::Via { .. }) {
                    if *after_via {
                        c.orphan_via += 1;
                    }
                    *after_via = true;
                    c.via += 1;
                    return;
                }
                *after_via = false;
                match f {
                    Frame::Hello { .. } => c.hello += 1,
                    Frame::Stats { .. } => c.stats += 1,
                    Frame::Status { .. } => c.status += 1,
                    Frame::Detection(_) => c.detection += 1,
                    Frame::OpenLink(_) => c.open_link += 1,
                    Frame::HubFix(h) => {
                        c.hub_fix += 1;
                        *last_fix = Some(h);
                    }
                    Frame::Csbee(line) => match adsb::parse(&line) {
                        Ok(_) => c.csbee_ok += 1,
                        Err(_) => c.csbee_bad += 1,
                    },
                    _ => c.other_data += 1,
                }
            });
        }
    }

    pub fn print(&mut self) {
        let now = crate::app::now_ms();
        if now.saturating_sub(self.last_print_ms) < 5000 {
            return;
        }
        self.last_print_ms = now;
        let c = &self.n;
        crate::println!(
            "hub-p4: webheadloop rx bytes={} via={} hello={} stats={} status={} det={} open={} other={} hubfix={} csbee_ok={} csbee_bad={} corrupt={} overflow={} orphan_via={} uart4_err={} ring_full={}",
            c.bytes, c.via, c.hello, c.stats, c.status, c.detection, c.open_link, c.other_data, c.hub_fix,
            c.csbee_ok, c.csbee_bad, c.corrupt, c.overflow, c.orphan_via,
            adsb_rx::UART_ERR.load(core::sync::atomic::Ordering::Relaxed),
            adsb_rx::RING_FULL.load(core::sync::atomic::Ordering::Relaxed)
        );
        if let Some(h) = self.last_fix {
            let pos = h.position.map(|p| (f64::from(p.lat_e7) / 1e7, f64::from(p.lon_e7) / 1e7, p.alt_hae_mm, p.age_ms));
            let near = h.position.is_some_and(|p| {
                (f64::from(p.lat_e7) / 1e7 - LAT).abs() < 1e-5 && (f64::from(p.lon_e7) / 1e7 - LON).abs() < 1e-5
            });
            crate::println!(
                "hub-p4: webheadloop last hubfix state={:?} sats={:?} pos={:?} test_point={} utc_ms={:?} pps={}",
                h.state, h.sats, pos, near, h.utc_ms, h.utc_pps
            );
        }
    }
}

fn test_aircraft() -> adsb::Aircraft {
    use adsb::flag;
    let flags = [flag::AIRBORNE, flag::BARO_ALT_VALID, flag::POSITION_VALID, flag::DIRECTION_VALID, flag::SPEED_VALID, flag::UPDATED_POSITION]
        .iter()
        .fold(0u32, |f, b| f | 1 << b);
    adsb::Aircraft {
        source: adsb::Source::ModeS,
        icao: 0xDE0003,
        flags,
        callsign: adsb::Callsign::new("TESTWH").unwrap_or_default(),
        squawk: Some(0o1200),
        category: 7,
        lat_deg: LAT + 0.01,
        lon_deg: LON + 0.01,
        baro_alt_ft: 1500,
        gnss_alt_ft: 0,
        direction_deg: 90.0,
        speed_kt: 80,
        baro_vrate_fpm: 0,
        gnss_vrate_fpm: 0,
        uat_emergency: 0,
        nicnac: 0,
        acdims: 0,
        version: 2,
        signal_dbm: -60,
        signal_quality: 6,
        frames_per_s: 2,
        es_frames_per_s: 4,
    }
}

fn test_stats(uptime_s: u32) -> adsb::Stats {
    adsb::Stats {
        version: adsb::PROTOCOL_VERSION,
        demods_per_s: 40,
        raw_squitters_per_s: 3,
        squitters_per_s: 2,
        raw_extended_squitters_per_s: 5,
        extended_squitters_per_s: 4,
        raw_uat_per_s: 0,
        uat_per_s: 0,
        aircraft: 1,
        tscal: 0,
        uptime_s,
    }
}
