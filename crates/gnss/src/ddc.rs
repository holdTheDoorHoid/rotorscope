//! I2C ("DDC") access helpers for u-blox receivers.
//!
//! The crate does no I/O; these constants and helpers describe what the hub's I2C code does:
//!
//! 1. Write the register address [`REG_AVAIL_HI`] (0xFD) and read 2 bytes: a **big-endian**
//!    count of bytes waiting in the receiver's output buffer ([`bytes_available`]).
//! 2. Write register [`REG_STREAM`] (0xFF) and read that many bytes (in chunks if the I2C
//!    driver has a limit), then pass them to [`crate::Parser::feed`].
//! 3. To send a frame, write it as an ordinary I2C write to the device (no register byte
//!    for the data stream).
//!
//! Reading from 0xFF when nothing is waiting returns `0xFF` fill bytes. **Do not** strip 0xFF
//! from real data: it is legal inside frames (e.g. an unknown `tAcc` is 0xFFFFFFFF, and a
//! checksum byte can be 0xFF). Read exactly the advertised count and the fill never arrives;
//! if you do read blind, the parser skips 0xFF between frames by itself.
//!
//! (These register numbers come from the u-blox integration manual / the project brief, not
//! from the M10 interface description, which covers only the message set.)

/// Default 7-bit I2C address. Configurable with `CFG-I2C-ADDRESS` (key 0x20510001, stored
/// as `address << 1`; the default value is 132 = 0x84 = 0x42 << 1).
pub const ADDRESS: u8 = 0x42;
/// Register holding the high byte of the number of bytes available.
pub const REG_AVAIL_HI: u8 = 0xFD;
/// Register holding the low byte of the number of bytes available.
pub const REG_AVAIL_LO: u8 = 0xFE;
/// Data stream register.
pub const REG_STREAM: u8 = 0xFF;

/// Decode the two bytes read starting at [`REG_AVAIL_HI`] into a byte count.
pub fn bytes_available(hi_lo: [u8; 2]) -> usize {
    u16::from_be_bytes(hi_lo) as usize
}

/// True if a block read from the stream register contains only idle fill (`0xFF`).
pub fn is_idle(block: &[u8]) -> bool {
    block.iter().all(|&b| b == 0xFF)
}
