//! An in-process CC26x2 ROM bootloader, for tests. It speaks the same
//! protocol the host does, keeps a flash array with NOR semantics (writes
//! can only clear bits, so a missed erase shows up), honours the CCFG's
//! erase and write-protect bits, and models the Sonoff's DTR/RTS gate.
//!
//! Pin model (ESP-style two-transistor gate, logical "asserted" values as
//! in [`Link`]): DTR=0,RTS=1 holds !RESET low; DTR=1,RTS=0 pulls DIO15
//! low; anything else leaves both high. The reset line has an RC delay, so
//! the ROM samples DIO15 [`BOOT_SAMPLE_US`] after reset is released. Time
//! advances only through [`Link::delay`].

use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::Duration;

use crate::ccfg::{CCFG_LEN, Ccfg};
use crate::crc::crc32;
use crate::proto::{ACK, Link, NACK, cmd, status};

pub const BOOT_SAMPLE_US: u64 = 1_000;
pub const SECTOR: usize = 0x2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Held in reset.
    Reset,
    /// Released, ROM has not sampled DIO15 yet.
    Booting { at_us: u64 },
    /// Running the flashed firmware: serial input is ignored.
    App,
    /// ROM bootloader waiting for 55 55.
    Unsynced,
    /// ROM bootloader taking commands.
    Ready,
}

pub struct Sim {
    pub flash: Vec<u8>,
    pub chip_id: u32,
    /// 32-bit registers MEMORY_READ can reach outside flash.
    pub regs: HashMap<u32, u32>,
    pub mode: Mode,
    dtr: bool,
    rts: bool,
    now_us: u64,
    inbuf: Vec<u8>,
    out: VecDeque<u8>,
    awaiting_host_ack: bool,
    last_status: u8,
    download: Option<(u32, u32)>,
    /// Every command byte executed, in order.
    pub commands: Vec<u8>,
    pub boots_to_bootloader: usize,
    pub boots_to_app: usize,
    pub host_nacks: usize,
    /// Fault: the Nth SEND_DATA (0-based, counted over the sim's life)
    /// fails with FLASH_FAIL.
    pub fail_send_data_at: Option<usize>,
    send_data_count: usize,
    /// Fault: the next N CRC32 replies are off by one.
    pub corrupt_crcs: usize,
    /// Fault: the next N reply packets carry a wrong checksum.
    pub corrupt_packets: usize,
    /// Fault: refuse BANK_ERASE regardless of CCFG.
    pub refuse_bank_erase: bool,
}

impl Sim {
    /// A chip with `flash` as its contents, running its firmware.
    pub fn new(flash: Vec<u8>) -> Sim {
        assert!(flash.len().is_multiple_of(SECTOR));
        let sectors = (flash.len() / SECTOR) as u32;
        let regs = HashMap::from([
            // ICEPICK_DEVICE_ID: PG 3, wafer 0xBB41 (CC13x2/CC26x2).
            (0x5000_1318, 0x3BB4_102F),
            // FCFG1 USER_ID.
            (0x5000_1294, 0x2084_0000),
            // FLASH.FLASH_SIZE: sectors.
            (0x4003_002C, sectors),
            // Primary IEEE address.
            (0x5000_12F0, 0x4433_2211),
            (0x5000_12F4, 0x00124B00),
        ]);
        Sim {
            flash,
            chip_id: 0x3102_0000,
            regs,
            mode: Mode::App,
            dtr: true,
            rts: true,
            now_us: 0,
            inbuf: Vec::new(),
            out: VecDeque::new(),
            awaiting_host_ack: false,
            last_status: status::SUCCESS,
            download: None,
            commands: Vec::new(),
            boots_to_bootloader: 0,
            boots_to_app: 0,
            host_nacks: 0,
            fail_send_data_at: None,
            send_data_count: 0,
            corrupt_crcs: 0,
            corrupt_packets: 0,
            refuse_bank_erase: false,
        }
    }

    fn ccfg(&self) -> Ccfg {
        Ccfg::from_image(&self.flash).unwrap()
    }

    fn reset_low(&self) -> bool {
        !self.dtr && self.rts
    }
    fn bsl_low(&self) -> bool {
        self.dtr && !self.rts
    }

    fn pins_changed(&mut self) {
        if self.reset_low() {
            self.mode = Mode::Reset;
            self.inbuf.clear();
            self.out.clear();
            self.awaiting_host_ack = false;
            self.download = None;
        } else if self.mode == Mode::Reset {
            self.mode = Mode::Booting {
                at_us: self.now_us + BOOT_SAMPLE_US,
            };
        }
    }

    /// The ROM's decision at boot: bootloader if the image is not marked
    /// valid, or if the CCFG backdoor is enabled and its pin is at its
    /// active level.
    fn boot(&mut self) {
        let c = self.ccfg();
        let backdoor = c.bootloader_enable() == 0xC5
            && c.backdoor_enable() == 0xC5
            && c.backdoor_pin() == 15
            && (self.bsl_low() != c.backdoor_active_high());
        if c.image_valid != 0 || backdoor {
            self.mode = Mode::Unsynced;
            self.boots_to_bootloader += 1;
        } else {
            self.mode = Mode::App;
            self.boots_to_app += 1;
        }
        self.inbuf.clear();
        self.out.clear();
        self.download = None;
        self.last_status = status::SUCCESS;
    }

    fn tick(&mut self) {
        if let Mode::Booting { at_us } = self.mode
            && self.now_us >= at_us
        {
            self.boot();
        }
    }

    fn reply(&mut self, data: &[u8]) {
        let mut sum = data.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        if self.corrupt_packets > 0 {
            self.corrupt_packets -= 1;
            sum = sum.wrapping_add(1);
        }
        // A real target may pad with zeros first; do it to exercise that.
        self.out.push_back(0);
        self.out.push_back((data.len() + 2) as u8);
        self.out.push_back(sum);
        self.out.extend(data.iter().copied());
        self.awaiting_host_ack = true;
    }

    fn process(&mut self) {
        loop {
            if self.awaiting_host_ack {
                if self.inbuf.len() < 2 {
                    return;
                }
                let a: Vec<u8> = self.inbuf.drain(..2).collect();
                if a != ACK {
                    self.host_nacks += 1;
                }
                self.awaiting_host_ack = false;
                continue;
            }
            match self.mode {
                Mode::Reset | Mode::Booting { .. } | Mode::App => {
                    self.inbuf.clear();
                    return;
                }
                Mode::Unsynced => {
                    let Some(i) = self.inbuf.windows(2).position(|w| w == [0x55, 0x55]) else {
                        return;
                    };
                    self.inbuf.drain(..i + 2);
                    self.out.extend(ACK);
                    self.mode = Mode::Ready;
                }
                Mode::Ready => {
                    let Some(&size) = self.inbuf.first() else {
                        return;
                    };
                    let size = usize::from(size);
                    if size < 3 {
                        self.inbuf.remove(0);
                        continue;
                    }
                    if self.inbuf.len() < size {
                        return;
                    }
                    let p: Vec<u8> = self.inbuf.drain(..size).collect();
                    let (sum, c, data) = (p[1], p[2], &p[3..]);
                    if data.iter().fold(c, |s, &b| s.wrapping_add(b)) != sum {
                        self.out.extend(NACK);
                        continue;
                    }
                    self.out.extend(ACK);
                    self.execute(c, data);
                }
            }
        }
    }

    fn be32(d: &[u8], i: usize) -> u32 {
        u32::from_be_bytes(d[4 * i..4 * i + 4].try_into().unwrap())
    }

    fn flash_range(&self, addr: u32, len: u32) -> Option<std::ops::Range<usize>> {
        let (a, l) = (addr as usize, len as usize);
        (a.checked_add(l)? <= self.flash.len()).then_some(a..a + l)
    }

    fn execute(&mut self, c: u8, d: &[u8]) {
        self.commands.push(c);
        let st = match (c, d.len()) {
            (cmd::GET_STATUS, 0) => {
                let s = self.last_status;
                self.reply(&[s]);
                return;
            }
            (cmd::PING, 0) => status::SUCCESS,
            (cmd::GET_CHIP_ID, 0) => {
                let id = self.chip_id.to_be_bytes();
                self.reply(&id);
                status::SUCCESS
            }
            (cmd::RESET, 0) => {
                // Software reset: the pins are whatever the host left them.
                self.awaiting_host_ack = false;
                self.boot();
                // boot() clears the output queue; the ACK went out first.
                self.out.extend(ACK);
                return;
            }
            (cmd::MEMORY_READ, 6) => {
                let (addr, access, count) = (Self::be32(d, 0), d[4], d[5]);
                let (width, max) = if access == 1 { (4, 63) } else { (1, 253) };
                let len = u32::from(count) * width;
                if access > 1 || count == 0 || count > max {
                    status::INVALID_CMD
                } else if let Some(r) = self.flash_range(addr, len) {
                    let v = self.flash[r].to_vec();
                    self.reply(&v);
                    status::SUCCESS
                } else if access == 1 && count == 1 && self.regs.contains_key(&addr) {
                    let v = self.regs[&addr].to_le_bytes();
                    self.reply(&v);
                    status::SUCCESS
                } else {
                    status::INVALID_ADR
                }
            }
            (cmd::CRC32, 12) => {
                let (addr, len) = (Self::be32(d, 0), Self::be32(d, 1));
                match self.flash_range(addr, len) {
                    Some(r) => {
                        let mut crc = crc32(&self.flash[r]);
                        if self.corrupt_crcs > 0 {
                            self.corrupt_crcs -= 1;
                            crc ^= 1;
                        }
                        self.reply(&crc.to_be_bytes());
                        status::SUCCESS
                    }
                    None => status::INVALID_ADR,
                }
            }
            (cmd::BANK_ERASE, 0) => {
                let c = self.ccfg();
                let sectors = (self.flash.len() / SECTOR) as u32;
                if self.refuse_bank_erase
                    || !c.bank_erase_allowed()
                    || (0..sectors).any(|s| c.sector_protected(s))
                {
                    status::FLASH_FAIL
                } else {
                    self.flash.fill(0xFF);
                    // As the real ROM does (seen on a CC2652P): keep the
                    // bootloader enabled and debug access open.
                    let base = self.flash.len() - CCFG_LEN;
                    self.flash[base + 0x33] = 0xC5; // BL_CONFIG.BOOTLOADER_ENABLE
                    self.flash[base + 0x38] = 0xC5; // CCFG_TI_OPTIONS.TI_FA_ENABLE
                    for off in [0x3C, 0x40] {
                        // CCFG_TAP_DAP_0/1: C5 C5 C5 FF
                        self.flash[base + off..base + off + 3].fill(0xC5);
                    }
                    status::SUCCESS
                }
            }
            (cmd::SECTOR_ERASE, 4) => {
                let addr = Self::be32(d, 0) as usize;
                if !addr.is_multiple_of(SECTOR) || addr >= self.flash.len() {
                    status::INVALID_ADR
                } else if self.ccfg().sector_protected((addr / SECTOR) as u32) {
                    status::FLASH_FAIL
                } else {
                    self.flash[addr..addr + SECTOR].fill(0xFF);
                    status::SUCCESS
                }
            }
            (cmd::DOWNLOAD, 8) => {
                let (addr, len) = (Self::be32(d, 0), Self::be32(d, 1));
                if len % 4 != 0 || self.flash_range(addr, len).is_none() {
                    self.download = None;
                    status::INVALID_ADR
                } else {
                    self.download = Some((addr, len));
                    status::SUCCESS
                }
            }
            (cmd::SEND_DATA, n) if n > 0 => {
                let n_sent = self.send_data_count;
                self.send_data_count += 1;
                match self.download {
                    None => status::INVALID_CMD,
                    Some((_, left)) if n as u32 > left => status::INVALID_ADR,
                    Some(_) if self.fail_send_data_at == Some(n_sent) => status::FLASH_FAIL,
                    Some((addr, left)) => {
                        for (i, &b) in d.iter().enumerate() {
                            // NOR flash: programming only clears bits.
                            self.flash[addr as usize + i] &= b;
                        }
                        let left = left - n as u32;
                        self.download = (left > 0).then_some((addr + n as u32, left));
                        status::SUCCESS
                    }
                }
            }
            _ => status::UNKNOWN_CMD,
        };
        self.last_status = st;
    }

    /// The CCFG block as the chip currently holds it.
    pub fn ccfg_block(&self) -> &[u8] {
        &self.flash[self.flash.len() - CCFG_LEN..]
    }
}

impl Link for Sim {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tick();
        self.inbuf.extend_from_slice(bytes);
        self.process();
        Ok(())
    }

    fn read_byte(&mut self, _timeout: Duration) -> io::Result<Option<u8>> {
        self.tick();
        Ok(self.out.pop_front())
    }

    fn set_dtr(&mut self, asserted: bool) -> io::Result<()> {
        self.tick();
        self.dtr = asserted;
        self.pins_changed();
        Ok(())
    }

    fn set_rts(&mut self, asserted: bool) -> io::Result<()> {
        self.tick();
        self.rts = asserted;
        self.pins_changed();
        Ok(())
    }

    fn discard_input(&mut self) -> io::Result<()> {
        self.out.clear();
        Ok(())
    }

    fn delay(&mut self, d: Duration) {
        self.now_us += d.as_micros() as u64;
        self.tick();
    }
}
