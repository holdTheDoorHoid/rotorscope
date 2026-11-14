//! Node links on the P4: the ESP-NOW gateway on UART3 (header UART3-IN,
//! RX = GPIO28), 921600 baud, `link` frames.
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
                    for &b in &buf[..n] {
                        if ring.push_back(b).is_err() {
                            RING_FULL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
                Err(_) => {
                    UART_ERR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    break;
                }
            }
        }
        uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
    });
}

pub const BAUD: u32 = 921_600;
const MAX_RELAYED: usize = 16;

#[derive(Clone, Copy, Default)]
pub struct NodeStats {
    pub mac: [u8; 6],
    pub rssi: i8,
    pub frames: u32,
    pub detections: u32,
    pub hello: bool,
    /// From the node's Hello.
    pub role: Option<link::NodeRole>,
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
}

impl<'d> GatewayLink<'d> {
    pub fn new(uart: impl esp_hal::uart::Instance + 'static, rx_pin: impl esp_hal::gpio::interconnect::PeripheralInput<'static>) -> Self {
        let cfg = Config::default()
            .with_baudrate(BAUD)
            .with_rx(RxConfig::default().with_fifo_full_threshold(64).with_timeout(10));
        let mut uart = Uart::new(uart, cfg).expect("UART3 config").with_rx(rx_pin);
        uart.set_interrupt_handler(on_uart);
        critical_section::with(|cs| {
            uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
            uart.listen(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
            SERIAL.borrow_ref_mut(cs).replace(uart);
        });
        Self { _uart: core::marker::PhantomData, dec: Decoder::new(), via: None, stats: LinkStats::default(), nodes: heapless::Vec::new(), out: Out::new(), out_full: 0 }
    }

    /// Drain whatever the UART has; returns frames handled.
    pub fn poll(&mut self, now_ms: u64) -> u32 {
        let mut buf = [0u8; 256];
        let mut handled = 0;
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
            let (stats, nodes, via, out, out_full) = (&mut self.stats, &mut self.nodes, &mut self.via, &mut self.out, &mut self.out_full);
            self.dec.feed(&buf[..n], |r| {
                handled += 1;
                match r {
                    Ok(f) => Self::on_frame(stats, nodes, via, out, out_full, now_ms, f),
                    Err(LinkError::Corrupt) => {
                        stats.corrupt += 1;
                        *via = None;
                    }
                    Err(LinkError::Overflow) => {
                        stats.overflow += 1;
                        *via = None;
                    }
                }
            });
        }
        handled
    }

    fn on_frame(
        stats: &mut LinkStats,
        nodes: &mut heapless::Vec<NodeStats, MAX_RELAYED>,
        via: &mut Option<([u8; 6], i8)>,
        out: &mut Out,
        out_full: &mut u32,
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
            if matches!(f, Frame::Detection(_) | Frame::OpenLink(_)) && out.push_back((0, f)).is_err() {
                *out_full += 1;
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
            Frame::Stats { uptime_us, rid_found, dropped, .. } => {
                n.rid_found = *rid_found;
                n.dropped = *dropped;
                n.uptime_s = (*uptime_us / 1_000_000) as u32;
            }
            _ => {}
        }
        if matches!(f, Frame::Detection(_) | Frame::OpenLink(_)) && out.push_back((i as u8 + 1, f)).is_err() {
            *out_full += 1;
        }
    }
}
