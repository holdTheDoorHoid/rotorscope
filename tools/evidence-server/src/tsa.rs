//! RFC 3161 time-stamping of a chain head (outside proof that the records
//! existed by then). Off unless the server is started with `--tsa URL`.
//!
//! The request carries only a SHA-256 imprint (of boot id, record number
//! and checkpoint hash): no record contents leave the server. The reply is
//! stored as received; check it with standard tools, e.g.
//! `openssl ts -reply -in token.tsr -text` and `openssl ts -verify`.
//! OpenTimestamps is the other planned route (docs/EVIDENCE.md).

/// DER TimeStampReq for a SHA-256 imprint, certReq true, with a nonce.
pub fn request_der(imprint: &[u8; 32], nonce: [u8; 8]) -> Vec<u8> {
    let mut n = nonce;
    n[0] &= 0x7F; // positive INTEGER
    n[0] |= 0x01; // and no leading zero byte needed
    let mut v = vec![0x30, 0x43, 0x02, 0x01, 0x01, 0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20];
    v.extend_from_slice(imprint);
    v.extend_from_slice(&[0x02, 0x08]);
    v.extend_from_slice(&n);
    v.extend_from_slice(&[0x01, 0x01, 0xff]);
    debug_assert_eq!(v.len(), 2 + 0x43);
    v
}

/// The PKIStatus of a TimeStampResp: 0 granted, 1 granted with mods.
pub fn response_status(resp: &[u8]) -> Option<u8> {
    // 30 <len> 30 <len> 02 01 <status>
    let skip_len = |b: &[u8], i: usize| -> Option<usize> {
        let l = *b.get(i)?;
        if l < 0x80 { Some(i + 1) } else { Some(i + 1 + (l & 0x7f) as usize) }
    };
    if resp.first() != Some(&0x30) {
        return None;
    }
    let i = skip_len(resp, 1)?;
    if resp.get(i) != Some(&0x30) {
        return None;
    }
    let j = skip_len(resp, i + 1)?;
    (resp.get(j..j + 2)? == [0x02, 0x01]).then(|| resp.get(j + 2).copied())?
}

/// The largest reply taken from an authority (a token with its signing
/// certificate chain is a few kilobytes).
pub const MAX_REPLY: usize = 256 << 10;

/// Read at most `max` bytes; more is an error, not a truncated reply.
pub fn read_capped(r: impl std::io::Read, max: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut r.take(max as u64 + 1), &mut body).map_err(|e| e.to_string())?;
    if body.len() > max {
        return Err(format!("reply longer than {max} bytes"));
    }
    Ok(body)
}

pub fn request(url: &str, imprint: &[u8; 32]) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; 8];
    getrandom::getrandom(&mut nonce).map_err(|e| e.to_string())?;
    let req = request_der(imprint, nonce);
    let resp = ureq::post(url)
        .set("Content-Type", "application/timestamp-query")
        .timeout(std::time::Duration::from_secs(20))
        .send_bytes(&req)
        .map_err(|e| e.to_string())?;
    let body = read_capped(resp.into_reader(), MAX_REPLY)?;
    match response_status(&body) {
        Some(0 | 1) => Ok(body),
        Some(s) => Err(format!("authority refused (PKIStatus {s})")),
        None => Err("reply is not a TimeStampResp".into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn request_is_well_formed() {
        let r = super::request_der(&[0xAB; 32], [0xFF; 8]);
        assert_eq!(r.len(), 69);
        assert_eq!(r[1] as usize, r.len() - 2);
        assert_eq!(r[r.len() - 3..], [1, 1, 0xff]);
        assert_eq!(super::response_status(&[0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x00]), Some(0));
        assert_eq!(super::response_status(&[0x30, 0x81, 0x05, 0x30, 0x03, 0x02, 0x01, 0x02]), Some(2));
    }

    /// An authority (or whoever answers in its place) cannot make the
    /// server hold an endless reply in memory.
    #[test]
    fn a_reply_is_capped() {
        let endless = std::io::repeat(0x30);
        assert!(super::read_capped(endless, 1024).unwrap_err().contains("longer than 1024"));
        assert_eq!(super::read_capped(&[1u8; 1024][..], 1024).unwrap().len(), 1024);
    }
}
