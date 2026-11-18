//! MAVLink's X.25 checksum (CRC-16/MCRF4XX: init 0xFFFF, no final XOR).

pub fn x25_accumulate(crc: u16, b: u8) -> u16 {
    let mut tmp = b ^ (crc as u8);
    tmp ^= tmp << 4;
    let tmp = tmp as u16;
    (crc >> 8) ^ (tmp << 8) ^ (tmp << 3) ^ (tmp >> 4)
}

pub fn x25_continue(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc = x25_accumulate(crc, b);
    }
    crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_value() {
        // CRC-16/MCRF4XX check value for "123456789".
        assert_eq!(super::x25_continue(0xFFFF, b"123456789"), 0x6F91);
    }
}
