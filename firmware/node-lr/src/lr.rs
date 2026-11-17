//! The thin LR2021 SPI driver: BUSY handshake, command writes, responses,
//! FIFO reads. Which bytes to send is decided in `crates/lr2021cfg` (host
//! tested); this file only moves them. Receive only: it has no way to fill
//! the TX FIFO or start a transmission.

use embassy_time::{Duration, Instant, Timer};
use esp_hal::Blocking;
use esp_hal::gpio::{Input, Output};
use esp_hal::spi::master::Spi;
use lr2021cfg::cmd;
use lr2021cfg::config::Seq;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// BUSY stayed high (no chip, or it is stuck).
    Busy,
    Spi,
}

pub struct Lr {
    spi: Spi<'static, Blocking>,
    nss: Output<'static>,
    busy: Input<'static>,
    rst: Output<'static>,
    pub irq: Input<'static>,
    /// Commands the chip reported as failed (status "fail"), for the status line.
    pub cmd_fails: u32,
}

impl Lr {
    pub fn new(spi: Spi<'static, Blocking>, nss: Output<'static>, busy: Input<'static>, rst: Output<'static>, irq: Input<'static>) -> Self {
        Self { spi, nss, busy, rst, irq, cmd_fails: 0 }
    }

    /// Hardware reset, then wait for the chip to come up.
    pub async fn reset(&mut self) {
        self.rst.set_low();
        Timer::after_millis(2).await;
        self.rst.set_high();
        Timer::after_millis(300).await;
    }

    fn wait_ready(&mut self, timeout: Duration) -> Result<(), Error> {
        let t0 = Instant::now();
        while self.busy.is_high() {
            if t0.elapsed() > timeout {
                return Err(Error::Busy);
            }
        }
        Ok(())
    }

    fn xfer(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        self.nss.set_low();
        let r = self.spi.transfer(buf).map_err(|_| Error::Spi);
        self.nss.set_high();
        r
    }

    /// One command. The bytes clocked back carry the status of the
    /// previous command; a "fail" there is counted.
    pub fn write(&mut self, c: &[u8]) -> Result<(), Error> {
        self.wait_ready(Duration::from_millis(100))?;
        let mut buf = [0u8; 16];
        let n = c.len().min(16);
        buf[..n].copy_from_slice(&c[..n]);
        self.xfer(&mut buf[..n])?;
        if cmd::cmd_status(&buf) == 0 && buf[0] != 0 {
            self.cmd_fails = self.cmd_fails.wrapping_add(1);
        }
        Ok(())
    }

    /// A command with a response: `rsp` is filled with status (2) + data.
    pub fn read(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<(), Error> {
        self.write(req)?;
        self.wait_ready(Duration::from_millis(5))?;
        rsp.fill(0);
        self.xfer(rsp)
    }

    /// `n` bytes of the RX FIFO into `out` (opcode, then the data, in one
    /// transaction).
    pub fn read_fifo(&mut self, out: &mut [u8]) -> Result<(), Error> {
        self.wait_ready(Duration::from_millis(5))?;
        out.fill(0);
        self.nss.set_low();
        let mut op = cmd::READ_RX_FIFO;
        let r = self.spi.transfer(&mut op).and_then(|_| self.spi.transfer(out)).map_err(|_| Error::Spi);
        self.nss.set_high();
        r
    }

    pub fn run(&mut self, seq: &Seq) -> Result<(), Error> {
        for c in seq {
            self.write(c)?;
        }
        Ok(())
    }

    /// GetVersion: (major, minor).
    pub fn version(&mut self) -> Result<(u8, u8), Error> {
        let mut rsp = [0u8; cmd::VERSION_RSP];
        self.read(&cmd::GET_VERSION, &mut rsp)?;
        Ok(cmd::parse_version(&rsp))
    }

    pub fn irq_status(&mut self) -> Result<u32, Error> {
        let mut rsp = [0u8; cmd::IRQ_RSP];
        self.read(&cmd::GET_AND_CLEAR_IRQ, &mut rsp)?;
        Ok(cmd::parse_irq(&rsp))
    }

    pub fn packet_len(&mut self) -> Result<u16, Error> {
        let mut rsp = [0u8; cmd::PKT_LEN_RSP];
        self.read(&cmd::GET_RX_PKT_LENGTH, &mut rsp)?;
        Ok(cmd::parse_pkt_len(&rsp))
    }

    pub fn packet_status(&mut self, req: &[u8; 2]) -> Result<[u8; cmd::PKT_STATUS_RSP], Error> {
        let mut rsp = [0u8; cmd::PKT_STATUS_RSP];
        self.read(req, &mut rsp)?;
        Ok(rsp)
    }

    /// Instantaneous RSSI, dBm (the control scan's energy sweeps and burst
    /// timing).
    pub fn rssi_inst(&mut self) -> Result<i8, Error> {
        let mut rsp = [0u8; cmd::RSSI_INST_RSP];
        self.read(&cmd::GET_RSSI_INST, &mut rsp)?;
        Ok(cmd::parse_rssi_inst(&rsp))
    }
}
