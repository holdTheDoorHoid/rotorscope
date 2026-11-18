//! Bench only (`--features gpsloop` or `gpsloop-m10`): an on-chip loop-back
//! test of the GPS and ADS-B inputs, like the gateway self-test (BRINGUP
//! step 7). Nothing outside the chip is driven or needed; **leave the UART3-IN
//! header, GPIO29 and GPIO30 unconnected while it runs**.
//!
//! - **GPS.** UART2 plays the receiver: its TX drives the GPS RX pad
//!   (GPIO28) and its RX listens to the hub's GPS TX pad (GPIO27), so the
//!   hub's probe and configuration commands really leave UART1 and come back
//!   through the pads. `gpsloop`: a Quectel LC29H(AA) at 115200 that answers
//!   `$PQTMVERNO`, ACKs `$PAIR062` and `$PQTMCFGPPS`, and sends a dual-band
//!   NMEA 4.10 epoch (GGA, GSA x4, GSV, RMC) every second. It also ACKs
//!   `$PAIR391,1` (jamming detector) and then adds `$PAIRSPF,1` (no jamming)
//!   to every epoch, so the integrity monitor's Quectel input is exercised. `gpsloop-m10`: a
//!   u-blox MAX-M10S as shipped, NMEA at **9600**, that answers MON-VER,
//!   ACKs every CFG-VALSET, follows the baud change to 115200 and then sends
//!   UBX NAV-PVT, so the hub's baud hunt is exercised too.
//! - **PPS.** The PPS pad (GPIO29) is driven from inside the chip: high at
//!   the top of each emulated second (once the emulated module has a fix and,
//!   LC29H, PPS was configured), low 100 ms later.
//! - **ADS-B.** UART4's TX drives its own RX pad (GPIO30) with a CSBee
//!   aircraft line and a statistics line once a second.
//!
//! The emulated fix is a fixed test point; the aircraft is callsign
//! `TESTLOOP`, address DE0002, obviously made up.

use esp_hal::Blocking;
use esp_hal::time::Instant;
use esp_hal::uart::{Config, Uart};

use crate::gps;
use crate::uartring::adsb_rx;

const M10: bool = cfg!(feature = "gpsloop-m10");
/// The test point (lat, lon, metres above the ellipsoid).
const LAT: f64 = 39.952_6;
const LON: f64 = -75.165_2;
const HAE: f64 = 12.5;
/// Seconds of "searching" before the emulated module reports a fix.
const FIX_AFTER: u32 = 3;
const START_UTC_S: u64 = 1_794_787_200; // 2026-11-16T00:00:00Z

pub struct Loop {
    uart: Uart<'static, Blocking>,
    line: heapless::Vec<u8, 96>,
    ubx: heapless::Vec<u8, 128>,
    second: u32,
    next_epoch: Instant,
    pps_off: Option<Instant>,
    pps_configured: bool,
    /// LC29H: `PAIR391,1` received; `$PAIRSPF` every epoch.
    jam_detect: bool,
    /// M10: NMEA still on, UBX NAV-PVT on, current baud.
    nmea_on: bool,
    pvt_on: bool,
    baud: u32,
    pub cmds: u32,
    pub bad: u32,
}

fn xor(b: &[u8]) -> u8 {
    b.iter().fold(0, |a, &c| a ^ c)
}

impl Loop {
    pub fn new(uart2: esp_hal::peripherals::UART2<'static>) -> Self {
        // SAFETY: bench only. GPIO28 is the GPS RX pad (UART1 RX listens to
        // it); GPIO27 the GPS TX pad (UART1 drives it). UART2 drives 28 and
        // listens to 27: the pads are shared on purpose.
        let tx = unsafe { esp_hal::peripherals::GPIO28::steal() };
        let rx = unsafe { esp_hal::peripherals::GPIO27::steal() };
        let baud = if M10 { 9_600 } else { 115_200 };
        let uart = Uart::new(uart2, Config::default().with_baudrate(baud)).expect("UART2").with_tx(tx).with_rx(rx);
        crate::println!(
            "hub-p4: gpsloop: emulating {} on UART2 (TX GPIO28, RX GPIO27) at {baud}; PPS on GPIO29; CSBee on GPIO30",
            if M10 { "u-blox MAX-M10S" } else { "Quectel LC29H(AA)" }
        );
        Self {
            uart,
            line: heapless::Vec::new(),
            ubx: heapless::Vec::new(),
            second: 0,
            next_epoch: Instant::now() + esp_hal::time::Duration::from_millis(1000),
            pps_off: None,
            pps_configured: M10,
            jam_detect: false,
            nmea_on: true,
            pvt_on: false,
            baud,
            cmds: 0,
            bad: 0,
        }
    }

    fn send(&mut self, b: &[u8]) {
        // Blocking: bench only, and at most a few hundred bytes a second.
        let mut rest = b;
        while !rest.is_empty() {
            match self.uart.write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(_) => break,
            }
        }
    }

    fn sentence(&mut self, body: &str) {
        let mut out: heapless::String<128> = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(&mut out, format_args!("${body}*{:02X}\r\n", xor(body.as_bytes())));
        self.send(out.as_bytes());
    }

    pub fn tick(&mut self) {
        let mut buf = [0u8; 64];
        while let Ok(n @ 1..) = self.uart.read_buffered(&mut buf) {
            for &b in &buf[..n] {
                self.on_byte(b);
            }
        }
        if let Some(t) = self.pps_off
            && Instant::now() >= t
        {
            gps::pps_drive(false);
            self.pps_off = None;
        }
        if Instant::now() < self.next_epoch {
            return;
        }
        self.next_epoch += esp_hal::time::Duration::from_millis(1000);
        self.second += 1;
        let fixed = self.second > FIX_AFTER;
        if fixed && self.pps_configured {
            gps::pps_drive(true);
            self.pps_off = Some(Instant::now() + esp_hal::time::Duration::from_millis(100));
        }
        if M10 {
            if self.nmea_on {
                self.nmea_epoch(fixed);
            }
            if self.pvt_on {
                self.nav_pvt(fixed);
            }
        } else {
            self.nmea_epoch(fixed);
        }
        self.csbee();
    }

    fn on_byte(&mut self, b: u8) {
        // UBX: collect from B5 62 to the end of the frame.
        if !self.ubx.is_empty() || b == 0xB5 {
            let _ = self.ubx.push(b);
            let u = &self.ubx;
            if u.len() >= 2 && u[1] != 0x62 {
                self.ubx.clear();
            } else if u.len() >= 6 {
                let total = 8 + usize::from(u16::from_le_bytes([u[4], u[5]]));
                if total > self.ubx.capacity() {
                    self.ubx.clear();
                } else if u.len() == total {
                    let frame: heapless::Vec<u8, 128> = self.ubx.clone();
                    self.ubx.clear();
                    self.on_ubx(&frame);
                }
            }
            return;
        }
        match b {
            b'$' => {
                self.line.clear();
                let _ = self.line.push(b);
            }
            b'\n' => {
                let line: heapless::Vec<u8, 96> = self.line.clone();
                self.line.clear();
                self.on_line(&line);
            }
            b'\r' => {}
            _ if !self.line.is_empty() => {
                let _ = self.line.push(b);
            }
            _ => {}
        }
    }

    fn on_line(&mut self, line: &[u8]) {
        let Some(star) = line.iter().position(|&c| c == b'*') else { return };
        let body = &line[1..star];
        let cs = core::str::from_utf8(&line[star + 1..]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
        if cs != Some(xor(body)) {
            self.bad += 1;
            return;
        }
        self.cmds += 1;
        if M10 {
            return; // the M10 ignores $PQTMVERNO and any other NMEA input
        }
        let body = core::str::from_utf8(body).unwrap_or("");
        if body == "PQTMVERNO" {
            self.sentence("PQTMVERNO,LC29HAA-LOOPBACK,2026/11/15,00:00:00");
        } else if let Some(args) = body.strip_prefix("PAIR062,") {
            let ok = args.split(',').count() == 2;
            self.sentence(if ok { "PAIR001,062,0" } else { "PAIR001,062,4" });
        } else if let Some(args) = body.strip_prefix("PQTMCFGPPS,W,") {
            self.pps_configured = args.starts_with("1,1,");
            self.sentence("PQTMCFGPPS,OK");
        } else if let Some(arg) = body.strip_prefix("PAIR391,") {
            self.jam_detect = arg == "1";
            self.sentence("PAIR001,391,0");
        } else if let Some(cmd) = body.strip_prefix("PAIR").and_then(|r| r.get(..3)) {
            let mut b: heapless::String<24> = heapless::String::new();
            let _ = core::fmt::Write::write_fmt(&mut b, format_args!("PAIR001,{cmd},3"));
            self.sentence(&b);
        }
    }

    fn on_ubx(&mut self, f: &[u8]) {
        let (a, b) = gnss::fletcher(&f[2..f.len() - 2]);
        if a != f[f.len() - 2] || b != f[f.len() - 1] {
            self.bad += 1;
            return;
        }
        self.cmds += 1;
        if !M10 {
            return; // the LC29H does not speak UBX
        }
        let (class, id, p) = (f[2], f[3], &f[6..f.len() - 2]);
        let mut out = [0u8; 128];
        match (class, id) {
            (0x0A, 0x04) if p.is_empty() => {
                let mut pl = [0u8; 70];
                pl[..17].copy_from_slice(b"ROM SPG 5.10-LOOP");
                pl[30..38].copy_from_slice(b"000A0000");
                pl[40..53].copy_from_slice(b"PROTVER=34.10");
                if let Some(n) = gnss::build_frame(&mut out, 0x0A, 0x04, &pl) {
                    self.send(&out[..n]);
                }
            }
            (0x06, 0x8A) => {
                if let Some(n) = gnss::build_frame(&mut out, 0x05, 0x01, &[0x06, 0x8A]) {
                    self.send(&out[..n]);
                }
                // Items after the 4-byte header: key (u32) + value.
                let mut i = 4;
                let mut new_baud = None;
                while i + 4 <= p.len() {
                    let key = u32::from_le_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
                    let size = match (key >> 28) & 7 {
                        1 | 2 => 1,
                        3 => 2,
                        4 => 4,
                        _ => 8,
                    };
                    let v = &p[i + 4..(i + 4 + size).min(p.len())];
                    let val = v.iter().rev().fold(0u32, |a, &x| a << 8 | u32::from(x));
                    match key {
                        gnss::config::keys::UART1OUTPROT_NMEA => self.nmea_on = val != 0,
                        gnss::config::keys::MSGOUT_UBX_NAV_PVT_UART1 => self.pvt_on = val != 0,
                        gnss::config::keys::UART1_BAUDRATE => new_baud = Some(val),
                        _ => {}
                    }
                    i += 4 + size;
                }
                if let Some(b) = new_baud {
                    let _ = self.uart.flush();
                    // The ACK's last bits leave the shift register after the FIFO empties.
                    let t = Instant::now();
                    while t.elapsed().as_millis() < 3 {}
                    self.baud = b;
                    let _ = self.uart.apply_config(&Config::default().with_baudrate(b));
                    crate::println!("hub-p4: gpsloop: emulated M10 now at {b} baud");
                }
            }
            _ => {}
        }
    }

    fn utc_s(&self) -> u64 {
        START_UTC_S + u64::from(self.second)
    }

    fn nmea_epoch(&mut self, fixed: bool) {
        let c = crate::civil::civil(self.utc_s() * 1000);
        let mut t: heapless::String<16> = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(&mut t, format_args!("{:02}{:02}{:02}.000", c.hour, c.minute, c.second));
        let mut d: heapless::String<8> = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(&mut d, format_args!("{:02}{:02}{:02}", c.day, c.month, c.year % 100));
        let (lat, ns) = nmea_coord(LAT, 2, 'N', 'S');
        let (lon, ew) = nmea_coord(LON, 3, 'E', 'W');
        let mut b: heapless::String<128> = heapless::String::new();
        use core::fmt::Write;
        if fixed {
            let _ = write!(b, "GNGGA,{t},{lat},{ns},{lon},{ew},1,14,0.70,{:.3},M,-33.900,M,,", HAE + 33.9);
        } else {
            let _ = write!(b, "GNGGA,{t},,,,,0,00,99.99,,M,,M,,");
        }
        self.sentence(&b);
        if fixed {
            self.sentence("GNGSA,A,3,05,13,15,18,23,24,195,,,,,,1.20,0.70,0.97,1");
            self.sentence("GNGSA,A,3,74,75,84,,,,,,,,,,1.20,0.70,0.97,2");
            self.sentence("GNGSA,A,3,04,11,,,,,,,,,,,1.20,0.70,0.97,3");
            self.sentence("GNGSA,A,3,19,20,,,,,,,,,,,1.20,0.70,0.97,4");
        } else {
            self.sentence("GNGSA,A,1,,,,,,,,,,,,,,,,1");
        }
        self.sentence("GPGSV,2,1,07,05,60,045,44,13,40,120,41,15,30,200,38,18,25,300,36,1");
        self.sentence("GPGSV,2,2,07,23,15,080,33,24,10,330,30,195,55,190,40,1");
        self.sentence("GPGSV,1,1,03,05,60,045,38,13,40,120,35,195,55,190,36,8");
        self.sentence("GLGSV,1,1,03,74,50,100,39,75,35,160,37,84,20,250,31,1");
        self.sentence("GAGSV,1,1,02,04,45,070,40,11,30,310,37,7");
        self.sentence("GAGSV,1,1,02,04,45,070,36,11,30,310,33,1");
        self.sentence("GBGSV,1,1,02,19,70,010,42,20,40,140,39,1");
        self.sentence("GBGSV,1,1,02,19,70,010,38,20,40,140,35,5");
        b.clear();
        if fixed {
            let _ = write!(b, "GNRMC,{t},A,{lat},{ns},{lon},{ew},0.00,0.00,{d},,,A,V");
        } else {
            let _ = write!(b, "GNRMC,{t},V,,,,,,,{d},,,N,V");
        }
        self.sentence(&b);
        if self.jam_detect {
            self.sentence("PAIRSPF,1");
        }
    }

    fn nav_pvt(&mut self, fixed: bool) {
        let c = crate::civil::civil(self.utc_s() * 1000);
        let mut p = [0u8; 92];
        p[0..4].copy_from_slice(&((self.second % 604_800) * 1000).to_le_bytes());
        p[4..6].copy_from_slice(&(c.year as u16).to_le_bytes());
        p[6] = c.month;
        p[7] = c.day;
        p[8] = c.hour;
        p[9] = c.minute;
        p[10] = c.second;
        p[11] = if fixed { 0x07 } else { 0x00 };
        p[12..16].copy_from_slice(&30u32.to_le_bytes());
        p[20] = if fixed { 3 } else { 0 };
        p[21] = u8::from(fixed);
        p[23] = if fixed { 12 } else { 0 };
        p[24..28].copy_from_slice(&((LON * 1e7) as i32).to_le_bytes());
        p[28..32].copy_from_slice(&((LAT * 1e7) as i32).to_le_bytes());
        p[32..36].copy_from_slice(&((HAE * 1000.0) as i32).to_le_bytes());
        p[36..40].copy_from_slice(&(((HAE + 33.9) * 1000.0) as i32).to_le_bytes());
        p[40..44].copy_from_slice(&2_500u32.to_le_bytes());
        p[44..48].copy_from_slice(&4_000u32.to_le_bytes());
        p[76..78].copy_from_slice(&120u16.to_le_bytes());
        if !fixed {
            p[78] = 0x01; // invalidLlh
        }
        let mut out = [0u8; 100];
        if let Some(n) = gnss::build_frame(&mut out, 0x01, 0x07, &p) {
            self.send(&out[..n]);
        }
    }

    /// One CSBee aircraft line and one statistics line on UART4's own pad.
    fn csbee(&mut self) {
        use adsb::flag;
        let flags = [flag::AIRBORNE, flag::BARO_ALT_VALID, flag::POSITION_VALID, flag::DIRECTION_VALID, flag::SPEED_VALID, flag::UPDATED_POSITION]
            .iter()
            .fold(0u32, |f, b| f | 1 << b);
        let a = adsb::Aircraft {
            source: adsb::Source::ModeS,
            icao: 0xDE0002,
            flags,
            callsign: adsb::Callsign::new("TESTLOOP").unwrap_or_default(),
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
        };
        let stats = adsb::Stats {
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
            uptime_s: self.second,
        };
        let mut line = [0u8; adsb::MAX_LINE];
        if let Some(n) = adsb::encode::aircraft(&a, &mut line) {
            adsb_rx::write_all(&line[..n]);
        }
        if let Some(n) = adsb::encode::stats(&stats, &mut line) {
            adsb_rx::write_all(&line[..n]);
        }
    }
}

/// Degrees to NMEA `ddmm.mmmmmm` with hemisphere.
fn nmea_coord(v: f64, deg_digits: usize, pos: char, neg: char) -> (heapless::String<16>, char) {
    let a = if v < 0.0 { -v } else { v };
    let d = a as u32;
    let m = (a - f64::from(d)) * 60.0;
    let mut s: heapless::String<16> = heapless::String::new();
    let _ = if deg_digits == 2 {
        core::fmt::Write::write_fmt(&mut s, format_args!("{d:02}{m:09.6}"))
    } else {
        core::fmt::Write::write_fmt(&mut s, format_args!("{d:03}{m:09.6}"))
    };
    (s, if v < 0.0 { neg } else { pos })
}
