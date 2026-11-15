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
//! Both are rate-limited per transmitter ([`GAP_US`]), like open-link
//! sightings: a beacon comes ten times a second, the hub needs one a second.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use crate::report;
use crate::rx::RxFrame;

/// Shortest gap between two reports of one transmitter, per kind.
pub const GAP_US: u64 = 1_000_000;
/// Transmitters remembered per kind; the stalest is replaced when full.
const TRACKED: usize = 16;

pub static DJI: AtomicU32 = AtomicU32::new(0);
pub static WIFI_DRONES: AtomicU32 = AtomicU32::new(0);

const DJI_OUI: [u8; 3] = rid::dji::OUI;
const SUBTYPE_PROBE_RESP: u8 = 5;
const SUBTYPE_BEACON: u8 = 8;

/// Last report time per transmitter.
struct RateLimit {
    seen: heapless::Vec<([u8; 6], u64), TRACKED>,
}

impl RateLimit {
    const fn new() -> Self {
        Self { seen: heapless::Vec::new() }
    }

    /// True (and remembered) if `mac` was not reported in the last `GAP_US`.
    fn allow(&mut self, mac: [u8; 6], now: u64) -> bool {
        if let Some(e) = self.seen.iter_mut().find(|e| e.0 == mac) {
            if now.saturating_sub(e.1) < GAP_US {
                return false;
            }
            e.1 = now;
            return true;
        }
        if self.seen.is_full() {
            // Replace the stalest.
            if let Some(i) = (0..self.seen.len()).min_by_key(|&i| self.seen[i].1) {
                self.seen.swap_remove(i);
            }
        }
        let _ = self.seen.push((mac, now));
        true
    }
}

/// Per-task state (lives in the drain task, no locking).
pub struct Detect {
    dji: RateLimit,
    drones: RateLimit,
}

impl Detect {
    pub const fn new() -> Self {
        Self { dji: RateLimit::new(), drones: RateLimit::new() }
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
        if body.len() < 7 || !self.dji.allow(src, f.up_us) {
            return;
        }
        DJI.fetch_add(1, Relaxed);
        report::dji_json(f, src, body);
        report::dji_hub(f, src, body);
    }

    fn wifi_drone(&mut self, f: &RxFrame, src: [u8; 6], ies: &[u8]) {
        let ssid = rid::wifi::elements(ies).find_map(|(id, b)| (id == 0).then_some(b)).unwrap_or(&[]);
        let Some(maker) = classify(&src, ssid) else { return };
        if !self.drones.allow(src, f.up_us) {
            return;
        }
        WIFI_DRONES.fetch_add(1, Relaxed);
        report::wifi_drone_json(f, maker, src, ssid);
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
