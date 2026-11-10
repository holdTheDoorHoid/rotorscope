//! The modem's undocumented "dump engine" (`0x600a9004..`), both ways we use
//! it.
//!
//! Register facts come from esp-sdr-zephyr (`doc/hardware-quirks.md`, "The
//! capture engine is the C6's with a rate field", and
//! `lib/esp_sdr/src/esp_sdr_priv.h` / `esp_sdr_rx.c` / `esp_sdr_diag.c`,
//! GPL-3.0-or-later), cross-checked against ESPARGOS esp-sdr's C5 notes and
//! C5VRX's `enable_continuous_modem` (which writes the same lane word). This
//! is an independent re-implementation of the documented sequence.
//!
//! - **Snapshot (Path A).** Raw source 15, a rate index (0..5 = 80, 40, 20,
//!   10, 8, 4 MS/s), a count; the engine takes SRAM block 1 (owner bits 11:8
//!   of `0x60095004` = 2, with `MAC_DUMP_ALLOC`, bit 16, putting the samples
//!   at 0x40830000), fills `count` words and sets DONE. While it owns the
//!   block the CPU's reads there return stale data and writes are dropped;
//!   nothing faults, so nothing of ours may live there (ld/sdr_bank.x).
//!   16,380 samples took 822 µs at index 2 on esp-sdr-zephyr's bench.
//! - **Free run (Path B).** Same set-up, rate index 0, count 0x1ffff (the
//!   17-bit counter never reaches DONE above 65535, so it never stops), then
//!   the owner bits go straight back to the CPU. The engine writes nothing
//!   more to SRAM, but the modem debug bus keeps carrying the dump word,
//!   which is what [`crate::route`] taps.
//!
//! The source-gate register `0x600a20b4` that the C6 needs is deliberately
//! never touched: on the C5 any access to it "takes 1.64 ms and reads 0".

use core::sync::atomic::{Ordering, compiler_fence};

const CTRL: usize = 0x600a_9004;
const CTRL_RUN: u32 = 1 << 31;
const CTRL_TRIGGER: u32 = 1 << 19;
const CTRL_DONE: u32 = 1 << 18;
const CTRL_COUNT_MASK: u32 = 0x1_ffff;
/// Endless: counts above 65535 never reach DONE.
const COUNT_ENDLESS: u32 = 0x1_ffff;

const MODE: usize = 0x600a_9008;
const MODE_SOURCE_SHIFT: u32 = 17;
const MODE_SOURCE_MASK: u32 = 0xf << MODE_SOURCE_SHIFT;
const MODE_RATE_SHIFT: u32 = 21;
const MODE_RATE_MASK: u32 = 0x7 << MODE_RATE_SHIFT;
const SOURCE_RX_IQ: u32 = 15;

/// Lane selectors: four 6-bit fields picking lanes 24..27, plus bit 24.
/// esp-sdr-zephyr: "Only the lane word matters ... `0x600a9018` = lanes 24 to
/// 27 with bit 24 set. Without bit 24, or with other lane numbers, the bus
/// carries no I/Q." C5VRX writes the same value field by field.
const LANES: usize = 0x600a_9018;
const LANES_MASK: u32 = 0x01ff_ffff;
const LANES_IQ: u32 = 24 | (25 << 6) | (26 << 12) | (27 << 18) | (1 << 24);

/// Clock forces, as librftest's `adctrig()` sets them.
const CLK_FORCE: usize = 0x600a_9c04;
const CLK_EN: usize = 0x600a_0800;
const CLK_EN_BIT: u32 = 1 << 2;

/// HP_SYSTEM SRAM usage: bits 11:8 one-hot block owner, bit 16 MAC_DUMP_ALLOC.
const SRAM_USAGE: usize = 0x6009_5004;
const USAGE_OWNER_MASK: u32 = 0xf00 | (1 << 16);
const USAGE_BLOCK1: u32 = (1 << 9) | (1 << 16);

/// Rate indices.
pub const RATE_80: u32 = 0;
pub const RATE_20: u32 = 2;

/// Where a snapshot lands: 64 KiB into block 1.
pub const SNAPSHOT_ADDR: usize = 0x4083_0000;
/// Largest snapshot (esp-sdr: 16,380 words fill the 64 KiB half).
pub const SNAPSHOT_WORDS: usize = 16_380;

const SENTINEL: u32 = 0xa5a0_055a;

#[inline(always)]
fn rd(a: usize) -> u32 {
    // SAFETY: fixed modem / HP_SYSTEM registers, always mapped.
    unsafe { (a as *const u32).read_volatile() }
}
#[inline(always)]
fn wr(a: usize, v: u32) {
    // SAFETY: as `rd`.
    unsafe { (a as *mut u32).write_volatile(v) }
}
#[inline(always)]
fn fence() {
    compiler_fence(Ordering::SeqCst);
    // SAFETY: a memory fence has no other effect.
    unsafe { core::arch::asm!("fence rw, rw") };
}

/// Point the engine at receive I/Q and give it block 1. Returns the usage
/// word to restore. From here until [`disarm`] the CPU must not touch block 1.
fn arm() -> u32 {
    let saved = rd(SRAM_USAGE);
    wr(CTRL, 0);
    wr(CLK_FORCE, u32::MAX);
    wr(CLK_EN, rd(CLK_EN) | CLK_EN_BIT);
    wr(LANES, (rd(LANES) & !LANES_MASK) | LANES_IQ);
    wr(MODE, (rd(MODE) & !MODE_SOURCE_MASK) | (SOURCE_RX_IQ << MODE_SOURCE_SHIFT));
    wr(SRAM_USAGE, (saved & !USAGE_OWNER_MASK) | USAGE_BLOCK1);
    // The bank switch must land before the engine starts writing.
    fence();
    let _ = rd(SRAM_USAGE);
    saved
}

fn set_rate(rate: u32) {
    wr(MODE, (rd(MODE) & !MODE_RATE_MASK) | ((rate << MODE_RATE_SHIFT) & MODE_RATE_MASK));
}

fn start(count: u32) {
    let ctrl = CTRL_RUN | (count & CTRL_COUNT_MASK);
    // Clear a stale DONE first (as upstream), then RUN and a trigger pulse.
    wr(CTRL, (count & CTRL_COUNT_MASK) | CTRL_DONE);
    wr(CTRL, ctrl);
    wr(CTRL, ctrl | CTRL_TRIGGER);
    wr(CTRL, ctrl);
}

fn restore_usage(saved: u32) {
    wr(SRAM_USAGE, saved);
    fence();
    let _ = rd(SRAM_USAGE);
}

/// Start the free-running engine that feeds the debug bus (Path B). Block 1
/// is handed back to the CPU before returning.
pub fn free_run() {
    let saved = arm();
    set_rate(RATE_80);
    start(COUNT_ENDLESS);
    restore_usage(saved);
}

/// Stop the engine.
pub fn stop() {
    wr(CTRL, 0);
}

/// Stop the engine and clear the block-1 owner and MAC_DUMP_ALLOC bits
/// (before a reset into the ROM loader, as C5VRX does).
pub fn release() {
    wr(CTRL, 0);
    restore_usage(rd(SRAM_USAGE) & !USAGE_OWNER_MASK);
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code, reason = "fields are read through Debug in the error line")]
pub enum SnapError {
    /// DONE (bit 18) never came; the control word at the time. (ESPARGOS
    /// esp-sdr's C5 raw-source probe waits on bit 22 instead: if that one is
    /// set here, that is the finding.)
    Timeout(u32),
    /// DONE came but a sentinel survived: a short capture.
    Short,
}

/// Path A: capture `count` words at `rate` into [`SNAPSHOT_ADDR`]. Busy-waits
/// (about 822 µs at 20 MS/s, at most `timeout_us`). Path B must be stopped:
/// its DMA ring lives in the same block. Returns the capture time in µs.
pub fn snapshot(rate: u32, count: usize, timeout_us: u64) -> Result<u32, SnapError> {
    let count = count.min(SNAPSHOT_WORDS);
    let words = SNAPSHOT_ADDR as *mut u32;
    // SAFETY: the bank is ours (ld/sdr_bank.x) and the CPU still owns it.
    unsafe {
        words.write_volatile(SENTINEL);
        words.add(count - 1).write_volatile(SENTINEL);
    }
    fence();
    let t0 = esp_hal::time::Instant::now();
    let saved = arm();
    set_rate(rate);
    start(count as u32);
    let mut done = false;
    while t0.elapsed().as_micros() < timeout_us {
        if rd(CTRL) & CTRL_DONE != 0 {
            done = true;
            break;
        }
    }
    let us = t0.elapsed().as_micros() as u32;
    let ctrl = rd(CTRL);
    wr(CTRL, 0);
    restore_usage(saved);
    if !done {
        return Err(SnapError::Timeout(ctrl));
    }
    // SAFETY: the CPU owns block 1 again.
    let (a, b) = unsafe { (words.read_volatile(), words.add(count - 1).read_volatile()) };
    if a == SENTINEL || b == SENTINEL {
        return Err(SnapError::Short);
    }
    Ok(us)
}

/// The held snapshot's words.
pub fn snapshot_words(count: usize) -> &'static [u32] {
    // SAFETY: block 1 upper half is reserved for us; the caller does not
    // start another capture while holding this slice (single-threaded loop).
    unsafe { core::slice::from_raw_parts(SNAPSHOT_ADDR as *const u32, count.min(SNAPSHOT_WORDS)) }
}
