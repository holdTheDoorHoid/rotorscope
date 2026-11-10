//! CRC-32 (IEEE 802.3, the one `zlib.crc32`, `crc32` and Python's
//! `binascii.crc32` compute), nibble-table, no allocation.

const POLY: u32 = 0xedb8_8320;

const TABLE: [u32; 16] = {
    let mut t = [0u32; 16];
    let mut n = 0;
    while n < 16 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 4 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
};

/// Running CRC: start with [`Crc32::new`], `update` any number of times,
/// `finish`.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Crc32(!0)
    }
    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.0;
        for &b in data {
            c ^= b as u32;
            c = TABLE[(c & 15) as usize] ^ (c >> 4);
            c = TABLE[(c & 15) as usize] ^ (c >> 4);
        }
        self.0 = c;
    }
    pub const fn finish(self) -> u32 {
        !self.0
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        // The standard check value for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
        let mut c = Crc32::new();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), 0xcbf4_3926);
    }
}
