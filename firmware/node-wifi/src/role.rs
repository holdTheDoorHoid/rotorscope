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
//!
//! ESP-NOW builds (WP-13 section 7.2): every plan, from the hub or read back
//! from the stored job, gets its gateway-channel visits and is checked
//! against the coordinator's rule (`coverage::plans::node_espnow_plan`); one
//! that would still leave the node deaf is refused (the previous plan
//! stays, `node_common::health().plans_refused`). And the deaf fall-back:
//! no accepted gateway packet for a minute on a hub plan, and the node goes
//! back to its built-in plan (id 0). If the gateway is heard within the
//! next 30 s the plan is blamed and its id refused for 10 min, doubling
//! with each further blame up to 1 h; if not, the gateway was away and the
//! plan is taken again (`node_common::DeafWatch`).

use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use link::{Assignment, NodeRole};

use crate::chip::{self, Hop};
use crate::report;

/// Hops in a plan: the hub's 24, plus room for every channel-6 visit an
/// ESP-NOW node may add (`coverage::plans::NODE_MAX_HOPS`), so adding them
/// can never overflow. RAM: a `Hop` is 16 bytes, so a plan is 792 bytes
/// (it was 520 at 32 hops).
pub const MAX_HOPS: usize = coverage::plans::NODE_MAX_HOPS;
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
    // The built-in plans pass the rule (tested in coverage with the same
    // tables); if one ever did not, run it as it is rather than nothing.
    espnow_rule(&p).unwrap_or(p)
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
    espnow_rule(&p)
}

/// Protocol 13: whether the hub's `control content` opt-in is on, from the
/// assignment's `ControlFlags` entry (none = off). It turns the MAVLink scan
/// on open Wi-Fi on (`rx::MAV_ON`, `wifiwatch::mavwifi`). `control ids` is
/// not used here: the Wi-Fi scan keys its links by network (BSSID).
pub fn content_from(a: &Assignment) -> bool {
    link::plan_control_flags(&a.plan).is_some_and(|(_, content)| content)
}

/// ESP-NOW nodes can only send on the gateway channel
/// (`airlink::GATEWAY_CHANNEL`): make sure the plan visits it at least once
/// a second (the hub's plans already do; the built-in 5 GHz plan does not).
/// Same rule as the hub's coordinator (`coverage::plans`). `Err`: no visit
/// can fix this plan (a hop longer than the limit); refuse it.
#[cfg(feature = "espnow")]
fn espnow_rule(p: &Plan) -> Result<Plan, &'static str> {
    const _: () = assert!(coverage::plans::ESPNOW_CHANNEL == airlink::GATEWAY_CHANNEL as u32);
    let mut wide: heapless::Vec<(u32, u16), MAX_HOPS> = heapless::Vec::new();
    for h in p {
        // Dwells are clamped to MAX_DWELL_MS, so they fit a u16.
        wide.push((u32::from(h.channel()), h.dwell_ms().min(u64::from(u16::MAX)) as u16)).map_err(|_| "plan too long")?;
    }
    let with = coverage::plans::node_espnow_plan(&wide).map_err(|e| e.as_str())?;
    if with == wide {
        return Ok(p.clone());
    }
    Ok(with.iter().map(|&(ch, ms)| Hop(ch as u8, u64::from(ms))).collect())
}
#[cfg(not(feature = "espnow"))]
fn espnow_rule(p: &Plan) -> Result<Plan, &'static str> {
    Ok(p.clone())
}

/// Bench only, see node-ble's `role::BENCH_FLIP`: leave Wi-Fi 2.4 out of `can`.
pub const BENCH_FLIP: bool = option_env!("ROTORSCOPE_BENCH_FLIP").is_some();

/// Bench only, see `node_common::bench_can_mask`: report only one role.
pub const BENCH_CAN: u8 = node_common::bench_can_mask(option_env!("ROTORSCOPE_BENCH_CAN"));

/// Set the job at boot: a stored job this image can run, else the default.
/// A stored job's `control content` is not taken: the MAVLink scan stays
/// off until the hub says otherwise (it resends every 5 s). A stored plan
/// that breaks the ESP-NOW rule is refused (counted, said on USB): the node
/// starts on its built-in plan in the stored role.
pub fn init(stored: Option<Assignment>, can: u8) {
    let can = if BENCH_FLIP { can & !NodeRole::Wifi24.bit() } else { can };
    CAN.store(can & BENCH_CAN, Relaxed);
    let job = match stored.filter(|a| is_wifi(a.role) && chip::OWN_ROLES & a.role.bit() != 0) {
        Some(a) => match plan_from(&a) {
            Ok(plan) => Job { role: a.role, plan, id: a.id },
            Err(why) => {
                node_common::count_plan_refused();
                crate::usb::usb_line!(r#"{{"t":"err","what":"stored_job","why":"{why}","plan":{}}}"#, a.id);
                Job { role: a.role, plan: builtin(a.role), id: 0 }
            }
        },
        None => Job { role: chip::BOOT_ROLE, plan: builtin(chip::BOOT_ROLE), id: 0 },
    };
    critical_section::with(|cs| *JOB.borrow_ref_mut(cs) = Some(job));
}

pub fn current() -> Job {
    critical_section::with(|cs| JOB.borrow_ref(cs).clone()).unwrap_or_else(|| Job {
        role: chip::BOOT_ROLE,
        plan: builtin(chip::BOOT_ROLE),
        id: 0,
    })
}

/// Gateway packets the link verified so far (beacons, syncs, commands,
/// repeats, and genuine ones refused only for their epoch). Any change
/// means the gateway was heard.
#[cfg(feature = "espnow")]
fn gateway_heard() -> u32 {
    node_common::linkauth::heard()
}
#[cfg(not(feature = "espnow"))]
fn gateway_heard() -> u32 {
    0
}

/// Say a deaf-watch event on USB and act on it.
fn deaf_event(e: node_common::DeafEvent) {
    use node_common::DeafEvent;
    match e {
        DeafEvent::FellBack { plan } => {
            let cur = current();
            let job = Job { role: cur.role, plan: builtin(cur.role), id: 0 };
            critical_section::with(|cs| *JOB.borrow_ref_mut(cs) = Some(job.clone()));
            crate::rx::set_mavlink(false);
            crate::usb::usb_line!(
                r#"{{"t":"deaf","verdict":"fell_back","plan":{plan},"note":"no gateway packet for {} s on this plan: built-in plan; gateway heard within {} s blames the plan"}}"#,
                node_common::DEAF_MS / 1000,
                node_common::PROBATION_MS / 1000
            );
            CHANGED.signal(());
            report::assign_json(&job);
            report::status_hub();
        }
        DeafEvent::Blamed { plan, refuse_ms } => {
            crate::usb::usb_line!(r#"{{"t":"deaf","verdict":"blamed","plan":{plan},"refused_s":{}}}"#, refuse_ms / 1000);
        }
        DeafEvent::Outage { plan } => {
            crate::usb::usb_line!(r#"{{"t":"deaf","verdict":"outage","plan":{plan},"note":"gateway away, plan not blamed"}}"#);
        }
    }
}

/// The deaf fall-back, once a second (and before each command): feed the
/// watch what the uplink accepted, and act on what it says.
fn deaf_check(deaf: &mut node_common::DeafWatch, heard: &mut u32) {
    // Only a relayed node has a gateway to be deaf to.
    if !cfg!(feature = "espnow") {
        return;
    }
    let now = Instant::now().as_millis();
    let h = gateway_heard();
    if h != *heard {
        *heard = h;
        if let Some(e) = deaf.heard(now) {
            deaf_event(e);
        }
    }
    if let Some(e) = deaf.check(now, current().id) {
        deaf_event(e);
    }
    node_common::set_deaf_counts(deaf.counts());
}

/// Apply hub commands; switch image for a role this image does not do.
/// Between commands, once a second, the deaf fall-back.
#[embassy_executor::task]
pub async fn control(mut images: node_common::Images<'static>) {
    let mut deaf = node_common::DeafWatch::new(Instant::now().as_millis());
    let mut heard = gateway_heard();
    loop {
        let a = match with_timeout(Duration::from_secs(1), COMMANDS.receive()).await {
            Ok(a) => a,
            Err(_) => {
                deaf_check(&mut deaf, &mut heard);
                continue;
            }
        };
        // The command itself came from the gateway (or the wire).
        deaf_check(&mut deaf, &mut heard);
        if is_wifi(a.role) && chip::OWN_ROLES & a.role.bit() != 0 {
            let now = Instant::now().as_millis();
            if deaf.refuse_assign(now, a.id) {
                REJECTED.fetch_add(1, Relaxed);
                node_common::set_deaf_counts(deaf.counts());
                crate::usb::usb_line!(
                    r#"{{"t":"err","what":"assign","why":"plan id made this node deaf","plan":{},"refused_s":{}}}"#,
                    a.id,
                    deaf.refused_for_ms(now, a.id) / 1000
                );
                continue;
            }
            let plan = match plan_from(&a) {
                Ok(p) => p,
                Err(why) => {
                    REJECTED.fetch_add(1, Relaxed);
                    node_common::count_plan_refused();
                    crate::usb::usb_line!(r#"{{"t":"err","what":"assign","why":"{why}","plan":{}}}"#, a.id);
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
        // Another image's role. Too soon since the last switch: refused
        // (and counted) by `switch` without a word about switching.
        if node_common::health().switch_wait_s == 0 {
            report::switch_json(&a);
            // Let the line reach USB before the flash write and reset.
            Timer::after(Duration::from_millis(50)).await;
        }
        let err = images.switch(&a);
        REJECTED.fetch_add(1, Relaxed);
        crate::usb::usb_line!(
            r#"{{"t":"err","what":"switch","role":"{:?}","why":"{}","wait_s":{}}}"#,
            a.role,
            err.as_str(),
            node_common::health().switch_wait_s
        );
    }
}
