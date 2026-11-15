//! SD card in **native (SD bus) mode, 1-bit**, bit-banged over three pins:
//! CLK, CMD and DAT0.
//!
//! Written for the ESP32-P4 hub (Elecrow CrowPanel Advanced 5"), whose card
//! slot is wired for SDMMC 1-bit only (no DAT1-3, no chip select, so SPI mode
//! is impossible) and whose pre-v3 chip has no SDMMC driver we can use. The
//! pins are abstracted by [`Pins`], so all of this runs on the host against a
//! simulated card (`tests/sim.rs`).
//!
//! What is implemented (SD Physical Layer Simplified Spec v9):
//! - command frames with CRC7, responses R1, R1b, R2, R3, R6, R7;
//! - initialisation at the slow clock: CMD0, CMD8, ACMD41 (HCS), CMD2, CMD3,
//!   CMD9, CMD7, ACMD6 (1-bit), CMD16 for standard-capacity cards;
//! - single and multiple block read (CMD17, CMD18 + CMD12) and write (CMD24,
//!   CMD25 + CMD12), data blocks with CRC16, the CRC status token and the
//!   busy signal on DAT0.
//!
//! **Timing.** The host changes CMD/DAT while CLK is low and the card samples
//! on the rising edge; the card changes its outputs after the falling edge
//! and the host samples just before the next rising edge. CLK idles high
//! between bits. [`Pins::half`] sets the speed: at most 400 kHz during
//! identification, then as fast as the pins allow (the spec allows 25 MHz in
//! default speed; callers should stay at or below what the board's wiring
//! is known to take).
//!
//! **Busy without blocking.** A write returns once the card has accepted the
//! block (CRC status "010"); the card then programs it while holding DAT0
//! low. The driver remembers that, and [`SdNative::is_busy`] lets a caller
//! skip work until the card is done instead of spinning. Any later command
//! still waits for busy first (bounded by [`BUSY_TIMEOUT_US`]).
//!
//! The card's serial number (CID PSN) is never parsed or exposed.

#![no_std]

/// Bytes in a data block. Only 512-byte blocks are used.
pub const BLOCK: usize = 512;
/// Clocks to wait for a command response start bit (spec: NCR <= 64).
pub const NCR_MAX: u32 = 80;
/// Longest wait for a read data block's start bit (spec: 100 ms).
pub const READ_TIMEOUT_US: u64 = 150_000;
/// Longest busy after a write or R1b (spec: 250 ms SDHC, 500 ms SDXC).
pub const BUSY_TIMEOUT_US: u64 = 600_000;

/// R1 card status bits that mean the command failed (OUT_OF_RANGE ..
/// AKE_SEQ_ERROR, not the state or APP_CMD bits).
pub const STATUS_ERRORS: u32 = 0xFDF9_8008;

// ------------------------------------------------------------------ CRCs --

/// CRC7 (x^7 + x^3 + 1) over `data`, as used in command and response frames.
pub fn crc7(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        for i in (0..8).rev() {
            let fb = ((b >> i) & 1) ^ ((crc >> 6) & 1);
            crc = (crc << 1) & 0x7F;
            if fb == 1 {
                crc ^= 0x09;
            }
        }
    }
    crc
}

const fn crc16_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC16_TABLE: [u16; 256] = crc16_table();

/// CRC16-CCITT (x^16 + x^12 + x^5 + 1, initial 0) over `data`, as used for
/// data blocks on a single data line.
pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |crc, &b| (crc << 8) ^ CRC16_TABLE[usize::from((crc >> 8) as u8 ^ b)])
}

/// A command frame: start 0, transmission 1, index, argument, CRC7, end 1.
pub fn command(index: u8, arg: u32) -> [u8; 6] {
    let a = arg.to_be_bytes();
    let mut f = [0x40 | (index & 0x3F), a[0], a[1], a[2], a[3], 0];
    f[5] = crc7(&f[..5]) << 1 | 1;
    f
}

// -------------------------------------------------------- card registers --

/// What the card told us about itself (CID and CSD), minus its serial number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardInfo {
    /// SDHC/SDXC (block addressed). False: SDSC (byte addressed).
    pub high_capacity: bool,
    /// Answered CMD8 (physical layer 2.0 or later).
    pub v2: bool,
    /// Capacity in 512-byte blocks.
    pub blocks: u32,
    /// CID manufacturer ID (MID).
    pub manufacturer: u8,
    /// CID OEM/application ID, two ASCII characters.
    pub oem: [u8; 2],
    /// CID product name, five ASCII characters.
    pub product: [u8; 5],
    /// CID product revision, BCD n.m.
    pub revision: u8,
    /// CID manufacturing date.
    pub year: u16,
    pub month: u8,
    /// CSD_STRUCTURE (0 = v1, 1 = v2, 2 = v3/SDUC).
    pub csd_version: u8,
}

/// Bits `hi..=lo` of a 128-bit register given most significant byte first.
pub fn reg_bits(r: &[u8; 16], hi: u32, lo: u32) -> u32 {
    let mut v = 0u32;
    for bit in (lo..=hi).rev() {
        let byte = 15 - (bit / 8) as usize;
        v = v << 1 | u32::from((r[byte] >> (bit % 8)) & 1);
    }
    v
}

/// Capacity in 512-byte blocks from a CSD.
pub fn csd_blocks(csd: &[u8; 16]) -> u32 {
    match reg_bits(csd, 127, 126) {
        0 => {
            let read_bl_len = reg_bits(csd, 83, 80);
            let c_size = reg_bits(csd, 73, 62);
            let mult = reg_bits(csd, 49, 47);
            let shift = (mult + 2 + read_bl_len).saturating_sub(9);
            (c_size + 1) << shift
        }
        // v2: C_SIZE 22 bits, v3 (SDUC) 28 bits; units of 512 KiB.
        1 => (reg_bits(csd, 69, 48) + 1).saturating_mul(1024),
        _ => (reg_bits(csd, 75, 48) + 1).saturating_mul(1024),
    }
}

impl CardInfo {
    pub fn parse(cid: &[u8; 16], csd: &[u8; 16], high_capacity: bool, v2: bool) -> Self {
        Self {
            high_capacity,
            v2,
            blocks: csd_blocks(csd),
            manufacturer: cid[0],
            oem: [cid[1], cid[2]],
            product: [cid[3], cid[4], cid[5], cid[6], cid[7]],
            revision: cid[8],
            year: 2000 + reg_bits(cid, 19, 12) as u16,
            month: reg_bits(cid, 11, 8) as u8,
            csd_version: reg_bits(csd, 127, 126) as u8,
        }
    }

    /// Capacity in MiB.
    pub fn mib(&self) -> u32 {
        self.blocks / 2048
    }
}

// ----------------------------------------------------------------- pins --

/// The three card lines. CMD and DAT0 are bidirectional: `*_out` drives the
/// line (enabling the output), `*_release` lets the pull-up and the card
/// have it, `*_in` reads the pad.
pub trait Pins {
    fn clk(&mut self, high: bool);
    fn cmd_out(&mut self, high: bool);
    fn cmd_release(&mut self);
    fn cmd_in(&mut self) -> bool;
    fn dat_out(&mut self, high: bool);
    fn dat_release(&mut self);
    fn dat_in(&mut self) -> bool;
    /// Wait half a clock period: at most 400 kHz when `slow`, else the
    /// transfer speed.
    fn half(&mut self, slow: bool);
    /// A free-running microsecond clock for timeouts.
    fn now_us(&mut self) -> u64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No response start bit to this command (no card, or not an SD card).
    NoResponse(u8),
    /// Response CRC7 wrong.
    ResponseCrc(u8),
    /// Response index or reserved bits wrong.
    BadResponse(u8),
    /// The card reported an error for this command (R1 status).
    Status(u8, u32),
    /// CMD8 echo wrong: the card cannot run at our voltage.
    Voltage,
    /// ACMD41 never finished.
    NotReady,
    /// Read data never started.
    DataTimeout,
    /// Read data CRC16 wrong.
    DataCrc,
    /// Read data end bit missing.
    DataFraming,
    /// The card rejected a written block's CRC.
    WriteCrc,
    /// The card refused a written block (status token bits).
    WriteError(u8),
    /// No CRC status token after a written block.
    NoWriteStatus,
    /// DAT0 stayed low (busy) too long.
    BusyTimeout,
    /// Not initialised.
    NoCard,
    /// Block number past the end of the card.
    OutOfRange,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for Error {}

/// Collects one data block bit by bit (start bit, data, CRC16, end bit), so
/// a block that starts while a response is still coming in is not lost.
struct DataIn<'a> {
    buf: &'a mut [u8],
    started: bool,
    n: usize,
    crc: u16,
    end: bool,
}

impl<'a> DataIn<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, started: false, n: 0, crc: 0, end: false }
    }

    #[inline(always)]
    fn push(&mut self, bit: bool) {
        if !self.started {
            self.started = !bit;
            return;
        }
        let total = self.buf.len() * 8;
        if self.n < total {
            let i = self.n / 8;
            if self.n.is_multiple_of(8) {
                self.buf[i] = 0;
            }
            if bit {
                self.buf[i] |= 0x80 >> (self.n % 8);
            }
        } else if self.n < total + 16 {
            self.crc = self.crc << 1 | u16::from(bit);
        } else if self.n == total + 16 {
            self.end = bit;
        } else {
            return;
        }
        self.n += 1;
    }

    fn done(&self) -> bool {
        self.started && self.n == self.buf.len() * 8 + 17
    }

    fn check(&self) -> Result<(), Error> {
        if crc16(self.buf) != self.crc {
            Err(Error::DataCrc)
        } else if !self.end {
            Err(Error::DataFraming)
        } else {
            Ok(())
        }
    }
}

/// The card driver.
pub struct SdNative<P> {
    pins: P,
    slow: bool,
    v2: bool,
    high_capacity: bool,
    rca: u16,
    busy: bool,
    info: Option<CardInfo>,
}

impl<P: Pins> SdNative<P> {
    pub fn new(mut pins: P) -> Self {
        pins.cmd_release();
        pins.dat_release();
        pins.clk(true);
        Self { pins, slow: true, v2: false, high_capacity: false, rca: 0, busy: false, info: None }
    }

    pub fn pins(&mut self) -> &mut P {
        &mut self.pins
    }

    pub fn info(&self) -> Option<&CardInfo> {
        self.info.as_ref()
    }

    /// Forget the card (it will need [`begin`](Self::begin) again).
    pub fn reset(&mut self) {
        self.info = None;
        self.busy = false;
        self.slow = true;
        self.pins.cmd_release();
        self.pins.dat_release();
    }

    // --------------------------------------------------- bit primitives --

    #[inline(always)]
    fn cmd_bit(&mut self, b: bool) {
        self.pins.clk(false);
        self.pins.cmd_out(b);
        self.pins.half(self.slow);
        self.pins.clk(true);
        self.pins.half(self.slow);
    }

    #[inline(always)]
    fn dat_bit(&mut self, b: bool) {
        self.pins.clk(false);
        self.pins.dat_out(b);
        self.pins.half(self.slow);
        self.pins.clk(true);
        self.pins.half(self.slow);
    }

    /// One clock; returns (CMD, DAT0) as they were just before the rising edge.
    #[inline(always)]
    fn sample(&mut self) -> (bool, bool) {
        self.pins.clk(false);
        self.pins.half(self.slow);
        let c = self.pins.cmd_in();
        let d = self.pins.dat_in();
        self.pins.clk(true);
        self.pins.half(self.slow);
        (c, d)
    }

    #[inline(always)]
    fn sample_dat(&mut self) -> bool {
        self.pins.clk(false);
        self.pins.half(self.slow);
        let d = self.pins.dat_in();
        self.pins.clk(true);
        self.pins.half(self.slow);
        d
    }

    fn clocks(&mut self, n: u32) {
        for _ in 0..n {
            self.sample();
        }
    }

    // ---------------------------------------------------------- commands --

    fn send(&mut self, index: u8, arg: u32) -> Result<(), Error> {
        if self.busy {
            self.wait_busy()?;
        }
        for byte in command(index, arg) {
            for i in (0..8).rev() {
                self.cmd_bit(byte >> i & 1 == 1);
            }
        }
        self.pins.cmd_release();
        Ok(())
    }

    /// Read a response of `out.len()` bytes (start bit included) and the 8
    /// clocks after it, feeding DAT0 to `rx` throughout when given.
    fn read_resp(&mut self, index: u8, out: &mut [u8], mut rx: Option<&mut DataIn<'_>>) -> Result<(), Error> {
        let mut waited = 0;
        loop {
            let (c, d) = self.sample();
            if let Some(rx) = rx.as_deref_mut() {
                rx.push(d);
            }
            if !c {
                break;
            }
            waited += 1;
            if waited > NCR_MAX {
                return Err(Error::NoResponse(index));
            }
        }
        out.fill(0);
        for n in 1..out.len() * 8 {
            let (c, d) = self.sample();
            if let Some(rx) = rx.as_deref_mut() {
                rx.push(d);
            }
            if c {
                out[n / 8] |= 0x80 >> (n % 8);
            }
        }
        for _ in 0..8 {
            let (_, d) = self.sample();
            if let Some(rx) = rx.as_deref_mut() {
                rx.push(d);
            }
        }
        Ok(())
    }

    /// R1 / R6 / R7: returns the 32-bit argument field.
    fn resp48(&mut self, index: u8, rx: Option<&mut DataIn<'_>>) -> Result<u32, Error> {
        let mut r = [0u8; 6];
        self.read_resp(index, &mut r, rx)?;
        if r[0] & 0xC0 != 0 || r[0] & 0x3F != index || r[5] & 1 == 0 {
            return Err(Error::BadResponse(index));
        }
        if crc7(&r[..5]) != r[5] >> 1 {
            return Err(Error::ResponseCrc(index));
        }
        Ok(u32::from_be_bytes([r[1], r[2], r[3], r[4]]))
    }

    /// R1 with the error bits checked.
    fn r1(&mut self, index: u8, rx: Option<&mut DataIn<'_>>) -> Result<u32, Error> {
        let s = self.resp48(index, rx)?;
        if s & STATUS_ERRORS != 0 {
            return Err(Error::Status(index, s));
        }
        Ok(s)
    }

    /// R3 (OCR): no CRC, index field all ones.
    fn r3(&mut self, index: u8) -> Result<u32, Error> {
        let mut r = [0u8; 6];
        self.read_resp(index, &mut r, None)?;
        if r[0] != 0x3F || r[5] != 0xFF {
            return Err(Error::BadResponse(index));
        }
        Ok(u32::from_be_bytes([r[1], r[2], r[3], r[4]]))
    }

    /// R2 (CID or CSD): the 128-bit register, CRC7 checked.
    fn r2(&mut self, index: u8) -> Result<[u8; 16], Error> {
        let mut r = [0u8; 17];
        self.read_resp(index, &mut r, None)?;
        if r[0] != 0x3F || r[16] & 1 == 0 {
            return Err(Error::BadResponse(index));
        }
        if crc7(&r[1..16]) != r[16] >> 1 {
            return Err(Error::ResponseCrc(index));
        }
        let mut reg = [0u8; 16];
        reg.copy_from_slice(&r[1..17]);
        Ok(reg)
    }

    // ------------------------------------------------------------- busy --

    /// Clock until DAT0 goes high (the card is done programming).
    pub fn wait_busy(&mut self) -> Result<(), Error> {
        let t0 = self.pins.now_us();
        let mut i = 0u32;
        loop {
            if self.sample_dat() {
                self.busy = false;
                return Ok(());
            }
            i += 1;
            if i.is_multiple_of(64) && self.pins.now_us().wrapping_sub(t0) > BUSY_TIMEOUT_US {
                return Err(Error::BusyTimeout);
            }
        }
    }

    /// Is the card still programming the last write? Costs 8 clocks; never
    /// waits.
    pub fn is_busy(&mut self) -> bool {
        if self.busy {
            for _ in 0..8 {
                if self.sample_dat() {
                    self.busy = false;
                    break;
                }
            }
        }
        self.busy
    }

    // ------------------------------------------------------------- init --

    /// Start initialisation: wake-up clocks, CMD0, CMD8. Then call
    /// [`poll_ready`](Self::poll_ready) until it says ready (spec: up to 1 s),
    /// then [`finish`](Self::finish). An empty slot shows up as
    /// `Err(NoResponse(55))` from the first `poll_ready`.
    pub fn begin(&mut self) -> Result<(), Error> {
        self.reset();
        for _ in 0..80 {
            self.cmd_bit(true);
        }
        self.pins.cmd_release();
        self.send(0, 0)?;
        self.clocks(8);
        // CMD8: 2.7-3.6 V, check pattern 0xAA.
        self.send(8, 0x1AA)?;
        match self.resp48(8, None) {
            Ok(r) if r & 0xFFF == 0x1AA => self.v2 = true,
            Ok(_) => return Err(Error::Voltage),
            Err(Error::NoResponse(_)) => self.v2 = false,
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// One ACMD41. True once the card has finished powering up.
    pub fn poll_ready(&mut self) -> Result<bool, Error> {
        self.send(55, 0)?;
        self.resp48(55, None)?;
        // HCS for v2 cards; voltage window 2.7-3.6 V.
        let arg = if self.v2 { 0x40FF_8000 } else { 0x00FF_8000 };
        self.send(41, arg)?;
        let ocr = self.r3(41)?;
        if ocr & 0x8000_0000 == 0 {
            return Ok(false);
        }
        self.high_capacity = self.v2 && ocr & 0x4000_0000 != 0;
        Ok(true)
    }

    /// Identify, select, 1-bit bus, 512-byte blocks; then switch to the fast
    /// clock.
    pub fn finish(&mut self) -> Result<CardInfo, Error> {
        self.send(2, 0)?;
        let cid = self.r2(2)?;
        self.send(3, 0)?;
        self.rca = (self.resp48(3, None)? >> 16) as u16;
        let rca = u32::from(self.rca) << 16;
        self.send(9, rca)?;
        let csd = self.r2(9)?;
        self.send(7, rca)?;
        self.r1(7, None)?;
        self.wait_busy()?;
        self.send(55, rca)?;
        self.r1(55, None)?;
        self.send(6, 0)?; // bus width 1 bit
        self.r1(6, None)?;
        if !self.high_capacity {
            self.send(16, BLOCK as u32)?;
            self.r1(16, None)?;
        }
        self.slow = false;
        let info = CardInfo::parse(&cid, &csd, self.high_capacity, self.v2);
        self.info = Some(info);
        Ok(info)
    }

    /// The whole initialisation at once (`max_polls` ACMD41 tries).
    pub fn init(&mut self, max_polls: u32) -> Result<CardInfo, Error> {
        self.begin()?;
        for _ in 0..max_polls {
            if self.poll_ready()? {
                return self.finish();
            }
        }
        Err(Error::NotReady)
    }

    /// CMD13: the card's status (also a cheap "still there?" check).
    pub fn status(&mut self) -> Result<u32, Error> {
        if self.info.is_none() {
            return Err(Error::NoCard);
        }
        self.send(13, u32::from(self.rca) << 16)?;
        self.r1(13, None)
    }

    // ------------------------------------------------------------- data --

    fn addr(&self, block: u32, count: usize) -> Result<u32, Error> {
        let info = self.info.ok_or(Error::NoCard)?;
        let end = u64::from(block) + count as u64;
        if end > u64::from(info.blocks) {
            return Err(Error::OutOfRange);
        }
        Ok(if self.high_capacity { block } else { block * BLOCK as u32 })
    }

    /// Clock DAT0 into `rx` until the block is complete.
    fn finish_block(&mut self, rx: &mut DataIn<'_>) -> Result<(), Error> {
        let t0 = self.pins.now_us();
        let mut i = 0u32;
        while !rx.started {
            let d = self.sample_dat();
            rx.push(d);
            i += 1;
            if i.is_multiple_of(64) && self.pins.now_us().wrapping_sub(t0) > READ_TIMEOUT_US {
                return Err(Error::DataTimeout);
            }
        }
        while !rx.done() {
            let d = self.sample_dat();
            rx.push(d);
        }
        rx.check()
    }

    /// Read `blocks.len()` blocks starting at `start`.
    pub fn read(&mut self, start: u32, blocks: &mut [[u8; BLOCK]]) -> Result<(), Error> {
        let addr = self.addr(start, blocks.len())?;
        match blocks {
            [] => Ok(()),
            [one] => {
                self.send(17, addr)?;
                let mut rx = DataIn::new(one);
                self.r1(17, Some(&mut rx))?;
                self.finish_block(&mut rx)?;
                self.clocks(8);
                Ok(())
            }
            [first, rest @ ..] => {
                self.send(18, addr)?;
                let r = (|| {
                    let mut rx = DataIn::new(first);
                    self.r1(18, Some(&mut rx))?;
                    self.finish_block(&mut rx)?;
                    for b in rest.iter_mut() {
                        let mut rx = DataIn::new(b);
                        self.finish_block(&mut rx)?;
                    }
                    Ok(())
                })();
                // Stop the transfer whatever happened; the card keeps sending
                // until CMD12 and stops two clocks after its end bit.
                let stop = self.stop();
                r.and(stop)
            }
        }
    }

    /// CMD12 (R1b), then wait out busy.
    fn stop(&mut self) -> Result<(), Error> {
        self.send(12, 0)?;
        // Out-of-range from reading up to the last block is normal here.
        self.resp48(12, None)?;
        self.wait_busy()
    }

    /// Send one data block on DAT0 and read the CRC status token. Leaves
    /// the card busy.
    fn write_block(&mut self, data: &[u8; BLOCK]) -> Result<(), Error> {
        let crc = crc16(data);
        // NWR: at least two clocks with DAT0 high, then the start bit.
        self.dat_bit(true);
        self.dat_bit(true);
        self.dat_bit(false);
        for &byte in data.iter() {
            for i in (0..8).rev() {
                self.dat_bit(byte >> i & 1 == 1);
            }
        }
        for i in (0..16).rev() {
            self.dat_bit(crc >> i & 1 == 1);
        }
        self.dat_bit(true);
        self.pins.dat_release();
        self.busy = true;
        // CRC status: start 0, three bits, end 1, normally two clocks later.
        let mut waited = 0;
        while self.sample_dat() {
            waited += 1;
            if waited > 16 {
                return Err(Error::NoWriteStatus);
            }
        }
        let mut s = 0u8;
        for _ in 0..3 {
            s = s << 1 | u8::from(self.sample_dat());
        }
        let _end = self.sample_dat();
        match s {
            0b010 => Ok(()),
            0b101 => Err(Error::WriteCrc),
            other => Err(Error::WriteError(other)),
        }
    }

    /// Write `blocks.len()` blocks starting at `start`. Returns once the card
    /// has accepted the last block; it may still be busy programming it
    /// (see [`is_busy`](Self::is_busy)).
    pub fn write(&mut self, start: u32, blocks: &[[u8; BLOCK]]) -> Result<(), Error> {
        let addr = self.addr(start, blocks.len())?;
        match blocks {
            [] => Ok(()),
            [one] => {
                self.send(24, addr)?;
                self.r1(24, None)?;
                self.write_block(one)
            }
            many => {
                self.send(25, addr)?;
                self.r1(25, None)?;
                let r = (|| {
                    for b in many {
                        self.write_block(b)?;
                        self.wait_busy()?;
                    }
                    Ok(())
                })();
                if r == Err(Error::BusyTimeout) {
                    return r;
                }
                let stop = self.stop();
                r.and(stop)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_frames_match_the_spec() {
        // Well-known frames (CRC values quoted throughout SD documentation).
        assert_eq!(command(0, 0), [0x40, 0, 0, 0, 0, 0x95]);
        assert_eq!(command(8, 0x1AA), [0x48, 0, 0, 0x01, 0xAA, 0x87]);
        assert_eq!(command(17, 0), [0x51, 0, 0, 0, 0, 0x55]);
        // Spec 4.5 example: CMD55 arg 0 -> CRC7 0x32 -> 0x65.
        assert_eq!(command(55, 0)[5], 0x65);
        // ACMD41 with HCS (common in SPI drivers): 0x77 last byte.
        assert_eq!(command(41, 0x4000_0000)[5], 0x77);
    }

    #[test]
    fn crc7_of_a_response() {
        // Spec 4.5 example: R1 response to CMD17, status 0x00000900 -> CRC7 0x33.
        assert_eq!(crc7(&[0x11, 0x00, 0x00, 0x09, 0x00]), 0x33);
    }

    #[test]
    fn crc16_of_ff_block() {
        // Spec 4.5: 512 bytes of 0xFF -> 0x7FA1.
        assert_eq!(crc16(&[0xFF; 512]), 0x7FA1);
        assert_eq!(crc16(b"123456789"), 0x31C3); // CRC-16/XMODEM check value
    }

    fn put(r: &mut [u8; 16], hi: u32, lo: u32, v: u32) {
        for bit in lo..=hi {
            let byte = 15 - (bit / 8) as usize;
            let m = 1u8 << (bit % 8);
            if v >> (bit - lo) & 1 == 1 {
                r[byte] |= m;
            } else {
                r[byte] &= !m;
            }
        }
    }

    #[test]
    fn csd_capacity() {
        // v2, C_SIZE 15159 -> (15159 + 1) * 512 KiB = 7.4 GiB.
        let mut csd = [0u8; 16];
        put(&mut csd, 127, 126, 1);
        put(&mut csd, 69, 48, 15159);
        assert_eq!(reg_bits(&csd, 69, 48), 15159);
        assert_eq!(csd_blocks(&csd), 15160 * 1024);
        // v1, 2 GB card: READ_BL_LEN 10, C_SIZE 4095, C_SIZE_MULT 7.
        let mut csd = [0u8; 16];
        put(&mut csd, 83, 80, 10);
        put(&mut csd, 73, 62, 4095);
        put(&mut csd, 49, 47, 7);
        assert_eq!(csd_blocks(&csd), 4096 << 10);
        assert_eq!(u64::from(csd_blocks(&csd)) * 512, 2u64 << 30);
    }

    #[test]
    fn cid_fields() {
        let mut cid = [0u8; 16];
        cid[0] = 0x03; // SanDisk
        cid[1..3].copy_from_slice(b"SD");
        cid[3..8].copy_from_slice(b"SC32G");
        cid[8] = 0x80;
        put(&mut cid, 19, 12, 23); // 2023
        put(&mut cid, 11, 8, 7);
        let i = CardInfo::parse(&cid, &[0x40; 16], true, true);
        assert_eq!((i.manufacturer, &i.oem, &i.product, i.revision), (3, b"SD", b"SC32G", 0x80));
        assert_eq!((i.year, i.month), (2023, 7));
    }
}
