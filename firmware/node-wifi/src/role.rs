//! What this node is doing (role, channel plan, plan id), what the board can
//! do, and the hub's commands (`link::Command::Assign`).
//!
//! Commands arrive from the ESP-NOW gateway (`uplink`) or, on a wired node,
//! on the hub UART's RX pin (`report::hub_reader`), and queue in
//! [`COMMANDS`]. The [`control`] task applies a Wi-Fi job at once (the
//! hopper restarts on the new plan: [`CHANGED`]) and answers with a
//! `Frame::Status` echoing the plan id; any other role means another image
//! (`node_common::Images::switch`: store the job, select its slot, reset).
//!
//! Protocol 13: a Wi-Fi plan may end with a `ControlFlags` entry (the hub's
//! `control content` opt-in); it switches the MAVLink scan on open Wi-Fi
//! ([`content_from`], `rx::MAV_ON`) and is not a hop. Off at boot and
//! whenever the hub's plan has no such entry.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use link::{Assignment, NodeRole};

use crate::chip::{self, Hop};
use crate::report;

/// Hops in a plan: the hub's 24, plus room for the channel-6 visits an
/// ESP-NOW node adds.
pub const MAX_HOPS: usize = 32;
pub type Plan = heapless::Vec<Hop, MAX_HOPS>;

/// Shortest and longest dwell accepted from a plan, ms. Shorter than ~10 ms
/// is mostly retune time; a plan error should not park the node for minutes.
const MIN_DWELL_MS: u16 = 10;
const MAX_DWELL_MS: u16 = 10_000;

#[derive(Clone)]
pub struct Job {
    pub role: NodeRole,
    pub plan: Plan,
    /// `Assignment::id` applied, 0 for the built-in plan.
    pub id: u16,
}

static JOB: Mutex<RefCell<Option<Job>>> = Mutex::new(RefCell::new(None));
/// The plan changed: the hopper drops its current dwell and starts the new plan.
pub static CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Commands from the hub, newest last. Two is plenty: the hub resends every 5 s.
pub static COMMANDS: Channel<CriticalSectionRawMutex, Assignment, 2> = Channel::new();
/// Roles this board can take (`Frame::Status::can`), set once at boot.
pub static CAN: AtomicU8 = AtomicU8::new(0);
/// Commands that changed the job / repeated the current one / could not be
/// carried out (bad plan, no image for the role).
pub static APPLIED: AtomicU32 = AtomicU32::new(0);
pub static REPEATS: AtomicU32 = AtomicU32::new(0);
pub static REJECTED: AtomicU32 = AtomicU32::new(0);

pub const fn is_wifi(role: NodeRole) -> bool {
    matches!(role, NodeRole::Wifi24 | NodeRole::Wifi5)
}

/// The node's own plan for a Wi-Fi role.
pub fn builtin(role: NodeRole) -> Plan {
    let base = if role == NodeRole::Wifi5 { chip::PLAN_5 } else { chip::PLAN_24 };
    let mut p = Plan::new();
    for &h in base {
        let _ = p.push(h);
    }
    espnow_rule(p)
}

/// The plan an assignment asks for; empty means the built-in one. A
/// protocol 13 `ControlFlags` entry is a setting, not a hop: skipped here,
/// read by [`content_from`].
pub fn plan_from(a: &Assignment) -> Result<Plan, &'static str> {
    let hops = || a.plan.iter().filter(|e| !link::is_control_flags_code(e.0));
    if hops().next().is_none() {
        return Ok(builtin(a.role));
    }
    let mut p = Plan::new();
    for &(ch, ms) in hops() {
        let valid = (1..=14).contains(&ch) || (32..=177).contains(&ch);
        if !valid {
            return Err("channel");
        }
        if cfg!(feature = "esp32s3") && ch > 14 {
            return Err("5 GHz channel on a 2.4 GHz chip");
        }
        let _ = p.push(Hop(ch as u8, u64::from(ms.clamp(MIN_DWELL_MS, MAX_DWELL_MS))));
    }
    Ok(espnow_rule(p))
}

/// Protocol 13: whether the hub's `control content` opt-in is on, from the
/// assignment's `ControlFlags` entry (none = off). It turns the MAVLink scan
/// on open Wi-Fi on (`rx::MAV_ON`, `wifiwatch::mavwifi`). `control ids` is
/// not used here: the Wi-Fi scan keys its links by network (BSSID).
pub fn content_from(a: &Assignment) -> bool {
    link::plan_control_flags(&a.plan).is_some_and(|(_, content)| content)
}

/// ESP-NOW nodes can only send on channel 6: make sure the plan visits it at
/// least once a second (the hub's plans already do; the built-in 5 GHz plan
/// does not). Same rule as the hub's coordinator (`coverage::plans`).
#[cfg(feature = "espnow")]
fn espnow_rule(p: Plan) -> Plan {
    let mut wide = coverage::Plan::new();
    for h in &p {
        let _ = wide.push((u32::from(h.channel()), h.dwell_ms() as u16));
    }
    let with = coverage::plans::with_espnow_visits(&wide);
    if with == wide {
        return p;
    }
    let mut out = Plan::new();
    for &(ch, ms) in &with {
        let _ = out.push(Hop(ch as u8, u64::from(ms)));
    }
    out
}
#[cfg(not(feature = "espnow"))]
fn espnow_rule(p: Plan) -> Plan {
    p
}

/// Bench only, see node-ble's `role::BENCH_FLIP`: leave Wi-Fi 2.4 out of `can`.
pub const BENCH_FLIP: bool = option_env!("ROTORSCOPE_BENCH_FLIP").is_some();

/// Bench only, see `node_common::bench_can_mask`: report only one role.
pub const BENCH_CAN: u8 = node_common::bench_can_mask(option_env!("ROTORSCOPE_BENCH_CAN"));

/// Set the job at boot: a stored job this image can run, else the default.
/// A stored job's `control content` is not taken: the MAVLink scan stays
/// off until the hub says otherwise (it resends every 5 s).
pub fn init(stored: Option<Assignment>, can: u8) {
    let can = if BENCH_FLIP { can & !NodeRole::Wifi24.bit() } else { can };
    CAN.store(can & BENCH_CAN, Relaxed);
    let job = stored
        .filter(|a| is_wifi(a.role) && chip::OWN_ROLES & a.role.bit() != 0)
        .and_then(|a| plan_from(&a).ok().map(|plan| Job { role: a.role, plan, id: a.id }))
        .unwrap_or_else(|| Job { role: chip::BOOT_ROLE, plan: builtin(chip::BOOT_ROLE), id: 0 });
    critical_section::with(|cs| *JOB.borrow_ref_mut(cs) = Some(job));
}

pub fn current() -> Job {
    critical_section::with(|cs| JOB.borrow_ref(cs).clone()).unwrap_or_else(|| Job {
        role: chip::BOOT_ROLE,
        plan: builtin(chip::BOOT_ROLE),
        id: 0,
    })
}

/// Apply hub commands; switch image for a role this image does not do.
#[embassy_executor::task]
pub async fn control(mut images: node_common::Images<'static>) {
    loop {
        let a = COMMANDS.receive().await;
        if is_wifi(a.role) && chip::OWN_ROLES & a.role.bit() != 0 {
            let plan = match plan_from(&a) {
                Ok(p) => p,
                Err(why) => {
                    REJECTED.fetch_add(1, Relaxed);
                    esp_println::println!(r#"{{"t":"err","what":"assign","why":"{why}","plan":{}}}"#, a.id);
                    continue;
                }
            };
            // Every accepted Wi-Fi command restates the opt-in (the hub
            // resends it every 5 s, and a changed setting is a new plan).
            crate::rx::set_mavlink(content_from(&a));
            let job = Job { role: a.role, plan, id: a.id };
            let changed = critical_section::with(|cs| {
                let mut cur = JOB.borrow_ref_mut(cs);
                let same = cur.as_ref().is_some_and(|c| c.role == job.role && c.id == job.id && c.plan == job.plan);
                if !same {
                    *cur = Some(job.clone());
                }
                !same
            });
            if !changed {
                REPEATS.fetch_add(1, Relaxed);
                continue;
            }
            APPLIED.fetch_add(1, Relaxed);
            CHANGED.signal(());
            report::assign_json(&job);
            report::status_hub();
            continue;
        }
        // Another image's role.
        report::switch_json(&a);
        // Let the line reach USB before the flash write and reset.
        Timer::after(Duration::from_millis(50)).await;
        let err = images.switch(&a);
        REJECTED.fetch_add(1, Relaxed);
        esp_println::println!(r#"{{"t":"err","what":"switch","role":"{:?}","why":"{}"}}"#, a.role, err.as_str());
    }
}
