//! The wired bus on UART3 (PLAN.md Phase 5a, docs/WIREBUS.md): many nodes
//! on the UART3-IN header, polled in turn by `wirebus::Master`.
//!
//! Wiring: hub TX (GPIO27) to every node's RX (XIAO D7); every node's TX
//! (XIAO D9, open-drain, through a diode) onto one return wire into hub RX
//! (GPIO28), with one 470 ohm pull-up to 3.3 V at the hub.
//!
//! **Where it runs.** The UART3 interrupt (`links.rs`) hands every received
//! byte to [`rx`] at once, and the UART's TX-done interrupt calls
//! [`service`] too, so the next poll follows an answer within one interrupt
//! latency rather than whenever the main loop (which stalls ~7 ms per screen
//! band) gets round to it. The main loop still calls [`service`] every pass
//! for timeouts, the 20 ms cycle and join slots.
//!
//! **Same frames as a gateway.** Each checked answer's frames are written to
//! the link's receive ring with a `Frame::Via { node: <its MAC>, rssi: 0 }`
//! in front (`wirebus::Attributor`), exactly what an ESP-NOW gateway writes.
//! `GatewayLink::poll` therefore decodes the bus like a gateway, and the node
//! table, tracker, evidence log, coverage coordinator and web-head feed see
//! each wired node under its own MAC with no change.
//!
//! **Commands.** `GatewayLink::send` (the coverage coordinator's `To` +
//! `Assign`) queues bytes here; they go out only between answers and join
//! windows (a node that sees a newer command before it answers stays silent,
//! by design), as whole frames, before the next poll.
//!
//! **Old gateways.** A device that streams without being polled (an ESP-NOW
//! gateway, or one point-to-point node) is recognised
//! (`Master::looks_legacy`) and the link drops back to plain reading: no
//! polls, bytes straight to the ring, as before this module. After 10 s of
//! silence it tries the bus again. Build without the `wirebus` feature to
//! never poll at all.

use core::cell::RefCell;

use critical_section::{CriticalSection, Mutex};
use esp_hal::Blocking;
use esp_hal::uart::Uart;
use heapless::Deque;
use wirebus::{Attributor, Event, Master, MasterConfig};

/// Nodes on the bus (the hub's relayed-node table holds 16 in all).
pub const MAX_BUS: usize = 16;
/// Poll the bus by default (feature `wirebus`, on in the default build).
pub const BUS_DEFAULT: bool = cfg!(feature = "wirebus");
/// Commands waiting for the line.
const TX_QUEUE: usize = 4096;
/// Bytes from the master (polls, join slots, grants) not yet in the FIFO.
const URGENT: usize = 256;
/// Legacy mode gives the bus another try after this much silence.
const LEGACY_RETRY_US: u64 = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bus,
    Legacy,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Bus => "bus",
            Mode::Legacy => "legacy",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Note {
    Joined([u8; 6], u8, bool),
    Absent([u8; 6], u8),
    Back([u8; 6], u8),
    Mode(Mode),
}

pub struct Shared {
    pub master: Master<MAX_BUS>,
    attr: Attributor,
    pub mode: Mode,
    mode_since_us: u64,
    /// Commands from the main loop: whole COBS frames.
    tx: Deque<u8, TX_QUEUE>,
    /// The master's own bytes, sent before any command.
    urgent: Deque<u8, URGENT>,
    /// A command frame is half in the FIFO: nothing else may go until its end.
    mid_frame: bool,
    /// When the TX FIFO will have drained (estimate), microseconds.
    tx_free_at: u64,
    last_rx_us: u64,
    pub notes: Deque<Note, 16>,
    /// Command bytes written to the wire, and refused for want of room.
    pub cmd_bytes: u32,
    pub cmd_full: u32,
    /// Bus frames passed to the ring, and lost because it was full.
    pub frames: u32,
    pub ring_full: u32,
    started: bool,
    /// A `TimeSync` to send when the command queue is next empty: UTC (ms)
    /// at hub time `at_us`, and whether that time is PPS-anchored. Encoded
    /// only then, so its `utc_ms` is stamped as the bytes leave.
    sync: Option<(u64, u64, bool)>,
    /// `TimeSync` commands written to the wire.
    pub syncs: u32,
}

/// The state lives in `.bss` (`STORE`, ~8 KB); `BUS` points at it once set up.
pub static BUS: Mutex<RefCell<Option<&'static mut Shared>>> = Mutex::new(RefCell::new(None));
static mut STORE: core::mem::MaybeUninit<Shared> = core::mem::MaybeUninit::uninit();

pub fn now_us() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_micros()
}

/// Set up the bus state (before the UART interrupt is enabled). Once.
pub fn init(epoch: u16) {
    critical_section::with(|cs| {
        assert!(BUS.borrow_ref(cs).is_none(), "bus::init twice");
        // SAFETY: written once, here, before anything can reach it through BUS.
        let store = unsafe { &mut *(&raw mut STORE) };
        let shared = store.write(Shared {
            master: Master::new(MasterConfig::new(), epoch),
            attr: Attributor::new(),
            mode: if BUS_DEFAULT { Mode::Bus } else { Mode::Legacy },
            mode_since_us: 0,
            tx: Deque::new(),
            urgent: Deque::new(),
            mid_frame: false,
            tx_free_at: 0,
            last_rx_us: 0,
            notes: Deque::new(),
            cmd_bytes: 0,
            cmd_full: 0,
            frames: 0,
            ring_full: 0,
            started: false,
            sync: None,
            syncs: 0,
        });
        BUS.borrow_ref_mut(cs).replace(shared);
    });
}

/// Queue command bytes (whole frames). All or nothing.
pub fn send(cs: CriticalSection<'_>, bytes: &[u8]) -> bool {
    let mut b = BUS.borrow_ref_mut(cs);
    let Some(s) = b.as_mut() else { return false };
    if s.tx.capacity() - s.tx.len() < bytes.len() {
        s.cmd_full += bytes.len() as u32;
        return false;
    }
    for &x in bytes {
        let _ = s.tx.push_back(x);
    }
    true
}

/// Protocol 11: send `Command::TimeSync` (a broadcast: every bus node, or
/// the ESP-NOW gateway, which forwards it to aero nodes) once the command
/// queue is empty. `utc_ms` is UTC at hub time `at_us`; the time actually
/// sent is advanced to the moment the frame is written. A newer call
/// replaces one not yet sent.
pub fn time_sync(utc_ms: u64, at_us: u64, pps: bool) {
    critical_section::with(|cs| {
        if let Some(s) = BUS.borrow_ref_mut(cs).as_mut() {
            s.sync = Some((utc_ms, at_us, pps));
        }
    });
}

/// `TimeSync` commands sent since boot.
pub fn syncs() -> u32 {
    critical_section::with(|cs| BUS.borrow_ref(cs).as_ref().map_or(0, |s| s.syncs))
}

/// Is this MAC a node on the bus (not a radio relay)?
pub fn on_bus(mac: &[u8; 6]) -> bool {
    critical_section::with(|cs| BUS.borrow_ref(cs).as_ref().is_some_and(|s| s.mode == Mode::Bus && s.master.entry(mac).is_some()))
}

/// Bytes from UART3 RX (called from the interrupt). Legacy: straight into
/// the ring. Bus: through the master; checked frames into the ring with
/// their node's `Via`.
pub fn rx<const N: usize>(cs: CriticalSection<'_>, uart: &mut Uart<'static, Blocking>, bytes: &[u8], ring: &mut Deque<u8, N>) -> u32 {
    let now = now_us();
    let mut b = BUS.borrow_ref_mut(cs);
    let Some(s) = b.as_mut() else { return push_all(ring, bytes) };
    s.last_rx_us = now;
    if s.mode == Mode::Legacy {
        return push_all(ring, bytes);
    }
    let lost = {
        let Shared { master, attr, notes, frames, .. } = s;
        let mut lost = 0;
        master.on_rx(now, bytes, &mut |e| on_event(e, attr, notes, frames, ring, &mut lost));
        lost
    };
    s.ring_full += lost;
    service_locked(s, uart, ring, now, false);
    lost
}

fn push_all<const N: usize>(ring: &mut Deque<u8, N>, bytes: &[u8]) -> u32 {
    let mut lost = 0;
    for &x in bytes {
        if ring.push_back(x).is_err() {
            lost += 1;
        }
    }
    lost
}

fn on_event<const N: usize>(e: Event<'_>, attr: &mut Attributor, notes: &mut Deque<Note, 16>, frames: &mut u32, ring: &mut Deque<u8, N>, lost: &mut u32) {
    match e {
        Event::Frame { mac, bytes, .. } => attr.frame(mac, bytes, &mut |chunk| {
            // Whole or nothing: half a frame would garble the next one.
            if ring.capacity() - ring.len() < chunk.len() {
                *lost += chunk.len() as u32;
                return;
            }
            *frames += 1;
            for &x in chunk {
                let _ = ring.push_back(x);
            }
        }),
        Event::AnswerEnd { .. } => attr.end_answer(),
        Event::Joined { mac, addr, again } => {
            let _ = notes.push_back(Note::Joined(mac, addr, again));
        }
        Event::Absent { mac, addr } => {
            let _ = notes.push_back(Note::Absent(mac, addr));
        }
        Event::Back { mac, addr } => {
            let _ = notes.push_back(Note::Back(mac, addr));
        }
    }
}

/// Timeouts, the next poll, and the TX FIFO. Called from the UART interrupt
/// (TX done, RX) and from the main loop (`main_loop` true: mode changes too).
pub fn service<const N: usize>(cs: CriticalSection<'_>, uart: &mut Uart<'static, Blocking>, ring: &mut Deque<u8, N>, main_loop: bool) {
    let now = now_us();
    let mut b = BUS.borrow_ref_mut(cs);
    if let Some(s) = b.as_mut() {
        service_locked(s, uart, ring, now, main_loop);
    }
}

fn service_locked<const N: usize>(s: &mut Shared, uart: &mut Uart<'static, Blocking>, ring: &mut Deque<u8, N>, now: u64, main_loop: bool) {
    if !s.started {
        s.started = true;
        s.mode_since_us = now;
    }
    if main_loop {
        match s.mode {
            Mode::Bus if s.master.looks_legacy(now) => {
                s.mode = Mode::Legacy;
                s.mode_since_us = now;
                let _ = s.notes.push_back(Note::Mode(Mode::Legacy));
            }
            Mode::Legacy
                if BUS_DEFAULT
                    && now.saturating_sub(s.last_rx_us) > LEGACY_RETRY_US
                    && now.saturating_sub(s.mode_since_us) > LEGACY_RETRY_US =>
            {
                s.mode = Mode::Bus;
                s.mode_since_us = now;
                let _ = s.notes.push_back(Note::Mode(Mode::Bus));
            }
            _ => {}
        }
    }
    // The master's bytes first, then commands (only while no node has the
    // line), then the master's next move.
    pump_urgent(s, uart, now);
    if s.mode == Mode::Legacy || !s.master.busy() {
        pump_commands(s, uart, now);
    }
    if s.mode == Mode::Legacy || !s.urgent.is_empty() {
        return;
    }
    // A poll may only follow whole command frames; with commands still
    // queued, they go first (the TX-done interrupt brings us back).
    if !s.master.busy() && (s.mid_frame || !s.tx.is_empty()) {
        return;
    }
    let busy = s.tx_free_at.saturating_sub(now);
    let Shared { master, attr, notes, frames, urgent, .. } = s;
    let mut lost = 0;
    let out = master.tick(now, busy, &mut |e| on_event(e, attr, notes, frames, ring, &mut lost));
    if let Some(bytes) = out {
        for &x in bytes {
            let _ = urgent.push_back(x);
        }
    }
    s.ring_full += lost;
    pump_urgent(s, uart, now);
}

/// Write as much of `q` as the FIFO takes now; returns bytes written.
fn write_some<const Q: usize>(q: &mut Deque<u8, Q>, uart: &mut Uart<'static, Blocking>, limit: usize) -> usize {
    let mut n = 0;
    while n < limit && !q.is_empty() && uart.write_ready() {
        let (front, _) = q.as_slices();
        let take = front.len().min(limit - n).min(16);
        // `write` waits only when the FIFO is full; `write_ready` says it is not.
        let Ok(w) = uart.write(&front[..take]) else { break };
        for _ in 0..w {
            q.pop_front();
        }
        n += w;
        if w < take {
            break;
        }
    }
    n
}

fn account(s: &mut Shared, now: u64, n: usize) {
    if n > 0 {
        s.tx_free_at = s.tx_free_at.max(now) + wirebus::wire_us(n, wirebus::BAUD);
    }
}

fn pump_urgent(s: &mut Shared, uart: &mut Uart<'static, Blocking>, now: u64) {
    let n = write_some(&mut s.urgent, uart, URGENT);
    account(s, now, n);
}

/// Commands, whole frames: once a frame is started it is finished before
/// anything else goes.
fn pump_commands(s: &mut Shared, uart: &mut Uart<'static, Blocking>, now: u64) {
    // A pending TimeSync goes only between whole commands (never between
    // a `To` and its `Assign`) and is stamped now, just before it is
    // written. Its ~0.15 ms on the wire is below the ms resolution.
    if s.tx.is_empty()
        && !s.mid_frame
        && uart.write_ready()
        && let Some((utc_ms, at_us, pps)) = s.sync.take()
    {
        let utc_ms = utc_ms + now.saturating_sub(at_us) / 1000;
        let mut buf = [0u8; 24];
        if let Ok(b) = link::encode_command(&link::Command::TimeSync { utc_ms, pps }, &mut buf) {
            for &x in b.iter() {
                let _ = s.tx.push_back(x);
            }
            s.syncs += 1;
        }
    }
    if s.tx.is_empty() {
        return;
    }
    let mut n = 0;
    while !s.tx.is_empty() && uart.write_ready() {
        // Up to the end of the frame in progress (its zero delimiter).
        let (front, _) = s.tx.as_slices();
        let end = front.iter().position(|&b| b == 0).map_or(front.len(), |i| i + 1);
        let take = end.min(16);
        let Ok(w) = uart.write(&front[..take]) else { break };
        let finished = w == end && front[end - 1] == 0;
        for _ in 0..w {
            s.tx.pop_front();
        }
        n += w;
        s.mid_frame = !finished;
        if w < take {
            break;
        }
        if finished && s.mode == Mode::Bus {
            // One frame at a time while polling: the poll must not wait long.
            break;
        }
    }
    s.cmd_bytes += n as u32;
    account(s, now, n);
}

/// Log lines for bus events (main loop).
pub fn print_notes() {
    while let Some(n) = critical_section::with(|cs| BUS.borrow_ref_mut(cs).as_mut().and_then(|s| s.notes.pop_front())) {
        match n {
            Note::Joined(mac, addr, again) => {
                crate::println!(
                    "hub-p4: bus node {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} joined as {addr}{}",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                    if again { " (again: same node, same address)" } else { "" }
                )
            }
            Note::Absent(mac, addr) => crate::println!(
                "hub-p4: bus node {addr} ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}) stopped answering",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            ),
            Note::Back(mac, addr) => crate::println!(
                "hub-p4: bus node {addr} ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}) answering again",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            ),
            Note::Mode(Mode::Legacy) => crate::println!(
                "hub-p4: UART3 carries an unpolled stream (an ESP-NOW gateway or one point-to-point node): bus polling off, reading it as before"
            ),
            Note::Mode(Mode::Bus) => crate::println!("hub-p4: UART3 quiet for 10 s: bus polling on again"),
        }
    }
}

/// The bus summary for the `hub` JSON line: `"bus":{...}` (no leading comma).
pub fn summary_json(w: &mut impl core::fmt::Write) {
    critical_section::with(|cs| {
        let b = BUS.borrow_ref(cs);
        let Some(s) = b.as_ref() else {
            let _ = write!(w, "\"bus\":null");
            return;
        };
        let m = &s.master;
        let (mut corrupt, mut timeouts) = (0u32, 0u32);
        for e in m.entries() {
            corrupt += e.stats.corrupt;
            timeouts += e.stats.timeouts;
        }
        let st = &m.stats;
        let up = now_us().max(1);
        let _ = write!(
            w,
            "\"bus\":{{\"mode\":\"{}\",\"nodes\":{},\"present\":{},\"polls\":{},\"corrupt\":{},\"timeouts\":{},\"join_slots\":{},\"grants\":{},\"join_collisions\":{},\"stray\":{},\"busy_pct\":{},\"cmd_bytes\":{},\"ring_full\":{}}}",
            s.mode.name(),
            m.entries().len(),
            m.present(),
            st.polls,
            corrupt,
            timeouts,
            st.join_slots,
            st.grants,
            st.join_corrupt,
            st.stray_bytes,
            st.busy_us * 100 / up,
            s.cmd_bytes,
            s.ring_full,
        );
    });
}

/// One `{"t":"bus"}` line with every node's counters, after the hub line.
/// Keys: `a` address, `mac`, `x` absent, `p` polls, `ok` good answers, `e`
/// empty answers, `to` silent polls, `bad` answers refused by CRC, `dup`
/// repeats dropped, `f` frames, `b` bytes, `r` mean reply us, `rmax` worst
/// reply us, `wmax` longest gap between two good answers ms, `j` joins.
pub fn nodes_json(w: &mut impl core::fmt::Write) {
    critical_section::with(|cs| {
        let b = BUS.borrow_ref(cs);
        let Some(s) = b.as_ref() else { return };
        let m = &s.master;
        let _ = write!(w, "{{\"t\":\"bus\",\"mode\":\"{}\",\"epoch\":{},\"budget\":{},\"nodes\":[", s.mode.name(), m.epoch(), m.budget());
        for (i, e) in m.entries().iter().enumerate() {
            let n = &e.stats;
            let mac = e.mac;
            let _ = write!(
                w,
                "{}{{\"a\":{},\"mac\":\"{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\",\"x\":{},\"p\":{},\"ok\":{},\"e\":{},\"to\":{},\"bad\":{},\"dup\":{},\"f\":{},\"b\":{},\"r\":{},\"rmax\":{},\"wmax\":{},\"j\":{}}}",
                if i == 0 { "" } else { "," },
                e.addr,
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                e.absent,
                n.polls,
                n.answers,
                n.empty,
                n.timeouts,
                n.corrupt,
                n.dups,
                n.frames,
                n.bytes,
                n.reply_us_mean(),
                n.reply_us_max,
                n.wait_us_max / 1000,
                n.joins,
            );
        }
        let _ = writeln!(w, "]}}");
    });
}
