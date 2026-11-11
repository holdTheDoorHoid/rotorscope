//! The CC13x2/CC26x2 ROM serial bootloader protocol (TI SWCU185,
//! "Bootloader" chapter), host side.
//!
//! - Every packet is `[size][checksum][cmd][data...]`; `size` counts all of
//!   it (so 3 + data), `checksum` is the low byte of the sum of `cmd` and
//!   `data`. Multi-byte parameters are big-endian on the wire.
//! - The target answers each packet with ACK `00 CC` or NACK `00 33`, and
//!   may send zero bytes before either. Commands that return data then send
//!   a packet `[size][checksum][data...]`, which the host ACKs.
//! - After reset the bootloader autobauds on `55 55` and ACKs it.
//! - Most commands leave a status that `GET_STATUS` returns (0x40 success).

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

pub const ACK: [u8; 2] = [0x00, 0xCC];
pub const NACK: [u8; 2] = [0x00, 0x33];
pub const SYNC: [u8; 2] = [0x55, 0x55];
/// Largest data field one packet carries (size byte 255 = 3 + 252).
pub const MAX_PAYLOAD: usize = 252;

pub mod cmd {
    pub const PING: u8 = 0x20;
    pub const DOWNLOAD: u8 = 0x21;
    pub const GET_STATUS: u8 = 0x23;
    pub const SEND_DATA: u8 = 0x24;
    pub const RESET: u8 = 0x25;
    pub const SECTOR_ERASE: u8 = 0x26;
    pub const CRC32: u8 = 0x27;
    pub const GET_CHIP_ID: u8 = 0x28;
    pub const MEMORY_READ: u8 = 0x2A;
    pub const BANK_ERASE: u8 = 0x2C;
    /// Not used by this tool: we never change CCFG fields in place.
    pub const SET_CCFG: u8 = 0x2D;
}

pub mod status {
    pub const SUCCESS: u8 = 0x40;
    pub const UNKNOWN_CMD: u8 = 0x41;
    pub const INVALID_CMD: u8 = 0x42;
    pub const INVALID_ADR: u8 = 0x43;
    pub const FLASH_FAIL: u8 = 0x44;

    pub fn name(s: u8) -> &'static str {
        match s {
            SUCCESS => "success",
            UNKNOWN_CMD => "unknown command",
            INVALID_CMD => "invalid command (bad parameters)",
            INVALID_ADR => "invalid address",
            FLASH_FAIL => "flash operation failed",
            _ => "unrecognised status",
        }
    }
}

/// MEMORY_READ access types.
pub const ACCESS_8: u8 = 0;
pub const ACCESS_32: u8 = 1;
/// MEMORY_READ limits: the reply must fit one packet.
pub const MAX_READ_WORDS: u8 = 63;

/// The byte pipe to the bootloader plus the two modem lines that drive
/// reset and the backdoor pin. `asserted = true` means the same as
/// pyserial's `setDTR(True)` / `setRTS(True)`: the line is asserted (which
/// on a CP2102N output is electrically low). `serialport` uses the same
/// sense, so a sequence written for pyserial carries over value for value.
pub trait Link {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// One byte, or `None` if none arrives within `timeout`.
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>>;
    fn set_dtr(&mut self, asserted: bool) -> io::Result<()>;
    fn set_rts(&mut self, asserted: bool) -> io::Result<()>;
    fn discard_input(&mut self) -> io::Result<()>;
    fn delay(&mut self, d: Duration);
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Timeout(String),
    Nack(String),
    BadPacket(String),
    Status { what: String, status: u8 },
    Refused(String),
    Verify(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "serial port: {e}"),
            Error::Timeout(w) => write!(f, "no answer from the bootloader to {w}"),
            Error::Nack(w) => write!(f, "the bootloader rejected {w} (NACK)"),
            Error::BadPacket(w) => write!(f, "garbled reply: {w}"),
            Error::Status { what, status: s } => {
                write!(f, "{what} failed: status 0x{s:02X} ({})", status::name(*s))
            }
            Error::Refused(w) => write!(f, "refused: {w}"),
            Error::Verify(w) => write!(f, "verify failed: {w}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn checksum(cmd: u8, data: &[u8]) -> u8 {
    data.iter().fold(cmd, |s, &b| s.wrapping_add(b))
}

/// One command packet. Panics if `data` is over [`MAX_PAYLOAD`].
pub fn frame(cmd: u8, data: &[u8]) -> Vec<u8> {
    assert!(data.len() <= MAX_PAYLOAD, "bootloader packet too long");
    let mut p = Vec::with_capacity(3 + data.len());
    p.push((3 + data.len()) as u8);
    p.push(checksum(cmd, data));
    p.push(cmd);
    p.extend_from_slice(data);
    p
}

fn be(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

const ACK_TIMEOUT: Duration = Duration::from_secs(2);
/// Bank erase and a CRC over all of flash take a while.
const SLOW_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Bsl<L> {
    link: L,
}

impl<L: Link> Bsl<L> {
    pub fn new(link: L) -> Self {
        Bsl { link }
    }

    pub fn link(&mut self) -> &mut L {
        &mut self.link
    }

    pub fn into_link(self) -> L {
        self.link
    }

    /// Reset into the ROM bootloader on a Sonoff Zigbee 3.0 USB Dongle Plus.
    /// DTR and RTS reach !RESET and DIO15 (the BSL pin) through a
    /// two-transistor gate: DTR=0,RTS=1 holds the chip in reset, DTR=1,RTS=0
    /// pulls DIO15 low, and equal values leave both high. The steps are the
    /// ones cc2538-bsl's `--sonoff` mode uses: hold reset, release it while
    /// pulling DIO15 low, give the ROM 2 ms to sample the pin, let go. (The
    /// comment table in cc2538-bsl names the two outputs the other way
    /// round; the sequence is what matters and is the same.) It ends with
    /// both lines asserted, which leaves reset and DIO15 high.
    pub fn enter_bootloader_sonoff(&mut self) -> Result<()> {
        let l = &mut self.link;
        l.set_dtr(false)?;
        l.set_rts(true)?;
        l.set_dtr(true)?;
        l.set_rts(false)?;
        l.delay(Duration::from_millis(2));
        l.set_rts(true)?;
        // Let the ROM start before talking to it.
        l.delay(Duration::from_millis(100));
        Ok(())
    }

    /// Restart into the firmware with the modem lines alone: pulse RTS
    /// with DTR deasserted (reset low, DIO15 high), then release.
    pub fn reset_by_pins_sonoff(&mut self) -> Result<()> {
        let l = &mut self.link;
        l.set_dtr(false)?;
        l.set_rts(true)?;
        l.delay(Duration::from_millis(10));
        l.set_rts(false)?;
        Ok(())
    }

    fn wait_ack(&mut self, what: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut prev: Option<u8> = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(b) = self.link.read_byte(left)? else {
                return Err(Error::Timeout(what.into()));
            };
            if prev == Some(0) {
                match b {
                    0xCC => return Ok(()),
                    0x33 => return Err(Error::Nack(what.into())),
                    _ => {}
                }
            }
            prev = Some(b);
        }
    }

    /// Autobaud. Only valid once per reset.
    pub fn sync(&mut self) -> Result<()> {
        self.link.discard_input()?;
        self.link.write_all(&SYNC)?;
        self.wait_ack("sync (55 55)", ACK_TIMEOUT)
    }

    fn send(&mut self, c: u8, data: &[u8], what: &str, timeout: Duration) -> Result<()> {
        self.link.write_all(&frame(c, data))?;
        self.wait_ack(what, timeout)
    }

    /// Read one reply packet and ACK it (NACK it if the checksum is bad).
    fn receive(&mut self, what: &str, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let next = |link: &mut L| -> Result<u8> {
            let left = deadline.saturating_duration_since(Instant::now());
            link.read_byte(left)?
                .ok_or_else(|| Error::Timeout(format!("{what} (reply)")))
        };
        // Zero bytes may precede the packet.
        let size = loop {
            let b = next(&mut self.link)?;
            if b != 0 {
                break b;
            }
        };
        if size < 2 {
            return Err(Error::BadPacket(format!("{what}: size byte {size}")));
        }
        let sum = next(&mut self.link)?;
        let mut data = Vec::with_capacity(usize::from(size) - 2);
        for _ in 2..size {
            data.push(next(&mut self.link)?);
        }
        if checksum(0, &data) != sum {
            self.link.write_all(&NACK)?;
            return Err(Error::BadPacket(format!("{what}: reply checksum mismatch")));
        }
        self.link.write_all(&ACK)?;
        Ok(data)
    }

    pub fn get_status(&mut self) -> Result<u8> {
        self.send(cmd::GET_STATUS, &[], "GET_STATUS", ACK_TIMEOUT)?;
        match self.receive("GET_STATUS", ACK_TIMEOUT)?[..] {
            [s] => Ok(s),
            ref other => Err(Error::BadPacket(format!(
                "GET_STATUS returned {} bytes",
                other.len()
            ))),
        }
    }

    fn check(&mut self, what: &str) -> Result<()> {
        match self.get_status()? {
            status::SUCCESS => Ok(()),
            s => Err(Error::Status {
                what: what.into(),
                status: s,
            }),
        }
    }

    pub fn ping(&mut self) -> Result<()> {
        self.send(cmd::PING, &[], "PING", ACK_TIMEOUT)?;
        self.check("PING")
    }

    /// GET_CHIP_ID: four bytes, most significant first.
    pub fn chip_id(&mut self) -> Result<u32> {
        self.send(cmd::GET_CHIP_ID, &[], "GET_CHIP_ID", ACK_TIMEOUT)?;
        let d = self.receive("GET_CHIP_ID", ACK_TIMEOUT)?;
        self.check("GET_CHIP_ID")?;
        let b: [u8; 4] = d
            .try_into()
            .map_err(|_| Error::BadPacket("GET_CHIP_ID: not 4 bytes".into()))?;
        Ok(u32::from_be_bytes(b))
    }

    /// MEMORY_READ, CC26xx form: `[addr:4][access type:1][count:1]`. With
    /// 32-bit access it returns `4 * words` bytes in memory order.
    pub fn read_words(&mut self, addr: u32, words: u8) -> Result<Vec<u8>> {
        assert!((1..=MAX_READ_WORDS).contains(&words));
        let mut d = be(&[addr]);
        d.extend_from_slice(&[ACCESS_32, words]);
        let what = format!("MEMORY_READ at 0x{addr:08X}");
        self.send(cmd::MEMORY_READ, &d, &what, ACK_TIMEOUT)?;
        let data = self.receive(&what, ACK_TIMEOUT)?;
        self.check(&what)?;
        if data.len() != 4 * usize::from(words) {
            return Err(Error::BadPacket(format!(
                "{what}: {} bytes, wanted {}",
                data.len(),
                4 * words
            )));
        }
        Ok(data)
    }

    pub fn read_u32(&mut self, addr: u32) -> Result<u32> {
        let d = self.read_words(addr, 1)?;
        Ok(u32::from_le_bytes(d[..4].try_into().unwrap()))
    }

    /// CRC32, CC26xx form: `[addr:4][size:4][read repeat count:4]`; the
    /// repeat count is 0 (read each location once).
    pub fn crc32(&mut self, addr: u32, size: u32) -> Result<u32> {
        let what = format!("CRC32 over 0x{addr:X}+0x{size:X}");
        self.send(cmd::CRC32, &be(&[addr, size, 0]), &what, SLOW_TIMEOUT)?;
        let d = self.receive(&what, SLOW_TIMEOUT)?;
        self.check(&what)?;
        let b: [u8; 4] = d
            .try_into()
            .map_err(|_| Error::BadPacket("CRC32: not 4 bytes".into()))?;
        Ok(u32::from_be_bytes(b))
    }

    /// Erase all of main flash, CCFG included.
    pub fn bank_erase(&mut self) -> Result<()> {
        self.send(cmd::BANK_ERASE, &[], "BANK_ERASE", SLOW_TIMEOUT)?;
        self.check("BANK_ERASE")
    }

    /// Erase one sector; CC26xx form takes the sector address only.
    pub fn sector_erase(&mut self, addr: u32) -> Result<()> {
        let what = format!("SECTOR_ERASE at 0x{addr:X}");
        self.send(cmd::SECTOR_ERASE, &be(&[addr]), &what, SLOW_TIMEOUT)?;
        self.check(&what)
    }

    /// Announce a program run of `size` bytes (a multiple of 4) at `addr`.
    pub fn download(&mut self, addr: u32, size: u32) -> Result<()> {
        assert!(
            size.is_multiple_of(4),
            "DOWNLOAD size must be a multiple of 4"
        );
        let what = format!("DOWNLOAD 0x{addr:X}+0x{size:X}");
        self.send(cmd::DOWNLOAD, &be(&[addr, size]), &what, ACK_TIMEOUT)?;
        self.check(&what)
    }

    /// Program the next bytes of the current DOWNLOAD run.
    pub fn send_data(&mut self, data: &[u8]) -> Result<()> {
        self.send(cmd::SEND_DATA, data, "SEND_DATA", SLOW_TIMEOUT)?;
        self.check("SEND_DATA")
    }

    /// Software reset. The chip boots whatever the CCFG and pins say.
    pub fn reset(&mut self) -> Result<()> {
        self.send(cmd::RESET, &[], "RESET", ACK_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        assert_eq!(frame(cmd::PING, &[]), vec![3, 0x20, 0x20]);
        // DOWNLOAD 0x00000000 + 0x58000: checksum = 0x21 + 0x05 + 0x80.
        assert_eq!(
            frame(cmd::DOWNLOAD, &be(&[0, 0x58000])),
            vec![11, 0xA6, 0x21, 0, 0, 0, 0, 0, 0x05, 0x80, 0x00]
        );
        // The CC26xx MEMORY_READ checksum cc2538-bsl writes as
        // `cmd + sum(addr) + 2` (access 1 + count 1).
        let d = [0x50, 0x00, 0x13, 0x18, ACCESS_32, 1];
        assert_eq!(
            frame(cmd::MEMORY_READ, &d)[1],
            (0x2Au8).wrapping_add(0x50 + 0x13 + 0x18 + 2)
        );
        assert_eq!(frame(cmd::SEND_DATA, &[0xFF; 252])[0], 255);
    }

    #[test]
    #[should_panic]
    fn oversize_frame() {
        frame(cmd::SEND_DATA, &[0; 253]);
    }

    #[test]
    fn checksum_wraps() {
        assert_eq!(checksum(0xFF, &[0x02]), 0x01);
        assert_eq!(checksum(0, &[]), 0);
    }
}
