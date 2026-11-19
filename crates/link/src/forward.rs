//! Protocol 5: re-sending a link stream to a second board (a "web head").
//!
//! A hub that already decodes a gateway's stream can pass the same frames
//! on, unchanged, over another UART, together with frames of its own
//! ([`Frame::HubFix`], [`Frame::Csbee`]). [`Forwarder`] is that byte queue,
//! with no I/O of its own: the firmware drains it into a UART TX FIFO
//! whenever there is room, so the hub's main loop never waits for the wire.
//!
//! Two rules keep the copy faithful:
//! - **Whole frames only.** A frame that does not fit in the free space is
//!   dropped (and counted), never cut, so the receiver never sees half a
//!   frame from the sender.
//! - **A `Via` travels with its frame.** [`Frame::Via`] names the node of the
//!   *next* frame. The forwarder holds a `Via` back and queues it together
//!   with the frame it belongs to, or drops both, so a lost frame can never
//!   shift a node's name onto someone else's frame. A `Via` followed by a
//!   stream error ([`Forwarder::reset_via`]) is dropped.
//!
//! And one rule keeps it honest: **only what the sender may originate**
//! (`origin`). The relayed stream is sorted exactly as the hub sorts it
//! ([`crate::origin::Sorter`]): a relayed frame must be one a node may send,
//! a frame with no `Via` one a node or the gateway may send, and a `Via`
//! straight after a `Via` is refused with the one it follows. So a
//! `HubFix`, `Csbee` or `Flags` from the node link never reaches the web
//! head looking like the hub's own; only [`Forwarder::send`] queues those.

use heapless::Deque;

use crate::origin::{Sorted, Sorter};
use crate::{Frame, MAX_FRAME, encode};

/// Byte queue of whole, COBS-delimited frames for a second board.
pub struct Forwarder<const N: usize> {
    queue: Deque<u8, N>,
    sorter: Sorter,
    /// Frames queued (a `Via` and its frame count as two).
    pub frames: u32,
    /// Frames dropped because the queue had no room for them.
    pub dropped: u32,
    /// Relayed frames refused because their sender may not originate them
    /// (`origin`); a refused frame's `Via` is not counted again.
    pub refused: u32,
    /// Frames that would not encode (cannot happen for decoded frames; kept
    /// so a mistake shows up as a number instead of silence).
    pub encode_err: u32,
}

impl<const N: usize> Default for Forwarder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Forwarder<N> {
    pub const fn new() -> Self {
        Self { queue: Deque::new(), sorter: Sorter::new(), frames: 0, dropped: 0, refused: 0, encode_err: 0 }
    }

    /// Pass on one frame from the decoded stream, in stream order.
    pub fn relay(&mut self, frame: &Frame) {
        match self.sorter.sort(frame) {
            Sorted::Marker => {}
            Sorted::Relayed { node, rssi } => self.push(Some(&Frame::Via { node, rssi }), frame),
            Sorted::Own => self.push(None, frame),
            Sorted::Refused { .. } => self.refused += 1,
        }
    }

    /// Queue a frame of the sender's own (not part of the relayed stream).
    /// A held `Via` stays held: it belongs to the next relayed frame.
    pub fn send(&mut self, frame: &Frame) {
        self.push(None, frame);
    }

    /// The decoded stream had an error: a held `Via` no longer names
    /// anything (the receiving hub does the same).
    pub fn reset_via(&mut self) {
        if self.sorter.reset() {
            self.dropped += 1;
        }
    }

    fn push(&mut self, via: Option<&Frame>, frame: &Frame) {
        let mut a = [0u8; MAX_FRAME];
        let mut b = [0u8; MAX_FRAME];
        let first = match via.map(|v| encode(v, &mut a)) {
            None => &[][..],
            Some(Ok(bytes)) => &bytes[..],
            Some(Err(_)) => {
                self.encode_err += 1;
                &[][..]
            }
        };
        let Ok(second) = encode(frame, &mut b) else {
            self.encode_err += 1;
            self.dropped += u32::from(via.is_some());
            return;
        };
        let n = u32::from(!first.is_empty()) + 1;
        if self.free() < first.len() + second.len() {
            self.dropped += n;
            return;
        }
        for &x in first.iter().chain(second.iter()) {
            let _ = self.queue.push_back(x);
        }
        self.frames += n;
    }

    /// Bytes of room left.
    pub fn free(&self) -> usize {
        N - self.queue.len()
    }

    /// Bytes waiting.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The next waiting bytes (contiguous; there may be more after them).
    pub fn peek(&self) -> &[u8] {
        self.queue.as_slices().0
    }

    /// Forget `n` bytes from the front: they were written to the wire.
    pub fn consume(&mut self, n: usize) {
        for _ in 0..n {
            self.queue.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Decoder, Detection, NodeRole};

    fn det(fill: u8) -> Frame {
        // A full pack: ~250 bytes on the wire, so small queues fill fast.
        let mut messages = heapless::Vec::new();
        while messages.push([fill; rid::MESSAGE_SIZE]).is_ok() {}
        Frame::Detection(Detection {
            transport: rid::Transport::WifiBeacon,
            channel: 6,
            rssi: -50,
            mac: [fill; 6],
            counter: fill,
            uptime_us: 1,
            messages,
        })
    }

    fn via(n: u8) -> Frame {
        Frame::Via { node: [2, 0, 0, 0, 0, n], rssi: -40 }
    }

    fn drain<const N: usize>(f: &mut Forwarder<N>) -> std::vec::Vec<Frame> {
        let mut bytes = std::vec::Vec::new();
        while !f.is_empty() {
            let chunk = f.peek().len().min(7); // odd-sized writes, as a FIFO takes them
            bytes.extend_from_slice(&f.peek()[..chunk]);
            f.consume(chunk);
        }
        let mut out = std::vec::Vec::new();
        Decoder::new().feed(&bytes, |r| out.push(r.unwrap()));
        out
    }

    #[test]
    fn stream_comes_out_unchanged() {
        let mut f = Forwarder::<4096>::new();
        let hello = Frame::Hello { role: NodeRole::Ble, protocol: 4, firmware: "x".try_into().unwrap() };
        let input = [via(1), hello.clone(), via(2), det(1), det(2), via(1), det(3)];
        for fr in &input {
            f.relay(fr);
        }
        assert_eq!(drain(&mut f), input.to_vec());
        assert_eq!((f.frames, f.dropped), (7, 0));
    }

    #[test]
    fn via_and_its_frame_are_dropped_together() {
        let mut probe = [0u8; MAX_FRAME];
        let det_len = encode(&det(1), &mut probe).unwrap().len();
        let via_len = encode(&via(1), &mut probe).unwrap().len();
        // Room for one Via + Detection pair and a bit, not two.
        let mut f = Forwarder::<400>::new();
        assert!(400 >= via_len + det_len && 400 < 2 * (via_len + det_len), "{via_len} {det_len}");
        f.relay(&via(1));
        f.relay(&det(1));
        f.relay(&via(2));
        f.relay(&det(2)); // no room: both go
        assert_eq!(f.dropped, 2);
        assert_eq!(drain(&mut f), [via(1), det(1)].to_vec());
        // Room again: the next pair goes through and keeps its own node.
        f.relay(&via(3));
        f.relay(&det(3));
        assert_eq!(drain(&mut f), [via(3), det(3)].to_vec());
    }

    #[test]
    fn own_frames_do_not_steal_a_held_via() {
        let mut f = Forwarder::<4096>::new();
        let own = Frame::Csbee(b"#S:x".iter().copied().collect());
        f.relay(&via(5));
        f.send(&own);
        f.relay(&det(5));
        assert_eq!(drain(&mut f), [own, via(5), det(5)].to_vec());
    }

    #[test]
    fn orphan_via_is_dropped() {
        let mut f = Forwarder::<4096>::new();
        f.relay(&via(3));
        f.reset_via();
        f.relay(&det(9)); // a frame of the link itself
        assert_eq!(drain(&mut f), [det(9)].to_vec());
        assert_eq!(f.dropped, 1);
    }

    /// Two `Via`s in a row: the second is a node's frame posing as a marker
    /// (a gateway writes each `Via` with its frame). Neither is passed on,
    /// and neither names the frame after them.
    #[test]
    fn via_after_via_is_refused_with_the_one_it_follows() {
        let mut f = Forwarder::<4096>::new();
        f.relay(&via(1));
        f.relay(&via(2));
        let gw_hello = Frame::Hello { role: NodeRole::Gateway, protocol: 13, firmware: "gw key x".try_into().unwrap() };
        f.relay(&gw_hello);
        f.relay(&via(1));
        f.relay(&det(1));
        assert_eq!(drain(&mut f), [gw_hello, via(1), det(1)].to_vec());
        assert_eq!((f.refused, f.dropped), (1, 0));
    }

    fn hub_only() -> [Frame; 3] {
        [
            Frame::HubFix(crate::HubFix { state: crate::GnssState::Fix3d, sats: Some(9), position: None, utc_ms: Some(1), utc_pps: true, held: false, utc_held: false }),
            Frame::Csbee(b"#A:x".iter().copied().collect()),
            Frame::Flags(crate::FlagReport { who: "x".try_into().unwrap(), level: 3, items: heapless::Vec::new(), pack_date: 0 }),
        ]
    }

    #[test]
    fn relayed_hub_frames_never_reach_the_output() {
        let mut f = Forwarder::<4096>::new();
        for fr in &hub_only() {
            f.relay(&via(1)); // from a node, through the gateway
            f.relay(fr);
            f.relay(fr); // the link's own (no Via): not the gateway's to send either
        }
        f.relay(&via(2));
        f.relay(&det(2));
        assert_eq!(drain(&mut f), [via(2), det(2)].to_vec());
        assert_eq!(f.refused, 6);
        // The hub's own go through.
        for fr in &hub_only() {
            f.send(fr);
        }
        assert_eq!(drain(&mut f), hub_only().to_vec());
    }

    #[test]
    fn relayed_bus_control_and_forged_reports_are_refused() {
        let mut f = Forwarder::<4096>::new();
        let forged = [
            Frame::BusJoin { mac: [1; 6], want: 0, check: 0 },
            Frame::BusDone { addr: 1, seq: 1, more: false, crc: 0 },
            Frame::Hello { role: NodeRole::Gateway, protocol: 13, firmware: "gw key x".try_into().unwrap() },
        ];
        for fr in &forged {
            f.relay(&via(1));
            f.relay(fr);
        }
        assert!(drain(&mut f).is_empty());
        assert_eq!(f.refused, 3);
    }
}
