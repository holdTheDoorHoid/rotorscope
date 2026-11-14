//! What each role listens to, and how extra nodes in one role split the work.
//!
//! Everything a person might want to tune is a table at the top of this file.
//! Wi-Fi plans are `(channel, dwell ms)`; raw-capture plans (DroneID, FPV) are
//! `(centre frequency in 100 kHz units, dwell ms)`, so 5756.5 MHz is 57565.

use link::NodeRole;

use crate::Plan;

/// The order coverage slots are filled in, one node per slot. A node fills
/// the first slot it can (see `crate::Coordinator`); a role that appears
/// twice gets its second node only after every slot above it is full.
/// Nodes past the end keep the role they have (extra Bluetooth or Wi-Fi
/// nodes simply duplicate). At most [`crate::MAX_LADDER`] entries.
pub const LADDER: &[NodeRole] = &[
    NodeRole::Wifi24,  // 1. Remote ID on 2.4 GHz Wi-Fi (most Remote ID is here, channel 6)
    NodeRole::Ble,     // 2. Remote ID over Bluetooth
    NodeRole::Wifi5,   // 3. Remote ID and FPV links on 5 GHz Wi-Fi
    NodeRole::DroneId, // 4. DJI DroneID bursts
    NodeRole::Fpv,     // 5. Analog FPV video and RF fingerprints
    NodeRole::DroneId, // 6. Second DroneID: one node per band instead of alternating
    NodeRole::Wifi5,   // 7. Second 5 GHz: one parks on 149, one sweeps the rest
    NodeRole::Wifi24,  // 8. Second 2.4 GHz: one parks on 6, one sweeps 1-13
    NodeRole::Fpv,     // 9. Second FPV: each sweeps half the channels
    NodeRole::Ble,     // 10. Second Bluetooth: a duplicate (more chances to hear a packet)
];

// ------------------------------------------------------------- Wi-Fi 2.4 --

/// One 2.4 GHz node: today's plan (`firmware/node-wifi/src/chip.rs`
/// `PLAN_24`). Channel 6 ~75% of the time (NAN Remote ID is always there),
/// short visits to 1 and 11.
pub const WIFI24_ALONE: &[(u32, u16)] = &[(6, 150), (1, 50), (6, 150), (11, 50)];
/// With two or more: the first parks on 6.
pub const WIFI24_PARK: &[(u32, u16)] = &[(6, 1000)];
/// ...and the others share these, split into contiguous runs.
pub const WIFI24_SWEEP: &[u32] = &[1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13];
pub const WIFI24_SWEEP_MS: u16 = 80;

// --------------------------------------------------------------- Wi-Fi 5 --

/// One 5 GHz node: today's plan with UNII-1 included. 149 is the anchor
/// (150 ms between every visit), visits of 50 ms to 153-165 and 36-48.
pub const WIFI5_ANCHOR: u32 = 149;
pub const WIFI5_ANCHOR_MS: u16 = 150;
pub const WIFI5_VISITS: &[u32] = &[153, 157, 161, 165, 36, 40, 44, 48];
pub const WIFI5_VISIT_MS: u16 = 50;
/// With two or more: the first parks on 149...
pub const WIFI5_PARK: &[(u32, u16)] = &[(149, 1000)];
/// ...and the others share these.
pub const WIFI5_SWEEP: &[u32] = &[36, 40, 44, 48, 153, 157, 161, 165];
pub const WIFI5_SWEEP_MS: u16 = 100;

// --------------------------------------------------------------- DroneID --

/// DJI OcuSync DroneID burst centres, 100 kHz units. A burst repeats about
/// every 640 ms, so 700 ms on a frequency catches at least one.
pub const DRONEID_58: &[u32] = &[57565, 57765, 57965];
pub const DRONEID_24: &[u32] = &[24145, 24295, 24445, 24595, 23995];
pub const DRONEID_MS: u16 = 700;

// ------------------------------------------------------------------- FPV --

/// Analog FPV channels (bands A, B, E, F, R and L, docs/SIGNALS.md),
/// 100 kHz units, sorted, duplicates removed. One FPV node runs its own
/// built-in sweep (empty plan); two or more split this list.
pub const FPV_CHANNELS: &[u32] = &[
    53620, 53990, 54360, 54730, 55100, 55470, 55840, 56210, // L
    56450, 56580, 56650, 56850, 56950, 57050, 57250, 57320, 57330, 57400, 57450, 57520, 57600,
    57650, 57690, 57710, 57800, 57850, 57900, 58000, 58050, 58060, 58090, 58200, 58250, 58280,
    58400, 58430, 58450, 58470, 58600, 58650, 58660, 58800, 58850, 59050, 59170, 59250, 59450,
];
pub const FPV_MS: u16 = 60;

// ------------------------------------------------------- ESP-NOW nodes --

/// A node relayed through the ESP-NOW gateway can only send while tuned to
/// 2.4 GHz channel 6 (the gateway's channel). Every Wi-Fi plan for such a
/// node gets a visit to 6 so that it is never away from 6 for longer than
/// this.
pub const ESPNOW_MAX_AWAY_MS: u32 = 1000;
/// How long that visit lasts: long enough to send a batch (a send takes
/// ~2-10 ms, batches wait up to 50 ms) after the retune.
pub const ESPNOW_VISIT_MS: u16 = 40;
pub const ESPNOW_CHANNEL: u32 = 6;

/// Contiguous part `idx` of `parts` near-equal parts of `list`. More parts
/// than items: the extra parts repeat items (a duplicate is better than idle).
fn part(list: &[u32], parts: usize, idx: usize) -> &[u32] {
    let n = list.len();
    if parts == 0 || n == 0 {
        return list;
    }
    if parts > n {
        let i = idx % n;
        return &list[i..i + 1];
    }
    let (start, end) = (idx * n / parts, (idx + 1) * n / parts);
    &list[start..end]
}

fn push_all(plan: &mut Plan, hops: impl IntoIterator<Item = (u32, u16)>) {
    for h in hops {
        // Tables are sized to fit (tested); never panic on a bad edit.
        let _ = plan.push(h);
    }
}

/// The plan for node `idx` of `k` nodes in `role`, before the ESP-NOW rule.
pub fn base_plan(role: NodeRole, k: usize, idx: usize) -> Plan {
    let mut p = Plan::new();
    let k = k.max(1);
    match role {
        NodeRole::Wifi24 => match (k, idx) {
            (1, _) => push_all(&mut p, WIFI24_ALONE.iter().copied()),
            (_, 0) => push_all(&mut p, WIFI24_PARK.iter().copied()),
            (_, i) => push_all(&mut p, part(WIFI24_SWEEP, k - 1, i - 1).iter().map(|&c| (c, WIFI24_SWEEP_MS))),
        },
        NodeRole::Wifi5 => match (k, idx) {
            (1, _) => {
                for &c in WIFI5_VISITS {
                    push_all(&mut p, [(WIFI5_ANCHOR, WIFI5_ANCHOR_MS), (c, WIFI5_VISIT_MS)]);
                }
            }
            (_, 0) => push_all(&mut p, WIFI5_PARK.iter().copied()),
            (_, i) => push_all(&mut p, part(WIFI5_SWEEP, k - 1, i - 1).iter().map(|&c| (c, WIFI5_SWEEP_MS))),
        },
        NodeRole::DroneId => match (k, idx) {
            (1, _) => push_all(&mut p, DRONEID_58.iter().chain(DRONEID_24).map(|&f| (f, DRONEID_MS))),
            (_, 0) => push_all(&mut p, DRONEID_58.iter().map(|&f| (f, DRONEID_MS))),
            (_, i) => push_all(&mut p, part(DRONEID_24, k - 1, i - 1).iter().map(|&f| (f, DRONEID_MS))),
        },
        NodeRole::Fpv => {
            if k > 1 {
                push_all(&mut p, part(FPV_CHANNELS, k, idx).iter().map(|&f| (f, FPV_MS)));
            }
        }
        NodeRole::Ble | NodeRole::Gateway => {}
    }
    p
}

/// Whether a plan for `role` is a Wi-Fi channel plan.
pub const fn is_wifi(role: NodeRole) -> bool {
    matches!(role, NodeRole::Wifi24 | NodeRole::Wifi5)
}

/// Add channel-6 visits so the node is never away from 6 for more than
/// [`ESPNOW_MAX_AWAY_MS`], counting around the end of the cycle.
pub fn with_espnow_visits(plan: &Plan) -> Plan {
    let visit = (ESPNOW_CHANNEL, ESPNOW_VISIT_MS);
    let mut out = Plan::new();
    let mut away: u32 = 0;
    for &(ch, ms) in plan {
        if ch == ESPNOW_CHANNEL {
            let _ = out.push((ch, ms));
            away = 0;
            continue;
        }
        if away > 0 && away + u32::from(ms) > ESPNOW_MAX_AWAY_MS {
            let _ = out.push(visit);
            away = 0;
        }
        let _ = out.push((ch, ms));
        away += u32::from(ms);
    }
    // Around the end: time away before the first 6 plus after the last.
    let lead: u32 = out.iter().take_while(|h| h.0 != ESPNOW_CHANNEL).map(|h| u32::from(h.1)).sum();
    let has6 = out.iter().any(|h| h.0 == ESPNOW_CHANNEL);
    if !has6 || lead + away > ESPNOW_MAX_AWAY_MS {
        let _ = out.push(visit);
    }
    out
}

/// Longest time a plan stays away from channel 6, going round the cycle
/// twice so the wrap is counted. `None` if it never visits 6.
pub fn longest_away_from_6(plan: &[(u32, u16)]) -> Option<u32> {
    if !plan.iter().any(|h| h.0 == ESPNOW_CHANNEL) {
        return None;
    }
    let (mut away, mut worst) = (0u32, 0u32);
    for &(ch, ms) in plan.iter().chain(plan) {
        if ch == ESPNOW_CHANNEL {
            away = 0;
        } else {
            away += u32::from(ms);
            worst = worst.max(away);
        }
    }
    Some(worst)
}
