//! Many radio nodes on one hub UART (PLAN.md Phase 5a, docs/WIREBUS.md).
//!
//! The hub's TX goes to every node's RX; every node's TX goes, open-drain,
//! onto one shared return wire with one pull-up. Only one node may talk at a
//! time, so the hub (the bus [`master`]) polls them in turn:
//!
//! - [`link::Command::Poll`] "node N, go ahead": node N answers at once with
//!   its queued `link` frames (whole frames, up to a byte budget) and a
//!   [`link::Frame::BusDone`] carrying a CRC of the answer. Everyone else
//!   stays silent. The hub keeps an answer only if the CRC matches; the next
//!   poll acknowledges it, and a node resends an answer that was not
//!   acknowledged (stop-and-wait, one answer in flight per node).
//! - [`link::Command::JoinSlot`] "anyone new?", now and then: a node without
//!   an address answers with a [`link::Frame::BusJoin`] (its station MAC) in
//!   a random slot; two nodes in the same slot collide, their frames fail the
//!   check, and both back off at random and try again. The hub answers each
//!   good join with [`link::Command::Address`]: a short bus address bound to
//!   the MAC, the same one every time that MAC joins (image switch, reboot).
//! - Commands for one node ([`link::Command::Assign`]) go out as on the
//!   ESP-NOW gateway link: [`link::Command::To`] naming the node, then the
//!   command. Every node hears them; only the named one takes them.
//!
//! The hub tells the rest of its firmware who sent a frame by MAC, exactly as
//! a gateway's [`link::Frame::Via`] does, so trackers and reports do not
//! change. `link` frames travel unchanged: a node answers with the same bytes
//! it would write to a point-to-point UART.
//!
//! Both ends here are pure state machines driven by explicit time
//! (microseconds) and received bytes, so the whole protocol runs on a laptop
//! against a simulated wire (`tests` module). The firmware glue only moves
//! bytes and reads a clock.
#![cfg_attr(not(test), no_std)]

pub mod crc;
pub mod master;
pub mod node;

#[cfg(test)]
mod sim;
#[cfg(test)]
mod tests;

pub use master::{BusStats, Event, Master, MasterConfig, NodeStats};
pub use node::{Node, NodeConfig, NodeCounters, Outbox, TimeSyncRx};

/// Default line speed. The XIAO ESP32-C5 and the ESP32-P4 both run 921600
/// cleanly over short wires; RS-485 transceivers keep it over long ones.
pub const BAUD: u32 = 921_600;

/// Largest answer budget the hub asks for, in bytes (about 11 ms at 921600).
pub const MAX_BUDGET: usize = 1024;

/// Largest unit a node sends as one piece: one `link` frame, or a frame with
/// a [`link::Frame::Via`] in front (an ESP-NOW gateway relaying). A unit is
/// never split across two answers.
pub const MAX_UNIT: usize = link::MAX_FRAME + 16;

/// Largest encoded [`link::Frame::BusDone`] / [`link::Frame::BusJoin`].
pub const MAX_DONE: usize = 16;

/// Largest answer on the wire: a full budget (or one unit longer than the
/// budget, which is always allowed), then the `BusDone`.
pub const MAX_ANSWER: usize = if MAX_BUDGET > MAX_UNIT { MAX_BUDGET } else { MAX_UNIT } + MAX_DONE;

/// Highest bus address. Addresses are 1..=MAX_ADDR (0 = none).
pub const MAX_ADDR: u8 = 32;

/// Postcard variant index of `Frame::BusDone` / `Frame::BusJoin`, checked by
/// a test against `link`.
pub const TAG_BUS_JOIN: u8 = 14;
pub const TAG_BUS_DONE: u8 = 15;
/// Postcard variant index of `Frame::Via`.
pub const TAG_VIA: u8 = 4;

/// Microseconds `bytes` take on the wire at `baud` (8N1: 10 bits a byte),
/// rounded up.
pub const fn wire_us(bytes: usize, baud: u32) -> u64 {
    let bits = bytes as u64 * 10 * 1_000_000;
    bits.div_ceil(baud as u64)
}

/// The postcard variant index of one COBS frame (delimiter included or not)
/// without decoding it: the first decoded byte. `None` for an empty chunk.
/// Every variant index used by `link` is below 128, so it is one varint byte.
pub fn chunk_tag(chunk: &[u8]) -> Option<u8> {
    match chunk {
        [] | [0, ..] => None,
        // COBS code 1: the first data byte is a zero.
        [1, ..] => Some(0),
        [_, first, ..] if *first != 0 => Some(*first),
        _ => None,
    }
}

/// Whether a frame with this variant index is a `Via` marker.
pub fn is_via(chunk: &[u8]) -> bool {
    chunk_tag(chunk) == Some(TAG_VIA)
}

/// Bus addresses whose node-supplied `Via` the [`Attributor`] honours.
pub const MAX_GATEWAYS: usize = 4;

/// Names the sender of every frame from the bus the way an ESP-NOW gateway
/// does, so a hub can feed bus frames into the same decoder and node table
/// as a gateway link: a [`link::Frame::Via`] with the polled node's MAC goes
/// before each of its frames.
///
/// Origin rules (`link::origin`, docs/WIREBUS.md "Who may send what"):
/// - a bus node's frame passes only if a node may originate it; anything
///   else (`HubFix`, `Csbee`, `Flags`, bus control inside an answer, a
///   `Hello` claiming the gateway role, a `ForgedLink` report...) is
///   refused, counted and logged for a `ForgedLink` report;
/// - a node-supplied `Via` is honoured only from a node registered as a
///   gateway ([`Attributor::register_gateway`]); from any other node it is
///   refused, so no node can attribute frames to another MAC. A
///   registered gateway's `Via` is held and passed on together with its
///   frame, which must itself be a node's;
/// - a registered gateway's own frames (its `Hello`, its `ForgedLink`
///   reports) pass with no `Via`, exactly as on a point-to-point gateway
///   link.
///
/// A frame whose start is no known frame is passed on with the sender's
/// `Via` (the hub's decoder counts it as corrupt), as before. The check
/// reads only a frame's first bytes (`link::origin::origin_of_encoded`):
/// it runs in the hub's UART interrupt.
#[derive(Debug, Default)]
pub struct Attributor {
    /// A registered gateway's `Via`, waiting for its frame.
    held: Option<heapless::Vec<u8, 16>>,
    gateways: heapless::Vec<[u8; 6], MAX_GATEWAYS>,
    /// `Via`s written by us, frames passed through under a gateway's own `Via`.
    pub added: u32,
    pub relayed: u32,
    /// Frames refused because their sender may not originate them.
    pub refused: u32,
    /// The same, per claimed sender, for the hub's once-a-second
    /// `ForgedLink` report (`link::origin::FORGED_ORIGIN`).
    pub log: link::origin::RefusalLog<4>,
}

impl Attributor {
    pub const fn new() -> Self {
        Self {
            held: None,
            gateways: heapless::Vec::new(),
            added: 0,
            relayed: 0,
            refused: 0,
            log: link::origin::RefusalLog::new(),
        }
    }

    /// Trust the node with this station MAC to relay others (an ESP-NOW
    /// gateway sitting on the bus): its `Via` markers are honoured. False if
    /// the list is full. No firmware registers one today (the gateway has
    /// no bus mode); a hub setting would.
    pub fn register_gateway(&mut self, mac: [u8; 6]) -> bool {
        self.gateways.contains(&mac) || self.gateways.push(mac).is_ok()
    }

    pub fn is_gateway(&self, mac: &[u8; 6]) -> bool {
        self.gateways.contains(mac)
    }

    fn refuse(&mut self, mac: [u8; 6]) {
        self.refused = self.refused.wrapping_add(1);
        // rssi 0: a wire has no signal strength.
        self.log.record(mac, link::origin::FORGED_ORIGIN, 0);
    }

    /// One frame (COBS, delimiter included) from an answer of node `mac`;
    /// `out` gets the bytes to pass on, in order.
    pub fn frame(&mut self, mac: [u8; 6], bytes: &[u8], out: &mut impl FnMut(&[u8])) {
        let gateway = self.is_gateway(&mac);
        if is_via(bytes) {
            // A Via straight after a Via: the first named nothing.
            let pending = self.held.take().is_some();
            match heapless::Vec::from_slice(bytes) {
                Ok(v) if gateway && !pending => self.held = Some(v),
                _ => self.refuse(mac),
            }
            return;
        }
        // From the frame's first bytes: this runs in the UART interrupt,
        // where a whole decoded frame would be ~600 bytes of stack.
        let origin = link::origin::origin_of_encoded(bytes);
        if let Some(via) = self.held.take() {
            // A gateway relaying: the frame must be one a node may send.
            if origin.is_some_and(|o| o != link::origin::Origin::Node) {
                self.refuse(mac);
                return;
            }
            self.relayed += 1;
            out(&via);
            out(bytes);
            return;
        }
        match origin {
            // Undecodable: with its sender's Via; the hub counts it corrupt.
            None | Some(link::origin::Origin::Node) => {}
            // A registered gateway's own frame: the link's own, no Via.
            Some(link::origin::Origin::Gateway) if gateway => {
                out(bytes);
                return;
            }
            Some(_) => {
                self.refuse(mac);
                return;
            }
        }
        let mut buf = [0u8; 16];
        // rssi 0: a wire has no signal strength.
        if let Ok(v) = link::encode(&link::Frame::Via { node: mac, rssi: 0 }, &mut buf) {
            self.added += 1;
            out(v);
        }
        out(bytes);
    }

    /// An answer ended: a `Via` left without its frame names nothing.
    pub fn end_answer(&mut self) {
        self.held = None;
    }
}
