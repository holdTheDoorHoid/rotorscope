//! A simulated bus for the tests: one hub, N nodes, the hub's TX wire to
//! every node, and the shared open-drain return wire, at byte resolution.
//!
//! Modelled:
//! - Bytes take their real wire time (10 bits at the baud rate).
//! - The hub's UART interrupt hands bytes over when 64 have arrived or the
//!   line has been idle 10 bit-times... [`HUB_RX_IDLE_NS`]; it runs
//!   `on_rx` then `tick` at once (the fast path). The main loop also ticks
//!   every [`Sim::main_period_ns`], with a long stall now and then (the P4
//!   draws its screen in bands of ~7 ms).
//! - A node reads its UART after an idle gap plus a random reaction delay
//!   (an interrupt-executor task), now and then a long spike; it answers
//!   with everything that has arrived by then, so a poll overtaken by a newer
//!   command is not answered.
//! - Two nodes driving the return wire in the same byte time collide: the
//!   open-drain line ANDs them, and misaligned bytes come out garbled.
//! - Noise: any byte on either wire can be replaced at random.
//! - Nodes can be unplugged, rebooted (same MAC, empty queue) and the hub
//!   rebooted (new epoch).

use std::collections::VecDeque;
use std::vec::Vec;

use link::{Detection, DroneIdRecord, DroneIdSighting, Frame, ThreatKind, ThreatSighting};

use crate::master::{Event, Master, MasterConfig};
use crate::node::{Node, NodeConfig, Outbox};

pub const NS: u64 = 1;
pub const US: u64 = 1_000 * NS;
pub const MS: u64 = 1_000 * US;
pub const SEC: u64 = 1_000 * MS;
/// UART RX timeout (10 symbols at 921600 = 108 us).
pub const HUB_RX_IDLE_NS: u64 = 108 * US;
pub const NODE_RX_IDLE_NS: u64 = 108 * US;

/// A simple deterministic random source.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    pub fn chance(&mut self, p: f64) -> bool {
        (self.next() % 1_000_000) as f64 / 1_000_000.0 < p
    }
}

/// Frame kinds the traffic generator makes, with the id in `uptime_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Wi-Fi Remote ID beacon with a 5-message pack (~150 bytes encoded).
    RidPack,
    /// Bluetooth legacy advert, one message (~50 bytes).
    RidSingle,
    /// DJI DroneID flight record (~75 bytes).
    DroneId,
    /// A Wi-Fi attack / jamming report (~55 bytes).
    Threat,
    /// Once-a-second statistics (~25 bytes).
    Stats,
    /// A full 9-message pack at its widest (~260 bytes).
    RidFull,
}

pub fn make_frame(kind: Kind, id: u64, rng: &mut Rng) -> Frame {
    let mut msg = || {
        let mut m = [0u8; rid::MESSAGE_SIZE];
        for b in m.iter_mut() {
            *b = rng.next() as u8;
        }
        m
    };
    match kind {
        Kind::RidPack | Kind::RidSingle | Kind::RidFull => {
            let n = match kind {
                Kind::RidPack => 5,
                Kind::RidSingle => 1,
                _ => rid::MAX_PACK,
            };
            let mut messages = heapless::Vec::new();
            for _ in 0..n {
                messages.push(msg()).unwrap();
            }
            Frame::Detection(Detection {
                transport: if n == 1 { rid::Transport::BleLegacy } else { rid::Transport::WifiBeacon },
                channel: 6,
                rssi: -60,
                mac: [0x60, 0x60, 0x1F, 1, 2, 3],
                counter: id as u8,
                uptime_us: id,
                messages,
            })
        }
        Kind::DroneId => Frame::DroneId(DroneIdSighting {
            freq: 57_565,
            rssi: -70,
            snr_db: 12,
            uptime_us: id,
            record: DroneIdRecord::Flight {
                serial: *b"1581F5FKD229R00A",
                lat_raw: 399_500_000,
                lon_raw: -751_600_000,
                altitude_raw: 1200,
                height_raw: 800,
                v_north: 120,
                v_east: -40,
                v_up: 3,
                yaw_raw: 900,
                pilot_lat_raw: 399_499_000,
                pilot_lon_raw: -751_601_000,
                home_lat_raw: 399_499_000,
                home_lon_raw: -751_601_000,
                product_type: 68,
            },
        }),
        Kind::Threat => Frame::Threat(ThreatSighting {
            kind: ThreatKind::Deauth,
            channel: 6,
            freq: 24_370,
            width_khz: 20_000,
            level: -40,
            count: 250,
            window_ms: 1000,
            source: [0xDE, 0xAD, 0xBE, 0xEF, 0, 1],
            target: [0xFF; 6],
            detail: 7,
            ssid: heapless::String::new(),
            uptime_us: id,
        }),
        Kind::Stats => Frame::Stats { uptime_us: id, frames_seen: 1000, rid_found: 10, dropped: 0, by_transport: heapless::Vec::new() },
    }
}

/// The id a generated frame carries.
pub fn frame_id(f: &Frame) -> Option<u64> {
    match f {
        Frame::Detection(d) => Some(d.uptime_us),
        Frame::DroneId(d) => Some(d.uptime_us),
        Frame::Threat(t) => Some(t.uptime_us),
        Frame::Stats { uptime_us, .. } => Some(*uptime_us),
        _ => None,
    }
}

/// Frames per second of each kind.
#[derive(Debug, Clone, Default)]
pub struct Traffic(pub Vec<(Kind, f64)>);

impl Traffic {
    /// A node watching one drone: what the bench sees with one Remote ID
    /// test transmitter, plus housekeeping.
    pub fn quiet() -> Self {
        Traffic(vec![(Kind::Stats, 1.0), (Kind::RidPack, 3.0), (Kind::Threat, 0.2)])
    }
    /// A busy field day for one node: ~5 drones broadcasting Remote ID at
    /// ~4 Hz on the node's transport, DroneID bursts, an attack going on.
    pub fn busy() -> Self {
        Traffic(vec![
            (Kind::Stats, 1.0),
            (Kind::RidPack, 10.0),
            (Kind::RidSingle, 10.0),
            (Kind::DroneId, 5.0),
            (Kind::Threat, 2.0),
        ])
    }
    /// Far beyond anything real: a node spamming full packs.
    pub fn flood(per_s: f64) -> Self {
        Traffic(vec![(Kind::RidFull, per_s)])
    }
}

/// The node's queue in the simulation: whole encoded frames, at most
/// `cap`, the oldest dropped when full (as node-sdr does).
pub struct SimOutbox {
    pub q: VecDeque<(u64, u64, Vec<u8>)>, // (id, enqueue ns, bytes)
    pub cap: usize,
    pub dropped: u64,
    /// Ids taken into an answer, with their enqueue time.
    pub taken: VecDeque<(u64, u64)>,
}

impl Outbox for SimOutbox {
    fn peek(&mut self) -> Option<&[u8]> {
        self.q.front().map(|f| &f.2[..])
    }
    fn pop(&mut self) {
        if let Some((id, t, _)) = self.q.pop_front() {
            self.taken.push_back((id, t));
        }
    }
}

pub struct SimNode {
    pub node: Node,
    pub mac: [u8; 6],
    pub out: SimOutbox,
    pub plugged: bool,
    pub traffic: Traffic,
    next_gen: Vec<u64>,
    fifo: Vec<u8>,
    last_rx_ns: u64,
    wake_ns: Option<u64>,
    busy_until: u64,
    /// Random reaction delay range and the chance of a long spike.
    pub react_ns: (u64, u64),
    pub spike: (f64, u64),
    pub next_id: u64,
    pub sent_ids: Vec<u64>,
    pub assigns: u32,
    pub reboots: u32,
    /// Longest answer this node put on the wire, bytes.
    pub max_answer: usize,
}

pub struct Delivered {
    pub node: usize,
    #[allow(dead_code, reason = "kept for debugging a failing run")]
    pub id: u64,
    pub latency_ns: u64,
}

pub struct Sim {
    pub now: u64,
    pub byte_ns: u64,
    pub master: Master<16>,
    pub nodes: Vec<SimNode>,
    pub rng: Rng,
    /// Bytes in flight to the nodes: (arrival ns, byte).
    hub_tx: VecDeque<(u64, u8)>,
    hub_tx_free: u64,
    /// Bytes in flight on the return wire: (end ns, byte, node).
    ret: Vec<(u64, u8, usize)>,
    hub_fifo: Vec<u8>,
    hub_last_rx: u64,
    pub main_period_ns: u64,
    /// Every `.0` ns the main loop stalls for `.1` ns.
    pub stall: (u64, u64),
    next_main: u64,
    pub noise_hub_tx: f64,
    pub noise_ret: f64,
    pub delivered: Vec<Delivered>,
    pub collisions: u64,
    /// Frames decoded from master events that did not decode (must stay 0).
    pub undecodable: u64,
    /// Events seen.
    pub joined: Vec<([u8; 6], u8, bool)>,
    pub absent: Vec<[u8; 6]>,
    pub back: Vec<[u8; 6]>,
    /// Ids still owed per node across a reboot (lost legitimately).
    pub lost_at_reboot: u64,
    pub hub_reboots: u32,
    /// Bytes put on the return wire by nodes.
    pub ret_bytes: u64,
}

pub fn mac_of(i: usize) -> [u8; 6] {
    [0x3C, 0xDC, 0x75, 0x0C, 0x50, i as u8 + 1]
}

impl Sim {
    pub fn new(n: usize, traffic: Traffic, seed: u64) -> Self {
        let cfg = MasterConfig::new();
        let mut rng = Rng::new(seed);
        let epoch = rng.next() as u16;
        let byte_ns = 10 * 1_000_000_000 / u64::from(cfg.baud);
        let mut s = Sim {
            now: 0,
            byte_ns,
            master: Master::new(cfg, epoch),
            nodes: Vec::new(),
            rng,
            hub_tx: VecDeque::new(),
            hub_tx_free: 0,
            ret: Vec::new(),
            hub_fifo: Vec::new(),
            hub_last_rx: 0,
            main_period_ns: 500 * US,
            stall: (250 * MS, 7 * MS),
            next_main: 0,
            noise_hub_tx: 0.0,
            noise_ret: 0.0,
            delivered: Vec::new(),
            collisions: 0,
            undecodable: 0,
            joined: Vec::new(),
            absent: Vec::new(),
            back: Vec::new(),
            lost_at_reboot: 0,
            hub_reboots: 0,
            ret_bytes: 0,
        };
        for i in 0..n {
            s.add_node(traffic.clone(), i);
        }
        s
    }

    pub fn add_node(&mut self, traffic: Traffic, i: usize) {
        let mac = mac_of(i);
        let seed = self.rng.next() as u32;
        let next_gen = traffic.0.iter().map(|_| self.rng.below(SEC)).collect();
        self.nodes.push(SimNode {
            node: Node::new(NodeConfig::new(), mac, seed, 0),
            mac,
            out: SimOutbox { q: VecDeque::new(), cap: 16, dropped: 0, taken: VecDeque::new() },
            plugged: true,
            traffic,
            next_gen,
            fifo: Vec::new(),
            last_rx_ns: 0,
            wake_ns: None,
            busy_until: 0,
            react_ns: (30 * US, 400 * US),
            spike: (0.0, 0),
            next_id: 1,
            sent_ids: Vec::new(),
            assigns: 0,
            reboots: 0,
            max_answer: 0,
        });
    }

    fn us(&self) -> u64 {
        self.now / US
    }

    /// Reboot node `i` (an image switch): same MAC, empty queue, no address.
    pub fn reboot_node(&mut self, i: usize) {
        let seed = self.rng.next() as u32;
        let n = &mut self.nodes[i];
        self.lost_at_reboot += (n.out.q.len() + n.out.taken.len()) as u64;
        n.out.q.clear();
        n.out.taken.clear();
        n.node = Node::new(NodeConfig::new(), n.mac, seed, 0);
        n.fifo.clear();
        n.wake_ns = None;
        n.reboots += 1;
    }

    pub fn reboot_hub(&mut self) {
        let epoch = self.rng.next() as u16;
        self.master = Master::new(MasterConfig::new(), epoch);
        self.hub_fifo.clear();
        self.hub_reboots += 1;
    }

    /// Send hub bytes (commands) to every node.
    pub fn hub_send(&mut self, bytes: &[u8]) {
        let start = self.hub_tx_free.max(self.now);
        for (k, &b) in bytes.iter().enumerate() {
            let b = if self.rng.chance(self.noise_hub_tx) { self.rng.next() as u8 } else { b };
            self.hub_tx.push_back((start + (k as u64 + 1) * self.byte_ns, b));
        }
        self.hub_tx_free = start + bytes.len() as u64 * self.byte_ns;
    }

    fn node_send(&mut self, i: usize, bytes: &[u8]) {
        let start = self.now + 5 * US;
        let n = bytes.len() as u64;
        if self.nodes[i].plugged {
            for (k, &b) in bytes.iter().enumerate() {
                self.ret.push((start + (k as u64 + 1) * self.byte_ns, b, i));
            }
            self.ret_bytes += n;
        }
        self.nodes[i].busy_until = start + n * self.byte_ns;
    }

    fn master_events(&mut self, ev: &mut Vec<OwnedEvent>) {
        for e in ev.drain(..) {
            match e {
                OwnedEvent::Frame(mac, bytes) => {
                    let Some(i) = self.nodes.iter().position(|n| n.mac == mac) else { continue };
                    let mut dec = link::Decoder::new();
                    let mut got = None;
                    dec.feed(&bytes, |r| got = Some(r));
                    match got {
                        Some(Ok(f)) => {
                            let Some(id) = frame_id(&f) else { continue };
                            let n = &mut self.nodes[i];
                            // When was it queued? Look it up among the taken.
                            let t = n.out.taken.iter().find(|x| x.0 == id).map(|x| x.1);
                            while n.out.taken.front().is_some_and(|x| x.0 <= id) {
                                n.out.taken.pop_front();
                            }
                            n.sent_ids.push(id);
                            self.delivered.push(Delivered { node: i, id, latency_ns: t.map_or(0, |t| self.now.saturating_sub(t)) });
                        }
                        _ => self.undecodable += 1,
                    }
                }
                OwnedEvent::Joined(mac, addr, again) => self.joined.push((mac, addr, again)),
                OwnedEvent::Absent(mac) => self.absent.push(mac),
                OwnedEvent::Back(mac) => self.back.push(mac),
                OwnedEvent::AnswerEnd => {}
            }
        }
    }

    fn hub_service(&mut self, rx: bool) {
        let now_us = self.us();
        let mut ev = Vec::new();
        let mut collect = |e: Event<'_>| ev.push(OwnedEvent::from(e));
        if rx && !self.hub_fifo.is_empty() {
            let bytes = core::mem::take(&mut self.hub_fifo);
            self.master.on_rx(now_us, &bytes, &mut collect);
        }
        let busy = self.hub_tx_free.saturating_sub(self.now) / US;
        let out = self.master.tick(now_us, busy, &mut collect).map(|b| b.to_vec());
        self.master_events(&mut ev);
        if let Some(b) = out {
            self.hub_send(&b);
        }
    }

    /// Advance by `dt` ns in steps of a quarter byte.
    pub fn run(&mut self, dt: u64) {
        let end = self.now + dt;
        let step = self.byte_ns / 4;
        while self.now < end {
            self.now += step;
            self.step();
        }
    }

    fn step(&mut self) {
        let now = self.now;
        // Traffic.
        for i in 0..self.nodes.len() {
            for k in 0..self.nodes[i].traffic.0.len() {
                let (kind, rate) = self.nodes[i].traffic.0[k];
                if rate <= 0.0 || self.nodes[i].next_gen[k] > now {
                    continue;
                }
                let gap = (SEC as f64 / rate) as u64;
                let jitter = self.rng.below(gap / 2 + 1);
                self.nodes[i].next_gen[k] = now + gap / 2 + jitter + gap / 4;
                let id = self.nodes[i].next_id;
                self.nodes[i].next_id += 1;
                let f = make_frame(kind, id, &mut self.rng);
                let mut buf = [0u8; link::MAX_FRAME];
                let bytes = link::encode(&f, &mut buf).unwrap().to_vec();
                let o = &mut self.nodes[i].out;
                if o.q.len() >= o.cap {
                    o.q.pop_front();
                    o.dropped += 1;
                }
                o.q.push_back((id, now, bytes));
            }
        }
        // Hub TX to the nodes.
        while self.hub_tx.front().is_some_and(|x| x.0 <= now) {
            let (_, b) = self.hub_tx.pop_front().unwrap();
            for n in self.nodes.iter_mut().filter(|n| n.plugged) {
                n.fifo.push(b);
                n.last_rx_ns = now;
                n.wake_ns = None;
            }
        }
        // Nodes: read after idle + reaction delay; answer; ticks.
        for i in 0..self.nodes.len() {
            let n = &mut self.nodes[i];
            if !n.fifo.is_empty() && n.wake_ns.is_none() && now >= n.last_rx_ns + NODE_RX_IDLE_NS {
                let mut d = n.react_ns.0 + self.rng.below(n.react_ns.1 - n.react_ns.0);
                if self.rng.chance(n.spike.0) {
                    d += n.spike.1;
                }
                n.wake_ns = Some(now + d);
            }
            let n = &mut self.nodes[i];
            if n.wake_ns.is_some_and(|w| now >= w && now >= n.busy_until) {
                n.wake_ns = None;
                let bytes = core::mem::take(&mut n.fifo);
                let mut assigns = 0;
                let ans = n.node.on_rx(now / US, &bytes, &mut n.out, &mut |_| assigns += 1).map(|a| a.to_vec());
                n.assigns += assigns;
                if let Some(a) = ans {
                    n.node.sent();
                    n.max_answer = n.max_answer.max(a.len());
                    self.node_send(i, &a);
                }
            }
            let n = &mut self.nodes[i];
            if now >= n.busy_until
                && n.node.next_deadline().is_some_and(|d| now / US >= d)
                && let Some(b) = n.node.tick(now / US).map(|b| b.to_vec())
            {
                self.node_send(i, &b);
            }
        }
        // Return wire: collisions, noise.
        let mut arrived: Vec<(u64, u8, usize)> = Vec::new();
        self.ret.retain(|x| {
            if x.0 <= now {
                arrived.push(*x);
                false
            } else {
                true
            }
        });
        if !arrived.is_empty() {
            arrived.sort_by_key(|x| x.0);
            let mut k = 0;
            while k < arrived.len() {
                let (t, mut b, who) = arrived[k];
                let mut j = k + 1;
                // Bytes from other nodes overlapping this one in time.
                let mut overlap = self.ret.iter().any(|x| x.2 != who && x.0 < t + self.byte_ns);
                while j < arrived.len() && arrived[j].0 < t + self.byte_ns {
                    if arrived[j].2 != who {
                        overlap = true;
                        b &= arrived[j].1;
                    }
                    j += 1;
                }
                if overlap {
                    self.collisions += 1;
                    b &= self.rng.next() as u8 | 0x81;
                }
                if self.rng.chance(self.noise_ret) {
                    b = self.rng.next() as u8;
                }
                self.hub_fifo.push(b);
                self.hub_last_rx = t;
                k = j.max(k + 1);
            }
        }
        // Hub interrupt: 64 bytes or idle.
        if self.hub_fifo.len() >= 64 || (!self.hub_fifo.is_empty() && now >= self.hub_last_rx + HUB_RX_IDLE_NS) {
            self.hub_service(true);
        }
        // Hub main loop.
        if now >= self.next_main {
            let stalled = self.stall.0 > 0 && (now % self.stall.0) < self.stall.1;
            if !stalled {
                self.hub_service(false);
            }
            self.next_main = now + self.main_period_ns;
        }
    }

    /// Per node: ids delivered strictly increasing (in order, no repeats).
    pub fn check_order(&self) -> Result<(), String> {
        for (i, n) in self.nodes.iter().enumerate() {
            for w in n.sent_ids.windows(2) {
                if w[1] <= w[0] {
                    return Err(format!("node {i}: id {} after {}", w[1], w[0]));
                }
            }
        }
        Ok(())
    }

    /// Ids generated, delivered, dropped by full queues, still on the node.
    pub fn accounting(&self) -> (u64, u64, u64, u64) {
        let mut generated = 0;
        let mut del = 0;
        let mut drop = 0;
        let mut left = 0;
        for n in &self.nodes {
            generated += n.next_id - 1;
            del += n.sent_ids.len() as u64;
            drop += n.out.dropped;
            left += (n.out.q.len() + n.out.taken.len()) as u64;
        }
        (generated, del, drop, left)
    }

    /// Latency percentiles (ns) of what was delivered after `from_ns`.
    pub fn latency(&self, from: usize) -> (u64, u64, u64, u64) {
        let mut l: Vec<u64> = self.delivered[from..].iter().map(|d| d.latency_ns).collect();
        if l.is_empty() {
            return (0, 0, 0, 0);
        }
        l.sort_unstable();
        let pct = |p: f64| l[((l.len() - 1) as f64 * p) as usize];
        let mean = l.iter().sum::<u64>() / l.len() as u64;
        (mean, pct(0.5), pct(0.99), *l.last().unwrap())
    }

    pub fn all_joined(&self) -> bool {
        self.nodes.iter().all(|n| !n.plugged || n.node.addr().is_some())
            && self.nodes.iter().filter(|n| n.plugged).all(|n| self.master.entry(&n.mac).is_some_and(|e| Some(e.addr) == n.node.addr()))
    }
}

enum OwnedEvent {
    Frame([u8; 6], Vec<u8>),
    Joined([u8; 6], u8, bool),
    Absent([u8; 6]),
    Back([u8; 6]),
    AnswerEnd,
}

impl From<Event<'_>> for OwnedEvent {
    fn from(e: Event<'_>) -> Self {
        match e {
            Event::Frame { mac, bytes, .. } => OwnedEvent::Frame(mac, bytes.to_vec()),
            Event::Joined { mac, addr, again } => OwnedEvent::Joined(mac, addr, again),
            Event::Absent { mac, .. } => OwnedEvent::Absent(mac),
            Event::Back { mac, .. } => OwnedEvent::Back(mac),
            Event::AnswerEnd { .. } => OwnedEvent::AnswerEnd,
        }
    }
}
