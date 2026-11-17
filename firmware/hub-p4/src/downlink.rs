//! Hub-to-node commands: runs the coverage coordinator over the relayed node
//! table and sends each node its assignment through the ESP-NOW gateway
//! (`Command::To { node }` then `Command::Assign`).
//!
//! Every [`RUN_MS`] the coordinator decides; a changed assignment goes out at
//! once, and every assignment is resent every [`RESEND_MS`] even if
//! unchanged (commands are idempotent, and the gateway gives up on a node
//! that does not ACK after a few tries).

use coverage::{Coordinator, NodeInfo};
use link::{AeroMode, Command, NodeRole};

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

    /// Console `a`: auto -> 1090 -> UAT -> FLARM -> OGN -> control links
    /// -> auto. Until reboot. Returns the new setting.
    #[inline(never)]
    pub fn cycle_aero(&mut self) -> Option<AeroMode> {
        let r = self.coord.aero.region;
        self.coord.aero.only = match self.coord.aero.only {
            None => Some(AeroMode::Es1090),
            Some(AeroMode::Es1090) => Some(AeroMode::Uat978),
            Some(AeroMode::Uat978) => Some(AeroMode::FlarmHop { band: r, ogn: false }),
            Some(AeroMode::FlarmHop { ogn: false, .. }) => Some(AeroMode::FlarmHop { band: r, ogn: true }),
            // The regions come from the coverage settings.
            Some(AeroMode::FlarmHop { ogn: true, .. }) => Some(AeroMode::Control { regions: 0, energy: 0 }),
            Some(AeroMode::Control { .. } | AeroMode::ControlFlags { .. }) => None,
        };
        self.last_run = None; // decide again now
        self.coord.aero.only
    }

    /// The two control-link opt-ins (SITE.TXT / console `L control ...`).
    /// True when they changed; the next run sends new plans.
    pub fn set_control(&mut self, read_ids: bool, decode_content: bool) -> bool {
        let c = &mut self.coord.aero.control;
        if (c.read_ids, c.decode_content) == (read_ids, decode_content) {
            return false;
        }
        c.read_ids = read_ids;
        c.decode_content = decode_content;
        self.last_run = None;
        true
    }

    /// `adsbee_1090`: the hub's ADSBee hears 1090 MHz, so aero nodes need not.
    pub fn tick(&mut self, gw: &mut GatewayLink<'_>, adsbee_1090: bool, now: u64) {
        if self.last_run.is_some_and(|t| now.saturating_sub(t) < RUN_MS) {
            return;
        }
        self.last_run = Some(now);
        if self.coord.aero.es1090_elsewhere != adsbee_1090 {
            println!(
                "hub-p4: coverage: ADSBee {} 1090 MHz, aero nodes {}",
                if adsbee_1090 { "hears" } else { "does not hear" },
                if adsbee_1090 { "leave it out" } else { "add it to their turns" }
            );
            self.coord.aero.es1090_elsewhere = adsbee_1090;
        }
        let mut table: heapless::Vec<NodeInfo, MAX_RELAYED> = heapless::Vec::new();
        for n in gw.nodes.iter() {
            let _ = table.push(NodeInfo {
                key: n.mac,
                can: n.can,
                role: n.role,
                plan: n.plan,
                // Over ESP-NOW (needs channel-6 visits to hear us), unless
                // the node is on the wired bus.
                relayed: !crate::bus::on_bus(&n.mac),
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

/// The aero override for the console and the evidence log.
pub fn aero_only_label(m: Option<AeroMode>) -> &'static str {
    match m {
        None => "auto (coverage default)",
        Some(AeroMode::Es1090) => "1090 ES only",
        Some(AeroMode::Uat978) => "UAT 978 only",
        Some(AeroMode::FlarmHop { ogn: false, .. }) => "FLARM hop only",
        Some(AeroMode::FlarmHop { ogn: true, .. }) => "OGN hop only",
        Some(AeroMode::Control { .. } | AeroMode::ControlFlags { .. }) => "control links only",
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
        NodeRole::Aero => "LR radio",
    }
}
