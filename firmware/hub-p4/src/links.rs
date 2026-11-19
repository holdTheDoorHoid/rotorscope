//! Node links on the P4: the UART3 peripheral on the **UART1 header**
//! (RX = GPIO48, TX = GPIO47; DIP switch 1 on UART1; swapped with the GPS
//! 2026-11-17),
//! 921600 baud, `link` frames in, `link::Command`s out. Two ways to use it,
//! told apart at run time (`bus.rs`):
//! - **the wired bus** (default): many nodes share the two wires and the hub
//!   polls them in turn (`crates/wirebus`, docs/WIREBUS.md). `bus.rs` turns
//!   each node's frames into the same stream a gateway would send (a `Via`
//!   naming the node before each frame), so everything below is unchanged;
//! - **an ESP-NOW gateway** (or one point-to-point node) that streams on its
//!   own, read exactly as before; its RX is XIAO D7 = GPIO12.
//!
//! A gateway link speaks for many nodes: a `Frame::Via { node, rssi }` names
//! the sender of the one frame after it. Frames without a `Via` belong to the
//! link itself (a wired node, or the gateway's own `Hello` and reports).
//!
//! **Who may send what** (`link::origin`, docs/ESPNOW.md): every frame is
//! sorted by `link::origin::Sorter` before anything here or the web-head
//! feed uses it. A frame after a `Via` must be one a node may originate; a
//! frame with no `Via` one a node or the gateway may originate; a `Via`
//! names exactly the next frame and is dropped with it if that frame is
//! refused, and a `Via` straight after a `Via` (a node's own, passed by an
//! old gateway) is refused with the one it follows, so it can never lend its
//! name to the gateway's next own frame. A fixed gateway already drops all
//! of these; this is the second line, for an old or compromised gateway and
//! the wired bus. Refusals are counted (`refused`) and raised once a second
//! per claimed sender as the hub's own `Threat` / `ForgedLink`, `detail` 5,
//! to the tracker and the web head.
//!
//! **Node table.** [`MAX_RELAYED`] entries; when it is full, a new sender
//! takes the slot of the node silent longest, if that one has been silent
//! for [`EVICT_AFTER_MS`] (`evicted`). Otherwise its frames are dropped and
//! counted (`table_full`) until a slot frees up.
//!
//! The UART FIFO (128 bytes) fills in 1.4 ms at 921600 baud, but a screen
//! band takes ~6 ms to draw and a touch read ~3 ms. So the UART interrupt
//! (FIFO 64 bytes full, or 10 idle symbols) moves bytes into a 64 KB ring in PSRAM
//! (~0.7 s of full-speed traffic) and `poll` decodes from the ring whenever
//! the main loop gets to it.
//!
//! On the bus the interrupt also runs the bus master, so the next poll goes
//! out as soon as an answer ends (and on TX done), not when the loop gets
//! there.
//!
//! Sending is the other way round and much smaller (a few hundred bytes every
//! few seconds): [`GatewayLink::send`] queues bytes in `bus.rs`, which tops up
//! the 128-byte TX FIFO between polls without ever waiting for it.
//!
//! **Protocol 14 gateway** (docs/LINK-KEYS-DESIGN.md, package 13e). A gateway
//! that says `Hello` in `link::gw` messages (recognised by `bus.rs` in bus
//! mode, or here in plain reading) switches this link to [`Wire::Gateway`]:
//! the bytes are `GwUp` messages (decoder buffer in PSRAM), every node
//! packet is verified by `hublink::link::HubLink` (keys from `linkauth.rs`)
//! and only then are its frames sorted (`link::origin::node_batch`) and
//! queued, under the node number of the verified roster slot (slot + 1;
//! table numbers stay for the bus, a point-to-point node and an open kit).
//! The hub seals what goes the other way: a beacon every 500 ms (checked
//! on every `poll`, which also runs between screen bands), the roster,
//! commands from the coverage coordinator ([`GatewayLink::send`] turns
//! `To` + `Assign` into a sealed Transmit), syncs, and one `TimeSync` per
//! node, each sealed as it is queued on an empty line. The hub's refusals
//! and the gateway's `Refused` reports become `ForgedLink` threats
//! (details 1-4, 8-16). A gateway on an older protocol is refused: on a
//! keyed or closed hub its relayed frames are dropped, and the screen says
//! `OLD FIRMWARE: gateway`. The wired bus and a point-to-point node are read
//! exactly as before.
//!
//! The receive ring is 64 KB in PSRAM (`linkauth::RING`, about 0.7 s at
//! line rate; spike S0 saw 600 ms passes), and so is the tracker queue
//! (`linkauth::OUT`, 256 frames; `poll` stops taking bytes while it is half
//! full, so a long pass cannot overflow it).

use core::cell::RefCell;

use critical_section::Mutex;
use esp_hal::uart::{Config, RxConfig, Uart, UartInterrupt};
use esp_hal::{Blocking, handler, ram};
use hublink::link::{HubLink, Mode as KitMode, Sink, Up};
use link::gw::{GwError, GwUp};
use link::{Decoder, Frame, LinkError};

use crate::linkauth::{self, Ring};

static SERIAL: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
/// The receive ring, in PSRAM (`linkauth::Store`), set before the interrupt.
static RX: Mutex<RefCell<Option<&'static mut Ring>>> = Mutex::new(RefCell::new(None));
/// Bytes lost because the ring was full (the main loop stalled > ~0.7 s).
static RING_FULL: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static UART_ERR: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[handler]
#[ram]
fn on_uart() {
    critical_section::with(|cs| {
        let mut serial = SERIAL.borrow_ref_mut(cs);
        let Some(uart) = serial.as_mut() else { return };
        let mut rx = RX.borrow_ref_mut(cs);
        let Some(ring) = rx.as_deref_mut() else { return };
        let mut buf = [0u8; 64];
        loop {
            match uart.read_buffered(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Legacy: straight into the ring; bus: through the master.
                    let lost = crate::bus::rx(cs, uart, &buf[..n], ring);
                    RING_FULL.fetch_add(lost, core::sync::atomic::Ordering::Relaxed);
                }
                Err(_) => {
                    UART_ERR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    break;
                }
            }
        }
        // TX done (a poll or a command is out) or RX: the bus's next move.
        crate::bus::service(cs, uart, ring, false);
        uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout | UartInterrupt::TxDone);
    });
}

pub const BAUD: u32 = 921_600;
pub const MAX_RELAYED: usize = 16;
/// A relayed node silent this long may give its table slot to a new
/// sender: longer than the coverage coordinator's `coverage::GONE_MS` (15 s,
/// when it hands the node's job to someone else) and the evidence log's
/// node-gone time (30 s), so both have seen the node go before its slot
/// (and node number) is reused.
pub const EVICT_AFTER_MS: u64 = 60_000;
/// Refusals are reported at most once per this window per claimed sender.
const REFUSAL_WINDOW_MS: u32 = 1000;

#[derive(Clone, Copy, Default)]
pub struct NodeStats {
    /// Its node number on the hub (tracker, evidence, site file): the
    /// verified roster slot + 1 on a keyed gateway link, else a table
    /// number. 0 is the link itself.
    pub node: u8,
    pub mac: [u8; 6],
    pub rssi: i8,
    pub frames: u32,
    pub detections: u32,
    pub hello: bool,
    /// From the node's Hello or Status.
    pub role: Option<link::NodeRole>,
    /// From the node's Status: roles it can take (mask of `NodeRole::bit`),
    /// and the plan id it runs. `can` 0 = never sent a Status.
    pub can: u8,
    pub plan: u16,
    /// What the coverage coordinator last asked of it, and whether the node
    /// reports running exactly that.
    pub assigned: Option<(link::NodeRole, u16)>,
    pub applied: bool,
    /// From the node's last Stats.
    pub rid_found: u32,
    pub dropped: u32,
    pub uptime_s: u32,
    /// Hub time the node was last heard, ms.
    pub last_ms: u64,
}

/// Frames for the tracker: (node number, frame). Node numbers are
/// [`NodeStats::node`] for relayed nodes, 0 for a node wired to the link.
/// In PSRAM (`linkauth::OUT` entries).
pub type Out = linkauth::Out;

/// A node's latest `Status` and `Stats` for the tracker: its role and
/// plan (a change resets what the deaf-node check thinks normal), and its
/// health for the deaf-node and node-shed checks. Kept apart from `out` so a
/// flood that fills `out` cannot cost the very report that shows it, and
/// never logged as evidence. One per node (the latest of each wins).
/// Packed (20 bytes): `GatewayLink` lives in the main loop's frame.
#[derive(Clone, Copy)]
pub struct NodeDue {
    pub node: u8,
    /// From a Status: role (`None`: no Status pending), roles it can take,
    /// plan.
    pub role: Option<link::NodeRole>,
    pub can: u8,
    pub plan: u16,
    /// From a Stats: node uptime ms (low 32 bits; a wrap reads as a reboot,
    /// which only restarts the checks), frames seen, drop total.
    pub has_stats: bool,
    pub up_ms: u32,
    pub seen: u32,
    pub dropped: u32,
}
pub type StatsDue = heapless::Vec<NodeDue, { MAX_RELAYED + 1 }>;

#[derive(Default)]
pub struct LinkStats {
    pub bytes: u32,
    pub frames: u32,
    pub corrupt: u32,
    pub overflow: u32,
    pub uart_err: u32,
    /// Bytes lost to a full ring buffer.
    pub ring_full: u32,
    /// Frames with no `Via` before them: a node wired straight to this link.
    pub direct: u32,
    /// Frames refused because their sender may not originate them
    /// (`link::origin`): after a `Via`, not a node's; with none, the hub's
    /// or bus control. A refused frame's `Via` is not counted again.
    pub refused: u32,
    /// Relayed nodes that gave their slot to a new sender after
    /// [`EVICT_AFTER_MS`] of silence, and frames from new senders dropped
    /// because every slot was in use.
    pub evicted: u32,
    pub table_full: u32,
    /// Hub-raised `ForgedLink` threats for refusals (link and bus).
    pub refusal_threats: u32,
    /// The gateway's own Hello firmware string: `"gw key <id>"` when the
    /// ESP-NOW link is signed, `"gw OPEN no key"` when it is not (protocol
    /// 13 and older); a protocol 14 gateway's (`"gw nokey ..."`).
    pub gw_hello: heapless::String<16>,
    /// Frames an older gateway relayed that a keyed (or closed) hub dropped.
    pub old_gw_dropped: u32,
    /// Gateway messages that did not decode, and over-long ones.
    pub gw_corrupt: u32,
    pub gw_overflow: u32,
}

impl LinkStats {
    /// A gateway from before protocol 14 (an open hub still reads one)
    /// said in its last Hello that it was built without a link key: anyone
    /// nearby can inject node reports and commands. Shown on the screen as
    /// `OPEN LINK` for as long as it holds. A protocol 14 gateway's Hello
    /// says `gw nokey` (it never holds a key; the hub's record decides).
    pub fn gw_open(&self) -> bool {
        self.gw_hello.starts_with("gw OPEN")
    }
}

/// What the node link's bytes are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wire {
    /// `link::Frame`s: the wired bus (through `bus.rs`), one point-to-point
    /// node, or a gateway older than protocol 14.
    Frames,
    /// `link::gw` messages from a protocol 14 gateway.
    Gateway,
}

/// The UART queue as the verifier's [`Sink`].
struct BusSink;

impl Sink for BusSink {
    fn send(&mut self, bytes: &[u8]) -> bool {
        critical_section::with(|cs| crate::bus::send(cs, bytes))
    }
}

pub struct GatewayLink<'d> {
    _uart: core::marker::PhantomData<&'d ()>,
    dec: Decoder,
    /// The keyed link (PSRAM): verifier, sealer, kit state.
    pub auth: &'static mut HubLink,
    gw_dec: &'static mut link::gw::GwDecoder<[u8; link::gw::MAX_GW_FRAME]>,
    /// Nodes' `NodeHealth` (protocol 14), for `Tracker::ingest_node_health`.
    pub health: &'static mut linkauth::HealthQueue,
    /// Frames verified per node number since the main loop last took them,
    /// for `Tracker::link_received` (counted before anything drops one).
    pub received: [u32; MAX_RELAYED + 1],
    /// Node numbers that changed hands (`Tracker::forget_node`).
    pub forget: heapless::Vec<u8, { MAX_RELAYED + 1 }>,
    pub wire: Wire,
    /// The kit records and boot counter as read at boot.
    pub kit: hublink::kit::KitState,
    pub counter: hublink::kit::Counter,
    /// A gateway's `link::Frame` Hello said this older protocol.
    old_gw: Option<u8>,
    /// GPS time for the next time-sync round (UTC ms at hub ms, PPS), and
    /// the round's in progress.
    time: Option<(u64, u64, bool)>,
    time_base: Option<(u64, u64, bool)>,
    last_print: u64,
    sorter: link::origin::Sorter,
    /// Refusals per claimed sender, for the once-a-second threat.
    refusals: link::origin::RefusalLog<8>,
    pub stats: LinkStats,
    pub nodes: heapless::Vec<NodeStats, MAX_RELAYED>,
    /// Detections and open-link sightings waiting for the tracker (PSRAM).
    pub out: &'static mut Out,
    /// Frames dropped because `out` was full.
    pub out_full: u32,
    /// Nodes' Stats waiting for the tracker.
    pub stats_due: StatsDue,
    /// Aero nodes' raw aircraft frames (protocol 11), in their own queue in
    /// PSRAM (`aero::Store`): they come up to 200 a second and `out` is
    /// sized for drone detections. `None` (no PSRAM): they share `out`.
    pub air: Option<&'static mut crate::aero::AirQueue>,
    /// Aircraft frames dropped because `air` was full.
    pub air_full: u32,
    /// Bytes written to the TX FIFO, and bytes refused for want of queue room.
    pub tx_bytes: u32,
    pub tx_full: u32,
    /// The web head's copy of this stream (`webhead.rs`), when built with one.
    pub feed: Option<crate::webhead::Feed>,
}

impl<'d> GatewayLink<'d> {
    pub fn new(
        uart: impl esp_hal::uart::Instance + 'static,
        rx_pin: impl esp_hal::gpio::interconnect::PeripheralInput<'static>,
        tx_pin: impl esp_hal::gpio::interconnect::PeripheralOutput<'static>,
        parts: linkauth::Parts,
    ) -> Self {
        let cfg = Config::default()
            .with_baudrate(BAUD)
            .with_rx(RxConfig::default().with_fifo_full_threshold(64).with_timeout(10));
        let mut uart = Uart::new(uart, cfg).expect("node link UART config").with_rx(rx_pin).with_tx(tx_pin);
        // A random epoch per boot: nodes holding an address from an earlier
        // boot join again (and ask for the same address back).
        let mut epoch = [0u8; 2];
        esp_hal::rng::Rng::new().read(&mut epoch);
        crate::bus::init(u16::from_le_bytes(epoch));
        uart.set_interrupt_handler(on_uart);
        let all = UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout | UartInterrupt::TxDone;
        let linkauth::Parts { ring, dec: gw_dec, out, health, link: auth, kit, counter } = parts;
        critical_section::with(|cs| {
            RX.borrow_ref_mut(cs).replace(ring);
            uart.clear_interrupts(all);
            uart.listen(all);
            SERIAL.borrow_ref_mut(cs).replace(uart);
        });
        Self {
            _uart: core::marker::PhantomData,
            dec: Decoder::new(),
            auth,
            gw_dec,
            health,
            received: [0; MAX_RELAYED + 1],
            forget: heapless::Vec::new(),
            wire: Wire::Frames,
            kit,
            counter,
            old_gw: None,
            time: None,
            time_base: None,
            last_print: 0,
            sorter: link::origin::Sorter::new(),
            refusals: link::origin::RefusalLog::new(),
            stats: LinkStats::default(),
            nodes: heapless::Vec::new(),
            out,
            out_full: 0,
            stats_due: StatsDue::new(),
            air: None,
            air_full: 0,
            tx_bytes: 0,
            tx_full: 0,
            feed: None,
        }
    }

    /// Bench only (`fakenode`, `fakedrone`): put bytes into the receive ring as if they
    /// had arrived on the wire.
    #[cfg(any(feature = "fakenode", feature = "fakedrone"))]
    pub fn inject(&mut self, bytes: &[u8]) {
        critical_section::with(|cs| {
            if let Some(ring) = RX.borrow_ref_mut(cs).as_deref_mut() {
                for &b in bytes {
                    let _ = ring.push_back(b);
                }
            }
        });
    }

    /// Queue `bytes` (whole command frames) for the gateway or the bus
    /// nodes. All or nothing: false, and counted, if the queue lacks room.
    /// To a protocol 14 gateway, `To { node }` and the frames after it are
    /// sealed for that node instead ([`HubLink::command`]).
    pub fn send(&mut self, bytes: &[u8]) -> bool {
        let ok = if self.wire == Wire::Gateway {
            self.send_sealed(bytes)
        } else {
            critical_section::with(|cs| crate::bus::send(cs, bytes))
        };
        self.pump_tx(false);
        ok
    }

    /// `To { node }` then whole command frames: one sealed Transmit.
    #[inline(never)]
    fn send_sealed(&mut self, bytes: &[u8]) -> bool {
        let Some((node, frames)) = hublink::link::split_to(bytes) else { return false };
        self.auth.command(node, frames, crate::app::now_ms(), &mut BusSink).is_ok()
    }

    /// GPS time for the nodes, once a second: on the bus or to an old
    /// gateway as before (`bus::time_sync`); to a protocol 14 gateway, one
    /// sealed `TimeSync` per synced node, each as the line is free.
    pub fn time_sync(&mut self, utc_ms: u64, now_ms: u64, pps: bool) {
        crate::bus::time_sync(utc_ms, now_ms * 1000, pps);
        self.time = Some((utc_ms, now_ms, pps));
    }

    /// A beacon now if the last is `min_ms` old: before a step that blocks
    /// the loop (the SD mount, up to ~0.6 s), so the gap stays under 1 s.
    pub fn beacon_if_older(&mut self, now_ms: u64, min_ms: u64) {
        if self.wire == Wire::Gateway {
            self.auth.beacon_if_older(now_ms, min_ms, &mut BusSink);
            self.pump_tx(false);
        }
    }

    /// The node with this number.
    pub fn node(&self, node: u8) -> Option<&NodeStats> {
        self.nodes.iter().find(|n| n.node == node)
    }

    /// The strip's link line (`screen::Caveats::link`).
    pub fn link_caveats(&self, now_ms: u64) -> screen::LinkCaveats {
        let v = self.auth.view(now_ms);
        screen::LinkCaveats {
            kit: linkauth::kit_problem(&self.kit, &self.counter),
            old_gateway: v.old_gateway || self.old_gw.is_some(),
            old_nodes: v.old_nodes,
            unprovisioned: v.unprovisioned,
            bench_gateway: v.bench_gateway,
        }
    }

    /// The link carries no authentication: an open kit, or an old gateway
    /// built without a key.
    pub fn link_open(&self) -> bool {
        self.auth.mode() == KitMode::Open || self.stats.gw_open()
    }

    /// Bus timeouts and polls, and queued commands into the TX FIFO, as
    /// many as fit right now (`bus.rs`; also run from the UART interrupt).
    fn pump_tx(&mut self, main_loop: bool) {
        critical_section::with(|cs| {
            let mut serial = SERIAL.borrow_ref_mut(cs);
            let Some(uart) = serial.as_mut() else { return };
            let mut rx = RX.borrow_ref_mut(cs);
            let Some(ring) = rx.as_deref_mut() else { return };
            crate::bus::service(cs, uart, ring, main_loop);
            if let Some(s) = crate::bus::BUS.borrow_ref(cs).as_ref() {
                self.tx_bytes = s.cmd_bytes;
                self.tx_full = s.cmd_full;
            }
        });
    }

    /// Drain whatever the UART has; returns frames handled.
    pub fn poll(&mut self, now_ms: u64) -> u32 {
        let mut buf = [0u8; 256];
        let mut handled = 0;
        self.pump_tx(true);
        crate::bus::print_notes();
        if let Some(f) = self.feed.as_mut() {
            f.pump();
        }
        self.stats.uart_err = UART_ERR.load(core::sync::atomic::Ordering::Relaxed);
        self.stats.ring_full = RING_FULL.load(core::sync::atomic::Ordering::Relaxed);
        self.choose_wire();
        loop {
            // Leave bytes in the ring while the tracker queue is half full:
            // the main loop drains it, the next pass carries on.
            if self.out.len() > linkauth::OUT / 2 {
                break;
            }
            let n = critical_section::with(|cs| {
                let mut rx = RX.borrow_ref_mut(cs);
                let Some(ring) = rx.as_deref_mut() else { return 0 };
                let mut n = 0;
                while n < buf.len() {
                    let Some(b) = ring.pop_front() else { break };
                    buf[n] = b;
                    n += 1;
                }
                n
            });
            if n == 0 {
                break;
            }
            self.stats.bytes += n as u32;
            if self.wire == Wire::Gateway {
                handled += self.gateway_bytes(&buf[..n], now_ms);
                continue;
            }
            if crate::bus::mode() == crate::bus::Mode::Legacy && self.sniff(&buf[..n], now_ms) {
                // A protocol 14 gateway: the rest is its messages.
                continue;
            }
            let auth_mode = self.auth.mode();
            let (stats, nodes, sorter, refusals, feed, old_gw, forget) = (
                &mut self.stats,
                &mut self.nodes,
                &mut self.sorter,
                &mut self.refusals,
                &mut self.feed,
                &mut self.old_gw,
                &mut self.forget,
            );
            let mut q = Queues {
                out: &mut *self.out,
                out_full: &mut self.out_full,
                air: &mut self.air,
                air_full: &mut self.air_full,
                stats: &mut self.stats_due,
            };
            self.dec.feed(&buf[..n], |r| {
                handled += 1;
                match r {
                    Ok(f) => {
                        // The web head gets the stream as it came, before
                        // anything here consumes the frame; its forwarder
                        // sorts it exactly as `on_frame` does, so it passes
                        // on nothing refused here.
                        if let Some(feed) = feed.as_mut() {
                            feed.relay(&f);
                        }
                        Self::on_frame(stats, nodes, sorter, refusals, &mut q, now_ms, f, auth_mode, old_gw, forget)
                    }
                    Err(e) => {
                        match e {
                            LinkError::Corrupt => stats.corrupt += 1,
                            LinkError::Overflow => stats.overflow += 1,
                        }
                        sorter.reset();
                        if let Some(feed) = feed.as_mut() {
                            feed.stream_error();
                        }
                    }
                }
            });
        }
        if self.wire == Wire::Gateway {
            self.gateway_timers(now_ms);
        }
        self.report_refusals(now_ms);
        if let Some(f) = self.feed.as_mut() {
            f.pump();
        }
        handled
    }

    /// Follow `bus.rs`: a protocol 14 gateway's `Hello` seen in bus mode
    /// switches to [`Wire::Gateway`]; back to bus mode (10 s of silence)
    /// switches back.
    fn choose_wire(&mut self) {
        let to = if crate::bus::take_gw_hello() {
            Wire::Gateway
        } else if self.wire == Wire::Gateway && crate::bus::mode() == crate::bus::Mode::Bus {
            Wire::Frames
        } else {
            return;
        };
        self.set_wire(to);
    }

    fn set_wire(&mut self, to: Wire) {
        if self.wire == to {
            return;
        }
        println!(
            "hub-p4: node link now reads {}",
            if to == Wire::Gateway { "a protocol 14 gateway (link::gw messages, verified per node)" } else { "link frames (bus, point-to-point node, or an old gateway)" }
        );
        self.wire = to;
        self.gw_dec.reset();
        self.dec = Decoder::new();
        self.sorter.reset();
        crate::bus::set_gateway14(to == Wire::Gateway);
        // Node numbers mean something else on the other wire.
        for n in self.nodes.iter() {
            let _ = self.forget.push(n.node);
        }
        self.nodes.clear();
        self.old_gw = None;
    }

    /// Plain reading: is it a protocol 14 gateway? Its `Hello` switches
    /// the wire and is handled at once (the roster must follow promptly).
    #[inline(never)]
    fn sniff(&mut self, bytes: &[u8], now_ms: u64) -> bool {
        let mut hello: Option<GwUp<'static>> = None;
        self.gw_dec.feed_up(bytes, |r| {
            if let Ok(GwUp::Hello { protocol, firmware, mac, channel }) = r
                && firmware.starts_with("gw ")
            {
                hello = Some(GwUp::Hello { protocol, firmware, mac, channel });
            }
        });
        let Some(h) = hello else { return false };
        self.set_wire(Wire::Gateway);
        self.gw_message(h, now_ms);
        true
    }

    /// Bytes from a protocol 14 gateway; returns messages handled.
    #[inline(never)]
    fn gateway_bytes(&mut self, bytes: &[u8], now_ms: u64) -> u32 {
        let mut handled = 0;
        // The decoder lives in PSRAM next to the verifier: take it out of
        // `self` for the call (a pointer swap, not a copy).
        let dec: *mut link::gw::GwDecoder<[u8; link::gw::MAX_GW_FRAME]> = &mut *self.gw_dec;
        // SAFETY: `gw_dec` is a `&'static mut` to its own PSRAM field;
        // nothing reached through `self` in `gw_message` touches it.
        unsafe { &mut *dec }.feed_up(bytes, |r| {
            handled += 1;
            match r {
                Ok(m) => self.gw_message(m, now_ms),
                Err(GwError::Overflow) => self.stats.gw_overflow += 1,
                Err(_) => self.stats.gw_corrupt += 1,
            }
        });
        handled
    }

    /// One gateway message: verified packets to the node table and queues.
    fn gw_message(&mut self, m: GwUp<'_>, now_ms: u64) {
        if let GwUp::Hello { firmware, .. } = &m {
            self.stats.gw_hello = firmware.clone();
        }
        let mut q = Queues {
            out: &mut *self.out,
            out_full: &mut self.out_full,
            air: &mut self.air,
            air_full: &mut self.air_full,
            stats: &mut self.stats_due,
        };
        let (stats, nodes, refusals, feed, received, forget, health) =
            (&mut self.stats, &mut self.nodes, &mut self.refusals, &mut self.feed, &mut self.received, &mut self.forget, &mut *self.health);
        self.auth.on_up(m, now_ms, &mut BusSink, &mut |up| match up {
            Up::Batch { slot, mac, rssi, payload } => {
                let Some(i) = table_entry(nodes, mac, slot.map(HubLink::node_number), now_ms, stats, forget) else { return };
                let node = nodes[i].node;
                if let Some(r) = received.get_mut(usize::from(node)) {
                    *r = r.wrapping_add(airlink::v3::frames(payload).count() as u32);
                }
                link::origin::node_batch(payload, |c| match c {
                    link::origin::NodeChunk::Relay { frame, .. } => {
                        stats.frames += 1;
                        if let Some(feed) = feed.as_mut() {
                            feed.relay(&Frame::Via { node: mac, rssi });
                            feed.relay(&frame);
                        }
                        if let Frame::NodeHealth(h) = &frame {
                            if health.is_full() {
                                health.pop_front();
                            }
                            let _ = health.push_back((node, *h));
                        }
                        node_frame(&mut nodes[i], node, rssi, &mut q, now_ms, frame);
                    }
                    link::origin::NodeChunk::Refused { .. } => {
                        stats.refused += 1;
                        refusals.record(mac, link::origin::FORGED_ORIGIN, rssi);
                    }
                    link::origin::NodeChunk::Corrupt => stats.corrupt += 1,
                });
            }
            Up::Gateway { accepted, protocol } => {
                if !accepted {
                    println!("hub-p4: gateway speaks protocol {protocol}: REFUSED (needs {}); reflash it", link::PROTOCOL_VERSION);
                }
            }
            Up::Refused(r) => raise_refusal(&r, feed, &mut q, stats, now_ms),
        });
    }

    /// The verifier's timers (beacon, roster, syncs, refusals), the time
    /// syncs, and a console line every 5 s.
    #[inline(never)]
    fn gateway_timers(&mut self, now_ms: u64) {
        let mut q = Queues {
            out: &mut *self.out,
            out_full: &mut self.out_full,
            air: &mut self.air,
            air_full: &mut self.air_full,
            stats: &mut self.stats_due,
        };
        let (stats, feed) = (&mut self.stats, &mut self.feed);
        self.auth.tick(now_ms, &mut BusSink, |r| raise_refusal(&r, feed, &mut q, stats, now_ms));
        if let Some(t) = self.time.take() {
            self.time_base = Some(t);
            self.auth.time_sync_round(now_ms);
        }
        if self.auth.time_sync_pending()
            && crate::bus::tx_idle()
            && let Some((utc, at, pps)) = self.time_base
        {
            let utc_now = utc + now_ms.saturating_sub(at);
            self.auth.time_sync_step(now_ms, utc_now, pps, &mut BusSink);
        }
        self.pump_tx(false);
        if now_ms.saturating_sub(self.last_print) >= 5_000 {
            self.last_print = now_ms;
            self.print_gateway();
        }
    }

    fn print_gateway(&self) {
        let a = &*self.auth;
        let s = &a.stats;
        let r = &s.rejects;
        use airlink::v3::Reject as R;
        let (gm, gch) = a.gateway().map_or(([0; 6], 0), |g| (g.mac, g.channel));
        println!(
            "hub-p4: gwlink kit={} epoch={:?} gw={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ch={} pkts={} ok={} dup={} open={} bad_tag={} replay={} unauth={} stale={} old_fw={} not_in_roster={} late_uplink={} future_uplink={} malformed={} foreign={} unprov={} beacons={} rosters={} syncs={} answers={} cmds={} cmd_refused={} cancels={} time_syncs={} ts_held_max={}ms acked={} given_up={} sink_full={} gw_refused={:?} corrupt={} overflow={} out_full={}",
            self.kit.name(), a.epoch(), gm[0], gm[1], gm[2], gm[3], gm[4], gm[5], gch,
            s.packets, s.accepted, s.duplicates, s.open_batches,
            r[R::BadTag.index()], r[R::Replay.index()], r[R::Unauthenticated.index()], r[R::Stale.index()], r[R::OldFirmware.index()],
            r[R::NotInRoster.index()], r[R::LateUplink.index()], r[R::FutureUplink.index()], r[R::Malformed.index()], r[R::Foreign.index()],
            s.unprovisioned, s.beacons, s.rosters, s.syncs, s.sync_answers, s.commands, s.command_refused, s.cancels,
            s.time_syncs, s.time_sync_held_max_ms, s.tx_acked, s.tx_given_up, s.sink_full, s.gw_refused,
            self.stats.gw_corrupt, self.stats.gw_overflow, self.out_full
        );
        if let Some(g) = a.gw_stats {
            println!(
                "hub-p4: gateway uptime={}s beacons_sent={} foreign={} uart_drops={} refused={:?}",
                g.uptime_s, g.beacons_sent, g.foreign, g.uart_drops, g.refused
            );
        }
    }

    /// Once a second per claimed sender: the refusals here and on the wired
    /// bus as the hub's own `Threat` / `ForgedLink` (`detail` 5), to the
    /// tracker (node 0, the link) and the web head.
    fn report_refusals(&mut self, now_ms: u64) {
        let mut out: heapless::Vec<link::origin::Refusal, 12> = heapless::Vec::new();
        self.refusals.drain(now_ms, REFUSAL_WINDOW_MS, |r| {
            let _ = out.push(r);
        });
        crate::bus::drain_refusals(now_ms, REFUSAL_WINDOW_MS, |r| {
            let _ = out.push(r);
        });
        let mut q = Queues {
            out: &mut *self.out,
            out_full: &mut self.out_full,
            air: &mut self.air,
            air_full: &mut self.air_full,
            stats: &mut self.stats_due,
        };
        for r in &out {
            raise_refusal(r, &mut self.feed, &mut q, &mut self.stats, now_ms);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_frame(
        stats: &mut LinkStats,
        nodes: &mut heapless::Vec<NodeStats, MAX_RELAYED>,
        sorter: &mut link::origin::Sorter,
        refusals: &mut link::origin::RefusalLog<8>,
        q: &mut Queues<'_>,
        now_ms: u64,
        f: Frame,
        auth: KitMode,
        old_gw: &mut Option<u8>,
        forget: &mut heapless::Vec<u8, { MAX_RELAYED + 1 }>,
    ) {
        use link::origin::Sorted;
        stats.frames += 1;
        let (mac, rssi) = match sorter.sort(&f) {
            Sorted::Marker => return,
            Sorted::Refused { from, .. } => {
                stats.refused += 1;
                // The link's own stream (no Via): all-zero source.
                let (src, rssi) = from.unwrap_or(([0; 6], 0));
                refusals.record(src, link::origin::FORGED_ORIGIN, rssi);
                return;
            }
            Sorted::Own => {
                stats.direct += 1;
                if let Frame::Hello { role: link::NodeRole::Gateway, firmware, protocol } = &f {
                    stats.gw_hello = firmware.clone();
                    // Protocol 13 and older: its nodes' frames are not
                    // verified by this hub (docs/LINK-KEYS-DESIGN.md 6.4).
                    if *protocol < link::PROTOCOL_VERSION && old_gw.is_none() {
                        println!(
                            "hub-p4: OLD FIRMWARE: the gateway speaks protocol {protocol} (needs {}); {}",
                            link::PROTOCOL_VERSION,
                            if auth == KitMode::Open { "open kit: read as before" } else { "its relayed frames are refused; reflash it" }
                        );
                    }
                    *old_gw = (*protocol < link::PROTOCOL_VERSION).then_some(*protocol);
                }
                q.health(0, &f);
                if is_data(&f) {
                    q.push(0, f);
                }
                return;
            }
            Sorted::Relayed { node, rssi } => (node, rssi),
        };
        // An older gateway relays what this hub cannot check: only an open
        // kit takes it. (Bus nodes come through `Via`s too, but a bus never
        // says a gateway `Hello`.)
        if old_gw.is_some() && auth != KitMode::Open {
            stats.old_gw_dropped += 1;
            return;
        }
        let Some(i) = table_entry(nodes, mac, None, now_ms, stats, forget) else { return };
        let node = nodes[i].node;
        node_frame(&mut nodes[i], node, rssi, q, now_ms, f);
    }
}

/// The table entry for `mac`: with `want`, at that node number (a verified
/// roster slot); without, any free number, or, when the table is full, the
/// number of the node silent longest once it has been silent long enough.
/// A number that changes hands goes on `forget`.
fn table_entry(
    nodes: &mut heapless::Vec<NodeStats, MAX_RELAYED>,
    mac: [u8; 6],
    want: Option<u8>,
    now_ms: u64,
    stats: &mut LinkStats,
    forget: &mut heapless::Vec<u8, { MAX_RELAYED + 1 }>,
) -> Option<usize> {
    if let Some(node) = want {
        if let Some(i) = nodes.iter().position(|n| n.node == node) {
            if nodes[i].mac != mac {
                let _ = forget.push(node);
                nodes[i] = NodeStats { node, mac, ..Default::default() };
            }
            return Some(i);
        }
        // The slot's number may be held by a table entry for this MAC from
        // before: give the number back.
        if let Some(i) = nodes.iter().position(|n| n.mac == mac) {
            let _ = forget.push(nodes[i].node);
            nodes[i] = NodeStats { node, mac, ..Default::default() };
            return Some(i);
        }
    } else if let Some(i) = nodes.iter().position(|n| n.mac == mac) {
        return Some(i);
    }
    if !nodes.is_full() {
        let node = want.unwrap_or_else(|| (1..=MAX_RELAYED as u8).find(|k| !nodes.iter().any(|n| n.node == *k)).unwrap_or(1));
        let _ = nodes.push(NodeStats { node, mac, ..Default::default() });
        return Some(nodes.len() - 1);
    }
    // Full: the slot of the node silent longest, once it has been silent
    // long enough.
    let stale = (0..nodes.len())
        .filter(|&i| now_ms.saturating_sub(nodes[i].last_ms) > EVICT_AFTER_MS)
        .min_by_key(|&i| nodes[i].last_ms);
    let Some(i) = stale else {
        stats.table_full += 1;
        return None;
    };
    stats.evicted += 1;
    let old = nodes[i].node;
    let _ = forget.push(old);
    nodes[i] = NodeStats { node: want.unwrap_or(old), mac, ..Default::default() };
    Some(i)
}

/// One node frame, from a known node: its table entry, its health for the
/// tracker, and the frame itself if it carries data.
fn node_frame(n: &mut NodeStats, node: u8, rssi: i8, q: &mut Queues<'_>, now_ms: u64, f: Frame) {
    n.rssi = rssi;
    n.frames += 1;
    n.last_ms = now_ms;
    match &f {
        Frame::Detection(_) => n.detections += 1,
        Frame::Hello { role, .. } => {
            n.hello = true;
            n.role = Some(*role);
        }
        Frame::Status { role, can, plan } => {
            n.role = Some(*role);
            n.can = *can;
            n.plan = *plan;
        }
        Frame::Stats { uptime_us, rid_found, dropped, .. } => {
            n.rid_found = *rid_found;
            n.dropped = *dropped;
            n.uptime_s = (*uptime_us / 1_000_000) as u32;
        }
        _ => {}
    }
    q.health(node, &f);
    if is_data(&f) {
        q.push(node, f);
    }
}

/// A refusal (the hub's, the bus's or the gateway's) as the hub's own
/// `Threat` / `ForgedLink` to the tracker (node 0, the link) and the web head.
fn raise_refusal(r: &link::origin::Refusal, feed: &mut Option<crate::webhead::Feed>, q: &mut Queues<'_>, stats: &mut LinkStats, now_ms: u64) {
    let t = link::origin::forged_link(r, [0; 6], 0, 0, now_ms * 1000);
    crate::println!(
        "hub-p4: refused {} packet(s)/frame(s) from {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} in {} ms: {} (forged_link detail {})",
        r.count, r.source[0], r.source[1], r.source[2], r.source[3], r.source[4], r.source[5], r.window_ms,
        link::origin::forged_reason(r.detail).unwrap_or("?"), r.detail
    );
    let f = Frame::Threat(t);
    if let Some(feed) = feed.as_mut() {
        feed.fwd.send(&f);
    }
    q.push(0, f);
    stats.refusal_threats += 1;
}

/// Where `on_frame` puts frames for the main loop.
struct Queues<'a> {
    out: &'a mut Out,
    out_full: &'a mut u32,
    air: &'a mut Option<&'static mut crate::aero::AirQueue>,
    air_full: &'a mut u32,
    stats: &'a mut StatsDue,
}

impl Queues<'_> {
    /// Keep `node`'s latest Status or Stats for the tracker. Out of line:
    /// `poll` inlines into the main loop, whose frame is the P4's tightest
    /// stack budget.
    #[inline(never)]
    fn health(&mut self, node: u8, f: &Frame) {
        if !matches!(f, Frame::Status { .. } | Frame::Stats { .. }) {
            return;
        }
        let i = match self.stats.iter().position(|x| x.node == node) {
            Some(i) => i,
            None => {
                let empty = NodeDue { node, role: None, can: 0, plan: 0, has_stats: false, up_ms: 0, seen: 0, dropped: 0 };
                if self.stats.push(empty).is_err() {
                    return;
                }
                self.stats.len() - 1
            }
        };
        let d = &mut self.stats[i];
        match f {
            Frame::Status { role, can, plan } => {
                (d.role, d.can, d.plan) = (Some(*role), *can, *plan);
            }
            Frame::Stats { uptime_us, frames_seen, dropped, .. } => {
                d.has_stats = true;
                d.up_ms = (*uptime_us / 1000) as u32;
                (d.seen, d.dropped) = (*frames_seen, *dropped);
            }
            _ => {}
        }
    }

    fn push(&mut self, node: u8, f: Frame) {
        match (f, self.air.as_deref_mut()) {
            (Frame::Air(a), Some(q)) => {
                if q.push_back((node, a)).is_err() {
                    *self.air_full += 1;
                }
            }
            (f, _) => {
                if self.out.push_back((node, f)).is_err() {
                    *self.out_full += 1;
                }
            }
        }
    }
}

/// Frames that carry detections for the tracker (not link bookkeeping).
fn is_data(f: &Frame) -> bool {
    matches!(
        f,
        Frame::Detection(_)
            | Frame::OpenLink(_)
            | Frame::DroneId(_)
            | Frame::Rf(_)
            | Frame::DjiWifi { .. }
            | Frame::WifiDrone(_)
            // Protocol 9: attacks and jamming from nodes, and forged-link
            // reports from the gateway itself.
            | Frame::Threat(_)
            // Protocol 11: aero nodes' raw aircraft frames, and their
            // state (kept per node by `aero.rs`, not logged as evidence).
            | Frame::Air(_)
            | Frame::AeroStatus(_)
            // Protocol 12: control links and control-band energy from the
            // same nodes (tracker `ctrllinks`, logged as evidence).
            | Frame::Control(_)
            | Frame::ControlEnergy(_)
            // Protocol 13: the MAVLink frame behind a MavlinkAttack threat
            // (logged as evidence; the threat is a `Frame::Threat`).
            | Frame::MavlinkEvidence(_)
    )
}
