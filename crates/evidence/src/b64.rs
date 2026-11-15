//! Standard base64 (RFC 4648, with `=` padding), no allocation. Used for
//! the console stream (`@EV1 <base64>` lines) and export bundles.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encoded length of `n` bytes.
pub const fn encoded_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// Encode into `out`; returns the bytes written, or `None` if `out` is short.
pub fn encode(input: &[u8], out: &mut [u8]) -> Option<usize> {
    let need = encoded_len(input.len());
    if out.len() < need {
        return None;
    }
    for (i, c) in input.chunks(3).enumerate() {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let o = &mut out[i * 4..i * 4 + 4];
        o[0] = ALPHABET[(b[0] >> 2) as usize];
        o[1] = ALPHABET[(((b[0] & 3) << 4) | (b[1] >> 4)) as usize];
        o[2] = if c.len() > 1 { ALPHABET[(((b[1] & 15) << 2) | (b[2] >> 6)) as usize] } else { b'=' };
        o[3] = if c.len() > 2 { ALPHABET[(b[2] & 63) as usize] } else { b'=' };
    }
    Some(need)
}

fn val(c: u8) -> Option<u8> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// Decode into `out`; returns the bytes written. `None` on a bad
/// character, bad padding, or a short `out`.
pub fn decode(input: &[u8], out: &mut [u8]) -> Option<usize> {
    if !input.len().is_multiple_of(4) {
        return None;
    }
    let mut n = 0;
    for (i, q) in input.chunks(4).enumerate() {
        let last = i + 1 == input.len() / 4;
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut v = [0u8; 4];
        for j in 0..4 - pad {
            v[j] = val(q[j])?;
        }
        let bytes = [(v[0] << 2) | (v[1] >> 4), (v[1] << 4) | (v[2] >> 2), (v[2] << 6) | v[3]];
        let take = 3 - pad;
        if out.len() < n + take {
            return None;
        }
        out[n..n + take].copy_from_slice(&bytes[..take]);
        n += take;
    }
    Some(n)
}

#[cfg(feature = "std")]
pub fn encode_vec(input: &[u8]) -> std::string::String {
    let mut out = std::vec![0u8; encoded_len(input.len())];
    encode(input, &mut out).unwrap();
    std::string::String::from_utf8(out).unwrap()
}

#[cfg(feature = "std")]
pub fn decode_vec(input: &str) -> Option<std::vec::Vec<u8>> {
    let mut out = std::vec![0u8; input.len() / 4 * 3];
    let n = decode(input.trim().as_bytes(), &mut out)?;
    out.truncate(n);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            let mut b = [0u8; 16];
            let n = encode(i.as_bytes(), &mut b).unwrap();
            assert_eq!(&b[..n], o.as_bytes());
            let mut d = [0u8; 16];
            let m = decode(o.as_bytes(), &mut d).unwrap();
            assert_eq!(&d[..m], i.as_bytes());
        }
        let mut d = [0u8; 16];
        assert!(decode(b"Zm9=Yg==", &mut d).is_none());
        assert!(decode(b"Zm9", &mut d).is_none());
        assert!(decode(b"Z!9v", &mut d).is_none());
    }
}
