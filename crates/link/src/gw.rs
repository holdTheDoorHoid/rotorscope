//! Protocol 14: the ESP-NOW gateway's UART to the hub (docs/LINK-KEYS-DESIGN.md
//! 6.3). The gateway holds no key: it hands the hub every packet it heard,
//! still sealed ([`GwUp::Packet`]), and sends on the air what the hub sealed
//! ([`GwDown::Beacon`], [`GwDown::Transmit`]). Its own reports are messages of
//! their own, never [`crate::Frame`]s, so the hub takes a node frame only from
//! inside a packet it has verified, and `Via` disappears from this wire.
//!
//! Framing as for frames: postcard, COBS, a zero byte after each message. The
//! limit is [`MAX_GW_FRAME`] (a full 1024-byte packet plus its fields), not
//! [`crate::MAX_FRAME`], which stays 320 for node frames.
//!
//! **Memory.** Decoded messages borrow their packet bytes from the decoder's
//! buffer (`data: &[u8]`), and the decoder works in a buffer the caller
//! supplies ([`GwDecoder::new`]): the hub keeps its 1,100 bytes in a static
//! or in PSRAM, never in the main loop's stack frame, and nothing the size
//! of a packet is copied onto the stack. Encoding writes into the caller's
//! buffer too ([`encode_up`], [`encode_down`]).
//!
//! **Limits.** Every message is checked against its limits on both sides: a
//! message that breaks one is never encoded ([`GwError::TooLong`]) and never
//! handed out ([`GwError::Corrupt`]). A message with bytes left over after
//! it is corrupt too.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Largest encoded gateway message, COBS overhead and delimiter included.
/// Size gateway-link buffers with this.
pub const MAX_GW_FRAME: usize = 1100;
/// Largest ESP-NOW packet a [`GwUp::Packet`] or [`GwDown::Transmit`]
/// carries: `airlink::MAX_PACKET` (the `link` crate does not depend on
/// `airlink`; the hub and gateway check the two are equal).
pub const MAX_PACKET: usize = 1024;
/// Largest beacon a [`GwDown::Beacon`] carries (16 slots need 152 bytes).
pub const MAX_BEACON: usize = 160;
/// Most nodes a [`GwDown::Roster`] names (the kit's slots).
pub const MAX_ROSTER: usize = 16;

/// Why the gateway dropped packets without passing them on
/// ([`GwUp::Refused`]). It holds no key, so these are its cheap, non-secret
/// filters only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefuseReason {
    /// The sender's address is not in the roster.
    NotInRoster,
    /// A roster address over its budget (packets or bytes a second).
    OverBudget,
    /// A version 2 (kit key) packet: a board that was not reflashed.
    OldFirmware,
    /// Ours (magic "RS") but not a packet the gateway can pass on.
    Malformed,
}

impl RefuseReason {
    pub const COUNT: usize = 4;
    pub const ALL: [RefuseReason; RefuseReason::COUNT] =
        [RefuseReason::NotInRoster, RefuseReason::OverBudget, RefuseReason::OldFirmware, RefuseReason::Malformed];

    /// The `ForgedLink` `detail` the hub reports it under.
    pub const fn detail(self) -> u32 {
        use crate::origin::*;
        match self {
            RefuseReason::NotInRoster => FORGED_NOT_IN_ROSTER,
            RefuseReason::OverBudget => FORGED_OVER_BUDGET,
            RefuseReason::OldFirmware => FORGED_OLD_FIRMWARE,
            RefuseReason::Malformed => FORGED_MALFORMED,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            RefuseReason::NotInRoster => "not_in_roster",
            RefuseReason::OverBudget => "over_budget",
            RefuseReason::OldFirmware => "old_firmware",
            RefuseReason::Malformed => "malformed",
        }
    }

    /// Index into [`GwStats::refused`].
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// The gateway's running totals since it booted ([`GwUp::Stats`], once a
/// second). Each wraps at `u32::MAX`; the hub takes differences.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GwStats {
    /// Gateway uptime, seconds (tells a restart from a wrap).
    pub uptime_s: u32,
    /// Beacons the hub sent that went on the air.
    pub beacons_sent: u32,
    /// ESP-NOW packets that were not ours at all (other users of the air).
    pub foreign: u32,
    /// Packets dropped because the UART to the hub was full.
    pub uart_drops: u32,
    /// Packets refused, per [`RefuseReason`] (in [`RefuseReason::ALL`] order).
    pub refused: [u32; RefuseReason::COUNT],
}

/// Gateway to hub. Keep new variants last (postcard tags by index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GwUp<'a> {
    /// At boot and every few seconds. `protocol` must be
    /// [`crate::PROTOCOL_VERSION`] (14) on a keyed kit; the hub refuses an
    /// older gateway and says so. `mac` is the gateway's station address
    /// (every hub-sealed packet's tag covers it), `channel` its radio's.
    Hello { protocol: u8, firmware: String<16>, mac: [u8; 6], channel: u8 },
    /// One ESP-NOW packet as heard, still sealed: `src` the radio's source
    /// address, `rssi` dBm, `held_us` how long it waited in the gateway
    /// before this message was written, `data` at most [`MAX_PACKET`].
    Packet { src: [u8; 6], rssi: i8, held_us: u32, data: &'a [u8] },
    /// A [`GwDown::Transmit`] is finished: acknowledged by the radio (which
    /// proves only that something on the air answered), or given up.
    /// `attempts` sends, `held_ms` from the hub's message to the first send.
    TxDone { id: u16, to: [u8; 6], radio_acked: bool, attempts: u8, held_ms: u16 },
    /// Packets from `src` dropped for `reason` over the last `window_ms`
    /// (`count` of them, strongest at `rssi`): at most once a second per
    /// address and reason. `src` all zero: many addresses, shared.
    Refused { src: [u8; 6], reason: RefuseReason, count: u32, rssi: i8, window_ms: u32 },
    /// Once a second.
    Stats(GwStats),
}

/// Hub to gateway. Keep new variants last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GwDown<'a> {
    /// The kit's node addresses (every 5 s and on change). Packets from
    /// anyone else are refused ([`RefuseReason::NotInRoster`]).
    Roster(Vec<[u8; 6], MAX_ROSTER>),
    /// A hub-sealed beacon: broadcast as it is, once. The hub sends a fresh
    /// one every 500 ms; the gateway never repeats one.
    Beacon(&'a [u8]),
    /// A hub-sealed packet for one node: sent while the node is on channel,
    /// retried for up to `give_up_ms` (commands 2,000, time syncs 300), then
    /// [`GwUp::TxDone`]. `urgent` (time syncs): before anything queued, and
    /// dropped if more than 300 ms old.
    Transmit { id: u16, to: [u8; 6], urgent: bool, give_up_ms: u16, data: &'a [u8] },
    /// Drop whatever is queued for `to`: a newer command replaces it.
    Cancel { to: [u8; 6] },
}

impl GwUp<'_> {
    /// Within every limit of this wire.
    pub fn fits(&self) -> bool {
        match self {
            GwUp::Packet { data, .. } => data.len() <= MAX_PACKET,
            GwUp::Hello { .. } | GwUp::TxDone { .. } | GwUp::Refused { .. } | GwUp::Stats(_) => true,
        }
    }
}

impl GwDown<'_> {
    /// Within every limit of this wire.
    pub fn fits(&self) -> bool {
        match self {
            GwDown::Beacon(data) => data.len() <= MAX_BEACON,
            GwDown::Transmit { data, .. } => data.len() <= MAX_PACKET,
            GwDown::Roster(_) | GwDown::Cancel { .. } => true,
        }
    }
}

/// What went wrong on the gateway wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GwError {
    /// Decoding: more than [`MAX_GW_FRAME`] bytes (or the caller's buffer)
    /// arrived without a delimiter; they were dropped up to the next one.
    Overflow,
    /// Decoding: a delimited chunk was not one whole, valid message within
    /// its limits (corruption, a reader that joined mid-message, a peer on
    /// another protocol, or bytes left over after the message).
    Corrupt,
    /// Encoding: the message breaks a limit ([`GwUp::fits`]); nothing written.
    TooLong,
    /// Encoding: the caller's buffer is too small.
    BufferTooSmall,
}

fn encode<'b, T: Serialize>(m: &T, fits: bool, buf: &'b mut [u8]) -> Result<&'b mut [u8], GwError> {
    if !fits {
        return Err(GwError::TooLong);
    }
    let out = postcard::to_slice_cobs(m, buf).map_err(|_| GwError::BufferTooSmall)?;
    debug_assert!(out.len() <= MAX_GW_FRAME);
    Ok(out)
}

/// Serialize a gateway-to-hub message into `buf` (COBS, trailing zero
/// included). Returns the used part; send it as is. [`MAX_GW_FRAME`] bytes
/// always suffice.
pub fn encode_up<'b>(m: &GwUp<'_>, buf: &'b mut [u8]) -> Result<&'b mut [u8], GwError> {
    encode(m, m.fits(), buf)
}

/// Serialize a hub-to-gateway message, as [`encode_up`].
pub fn encode_down<'b>(m: &GwDown<'_>, buf: &'b mut [u8]) -> Result<&'b mut [u8], GwError> {
    encode(m, m.fits(), buf)
}

/// COBS-decode `buf` (no zero byte in it) in place; the decoded length, or
/// `None` if it is not valid COBS.
fn cobs_decode_in_place(buf: &mut [u8]) -> Option<usize> {
    let (mut r, mut w) = (0, 0);
    while r < buf.len() {
        let code = usize::from(buf[r]);
        if code == 0 {
            return None;
        }
        r += 1;
        let end = r + code - 1;
        if end > buf.len() {
            return None;
        }
        // `w < r` throughout, so this never overwrites unread bytes.
        for i in r..end {
            buf[w] = buf[i];
            w += 1;
        }
        r = end;
        if code != 0xFF && r < buf.len() {
            buf[w] = 0;
            w += 1;
        }
    }
    Some(w)
}

/// One whole message from a decoded chunk: no bytes left over.
fn whole<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Option<T> {
    match postcard::take_from_bytes::<T>(bytes) {
        Ok((m, rest)) if rest.is_empty() => Some(m),
        _ => None,
    }
}

/// Streaming decoder for one direction of the gateway wire, in a buffer the
/// caller owns: `GwDecoder::new(&mut BUF)` with a `static` or PSRAM array of
/// [`MAX_GW_FRAME`] bytes (a smaller buffer works, and refuses longer
/// messages as [`GwError::Overflow`]). Zero bytes between messages are
/// ignored. Feed it whatever arrived; partial messages are kept.
pub struct GwDecoder<B> {
    buf: B,
    len: usize,
    /// Dropping bytes until the next delimiter (after an overflow).
    skipping: bool,
}

impl<B: AsMut<[u8]>> GwDecoder<B> {
    pub const fn new(buf: B) -> Self {
        Self { buf, len: 0, skipping: false }
    }

    /// Drop a partial message (the line was reset).
    pub fn reset(&mut self) {
        self.len = 0;
        self.skipping = false;
    }

    /// Hand each complete chunk (COBS, delimiter removed) to `on`, in order.
    fn chunks(&mut self, input: &[u8], mut on: impl FnMut(Result<&mut [u8], GwError>)) {
        let buf = self.buf.as_mut();
        let cap = buf.len().min(MAX_GW_FRAME);
        for &b in input {
            if b == 0 {
                if self.skipping {
                    self.skipping = false;
                } else if self.len > 0 {
                    let n = core::mem::take(&mut self.len);
                    on(Ok(&mut buf[..n]));
                }
                continue;
            }
            if self.skipping {
                continue;
            }
            // Room for the bytes and the delimiter: MAX_GW_FRAME in all.
            if self.len + 1 >= cap {
                self.len = 0;
                self.skipping = true;
                on(Err(GwError::Overflow));
                continue;
            }
            buf[self.len] = b;
            self.len += 1;
        }
    }

    /// Gateway-to-hub bytes (the hub's side). `on` sees each message, or
    /// each error, in stream order; a message's `data` borrows this
    /// decoder's buffer for the length of the call.
    pub fn feed_up(&mut self, input: &[u8], mut on: impl FnMut(Result<GwUp<'_>, GwError>)) {
        self.chunks(input, |c| match c {
            Ok(chunk) => {
                let m = cobs_decode_in_place(chunk).and_then(|n| whole::<GwUp>(&chunk[..n])).filter(GwUp::fits);
                on(m.ok_or(GwError::Corrupt))
            }
            Err(e) => on(Err(e)),
        });
    }

    /// Hub-to-gateway bytes (the gateway's side), as [`GwDecoder::feed_up`].
    pub fn feed_down(&mut self, input: &[u8], mut on: impl FnMut(Result<GwDown<'_>, GwError>)) {
        self.chunks(input, |c| match c {
            Ok(chunk) => {
                let m = cobs_decode_in_place(chunk).and_then(|n| whole::<GwDown>(&chunk[..n])).filter(GwDown::fits);
                on(m.ok_or(GwError::Corrupt))
            }
            Err(e) => on(Err(e)),
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec as StdVec;

    static FULL: [u8; MAX_PACKET] = [0xFF; MAX_PACKET];

    /// One of every message, at its largest and with every integer at its
    /// widest varint, plus small ones.
    pub(crate) fn ups() -> StdVec<GwUp<'static>> {
        std::vec![
            GwUp::Hello { protocol: crate::PROTOCOL_VERSION, firmware: "gateway 0.1.0-ab".try_into().unwrap(), mac: [0xFF; 6], channel: 6 },
            GwUp::Packet { src: [0xFF; 6], rssi: i8::MIN, held_us: u32::MAX, data: &FULL },
            GwUp::Packet { src: [1, 2, 3, 4, 5, 6], rssi: -60, held_us: 120, data: &[0; 40] },
            GwUp::Packet { src: [0; 6], rssi: 0, held_us: 0, data: &[] },
            GwUp::TxDone { id: u16::MAX, to: [0xFF; 6], radio_acked: true, attempts: u8::MAX, held_ms: u16::MAX },
            GwUp::Refused { src: [0xFF; 6], reason: RefuseReason::Malformed, count: u32::MAX, rssi: i8::MIN, window_ms: u32::MAX },
            GwUp::Stats(GwStats { uptime_s: u32::MAX, beacons_sent: u32::MAX, foreign: u32::MAX, uart_drops: u32::MAX, refused: [u32::MAX; 4] }),
            GwUp::Stats(GwStats::default()),
        ]
    }

    pub(crate) fn downs() -> StdVec<GwDown<'static>> {
        std::vec![
            GwDown::Roster(core::iter::repeat_n([0xFF; 6], MAX_ROSTER).collect()),
            GwDown::Roster(Vec::new()),
            GwDown::Beacon(&FULL[..MAX_BEACON]),
            GwDown::Transmit { id: u16::MAX, to: [0xFF; 6], urgent: true, give_up_ms: u16::MAX, data: &FULL },
            GwDown::Transmit { id: 1, to: [1; 6], urgent: false, give_up_ms: 300, data: &[0; 35] },
            GwDown::Cancel { to: [0xFF; 6] },
        ]
    }

    fn wire_up(ms: &[GwUp<'_>]) -> StdVec<u8> {
        let mut out = StdVec::new();
        let mut buf = [0u8; MAX_GW_FRAME];
        for m in ms {
            out.extend_from_slice(encode_up(m, &mut buf).unwrap());
        }
        out
    }

    fn decode_up(dec: &mut GwDecoder<&mut [u8; MAX_GW_FRAME]>, bytes: &[u8]) -> StdVec<Result<std::string::String, GwError>> {
        // Messages borrow the decoder's buffer: compare their Debug text.
        let mut out = StdVec::new();
        dec.feed_up(bytes, |r| out.push(r.map(|m| std::format!("{m:?}"))));
        out
    }

    #[test]
    fn round_trip_both_ways() {
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        for m in ups() {
            let wire = wire_up(core::slice::from_ref(&m));
            assert_eq!(wire.last(), Some(&0));
            assert!(!wire[..wire.len() - 1].contains(&0));
            assert_eq!(decode_up(&mut dec, &wire), [Ok(std::format!("{m:?}"))]);
        }
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        let mut buf = [0u8; MAX_GW_FRAME];
        for m in downs() {
            let wire = encode_down(&m, &mut buf).unwrap();
            let mut got = StdVec::new();
            dec.feed_down(wire, |r| got.push(r.map(|d| d == m)));
            assert_eq!(got, [Ok(true)], "{m:?}");
        }
    }

    /// The largest messages fit `MAX_GW_FRAME` with room, and are about the
    /// size the design gives (about 1,050 bytes).
    #[test]
    fn largest_messages_fit() {
        let mut buf = [0u8; 2048];
        let up = ups().iter().map(|m| encode_up(m, &mut buf).unwrap().len()).max().unwrap();
        let down = downs().iter().map(|m| encode_down(m, &mut buf).unwrap().len()).max().unwrap();
        std::println!("largest GwUp {up} bytes, GwDown {down} bytes");
        assert!(up <= MAX_GW_FRAME && down <= MAX_GW_FRAME, "{up} {down}");
        assert!(up <= 1050 && down <= 1050, "{up} {down}");
        // A minimum (40-byte) packet costs its payload plus a few bytes.
        let small = encode_up(&ups()[2], &mut buf).unwrap().len();
        assert!(small <= 40 + 6 + 12, "{small}");
        // The beacon at its 16-slot size.
        let b = encode_down(&GwDown::Beacon(&[0xFF; 152]), &mut buf).unwrap().len();
        assert!(b <= 160, "{b}");
        // Exactly MAX_GW_FRAME of buffer always suffices.
        let mut exact = [0u8; MAX_GW_FRAME];
        for m in ups() {
            encode_up(&m, &mut exact).unwrap();
        }
        // Decoded messages borrow: no packet-sized value on the stack.
        assert!(core::mem::size_of::<GwUp>() <= 64, "{}", core::mem::size_of::<GwUp>());
        assert!(core::mem::size_of::<GwDown>() <= 128, "{}", core::mem::size_of::<GwDown>());
        // And the node frame limit did not move.
        assert_eq!(crate::MAX_FRAME, 320);
    }

    #[test]
    fn oversized_is_never_encoded() {
        let big = [1u8; MAX_PACKET + 1];
        let mut buf = [0u8; 4096];
        let p = GwUp::Packet { src: [1; 6], rssi: -1, held_us: 1, data: &big };
        assert_eq!(encode_up(&p, &mut buf), Err(GwError::TooLong));
        let t = GwDown::Transmit { id: 1, to: [1; 6], urgent: false, give_up_ms: 1, data: &big };
        assert_eq!(encode_down(&t, &mut buf), Err(GwError::TooLong));
        assert_eq!(encode_down(&GwDown::Beacon(&big[..MAX_BEACON + 1]), &mut buf), Err(GwError::TooLong));
        // A buffer too small is an error, not a panic.
        assert_eq!(encode_up(&ups()[1], &mut [0u8; 100]), Err(GwError::BufferTooSmall));
    }

    /// A message over a limit that a hostile or broken peer encoded anyway
    /// (here with postcard directly) is refused on decode.
    #[test]
    fn oversized_is_never_accepted() {
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        // A beacon of 161 bytes: well inside the frame limit, over its own.
        let mut buf = [0u8; MAX_GW_FRAME];
        let wire = postcard::to_slice_cobs(&GwDown::Beacon(&[7; MAX_BEACON + 1]), &mut buf).unwrap();
        let mut got = StdVec::new();
        dec.feed_down(wire, |r| got.push(r.is_ok()));
        assert_eq!(got, [false]);
        // A 1025-byte packet: refused (it is also past the frame limit).
        let big = [1u8; MAX_PACKET + 1];
        let mut buf = [0u8; 2048];
        let wire = postcard::to_slice_cobs(&GwUp::Packet { src: [1; 6], rssi: 0, held_us: 0, data: &big }, &mut buf).unwrap();
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        let got = decode_up(&mut dec, wire);
        assert!(got.iter().all(|r| r.is_err()), "{got:?}");
        // A run of bytes longer than any message: one overflow, then the
        // next message decodes.
        let mut stream = std::vec![0x55u8; 3 * MAX_GW_FRAME];
        stream.push(0);
        stream.extend_from_slice(&wire_up(&ups()[2..3]));
        let got = decode_up(&mut dec, &stream);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], Err(GwError::Overflow));
        assert!(got[1].is_ok());
        // The caller's smaller buffer is the limit.
        let mut small = [0u8; 64];
        let mut dec = GwDecoder::new(&mut small);
        let mut got = StdVec::new();
        dec.feed_up(&wire_up(&ups()[1..3]), |r| got.push(r.is_ok()));
        assert_eq!(got, [false, true], "the full packet overflows 64 bytes, the 40-byte one fits");
    }

    /// Every message cut short, delimited at every point: never accepted,
    /// never a panic; the next whole message still decodes.
    #[test]
    fn truncated_is_never_accepted() {
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        for m in ups() {
            let wire = wire_up(core::slice::from_ref(&m));
            let body = &wire[..wire.len() - 1];
            for cut in 1..body.len() {
                let mut chunk = body[..cut].to_vec();
                chunk.push(0);
                let got = decode_up(&mut dec, &chunk);
                assert!(got.iter().all(|r| r.is_err()), "{m:?} cut at {cut}: {got:?}");
            }
            assert_eq!(decode_up(&mut dec, &wire), [Ok(std::format!("{m:?}"))]);
        }
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        let mut buf = [0u8; MAX_GW_FRAME];
        for m in downs() {
            let wire = encode_down(&m, &mut buf).unwrap().to_vec();
            for cut in 1..wire.len() - 1 {
                let mut chunk = wire[..cut].to_vec();
                chunk.push(0);
                let mut ok = false;
                dec.feed_down(&chunk, |r| ok |= r.is_ok());
                assert!(!ok, "{m:?} cut at {cut}");
            }
        }
    }

    /// A whole message followed by extra bytes before the delimiter is not
    /// a message.
    #[test]
    fn trailing_bytes_are_corrupt() {
        let m = GwDown::Cancel { to: [9; 6] };
        let mut raw = std::vec![0u8; 64];
        let n = postcard::to_slice(&m, &mut raw).unwrap().len();
        raw.truncate(n);
        raw.push(0x42);
        let mut buf = [0u8; 64];
        let enc = cobs_encode(&raw, &mut buf);
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        let mut got = StdVec::new();
        dec.feed_down(enc, |r| got.push(r.is_ok()));
        assert_eq!(got, [false]);
    }

    /// Plain COBS, for building hostile frames by hand.
    fn cobs_encode<'b>(src: &[u8], out: &'b mut [u8]) -> &'b [u8] {
        let (mut code_at, mut w, mut code) = (0, 1, 1u8);
        for &b in src {
            if b == 0 {
                out[code_at] = code;
                code_at = w;
                w += 1;
                code = 1;
            } else {
                out[w] = b;
                w += 1;
                code += 1;
                if code == 0xFF {
                    out[code_at] = code;
                    code_at = w;
                    w += 1;
                    code = 1;
                }
            }
        }
        out[code_at] = code;
        out[w] = 0;
        &out[..w + 1]
    }

    /// The decoder's COBS agrees with postcard's on every message, and with
    /// the hand encoder on every length around the 254-byte block edge.
    #[test]
    fn cobs_agrees_with_postcard() {
        for len in [0usize, 1, 2, 253, 254, 255, 256, 507, 508, 509, 1000] {
            for fill in [0u8, 1, 0xFF] {
                let src: StdVec<u8> = (0..len).map(|i| if fill == 1 { (i % 7) as u8 } else { fill }).collect();
                let mut out = [0u8; 2048];
                let enc = cobs_encode(&src, &mut out).to_vec();
                let mut body = enc[..enc.len() - 1].to_vec();
                if body.is_empty() {
                    continue;
                }
                let n = cobs_decode_in_place(&mut body).unwrap();
                assert_eq!(&body[..n], &src[..], "len {len} fill {fill}");
            }
        }
        // A code that runs past the end is refused.
        assert_eq!(cobs_decode_in_place(&mut [5, 1, 2]), None);
    }

    #[test]
    fn split_and_run_together_and_resync() {
        let mut stream = std::vec![0x13, 0x37, 0x00, 0x00, 0x00];
        stream.extend_from_slice(&wire_up(&ups()));
        let want: StdVec<_> = ups().iter().map(|m| Ok(std::format!("{m:?}"))).collect();
        for split in (0..stream.len()).step_by(37).chain([stream.len() - 1, 1, 2, 3]) {
            let mut store = [0u8; MAX_GW_FRAME];
            let mut dec = GwDecoder::new(&mut store);
            let mut got = decode_up(&mut dec, &stream[..split]);
            got.extend(decode_up(&mut dec, &stream[split..]));
            assert_eq!(got[0], Err(GwError::Corrupt), "split {split}");
            assert_eq!(&got[1..], &want[..], "split {split}");
        }
        // Byte at a time.
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        let mut n = 0;
        for b in &stream {
            dec.feed_up(core::slice::from_ref(b), |r| n += r.is_ok() as usize);
        }
        assert_eq!(n, ups().len());
    }

    /// Random bytes never panic; whatever happens to decode is within its
    /// limits.
    #[test]
    fn garbage_never_panics() {
        let mut store = [0u8; MAX_GW_FRAME];
        let mut up = GwDecoder::new(&mut store);
        let mut store2 = [0u8; MAX_GW_FRAME];
        let mut down = GwDecoder::new(&mut store2);
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut chunk = [0u8; 211];
        for _ in 0..20_000 {
            for b in chunk.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = if x.is_multiple_of(13) { 0 } else { x as u8 };
            }
            let n = (x % 212) as usize;
            up.feed_up(&chunk[..n], |r| {
                if let Ok(m) = r {
                    assert!(m.fits());
                }
            });
            down.feed_down(&chunk[..n], |r| {
                if let Ok(m) = r {
                    assert!(m.fits());
                }
            });
        }
    }

    #[test]
    fn refuse_reasons_map_to_forged_link_details() {
        let d: StdVec<u32> = RefuseReason::ALL.iter().map(|r| r.detail()).collect();
        assert_eq!(d, [9, 10, 8, 15]);
        for (i, r) in RefuseReason::ALL.into_iter().enumerate() {
            assert_eq!(r.index(), i);
            assert!(crate::origin::forged_reason(r.detail()).is_some());
            assert_eq!(postcard::to_slice(&r, &mut [0u8; 4]).unwrap(), [i as u8]);
        }
    }
}
