//! Bench only (`floodtest`, docs/BENCH.md "Frame flood"): synthetic frames
//! pushed into this node's own receive path (`rx::receive`, where the radio
//! callback puts real ones) at a rate typed on USB. **Nothing is
//! transmitted**: the frames exist only inside this node. Type a whole line
//! and Enter:
//!
//! - `flood rid <n>`: `n` Remote ID beacons a second, each from a new random
//!   address (id `FLOODFAKE0000000001`);
//! - `flood tool <n>`: beacons named `ESP_RIDS`, `pwned`, `GhostNet` in turn
//!   (attack-tool signatures), random addresses;
//! - `flood drone <n>`: beacons from a drone maker's address block (DJI)
//!   named `MavicAir-FLOOD`, random low address bytes;
//! - `flood dji <n>`: beacons carrying a DJI DroneID element (OUI
//!   26:37:12, serial `FLOODDJI00000001`), random addresses;
//! - `flood wfb <n>`: wfb-ng data frames, 64 link ids `0x7E57C0`.. in turn;
//! - `flood spoof <n>`: DJI DroneID elements from the real drone's address
//!   ([`REAL_MAC`]) with one fixed false position;
//! - `flood real on|off`: the "real" drone at [`REAL_MAC`]: one Remote ID
//!   beacon a second (id `FLOODREAL0000000001`) and one DJI DroneID element
//!   a second (serial `FLOODREAL0000001`, sequence number moving on);
//! - `flood off`: everything off; `flood`: print the state now.
//!
//! Rates are per second, up to [`MAX_RATE`] per kind, and kinds mix. Every
//! second while anything runs, a line
//! `{"t":"flood","rate":{...},"sent":{...},"real":{...}}`: frames injected
//! per kind (totals), and for the real drone's Remote ID: injected, reached
//! the drain task (not lost to a full receive pool), admitted, queued for
//! the hub. Everything is marked as a test (FLOOD... identities, link ids
//! starting 7E57).

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_time::{Duration, Instant, Ticker};
use rid::encode;
use rid::message::{
    BasicId, FixedStr, HeightType, HorizontalAccuracy, IdType, LatLon, Location, Message, SpeedAccuracy, Status, UaType,
    VerticalAccuracy,
};

use crate::rx::{self, RxKind};
use crate::usb::usb_line;

/// The real drone's address ("REAL", locally administered).
pub const REAL_MAC: [u8; 6] = [0x02, 0x52, 0x45, 0x41, 0x4C, 0x01];
/// Most frames a second per kind.
pub const MAX_RATE: u32 = 5000;
const TICK_MS: u64 = 10;

const KINDS: usize = 6;
const NAMES: [&str; KINDS] = ["rid", "tool", "drone", "dji", "wfb", "spoof"];

/// The real drone's Remote ID frames: injected, seen by the drain task,
/// admitted (`report::rid_admit`), queued for the hub.
pub static REAL_SENT: AtomicU32 = AtomicU32::new(0);
pub static REAL_SEEN: AtomicU32 = AtomicU32::new(0);
pub static REAL_ADMITTED: AtomicU32 = AtomicU32::new(0);
pub static REAL_QUEUED: AtomicU32 = AtomicU32::new(0);
/// The real drone's DJI elements injected.
pub static REAL_DJI_SENT: AtomicU32 = AtomicU32::new(0);

/// Called by the drain task for every Remote ID frame it decodes.
pub fn note_rid(src: [u8; 6], admitted: bool, queued: bool) {
    if src != REAL_MAC {
        return;
    }
    REAL_SEEN.fetch_add(1, Relaxed);
    if admitted {
        REAL_ADMITTED.fetch_add(1, Relaxed);
    }
    if queued {
        REAL_QUEUED.fetch_add(1, Relaxed);
    }
}

/// xorshift32, seeded from the hardware RNG.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// A random unicast address with `first` as its first byte.
    fn mac(&mut self, first: u8) -> [u8; 6] {
        let a = self.next().to_le_bytes();
        let b = self.next().to_le_bytes();
        [first & 0xFE, a[0], a[1], a[2], b[0], b[1]]
    }
}

const FRAME: usize = 400;

/// A Remote ID Message Pack: Basic ID `id` and a Location.
fn rid_pack(id: &[u8], lat: f64, lon: f64, out: &mut [u8]) -> Option<usize> {
    let basic = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(id).ok()?,
    });
    let location = Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveGround,
        direction: Some(90.0),
        speed_horizontal: Some(5.0),
        speed_vertical: Some(0.0),
        position: Some(LatLon { lat, lon }),
        altitude_pressure: None,
        altitude_geodetic: Some(60.0),
        height: Some(40.0),
        horizontal_accuracy: HorizontalAccuracy::M10,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::Unknown,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: None,
        timestamp_accuracy: None,
    });
    let msgs = [encode::message(&basic).ok()?, encode::message(&location).ok()?];
    encode::pack(&msgs, out).ok()
}

/// A beacon (no FCS yet) from `src` named `ssid` with one vendor element
/// whose body is `vendor` (from the OUI on), or none.
fn beacon(src: [u8; 6], ssid: &[u8], vendor: Option<&[u8]>, out: &mut [u8; FRAME]) -> usize {
    let mut n = 0;
    let mut put = |b: &[u8]| {
        out[n..n + b.len()].copy_from_slice(b);
        n += b.len();
    };
    put(&[0x80, 0, 0, 0]);
    put(&[0xFF; 6]);
    put(&src);
    put(&src);
    put(&[0, 0]);
    put(&[0; 8]); // timestamp
    put(&100u16.to_le_bytes());
    put(&0x0421u16.to_le_bytes());
    put(&[0, ssid.len() as u8]);
    put(ssid);
    put(&[1, 1, 0x8C]);
    if let Some(v) = vendor {
        put(&[221, v.len() as u8]);
        put(v);
    }
    n
}

/// A DJI DroneID flight-information element body.
fn dji_body(serial: &[u8], seq: u16, lat: f64, lon: f64, out: &mut [u8]) -> Option<usize> {
    let info = rid::dji::FlightInfo {
        version: 2,
        seq,
        state: 0,
        serial: FixedStr::new(serial).ok()?,
        position: Some(LatLon { lat, lon }),
        ext: None,
    };
    encode::dji_flight_info(&info, out).ok()
}

/// Inject `frame[..n]` plus an FCS slot (the callback's frames carry one).
fn inject(kind: RxKind, frame: &mut [u8; FRAME], n: usize) {
    let end = n + crate::chip::FCS_LEN;
    frame[n..end].fill(0);
    rx::receive(kind, &frame[..end], -50, rx::TUNED_CHANNEL.load(Relaxed));
}

struct Injector {
    rng: Rng,
    rates: [u32; KINDS],
    acc: [u32; KINDS],
    sent: [u32; KINDS],
    real: bool,
    real_next: Instant,
    fake_pack: [u8; 64],
    fake_len: usize,
    real_pack: [u8; 64],
    real_len: usize,
    drone_prefix: [u8; 3],
    n: u32,
}

impl Injector {
    fn one(&mut self, k: usize, f: &mut [u8; FRAME]) {
        self.n = self.n.wrapping_add(1);
        let n = self.n;
        let mut body = [0u8; 128];
        let len = match k {
            // Remote ID from a new random address each time.
            0 => {
                let src = self.rng.mac(0x02);
                match encode::beacon_frame(src, b"FLOOD", 6, n as u8, &self.fake_pack[..self.fake_len], f) {
                    Ok(len) => len,
                    Err(_) => return,
                }
            }
            1 => {
                const TOOLS: [&[u8]; 3] = [b"ESP_RIDS", b"pwned", b"GhostNet"];
                let src = self.rng.mac(0x02);
                beacon(src, TOOLS[n as usize % 3], None, f)
            }
            2 => {
                let mut src = self.rng.mac(0);
                src[..3].copy_from_slice(&self.drone_prefix);
                beacon(src, b"MavicAir-FLOOD", None, f)
            }
            3 => {
                let src = self.rng.mac(0x02);
                let Some(b) = dji_body(b"FLOODDJI00000001", n as u16, 40.0, -75.0, &mut body) else { return };
                beacon(src, b"FLOOD", Some(&body[..b]), f)
            }
            4 => {
                let Some(len) = openlink::build::fake_frame(openlink::System::WfbNg, n, &mut f[..]) else { return };
                // A2 = A3 = 57:42 + (link id << 8 | port), big-endian.
                let id = 0x7E_57C0u32 + (n % 64);
                let ch = ((id << 8) | u32::from(openlink::build::FAKE_WFB_PORT)).to_be_bytes();
                for at in [12, 18] {
                    f[at..at + 4].copy_from_slice(&ch);
                }
                self.sent[k] = self.sent[k].wrapping_add(1);
                inject(RxKind::Data, f, len);
                return;
            }
            _ => {
                // The forger: the real drone's address, one false position.
                let Some(b) = dji_body(b"FLOODREAL0000001", 0, 1.0, 1.0, &mut body) else { return };
                beacon(REAL_MAC, b"FLOOD", Some(&body[..b]), f)
            }
        };
        self.sent[k] = self.sent[k].wrapping_add(1);
        inject(RxKind::Mgmt, f, len);
    }

    fn real(&mut self, f: &mut [u8; FRAME]) {
        let n = REAL_SENT.fetch_add(1, Relaxed);
        if let Ok(len) = encode::beacon_frame(REAL_MAC, b"FLOOD", 6, n as u8, &self.real_pack[..self.real_len], f) {
            inject(RxKind::Mgmt, f, len);
        }
        let mut body = [0u8; 128];
        if let Some(b) = dji_body(b"FLOODREAL0000001", n as u16, 39.95, -75.16, &mut body) {
            let len = beacon(REAL_MAC, b"FLOOD", Some(&body[..b]), f);
            REAL_DJI_SENT.fetch_add(1, Relaxed);
            inject(RxKind::Mgmt, f, len);
        }
    }

    fn tick(&mut self, f: &mut [u8; FRAME]) {
        if self.real && Instant::now() >= self.real_next {
            self.real_next += Duration::from_secs(1);
            self.real(f);
        }
        for k in 0..KINDS {
            self.acc[k] += self.rates[k] * TICK_MS as u32;
            while self.acc[k] >= 1000 {
                self.acc[k] -= 1000;
                self.one(k, f);
            }
        }
    }

    fn status(&self) {
        usb_line!(
            r#"{{"t":"flood","rate":{{"rid":{},"tool":{},"drone":{},"dji":{},"wfb":{},"spoof":{}}},"sent":{{"rid":{},"tool":{},"drone":{},"dji":{},"wfb":{},"spoof":{}}},"real":{{"on":{},"sent":{},"seen":{},"admitted":{},"queued":{},"dji_sent":{}}}}}"#,
            self.rates[0], self.rates[1], self.rates[2], self.rates[3], self.rates[4], self.rates[5],
            self.sent[0], self.sent[1], self.sent[2], self.sent[3], self.sent[4], self.sent[5],
            self.real,
            REAL_SENT.load(Relaxed),
            REAL_SEEN.load(Relaxed),
            REAL_ADMITTED.load(Relaxed),
            REAL_QUEUED.load(Relaxed),
            REAL_DJI_SENT.load(Relaxed)
        );
    }

    /// One console line. Returns false if it was not understood.
    fn command(&mut self, cmd: &str) -> bool {
        let mut w = cmd.split_ascii_whitespace();
        if w.next() != Some("flood") {
            return false;
        }
        match (w.next(), w.next()) {
            (None, _) => {}
            (Some("off"), None) => {
                self.rates = [0; KINDS];
                self.real = false;
            }
            (Some("real"), Some("on")) => {
                if !self.real {
                    self.real = true;
                    self.real_next = Instant::now();
                }
            }
            (Some("real"), Some("off")) => self.real = false,
            (Some(kind), Some(rate)) => {
                let Some(k) = NAMES.iter().position(|n| *n == kind) else { return false };
                let Ok(r) = rate.parse::<u32>() else { return false };
                self.rates[k] = r.min(MAX_RATE);
            }
            _ => return false,
        }
        self.status();
        true
    }
}

pub async fn console(mut read: impl FnMut(&mut [u8]) -> usize) -> ! {
    // The first 24-bit DJI block (an address `wifidrone` recognises).
    let drone_prefix = wifidrone::MAKERS[0]
        .prefixes
        .iter()
        .find(|p| p.bits == 24)
        .map(|p| [p.value[0], p.value[1], p.value[2]])
        .unwrap_or([0x60, 0x60, 0x1F]);
    let mut inj = Injector {
        rng: Rng(esp_hal::rng::Rng::new().random() | 1),
        rates: [0; KINDS],
        acc: [0; KINDS],
        sent: [0; KINDS],
        real: false,
        real_next: Instant::now(),
        fake_pack: [0; 64],
        fake_len: 0,
        real_pack: [0; 64],
        real_len: 0,
        drone_prefix,
        n: 0,
    };
    inj.fake_len = rid_pack(b"FLOODFAKE0000000001", 40.0, -75.0, &mut inj.fake_pack).unwrap_or(0);
    inj.real_len = rid_pack(b"FLOODREAL0000000001", 39.95, -75.16, &mut inj.real_pack).unwrap_or(0);
    let drone_ok = wifidrone::classify(&[drone_prefix[0], drone_prefix[1], drone_prefix[2], 1, 2, 3], b"MavicAir-FLOOD").is_some();
    usb_line!(
        r#"{{"t":"flood","note":"FLOOD TEST BUILD: synthetic frames into this node's receive path, nothing transmitted. Type 'flood rid|tool|drone|dji|wfb|spoof <per second>', 'flood real on|off', 'flood off' or 'flood' + Enter","packs_ok":{},"drone_ok":{drone_ok}}}"#,
        inj.fake_len > 0 && inj.real_len > 0
    );

    let mut frame = [0u8; FRAME];
    let mut line: heapless::Vec<u8, 48> = heapless::Vec::new();
    let mut buf = [0u8; 32];
    let mut ticker = Ticker::every(Duration::from_millis(TICK_MS));
    let mut last_status = Instant::now();
    loop {
        ticker.next().await;
        let n = read(&mut buf);
        for &b in &buf[..n] {
            if b != b'\r' && b != b'\n' {
                if line.push(b).is_err() {
                    line.clear();
                }
                continue;
            }
            let cmd = core::str::from_utf8(&line).unwrap_or("").trim();
            if !cmd.is_empty() && !inj.command(cmd) {
                usb_line!(r#"{{"t":"flood","err":"type 'flood <rid|tool|drone|dji|wfb|spoof> <per second>', 'flood real on|off', 'flood off' or 'flood'"}}"#);
            }
            line.clear();
        }
        inj.tick(&mut frame);
        let running = inj.real || inj.rates.iter().any(|&r| r > 0);
        if running && last_status.elapsed() >= Duration::from_secs(1) {
            last_status = Instant::now();
            inj.status();
        }
    }
}
