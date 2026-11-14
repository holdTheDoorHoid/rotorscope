//! Coverage coordinator: decides what every radio node listens to.
//!
//! The hub keeps a table of its nodes (what each can do, what it is doing,
//! when it was last heard). Every couple of seconds it hands that table to
//! [`Coordinator::run`], which returns one [`link::Assignment`] per node for
//! the hub to send. See `README.md` for the rules in plain language.
//!
//! How a role is chosen: coverage slots ([`plans::LADDER`]) are filled in
//! priority order, one node per slot, only with roles in a node's `can`
//! mask. This is solved exactly as an assignment problem (Hungarian method,
//! at most 16 nodes x 26 columns): each slot is worth more than all slots
//! below it together, so a higher slot is never given up for lower ones, and
//! among the best fillings the one that moves the fewest nodes wins. A node
//! whose role changed less than [`ROLE_HOLD_MS`] ago may only take a slot of
//! its current role. Nodes beyond the ladder keep their role (duplicates).
//! Within a role, nodes split the work ([`plans::base_plan`]) and keep the
//! same part where they can.
//!
//! No allocation; deterministic (the same table always gives the same
//! answer, whatever order the nodes are listed in).
#![cfg_attr(not(test), no_std)]

pub mod plans;

use heapless::Vec;
use link::{Assignment, NodeRole};

/// Nodes considered per run. More are ignored (the hub tracks 16 relayed nodes).
pub const MAX_NODES: usize = 16;
/// Ladder entries at most.
pub const MAX_LADDER: usize = 24;
/// A node not heard for longer than this is gone: its slot is given to
/// someone else.
pub const GONE_MS: u64 = 15_000;
/// A node's role changes at most once in this long (a role change is a
/// reboot of a few seconds on the node).
pub const ROLE_HOLD_MS: u64 = 60_000;
/// Remembered nodes (role, part, plan) also cover nodes that are gone, so a
/// node that comes back is put back where it was.
pub const MEMORY: usize = 24;
/// A remembered node silent this long is forgotten.
pub const FORGET_MS: u64 = 600_000;

/// A plan as carried by [`Assignment::plan`].
pub type Plan = Vec<(u32, u16), 24>;
/// A node's identity: its station MAC for ESP-NOW nodes; for a wired node,
/// whatever the hub uses to name the link.
pub type Key = [u8; 6];

const _: () = assert!(plans::LADDER.len() <= MAX_LADDER);

/// One row of the hub's node table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub key: Key,
    /// Roles the node can take, a mask of [`NodeRole::bit`] from its
    /// `Frame::Status`. 0 if it never sent one: then only its current role.
    pub can: u8,
    /// Role it reported last (Hello or Status), if any.
    pub role: Option<NodeRole>,
    /// [`Assignment::id`] it reports having applied (0 = built-in plan).
    pub plan: u16,
    /// Relayed through the ESP-NOW gateway (sends only on 2.4 GHz channel 6).
    pub relayed: bool,
    /// Hub time the node was last heard, ms.
    pub last_heard_ms: u64,
}

/// What one node should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub key: Key,
    pub relayed: bool,
    pub assignment: Assignment,
    /// Role differs from what the node was doing (it will reboot).
    pub role_changed: bool,
    /// Plan or role differs from the last decision for this node.
    pub plan_changed: bool,
    /// The node reports this role and plan id: it has applied it.
    pub applied: bool,
}

pub type Decisions = Vec<Decision, MAX_NODES>;

#[derive(Debug, Clone)]
struct Memory {
    key: Key,
    role: NodeRole,
    /// Part of the role's work (0 = first).
    part: u8,
    plan: Plan,
    id: u16,
    /// When the role was last changed by us.
    changed_ms: Option<u64>,
    last_seen_ms: u64,
}

/// Remembers past decisions so that roles stay put; see the crate docs.
#[derive(Debug, Default)]
pub struct Coordinator {
    memory: Vec<Memory, MEMORY>,
    /// Ladder roles no live node had in the last run, as a mask of
    /// [`NodeRole::bit`] (what nobody is listening to).
    pub missing: u8,
}

const ASSIGNABLE: u8 = NodeRole::Wifi24.bit()
    | NodeRole::Wifi5.bit()
    | NodeRole::Ble.bit()
    | NodeRole::Fpv.bit()
    | NodeRole::DroneId.bit();

/// Slot value: slot `s` of `L` is worth `2^(STAY_BITS + L - 1 - s)`, so it
/// beats every slot below it together; staying in one's role is worth 1, and
/// all stays together (at most 16) are worth less than the smallest slot.
const STAY_BITS: u32 = 5;
const _: () = assert!(MAX_NODES < 1 << STAY_BITS);

impl Coordinator {
    pub const fn new() -> Self {
        Self { memory: Vec::new(), missing: 0 }
    }

    /// Decide every live node's assignment at hub time `now_ms`. Gone nodes
    /// (silent over [`GONE_MS`]), gateways and nodes that can do nothing are
    /// left out. Sorted by key.
    pub fn run(&mut self, nodes: &[NodeInfo], now_ms: u64) -> Decisions {
        // 1. Live, assignable nodes, sorted by key, one entry per key.
        let mut live: Vec<NodeInfo, MAX_NODES> = Vec::new();
        for n in nodes {
            if now_ms.saturating_sub(n.last_heard_ms) > GONE_MS || n.role == Some(NodeRole::Gateway) {
                continue;
            }
            let mut n = n.clone();
            if n.can == 0 {
                n.can = n.role.map_or(0, NodeRole::bit);
            }
            n.can &= ASSIGNABLE;
            if n.can == 0 {
                continue;
            }
            match live.iter().position(|m| m.key == n.key) {
                Some(i) if live[i].last_heard_ms < n.last_heard_ms => live[i] = n,
                Some(_) => {}
                None => {
                    let _ = live.push(n);
                }
            }
        }
        live.sort_unstable_by_key(|n| n.key);

        // 2. Current role and whether it is held.
        let mut current: Vec<Option<NodeRole>, MAX_NODES> = Vec::new();
        let mut held: Vec<bool, MAX_NODES> = Vec::new();
        for n in &live {
            let mem = self.memory.iter().find(|m| m.key == n.key);
            let _ = current.push(mem.map(|m| m.role).or(n.role));
            let _ = held.push(mem.and_then(|m| m.changed_ms).is_some_and(|t| now_ms.saturating_sub(t) < ROLE_HOLD_MS));
        }

        // 3. Fill the ladder.
        let ladder = plans::LADDER;
        let rows = live.len();
        let cols = ladder.len() + rows;
        let mut cost = [[0i64; MAX_LADDER + MAX_NODES]; MAX_NODES];
        for (i, n) in live.iter().enumerate() {
            for (s, &role) in ladder.iter().enumerate() {
                let feasible = n.can & role.bit() != 0 && (!held[i] || current[i] == Some(role));
                cost[i][s] = if feasible {
                    let worth = 1i64 << (STAY_BITS as usize + ladder.len() - 1 - s);
                    -(worth + i64::from(current[i] == Some(role)))
                } else {
                    INFEASIBLE
                };
            }
            // Columns past the ladder: no slot (cost 0).
        }
        let slot_of = hungarian(&cost, rows, cols);

        let mut role: Vec<NodeRole, MAX_NODES> = Vec::new();
        for (i, n) in live.iter().enumerate() {
            let r = match slot_of[i] {
                Some(s) if s < ladder.len() && cost[i][s] != INFEASIBLE => ladder[s],
                // No slot: keep the current role if it can, else its most
                // useful one.
                _ => match current[i] {
                    Some(c) if n.can & c.bit() != 0 => c,
                    _ => *ladder.iter().find(|r| n.can & r.bit() != 0).unwrap_or(&NodeRole::Ble),
                },
            };
            let _ = role.push(r);
        }
        let covered = role.iter().fold(0u8, |m, r| m | r.bit());
        self.missing = ladder.iter().fold(0u8, |m, r| m | r.bit()) & !covered;

        // 4. Parts within each role: keep the old part where possible.
        let mut part: Vec<u8, MAX_NODES> = Vec::new();
        let mut count = [0usize; 8];
        for r in &role {
            count[*r as usize] += 1;
        }
        let _ = part.resize(rows, u8::MAX);
        for (i, n) in live.iter().enumerate() {
            let k = count[role[i] as usize];
            if let Some(m) = self.memory.iter().find(|m| m.key == n.key && m.role == role[i]) {
                let taken = (0..i).any(|j| role[j] == role[i] && part[j] == m.part);
                if usize::from(m.part) < k && !taken {
                    part[i] = m.part;
                }
            }
        }
        for i in 0..rows {
            if part[i] == u8::MAX {
                let free = (0u8..).find(|p| !(0..rows).any(|j| j != i && role[j] == role[i] && part[j] == *p));
                part[i] = free.unwrap_or(0);
            }
        }

        // 5. Plans, ids, memory.
        let mut out = Decisions::new();
        for (i, n) in live.iter().enumerate() {
            let r = role[i];
            let base = plans::base_plan(r, count[r as usize], usize::from(part[i]));
            let plan = if n.relayed && plans::is_wifi(r) { plans::with_espnow_visits(&base) } else { base };
            let role_changed = current[i] != Some(r);
            let mi = self.memory.iter().position(|m| m.key == n.key);
            let old = mi.map(|i| &self.memory[i]);
            let same = old.is_some_and(|m| m.role == r && m.plan == plan);
            let id = match old {
                Some(m) if same => m.id,
                Some(m) => plan_id(r, &plan, m.id),
                None => plan_id(r, &plan, 0),
            };
            let changed_ms = if role_changed { Some(now_ms) } else { old.and_then(|m| m.changed_ms) };
            let mem = Memory { key: n.key, role: r, part: part[i], plan: plan.clone(), id, changed_ms, last_seen_ms: n.last_heard_ms };
            match mi {
                Some(i) => self.memory[i] = mem,
                None => self.remember(mem, now_ms),
            }
            let _ = out.push(Decision {
                key: n.key,
                relayed: n.relayed,
                applied: n.role == Some(r) && n.plan == id,
                assignment: Assignment { role: r, plan, id },
                role_changed,
                plan_changed: !same,
            });
        }
        out
    }

    fn remember(&mut self, mem: Memory, now_ms: u64) {
        self.memory.retain(|m| now_ms.saturating_sub(m.last_seen_ms) <= FORGET_MS);
        if self.memory.is_full() {
            let oldest = (0..self.memory.len()).min_by_key(|&i| (self.memory[i].last_seen_ms, self.memory[i].key));
            if let Some(i) = oldest {
                self.memory.swap_remove(i);
            }
        }
        let _ = self.memory.push(mem);
    }
}

/// A plan id from the plan's content (FNV-1a folded to 16 bits), so the same
/// plan keeps its id even across a hub restart. Never 0 (a node's built-in
/// plan) and never `previous` (so a different plan always gets a new id).
pub fn plan_id(role: NodeRole, plan: &[(u32, u16)], previous: u16) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    let mut eat = |b: u8| h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    eat(role as u8);
    for &(f, ms) in plan {
        f.to_le_bytes().into_iter().for_each(&mut eat);
        ms.to_le_bytes().into_iter().for_each(&mut eat);
    }
    let mut id = ((h >> 16) ^ (h & 0xFFFF)) as u16;
    while id == 0 || id == previous {
        id = id.wrapping_add(1);
    }
    id
}

const INFEASIBLE: i64 = 1 << 40;

/// Minimum-cost assignment of `rows` rows to distinct columns (`rows <=
/// cols`), Hungarian method with potentials (O(rows^2 * cols)). Returns the
/// column of each row.
fn hungarian(cost: &[[i64; MAX_LADDER + MAX_NODES]; MAX_NODES], rows: usize, cols: usize) -> [Option<usize>; MAX_NODES] {
    const M: usize = MAX_LADDER + MAX_NODES + 1;
    const INF: i64 = i64::MAX / 4;
    let mut u = [0i64; MAX_NODES + 1];
    let mut v = [0i64; M];
    let mut p = [0usize; M]; // row (1-based) holding column j
    let mut way = [0usize; M];
    for i in 1..=rows {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = [INF; M];
        let mut used = [false; M];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = INF;
            let mut j1 = 0;
            for j in 1..=cols {
                if !used[j] {
                    let cur = cost[i0 - 1][j - 1] - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=cols {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut out = [None; MAX_NODES];
    for j in 1..=cols {
        if p[j] != 0 {
            out[p[j] - 1] = Some(j - 1);
        }
    }
    out
}

#[cfg(test)]
mod tests;
