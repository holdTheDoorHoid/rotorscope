//! Advertising-report intake: runs inside the BLE host's receive loop.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use bt_hci::param::{LeAdvReportsIter, LeExtAdvDataStatus, LeExtAdvReport, LeExtAdvReportsIter, PhyKind};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use rid::Transport;
use trouble_host::prelude::EventHandler;

/// Largest advertising payload we keep. A full 9-message pack is 234 bytes of
/// AD data (pack 228 + service-data header 6), plus room for a Flags structure.
pub const MAX_ADV: usize = 255;

/// Reports waiting for the decode task. Remote ID is a few adverts per second
/// per drone, so this only fills if the decode/output side stalls; when it
/// does we count a drop rather than block HCI.
pub const QUEUE_LEN: usize = 16;

pub static REPORTS: Channel<CriticalSectionRawMutex, Report, QUEUE_LEN> = Channel::new();

/// One advert that passed the Remote ID pre-check, copied out of the HCI
/// buffer (which the host reuses as soon as the handler returns).
#[derive(Clone)]
pub struct Report {
    pub transport: Transport,
    /// Display order (most significant byte first), unlike HCI's little-endian.
    pub addr: [u8; 6],
    pub rssi: i8,
    pub uptime_us: u64,
    /// The controller or our reassembly lost part of the data.
    pub truncated: bool,
    pub len: u8,
    pub data: [u8; MAX_ADV],
}

impl Report {
    pub fn data(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

/// Free-running counters, read by the stats task. Relaxed is enough: each is
/// independent and only ever incremented.
#[derive(Default)]
pub struct Counters {
    /// Every advertising report the controller delivered (all devices).
    pub adv: AtomicU32,
    /// ...of which legacy (BT4) PDUs, reported through extended scanning.
    pub legacy: AtomicU32,
    /// ...extended PDUs whose primary channel was the 1M PHY.
    pub ext_1m: AtomicU32,
    /// ...extended PDUs on the coded PHY (Long Range).
    pub coded: AtomicU32,
    /// Old-style LE Advertising Report events. Extended scanning should route
    /// legacy PDUs through the extended report instead, so this staying at
    /// zero is the proof that it does.
    pub old_reports: AtomicU32,
    /// HCI report entries the host could not parse.
    pub hci_err: AtomicU32,
    /// Reports that passed the pre-check and were queued.
    pub rid_queued: AtomicU32,
    /// Queue full: a Remote ID report was lost.
    pub drops: AtomicU32,
    /// Remote ID reports with missing data (controller truncation, or a
    /// fragment chain we could not complete).
    pub truncated: AtomicU32,
    /// Decode task: payload located by `rid::ble::extract`.
    pub rid_found: AtomicU32,
    /// Decode task: payload located but not splittable into messages.
    pub rid_err: AtomicU32,
    /// Output lines/frames dropped because the USB host or UART fell behind.
    pub usb_drops: AtomicU32,
    pub uart_drops: AtomicU32,
    /// `scan_ext` refused to start (logged too).
    pub scan_err: AtomicU32,
}

pub static COUNTERS: Counters = Counters {
    adv: AtomicU32::new(0),
    legacy: AtomicU32::new(0),
    ext_1m: AtomicU32::new(0),
    coded: AtomicU32::new(0),
    old_reports: AtomicU32::new(0),
    hci_err: AtomicU32::new(0),
    rid_queued: AtomicU32::new(0),
    drops: AtomicU32::new(0),
    truncated: AtomicU32::new(0),
    rid_found: AtomicU32::new(0),
    rid_err: AtomicU32::new(0),
    usb_drops: AtomicU32::new(0),
    uart_drops: AtomicU32::new(0),
    scan_err: AtomicU32::new(0),
};

pub fn bump(c: &AtomicU32) {
    c.fetch_add(1, Relaxed);
}

/// Does this advertising data carry a Remote ID service-data structure
/// (AD type 0x16, UUID 0xFFFA, app code 0x0D)?
///
/// Deliberately our own few lines rather than `rid::ble::extract`: it runs on
/// every advert in the room, must never panic, and has to work on the *first
/// fragment* of a chained extended advert, where the structure may run past
/// the end of the bytes we have. Bytes we have must match; bytes we don't are
/// given the benefit of the doubt.
pub fn looks_like_rid(data: &[u8]) -> bool {
    let mut i = 0;
    while let Some(&len) = data.get(i) {
        let len = usize::from(len);
        if len == 0 {
            return false; // zero length: the rest is padding
        }
        if data.get(i + 1) == Some(&rid::ble::AD_SERVICE_DATA_16) && len >= 4 {
            let uuid_ok = data.get(i + 2..i + 4).is_none_or(|u| u == rid::ble::UUID);
            let app_ok = data.get(i + 4).is_none_or(|&c| c == rid::ble::APP_CODE);
            if uuid_ok && app_ok {
                return true;
            }
        }
        i += 1 + len;
    }
    false
}

fn display_addr(raw: &[u8]) -> [u8; 6] {
    let mut a = [0u8; 6];
    for (dst, src) in a.iter_mut().zip(raw.iter().rev()) {
        *dst = *src;
    }
    a
}

fn uptime_us() -> u64 {
    embassy_time::Instant::now().as_micros()
}

/// A chained extended advert being put back together. One slot is enough:
/// the controller delivers a chain's fragments back to back, and the only
/// chains we keep are Remote ID ones, of which there are few.
struct Pending {
    active: bool,
    addr: [u8; 6],
    sid: u8,
    report: Report,
}

impl Pending {
    fn matches(&self, addr: &[u8; 6], sid: u8) -> bool {
        self.active && self.addr == *addr && self.sid == sid
    }

    /// Append, marking the report truncated if it would overflow.
    fn append(&mut self, data: &[u8]) {
        let r = &mut self.report;
        let at = usize::from(r.len);
        let n = data.len().min(MAX_ADV - at);
        r.data[at..at + n].copy_from_slice(&data[..n]);
        r.len += n as u8;
        if n < data.len() {
            r.truncated = true;
        }
    }
}

pub struct ScanHandler {
    pending: RefCell<Pending>,
}

impl Default for ScanHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanHandler {
    pub const fn new() -> Self {
        Self {
            pending: RefCell::new(Pending {
                active: false,
                addr: [0; 6],
                sid: 0,
                report: Report {
                    transport: Transport::BleLegacy,
                    addr: [0; 6],
                    rssi: 0,
                    uptime_us: 0,
                    truncated: false,
                    len: 0,
                    data: [0; MAX_ADV],
                },
            }),
        }
    }

    fn ext_report(&self, r: &LeExtAdvReport<'_>) {
        bump(&COUNTERS.adv);
        let coded = |p: Option<PhyKind>| matches!(p, Some(PhyKind::LeCoded | PhyKind::LeCodedS2));
        // F3411 "Long Range" is coded on both legs; treat either leg being
        // coded as Long Range since only a coded receiver could hear it.
        let transport = if r.event_kind.legacy() {
            bump(&COUNTERS.legacy);
            Transport::BleLegacy
        } else if coded(Some(r.primary_adv_phy)) || coded(r.secondary_adv_phy) {
            bump(&COUNTERS.coded);
            Transport::BleLongRange
        } else {
            bump(&COUNTERS.ext_1m);
            Transport::BleExtended
        };

        let addr = display_addr(r.addr.raw());
        let mut p = self.pending.borrow_mut();
        match r.event_kind.data_status() {
            LeExtAdvDataStatus::Complete => {
                if p.matches(&addr, r.adv_sid) {
                    p.append(r.data);
                    p.active = false;
                    enqueue(&p.report);
                } else if looks_like_rid(r.data) {
                    enqueue(&fresh(transport, addr, r.rssi, r.data, false));
                }
            }
            LeExtAdvDataStatus::IncompleteMoreExpected => {
                if p.matches(&addr, r.adv_sid) {
                    p.append(r.data);
                } else if looks_like_rid(r.data) {
                    if p.active {
                        // A second Remote ID chain started before the first
                        // finished; the first is lost.
                        bump(&COUNTERS.truncated);
                    }
                    p.active = true;
                    p.addr = addr;
                    p.sid = r.adv_sid;
                    p.report = fresh(transport, addr, r.rssi, r.data, false);
                }
            }
            LeExtAdvDataStatus::IncompleteTruncated | LeExtAdvDataStatus::Reserved => {
                // The controller gave up on the chain (missed AUX_CHAIN_IND).
                // Pass on what we have: a pack's leading messages may be whole.
                if p.matches(&addr, r.adv_sid) {
                    p.append(r.data);
                    p.active = false;
                    p.report.truncated = true;
                    enqueue(&p.report);
                } else if looks_like_rid(r.data) {
                    enqueue(&fresh(transport, addr, r.rssi, r.data, true));
                }
            }
        }
    }
}

fn fresh(transport: Transport, addr: [u8; 6], rssi: i8, data: &[u8], truncated: bool) -> Report {
    let n = data.len().min(MAX_ADV);
    let mut report = Report {
        transport,
        addr,
        rssi,
        uptime_us: uptime_us(),
        truncated: truncated || n < data.len(),
        len: n as u8,
        data: [0; MAX_ADV],
    };
    report.data[..n].copy_from_slice(&data[..n]);
    report
}

fn enqueue(report: &Report) {
    if report.truncated {
        bump(&COUNTERS.truncated);
    }
    match REPORTS.try_send(report.clone()) {
        Ok(()) => bump(&COUNTERS.rid_queued),
        Err(_) => bump(&COUNTERS.drops),
    }
}

impl EventHandler for ScanHandler {
    fn on_ext_adv_reports(&self, reports: LeExtAdvReportsIter) {
        for r in reports {
            match r {
                Ok(r) => self.ext_report(&r),
                Err(_) => bump(&COUNTERS.hci_err),
            }
        }
    }

    /// Only reachable if the controller falls back to legacy scanning, which
    /// `scan_ext` should rule out. Handled anyway so nothing is silently lost.
    fn on_adv_reports(&self, reports: LeAdvReportsIter) {
        for r in reports {
            let Ok(r) = r else {
                bump(&COUNTERS.hci_err);
                continue;
            };
            bump(&COUNTERS.adv);
            bump(&COUNTERS.old_reports);
            if looks_like_rid(r.data) {
                enqueue(&fresh(Transport::BleLegacy, display_addr(r.addr.raw()), r.rssi, r.data, false));
            }
        }
    }
}
