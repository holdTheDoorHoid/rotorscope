//! GT911 touch controller on the board I2C (address 0x5D), polled.
//!
//! Status register 0x814E: bit 7 = new data, bits 0-3 = touch points. Point 1
//! is at 0x8150: x (LE u16), y (LE u16). The status must be cleared (write 0)
//! after each read or the chip stops reporting.

use crate::bb::BbI2c;

pub const ADDR: u8 = 0x5D;
const STATUS: [u8; 2] = [0x81, 0x4E];
const POINT1: [u8; 2] = [0x81, 0x50];
const PRODUCT_ID: [u8; 2] = [0x81, 0x40];

/// One touch event: where, and whether it is a new press (finger down after
/// none) rather than a held finger.
#[derive(Debug, Clone, Copy)]
pub struct Touch {
    pub x: u16,
    pub y: u16,
    pub pressed: bool,
}

#[derive(Default)]
pub struct Gt911 {
    down: bool,
}

impl Gt911 {
    /// Product id string ("911" on this board), as a check the chip answers.
    pub fn product_id(i2c: &mut BbI2c<'_>) -> Option<[u8; 4]> {
        let mut id = [0u8; 4];
        i2c.write_read(ADDR, &PRODUCT_ID, &mut id).then_some(id)
    }

    /// True while a finger is on the panel (cleared when the chip reports
    /// zero points, which it does once on release).
    pub fn is_down(&self) -> bool {
        self.down
    }

    /// Read the first touch point if the chip has new data.
    pub fn poll(&mut self, i2c: &mut BbI2c<'_>) -> Option<Touch> {
        let mut st = [0u8; 1];
        if !i2c.write_read(ADDR, &STATUS, &mut st) || st[0] & 0x80 == 0 {
            return None;
        }
        let n = st[0] & 0x0F;
        let mut p = [0u8; 4];
        let got = n > 0 && i2c.write_read(ADDR, &POINT1, &mut p);
        let _ = i2c.write(ADDR, &[STATUS[0], STATUS[1], 0]);
        if !got {
            self.down = false;
            return None;
        }
        let pressed = !self.down;
        self.down = true;
        Some(Touch { x: u16::from_le_bytes([p[0], p[1]]), y: u16::from_le_bytes([p[2], p[3]]), pressed })
    }
}
