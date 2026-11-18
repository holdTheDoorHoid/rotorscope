//! Wi-Fi detections besides Remote ID, from management frames the drain task
//! already has in hand:
//!
//! - **DJI legacy Wi-Fi DroneID**: a vendor element with OUI `26:37:12` in
//!   the beacons of older Wi-Fi DJI aircraft (`rid::dji`). Sent raw as
//!   `Frame::DjiWifi` (the element body from the OUI on; the hub decodes it
//!   with `rid::dji::parse_element`). The node never decodes it, so it never
//!   prints a drone serial.
//! - **Wi-Fi drones**: beacons and probe responses whose transmitter address
//!   or network name looks like a drone maker's (`crates/wifidrone`), sent as
//!   `Frame::WifiDrone`, with the maker's index in that table.
//!
//! Both are rate-limited per transmitter *and content*
//! (`wifiwatch::contentlimit`, [`GAP_US`]): a beacon comes ten times a
//! second, the hub needs one a second, but frames forged from a drone's own
//! address with other contents (a false position) must not take the drone's
//! place. The content is the element body (DJI) or the maker and network
//! name (Wi-Fi drone). An address whose reports go back and forth between
//! contents is flagged: `"alt":true` on its USB lines, and the stats line's
//! `alt` counts the alternations. Then each kind's budget
//! (`wifiwatch::shed`) caps the total, however many addresses send.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use wifiwatch::contentlimit::ContentLimit;
use wifiwatch::shed::Kind;
use wifiwatch::util::{hash, ms};

use crate::report;
use crate::rx::RxFrame;

/// Shortest gap between two reports of one transmitter's one content, per kind.
pub const GAP_US: u64 = 1_000_000;
/// (transmitter, content) pairs remembered per kind; the stalest is
/// replaced when full. 20 bytes each.
const TRACKED: usize = 24;
/// Contents forwarded per transmitter per gap: a forger holding one place
/// leaves the drone the other.
const PER_SOURCE: u8 = 2;

pub static DJI: AtomicU32 = AtomicU32::new(0);
pub static WIFI_DRONES: AtomicU32 = AtomicU32::new(0);
/// Report alternations (two transmitters on one address), per kind.
pub static DJI_ALT: AtomicU32 = AtomicU32::new(0);
pub static WIFI_DRONE_ALT: AtomicU32 = AtomicU32::new(0);

const DJI_OUI: [u8; 3] = rid::dji::OUI;
const SUBTYPE_PROBE_RESP: u8 = 5;
const SUBTYPE_BEACON: u8 = 8;

/// Per-task state (lives in the drain task, no locking).
pub struct Detect {
    dji: ContentLimit<TRACKED>,
    drones: ContentLimit<TRACKED>,
}

impl Detect {
    pub const fn new() -> Self {
        let gap = (GAP_US / 1000) as u32;
        Self { dji: ContentLimit::new(gap, PER_SOURCE), drones: ContentLimit::new(gap, PER_SOURCE) }
    }

    /// Look at one management frame (`frame` = MPDU without FCS).
    pub fn frame(&mut self, f: &RxFrame, frame: &[u8]) {
        let Some((subtype, src, ies)) = mgmt_ies(frame) else { return };
        if subtype == SUBTYPE_BEACON {
            self.dji(f, src, ies);
        }
        self.wifi_drone(f, src, ies);
    }

    fn dji(&mut self, f: &RxFrame, src: [u8; 6], ies: &[u8]) {
        // Cheap pre-check before walking the elements.
        if !ies.windows(3).any(|w| w == DJI_OUI) {
            return;
        }
        let Some(body) = rid::wifi::elements(ies).find_map(|(id, b)| (id == 221 && b.starts_with(&DJI_OUI)).then_some(b))
        else {
            return;
        };
        // Vendor type, 2 bytes, subcommand: shorter is not a DroneID element.
        if body.len() < 7 {
            return;
        }
        let v = self.dji.check(src, hash(body), ms(f.up_us));
        DJI_ALT.store(self.dji.alternations, Relaxed);
        if !v.send || !report::admit(Kind::DjiWifi) {
            return;
        }
        DJI.fetch_add(1, Relaxed);
        report::dji_json(f, src, body, v.alternating);
        report::dji_hub(f, src, body);
    }

    fn wifi_drone(&mut self, f: &RxFrame, src: [u8; 6], ies: &[u8]) {
        let ssid = rid::wifi::elements(ies).find_map(|(id, b)| (id == 0).then_some(b)).unwrap_or(&[]);
        let Some(maker) = classify(&src, ssid) else { return };
        let content = hash(ssid) ^ u32::from(maker).wrapping_mul(0x9E37_79B9);
        let v = self.drones.check(src, content, ms(f.up_us));
        WIFI_DRONE_ALT.store(self.drones.alternations, Relaxed);
        if !v.send || !report::admit(Kind::WifiDrone) {
            return;
        }
        WIFI_DRONES.fetch_add(1, Relaxed);
        report::wifi_drone_json(f, maker, src, ssid, v.alternating);
        report::wifi_drone_hub(f, maker, src, ssid);
    }
}

/// Beacon / probe response: (subtype, transmitter address, elements).
fn mgmt_ies(frame: &[u8]) -> Option<(u8, [u8; 6], &[u8])> {
    let fc0 = *frame.first()?;
    let fc1 = *frame.get(1)?;
    // Management (type 0), version 0, not protected.
    if fc0 & 0x0F != 0 || fc1 & 0x40 != 0 {
        return None;
    }
    let subtype = fc0 >> 4;
    if subtype != SUBTYPE_BEACON && subtype != SUBTYPE_PROBE_RESP {
        return None;
    }
    // Order bit: a 4-byte HT Control field follows the header.
    let header = if fc1 & 0x80 != 0 { 28 } else { 24 };
    let src = frame.get(10..16)?.try_into().ok()?;
    // Timestamp (8), interval (2), capabilities (2), then elements.
    Some((subtype, src, frame.get(header + 12..)?))
}

/// The maker this network probably belongs to (index into the hub's copy of
/// the same table, `crates/wifidrone`).
fn classify(mac: &[u8; 6], ssid: &[u8]) -> Option<u8> {
    wifidrone::classify(mac, ssid)
}
