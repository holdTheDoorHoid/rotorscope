//! TEA and XXTEA (Wheeler and Needham), as the OGN tracker library
//! (`ogn.h`: `TEA_Decrypt_Key0`) and SoftRF's `Legacy.cpp` (`btea`) use
//! them. Ported from SoftRF (GPL-3.0-or-later). The encrypting halves are
//! test-only.

const DELTA: u32 = 0x9e37_79b9;

/// OGN "whitening": TEA with an all-zero key, decrypting one 64-bit block.
pub(crate) fn tea_decrypt_key0(v: &mut [u32; 2], loops: u32) {
    let (mut v0, mut v1) = (v[0], v[1]);
    let mut sum = DELTA.wrapping_mul(loops);
    for _ in 0..loops {
        v1 = v1.wrapping_sub((v0 << 4) ^ v0.wrapping_add(sum) ^ (v0 >> 5));
        v0 = v0.wrapping_sub((v1 << 4) ^ v1.wrapping_add(sum) ^ (v1 >> 5));
        sum = sum.wrapping_sub(DELTA);
    }
    *v = [v0, v1];
}

#[cfg(test)]
pub(crate) fn tea_encrypt_key0(v: &mut [u32; 2], loops: u32) {
    let (mut v0, mut v1) = (v[0], v[1]);
    let mut sum = 0u32;
    for _ in 0..loops {
        sum = sum.wrapping_add(DELTA);
        v0 = v0.wrapping_add((v1 << 4) ^ v1.wrapping_add(sum) ^ (v1 >> 5));
        v1 = v1.wrapping_add((v0 << 4) ^ v0.wrapping_add(sum) ^ (v0 >> 5));
    }
    *v = [v0, v1];
}

#[cfg(feature = "legacy")]
#[inline]
fn mx(sum: u32, y: u32, z: u32, p: usize, e: u32, key: &[u32; 4]) -> u32 {
    (((z >> 5) ^ (y << 2)).wrapping_add((y >> 3) ^ (z << 4)))
        ^ ((sum ^ y).wrapping_add(key[(p & 3) ^ e as usize] ^ z))
}

/// XXTEA decryption of `v` (at least two words) in `rounds` rounds
/// (`btea` with negative `n` in SoftRF, which fixes 6 rounds).
#[cfg(feature = "legacy")]
pub(crate) fn xxtea_decrypt(v: &mut [u32], key: &[u32; 4], rounds: u32) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut sum = rounds.wrapping_mul(DELTA);
    let mut y = v[0];
    for _ in 0..rounds {
        let e = (sum >> 2) & 3;
        for p in (1..n).rev() {
            let z = v[p - 1];
            v[p] = v[p].wrapping_sub(mx(sum, y, z, p, e, key));
            y = v[p];
        }
        let z = v[n - 1];
        v[0] = v[0].wrapping_sub(mx(sum, y, z, 0, e, key));
        y = v[0];
        sum = sum.wrapping_sub(DELTA);
    }
}

#[cfg(all(test, feature = "legacy"))]
pub(crate) fn xxtea_encrypt(v: &mut [u32], key: &[u32; 4], rounds: u32) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut sum = 0u32;
    let mut z = v[n - 1];
    for _ in 0..rounds {
        sum = sum.wrapping_add(DELTA);
        let e = (sum >> 2) & 3;
        for p in 0..n - 1 {
            let y = v[p + 1];
            v[p] = v[p].wrapping_add(mx(sum, y, z, p, e, key));
            z = v[p];
        }
        let y = v[0];
        v[n - 1] = v[n - 1].wrapping_add(mx(sum, y, z, n - 1, e, key));
        z = v[n - 1];
    }
}
