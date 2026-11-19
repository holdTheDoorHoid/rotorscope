//! Both outputs: JSON lines on USB (for a laptop / DragonSync) and
//! postcard+COBS `link::Frame`s on the hub UART.
//!
//! The USB stream is machine-read, so only whole JSON objects go on it, one
//! per line, each with a `"t"` type tag. The only exception is a panic
//! backtrace, which a reader should treat as a crash marker. Lines go
//! through `usb` (an async writer; dropped, never blocking, when the host is
//! not keeping up).
//!
//! Hub frames go through [`to_hub`], which asks `wifiwatch::shed` first
//! (docs/BENCH.md "Frame flood"): the queue keeps room for Remote ID from
//! known sources and for the node's own health, each alarm kind has a
//! budget, and what is turned away is counted per kind ([`ShedLine`] in the
//! stats line; the total in the hub's `Stats::dropped`). A frame refused by
//! its budget is not printed on USB either, so a flood cannot fill that
//! stream instead.

use core::cell::RefCell;
use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
use esp_hal::Async;
#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
use esp_hal::uart::{UartRx, UartTx};
use heapless::Vec;
use rid::{MAX_PACK, RawMessage, Transport};
use serde::ser::{SerializeSeq, Serializer};
use serde::Serialize;
use wifiwatch::shed::{Kind, Shed};

use crate::chip;
use crate::role::{self, Plan};
use crate::usb::{self, usb_line};

pub type Messages = Vec<RawMessage, MAX_PACK>;

/// Hub frames waiting for the link (UART, wired bus or ESP-NOW batches).
/// Kept short: if the hub link stalls we would rather lose frames than block
/// the drain task. Who may use which slot: `wifiwatch::shed::Kind::room`.
pub const HUB_SLOTS: usize = 8;
pub(crate) static HUB_QUEUE: Channel<CriticalSectionRawMutex, Vec<u8, { link::MAX_FRAME }>, HUB_SLOTS> = Channel::new();
/// Hub frames lost after admission: encode errors, UART write errors.
/// (Frames turned away by [`SHED`] are counted there, per kind.)
pub static HUB_DROPPED: AtomicU32 = AtomicU32::new(0);

/// Hub-queue admission (~900 bytes). The salt is set at boot ([`init_shed`]).
static SHED: critical_section::Mutex<RefCell<Shed>> = critical_section::Mutex::new(RefCell::new(Shed::new(0)));

/// Give the Remote ID source table its random salt. Call once, with the
/// radio on (the hardware RNG is then truly random).
pub fn init_shed(salt: u32) {
    critical_section::with(|cs| *SHED.borrow_ref_mut(cs) = Shed::new(salt));
}

fn now_ms() -> wifiwatch::util::Ms {
    wifiwatch::util::ms(embassy_time::Instant::now().as_micros())
}

/// Budget check before reporting a frame of `kind` (both outputs). A
/// refusal is counted.
pub fn admit(kind: Kind) -> bool {
    critical_section::with(|cs| SHED.borrow_ref_mut(cs).admit(kind, now_ms()))
}

/// A Remote ID frame from `mac`: how to queue it, or `None` (counted) when
/// it is shed: a new source over the new-source budget, or a known one over
/// its own rate.
pub fn rid_admit(mac: [u8; 6]) -> Option<Kind> {
    critical_section::with(|cs| SHED.borrow_ref_mut(cs).rid(mac, now_ms()))
}

/// Hub frames turned away, per kind, totals (the stats line's `shed`).
#[derive(Serialize, Clone, Copy, Default)]
pub struct ShedLine {
    pub rid_known: u32,
    pub rid_new: u32,
    pub health: u32,
    pub threat: u32,
    pub wdrone: u32,
    pub dji: u32,
    pub open: u32,
    pub other: u32,
}

/// The shed counts, Remote ID sources known now, and the total.
pub fn shed_counts() -> (ShedLine, u32, u32) {
    critical_section::with(|cs| {
        let s = SHED.borrow_ref(cs);
        let d = |k: Kind| s.dropped[k as usize];
        let line = ShedLine {
            rid_known: d(Kind::RidKnown),
            rid_new: d(Kind::RidNew),
            health: d(Kind::Health),
            threat: d(Kind::Threat),
            wdrone: d(Kind::WifiDrone),
            dji: d(Kind::DjiWifi),
            open: d(Kind::OpenLink),
            other: d(Kind::Other),
        };
        (line, s.rid.known(now_ms()) as u32, s.total())
    })
}

/// Free hub-queue slots a frame of `kind` could use now (for a task about
/// to send several, e.g. `open_links`).
pub fn hub_room(kind: Kind) -> usize {
    kind.room(HUB_SLOTS).saturating_sub(HUB_QUEUE.len())
}

/// Write a serialized object as one line. Serialization failures are reported
/// as their own line rather than silently skipped.
fn emit<T: Serialize>(value: &T, buf: &mut [u8], what: &'static str) {
    match serde_json_core::to_slice(value, buf) {
        Ok(n) => {
            usb::line(&buf[..n]);
        }
        Err(_) => usb_line!(r#"{{"t":"err","what":"json","of":"{what}"}}"#),
    }
}

// ------------------------------------------------------------------- hello --

#[derive(Serialize)]
struct HelloLine<'a> {
    t: &'static str,
    fw: &'static str,
    ver: &'static str,
    chip: &'static str,
    role: link::NodeRole,
    proto: u8,
    fcs: usize,
    /// Roles this board can take (mask of `NodeRole::bit`), and the image in
    /// each app slot (`null` for an empty or unknown slot).
    can: u8,
    images: &'a [Option<&'static str>],
    /// Plan id applied (0 = built-in) and the plan.
    plan_id: u16,
    plan: &'a Plan,
    /// Why the chip last reset (`node_common::fault::name`), and whether
    /// this is a `faulttest` bench build.
    reset: &'static str,
    faulttest: bool,
    /// Bench features and build switches compiled in (`[]`: a normal build;
    /// build.rs, docs/BENCH.md "Bench builds").
    bench: heapless::Vec<&'static str, 8>,
}

pub fn hello_json(images: &[Option<&'static str>]) {
    let job = role::current();
    let line = HelloLine {
        t: "hello",
        fw: env!("CARGO_PKG_NAME"),
        ver: env!("CARGO_PKG_VERSION"),
        chip: chip::CHIP,
        role: job.role,
        proto: link::PROTOCOL_VERSION,
        fcs: chip::FCS_LEN,
        can: role::CAN.load(Relaxed),
        images,
        plan_id: job.id,
        plan: &job.plan,
        reset: node_common::fault::reset_reason(),
        faulttest: cfg!(feature = "faulttest"),
        bench: node_common::bench_list(env!("ROTORSCOPE_BENCH_LIST")),
    };
    emit(&line, &mut [0u8; 768], "hello");
    // Bench features compiled in (build.rs): say so, with the tag
    // tools/flash-node.sh looks for in the ELF.
    if !env!("ROTORSCOPE_BENCH_LIST").is_empty() {
        usb_line!(
            r#"{{"t":"warn","what":"bench_build","tag":"{}","note":"BENCH BUILD: never leave it on a fielded unit"}}"#,
            env!("ROTORSCOPE_BENCH_TAG")
        );
    }
}

/// Hello, then Status (protocol 4: the hub learns `can` and the plan id).
pub fn hello_hub() {
    let mut firmware = heapless::String::<16>::new();
    // "node-wifi 0.1.0" fits in 16; truncation would only shorten the label.
    let _ = write!(firmware, "node-wifi {}", env!("CARGO_PKG_VERSION"));
    to_hub(&link::Frame::Hello { role: role::current().role, protocol: link::PROTOCOL_VERSION, firmware });
    status_hub();
}

pub fn status_hub() {
    let job = role::current();
    to_hub(&link::Frame::Status { role: job.role, can: role::CAN.load(Relaxed), plan: job.id });
}

#[derive(Serialize)]
struct AssignLine<'a> {
    t: &'static str,
    role: link::NodeRole,
    plan_id: u16,
    plan: &'a Plan,
}

/// A new job from the hub, applied.
pub fn assign_json(job: &role::Job) {
    emit(&AssignLine { t: "assign", role: job.role, plan_id: job.id, plan: &job.plan }, &mut [0u8; 512], "assign");
}

/// About to switch image for `a`.
pub fn switch_json(a: &link::Assignment) {
    usb_line!(r#"{{"t":"switch","role":"{:?}","plan_id":{},"hops":{}}}"#, a.role, a.id, a.plan.len());
}

// ------------------------------------------------------------------- stats --

#[derive(Serialize)]
pub struct StatsLine {
    pub t: &'static str,
    pub up_us: u64,
    /// Role and plan id being run.
    pub role: link::NodeRole,
    pub plan_id: u16,
    /// Hub commands: applied (changed the job), repeats, rejected / switch failed.
    pub cmds: [u32; 3],
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
    /// Hub frames not sent, total: everything in `shed`, plus encode and
    /// UART write errors.
    pub hub_drop: u32,
    /// Hub frames turned away by kind (`wifiwatch::shed`): over the kind's
    /// budget, over a known Remote ID source's rate, or no room left for
    /// that kind in the queue. Totals. A frame shed by its budget is not
    /// printed on USB either.
    pub shed: ShedLine,
    /// Remote ID sources known now (heard twice; their frames get the
    /// queue's reserved room).
    pub rid_src: u32,
    /// USB lines dropped (host slow or absent; Remote ID line still going
    /// out), total.
    pub usb_drop: u32,
    /// Report alternations (one address going back to an earlier content
    /// after a different one: two transmitters on one address), totals,
    /// [DJI DroneID, Wi-Fi drone] (`wifiwatch::contentlimit`).
    pub alt: [u32; 2],
    /// Received frames whose `sig_len` claimed more than one receive buffer
    /// holds (`rx::RX_MAX`), clamped before reading, total.
    pub clamp: u32,
    /// Beacon element walks: [end at sig_len-4, end at sig_len, neither or both].
    /// The first should dominate; if the second does, `chip::FCS_LEN` is wrong.
    pub iefit: [u32; 3],
    /// `[channel, frames, visits]` per distinct channel in the current plan:
    /// management frames heard there and times the hopper tuned to it, totals
    /// since boot.
    pub per_ch: Vec<(u8, u32, u32), 24>,
    /// Failed band changes (C5), total.
    pub band_err: u32,
    /// DJI DroneID elements / Wi-Fi drone networks reported (rate-limited), total.
    pub dji: u32,
    pub wdrone: u32,
    /// Attack detection totals (`wifiwatch`): deauth + disassoc frames,
    /// authentication / association requests, probe requests, channel-switch
    /// frames, threats reported.
    pub kill: u32,
    pub auth: u32,
    pub probe: u32,
    pub csa: u32,
    pub threat: u32,
    /// Scouting visits off the plan (`coverage::plans::scout_channel`) and the
    /// last channel scouted.
    pub scout: (u32, u8),
    /// MAVLink over open Wi-Fi (protocol 13): [opt-in on (1/0), datagrams
    /// past the filter, turned away by the rate limit, turned away with
    /// every slot in use, analysed, findings], totals.
    pub mav: [u32; 6],
    /// Hostile-MAVLink pressure (`mavwatch::Pressure`, WP-11a): [new alarms
    /// held back by the budget, sources / parameter sets holding evidence
    /// given up, newcomers turned away by a full table, streams turned
    /// away], totals.
    pub mavp: [u32; 4],
    /// Drone networks (`wifiwatch::drones::Totals`): [learned, displaced
    /// while holding evidence, newcomers that had to wait], totals.
    pub dnet: [u32; 3],
    /// Node robustness (`node_common::health`, WP-13 section 7): [plans
    /// refused, deaf fall-backs, image switches refused, seconds until a
    /// switch is allowed]. Totals since boot, then the wait now.
    pub robust: [u32; 4],
}

pub fn stats_json(line: &StatsLine) {
    emit(line, &mut [0u8; 1536], "stats");
}

pub fn stats_hub(line: &StatsLine) {
    to_hub(&link::Frame::Stats {
        uptime_us: line.up_us,
        frames_seen: line.seen,
        rid_found: line.rid,
        // Everything lost: receive slots full, plus hub frames not sent
        // (shed by kind, or failed). Per kind only on USB (`shed`): the
        // Stats frame has no field for it.
        dropped: line.drop.wrapping_add(line.hub_drop),
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

pub fn detection_json(hit: &Hit<'_>) {
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
    // Formatted straight into the USB writer's buffer.
    if !usb::rid_line(|buf| serde_json_core::to_slice(&line, buf).ok()) {
        usb_line!(r#"{{"t":"err","what":"json","of":"rid"}}"#);
    }
}

/// `kind`: from [`rid_admit`]. True if queued.
pub fn detection_hub(hit: &Hit<'_>, kind: Kind) -> bool {
    if hit.messages.is_empty() {
        return false;
    }
    to_hub_as(kind, &link::Frame::Detection(link::Detection {
        transport: hit.transport,
        channel: hit.channel,
        rssi: hit.rssi,
        mac: hit.mac,
        counter: hit.counter,
        uptime_us: hit.up_us,
        messages: hit.messages.clone(),
    }))
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

// ------------------------------------------------------- DJI / Wi-Fi drones --

/// `{"t":"dji",...}`: a DJI DroneID element was heard. Only its subcommand
/// and length: the node never decodes it (no serials on the USB line).
/// `alt`: this address has alternated between element contents within the
/// last minute (`wifiwatch::contentlimit`): two transmitters, one a spoof.
pub fn dji_json(f: &crate::rx::RxFrame, src: [u8; 6], body: &[u8], alt: bool) {
    usb_line!(
        r#"{{"t":"dji","ch":{},"rssi":{},"mac":"{}","sub":{},"len":{},"up_us":{},"alt":{alt}}}"#,
        f.channel,
        f.rssi,
        Mac(src),
        body.get(6).copied().unwrap_or(0),
        body.len(),
        f.up_us
    );
}

pub fn dji_hub(f: &crate::rx::RxFrame, src: [u8; 6], body: &[u8]) {
    let Ok(element) = Vec::from_slice(&body[..body.len().min(255)]) else { return };
    to_hub(&link::Frame::DjiWifi { channel: f.channel, rssi: f.rssi, mac: src, uptime_us: f.up_us, element });
}

/// The network name, printable ASCII or nothing (it goes in a JSON string).
fn ssid_str(ssid: &[u8]) -> heapless::String<32> {
    let mut s = heapless::String::new();
    for &b in ssid.iter().take(32) {
        let _ = s.push(if (0x20..0x7F).contains(&b) && b != b'"' && b != b'\\' { b as char } else { '?' });
    }
    s
}

/// `alt`: as for [`dji_json`] (here the content is the network name).
pub fn wifi_drone_json(f: &crate::rx::RxFrame, maker: u8, src: [u8; 6], ssid: &[u8], alt: bool) {
    usb_line!(
        r#"{{"t":"wifidrone","maker":{maker},"ch":{},"rssi":{},"mac":"{}","ssid":"{}","up_us":{},"alt":{alt}}}"#,
        f.channel,
        f.rssi,
        Mac(src),
        ssid_str(ssid),
        f.up_us
    );
}

pub fn wifi_drone_hub(f: &crate::rx::RxFrame, maker: u8, src: [u8; 6], ssid: &[u8]) {
    // Raw bytes may not be UTF-8; the hub only shows it.
    let ssid = ssid_str(ssid);
    to_hub(&link::Frame::WifiDrone(link::WifiDroneSighting {
        maker,
        mac: src,
        ssid,
        channel: f.channel,
        rssi: f.rssi,
        uptime_us: f.up_us,
    }));
}

// ----------------------------------------------------------------- threats --

/// `{"t":"threat",...}`: an attack seen on the air (`wifiwatch`). Same fields
/// as `link::ThreatSighting`; `tool` names an attack tool's index.
#[derive(Serialize)]
struct ThreatLine<'a> {
    t: &'static str,
    kind: &'static str,
    ch: u8,
    rssi: i8,
    n: u32,
    win_ms: u32,
    src: Mac,
    dst: Mac,
    detail: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<&'static str>,
    ssid: &'a str,
    up_us: u64,
}

/// Report a threat on both outputs, if the threat budget allows (true if
/// it did).
pub fn threat(s: &link::ThreatSighting) -> bool {
    if !admit(Kind::Threat) {
        return false;
    }
    let tool = (s.kind == link::ThreatKind::AttackTool)
        .then(|| wifiwatch::TOOLS.get(s.detail as usize).copied())
        .flatten();
    let line = ThreatLine {
        t: "threat",
        kind: s.kind.name(),
        ch: s.channel,
        rssi: s.level,
        n: s.count,
        win_ms: s.window_ms,
        src: Mac(s.source),
        dst: Mac(s.target),
        detail: s.detail,
        tool,
        ssid: &s.ssid,
        up_us: s.uptime_us,
    };
    emit(&line, &mut [0u8; 384], "threat");
    to_hub(&link::Frame::Threat(s.clone()));
    true
}

// --------------------------------------------------------------------- hub --

/// Protocol 13: the MAVLink frame behind a `MavlinkAttack` threat sent
/// just before, for the hub's evidence log; and a line on USB.
pub fn mavlink_evidence(e: &link::MavlinkEvidence) {
    usb_line!(
        r#"{{"t":"mavlink_attack","kind":{},"sys":{},"comp":{},"msg":{},"bytes":{}}}"#,
        e.detail,
        e.sysid,
        e.compid,
        e.msgid,
        e.frame.len()
    );
    to_hub(&link::Frame::MavlinkEvidence(e.clone()));
}

/// Queue a frame for the hub if there is room for its kind
/// (`wifiwatch::shed::Kind::of`). True if queued.
pub(crate) fn to_hub(frame: &link::Frame) -> bool {
    to_hub_as(Kind::of(frame), frame)
}

/// [`to_hub`] with the kind given (Remote ID: known or new source).
pub(crate) fn to_hub_as(kind: Kind, frame: &link::Frame) -> bool {
    let mut buf = [0u8; link::MAX_FRAME];
    let Some(v) = link::encode(frame, &mut buf).ok().and_then(|enc| Vec::from_slice(enc).ok()) else {
        HUB_DROPPED.fetch_add(1, Relaxed);
        return false;
    };
    // One critical section: the room check and the send see the same queue
    // (the wired bus drains it from an interrupt executor).
    let queued = critical_section::with(|cs| {
        if !SHED.borrow_ref_mut(cs).room(kind, HUB_QUEUE.len(), HUB_SLOTS) {
            return None;
        }
        Some(HUB_QUEUE.try_send(v).is_ok())
    });
    match queued {
        Some(true) => true,
        // Counted by `room`.
        None => false,
        // Cannot happen (room <= HUB_SLOTS); counted if it does.
        Some(false) => {
            HUB_DROPPED.fetch_add(1, Relaxed);
            false
        }
    }
}

/// A wired node's hub commands, on the hub UART's RX pin (`chip::hub_rx_pin!`).
#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
#[embassy_executor::task]
pub async fn hub_reader(mut uart: UartRx<'static, Async>) {
    let mut dec = link::CommandDecoder::new();
    let mut buf = [0u8; 128];
    loop {
        let Ok(n) = uart.read_async(&mut buf).await else { continue };
        dec.feed(&buf[..n], |r| {
            if let Ok(link::Command::Assign(a)) = r {
                // Full: the older command is still queued; the hub resends.
                let _ = role::COMMANDS.try_send(a);
            }
        });
    }
}

/// Owns the hub UART and writes queued frames out.
#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
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
