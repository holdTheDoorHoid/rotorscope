//! The hub's commands (`link::Command::Assign`) and this node's status.
//!
//! This image does one role, Bluetooth. An `Assign(Ble)` only changes the
//! plan id the node echoes in `Frame::Status` (Bluetooth has no channel plan:
//! the controller scans all three advertising channels). Any other role is
//! another image: `node_common::Images::switch` stores the job, selects that
//! image's slot and resets.

use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use link::{Assignment, NodeRole};

use crate::out;

/// Commands from the hub (UART RX or ESP-NOW), newest last.
pub static COMMANDS: Channel<CriticalSectionRawMutex, Assignment, 2> = Channel::new();
/// Roles this board can take (`Frame::Status::can`), set once at boot.
pub static CAN: AtomicU8 = AtomicU8::new(NodeRole::Ble.bit());
/// `Assignment::id` applied (0 = none yet).
pub static PLAN_ID: AtomicU16 = AtomicU16::new(0);
/// Commands that changed the plan id / repeated it / could not be carried out.
pub static APPLIED: AtomicU32 = AtomicU32::new(0);
pub static REPEATS: AtomicU32 = AtomicU32::new(0);
pub static REJECTED: AtomicU32 = AtomicU32::new(0);

/// Bench only: build with `ROTORSCOPE_BENCH_FLIP=1` (tools/flash-node.sh
/// passes the environment through) and this image leaves Bluetooth and Wi-Fi
/// 2.4 out of the `can` it reports, while node-wifi's leaves out Wi-Fi 2.4.
/// The hub's coordinator then moves the node Bluetooth -> Wi-Fi 5 -> Bluetooth
/// ... once per 60 s role hold: hub-driven image switches in both directions
/// with one node, whatever else holds the 2.4 GHz slot. Never for a deployed node.
pub const BENCH_FLIP: bool = option_env!("ROTORSCOPE_BENCH_FLIP").is_some();

/// Bench only, see `node_common::bench_can_mask`: report only one role.
pub const BENCH_CAN: u8 = node_common::bench_can_mask(option_env!("ROTORSCOPE_BENCH_CAN"));

/// At boot: the board's roles, and the job this image was switched in for.
pub fn init(stored: Option<Assignment>, can: u8) {
    let hide = NodeRole::Ble.bit() | NodeRole::Wifi24.bit();
    let can = if BENCH_FLIP { can & !hide } else { can };
    CAN.store(can & BENCH_CAN, Relaxed);
    if let Some(a) = stored.filter(|a| a.role == NodeRole::Ble) {
        PLAN_ID.store(a.id, Relaxed);
    }
}

pub fn status_frame() -> link::Frame {
    link::Frame::Status { role: NodeRole::Ble, can: CAN.load(Relaxed), plan: PLAN_ID.load(Relaxed) }
}

#[embassy_executor::task]
pub async fn control(mut images: node_common::Images<'static>) {
    let mut line = [0u8; 160];
    loop {
        let a = COMMANDS.receive().await;
        if a.role == NodeRole::Ble {
            if PLAN_ID.swap(a.id, Relaxed) == a.id {
                REPEATS.fetch_add(1, Relaxed);
            } else {
                APPLIED.fetch_add(1, Relaxed);
                out::json_line(&out::AssignLine { t: "assign", role: a.role, plan_id: a.id }, &mut line);
                out::hub_frame(&status_frame());
            }
            continue;
        }
        out::json_line(&out::AssignLine { t: "switch", role: a.role, plan_id: a.id }, &mut line);
        // Let the line reach USB before the flash write and reset.
        Timer::after(Duration::from_millis(50)).await;
        let err = images.switch(&a);
        REJECTED.fetch_add(1, Relaxed);
        log::error!("switch to {:?} failed: {}", a.role, err.as_str());
    }
}
