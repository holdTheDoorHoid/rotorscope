//! Both outputs: JSON lines on USB (for a laptop / DragonSync) and
//! postcard+COBS `link::Frame`s on the hub UART.
//!
//! The USB stream is machine-read, so only whole JSON objects go on it, one
//! per line, each with a `"t"` type tag. The only exception is a panic
//! backtrace, which a reader should treat as a crash marker.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
#[cfg(not(feature = "espnow"))]
use esp_hal::Async;
#[cfg(not(feature = "espnow"))]
use esp_hal::uart::UartTx;
use heapless::Vec;
use rid::{MAX_PACK, RawMessage, Transport};
use serde::ser::{SerializeSeq, Serializer};
use serde::Serialize;

use crate::chip::{self, Hop};

pub type Messages = Vec<RawMessage, MAX_PACK>;

/// Large enough for a full Message Pack decoded to JSON (9 Location-sized
/// messages are ~4.2 KB) plus their raw hex (~0.5 KB). Lives in the drain task's state, not on a stack.
pub const JSON_BYTES: usize = 6 * 1024;

/// Hub frames waiting for the UART. Kept short: if the hub link stalls we would
/// rather lose stats than block the drain task.
pub(crate) static HUB_QUEUE: Channel<CriticalSectionRawMutex, Vec<u8, { link::MAX_FRAME }>, 8> = Channel::new();
/// Hub frames that could not be queued or encoded.
pub static HUB_DROPPED: AtomicU32 = AtomicU32::new(0);

/// Write a serialized object as one line. Serialization failures are reported
/// as their own line rather than silently skipped.
fn emit<T: Serialize>(value: &T, buf: &mut [u8], what: &'static str) {
    match serde_json_core::to_slice(value, buf) {
        // serde-json-core only writes UTF-8.
        Ok(n) => esp_println::println!("{}", core::str::from_utf8(&buf[..n]).unwrap_or("")),
        Err(_) => esp_println::println!(r#"{{"t":"err","what":"json","of":"{what}"}}"#),
    }
}

// ------------------------------------------------------------------- hello --

#[derive(Serialize)]
struct HelloLine {
    t: &'static str,
    fw: &'static str,
    ver: &'static str,
    chip: &'static str,
    role: link::NodeRole,
    proto: u8,
    fcs: usize,
    plan: &'static [Hop],
}

pub fn hello_json() {
    let line = HelloLine {
        t: "hello",
        fw: env!("CARGO_PKG_NAME"),
        ver: env!("CARGO_PKG_VERSION"),
        chip: chip::CHIP,
        role: chip::ROLE,
        proto: link::PROTOCOL_VERSION,
        fcs: chip::FCS_LEN,
        plan: chip::PLAN,
    };
    emit(&line, &mut [0u8; 512], "hello");
}

pub fn hello_hub() {
    let mut firmware = heapless::String::<16>::new();
    // "node-wifi 0.1.0" fits in 16; truncation would only shorten the label.
    let _ = write!(firmware, "node-wifi {}", env!("CARGO_PKG_VERSION"));
    to_hub(&link::Frame::Hello { role: chip::ROLE, protocol: link::PROTOCOL_VERSION, firmware });
}

// ------------------------------------------------------------------- stats --

#[derive(Serialize)]
pub struct StatsLine {
    pub t: &'static str,
    pub up_us: u64,
    /// Channel the hopper is on right now.
    pub ch: u8,
    /// Management frames seen, total and in the last second.
    pub seen: u32,
    pub fps: u32,
    /// Data frames looked at, total (0 with `chip::OPEN_LINKS` off).
    pub data: u32,
    /// Data frames recognised as an open FPV / telemetry link, total.
    pub open: u32,
    /// Open-link streams replaced in the node's table to make room, total.
    pub open_ev: u32,
    /// Frames that carried Remote ID, total.
    pub rid: u32,
    /// Frames lost to a full queue, total.
    pub drop: u32,
    /// Most queue slots in use during the last second (of `rx::SLOTS`).
    pub qmax: u32,
    /// Non-management callbacks (filter leaks).
    pub other: u32,
    /// Frames longer than a slot (kept truncated).
    pub trunc: u32,
    /// Failed channel changes, total.
    pub ch_err: u32,
    /// Hub frames lost (queue full / encode error), total.
    pub hub_drop: u32,
    /// Beacon element walks: [end at sig_len-4, end at sig_len, neither or both].
    /// The first should dominate; if the second does, `chip::FCS_LEN` is wrong.
    pub iefit: [u32; 3],
    /// `[channel, frames]` per distinct channel in the plan, total.
    pub per_ch: Vec<(u8, u32), 16>,
}

pub fn stats_json(line: &StatsLine) {
    emit(line, &mut [0u8; 512], "stats");
}

pub fn stats_hub(line: &StatsLine) {
    to_hub(&link::Frame::Stats {
        uptime_us: line.up_us,
        frames_seen: line.seen,
        rid_found: line.rid,
        dropped: line.drop,
        // Beacons and NAN are told apart only after decoding, so no split here.
        by_transport: heapless::Vec::new(),
    });
}

// --------------------------------------------------------------- detection --

/// One Remote ID frame, as found by `rid::wifi::extract`.
pub struct Hit<'a> {
    pub transport: Transport,
    pub channel: u8,
    pub rssi: i8,
    pub mac: [u8; 6],
    pub counter: u8,
    pub up_us: u64,
    pub messages: &'a Messages,
    /// Set when the payload could not be split into messages.
    pub error: Option<rid::Error>,
}

#[derive(Serialize)]
struct RidLine<'a> {
    t: &'static str,
    tr: Transport,
    ch: u8,
    rssi: i8,
    mac: Mac,
    ctr: u8,
    up_us: u64,
    msgs: Decoded<'a>,
    raw: RawList<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    err: Option<rid::Error>,
}

pub fn detection_json(hit: &Hit<'_>, buf: &mut [u8]) {
    let line = RidLine {
        t: "rid",
        tr: hit.transport,
        ch: hit.channel,
        rssi: hit.rssi,
        mac: Mac(hit.mac),
        ctr: hit.counter,
        up_us: hit.up_us,
        msgs: Decoded(hit.messages),
        raw: RawList(hit.messages),
        err: hit.error,
    };
    emit(&line, buf, "rid");
}

pub fn detection_hub(hit: &Hit<'_>) {
    if hit.messages.is_empty() {
        return;
    }
    to_hub(&link::Frame::Detection(link::Detection {
        transport: hit.transport,
        channel: hit.channel,
        rssi: hit.rssi,
        mac: hit.mac,
        counter: hit.counter,
        uptime_us: hit.up_us,
        messages: hit.messages.clone(),
    }));
}

/// `aa:bb:cc:dd:ee:ff`
struct Mac([u8; 6]);

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

impl Serialize for Mac {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// Lowercase hex of a raw message, for ones the decoder rejects.
struct Hex<'a>(&'a [u8]);

impl Serialize for Hex<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        struct D<'a>(&'a [u8]);
        impl fmt::Display for D<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
        }
        s.collect_str(&D(self.0))
    }
}

/// The raw messages as hex, so a reader can re-decode exactly what was heard
/// rather than trusting a round trip through the JSON form.
struct RawList<'a>(&'a [RawMessage]);

impl Serialize for RawList<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|m| Hex(m)))
    }
}

/// The message list, each decoded with `rid` where possible, else its hex.
struct Decoded<'a>(&'a Messages);

impl Serialize for Decoded<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for raw in self.0 {
            match rid::message::decode(raw) {
                Ok(msg) => seq.serialize_element(&msg)?,
                Err(_) => seq.serialize_element(&Hex(raw))?,
            }
        }
        seq.end()
    }
}

// -------------------------------------------------------------- open links --

/// `{"t":"open",...}`: one stream of an open FPV link, at most once a second.
/// `id` is the link's cleartext identifier (wfb-ng link ID, RubyFPV vehicle
/// ID, DroneBridge comm ID) or null. The node is a raw sensor and prints it;
/// the hub and rid-view hide it unless their operator opts in.
#[derive(Serialize)]
struct OpenLine {
    t: &'static str,
    sys: openlink::System,
    id: Option<u32>,
    port: u8,
    dir: openlink::Direction,
    ch: u8,
    rssi: i8,
    mac: Mac,
    up_us: u64,
    /// Frames counted on this stream since the node started tracking it.
    n: u32,
}

pub fn open_json(s: &openlink::Sighting) {
    let line = OpenLine {
        t: "open",
        sys: s.link.system,
        id: s.link.id,
        port: s.link.port,
        dir: s.link.direction,
        ch: s.channel,
        rssi: s.rssi,
        mac: Mac(s.link.tx),
        up_us: s.last_us,
        n: s.packets,
    };
    emit(&line, &mut [0u8; 256], "open");
}

pub fn open_hub(s: &openlink::Sighting) {
    to_hub(&link::Frame::OpenLink(link::OpenLinkSighting {
        link: s.link,
        channel: s.channel,
        rssi: s.rssi,
        uptime_us: s.last_us,
        packets: s.packets,
    }));
}

// --------------------------------------------------------------------- hub --

fn to_hub(frame: &link::Frame) {
    let mut buf = [0u8; link::MAX_FRAME];
    let queued = link::encode(frame, &mut buf)
        .ok()
        .and_then(|enc| Vec::from_slice(enc).ok())
        .is_some_and(|v| HUB_QUEUE.try_send(v).is_ok());
    if !queued {
        HUB_DROPPED.fetch_add(1, Relaxed);
    }
}

/// Owns the hub UART and writes queued frames out.
#[cfg(not(feature = "espnow"))]
#[embassy_executor::task]
pub async fn hub_writer(mut uart: UartTx<'static, Async>) {
    loop {
        let frame = HUB_QUEUE.receive().await;
        let mut rest = &frame[..];
        while !rest.is_empty() {
            match uart.write_async(rest).await {
                Ok(n) => rest = &rest[n..],
                Err(_) => {
                    HUB_DROPPED.fetch_add(1, Relaxed);
                    break;
                }
            }
        }
    }
}
