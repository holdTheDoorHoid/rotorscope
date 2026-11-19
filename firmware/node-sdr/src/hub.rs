//! What this node tells the hub (`crates/link`, protocol 4), and what the hub
//! tells it.
//!
//! Frames are encoded here as they happen and queued; the transport (the
//! ESP-NOW uplink, feature `espnow`) takes them from the queue between
//! captures. Without a transport they are counted and dropped, so the
//! capture path is the same in every build.
//!
//! With the `wirebus` feature the capture loop moves queued frames to
//! [`BUS_OUT`] for the bus task (`node_common::bus`) and applies the
//! assignments it puts in [`BUS_COMMANDS`].
//!
//! - `Hello` + `Status` (and, protocol 14, `NodeHealth`) at boot, every
//!   [`KEEPALIVE_US`] and whenever the role or plan changes (the hub forgets
//!   a node after 15 s of silence).
//! - `DroneId`: one per decoded or detected burst, as it comes.
//! - `Rf`: FPV scanner verdicts, at most one per channel per [`RF_REPEAT_US`]
//!   unless the kind changes.
//! - `Threat` (protocol 9): jamming on the drone bands (`radio/jam.rs`), each
//!   kind at most about once a second while it lasts.

use heapless::{Deque, Vec};
use link::{Assignment, Command, CommandDecoder, Frame, MAX_FRAME, NodeRole, PROTOCOL_VERSION, RfSighting};

pub type Encoded = Vec<u8, MAX_FRAME>;

/// Wired bus (feature `wirebus`): frames for the bus task, and the hub's
/// assignments from it.
#[cfg(feature = "wirebus")]
pub static BUS_OUT: embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, Encoded, 8> =
    embassy_sync::channel::Channel::new();
#[cfg(feature = "wirebus")]
pub static BUS_COMMANDS: embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, Assignment, 2> =
    embassy_sync::channel::Channel::new();

/// Frames waiting for the uplink. When full the oldest is dropped (and
/// counted): a stale sighting is worth less than a fresh one.
const QUEUE: usize = 16;
pub const KEEPALIVE_US: u64 = 5_000_000;
pub const RF_REPEAT_US: u64 = 10_000_000;

/// What this image can do: both raw-capture jobs.
pub const CAN: u8 = NodeRole::DroneId.bit() | NodeRole::Fpv.bit();

pub struct Hub {
    q: Deque<Encoded, QUEUE>,
    pub role: NodeRole,
    pub plan_id: u16,
    next_hello_us: u64,
    /// Per channel (MHz): last kind reported and when.
    rf_last: Vec<(u16, link::RfKind, u64), 48>,
    pub queued: u32,
    pub dropped: u32,
    /// Frames built in HIL runs: encoded and checked, never queued.
    pub hil_frames: u32,
    pub droneid: u32,
    pub rf: u32,
    pub threats: u32,
    pub commands: u32,
    pub bad_commands: u32,
    dec: CommandDecoder,
}

fn now_us() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_micros()
}

pub fn encode(f: &Frame) -> Option<Encoded> {
    let mut buf = [0u8; MAX_FRAME];
    let wire = link::encode(f, &mut buf).ok()?;
    Vec::from_slice(wire).ok()
}

impl Hub {
    pub fn new(role: NodeRole) -> Self {
        Hub {
            q: Deque::new(),
            role,
            plan_id: 0,
            next_hello_us: 0,
            rf_last: Vec::new(),
            queued: 0,
            dropped: 0,
            hil_frames: 0,
            droneid: 0,
            rf: 0,
            threats: 0,
            commands: 0,
            bad_commands: 0,
            dec: CommandDecoder::new(),
        }
    }

    pub fn push(&mut self, f: &Frame) {
        let Some(e) = encode(f) else {
            self.dropped += 1;
            return;
        };
        if self.q.is_full() {
            let _ = self.q.pop_front();
            self.dropped += 1;
        }
        let _ = self.q.push_back(e);
        self.queued += 1;
    }

    pub fn len(&self) -> usize {
        self.q.len()
    }

    pub fn front(&self) -> Option<&Encoded> {
        self.q.front()
    }

    pub fn pop(&mut self) -> Option<Encoded> {
        self.q.pop_front()
    }

    /// The role or plan changed: say so at the next chance.
    pub fn set(&mut self, role: NodeRole, plan_id: u16) {
        if (role, plan_id) != (self.role, self.plan_id) {
            self.role = role;
            self.plan_id = plan_id;
            self.next_hello_us = 0;
        }
    }

    /// Queue Hello + Status now.
    pub fn keepalive_now(&mut self) {
        self.next_hello_us = 0;
        self.keepalive();
    }

    /// Queue Hello + Status if due. Returns whether it did.
    pub fn keepalive(&mut self) -> bool {
        let t = now_us();
        if t < self.next_hello_us {
            return false;
        }
        self.next_hello_us = t + KEEPALIVE_US;
        let mut fw = heapless::String::<16>::new();
        let _ = fw.push_str("node-sdr ");
        let _ = fw.push_str(env!("CARGO_PKG_VERSION"));
        self.push(&Frame::Hello { role: self.role, protocol: PROTOCOL_VERSION, firmware: fw });
        self.push(&Frame::Status { role: self.role, can: CAN, plan: self.plan_id });
        self.push_node_health();
        true
    }

    /// Protocol 14: `NodeHealth` with the Hello, every 5 s. No per-kind
    /// budget here: frames dropped from a full queue count as `other`.
    /// Out of line, so its frame is never part of the capture loop's.
    #[inline(never)]
    fn push_node_health(&mut self) {
        let mut shed = [0u32; link::SHED_KINDS];
        shed[link::SHED_KINDS - 1] = self.dropped;
        self.push(&Frame::NodeHealth(node_common::node_health(shed, [0, 0])));
    }

    pub fn droneid(&mut self, s: link::DroneIdSighting, hil: bool) {
        let f = Frame::DroneId(s);
        if hil {
            // Built and round-tripped exactly as a live one, but not sent: a
            // recording must never reach the hub as a live drone.
            if let Some(e) = encode(&f) {
                let mut dec = link::Decoder::new();
                let mut ok = false;
                dec.feed(&e, |r| ok = r.as_ref().is_ok_and(|g| *g == f));
                if ok {
                    self.hil_frames += 1;
                }
            }
            return;
        }
        self.droneid += 1;
        self.push(&f);
    }

    /// An FPV verdict; rate-limited per channel.
    pub fn rf(&mut self, s: RfSighting, mhz: u16) {
        let t = s.uptime_us;
        match self.rf_last.iter_mut().find(|e| e.0 == mhz) {
            Some(e) if e.1 == s.kind && t < e.2 + RF_REPEAT_US => return,
            Some(e) => *e = (mhz, s.kind, t),
            None => {
                if self.rf_last.is_full() {
                    self.rf_last.remove(0);
                }
                let _ = self.rf_last.push((mhz, s.kind, t));
            }
        }
        self.rf += 1;
        self.push(&Frame::Rf(s));
    }

    /// Jamming seen by the node (`radio/jam.rs` already limits each kind to
    /// about one a second).
    pub fn threat(&mut self, s: link::ThreatSighting) {
        self.threats += 1;
        self.push(&Frame::Threat(s));
    }

    /// Feed hub-to-node bytes (one or more whole COBS command frames);
    /// returns the last assignment for this node, if any.
    pub fn commands(&mut self, bytes: &[u8]) -> Option<Assignment> {
        let mut got = None;
        let (mut good, mut bad) = (0, 0);
        self.dec.feed(bytes, |r| match r {
            Ok(Command::Assign(a)) => {
                good += 1;
                got = Some(a);
            }
            Ok(Command::To { .. }) => good += 1,
            // Protocol 10 bus commands never come this way (the bus glue
            // takes them); counted as good, ignored.
            Ok(_) => good += 1,
            Err(_) => bad += 1,
        });
        self.commands += good;
        self.bad_commands += bad;
        got
    }
}

/// The role numbers this image accepts in an assignment.
pub fn role_name(r: NodeRole) -> &'static str {
    match r {
        NodeRole::Wifi24 => "wifi24",
        NodeRole::Wifi5 => "wifi5",
        NodeRole::Ble => "ble",
        NodeRole::Fpv => "fpv",
        NodeRole::DroneId => "droneid",
        NodeRole::Gateway => "gateway",
        NodeRole::Aero => "aero",
    }
}
