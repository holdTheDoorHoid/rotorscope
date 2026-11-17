//! The wired bus, node side (feature `wirebus`; docs/WIREBUS.md).
//!
//! Many nodes share one hub UART: the hub's TX goes to every node's RX, and
//! every node's TX goes onto one shared return wire with one pull-up at the
//! hub. [`run`] is the whole node end: it reads the hub's commands, answers
//! when polled with the frames its image queued, joins the bus when it has
//! no address, and hands `Assign` commands meant for this node to the image.
//! The protocol itself is `crates/wirebus` (host-tested); this file only
//! moves bytes and reads the clock.
//!
//! **Pins (XIAO ESP32-C5), the same in every image:**
//! - RX = **D7 (GPIO12)**, from the hub's TX wire, exactly as a
//!   point-to-point wired node.
//! - TX = **D9 (GPIO9)**, *open-drain*, onto the shared return wire. Not D6
//!   (GPIO11), the point-to-point TX pin: GPIO11 is UART0's default TX, and
//!   the chip's ROM prints its boot banner there, push-pull, at every reset;
//!   on a shared wire that would garble other nodes' answers each time any
//!   node reboots or switches image. GPIO9 is not a strapping pin on the C5
//!   and no image uses it (node-sdr's capture pads are 0-5, 25 and 27).
//!
//! **Open drain.** esp-hal 1.2 on the C5 supports it: the pin is set up as an
//! `Output` with `DriveMode::OpenDrain` and handed to the UART as a frozen
//! output signal, so the UART cannot switch it back to push-pull. The pin
//! then only ever pulls the wire low (the 0 bits of a byte); between bytes
//! and whenever the node is silent it lets go, and the hub's pull-up holds
//! the wire high. A node can therefore never fight another node's low with
//! a high. It still could garble the line by talking out of turn, which the
//! protocol rules out: it sends only when polled by its own address (and
//! only if no newer command has arrived) or in a join slot it picked.
//!
//! **Timing.** Answers must start within the hub's first-byte window
//! (4 ms), so the firmware runs [`run`] on an interrupt-mode executor above
//! the radio work (see each image's `bus_task`). A node that is late anyway
//! sees the hub's next command before it starts and stays silent.

use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_time::{Instant, with_deadline};
use esp_hal::Async;
use esp_hal::gpio::interconnect::PeripheralInput;
use esp_hal::gpio::{DriveMode, Level, Output, OutputConfig, OutputPin, Pull};
use esp_hal::uart::{Config, Instance, Uart, UartTx};
use link::Assignment;
pub use wirebus::{NodeCounters, Outbox};
use wirebus::{Node, NodeConfig};

/// Line speed; must match the hub.
pub const BAUD: u32 = wirebus::BAUD;

/// The station MAC: the identity the hub keys this node on, the same in
/// every image on the board (and the same address an ESP-NOW build sends
/// from, so a node keeps its identity moving from ESP-NOW to the wire).
pub fn station_mac() -> [u8; 6] {
    let m = esp_hal::efuse::interface_mac_address(esp_hal::efuse::InterfaceMacAddress::Station);
    let mut out = [0u8; 6];
    out.copy_from_slice(&m.as_bytes()[..6]);
    out
}

/// The bus UART: TX open-drain on `tx`, RX on `rx`, 921600 8N1.
pub fn uart<'d>(uart: impl Instance + 'd, tx: impl OutputPin + 'd, rx: impl PeripheralInput<'d>) -> Uart<'d, Async> {
    let cfg = OutputConfig::default().with_drive_mode(DriveMode::OpenDrain).with_pull(Pull::None);
    // High = released: nothing is driven until the UART sends a 0 bit.
    let tx = Output::new(tx, Level::High, cfg);
    Uart::new(uart, Config::default().with_baudrate(BAUD))
        .expect("bus UART config")
        .with_tx(tx.into_peripheral_output())
        .with_rx(rx)
        .into_async()
}

/// One unit for [`Staged`]: an encoded frame (or a `Via` and its frame).
pub type Unit = heapless::Vec<u8, { wirebus::MAX_UNIT }>;

/// An [`Outbox`] over any "give me the next encoded frame" source (an
/// image's hub queue), holding one frame while it waits for room.
pub struct Staged<F> {
    next: F,
    held: Option<Unit>,
}

impl<F: FnMut(&mut Unit) -> bool> Staged<F> {
    /// `next` fills the vector with one whole encoded frame and returns
    /// true, or returns false when nothing is queued.
    pub const fn new(next: F) -> Self {
        Self { next, held: None }
    }
}

impl<F: FnMut(&mut Unit) -> bool> Outbox for Staged<F> {
    fn peek(&mut self) -> Option<&[u8]> {
        if self.held.is_none() {
            let mut v = heapless::Vec::new();
            if (self.next)(&mut v) && !v.is_empty() {
                self.held = Some(v);
            }
        }
        self.held.as_deref()
    }

    fn pop(&mut self) {
        self.held = None;
    }
}

/// The node's state for its USB status line.
static STATE: Mutex<Cell<(Option<u8>, NodeCounters)>> = Mutex::new(Cell::new((None, NodeCounters {
    polls: 0,
    answers: 0,
    resends: 0,
    empty: 0,
    stale: 0,
    joins: 0,
    granted: 0,
    lost: 0,
    commands: 0,
    bad: 0,
    frames: 0,
    bytes: 0,
    oversize: 0,
})));
/// UART receive errors (framing, FIFO overflow) and failed writes.
pub static RX_ERR: AtomicU32 = AtomicU32::new(0);
pub static TX_ERR: AtomicU32 = AtomicU32::new(0);

/// The bus address held now and the running counters.
pub fn snapshot() -> (Option<u8>, NodeCounters) {
    critical_section::with(|cs| STATE.borrow(cs).get())
}

/// One JSON object for the image's USB stream, e.g. once a second:
/// `{"t":"bus","addr":3,"polls":..,"answers":..,..}`.
pub fn json_line() -> heapless::String<320> {
    let (addr, c) = snapshot();
    let mut s = heapless::String::new();
    let _ = core::fmt::write(
        &mut s,
        format_args!(
            r#"{{"t":"bus","addr":{},"polls":{},"answers":{},"resends":{},"empty":{},"stale":{},"joins":{},"granted":{},"lost":{},"cmds":{},"bad":{},"frames":{},"bytes":{},"rx_err":{},"tx_err":{}}}"#,
            addr.unwrap_or(0),
            c.polls,
            c.answers,
            c.resends,
            c.empty,
            c.stale,
            c.joins,
            c.granted,
            c.lost,
            c.commands,
            c.bad,
            c.frames,
            c.bytes,
            RX_ERR.load(Relaxed),
            TX_ERR.load(Relaxed),
        ),
    );
    s
}

fn now_us() -> u64 {
    Instant::now().as_micros()
}

async fn send(tx: &mut UartTx<'_, Async>, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match tx.write_async(bytes).await {
            Ok(n) => bytes = &bytes[n..],
            Err(_) => {
                TX_ERR.fetch_add(1, Relaxed);
                return;
            }
        }
    }
    // Wait until the last stop bit is out, so nothing else starts early.
    if tx.flush_async().await.is_err() {
        TX_ERR.fetch_add(1, Relaxed);
    }
}

/// The node end of the bus, forever. `outbox` is the image's hub queue;
/// `on_assign` gets each `Assign` addressed to this node (`To` + `Assign`).
pub async fn run(uart: Uart<'static, Async>, outbox: &mut impl Outbox, on_assign: &mut impl FnMut(Assignment)) -> ! {
    let seed = esp_hal::rng::Rng::new().random();
    let (mut rx, mut tx) = uart.split();
    let mut node = Node::new(NodeConfig::new(), station_mac(), seed, 0);
    let mut buf = [0u8; 128];
    loop {
        critical_section::with(|cs| STATE.borrow(cs).set((node.addr(), *node.counters())));
        if let Some(b) = node.tick(now_us()) {
            send(&mut tx, b).await;
        }
        let read = match node.next_deadline() {
            Some(at) => with_deadline(Instant::from_micros(at), rx.read_async(&mut buf)).await.ok(),
            None => Some(rx.read_async(&mut buf).await),
        };
        let n = match read {
            None => continue, // a deadline: `tick` handles it
            Some(Ok(n)) => n,
            Some(Err(_)) => {
                RX_ERR.fetch_add(1, Relaxed);
                continue;
            }
        };
        let mut ready = node.on_rx(now_us(), &buf[..n], outbox, on_assign).is_some();
        // Bytes that came in meanwhile are newer than the poll: if the hub
        // has moved on, `on_rx` withdraws the answer and we stay silent.
        loop {
            match rx.read_buffered(&mut buf) {
                Ok(0) => break,
                Ok(m) => ready = node.on_rx(now_us(), &buf[..m], outbox, on_assign).is_some(),
                Err(_) => {
                    RX_ERR.fetch_add(1, Relaxed);
                    break;
                }
            }
        }
        if ready {
            if let Some(a) = node.answer() {
                send(&mut tx, a).await;
            }
            node.sent();
        }
    }
}
