//! CRC-16/CCITT-FALSE (polynomial 0x1021, initial 0xFFFF, no reflection, no
//! final XOR), table-driven. Check value: `crc(b"123456789") == 0x29B1`.
//!
//! Used for the end-of-answer check ([`link::Frame::BusDone`]) and the join
//! and address checks. 16 bits catch every error of up to 3 bits and every
//! burst up to 16 bits in an answer of up to 4 KB, and the postcard frame
//! structure has to survive on top; a collision at join leaves far more
//! damage than that.

const fn table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u16; 256] = table();

/// A running CRC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc(u16);

impl Default for Crc {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc {
    pub const fn new() -> Self {
        Crc(0xFFFF)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let mut c = self.0;
        for &b in bytes {
            c = (c << 8) ^ TABLE[usize::from(((c >> 8) as u8) ^ b)];
        }
        self.0 = c;
    }

    pub fn finish(self) -> u16 {
        self.0
    }
}

/// CRC of one slice.
pub fn crc(bytes: &[u8]) -> u16 {
    let mut c = Crc::new();
    c.update(bytes);
    c.finish()
}

/// [`link::Frame::BusJoin::check`].
pub fn join_check(mac: &[u8; 6], want: u8) -> u16 {
    let mut c = Crc::new();
    c.update(b"J");
    c.update(mac);
    c.update(&[want]);
    c.finish()
}

/// [`link::Command::Address::check`].
pub fn address_check(mac: &[u8; 6], addr: u8, epoch: u16) -> u16 {
    let mut c = Crc::new();
    c.update(b"A");
    c.update(mac);
    c.update(&[addr]);
    c.update(&epoch.to_le_bytes());
    c.finish()
}

/// [`link::Frame::BusDone::crc`]: the answer's bytes before the `BusDone`
/// frame, then its own fields.
pub fn done_crc(answer: &[u8], addr: u8, seq: u8, more: bool) -> u16 {
    let mut c = Crc::new();
    c.update(answer);
    c.update(&[addr, seq, u8::from(more)]);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        assert_eq!(crc(b"123456789"), 0x29B1);
        assert_eq!(crc(b""), 0xFFFF);
    }

    #[test]
    fn checks_differ_by_purpose() {
        let mac = [1, 2, 3, 4, 5, 6];
        assert_ne!(join_check(&mac, 3), address_check(&mac, 3, 0));
        assert_ne!(join_check(&mac, 3), join_check(&mac, 4));
        assert_ne!(done_crc(b"x", 1, 2, false), done_crc(b"x", 1, 2, true));
    }
}
