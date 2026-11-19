//! The hub's own SPI flash through the chip's ROM routines
//! (`esp_rom_spiflash_*`), for `hublink::flash::Flash` (docs/LINK-KEYS-DESIGN.md
//! 4.2: the kit records `rskit` and the boot counter `rsboot`).
//!
//! **Why the ROM.** `esp-storage` (the HAL's flash crate) has no ESP32-P4
//! support in the pinned esp-hal, and the pre-v3 patch adds none. The ROM
//! routines are there on this silicon: the pre-v3 ROM table the patch links
//! (`esp32p4-eco0_4/rom/esp32p4.rom.ld`) gives `esp_rom_spiflash_read`,
//! `_write` and `_erase_sector`, and they are what ESP-IDF's own 2nd-stage
//! bootloader (`bootloader_flash.c`) uses to read and write flash on the P4.
//! `esp-storage` calls the same routines on the chips it supports, the same
//! way as here.
//!
//! **The one rule while flash is busy.** The CPU runs this firmware from
//! flash through the cache. During an erase or program the flash cannot be
//! read, so nothing may fetch from it until the routine returns: interrupts
//! are off (a critical section), the calling function is in RAM (`#[ram]`),
//! the routine is in ROM, and the data it reads or writes is a buffer on the
//! stack (internal RAM), never PSRAM or a flash constant. The work is done
//! once, at boot, before the node link's UART interrupt exists, so no byte
//! can be lost meanwhile; an erase (one in 512 boots) blocks for up to a few
//! hundred milliseconds.
//!
//! **Not done here.** No write-protect unlock (`esp_rom_spiflash_unlock`
//! rewrites the flash status register; ESP-IDF's bootloader already clears
//! the protection at every boot, and a write that does not land is caught by
//! the read-back in `hublink::kit`). No access outside a checked partition:
//! every erase and write comes through `hublink::flash::Window`.
//!
//! **Untested on the chip until the bench** (see `linkauth.rs` for the
//! bring-up order): the read routine first, with writes off
//! (`--features rskit-readonly`), then one append per boot.

use esp_hal::ram;
use hublink::flash::{Flash, FlashError, SECTOR};

unsafe extern "C" {
    fn esp_rom_spiflash_read(src_addr: u32, data: *mut u32, len: i32) -> i32;
    fn esp_rom_spiflash_write(dest_addr: u32, data: *const u32, len: i32) -> i32;
    fn esp_rom_spiflash_erase_sector(sector_number: u32) -> i32;
}

/// Words moved per ROM call (a stack buffer: internal RAM).
const CHUNK_WORDS: usize = 64;

/// The ROM flash routines. One instance, at boot.
pub struct RomFlash {
    /// `rskit-readonly` builds: erases and writes refused.
    pub writes: bool,
}

fn rc(r: i32) -> Result<(), FlashError> {
    if r == 0 { Ok(()) } else { Err(FlashError::Io(r)) }
}

#[ram]
fn rom_read(addr: u32, words: &mut [u32; CHUNK_WORDS], n: usize) -> i32 {
    // SAFETY: the ROM routine reads `n * 4` bytes of flash at `addr` into
    // `words` (internal RAM, at least that long); interrupts are off and
    // this function is in RAM, so nothing fetches from flash meanwhile.
    critical_section::with(|_| unsafe { esp_rom_spiflash_read(addr, words.as_mut_ptr(), (n * 4) as i32) })
}

#[ram]
fn rom_write(addr: u32, words: &[u32; CHUNK_WORDS], n: usize) -> i32 {
    // SAFETY: as `rom_read`; `addr` is inside a checked partition
    // (`hublink::flash::Window`) and `words` holds `n` words.
    critical_section::with(|_| unsafe { esp_rom_spiflash_write(addr, words.as_ptr(), (n * 4) as i32) })
}

#[ram]
fn rom_erase(sector: u32) -> i32 {
    // SAFETY: as `rom_read`; the sector is inside a checked partition.
    critical_section::with(|_| unsafe { esp_rom_spiflash_erase_sector(sector) })
}

impl Flash for RomFlash {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        if !addr.is_multiple_of(4) || !buf.len().is_multiple_of(4) {
            return Err(FlashError::NotAligned);
        }
        let mut words = [0u32; CHUNK_WORDS];
        let mut a = addr;
        for chunk in buf.chunks_mut(CHUNK_WORDS * 4) {
            let n = chunk.len() / 4;
            rc(rom_read(a, &mut words, n))?;
            for (d, w) in chunk.chunks_exact_mut(4).zip(&words[..n]) {
                d.copy_from_slice(&w.to_le_bytes());
            }
            a += chunk.len() as u32;
        }
        words.fill(0); // it may have held the master record
        core::hint::black_box(&words);
        Ok(())
    }

    fn erase_sector(&mut self, addr: u32) -> Result<(), FlashError> {
        if !self.writes {
            return Err(FlashError::ReadOnly);
        }
        if !addr.is_multiple_of(SECTOR) {
            return Err(FlashError::NotAligned);
        }
        rc(rom_erase(addr / SECTOR))
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), FlashError> {
        if !self.writes {
            return Err(FlashError::ReadOnly);
        }
        if !addr.is_multiple_of(4) || !data.len().is_multiple_of(4) {
            return Err(FlashError::NotAligned);
        }
        let mut words = [0u32; CHUNK_WORDS];
        let mut a = addr;
        for chunk in data.chunks(CHUNK_WORDS * 4) {
            let n = chunk.len() / 4;
            for (w, s) in words.iter_mut().zip(chunk.chunks_exact(4)) {
                *w = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            }
            rc(rom_write(a, &words, n))?;
            a += chunk.len() as u32;
        }
        Ok(())
    }
}
