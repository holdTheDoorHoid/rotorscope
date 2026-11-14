//! Node links on the P4: the ESP-NOW gateway on UART3 (header UART3-IN,
//! RX = GPIO28), 921600 baud, `link` frames.
//!
//! A gateway link speaks for many nodes: a `Frame::Via { node, rssi }` names
//! the sender of the one frame after it. Frames without a `Via` belong to the
//! link itself (a wired node). Blocking and polled for now: the UART FIFO
//! (128 bytes) fills in 1.4 ms at 921600 baud, so `poll` must run at least
//! that often until the reader moves to an interrupt.

use esp_hal::uart::{Config, UartRx};
use esp_hal::Blocking;
use link::{Decoder, Frame, LinkError};

pub const BAUD: u32 = 921_600;
const MAX_RELAYED: usize = 16;

#[derive(Clone, Copy, Default)]
pub struct NodeStats {
    pub mac: [u8; 6],
    pub rssi: i8,
    pub frames: u32,
    pub detections: u32,
    pub hello: bool,
}

#[derive(Default)]
pub struct LinkStats {
    pub bytes: u32,
    pub frames: u32,
    pub corrupt: u32,
    pub overflow: u32,
    pub uart_err: u32,
    /// Frames with no `Via` before them: a node wired straight to this link.
    pub direct: u32,
}

pub struct GatewayLink<'d> {
    rx: UartRx<'d, Blocking>,
    dec: Decoder,
    via: Option<([u8; 6], i8)>,
    pub stats: LinkStats,
    pub nodes: heapless::Vec<NodeStats, MAX_RELAYED>,
}

impl<'d> GatewayLink<'d> {
    pub fn new(uart: impl esp_hal::uart::Instance + 'd, rx_pin: impl esp_hal::gpio::interconnect::PeripheralInput<'d>) -> Self {
        let rx = UartRx::new(uart, Config::default().with_baudrate(BAUD)).expect("UART3 config").with_rx(rx_pin);
        Self { rx, dec: Decoder::new(), via: None, stats: LinkStats::default(), nodes: heapless::Vec::new() }
    }

    /// Drain whatever the UART has; returns frames handled.
    pub fn poll(&mut self) -> u32 {
        let mut buf = [0u8; 128];
        let mut handled = 0;
        while self.rx.read_ready() {
            let n = match self.rx.read_buffered(&mut buf) {
                Ok(n) => n,
                Err(_) => {
                    self.stats.uart_err += 1;
                    continue;
                }
            };
            if n == 0 {
                break;
            }
            self.stats.bytes += n as u32;
            let (stats, nodes, via) = (&mut self.stats, &mut self.nodes, &mut self.via);
            self.dec.feed(&buf[..n], |r| {
                handled += 1;
                match r {
                    Ok(f) => Self::on_frame(stats, nodes, via, f),
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
        f: Frame,
    ) {
        stats.frames += 1;
        if let Frame::Via { node, rssi } = f {
            *via = Some((node, rssi));
            return;
        }
        let Some((mac, rssi)) = via.take() else {
            stats.direct += 1;
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
        match f {
            Frame::Detection(_) => n.detections += 1,
            Frame::Hello { .. } => n.hello = true,
            _ => {}
        }
    }
}
