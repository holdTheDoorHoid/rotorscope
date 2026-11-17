//! What the hub tells the node, and what the node tells back about itself.
//!
//! Commands arrive from whichever link the build has (UART, wired bus,
//! ESP-NOW) and land here: `Assign` replaces the plan (the radio task picks
//! it up), `TimeSync` feeds the UTC clock with its arrival time.

use core::cell::RefCell;

use aerohop::UtcClock;
use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use link::{AeroStatus, AeroTime, Assignment, NodeRole};
use portable_atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering::Relaxed};

use crate::out;

/// The hub's clock, as this node estimates it.
pub static CLOCK: Mutex<RefCell<UtcClock>> = Mutex::new(RefCell::new(UtcClock::new()));
/// A new plan for the radio task.
pub static PLAN: Signal<CriticalSectionRawMutex, Assignment> = Signal::new();
/// `Assignment::id` applied (0 = built-in plan).
pub static PLAN_ID: AtomicU16 = AtomicU16::new(0);
/// Assigns applied / repeated / refused (another role: this board has one image).
pub static APPLIED: AtomicU32 = AtomicU32::new(0);
pub static REPEATS: AtomicU32 = AtomicU32::new(0);
pub static REJECTED: AtomicU32 = AtomicU32::new(0);

/// The radio task's latest state, for `Frame::AeroStatus`.
pub static STATUS: Mutex<RefCell<AeroStatus>> = Mutex::new(RefCell::new(AeroStatus {
    mode: 0,
    freq_khz: 0,
    time: AeroTime::Unknown,
    parked: false,
    radio_ok: false,
    rx_restarts: 0,
    rejected: 0,
    rate_dropped: 0,
}));
/// Set when `STATUS` changed in a way the hub should hear now.
pub static STATUS_CHANGED: AtomicBool = AtomicBool::new(false);

pub fn now_us() -> u64 {
    Instant::now().as_micros()
}

/// A command from the hub, `at_us` = when it arrived (node uptime).
#[cfg_attr(feature = "wirebus", allow(dead_code))]
pub fn on_command(c: link::Command, at_us: u64) {
    match c {
        link::Command::Assign(a) => on_assign(a),
        link::Command::TimeSync { utc_ms, pps } => on_time(at_us, utc_ms, pps),
        _ => {}
    }
}

pub fn on_assign(a: Assignment) {
    if a.role != NodeRole::Aero {
        // One image on this board: nothing to switch to.
        REJECTED.fetch_add(1, Relaxed);
        return;
    }
    if PLAN_ID.swap(a.id, Relaxed) == a.id {
        REPEATS.fetch_add(1, Relaxed);
        return;
    }
    APPLIED.fetch_add(1, Relaxed);
    out::line(format_args!(r#"{{"t":"assign","plan_id":{},"entries":{}}}"#, a.id, a.plan.len()));
    PLAN.signal(a);
    out::hub_frame(&status_frame());
}

pub fn on_time(at_us: u64, utc_ms: u64, pps: bool) {
    out::bump(&out::COUNTERS.time_syncs);
    critical_section::with(|cs| CLOCK.borrow_ref_mut(cs).sync(at_us, utc_ms, pps));
}

/// UTC now, if known.
pub fn utc_ms(at_us: u64) -> Option<u64> {
    critical_section::with(|cs| CLOCK.borrow_ref(cs).now_ms(at_us))
}

pub fn time_state(at_us: u64) -> AeroTime {
    critical_section::with(|cs| {
        let c = CLOCK.borrow_ref(cs);
        match (c.known(at_us), c.pps()) {
            (false, _) => AeroTime::Unknown,
            (true, false) => AeroTime::Hub,
            (true, true) => AeroTime::HubPps,
        }
    })
}

pub fn status_frame() -> link::Frame {
    link::Frame::Status { role: NodeRole::Aero, can: NodeRole::Aero.bit(), plan: PLAN_ID.load(Relaxed) }
}

pub fn aero_status_frame() -> link::Frame {
    link::Frame::AeroStatus(critical_section::with(|cs| STATUS.borrow_ref(cs).clone()))
}

/// Update the shared status; flag it for sending if a field the hub acts
/// on (mode, time, parking, radio) changed.
pub fn set_status(f: impl FnOnce(&mut AeroStatus)) {
    let changed = critical_section::with(|cs| {
        let mut s = STATUS.borrow_ref_mut(cs);
        let before = (s.mode, s.time, s.parked, s.radio_ok);
        f(&mut s);
        before != (s.mode, s.time, s.parked, s.radio_ok)
    });
    if changed {
        STATUS_CHANGED.store(true, Relaxed);
    }
}
