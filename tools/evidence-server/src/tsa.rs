//! RFC 3161 requests on the wire: the request bytes and a careful HTTP
//! client. What is stamped and when: [`crate::stamp`].
//!
//! The request carries only a SHA-256 imprint and a random nonce: no record
//! contents, no unit or owner names leave the server. The reply is stored
//! as received; check it with standard tools, e.g.
//! `openssl ts -reply -in token.tsr -text` and `openssl ts -verify`.

/// DER TimeStampReq for a SHA-256 imprint, certReq true, with a nonce.
/// The nonce is used as [`nonce`] makes it (see there).
pub fn request_der(imprint: &[u8; 32], nonce: [u8; 8]) -> Vec<u8> {
    let n = self::nonce(nonce);
    let mut v = vec![0x30, 0x43, 0x02, 0x01, 0x01, 0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20];
    v.extend_from_slice(imprint);
    v.extend_from_slice(&[0x02, 0x08]);
    v.extend_from_slice(&n);
    v.extend_from_slice(&[0x01, 0x01, 0xff]);
    debug_assert_eq!(v.len(), 2 + 0x43);
    v
}

/// An 8-byte nonce as the request carries it: a positive INTEGER whose
/// first byte is not zero (so its DER contents are exactly these 8 bytes,
/// and the reply must echo exactly them).
pub fn nonce(random: [u8; 8]) -> [u8; 8] {
    let mut n = random;
    n[0] &= 0x7F;
    n[0] |= 0x01;
    n
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

/// The HTTP client for authorities: a generic user agent, no cookies (the
/// `cookies` feature is off), no proxy taken from the environment, no
/// redirects followed (a 3xx is a failure, so a reply can only come from
/// the host configured), and a deadline on the whole exchange, body
/// included, so a slow authority cannot hold a worker.
pub fn agent(timeout: std::time::Duration) -> ureq::Agent {
    agent_with(timeout, USER_AGENT)
}

/// The same client with another fixed user agent (OpenTimestamps calendars).
pub fn agent_with(timeout: std::time::Duration, user_agent: &str) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .try_proxy_from_env(false)
        .user_agent(user_agent)
        .timeout_connect(timeout)
        .timeout(timeout)
        .max_idle_connections(0)
        .build()
}

/// Says only what the client speaks, nothing about this server or unit.
///
/// Requests also set `Accept-Encoding: identity`: ureq adds `gzip` when
/// another crate in a workspace-wide build turns its `gzip` feature on
/// (tools/airspace-pack does), and the bytes sent should not depend on how
/// the server was built.
pub const USER_AGENT: &str = "rfc3161-client";

/// POST a TimeStampReq; the reply's body, at most [`MAX_REPLY`] bytes.
/// Only `200` is a reply.
pub fn post_query(agent: &ureq::Agent, url: &str, req: &[u8]) -> Result<Vec<u8>, String> {
    let resp = agent
        .post(url)
        .set("Content-Type", "application/timestamp-query")
        .set("Accept-Encoding", "identity")
        .send_bytes(req)
        .map_err(short_error)?;
    checked_body(resp, MAX_REPLY)
}

/// A `200` reply's body, uncompressed, of stated length, at most `max`
/// bytes; anything else is an error.
pub fn checked_body(resp: ureq::Response, max: usize) -> Result<Vec<u8>, String> {
    if resp.status() != 200 {
        return Err(format!("HTTP {} (redirects are not followed)", resp.status()));
    }
    plain_body(&resp)?;
    read_capped(resp.into_reader(), max)
}

/// Refuse a reply that is compressed, or whose length is not stated.
///
/// The request asks for `identity`, so a compressed reply is already a
/// misbehaving service. Whether ureq would decompress it depends on its
/// `gzip` feature, which a workspace-wide build turns on (another crate uses
/// ureq's defaults) and a build of this package alone does not. When ureq
/// decompresses, it removes both `Content-Encoding` and `Content-Length`; so
/// a reply must state its length (or be chunked), and then a compressed one
/// is refused in either build: by its header here, or by its missing length
/// there. (A reply both compressed and chunked would still be decompressed
/// by a gzip-enabled build; it is then checked like any other: it must be a
/// stamp of exactly the imprint and nonce sent, within the size cap.)
fn plain_body(resp: &ureq::Response) -> Result<(), String> {
    if let Some(ce) = resp.header("content-encoding")
        && !ce.trim().eq_ignore_ascii_case("identity")
    {
        let ce: String = ce.chars().filter(|c| !c.is_control()).take(40).collect();
        return Err(format!("reply is compressed (Content-Encoding: {ce}): refused"));
    }
    let chunked = resp.header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    if resp.header("content-length").is_none() && !chunked {
        return Err("reply has no length (no Content-Length and not chunked; or it was compressed and this build's HTTP client unpacked it): refused".into());
    }
    Ok(())
}

/// GET a published file (certificate chain, revocation list), capped.
pub fn get_capped(agent: &ureq::Agent, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let resp = agent.get(url).set("Accept-Encoding", "identity").call().map_err(short_error)?;
    checked_body(resp, max)
}

/// A transport error, without the body an authority may have sent.
pub fn short_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => {
            let s = t.to_string();
            s.chars().take(200).collect()
        }
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
