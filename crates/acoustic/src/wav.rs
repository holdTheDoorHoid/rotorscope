//! The 44-byte header of a 16-bit mono WAV file, for audio clips on the SD
//! card. The hub writes the header with a zero length when a clip starts and
//! rewrites it when the clip ends.

/// Header length, bytes.
pub const HEADER_LEN: usize = 44;

/// Header for `samples` 16-bit mono samples at `rate` Hz.
pub fn header(rate: u32, samples: u32) -> [u8; HEADER_LEN] {
    let data = samples.saturating_mul(2);
    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}
