//! CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320, init and final XOR
//! 0xFFFFFFFF). The bootloader's CRC32 command computes this same CRC, so a
//! local CRC over the bytes we hold can be compared with the chip's own.

const fn table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = table();

pub fn crc32(data: &[u8]) -> u32 {
    !data
        .iter()
        .fold(!0u32, |c, &b| TABLE[usize::from((c as u8) ^ b)] ^ (c >> 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(&[0xFF; 4]), 0xFFFF_FFFF);
    }
}
