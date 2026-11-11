//! [`Link`] over a real serial port with the `serialport` crate.
//!
//! `serialport`'s `write_data_terminal_ready(true)` and
//! `write_request_to_send(true)` assert the line (TIOCMBIS on Linux), the
//! same as pyserial's `setDTR(True)` / `setRTS(True)`.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::time::Duration;

use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

use crate::proto::Link;

pub struct SerialLink {
    port: TTYPort,
    buf: VecDeque<u8>,
}

impl SerialLink {
    /// Open `path` 8N1, no flow control. The port is opened exclusively
    /// (TIOCEXCL), so nothing else can write to it while we flash.
    pub fn open(path: &str, baud: u32) -> serialport::Result<SerialLink> {
        let port = serialport::new(path, baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .timeout(Duration::from_millis(100))
            .open_native()?;
        Ok(SerialLink {
            port,
            buf: VecDeque::new(),
        })
    }
}

fn io_err(e: serialport::Error) -> io::Error {
    io::Error::other(e)
}

impl Link for SerialLink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.port.write_all(bytes)?;
        self.port.flush()
    }

    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        if let Some(b) = self.buf.pop_front() {
            return Ok(Some(b));
        }
        if timeout.is_zero() {
            return Ok(None);
        }
        self.port.set_timeout(timeout).map_err(io_err)?;
        let mut tmp = [0u8; 512];
        match self.port.read(&mut tmp) {
            Ok(0) => Ok(None),
            Ok(n) => {
                self.buf.extend(&tmp[..n]);
                Ok(self.buf.pop_front())
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn set_dtr(&mut self, asserted: bool) -> io::Result<()> {
        self.port
            .write_data_terminal_ready(asserted)
            .map_err(io_err)
    }

    fn set_rts(&mut self, asserted: bool) -> io::Result<()> {
        self.port.write_request_to_send(asserted).map_err(io_err)
    }

    fn discard_input(&mut self) -> io::Result<()> {
        self.buf.clear();
        self.port.clear(ClearBuffer::Input).map_err(io_err)
    }

    fn delay(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}
