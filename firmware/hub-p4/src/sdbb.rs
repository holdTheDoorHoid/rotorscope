//! The card slot's three lines as [`sdnative::Pins`], and the card as an
//! `embedded-sdmmc` block device.
//!
//! The slot is wired for SD native 1-bit mode on SDMMC slot 0: CLK GPIO43,
//! CMD GPIO44, DAT0 GPIO39 (Elecrow's IDF example, `bsp_sd.c`: 1-bit, 10 MHz
//! at most, internal pull-ups, no power switch). There is no DAT3/CS, so SPI
//! mode is impossible, and the SDMMC controller is not usable from our
//! pre-v3 esp-hal; the protocol is bit-banged by `sdnative`.
//!
//! Speed: at most 400 kHz while identifying, then as fast as the GPIOs go
//! but never above [`FAST_MAX_HZ`] (Elecrow's 10 MHz ceiling for this
//! wiring). Half periods are calibrated busy-waits.

use core::cell::RefCell;

use embedded_sdmmc::{Block, BlockCount, BlockDevice, BlockIdx};
use esp_hal::gpio::{Flex, InputConfig, Level, OutputConfig, Pull};
use esp_hal::time::Instant;
use sdnative::{BLOCK, Pins, SdNative};

/// Identification clock ceiling.
const SLOW_HZ: u32 = 400_000;
/// Transfer clock ceiling (Elecrow uses 10 MHz on this slot).
pub const FAST_MAX_HZ: u32 = 10_000_000;

pub struct BbPins<'d> {
    clk: Flex<'d>,
    cmd: Flex<'d>,
    dat: Flex<'d>,
    cmd_driving: bool,
    dat_driving: bool,
    slow_spins: u32,
    fast_spins: u32,
}

#[inline(always)]
fn spin(n: u32) {
    let mut i = 0;
    while i < n {
        i = core::hint::black_box(i) + 1;
    }
}

fn lvl(high: bool) -> Level {
    if high { Level::High } else { Level::Low }
}

impl<'d> BbPins<'d> {
    pub fn new(mut clk: Flex<'d>, mut cmd: Flex<'d>, mut dat: Flex<'d>) -> Self {
        clk.set_level(Level::High);
        clk.apply_output_config(&OutputConfig::default());
        clk.set_output_enable(true);
        let ic = InputConfig::default().with_pull(Pull::Up);
        for p in [&mut cmd, &mut dat] {
            p.set_level(Level::High);
            p.apply_output_config(&OutputConfig::default().with_pull(Pull::Up));
            p.apply_input_config(&ic);
            p.set_input_enable(true);
            p.set_output_enable(false);
        }
        // Spins per microsecond, measured.
        let n = 200_000;
        let t = Instant::now();
        spin(n);
        let us = t.elapsed().as_micros().max(1) as u32;
        let per_us = (n / us).max(1);
        // Half periods: 1.25 us at 400 kHz; 50 ns at 10 MHz. The GPIO writes
        // themselves add to this, so the real clock is somewhat slower.
        let slow_spins = per_us * 1_000_000 / SLOW_HZ / 2 + 1;
        let fast_spins = (per_us * 1_000_000 / FAST_MAX_HZ / 2).max(1);
        Self { clk, cmd, dat, cmd_driving: false, dat_driving: false, slow_spins, fast_spins }
    }

    /// Calibration result, for the log: (spins per slow half, per fast half).
    pub fn spins(&self) -> (u32, u32) {
        (self.slow_spins, self.fast_spins)
    }

    /// Idle levels of CMD and DAT0 with nothing driving them (pull-ups).
    pub fn idle_levels(&self) -> (bool, bool) {
        (self.cmd.is_high(), self.dat.is_high())
    }
}

impl Pins for BbPins<'_> {
    #[inline(always)]
    fn clk(&mut self, high: bool) {
        self.clk.set_level(lvl(high));
    }
    #[inline(always)]
    fn cmd_out(&mut self, high: bool) {
        self.cmd.set_level(lvl(high));
        if !self.cmd_driving {
            self.cmd.set_output_enable(true);
            self.cmd_driving = true;
        }
    }
    fn cmd_release(&mut self) {
        self.cmd.set_level(Level::High);
        self.cmd.set_output_enable(false);
        self.cmd_driving = false;
    }
    #[inline(always)]
    fn cmd_in(&mut self) -> bool {
        self.cmd.is_high()
    }
    #[inline(always)]
    fn dat_out(&mut self, high: bool) {
        self.dat.set_level(lvl(high));
        if !self.dat_driving {
            self.dat.set_output_enable(true);
            self.dat_driving = true;
        }
    }
    fn dat_release(&mut self) {
        self.dat.set_level(Level::High);
        self.dat.set_output_enable(false);
        self.dat_driving = false;
    }
    #[inline(always)]
    fn dat_in(&mut self) -> bool {
        self.dat.is_high()
    }
    #[inline(always)]
    fn half(&mut self, slow: bool) {
        spin(if slow { self.slow_spins } else { self.fast_spins });
    }
    fn now_us(&mut self) -> u64 {
        Instant::now().duration_since_epoch().as_micros()
    }
}

pub type Card = SdNative<BbPins<'static>>;

/// Counters kept by the block device.
#[derive(Default, Clone, Copy)]
pub struct IoStats {
    pub blocks_read: u32,
    pub blocks_written: u32,
    pub errors: u32,
    /// Longest single block-device call, microseconds.
    pub max_io_us: u64,
}

/// The card as an `embedded-sdmmc` block device (its methods take `&self`).
pub struct SdDev {
    pub card: RefCell<Card>,
    pub stats: RefCell<IoStats>,
}

impl SdDev {
    pub fn new(card: Card) -> Self {
        Self { card: RefCell::new(card), stats: RefCell::new(IoStats::default()) }
    }

    fn note<T>(&self, t0: Instant, r: &Result<T, sdnative::Error>) {
        let mut s = self.stats.borrow_mut();
        s.max_io_us = s.max_io_us.max(t0.elapsed().as_micros());
        if r.is_err() {
            s.errors += 1;
        }
    }
}

/// Blocks moved per multi-block command.
const CHUNK: usize = 4;

impl BlockDevice for SdDev {
    type Error = sdnative::Error;

    fn read(&self, blocks: &mut [Block], start: BlockIdx) -> Result<(), Self::Error> {
        let t0 = Instant::now();
        let mut card = self.card.borrow_mut();
        let mut tmp = [[0u8; BLOCK]; CHUNK];
        let mut r = Ok(());
        for (i, chunk) in blocks.chunks_mut(CHUNK).enumerate() {
            let n = chunk.len();
            r = card.read(start.0 + (i * CHUNK) as u32, &mut tmp[..n]);
            if r.is_err() {
                break;
            }
            for (b, t) in chunk.iter_mut().zip(&tmp) {
                b.contents = *t;
            }
        }
        drop(card);
        if r.is_ok() {
            self.stats.borrow_mut().blocks_read += blocks.len() as u32;
        }
        self.note(t0, &r);
        r
    }

    fn write(&self, blocks: &[Block], start: BlockIdx) -> Result<(), Self::Error> {
        let t0 = Instant::now();
        let mut card = self.card.borrow_mut();
        let mut tmp = [[0u8; BLOCK]; CHUNK];
        let mut r = Ok(());
        for (i, chunk) in blocks.chunks(CHUNK).enumerate() {
            for (t, b) in tmp.iter_mut().zip(chunk) {
                *t = b.contents;
            }
            r = card.write(start.0 + (i * CHUNK) as u32, &tmp[..chunk.len()]);
            if r.is_err() {
                break;
            }
        }
        drop(card);
        if r.is_ok() {
            self.stats.borrow_mut().blocks_written += blocks.len() as u32;
        }
        self.note(t0, &r);
        r
    }

    fn num_blocks(&self) -> Result<BlockCount, Self::Error> {
        let card = self.card.borrow();
        card.info().map(|i| BlockCount(i.blocks)).ok_or(sdnative::Error::NoCard)
    }
}
