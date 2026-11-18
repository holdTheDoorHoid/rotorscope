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
/// ...and the others share these, split into contiguous runs. 12-14 are
/// included on purpose: US gear avoids them, so a drone or attack tool set
/// to a Japanese/European channel would otherwise never be heard (14 is
/// Japan's 802.11b-only channel). Receive-only, so any channel is fine.
pub const WIFI24_SWEEP: &[u32] = &[1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14];
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
/// ...and the others share these. 169-177 (UNII-4) are where some FPV links
/// and modified drones go to be alone; the DFS channels (52-144) are only
/// scouted ([`SCOUT_5`]): sweeping them all would not fit a 24-hop plan.
pub const WIFI5_SWEEP: &[u32] = &[36, 40, 44, 48, 153, 157, 161, 165, 169, 173, 177];
pub const WIFI5_SWEEP_MS: u16 = 100;

// -------------------------------------------------------- Wi-Fi scouting --

/// Every 2.4 GHz channel a node can receive, for scouting visits.
pub const SCOUT_24: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];
/// Every 20 MHz 5 GHz channel an ESP32-C5 can tune to: UNII-1 (36-48),
/// UNII-2A (52-64), UNII-2C (100-144), UNII-3 (149-165) and UNII-4
/// (169-177). Whether the C5's driver really tunes 169-177 is to be
/// checked on hardware (a refused channel shows in the node's `ch_err`).
pub const SCOUT_5: &[u8] = &[
    36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157,
    161, 165, 169, 173, 177,
];
/// A Wi-Fi node leaves its plan this often for one scouting visit to the
/// next channel of its band(s) that the plan does not cover (round robin,
/// [`scout_channel`]), so nothing can hide on a channel the plan skips:
/// a drone's network, an evil twin, a beacon flood. Costs
/// `SCOUT_MS / SCOUT_EVERY_MS` = 3.5% of air time.
pub const SCOUT_EVERY_MS: u32 = 3000;
/// One beacon interval (102.4 ms) and a little: every access point on the
/// channel is heard at least once per visit.
pub const SCOUT_MS: u16 = 105;

/// The `n`th scouting channel for a plan whose channels are `plan`: the
/// channels of the plan's band(s) ([`SCOUT_24`] if it has any 2.4 GHz
/// channel, [`SCOUT_5`] if it has any 5 GHz one) that the plan does not
/// visit, round robin. `None` if the plan already covers its bands.
pub fn scout_channel(plan: impl Iterator<Item = u8> + Clone, n: usize) -> Option<u8> {
    let has24 = plan.clone().any(|c| c <= 14);
    let has5 = plan.clone().any(|c| c > 14);
    let bands = [(has24, SCOUT_24), (has5, SCOUT_5)];
    let candidates = || {
        bands
            .iter()
            .filter(|b| b.0)
            .flat_map(|b| b.1.iter().copied())
            .filter(|&c| !plan.clone().any(|p| p == c))
    };
    let count = candidates().count();
    if count == 0 {
        return None;
    }
    candidates().nth(n % count)
}

// --------------------------------------------------------------- DroneID --

/// DJI OcuSync DroneID burst centres, 100 kHz units. A burst repeats about
/// every 640 ms, so 700 ms on a frequency catches at least one.
/// 5.8 GHz: every 10 MHz from 5746.5 to 5816.5; each dwell also covers the
/// channel 5 MHz below its own, so together they cover DroneID's 5 MHz
/// raster (5741.5-5816.5; Avata 2 recording, docs/DJI-LINKS.md). Must match
/// `firmware/node-sdr` `DRONEID_PLAN`.
pub const DRONEID_58: &[u32] = &[57465, 57565, 57665, 57765, 57865, 57965, 58065, 58165];
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
/// 2.4 GHz analog video channels (docs/SIGNALS.md section 3b: the 20 MHz
/// grid 2370-2510 and the 18 MHz camera grid 2411-2473), 100 kHz units,
/// sorted. A C5 reaches all of them.
pub const FPV_CHANNELS_24: &[u32] = &[
    23700, 23900, 24100, 24110, 24140, 24300, 24320, 24330, 24500, 24530, 24680, 24700, 24730, 24900, 25100,
];
/// Highest 5.8 GHz carrier a raw-capture C5 reaches (`fpvscan::channels::
/// REACH_MAX_MHZ`), 100 kHz units. Channels above it need an RX5808.
pub const FPV_C5_MAX: u32 = 58100;
/// Most channels in [`fpv_sweep`].
pub const FPV_SWEEP_MAX: usize = 64;

/// Off-plan channels every FPV node visits, one after each round of its
/// plan, rotating (100 kHz units). Not split between nodes and not part of
/// an assignment: the node adds them itself (`firmware/node-sdr`
/// `radio/jam.rs` `EDGE_MHZ`, which must match). They watch the parts of
/// the band a drone could use to slip past the main sweep:
///
/// - 5181-5241: the 5.170-5.250 GHz video band only DJI O4 (and some
///   Autel) products use; LO 5180-5240, inside the C5's measured reach.
/// - 5820-5945: the 5.8 GHz FPV channels above the main sweep's reach
///   ([`FPV_C5_MAX`]). Their LO (5819-5944 MHz) is above the highest one
///   verified on a chip (5816): UNVERIFIED until a bench run shows sane
///   floors there.
///
/// 2.3 GHz is deliberately absent (satellite radio and LTE live there as
/// continuous OFDM; see docs/JAMMING.md).
pub const FPV_EDGE: &[u32] = &[51810, 52010, 52210, 52410, 58200, 58400, 58600, 58800, 59050, 59250, 59450];
/// Highest LO verified on a chip (MHz): the DroneID plan's top hop.
pub const C5_LO_VERIFIED_MAX: u32 = 5816;

/// What FPV nodes split between them: every 2.4 GHz channel, then the
/// 5.8 GHz channels a C5 can reach (31 of 47; sending the others would
/// only be dropped by the node and unbalance the split). 46 in all, so two
/// nodes get 23 each, inside the 24-hop plan limit. One node runs the same
/// list as its built-in sweep (`fpvscan::band::sweep`).
pub fn fpv_sweep() -> heapless::Vec<u32, FPV_SWEEP_MAX> {
    FPV_CHANNELS_24.iter().chain(FPV_CHANNELS.iter().filter(|&&f| f <= FPV_C5_MAX)).copied().collect()
}

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
                push_all(&mut p, part(&fpv_sweep(), k, idx).iter().map(|&f| (f, FPV_MS)));
            }
        }
        NodeRole::Aero => p = aero_plan(k, idx, &AeroSettings::new()),
        NodeRole::Ble | NodeRole::Gateway => {}
    }
    p
}

// ------------------------------------------------------------------ Aero --
//
// An aero node (LR2021, docs/AERO-NODE.md) hears one aircraft signal at a
// time. Its plan entries are `(link::AeroMode::code(), dwell ms)`, visited
// in turn; a single entry (or dwell 0) stays.
//
// Default, one node: time-share 978 MHz UAT (US light aircraft below
// 18,000 ft, which the ADSBee 1090 cannot hear) and FLARM / OGN hopping
// (gliders, light aircraft, some drones), and add 1090 MHz only when the
// hub has no ADSBee hearing 1090 already. Several aero nodes split the
// same list, one or more modes each, so with enough of them each mode has
// a radio of its own.
//
// Protocol 12: the same radio also scans for drone control links
// (docs/CONTROL-LINKS.md, `AeroMode::Control`).
// - One node: aircraft and control links take turns, about half the time
//   each: one control entry as long as the aircraft turns together (25 s,
//   35 s with 1090). Aircraft beacons repeat every second or so, so 25 s
//   away loses little; a control link's ELRS sync packet comes every 5 s,
//   so a control turn of 25 s covers several plan steps.
// - Two or more: half the nodes (rounded down) do control links only, the
//   rest aircraft only. Two control nodes split the packet bands (sub-GHz
//   on one, 2.4 GHz on the other).
// - The two opt-ins (link IDs, decoded content) ride as one
//   `AeroMode::ControlFlags` entry at the end of any plan with a control
//   entry, only when one of them is on (no entry = both off).
// - Protocol 13: Wi-Fi plans get the same entry (`Coordinator::run`), last,
//   after the ESP-NOW visits, by the same rule: `control content` also
//   turns on the Wi-Fi nodes' MAVLink scan. The node takes it out of the
//   plan before hopping (`link::plan_control_flags`).

/// FLARM/OGN hopping region (SoftRF `RF_BAND_*`): 2 = USA/Canada (902-928
/// MHz). 1 = Europe (868 MHz), 3 = Australia/South America.
pub const AERO_REGION: u8 = 2;
/// Dwells when time-sharing. UAT aircraft send about once a second, FLARM
/// about once or twice a second, so 10 s catches each nearby aircraft
/// several times; OGN trackers are rarer in the US and get less.
pub const AERO_1090_MS: u16 = 10_000;
pub const AERO_UAT_MS: u16 = 10_000;
pub const AERO_FLARM_MS: u16 = 10_000;
pub const AERO_OGN_MS: u16 = 5_000;

/// Control-link regions scanned with packet plans by default (US):
/// 902-928 MHz, 433 MHz (US ELRS 433 domain, OpenLRSng) and 2.4 GHz.
pub const CONTROL_REGIONS_US: &[ctrlwatch::Region] =
    &[ctrlwatch::Region::Fcc915, ctrlwatch::Region::Us433, ctrlwatch::Region::Ism2g4];
/// Swept for energy only by default (US): 863-870 MHz (European gear
/// brought over; nothing legal transmits a control link there in the US).
pub const CONTROL_ENERGY_US: &[ctrlwatch::Region] = &[ctrlwatch::Region::Eu868];

/// A region list as an `AeroMode::Control` mask.
pub const fn region_mask(rs: &[ctrlwatch::Region]) -> u16 {
    let mut m = 0;
    let mut i = 0;
    while i < rs.len() {
        m |= link::control_region_bit(rs[i]);
        i += 1;
    }
    m
}

/// The control-link scan the hub asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlSettings {
    /// Scan for control links at all.
    pub enabled: bool,
    /// Regions with packet plans, and bands swept for energy only
    /// (`AeroMode::Control` masks).
    pub regions: u16,
    pub energy: u16,
    /// The opt-ins: off unless the owner turns them on (SITE.TXT
    /// `control ids on`, `control content on`).
    pub read_ids: bool,
    pub decode_content: bool,
}

impl ControlSettings {
    pub const fn us() -> Self {
        Self {
            enabled: true,
            regions: region_mask(CONTROL_REGIONS_US),
            energy: region_mask(CONTROL_ENERGY_US),
            read_ids: false,
            decode_content: false,
        }
    }

    /// The `AeroMode::ControlFlags` entry for these opt-ins, `None` when
    /// both are off. Aero plans end with it when they have a control entry;
    /// Wi-Fi plans always end with it (protocol 13: `control content`
    /// also turns on the MAVLink scan on open Wi-Fi, `wifiwatch::mavwifi`).
    pub fn flags_entry(&self) -> Option<(u32, u16)> {
        (self.read_ids || self.decode_content)
            .then(|| link::AeroMode::ControlFlags { read_ids: self.read_ids, decode_content: self.decode_content }.entry(0))
    }
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self::us()
    }
}

/// What the hub tells the coordinator about aero coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroSettings {
    /// Something else (the hub's ADSBee) already hears 1090 MHz: aero
    /// nodes leave it out.
    pub es1090_elsewhere: bool,
    /// Operator override: every aero node stays on this one mode.
    pub only: Option<link::AeroMode>,
    /// FLARM/OGN region.
    pub region: u8,
    /// Protocol 12: drone control links.
    pub control: ControlSettings,
}

impl AeroSettings {
    pub const fn new() -> Self {
        Self { es1090_elsewhere: false, only: None, region: AERO_REGION, control: ControlSettings::us() }
    }

    /// The control entry with this node's share of the regions.
    fn control_mode(&self, parts: usize, idx: usize) -> link::AeroMode {
        let c = &self.control;
        if parts <= 1 {
            return link::AeroMode::Control { regions: c.regions, energy: c.energy };
        }
        // Two or more control nodes: 2.4 GHz to the last, sub-GHz split
        // among the others; energy-only bands with the first.
        let g24 = link::control_region_bit(ctrlwatch::Region::Ism2g4);
        let sub: heapless::Vec<u32, 10> = (0..10).map(|i| 1u32 << i).filter(|b| c.regions as u32 & b & !(g24 as u32) != 0).collect();
        let (regions, energy) = if idx == parts - 1 && c.regions & g24 != 0 {
            (g24, 0)
        } else {
            let k = if c.regions & g24 != 0 { parts - 1 } else { parts };
            let mine = part(&sub, k.max(1), idx.min(k.saturating_sub(1)));
            (mine.iter().fold(0u16, |m, b| m | *b as u16), if idx == 0 { c.energy } else { 0 })
        };
        link::AeroMode::Control { regions, energy }
    }
}

impl Default for AeroSettings {
    fn default() -> Self {
        Self::new()
    }
}

/// The plan for aero node `idx` of `k`.
pub fn aero_plan(k: usize, idx: usize, s: &AeroSettings) -> Plan {
    use link::AeroMode;
    let mut p = Plan::new();
    let k = k.max(1);
    let control_on = s.control.enabled && (s.control.regions | s.control.energy) != 0;
    if let Some(m) = s.only {
        let m = match m {
            // The override names "control"; the regions are the settings'.
            AeroMode::Control { .. } => s.control_mode(1, 0),
            m => m,
        };
        let _ = p.push(m.entry(0));
        if matches!(m, AeroMode::Control { .. }) {
            push_all(&mut p, s.control.flags_entry());
        }
        return p;
    }
    let all = [
        (AeroMode::Es1090, AERO_1090_MS),
        (AeroMode::Uat978, AERO_UAT_MS),
        (AeroMode::FlarmHop { band: s.region, ogn: false }, AERO_FLARM_MS),
        (AeroMode::FlarmHop { band: s.region, ogn: true }, AERO_OGN_MS),
    ];
    let modes = if s.es1090_elsewhere { &all[1..] } else { &all[..] };
    // Who does what: with control on and two or more nodes, the last
    // floor(k/2) do control links only.
    let control_nodes = if control_on && k >= 2 { k / 2 } else { 0 };
    let air_nodes = k - control_nodes;
    if idx >= air_nodes {
        let _ = p.push(s.control_mode(control_nodes, idx - air_nodes).entry(0));
        push_all(&mut p, s.control.flags_entry());
        return p;
    }
    let codes: heapless::Vec<u32, 4> = modes.iter().map(|(m, _)| m.code()).collect();
    let mine = part(&codes, air_nodes, idx);
    let share = control_on && k == 1;
    for &c in mine {
        let dwell = if mine.len() == 1 && !share { 0 } else { modes.iter().find(|(m, _)| m.code() == c).map_or(0, |m| m.1) };
        let _ = p.push((c, dwell));
    }
    if share {
        // About half the time: as long as the aircraft turns together.
        let air: u32 = p.iter().map(|e| u32::from(e.1)).sum();
        let _ = p.push(s.control_mode(1, 0).entry(air.min(u32::from(u16::MAX)) as u16));
        push_all(&mut p, s.control.flags_entry());
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
