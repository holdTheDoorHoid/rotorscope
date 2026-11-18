//! Output: JSON lines for the laptop (USB) and link frames for the hub (UART).
//!
//! Both go through byte pipes drained by their own tasks, so a USB host that
//! is not reading can never stall the hub link or the radio. A line or frame
//! is written whole or not at all, so a reader never sees half of one.

use core::fmt::Write as _;
use core::sync::atomic::AtomicU32;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use rid::message::{self, Message};
use rid::{RawMessage, Transport};
use serde::ser::{SerializeSeq, Serializer};
use serde::Serialize;

use crate::adv::{COUNTERS, bump};

/// A few seconds of output at ambient rates; a full pack line is ~3 KB.
pub static USB: Pipe<CriticalSectionRawMutex, 8192> = Pipe::new();
pub static UART: Pipe<CriticalSectionRawMutex, 2048> = Pipe::new();

/// Largest JSON line: 9 decoded messages of the widest type (Location, ~4.2 KB)
/// plus their raw hex and the envelope. Longer lines are dropped (counted as USB drops).
pub const MAX_LINE: usize = 6 * 1024;

/// Write all of `bytes` to `pipe`, or nothing if it does not fit.
///
/// The free-space check and the write must be one atomic step because the
/// logger can be called from radio threads that preempt the executor.
pub fn emit<const N: usize>(pipe: &Pipe<CriticalSectionRawMutex, N>, bytes: &[u8], drops: &AtomicU32) -> bool {
    critical_section::with(|_| {
        if pipe.free_capacity() < bytes.len() {
            bump(drops);
            return false;
        }
        let mut rest = bytes;
        // The ring buffer hands out contiguous space only, so a wrap needs
        // two writes; with the space checked above this always finishes.
        while !rest.is_empty() {
            match pipe.try_write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(_) => break,
            }
        }
        true
    })
}

/// Serialize `value` as one JSON line onto the USB pipe.
pub fn json_line<T: Serialize>(value: &T, buf: &mut [u8]) {
    match serde_json_core::to_slice(value, buf) {
        Ok(n) if n < buf.len() => {
            buf[n] = b'\n';
            emit(&USB, &buf[..=n], &COUNTERS.usb_drops);
        }
        _ => bump(&COUNTERS.usb_drops),
    }
}

/// Encode a link frame onto the UART pipe.
pub fn hub_frame(frame: &link::Frame) {
    let mut buf = [0u8; link::MAX_FRAME];
    match link::encode(frame, &mut buf) {
        Ok(wire) => {
            emit(&UART, wire, &COUNTERS.uart_drops);
        }
        Err(_) => bump(&COUNTERS.uart_drops),
    }
}

/// "AA:BB:CC:DD:EE:FF".
pub struct Mac(pub [u8; 6]);

impl Serialize for Mac {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out: heapless::String<17> = heapless::String::new();
        for (i, b) in self.0.iter().enumerate() {
            let _ = write!(out, "{}{:02X}", if i == 0 { "" } else { ":" }, b);
        }
        s.serialize_str(&out)
    }
}

/// Lowercase hex, for messages we could not decode.
struct Hex<'a>(&'a [u8]);

impl Serialize for Hex<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out: heapless::String<{ 2 * rid::MESSAGE_SIZE }> = heapless::String::new();
        for b in self.0.iter().take(rid::MESSAGE_SIZE) {
            let _ = write!(out, "{b:02x}");
        }
        s.serialize_str(&out)
    }
}

#[derive(Serialize)]
struct BadMessage<'a> {
    error: rid::Error,
    raw: Hex<'a>,
}

/// The raw messages as hex, so a reader can re-decode exactly what was heard
/// rather than trusting a round trip through the JSON form.
struct RawList<'a>(&'a [RawMessage]);

impl Serialize for RawList<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|m| Hex(m)))
    }
}

/// Decodes while serializing, so no decoded copy of the pack is held.
struct Msgs<'a>(&'a [RawMessage]);

impl Serialize for Msgs<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for raw in self.0 {
            match message::decode(raw) {
                Ok(m) => seq.serialize_element::<Message>(&m)?,
                Err(error) => seq.serialize_element(&BadMessage { error, raw: Hex(raw) })?,
            }
        }
        seq.end()
    }
}

#[derive(Serialize)]
pub struct RidLine<'a> {
    t: &'static str,
    tr: Transport,
    ch: u8,
    rssi: i8,
    mac: Mac,
    ctr: u8,
    up_us: u64,
    /// Present (true) only when part of the advert was lost.
    #[serde(skip_serializing_if = "core::ops::Not::not")]
    trunc: bool,
    msgs: Msgs<'a>,
    raw: RawList<'a>,
}

impl<'a> RidLine<'a> {
    pub fn new(d: &'a link::Detection, truncated: bool) -> Self {
        Self {
            t: "rid",
            tr: d.transport,
            ch: d.channel,
            rssi: d.rssi,
            mac: Mac(d.mac),
            ctr: d.counter,
            up_us: d.uptime_us,
            trunc: truncated,
            msgs: Msgs(&d.messages),
            raw: RawList(&d.messages),
        }
    }
}

/// An advert that matched the Remote ID service data but whose payload the
/// `rid` crate rejected. Kept on the USB stream because it is exactly what a
/// developer bringing up a new transmitter needs to see.
#[derive(Serialize)]
pub struct RidErrLine<'a> {
    pub t: &'static str,
    pub tr: Transport,
    pub rssi: i8,
    pub mac: Mac,
    pub up_us: u64,
    pub error: rid::Error,
    pub len: usize,
    pub data: HexAll<'a>,
}

/// Full-length hex (up to one advert).
pub struct HexAll<'a>(pub &'a [u8]);

impl Serialize for HexAll<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out: heapless::String<{ 2 * crate::adv::MAX_ADV }> = heapless::String::new();
        for b in self.0 {
            let _ = write!(out, "{b:02x}");
        }
        s.serialize_str(&out)
    }
}

#[derive(Serialize)]
pub struct ScanInfo {
    pub phys: &'static str,
    pub interval_ms: u64,
    pub window_ms: u64,
    pub active: bool,
    pub dup_filter: bool,
}

#[derive(Serialize)]
pub struct HelloLine<'a> {
    pub t: &'static str,
    pub node: &'static str,
    pub fw: &'static str,
    pub chip: &'static str,
    pub proto: u8,
    pub scan: ScanInfo,
    pub uart_baud: u32,
    /// Hub frames go over ESP-NOW (true) or the UART.
    pub espnow: bool,
    /// Roles this board can take (mask of `NodeRole::bit`), the image in each
    /// app slot (`null`: empty or unknown), and the plan id applied.
    pub can: u8,
    pub images: &'a [Option<&'static str>],
    pub plan_id: u16,
    /// Why the chip last reset (`node_common::fault::name`), and whether
    /// this is a `faulttest` bench build.
    pub reset: &'static str,
    pub faulttest: bool,
    /// Bench features and build switches compiled in (`[]`: a normal build;
    /// build.rs, docs/BENCH.md "Bench builds").
    pub bench: heapless::Vec<&'static str, 8>,
}

/// `assign`: a new plan id from the hub; `switch`: leaving for another image.
#[derive(Serialize)]
pub struct AssignLine {
    pub t: &'static str,
    pub role: link::NodeRole,
    pub plan_id: u16,
}

/// ESP-NOW uplink counters, totals since boot (`uplink.rs`).
#[derive(Serialize)]
pub struct UplinkLine {
    pub t: &'static str,
    pub gw: bool,
    pub beacons: u32,
    pub cmds: u32,
    pub cmd_dups: u32,
    pub pkts: u32,
    pub frames: u32,
    pub failed: u32,
    pub dropped: u32,
    pub oversize: u32,
    /// Wi-Fi gaps the scan loop gave, and sends tried in them.
    pub gaps: u32,
    pub tries: u32,
    pub send_us: u32,
}

/// Per-second counts (deltas over the last `win_ms`), plus a few running
/// totals so a reader that misses lines can still tell what happened.
#[derive(Serialize, Default)]
pub struct StatsLine {
    pub t: &'static str,
    pub up_us: u64,
    pub win_ms: u32,
    /// All adverts seen, then split by how they arrived.
    pub adv: u32,
    pub legacy: u32,
    pub ext1m: u32,
    pub coded: u32,
    /// Old-style (non-extended) report events; expected to stay 0.
    pub old: u32,
    /// Remote ID: queued by the handler, found by the decoder, unparseable.
    pub rid: u32,
    pub rid_ok: u32,
    pub rid_err: u32,
    pub trunc: u32,
    pub drops: u32,
    pub usb_drops: u32,
    pub uart_drops: u32,
    pub hci_err: u32,
    pub scan_err: u32,
    pub heap_free: usize,
    /// Plan id applied, and hub commands: applied / repeats / rejected.
    pub plan_id: u16,
    pub cmds: [u32; 3],
    pub tot_adv: u32,
    pub tot_rid: u32,
    pub tot_drops: u32,
    /// Pairing pop-up adverts (`wifiwatch::ble`) in the window, of those from
    /// addresses not heard recently, and spam reports, total.
    pub pair: u32,
    pub pair_new: u32,
    pub threat: u32,
}

/// `{"t":"threat",...}`: Bluetooth pairing pop-up spam (`ThreatKind::BleSpam`).
/// `fam` = the pairing family with the most new addresses, `new` = new
/// addresses in the window, `n` = pairing adverts in the window.
#[derive(Serialize)]
pub struct ThreatLine<'a> {
    pub t: &'static str,
    pub kind: &'static str,
    pub fam: &'a str,
    pub new: u32,
    pub n: u32,
    pub win_ms: u32,
    pub rssi: i8,
    pub up_us: u64,
}

impl<'a> ThreatLine<'a> {
    pub fn new(s: &'a link::ThreatSighting) -> Self {
        Self {
            t: "threat",
            kind: s.kind.name(),
            fam: &s.ssid,
            new: s.detail,
            n: s.count,
            win_ms: s.window_ms,
            rssi: s.level,
            up_us: s.uptime_us,
        }
    }
}

