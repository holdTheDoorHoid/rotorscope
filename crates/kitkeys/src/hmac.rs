//! HMAC-SHA256 (RFC 2104) on top of `sha2`, with the key's inner and outer
//! pads absorbed once up front: each tag then costs the message's own SHA-256
//! blocks plus one block for the outer hash, not two extra pad blocks.

use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct Hmac {
    inner: Sha256,
    outer: Sha256,
}

impl Hmac {
    /// `key` up to one SHA-256 block (64 bytes); ours are 32.
    pub fn new(key: &[u8]) -> Self {
        assert!(key.len() <= 64, "HMAC key longer than a block");
        let mut ipad = [0x36u8; 64];
        let mut opad = [0x5cu8; 64];
        for (i, k) in key.iter().enumerate() {
            ipad[i] ^= k;
            opad[i] ^= k;
        }
        let mut inner = Sha256::new();
        inner.update(ipad);
        let mut outer = Sha256::new();
        outer.update(opad);
        // Do not leave key material in stack slots that outlive us.
        ipad.fill(0);
        opad.fill(0);
        core::hint::black_box(&ipad);
        core::hint::black_box(&opad);
        Hmac { inner, outer }
    }

    /// HMAC of the concatenation of `parts`.
    pub fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        let mut h = self.inner.clone();
        for p in parts {
            h.update(p);
        }
        let ih = h.finalize();
        let mut o = self.outer.clone();
        o.update(ih);
        let mut out = [0u8; 32];
        out.copy_from_slice(&o.finalize());
        out
    }
}
