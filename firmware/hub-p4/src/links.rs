//! Node links on the P4: UART3 (header UART3-IN, RX = GPIO28, TX = GPIO27),
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
//! link itself (a wired node).
//!
//! The UART FIFO (128 bytes) fills in 1.4 ms at 921600 baud, but a screen
//! band takes ~6 ms to draw and a touch read ~3 ms. So the UART interrupt
//! (FIFO 64 bytes full, or 10 idle symbols) moves bytes into an 8 KB ring
//! (~90 ms of full-speed traffic) and `poll` decodes from the ring whenever
//! the main loop gets to it.
//!
//! On the bus the interrupt also runs the bus master, so the next poll goes
//! out as soon as an answer ends (and on TX done), not when the loop gets
//! there.
//!
//! Sending is the other way round and much smaller (a few hundred bytes every
//! few seconds): [`GatewayLink::send`] queues bytes in `bus.rs`, which tops up
//! the 128-byte TX FIFO between polls without ever waiting for it.

use core::cell::RefCell;

use critical_section::Mutex;
use esp_hal::uart::{Config, RxConfig, Uart, UartInterrupt};
use esp_hal::{Blocking, handler, ram};
use link::{Decoder, Frame, LinkError};

const RING: usize = 8192;
static SERIAL: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
static RX: Mutex<RefCell<heapless::Deque<u8, RING>>> = Mutex::new(RefCell::new(heapless::Deque::new()));
/// Bytes lost because the ring was full (the main loop stalled > ~90 ms).
static RING_FULL: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static UART_ERR: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[handler]
#[ram]
fn on_uart() {
    critical_section::with(|cs| {
        let mut serial = SERIAL.borrow_ref_mut(cs);
        let Some(uart) = serial.as_mut() else { return };
        let mut ring = RX.borrow_ref_mut(cs);
        let mut buf = [0u8; 64];
        loop {
            match uart.read_buffered(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Legacy: straight into the ring; bus: through the master.
                    let lost = crate::bus::rx(cs, uart, &buf[..n], &mut ring);
                    RING_FULL.fetch_add(lost, core::sync::atomic::Ordering::Relaxed);
                }
                Err(_) => {
                    UART_ERR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    break;
                }
            }
        }
        // TX done (a poll or a command is out) or RX: the bus's next move.
        crate::bus::service(cs, uart, &mut ring, false);
        uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout | UartInterrupt::TxDone);
    });
}

pub const BAUD: u32 = 921_600;
pub const MAX_RELAYED: usize = 16;

#[derive(Clone, Copy, Default)]
pub struct NodeStats {
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

/// Frames for the tracker: (node number, frame). Node numbers are 1 + the
/// index in `nodes` for relayed nodes, 0 for a node wired to the link.
pub type Out = heapless::Deque<(u8, Frame), 8>;

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
    /// The gateway's own Hello firmware string: `"gw key <id>"` when the
    /// ESP-NOW link is signed, `"gw OPEN no key"` when it is not.
    pub gw_hello: heapless::String<16>,
}

pub struct GatewayLink<'d> {
    _uart: core::marker::PhantomData<&'d ()>,
    dec: Decoder,
    via: Option<([u8; 6], i8)>,
    pub stats: LinkStats,
    pub nodes: heapless::Vec<NodeStats, MAX_RELAYED>,
    /// Detections and open-link sightings waiting for the tracker.
    pub out: Out,
    /// Frames dropped because `out` was full.
    pub out_full: u32,
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
    ) -> Self {
        let cfg = Config::default()
            .with_baudrate(BAUD)
            .with_rx(RxConfig::default().with_fifo_full_threshold(64).with_timeout(10));
        let mut uart = Uart::new(uart, cfg).expect("UART3 config").with_rx(rx_pin).with_tx(tx_pin);
        // A random epoch per boot: nodes holding an address from an earlier
        // boot join again (and ask for the same address back).
        let mut epoch = [0u8; 2];
        esp_hal::rng::Rng::new().read(&mut epoch);
        crate::bus::init(u16::from_le_bytes(epoch));
        uart.set_interrupt_handler(on_uart);
        let all = UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout | UartInterrupt::TxDone;
        critical_section::with(|cs| {
            uart.clear_interrupts(all);
            uart.listen(all);
            SERIAL.borrow_ref_mut(cs).replace(uart);
        });
        Self {
            _uart: core::marker::PhantomData,
            dec: Decoder::new(),
            via: None,
            stats: LinkStats::default(),
            nodes: heapless::Vec::new(),
            out: Out::new(),
            out_full: 0,
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
            let mut ring = RX.borrow_ref_mut(cs);
            for &b in bytes {
                let _ = ring.push_back(b);
            }
        });
    }

    /// Queue `bytes` (whole command frames) for the gateway or the bus
    /// nodes. All or nothing: false, and counted, if the queue lacks room.
    pub fn send(&mut self, bytes: &[u8]) -> bool {
        let ok = critical_section::with(|cs| crate::bus::send(cs, bytes));
        self.pump_tx(false);
        ok
    }

    /// Bus timeouts and polls, and queued commands into the TX FIFO, as
    /// many as fit right now (`bus.rs`; also run from the UART interrupt).
    fn pump_tx(&mut self, main_loop: bool) {
        critical_section::with(|cs| {
            let mut serial = SERIAL.borrow_ref_mut(cs);
            let Some(uart) = serial.as_mut() else { return };
            let mut ring = RX.borrow_ref_mut(cs);
            crate::bus::service(cs, uart, &mut ring, main_loop);
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
        loop {
            let n = critical_section::with(|cs| {
                let mut ring = RX.borrow_ref_mut(cs);
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
            let (stats, nodes, via, feed) = (&mut self.stats, &mut self.nodes, &mut self.via, &mut self.feed);
            let mut q = Queues { out: &mut self.out, out_full: &mut self.out_full, air: &mut self.air, air_full: &mut self.air_full };
            self.dec.feed(&buf[..n], |r| {
                handled += 1;
                match r {
                    Ok(f) => {
                        // The web head gets the stream as it came, before
                        // anything here consumes the frame.
                        if let Some(feed) = feed.as_mut() {
                            feed.relay(&f);
                        }
                        Self::on_frame(stats, nodes, via, &mut q, now_ms, f)
                    }
                    Err(e) => {
                        match e {
                            LinkError::Corrupt => stats.corrupt += 1,
                            LinkError::Overflow => stats.overflow += 1,
                        }
                        *via = None;
                        if let Some(feed) = feed.as_mut() {
                            feed.stream_error();
                        }
                    }
                }
            });
        }
        if let Some(f) = self.feed.as_mut() {
            f.pump();
        }
        handled
    }

    fn on_frame(
        stats: &mut LinkStats,
        nodes: &mut heapless::Vec<NodeStats, MAX_RELAYED>,
        via: &mut Option<([u8; 6], i8)>,
        q: &mut Queues<'_>,
        now_ms: u64,
        f: Frame,
    ) {
        stats.frames += 1;
        if let Frame::Via { node, rssi } = f {
            *via = Some((node, rssi));
            return;
        }
        let Some((mac, rssi)) = via.take() else {
            stats.direct += 1;
            if let Frame::Hello { role: link::NodeRole::Gateway, firmware, .. } = &f {
                stats.gw_hello = firmware.clone();
            }
            if is_data(&f) {
                q.push(0, f);
            }
            return;
        };
        let i = match nodes.iter().position(|n| n.mac == mac) {
            Some(i) => i,
            None => {
                if nodes.push(NodeStats { mac, ..Default::default() }).is_err() {
                    return;
                }
                nodes.len() - 1
            }
        };
        let n = &mut nodes[i];
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
        if is_data(&f) {
            q.push(i as u8 + 1, f);
        }
    }
}

/// Where `on_frame` puts frames for the main loop.
struct Queues<'a> {
    out: &'a mut Out,
    out_full: &'a mut u32,
    air: &'a mut Option<&'static mut crate::aero::AirQueue>,
    air_full: &'a mut u32,
}

impl Queues<'_> {
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
