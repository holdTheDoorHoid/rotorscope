use super::*;
use link::NodeRole::*;
use plans::*;

/// Every ladder role (aero nodes can do nothing else, and nothing else can).
const ALL: u8 = ASSIGNABLE & !NodeRole::Aero.bit();

fn node(k: u8, can: u8, role: NodeRole) -> NodeInfo {
    NodeInfo { key: [2, 0, 0, 0, 0, k], can, role: Some(role), plan: 0, relayed: false, last_heard_ms: 0 }
}

fn relayed(mut n: NodeInfo) -> NodeInfo {
    n.relayed = true;
    n
}

fn bits(roles: &[NodeRole]) -> u8 {
    roles.iter().fold(0, |m, r| m | r.bit())
}

fn roles(d: &Decisions) -> std::vec::Vec<NodeRole> {
    d.iter().map(|d| d.assignment.role).collect()
}

/// Feed decisions back as if every node applied them at once.
fn apply(nodes: &mut [NodeInfo], d: &Decisions) {
    for n in nodes.iter_mut() {
        if let Some(d) = d.iter().find(|d| d.key == n.key) {
            n.role = Some(d.assignment.role);
            n.plan = d.assignment.id;
        }
    }
}

fn heard(nodes: &mut [NodeInfo], now: u64) {
    for n in nodes {
        n.last_heard_ms = now;
    }
}

fn count(d: &Decisions, r: NodeRole) -> usize {
    d.iter().filter(|d| d.assignment.role == r).count()
}

#[test]
fn ladder_order_with_capable_nodes() {
    // n all-capable nodes, all booted as 2.4 GHz: roles follow the ladder.
    for n in 1..=8u8 {
        let nodes: std::vec::Vec<_> = (0..n).map(|k| node(k, ALL, Wifi24)).collect();
        let d = Coordinator::new().run(&nodes, 0);
        assert_eq!(d.len(), n as usize);
        let mut want: std::vec::Vec<NodeRole> = LADDER[..n as usize].to_vec();
        let mut got = roles(&d);
        want.sort_by_key(|r| *r as u8);
        got.sort_by_key(|r| *r as u8);
        assert_eq!(got, want, "{n} nodes");
    }
}

#[test]
fn eight_nodes_split_the_work() {
    let nodes: std::vec::Vec<_> = (0..8).map(|k| node(k, ALL, Wifi24)).collect();
    let d = Coordinator::new().run(&nodes, 0);
    assert_eq!(count(&d, Wifi24), 2);
    assert_eq!(count(&d, Wifi5), 2);
    assert_eq!(count(&d, DroneId), 2);
    assert_eq!((count(&d, Ble), count(&d, Fpv)), (1, 1));
    let plans_of = |r| -> std::vec::Vec<Plan> {
        d.iter().filter(|d| d.assignment.role == r).map(|d| d.assignment.plan.clone()).collect()
    };
    // 2.4: one parks on 6, one sweeps 1-13 without 6.
    let w24 = plans_of(Wifi24);
    assert!(w24.iter().any(|p| p[..] == WIFI24_PARK[..]));
    assert!(w24.iter().any(|p| p.iter().map(|h| h.0).collect::<std::vec::Vec<_>>() == WIFI24_SWEEP));
    // 5: one parks on 149, the other sweeps 36-48 and 153-165.
    let w5 = plans_of(Wifi5);
    assert!(w5.iter().any(|p| p[..] == WIFI5_PARK[..]));
    assert!(w5.iter().any(|p| p.iter().map(|h| h.0).collect::<std::vec::Vec<_>>() == WIFI5_SWEEP));
    // DroneID: one per band.
    let di = plans_of(DroneId);
    assert!(di.iter().any(|p| p.iter().map(|h| h.0).collect::<std::vec::Vec<_>>() == DRONEID_58));
    assert!(di.iter().any(|p| p.iter().map(|h| h.0).collect::<std::vec::Vec<_>>() == DRONEID_24));
    assert!(di.iter().flatten().all(|h| h.1 == DRONEID_MS));
}

#[test]
fn single_node_plans_match_todays_firmware() {
    let p = base_plan(Wifi24, 1, 0);
    assert_eq!(&p[..], WIFI24_ALONE);
    let p = base_plan(Wifi5, 1, 0);
    assert_eq!(p.len(), 16);
    assert!(p.iter().step_by(2).all(|&h| h == (149, 150)));
    let visits: std::vec::Vec<u32> = p.iter().skip(1).step_by(2).map(|h| h.0).collect();
    assert_eq!(visits, [153, 157, 161, 165, 36, 40, 44, 48]);
    // One DroneID node walks the 5.8 list then the 2.4 list, 700 ms each:
    // node-sdr's DRONEID_PLAN exactly (this used to miss 5816.5).
    let p = base_plan(DroneId, 1, 0);
    let f: std::vec::Vec<u32> = p.iter().map(|h| h.0).collect();
    assert_eq!(
        f,
        [57465, 57565, 57665, 57765, 57865, 57965, 58065, 58165, 24145, 24295, 24445, 24595, 23995]
    );
    const { assert!(DRONEID_MS > 640, "a burst every ~640 ms must land in one dwell") };
    assert!(base_plan(Ble, 1, 0).is_empty());
    assert!(base_plan(Fpv, 1, 0).is_empty(), "one FPV node: built-in sweep");
}

#[test]
fn fpv_channels_are_the_signals_table() {
    let bands: [[u32; 8]; 6] = [
        [5865, 5845, 5825, 5805, 5785, 5765, 5745, 5725],
        [5733, 5752, 5771, 5790, 5809, 5828, 5847, 5866],
        [5705, 5685, 5665, 5645, 5885, 5905, 5925, 5945],
        [5740, 5760, 5780, 5800, 5820, 5840, 5860, 5880],
        [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917],
        [5362, 5399, 5436, 5473, 5510, 5547, 5584, 5621],
    ];
    let mut all: std::vec::Vec<u32> = bands.iter().flatten().map(|m| m * 10).collect();
    all.sort();
    all.dedup();
    assert_eq!(all, FPV_CHANNELS);
}

#[test]
fn fpv_sweep_is_both_bands_within_reach() {
    let mut want: std::vec::Vec<u32> = [
        [2410u32, 2430, 2450, 2470, 2370, 2390, 2490, 2510],
        [2414, 2432, 2450, 2468, 2411, 2433, 2453, 2473],
    ]
    .iter()
    .flatten()
    .map(|m| m * 10)
    .collect();
    want.sort();
    want.dedup();
    assert_eq!(want, FPV_CHANNELS_24);
    let sw = fpv_sweep();
    assert_eq!(sw.len(), 15 + 31, "15 at 2.4 GHz, the 31 reachable at 5.8");
    assert!(sw.windows(2).all(|w| w[0] < w[1]));
    assert!(sw.iter().all(|&f| f < 26_000 || f <= FPV_C5_MAX));
    // Two nodes: each inside the 24-hop plan.
    assert!(base_plan(Fpv, 2, 0).len() <= 23 && base_plan(Fpv, 2, 1).len() <= 23);
}

#[test]
fn splits_cover_everything_once() {
    for k in 2..=6 {
        let mut fpv: std::vec::Vec<u32> = (0..k).flat_map(|i| base_plan(Fpv, k, i)).map(|h| h.0).collect();
        fpv.sort();
        let want = fpv_sweep();
        if k <= want.len() {
            assert_eq!(fpv[..], want[..], "fpv k={k}");
        }
        let mut w24: std::vec::Vec<u32> = (1..k).flat_map(|i| base_plan(Wifi24, k, i)).map(|h| h.0).collect();
        w24.sort();
        w24.dedup();
        assert_eq!(w24, WIFI24_SWEEP, "2.4 k={k}");
        let mut w5: std::vec::Vec<u32> = (1..k).flat_map(|i| base_plan(Wifi5, k, i)).map(|h| h.0).collect();
        w5.sort();
        w5.dedup();
        let mut want = WIFI5_SWEEP.to_vec();
        want.sort();
        assert_eq!(w5, want, "5 k={k}");
        let mut di: std::vec::Vec<u32> = (1..k).flat_map(|i| base_plan(DroneId, k, i)).map(|h| h.0).collect();
        di.sort();
        di.dedup();
        let mut want = DRONEID_24.to_vec();
        want.sort();
        assert_eq!(di, want, "droneid k={k}");
    }
}

#[test]
fn every_plan_fits() {
    for r in [Wifi24, Wifi5, Ble, Fpv, DroneId] {
        for k in 1..=MAX_NODES {
            for i in 0..k {
                let p = base_plan(r, k, i);
                // `push_all` silently stops at 24: make sure nothing was cut.
                assert!(p.len() < 24 || r == Fpv && k == 2, "{r:?} k={k} i={i}: {}", p.len());
                if is_wifi(r) {
                    let e = with_espnow_visits(&p).unwrap();
                    assert!(e.len() < 24, "{r:?} k={k} i={i} espnow: {}", e.len());
                }
            }
        }
    }
    assert_eq!(base_plan(Fpv, 2, 0).len() + base_plan(Fpv, 2, 1).len(), fpv_sweep().len());
}

#[test]
fn only_roles_in_can() {
    let mut x: u32 = 0x9E37_79B9;
    let mut rnd = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    for _ in 0..2000 {
        let n = 1 + (rnd() % 8) as u8;
        let nodes: std::vec::Vec<_> = (0..n)
            .map(|k| {
                let can = (rnd() as u8 | 1 << (rnd() % 5)) & (ALL | Gateway.bit());
                let role = [Wifi24, Wifi5, Ble, Fpv, DroneId][(rnd() % 5) as usize];
                node(k, can, role)
            })
            .collect();
        let d = Coordinator::new().run(&nodes, 0);
        for d in &d {
            let n = nodes.iter().find(|n| n.key == d.key).unwrap();
            assert!(n.can & d.assignment.role.bit() != 0, "{n:?} -> {:?}", d.assignment.role);
            assert_ne!(d.assignment.role, Gateway);
        }
        // Coverage is greedy-optimal: no missing role that an idle-able node could take.
        let have = roles(&d);
        for (s, r) in LADDER.iter().enumerate().take(5) {
            if !have.contains(r) {
                // Every node that could do r is busy in a slot above it.
                for dd in &d {
                    let n = nodes.iter().find(|n| n.key == dd.key).unwrap();
                    if n.can & r.bit() != 0 {
                        let pos = LADDER.iter().position(|x| *x == dd.assignment.role).unwrap();
                        assert!(pos < s, "{r:?} uncovered while {n:?} does {:?}", dd.assignment.role);
                    }
                }
            }
        }
    }
}

#[test]
fn mask_limited_nodes_fill_around_each_other() {
    // A only 2.4; B can 2.4 or BLE, currently 2.4. B must go to BLE.
    let nodes = [node(1, Wifi24.bit(), Wifi24), node(2, bits(&[Wifi24, Ble]), Wifi24)];
    let d = Coordinator::new().run(&nodes, 0);
    assert_eq!(roles(&d), [Wifi24, Ble]);
    assert!(!d[0].role_changed && d[1].role_changed);
}

#[test]
fn gateways_and_unable_nodes_are_left_out() {
    let nodes = [
        node(1, Gateway.bit(), Gateway),
        node(2, ALL, Gateway),
        node(3, 0, Wifi24), // never sent Status: its own role only
        node(4, Gateway.bit(), Wifi24),
    ];
    let d = Coordinator::new().run(&nodes, 0);
    let keys: std::vec::Vec<u8> = d.iter().map(|d| d.key[5]).collect();
    assert_eq!(keys, [3]);
    assert_eq!(d[0].assignment.role, Wifi24);
}

#[test]
fn moves_the_fewest_nodes() {
    // 3 capable nodes on 2.4 / BLE / 5. The 2.4 one leaves: the 5 GHz node
    // (lowest slot) takes 2.4, BLE stays put.
    let mut nodes = std::vec![node(1, ALL, Wifi24), node(2, ALL, Ble), node(3, ALL, Wifi5)];
    let mut c = Coordinator::new();
    let d = c.run(&nodes, 0);
    assert_eq!(roles(&d), [Wifi24, Ble, Wifi5]);
    assert!(d.iter().all(|d| !d.role_changed));
    nodes.remove(0);
    let now = 70_000;
    heard(&mut nodes, now);
    let d = c.run(&nodes, now);
    assert_eq!(roles(&d), [Ble, Wifi24]);
    assert_eq!(d.iter().filter(|d| d.role_changed).count(), 1);
}

#[test]
fn two_same_nodes_only_one_moves() {
    let nodes = [node(1, ALL, Wifi24), node(2, ALL, Wifi24)];
    let d = Coordinator::new().run(&nodes, 0);
    assert_eq!(roles(&d), [Wifi24, Ble]);
    assert_eq!(d.iter().filter(|d| d.role_changed).count(), 1);
}

#[test]
fn gone_after_15_s() {
    let mut nodes = std::vec![node(1, ALL, Wifi24), node(2, ALL, Ble)];
    nodes[0].last_heard_ms = 1_000 + GONE_MS;
    nodes[1].last_heard_ms = 1_000;
    let d = Coordinator::new().run(&nodes, 1_000 + GONE_MS);
    assert_eq!(d.len(), 2);
    let d = Coordinator::new().run(&nodes, 1_001 + GONE_MS);
    assert_eq!(d.len(), 1);
}

#[test]
fn role_held_for_60_s() {
    // B (2.4 or BLE) is moved to BLE at t=0. At t=20 s A (2.4 only) leaves:
    // B may not move back to 2.4 until t=60 s.
    let mut nodes = std::vec![node(1, Wifi24.bit(), Wifi24), node(2, bits(&[Wifi24, Ble]), Wifi24)];
    let mut c = Coordinator::new();
    let d = c.run(&nodes, 0);
    assert_eq!(d[1].assignment.role, Ble);
    apply(&mut nodes, &d);
    let b = std::vec![nodes[1].clone()];
    for t in (2_000..60_000).step_by(2_000) {
        let mut b = b.clone();
        heard(&mut b, t);
        let d = c.run(&b, t);
        assert_eq!(d[0].assignment.role, Ble, "t={t}");
        assert_eq!(c.missing & Wifi24.bit(), Wifi24.bit());
    }
    let mut b = b.clone();
    heard(&mut b, 60_000);
    let d = c.run(&b, 60_000);
    assert_eq!(d[0].assignment.role, Wifi24);
    assert!(d[0].role_changed);
}

#[test]
fn churn_never_flaps() {
    // 6 nodes with mixed abilities come and go at random for 30 minutes;
    // the coordinator runs every 2 s. No node's role may change twice
    // within 60 s, and a stable set settles with no changes at all.
    let masks = [ALL, bits(&[Wifi24, Wifi5]), bits(&[Wifi24, Wifi5, Ble]), bits(&[Fpv, DroneId]), ALL, bits(&[Ble])];
    let mut nodes: std::vec::Vec<NodeInfo> =
        masks.iter().enumerate().map(|(k, &m)| node(k as u8, m, *LADDER.iter().find(|r| m & r.bit() != 0).unwrap())).collect();
    for (i, n) in nodes.iter_mut().enumerate() {
        n.relayed = i % 2 == 1;
    }
    let mut up = [true; 6];
    let mut last_change = [None::<u64>; 6];
    let mut c = Coordinator::new();
    let mut x: u32 = 12345;
    let mut changes = 0;
    for step in 0..900u64 {
        let now = step * 2_000;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        if step < 800 && x.is_multiple_of(7) {
            let i = (x >> 8) as usize % 6;
            up[i] = !up[i];
        }
        if step == 800 {
            up = [true; 6];
        }
        for (i, n) in nodes.iter_mut().enumerate() {
            if up[i] {
                n.last_heard_ms = now;
            }
        }
        let d = c.run(&nodes, now);
        for d in &d {
            let i = d.key[5] as usize;
            if d.role_changed {
                if let Some(t) = last_change[i] {
                    assert!(now - t >= ROLE_HOLD_MS, "node {i} changed at {t} and {now}");
                }
                last_change[i] = Some(now);
                if step > 840 {
                    changes += 1;
                }
            }
            assert!(nodes[i].can & d.assignment.role.bit() != 0);
        }
        apply(&mut nodes, &d);
    }
    assert_eq!(changes, 0, "settled set still changing");
}

#[test]
fn rejoin_keeps_role_and_part() {
    let mut nodes: std::vec::Vec<_> = (0..8).map(|k| node(k, ALL, Wifi24)).collect();
    let mut c = Coordinator::new();
    let d0 = c.run(&nodes, 0);
    apply(&mut nodes, &d0);
    // Node 3 leaves for 30 s, the others are reshuffled, then it returns.
    let away = nodes.remove(3);
    for t in (2_000..=30_000).step_by(2_000) {
        heard(&mut nodes, t);
        let d = c.run(&nodes, t);
        apply(&mut nodes, &d);
    }
    nodes.insert(3, away);
    for t in (32_000..=200_000).step_by(2_000) {
        heard(&mut nodes, t);
        let d = c.run(&nodes, t);
        apply(&mut nodes, &d);
    }
    let d = c.run(&nodes, 200_000);
    let mut got = roles(&d);
    got.sort_by_key(|r| *r as u8);
    let mut want = LADDER[..8].to_vec();
    want.sort_by_key(|r| *r as u8);
    assert_eq!(got, want, "back to full coverage");
    assert!(d.iter().all(|d| d.applied && !d.plan_changed));
}

#[test]
fn plan_ids_change_only_with_content() {
    let mut nodes = std::vec![node(1, Wifi24.bit(), Wifi24)];
    let mut c = Coordinator::new();
    let a = c.run(&nodes, 0)[0].clone();
    assert_ne!(a.assignment.id, 0);
    assert!(a.plan_changed && !a.applied);
    apply(&mut nodes, &std::iter::once(a.clone()).collect());
    let b = c.run(&nodes, 2_000)[0].clone();
    assert_eq!(b.assignment, a.assignment);
    assert!(!b.plan_changed && b.applied);
    // A second 2.4 node joins: node 1 is now one of two, its plan changes.
    nodes.push(node(2, Wifi24.bit(), Wifi24));
    let d = c.run(&nodes, 4_000);
    assert_ne!(d[0].assignment.id, a.assignment.id);
    assert!(d[0].plan_changed && !d[0].role_changed);
    // Same content, fresh coordinator (hub restart): same id.
    let mut fresh = Coordinator::new();
    let e = fresh.run(&nodes, 4_000);
    assert_eq!(e[0].assignment, d[0].assignment);
}

#[test]
fn plan_id_never_zero_or_previous() {
    let p = base_plan(Wifi24, 1, 0);
    let id = plan_id(Wifi24, &p, 0);
    assert_ne!(id, 0);
    assert_ne!(plan_id(Wifi24, &p, id), id);
    assert_eq!(plan_id(Wifi24, &p, id.wrapping_add(5)), id, "previous only matters when equal");
}

#[test]
fn espnow_nodes_visit_channel_6() {
    for r in [Wifi24, Wifi5] {
        for k in 1..=8 {
            for i in 0..k {
                let p = with_espnow_visits(&base_plan(r, k, i)).unwrap();
                let away = longest_away_from_6(&p).unwrap_or_else(|| panic!("{r:?} {k}/{i} never on 6: {p:?}"));
                assert!(away <= ESPNOW_MAX_AWAY_MS, "{r:?} {k}/{i}: {away} ms away: {p:?}");
            }
        }
    }
    // Plans that already visit 6 often enough are unchanged.
    assert_eq!(&with_espnow_visits(&base_plan(Wifi24, 1, 0)).unwrap()[..], WIFI24_ALONE);
    assert_eq!(&with_espnow_visits(&base_plan(Wifi24, 2, 0)).unwrap()[..], WIFI24_PARK);
    // End to end: relayed Wi-Fi nodes get the visits, wired ones do not.
    let nodes: std::vec::Vec<_> = (0..8).map(|k| if k % 2 == 0 { relayed(node(k, ALL, Wifi24)) } else { node(k, ALL, Wifi24) }).collect();
    let d = Coordinator::new().run(&nodes, 0);
    for d in &d {
        let a = &d.assignment;
        if !is_wifi(a.role) {
            continue;
        }
        let away = longest_away_from_6(&a.plan);
        if d.relayed {
            assert!(away.is_some_and(|w| w <= ESPNOW_MAX_AWAY_MS), "{d:?}");
        } else if a.role == Wifi5 {
            assert!(away.is_none(), "wired 5 GHz node sent to 6: {d:?}");
        }
    }
}

#[test]
fn deterministic_whatever_the_order() {
    let nodes: std::vec::Vec<_> = (0..7)
        .map(|k| {
            let mut n = node(k * 37 % 11, if k % 3 == 0 { ALL } else { bits(&[Wifi24, Wifi5, DroneId]) }, Wifi24);
            n.relayed = k % 2 == 0;
            n
        })
        .collect();
    let a = Coordinator::new().run(&nodes, 0);
    let mut rev = nodes.clone();
    rev.reverse();
    let b = Coordinator::new().run(&rev, 0);
    assert_eq!(a, b);
    let mut rot = nodes.clone();
    rot.rotate_left(3);
    assert_eq!(Coordinator::new().run(&rot, 0), a);
    // And over time: two coordinators fed the same history agree.
    let (mut c1, mut c2) = (Coordinator::new(), Coordinator::new());
    for t in (0..100_000).step_by(2_000) {
        let mut n = nodes.clone();
        heard(&mut n, t);
        assert_eq!(c1.run(&n, t), c2.run(&n, t));
    }
}

#[test]
fn more_nodes_than_ladder_duplicate() {
    let nodes: std::vec::Vec<_> = (0..14).map(|k| node(k, ALL, Ble)).collect();
    let d = Coordinator::new().run(&nodes, 0);
    assert_eq!(d.len(), 14);
    // Ladder (10) filled; the 4 extras stay Bluetooth.
    assert_eq!(count(&d, Ble), 2 + 4);
}

#[test]
fn duplicate_keys_use_the_latest() {
    let mut a = node(1, Wifi24.bit(), Wifi24);
    let mut b = node(1, Ble.bit(), Ble);
    a.last_heard_ms = 10;
    b.last_heard_ms = 20;
    let d = Coordinator::new().run(&[a, b], 20);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].assignment.role, Ble);
}

#[test]
fn commands_fit_the_link() {
    // The biggest assignment still encodes into one link frame.
    let mut nodes: std::vec::Vec<_> = (0..8).map(|k| relayed(node(k, ALL, Wifi24))).collect();
    nodes.push(relayed(node(9, Fpv.bit(), Fpv)));
    let d = Coordinator::new().run(&nodes, 0);
    for d in &d {
        let mut buf = [0u8; link::MAX_FRAME];
        let n = link::encode_command(&link::Command::Assign(d.assignment.clone()), &mut buf).unwrap().len();
        assert!(n <= 200, "{n} bytes");
    }
}

/// Every channel of DroneID's 5 MHz raster at 5.8 GHz (5741.5-5816.5, as
/// the Avata 2 recording used) sits whole inside some DroneID dwell: the
/// node tunes the whole MHz below a hop, so a hop covers its own channel
/// (+0.5 MHz) and the one 5 MHz lower (-4.5 MHz). A ~9 MHz burst needs its
/// centre within -5.5..+5.5 MHz of that LO in a 20 MHz capture.
#[test]
fn droneid_58_covers_the_5_mhz_raster() {
    for ch in (57415..=58165).step_by(50) {
        let ok = DRONEID_58.iter().any(|&h| {
            let lo = h - 5; // whole MHz below, 100 kHz units
            let off = ch as i32 - lo as i32;
            (-55..=55).contains(&off)
        });
        assert!(ok, "{ch} not covered");
    }
    // The published O4 schedule's 5.8 GHz channels are hops themselves.
    for c in [57565, 57765, 57965, 58165] {
        assert!(DRONEID_58.contains(&c));
    }
}

#[test]
fn scouting_visits_what_the_plan_skips() {
    let ch = |p: &[(u32, u16)]| p.iter().map(|h| h.0 as u8).collect::<std::vec::Vec<u8>>();
    // 2.4 GHz alone: 6, 1, 11 in the plan; the other eleven, 12-14 included.
    let p = ch(WIFI24_ALONE);
    let got: std::vec::Vec<u8> = (0..11).filter_map(|n| scout_channel(p.iter().copied(), n)).collect();
    assert_eq!(got, [2, 3, 4, 5, 7, 8, 9, 10, 12, 13, 14]);
    assert_eq!(scout_channel(p.iter().copied(), 11), Some(2), "round robin");
    // 5 GHz alone: everything but the plan's own channels, DFS and UNII-4 included.
    let p = ch(&base_plan(Wifi5, 1, 0));
    let got: std::vec::Vec<u8> = (0..SCOUT_5.len()).filter_map(|n| scout_channel(p.iter().copied(), n)).collect();
    for c in [52, 64, 100, 144, 169, 173, 177] {
        assert!(got.contains(&c), "{c}");
    }
    assert!(got.iter().all(|c| !p.contains(c)));
    assert!(got.iter().all(|&c| c > 14), "a 5 GHz plan scouts 5 GHz only");
    // A plan that covers its band has nothing to scout.
    assert_eq!(scout_channel(SCOUT_24.iter().copied(), 0), None);
    assert_eq!(scout_channel(core::iter::empty(), 0), None);
    // Sweeps reach 12-14 and UNII-4.
    assert!(WIFI24_SWEEP.ends_with(&[12, 13, 14]));
    assert!(WIFI5_SWEEP.ends_with(&[169, 173, 177]));
    assert!(SCOUT_MS as u32 * 100 / SCOUT_EVERY_MS <= 4, "scouting costs at most 4% of air time");
}

#[test]
fn fpv_edge_channels_are_off_the_sweep_and_tunable() {
    let sw = fpv_sweep();
    assert!(FPV_EDGE.windows(2).all(|w| w[0] < w[1]));
    for &f in FPV_EDGE {
        assert!(!sw.contains(&f), "{f} is already swept");
        let lo = f / 10 - 1;
        // The C5 PLL takes any whole MHz; the receive path is measured from
        // 5180 MHz up. Above C5_LO_VERIFIED_MAX it is unverified.
        assert!((5180..=5950).contains(&lo), "{f}: LO {lo}");
    }
    // The 5.1 GHz O4 band is covered whole by 20 MHz captures (+-10 MHz
    // around each LO, which sits 1 MHz below the carrier).
    let o4: std::vec::Vec<u32> = FPV_EDGE.iter().map(|&f| f - 10).filter(|&lo| lo < 53_000).collect();
    assert!(o4.first().is_some_and(|&lo| lo - 100 <= 51_700) && o4.last().is_some_and(|&lo| lo + 100 >= 52_500));
    assert!(o4.windows(2).all(|w| w[1] - w[0] <= 200));
    // Every upper 5.8 GHz analog channel the sweep skips is within 10 MHz
    // of an edge carrier.
    for &f in FPV_CHANNELS.iter().filter(|&&f| f > FPV_C5_MAX) {
        assert!(FPV_EDGE.iter().any(|&e| e.abs_diff(f) <= 100), "{f} not covered");
    }
    assert!(FPV_EDGE.iter().any(|&f| f / 10 - 1 > C5_LO_VERIFIED_MAX), "the unverified ones are marked by this constant");
}

/// Protocol 11: an aero node is assigned role Aero with the default plan:
/// UAT and FLARM/OGN (US) time-shared, plus 1090 when no ADSBee hears it;
/// protocol 12: plus a control-link turn as long as the aircraft turns.
#[test]
fn lone_aero_node_default_plan() {
    use link::AeroMode as M;
    let nodes = [node(1, Aero.bit(), Aero), node(2, ALL, Wifi24)];
    let mut c = Coordinator::new();
    let d = c.run(&nodes, 0);
    let a = d.iter().find(|d| d.key == nodes[0].key).unwrap();
    assert_eq!(a.assignment.role, Aero);
    let ctl = M::Control { regions: 0x241, energy: 0x004 };
    let want = [
        M::Es1090.entry(AERO_1090_MS),
        M::Uat978.entry(AERO_UAT_MS),
        M::FlarmHop { band: 2, ogn: false }.entry(AERO_FLARM_MS),
        M::FlarmHop { band: 2, ogn: true }.entry(AERO_OGN_MS),
        ctl.entry(35_000),
    ];
    assert_eq!(&a.assignment.plan[..], &want[..]);
    // The Wi-Fi node is unaffected.
    assert_eq!(d.iter().find(|d| d.key == nodes[1].key).unwrap().assignment.role, Wifi24);

    // With an ADSBee on the hub, 1090 is left to it; control still half.
    c.aero.es1090_elsewhere = true;
    let d2 = c.run(&nodes, 1000);
    let a2 = d2.iter().find(|d| d.key == nodes[0].key).unwrap();
    assert_eq!(&a2.assignment.plan[..3], &want[1..4]);
    assert_eq!(a2.assignment.plan[3], ctl.entry(25_000));
    assert!(a2.plan_changed);
    assert_ne!(a2.assignment.id, a.assignment.id);

    // Operator override: one mode, stay.
    c.aero.only = Some(M::Uat978);
    let d3 = c.run(&nodes, 2000);
    let a3 = d3.iter().find(|d| d.key == nodes[0].key).unwrap();
    assert_eq!(&a3.assignment.plan[..], &[M::Uat978.entry(0)][..]);

    // Control off: the aircraft plan alone, exactly as protocol 11.
    c.aero.only = None;
    c.aero.control.enabled = false;
    let d4 = c.run(&nodes, 3000);
    let a4 = d4.iter().find(|d| d.key == nodes[0].key).unwrap();
    assert_eq!(&a4.assignment.plan[..], &want[1..4]);
}

/// The two opt-ins ride as one flags entry, only when one is on, and every
/// plan with a control entry carries it.
#[test]
fn control_flags_entry() {
    use link::AeroMode as M;
    let mut s = AeroSettings { es1090_elsewhere: true, ..AeroSettings::new() };
    assert!(!s.control.read_ids && !s.control.decode_content, "off by default");
    let has_flags = |p: &[(u32, u16)]| p.iter().filter_map(|e| M::from_code(e.0)).find(|m| m.is_flags());
    assert_eq!(has_flags(&aero_plan(1, 0, &s)), None);
    s.control.decode_content = true;
    assert_eq!(has_flags(&aero_plan(1, 0, &s)), Some(M::ControlFlags { read_ids: false, decode_content: true }));
    s.control.read_ids = true;
    // Two nodes: aircraft on one (no flags), control on the other (flags).
    assert_eq!(has_flags(&aero_plan(2, 0, &s)), None);
    assert_eq!(has_flags(&aero_plan(2, 1, &s)), Some(M::ControlFlags { read_ids: true, decode_content: true }));
    // The override "control" takes the settings' regions and flags.
    s.only = Some(M::Control { regions: 0, energy: 0 });
    let p = aero_plan(1, 0, &s);
    assert_eq!(p[0], M::Control { regions: 0x241, energy: 0x004 }.entry(0));
    assert!(has_flags(&p).is_some());
}

/// Two aero nodes: one aircraft, one control links. Four: two each, the
/// control pair splitting sub-GHz and 2.4 GHz.
#[test]
fn aero_nodes_split_aircraft_and_control() {
    use link::AeroMode as M;
    let s = AeroSettings { es1090_elsewhere: true, ..AeroSettings::new() };
    let air = [M::Uat978.entry(AERO_UAT_MS), M::FlarmHop { band: 2, ogn: false }.entry(AERO_FLARM_MS), M::FlarmHop { band: 2, ogn: true }.entry(AERO_OGN_MS)];
    assert_eq!(&aero_plan(2, 0, &s)[..], &air[..]);
    assert_eq!(&aero_plan(2, 1, &s)[..], &[M::Control { regions: 0x241, energy: 0x004 }.entry(0)][..]);
    // Three: two aircraft, one control.
    assert!(aero_plan(3, 2, &s).iter().all(|e| matches!(M::from_code(e.0), Some(M::Control { .. }))));
    // Four: the control pair splits the bands, nothing lost.
    let c2 = aero_plan(4, 2, &s);
    let c3 = aero_plan(4, 3, &s);
    let Some(M::Control { regions: r2, energy: e2 }) = M::from_code(c2[0].0) else { panic!() };
    let Some(M::Control { regions: r3, energy: e3 }) = M::from_code(c3[0].0) else { panic!() };
    assert_eq!((r2, e2), (0x041, 0x004), "915 + 433, and 868 for energy");
    assert_eq!((r3, e3), (0x200, 0), "2.4 GHz");
    assert_eq!(r2 | r3, 0x241);
}

/// An aero node that never said its role (Status with `can` only) still
/// becomes Aero, not the Bluetooth fallback.
#[test]
fn aero_node_without_role_is_aero() {
    let mut n = node(1, Aero.bit(), Aero);
    n.role = None;
    let d = Coordinator::new().run(&[n], 0);
    assert_eq!(roles(&d), [Aero]);
}

/// Several aero nodes split the modes; a single mode per node stays (dwell 0).
/// (Control links off here: with them on, half the nodes do control.)
#[test]
fn aero_nodes_split_the_modes() {
    use link::AeroMode as M;
    let mut s = AeroSettings { es1090_elsewhere: true, ..AeroSettings::new() };
    s.control.enabled = false;
    let one = |k, i| aero_plan(k, i, &s);
    assert_eq!(&one(3, 0)[..], &[M::Uat978.entry(0)][..]);
    assert_eq!(&one(3, 1)[..], &[M::FlarmHop { band: 2, ogn: false }.entry(0)][..]);
    assert_eq!(&one(3, 2)[..], &[M::FlarmHop { band: 2, ogn: true }.entry(0)][..]);
    assert_eq!(
        &one(2, 1)[..],
        &[M::FlarmHop { band: 2, ogn: false }.entry(AERO_FLARM_MS), M::FlarmHop { band: 2, ogn: true }.entry(AERO_OGN_MS)][..]
    );
    // More nodes than modes: duplicates, never an empty plan.
    for i in 0..5 {
        assert_eq!(one(5, i).len(), 1);
    }
    // Every code is one the node understands, control on or off.
    for on in [false, true] {
        let mut s = AeroSettings::new();
        s.control.enabled = on;
        s.control.read_ids = on;
        for k in 1..=4 {
            for i in 0..k {
                let p = aero_plan(k, i, &s);
                assert!(!p.is_empty());
                for &(c, _) in &p {
                    assert!(M::from_code(c).is_some());
                }
            }
        }
    }
    let nodes = [node(1, Aero.bit(), Aero), node(2, Aero.bit(), Aero)];
    let d = Coordinator::new().run(&nodes, 0);
    assert_eq!(count(&d, Aero), 2);
    assert_ne!(d[0].assignment.plan, d[1].assignment.plan);
}

/// Protocol 13: `control content` reaches Wi-Fi nodes as the same flags
/// entry, last in their plans. Off by default (plans as before); a change
/// is a new plan with a new id (sent at once, `plan_changed`), and the
/// unchanged plan keeps its id across runs and hub restarts, so the 5 s
/// resend brings a restarted node the setting again.
#[test]
fn wifi_plans_carry_control_flags() {
    use link::AeroMode as M;
    let mut nodes = [relayed(node(1, Wifi24.bit(), Wifi24)), node(2, Wifi5.bit(), Wifi5), node(3, Ble.bit(), Ble)];
    let mut c = Coordinator::new();
    let d = c.run(&nodes, 0);
    for d in &d {
        assert_eq!(link::plan_control_flags(&d.assignment.plan), None, "off by default: {:?}", d.assignment.role);
    }
    let before: std::vec::Vec<_> = d.iter().map(|d| d.assignment.clone()).collect();
    apply(&mut nodes, &d);
    heard(&mut nodes, 2_000);
    // Content on: Wi-Fi plans change (new id, sent now); Bluetooth does not.
    c.aero.control.decode_content = true;
    let d = c.run(&nodes, 2_000);
    for (d, old) in d.iter().zip(&before) {
        if plans::is_wifi(d.assignment.role) {
            assert!(d.plan_changed && d.assignment.id != old.id, "{:?}", d.assignment.role);
            assert!(!d.applied, "the node has not got it yet");
            assert_eq!(*d.assignment.plan.last().unwrap(), M::ControlFlags { read_ids: false, decode_content: true }.entry(0));
            assert_eq!(&d.assignment.plan[..d.assignment.plan.len() - 1], &old.plan[..], "channels unchanged");
            assert_eq!(link::plan_control_flags(&d.assignment.plan), Some((false, true)));
        } else {
            assert!(!d.plan_changed);
            assert_eq!(link::plan_control_flags(&d.assignment.plan), None);
        }
    }
    let on: std::vec::Vec<_> = d.iter().map(|d| d.assignment.clone()).collect();
    apply(&mut nodes, &d);
    heard(&mut nodes, 4_000);
    // Steady: same plans, same ids, applied.
    let d = c.run(&nodes, 4_000);
    assert!(d.iter().all(|d| !d.plan_changed && d.applied));
    assert_eq!(d.iter().map(|d| d.assignment.clone()).collect::<std::vec::Vec<_>>(), on);
    // A hub that restarts with the same setting sends the same plans (ids
    // come from the content).
    let mut fresh = Coordinator::new();
    fresh.aero.control.decode_content = true;
    let d2 = fresh.run(&nodes, 4_000);
    assert_eq!(d2.iter().map(|d| d.assignment.clone()).collect::<std::vec::Vec<_>>(), on);
    // Off again: the entry goes, new ids.
    c.aero.control.decode_content = false;
    let d = c.run(&nodes, 6_000);
    for d in d.iter().filter(|d| plans::is_wifi(d.assignment.role)) {
        assert!(d.plan_changed);
        assert_eq!(link::plan_control_flags(&d.assignment.plan), None);
    }
}

/// Every Wi-Fi plan, ESP-NOW visits included, leaves room for the entry.
#[test]
fn wifi_plans_leave_room_for_flags() {
    for role in [Wifi24, Wifi5] {
        for k in 1..=8 {
            for idx in 0..k {
                let p = plans::with_espnow_visits(&base_plan(role, k, idx)).unwrap();
                assert!(p.len() < 24, "{role:?} {k}/{idx}: {} hops", p.len());
            }
        }
    }
}

/// A full 24-hop 5 GHz plan: no room left for a channel-6 visit.
fn full_5ghz_plan() -> Plan {
    let mut p = Plan::new();
    for i in 0..24 {
        let _ = p.push((WIFI5_SWEEP[i % WIFI5_SWEEP.len()], 100 + i as u16));
    }
    p
}

#[test]
fn full_24_hop_5ghz_plan_keeps_channel_6() {
    let full = full_5ghz_plan();
    assert!(full.is_full());
    // The visits do not fit: an error, never a silently shorter list.
    assert_eq!(with_espnow_visits(&full), Err(PlanFull));
    for room in [24, 23] {
        let p = espnow_plan(&full, room);
        assert!(p.len() <= room, "{room}: {} hops", p.len());
        let away = longest_away_from_6(&p);
        assert!(away.is_some_and(|w| w <= ESPNOW_MAX_AWAY_MS), "visits lost: {away:?} {p:?}");
        // Only the lowest-weight hops went: every hop kept dwells at least
        // as long as every hop dropped.
        let kept: std::vec::Vec<_> = p.iter().filter(|h| h.0 != ESPNOW_CHANNEL).copied().collect();
        let shortest_kept = kept.iter().map(|h| h.1).min().unwrap();
        for h in full.iter().filter(|h| !kept.contains(h)) {
            assert!(h.1 <= shortest_kept, "{h:?} dropped before a shorter hop: {p:?}");
        }
        // In the original order.
        let order: std::vec::Vec<_> = full.iter().filter(|h| kept.contains(h)).copied().collect();
        assert_eq!(kept, order);
    }
    // A plan that fits is not touched.
    let base = base_plan(Wifi5, 1, 0);
    assert_eq!(espnow_plan(&base, 23), with_espnow_visits(&base).unwrap());
}

/// End to end: a relayed 5 GHz node whose plan is full still gets its
/// visits, and the flags entry still fits after them.
#[test]
fn coordinator_full_plan_relayed_keeps_channel_6_and_flags() {
    for flags_on in [false, true] {
        for p in [full_5ghz_plan(), {
            let mut p = Plan::new();
            for _ in 0..24 {
                let _ = p.push((149, 1000));
            }
            p
        }] {
            for room in [24, 23] {
                let e = espnow_plan(&p, room);
                assert!(e.len() <= room);
                assert!(longest_away_from_6(&e).is_some_and(|w| w <= ESPNOW_MAX_AWAY_MS), "{e:?}");
            }
        }
        let mut c = Coordinator::new();
        c.aero.control.decode_content = flags_on;
        let d = c.run(&[relayed(node(1, ALL, Wifi5))], 0);
        let a = &d[0].assignment;
        assert_eq!(link::plan_control_flags(&a.plan).is_some(), flags_on);
        let hops: std::vec::Vec<_> = a.plan.iter().filter(|e| !link::is_control_flags_code(e.0)).copied().collect();
        assert!(longest_away_from_6(&hops).is_some_and(|w| w <= ESPNOW_MAX_AWAY_MS));
    }
}

/// The node's own check (role.rs): its working plan holds a full wire plan
/// with every visit; a plan the visits cannot fix is refused.
#[test]
fn node_plan_rule() {
    let full = full_5ghz_plan();
    let p = node_espnow_plan(&full).unwrap();
    assert!(p.len() > 24 && p.len() <= NODE_MAX_HOPS);
    assert!(longest_away_from_6(&p).is_some_and(|w| w <= ESPNOW_MAX_AWAY_MS));
    // Every hop kept, in order.
    let hops: std::vec::Vec<_> = p.iter().filter(|h| h.0 != ESPNOW_CHANNEL).copied().collect();
    assert_eq!(&hops[..], &full[..]);
    // Worst case for the insertion: every hop needs a visit before it.
    let worst: std::vec::Vec<_> = (0..24).map(|i| (if i % 2 == 0 { 36 } else { 40 }, 1000u16)).collect();
    let p = node_espnow_plan(&worst).unwrap();
    assert_eq!(p.len(), 48);
    assert!(p.len() <= NODE_MAX_HOPS);
    // One hop longer than the limit: no visit can fix it.
    assert_eq!(node_espnow_plan(&[(149, 1500)]), Err(NodePlanError::Deaf));
    assert_eq!(node_espnow_plan(&[(6, 150), (36, 1001)]), Err(NodePlanError::Deaf));
    let long: std::vec::Vec<_> = (0..25).map(|_| (36u32, 100u16)).collect();
    assert_eq!(node_espnow_plan(&long), Err(NodePlanError::TooLong));
    // The built-in plans pass.
    for r in [Wifi24, Wifi5] {
        for k in 1..=8 {
            for i in 0..k {
                assert!(node_espnow_plan(&base_plan(r, k, i)).is_ok(), "{r:?} {k}/{i}");
            }
        }
    }
}

/// The coordinator's ESP-NOW channel is the gateway's (one constant,
/// `airlink::GATEWAY_CHANNEL`; see `plans::ESPNOW_CHANNEL` for why it is
/// checked here rather than imported).
#[test]
fn espnow_channel_is_the_gateway_channel() {
    assert_eq!(ESPNOW_CHANNEL, u32::from(airlink::GATEWAY_CHANNEL));
}

/// What a relayed node does with a job read back from `rsjob` (role.rs
/// `init`): a channel-6-free stored plan gets its visits if they can keep
/// the node in reach of the gateway, and is refused (built-in plan) if no
/// visit can.
#[test]
fn stored_plan_without_6_refused() {
    // Long hops on 5 GHz (the dwell a node clamps to, 10 s): deaf whatever
    // is inserted between them.
    let deaf = [(149u32, 10_000u16), (36, 10_000)];
    assert_eq!(node_espnow_plan(&deaf), Err(NodePlanError::Deaf));
    // Ordinary hops without 6: the node inserts the visits itself.
    let fixable = [(149u32, 150u16), (153, 50), (157, 50)];
    let p = node_espnow_plan(&fixable).unwrap();
    assert!(p.iter().any(|h| h.0 == ESPNOW_CHANNEL));
    assert!(longest_away_from_6(&p).is_some_and(|w| w <= ESPNOW_MAX_AWAY_MS));
}
