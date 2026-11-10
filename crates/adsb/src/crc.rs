//! The CSBee line checksum.

/// CRC of a CSBee line body, exactly as the ADSBee computes it: CRC-16/
/// CCITT-FALSE (polynomial 0x1021, init 0xFFFF, MSB first, no final XOR),
/// then the two bytes swapped. The ADSBee prints the result with `%X`.
///
/// Bitwise rather than table-driven: a line is under 200 bytes and arrives
/// at most a few dozen times a second, so 512 bytes of table buy nothing.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc.swap_bytes()
}
