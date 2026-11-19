//! The hub's own SPI flash, behind a small trait ([`Flash`]) so everything
//! that reads the kit records and appends to the boot counter is host-tested
//! against a fake, and the firmware's one real implementation (ROM routines,
//! `firmware/hub-p4/src/flashrom.rs`) stays a few lines.
//!
//! **Writes go through a [`Window`] only.** A window is one partition from
//! the partition table, checked by [`crate::parttable::data_partition`]: a
//! data partition of the expected size, sector-aligned, overlapping nothing
//! else in the table. Every erase and write is bounds-checked against it, so
//! a bug above this layer cannot reach the bootloader, the partition table
//! or the application image. Nothing outside a window ever erases or writes.

/// One flash sector: the erase unit.
pub const SECTOR: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashError {
    /// The flash routine returned this code (ROM: 1 error, 2 timeout).
    Io(i32),
    /// Outside the window (or the flash).
    OutOfBounds,
    /// Address or length not a multiple of 4 (writes, reads) or of a
    /// sector (erases).
    NotAligned,
    /// Writes are switched off in this build (`rskit-readonly`).
    ReadOnly,
}

/// Raw flash access by absolute address. Reads and writes are whole 32-bit
/// words: `addr` and the length are multiples of 4.
pub trait Flash {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), FlashError>;
    /// Erase the 4 KB sector starting at `addr` (a multiple of [`SECTOR`]).
    fn erase_sector(&mut self, addr: u32) -> Result<(), FlashError>;
    /// Program `data` at `addr`. NOR flash: programming only clears bits.
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), FlashError>;
}

/// One partition of the flash: offsets are relative to its start, and
/// nothing outside `[start, start + len)` can be touched through it.
pub struct Window<'f, F: Flash> {
    flash: &'f mut F,
    start: u32,
    len: u32,
}

impl<'f, F: Flash> Window<'f, F> {
    /// `start` and `len` must be whole sectors (they come from a checked
    /// partition entry).
    pub fn new(flash: &'f mut F, start: u32, len: u32) -> Result<Self, FlashError> {
        if !start.is_multiple_of(SECTOR) || !len.is_multiple_of(SECTOR) || len == 0 {
            return Err(FlashError::NotAligned);
        }
        start.checked_add(len).ok_or(FlashError::OutOfBounds)?;
        Ok(Window { flash, start, len })
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn at(&self, off: usize, n: usize) -> Result<u32, FlashError> {
        let end = off.checked_add(n).ok_or(FlashError::OutOfBounds)?;
        if end > self.len as usize {
            return Err(FlashError::OutOfBounds);
        }
        if !off.is_multiple_of(4) || !n.is_multiple_of(4) {
            return Err(FlashError::NotAligned);
        }
        Ok(self.start + off as u32)
    }

    pub fn read(&mut self, off: usize, buf: &mut [u8]) -> Result<(), FlashError> {
        let a = self.at(off, buf.len())?;
        self.flash.read(a, buf)
    }

    /// Erase the sector at `off` (a multiple of [`SECTOR`]) inside the window.
    pub fn erase_sector(&mut self, off: usize) -> Result<(), FlashError> {
        if !off.is_multiple_of(SECTOR as usize) {
            return Err(FlashError::NotAligned);
        }
        let a = self.at(off, SECTOR as usize)?;
        self.flash.erase_sector(a)
    }

    pub fn write(&mut self, off: usize, data: &[u8]) -> Result<(), FlashError> {
        let a = self.at(off, data.len())?;
        self.flash.write(a, data)
    }
}

/// A flash in memory for tests: regions of real bytes at given addresses,
/// everything else unreadable. It behaves as NOR flash does (writes only
/// clear bits) and can be made to fail.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::vec::Vec;

    #[derive(Default)]
    pub struct MemFlash {
        pub regions: Vec<(u32, Vec<u8>)>,
        /// Every write and erase from now on fails with this.
        pub fail: Option<FlashError>,
        /// The next write programs only its first half (power cut).
        pub tear_next: bool,
        /// The next write lands, but its bytes read back wrong (a chip or
        /// bus fault the read-back must catch).
        pub garble_next: bool,
        pub writes: Vec<(u32, usize)>,
        pub erases: Vec<u32>,
    }

    impl MemFlash {
        pub fn with(regions: &[(u32, &[u8])]) -> Self {
            MemFlash { regions: regions.iter().map(|(a, b)| (*a, b.to_vec())).collect(), ..Default::default() }
        }

        fn find(&mut self, addr: u32, n: usize) -> Result<&mut [u8], FlashError> {
            for (a, b) in self.regions.iter_mut() {
                let a = *a as usize;
                let s = addr as usize;
                if s >= a && s + n <= a + b.len() {
                    return Ok(&mut b[s - a..s - a + n]);
                }
            }
            Err(FlashError::OutOfBounds)
        }

        pub fn bytes(&mut self, addr: u32, n: usize) -> Vec<u8> {
            self.find(addr, n).unwrap().to_vec()
        }
    }

    impl Flash for MemFlash {
        fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), FlashError> {
            if !addr.is_multiple_of(4) || !buf.len().is_multiple_of(4) {
                return Err(FlashError::NotAligned);
            }
            buf.copy_from_slice(self.find(addr, buf.len())?);
            Ok(())
        }

        fn erase_sector(&mut self, addr: u32) -> Result<(), FlashError> {
            if let Some(e) = self.fail {
                return Err(e);
            }
            if !addr.is_multiple_of(SECTOR) {
                return Err(FlashError::NotAligned);
            }
            self.erases.push(addr);
            self.find(addr, SECTOR as usize)?.fill(0xFF);
            Ok(())
        }

        fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), FlashError> {
            if let Some(e) = self.fail {
                return Err(e);
            }
            if !addr.is_multiple_of(4) || !data.len().is_multiple_of(4) {
                return Err(FlashError::NotAligned);
            }
            self.writes.push((addr, data.len()));
            let n = if core::mem::take(&mut self.tear_next) { data.len() / 2 } else { data.len() };
            let garble = core::mem::take(&mut self.garble_next);
            let dst = self.find(addr, data.len())?;
            for (d, s) in dst[..n].iter_mut().zip(data) {
                *d &= *s;
            }
            if garble {
                dst[0] ^= 0x01;
            }
            Ok(())
        }
    }
}
