//! Interrupt-driven receive rings for the slow serial inputs (GPS on UART1, wired to the UART3-IN header;
//! ADSBee on UART4), the same pattern as the gateway link (`links.rs`).
//!
//! The main loop can be away for ~7 ms (one screen band, or an SD block),
//! and a 128-byte FIFO lasts 11 ms at 115200 baud: just enough, with no
//! margin for a flush with read-back (up to 37 ms). So the UART interrupt
//! (FIFO 64 bytes full, or 10 idle symbols) moves bytes into a RAM ring and
//! the loop takes them out whenever it gets there.
//!
//! Each ring is its own module made by [`ring!`], because an esp-hal UART
//! interrupt handler is a plain `fn` with no arguments: it must find its
//! UART in a static.

macro_rules! ring {
    ($name:ident, $size:expr) => {
        pub mod $name {
            use core::cell::RefCell;
            use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

            use critical_section::Mutex;
            use esp_hal::uart::{Config, RxConfig, Uart, UartInterrupt};
            use esp_hal::{Blocking, handler, ram};

            pub const SIZE: usize = $size;
            static SERIAL: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
            static RX: Mutex<RefCell<heapless::Deque<u8, SIZE>>> = Mutex::new(RefCell::new(heapless::Deque::new()));
            /// Bytes received, lost to a full ring, and UART errors (framing,
            /// FIFO overflow).
            pub static BYTES: AtomicU32 = AtomicU32::new(0);
            pub static RING_FULL: AtomicU32 = AtomicU32::new(0);
            pub static UART_ERR: AtomicU32 = AtomicU32::new(0);

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
                                BYTES.fetch_add(n as u32, Relaxed);
                                for &b in &buf[..n] {
                                    if ring.push_back(b).is_err() {
                                        RING_FULL.fetch_add(1, Relaxed);
                                    }
                                }
                            }
                            Err(_) => {
                                UART_ERR.fetch_add(1, Relaxed);
                                break;
                            }
                        }
                    }
                    uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
                });
            }

            pub fn config(baud: u32) -> Config {
                Config::default()
                    .with_baudrate(baud)
                    .with_rx(RxConfig::default().with_fifo_full_threshold(64).with_timeout(10))
            }

            /// Take ownership of a configured UART and start receiving.
            pub fn install(mut uart: Uart<'static, Blocking>) {
                uart.set_interrupt_handler(on_uart);
                critical_section::with(|cs| {
                    uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
                    uart.listen(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
                    SERIAL.borrow_ref_mut(cs).replace(uart);
                });
            }

            /// Move received bytes into `buf`; returns how many.
            pub fn take(buf: &mut [u8]) -> usize {
                critical_section::with(|cs| {
                    let mut ring = RX.borrow_ref_mut(cs);
                    let mut n = 0;
                    while n < buf.len() {
                        let Some(b) = ring.pop_front() else { break };
                        buf[n] = b;
                        n += 1;
                    }
                    n
                })
            }

            /// Queue bytes on the TX FIFO without waiting: returns how many
            /// fitted. (GPS commands are a few dozen bytes; the FIFO is 128.)
            #[allow(dead_code)]
            pub fn write(bytes: &[u8]) -> usize {
                critical_section::with(|cs| {
                    let mut serial = SERIAL.borrow_ref_mut(cs);
                    let Some(uart) = serial.as_mut() else { return 0 };
                    let mut n = 0;
                    while n < bytes.len() && uart.write_ready() {
                        let end = (n + 32).min(bytes.len());
                        match uart.write(&bytes[n..end]) {
                            Ok(w) => n += w,
                            Err(_) => break,
                        }
                    }
                    n
                })
            }

            /// Bench only: write all of `bytes`, waiting for FIFO room (at most
            /// ~50 ms).
            #[allow(dead_code)]
            pub fn write_all(bytes: &[u8]) {
                let t = esp_hal::time::Instant::now();
                let mut n = 0;
                while n < bytes.len() && t.elapsed().as_millis() < 50 {
                    n += write(&bytes[n..]);
                }
            }

            /// Change the baud rate (and drop what was received at the old one).
            #[allow(dead_code)]
            pub fn set_baud(baud: u32) {
                critical_section::with(|cs| {
                    if let Some(uart) = SERIAL.borrow_ref_mut(cs).as_mut() {
                        let _ = uart.apply_config(&config(baud));
                    }
                    RX.borrow_ref_mut(cs).clear();
                });
            }

            /// Bench only: bytes as if they had arrived on the wire.
            #[allow(dead_code)]
            pub fn inject(bytes: &[u8]) {
                critical_section::with(|cs| {
                    let mut ring = RX.borrow_ref_mut(cs);
                    for &b in bytes {
                        let _ = ring.push_back(b);
                    }
                });
            }
        }
    };
}

// GPS: 115200 baud worst case ~1.2 KB/s of NMEA (dual-band GSV); 2 KB is
// over a second of it.
ring!(gps_rx, 2048);
// ADSBee: a busy sky is a few KB per second of CSBee text, in one burst
// each second. 4 KB holds a burst.
ring!(adsb_rx, 4096);
