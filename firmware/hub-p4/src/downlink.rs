//! Hub-to-node commands: runs the coverage coordinator over the relayed node
//! table and sends each node its assignment through the ESP-NOW gateway
//! (`Command::To { node }` then `Command::Assign`).
//!
//! Every [`RUN_MS`] the coordinator decides; a changed assignment goes out at
//! once, and every assignment is resent every [`RESEND_MS`] even if
//! unchanged (commands are idempotent, and the gateway gives up on a node
//! that does not ACK after a few tries).

use coverage::{Coordinator, NodeInfo};
use esp_println::println;
use link::{Command, NodeRole};

use crate::links::{GatewayLink, MAX_RELAYED};

pub const RUN_MS: u64 = 2_000;
pub const RESEND_MS: u64 = 5_000;

pub struct Downlink {
    coord: Coordinator,
    last_run: Option<u64>,
    last_full: u64,
    /// Assignments queued for the gateway since boot.
    pub commands: u32,
}

impl Downlink {
    pub const fn new() -> Self {
        Self { coord: Coordinator::new(), last_run: None, last_full: 0, commands: 0 }
    }

    pub fn tick(&mut self, gw: &mut GatewayLink<'_>, now: u64) {
        if self.last_run.is_some_and(|t| now.saturating_sub(t) < RUN_MS) {
            return;
        }
        self.last_run = Some(now);
        let mut table: heapless::Vec<NodeInfo, MAX_RELAYED> = heapless::Vec::new();
        for n in gw.nodes.iter() {
            let _ = table.push(NodeInfo {
                key: n.mac,
                can: n.can,
                role: n.role,
                plan: n.plan,
                relayed: true,
                last_heard_ms: n.last_ms,
            });
        }
        let decisions = self.coord.run(&table, now);
        let full = now.saturating_sub(self.last_full) >= RESEND_MS;
        if full {
            self.last_full = now;
        }
        for n in gw.nodes.iter_mut() {
            let d = decisions.iter().find(|d| d.key == n.mac);
            n.assigned = d.map(|d| (d.assignment.role, d.assignment.id));
            n.applied = d.is_some_and(|d| d.applied);
        }
        for d in &decisions {
            if d.plan_changed {
                let m = d.key;
                println!(
                    "hub-p4: assign {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} -> {:?} plan {:04x} ({} hops){}",
                    m[0], m[1], m[2], m[3], m[4], m[5],
                    d.assignment.role,
                    d.assignment.id,
                    d.assignment.plan.len(),
                    if d.role_changed { ", role change" } else { "" }
                );
            }
            if !(full || d.plan_changed) {
                continue;
            }
            let mut buf = [0u8; link::MAX_FRAME + 16];
            let Ok(to) = link::encode_command(&Command::To { node: d.key }, &mut buf) else { continue };
            let to_len = to.len();
            let Ok(assign) = link::encode_command(&Command::Assign(d.assignment.clone()), &mut buf[to_len..]) else { continue };
            let len = to_len + assign.len();
            if gw.send(&buf[..len]) {
                self.commands += 1;
            }
        }
        if full {
            println!(
                "hub-p4: coverage nodes={} assigned={} applied={} missing={:#04x} (bits: 0 Wi-Fi 2.4, 1 Wi-Fi 5, 2 BLE, 3 FPV, 4 DroneID)",
                gw.nodes.len(),
                decisions.len(),
                decisions.iter().filter(|d| d.applied).count(),
                self.coord.missing
            );
        }
    }
}

/// Health-screen label for a role.
pub fn label(r: NodeRole) -> &'static str {
    match r {
        NodeRole::Wifi24 => "Wi-Fi 2.4",
        NodeRole::Wifi5 => "Wi-Fi 5",
        NodeRole::Ble => "Bluetooth",
        NodeRole::Fpv => "FPV",
        NodeRole::DroneId => "DroneID",
        NodeRole::Gateway => "Gateway",
    }
}
