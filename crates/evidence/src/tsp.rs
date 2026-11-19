//! A minimal reader for RFC 3161 time-stamp replies and tokens: just enough
//! DER to pull out the status, the stamped imprint, the nonce and the
//! authority's time. It does **not** check the authority's signature or
//! certificates (use `openssl ts -verify`; `evidence-verify
//! --check-timestamps` runs or prints it).
//!
//! Hostile input is expected (the reply comes from a third party, and a
//! bundle from anyone): every read is bounds-checked, nothing allocates,
//! nothing recurses without bound, and no input panics.
//!
//! ```text
//! TimeStampResp ::= SEQUENCE { status PKIStatusInfo, timeStampToken ContentInfo OPTIONAL }
//! PKIStatusInfo ::= SEQUENCE { status INTEGER, ... }
//! ContentInfo   ::= SEQUENCE { contentType OID signedData, [0] EXPLICIT SignedData }
//! SignedData    ::= SEQUENCE { version, digestAlgorithms SET,
//!                              encapContentInfo SEQUENCE { eContentType OID tSTInfo,
//!                                                          [0] EXPLICIT OCTET STRING (TSTInfo) }, ... }
//! TSTInfo       ::= SEQUENCE { version INTEGER, policy OID,
//!                              messageImprint SEQUENCE { AlgorithmIdentifier, OCTET STRING },
//!                              serialNumber INTEGER, genTime GeneralizedTime,
//!                              accuracy SEQUENCE OPTIONAL, ordering BOOLEAN DEFAULT FALSE,
//!                              nonce INTEGER OPTIONAL, ... }
//! ```

/// DER of OID 2.16.840.1.101.3.4.2.1 (SHA-256), contents only.
pub const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
/// 1.2.840.113549.1.7.2 (CMS signedData).
pub const OID_SIGNED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02];
/// 1.2.840.113549.1.9.16.1.4 (id-ct-TSTInfo).
pub const OID_TST_INFO: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x10, 0x01, 0x04];

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const OCTET_STRING: u8 = 0x04;
const OID: u8 = 0x06;
const GENERALIZED_TIME: u8 = 0x18;
const BOOLEAN: u8 = 0x01;
const CTX0: u8 = 0xA0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The input ends inside an element.
    Truncated,
    /// A length form DER does not allow (indefinite, longer than 4 bytes).
    BadLength,
    /// A tag form this reader does not take (multi-byte tags).
    BadTag,
    /// The structure is not the expected one; the text says where.
    Unexpected(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Truncated => f.write_str("DER ends inside an element"),
            Error::BadLength => f.write_str("DER length form not allowed"),
            Error::BadTag => f.write_str("DER tag form not supported"),
            Error::Unexpected(w) => write!(f, "unexpected structure: {w}"),
        }
    }
}

/// One element: its tag and contents.
#[derive(Debug, Clone, Copy)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
}

/// Read one element from the front of `b`; returns it and what follows.
pub fn read(b: &[u8]) -> Result<(Tlv<'_>, &[u8]), Error> {
    let (&tag, rest) = b.split_first().ok_or(Error::Truncated)?;
    if tag & 0x1F == 0x1F {
        return Err(Error::BadTag);
    }
    let (&l0, rest) = rest.split_first().ok_or(Error::Truncated)?;
    let (len, rest) = if l0 < 0x80 {
        (l0 as usize, rest)
    } else {
        let n = (l0 & 0x7F) as usize;
        if n == 0 || n > 4 {
            return Err(Error::BadLength);
        }
        let bytes = rest.get(..n).ok_or(Error::Truncated)?;
        let len = bytes.iter().fold(0usize, |a, &x| (a << 8) | x as usize);
        (len, &rest[n..])
    };
    if len > rest.len() {
        return Err(Error::Truncated);
    }
    let (content, rest) = rest.split_at(len);
    Ok((Tlv { tag, content }, rest))
}

/// Read one element of tag `tag`.
fn expect<'a>(b: &'a [u8], tag: u8, what: &'static str) -> Result<(&'a [u8], &'a [u8]), Error> {
    let (t, rest) = read(b)?;
    if t.tag != tag {
        return Err(Error::Unexpected(what));
    }
    Ok((t.content, rest))
}

/// What a TimeStampResp says.
#[derive(Debug, Clone, Copy)]
pub struct Response<'a> {
    /// PKIStatus: 0 granted, 1 granted with modifications, 2 rejection,
    /// 3 waiting, 4 revocation warning, 5 revocation notification.
    pub status: u8,
    /// The token (a ContentInfo), when present.
    pub token: Option<&'a [u8]>,
}

/// Parse a whole TimeStampResp (nothing may follow it).
pub fn parse_response(b: &[u8]) -> Result<Response<'_>, Error> {
    let (resp, after) = expect(b, SEQUENCE, "TimeStampResp is not a SEQUENCE")?;
    if !after.is_empty() {
        return Err(Error::Unexpected("bytes after the TimeStampResp"));
    }
    let (status_info, rest) = expect(resp, SEQUENCE, "PKIStatusInfo is not a SEQUENCE")?;
    let (status, _) = expect(status_info, INTEGER, "PKIStatus is not an INTEGER")?;
    let status = match status {
        [s] if *s <= 5 => *s,
        _ => return Err(Error::Unexpected("PKIStatus out of range")),
    };
    let token = if rest.is_empty() {
        None
    } else {
        let (t, after) = read(rest)?;
        if t.tag != SEQUENCE || !after.is_empty() {
            return Err(Error::Unexpected("timeStampToken is not one ContentInfo"));
        }
        // The token's own bytes, tag and length included.
        Some(rest)
    };
    Ok(Response { status, token })
}

/// What a time-stamp token says (signature not checked).
#[derive(Debug, Clone, Copy)]
pub struct TstInfo<'a> {
    /// The authority's policy OID (contents).
    pub policy: &'a [u8],
    /// The imprint's hash algorithm OID (contents).
    pub hash_alg: &'a [u8],
    /// The stamped hash.
    pub imprint: &'a [u8],
    /// The authority's serial number for this token (INTEGER contents).
    pub serial: &'a [u8],
    /// genTime as written (ASCII, e.g. `20261118143005Z`).
    pub gen_time: &'a [u8],
    /// genTime as milliseconds since 1970 (UTC).
    pub gen_time_ms: u64,
    /// The nonce echoed from the request (INTEGER contents), if any.
    pub nonce: Option<&'a [u8]>,
}

/// Parse a token (a ContentInfo holding SignedData over a TSTInfo).
pub fn parse_token(b: &[u8]) -> Result<TstInfo<'_>, Error> {
    let (ci, _) = expect(b, SEQUENCE, "ContentInfo is not a SEQUENCE")?;
    let (oid, rest) = expect(ci, OID, "ContentInfo has no contentType")?;
    if oid != OID_SIGNED_DATA {
        return Err(Error::Unexpected("token is not CMS SignedData"));
    }
    let (explicit, _) = expect(rest, CTX0, "ContentInfo has no [0] content")?;
    let (sd, _) = expect(explicit, SEQUENCE, "SignedData is not a SEQUENCE")?;
    let (_, rest) = expect(sd, INTEGER, "SignedData has no version")?;
    let (_, rest) = expect(rest, SET, "SignedData has no digestAlgorithms")?;
    let (eci, _) = expect(rest, SEQUENCE, "SignedData has no encapContentInfo")?;
    let (oid, rest) = expect(eci, OID, "encapContentInfo has no eContentType")?;
    if oid != OID_TST_INFO {
        return Err(Error::Unexpected("signed content is not a TSTInfo"));
    }
    let (explicit, _) = expect(rest, CTX0, "encapContentInfo has no eContent")?;
    let (tst, _) = expect(explicit, OCTET_STRING, "eContent is not a primitive OCTET STRING")?;
    parse_tst_info(tst)
}

/// Parse a TSTInfo's DER.
pub fn parse_tst_info(b: &[u8]) -> Result<TstInfo<'_>, Error> {
    let (t, _) = expect(b, SEQUENCE, "TSTInfo is not a SEQUENCE")?;
    let (_, rest) = expect(t, INTEGER, "TSTInfo has no version")?;
    let (policy, rest) = expect(rest, OID, "TSTInfo has no policy")?;
    let (mi, rest) = expect(rest, SEQUENCE, "TSTInfo has no messageImprint")?;
    let (alg, mi_rest) = expect(mi, SEQUENCE, "messageImprint has no hashAlgorithm")?;
    let (hash_alg, _) = expect(alg, OID, "hashAlgorithm has no OID")?;
    let (imprint, _) = expect(mi_rest, OCTET_STRING, "messageImprint has no hashedMessage")?;
    let (serial, rest) = expect(rest, INTEGER, "TSTInfo has no serialNumber")?;
    let (gen_time, mut rest) = expect(rest, GENERALIZED_TIME, "TSTInfo has no genTime")?;
    let gen_time_ms = generalized_time_ms(gen_time).ok_or(Error::Unexpected("genTime is not YYYYMMDDHHMMSS[.f]Z"))?;
    let mut nonce = None;
    // Optional: accuracy (SEQUENCE), ordering (BOOLEAN), nonce (INTEGER),
    // then tsa [0] and extensions [1], which are not needed.
    while !rest.is_empty() {
        let (e, after) = read(rest)?;
        match e.tag {
            SEQUENCE | BOOLEAN => {}
            INTEGER => {
                nonce = Some(e.content);
                break;
            }
            _ => break,
        }
        rest = after;
    }
    Ok(TstInfo { policy, hash_alg, imprint, serial, gen_time, gen_time_ms, nonce })
}

/// The TSTInfo of a stored reply or a bare token, whichever `b` is.
pub fn tst_info(b: &[u8]) -> Result<TstInfo<'_>, Error> {
    match parse_response(b) {
        Ok(Response { token: Some(t), status: 0 | 1 }) => parse_token(t),
        Ok(Response { token: None, .. }) => Err(Error::Unexpected("reply carries no token")),
        Ok(_) => Err(Error::Unexpected("reply is not a granted stamp")),
        Err(_) => parse_token(b),
    }
}

/// Two INTEGER contents are the same number (leading zero bytes ignored).
pub fn same_integer(a: &[u8], b: &[u8]) -> bool {
    fn strip(x: &[u8]) -> &[u8] {
        let i = x.iter().position(|&v| v != 0).unwrap_or(x.len());
        x.get(i..).unwrap_or(&[])
    }
    strip(a) == strip(b)
}

/// Why a reply is not a stamp of what was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Not a TimeStampResp at all.
    NotAReply(Error),
    /// The authority said no (its PKIStatus).
    Status(u8),
    /// Granted, but the token cannot be read.
    BadToken(Error),
    /// The token's hash algorithm is not SHA-256.
    NotSha256,
    /// The token is over another hash.
    WrongImprint,
    /// The token does not echo the request's nonce (an old or someone
    /// else's token replayed).
    WrongNonce,
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Refused::NotAReply(e) => write!(f, "reply is not a TimeStampResp ({e})"),
            Refused::Status(s) => write!(f, "authority refused (PKIStatus {s})"),
            Refused::BadToken(e) => write!(f, "token cannot be read ({e})"),
            Refused::NotSha256 => f.write_str("token is not over a SHA-256 imprint"),
            Refused::WrongImprint => f.write_str("token is not over the imprint that was sent"),
            Refused::WrongNonce => f.write_str("token does not carry the nonce that was sent"),
        }
    }
}

/// Accept a reply only if it is a granted stamp of exactly `imprint`
/// (SHA-256) carrying `nonce`.
pub fn check_reply<'a>(reply: &'a [u8], imprint: &[u8; 32], nonce: &[u8]) -> Result<TstInfo<'a>, Refused> {
    let r = parse_response(reply).map_err(Refused::NotAReply)?;
    if !matches!(r.status, 0 | 1) {
        return Err(Refused::Status(r.status));
    }
    let t = r.token.ok_or(Refused::BadToken(Error::Unexpected("granted reply carries no token")))?;
    let i = parse_token(t).map_err(Refused::BadToken)?;
    if i.hash_alg != OID_SHA256 {
        return Err(Refused::NotSha256);
    }
    if i.imprint != imprint {
        return Err(Refused::WrongImprint);
    }
    if !i.nonce.is_some_and(|n| same_integer(n, nonce)) {
        return Err(Refused::WrongNonce);
    }
    Ok(i)
}

/// `YYYYMMDDHHMMSS[.fff...]Z` as milliseconds since 1970.
pub fn generalized_time_ms(s: &[u8]) -> Option<u64> {
    let digits = |r: core::ops::Range<usize>| -> Option<u64> {
        let b = s.get(r)?;
        b.iter().try_fold(0u64, |a, &c| c.is_ascii_digit().then(|| a * 10 + u64::from(c - b'0')))
    };
    let (y, mo, d, h, mi, se) = (digits(0..4)?, digits(4..6)?, digits(6..8)?, digits(8..10)?, digits(10..12)?, digits(12..14)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let rest = s.get(14..)?;
    let (frac, z) = match rest.split_first()? {
        (b'.', r) => {
            let n = r.iter().take_while(|c| c.is_ascii_digit()).count();
            if n == 0 {
                return None;
            }
            let ms = r[..n.min(3)].iter().fold(0u64, |a, &c| a * 10 + u64::from(c - b'0')) * [100, 10, 1][n.min(3) - 1];
            (ms, &r[n..])
        }
        _ => (0, rest),
    };
    if z != b"Z" {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(((days * 24 + h) * 60 + mi) * 60_000 + se * 1000 + frac)
}

fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        2 if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to a date at or after it (Howard Hinnant's method).
fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generalized_time() {
        assert_eq!(generalized_time_ms(b"19700101000000Z"), Some(0));
        assert_eq!(generalized_time_ms(b"20261118143005Z"), Some(1_795_012_205_000));
        assert_eq!(generalized_time_ms(b"20261118143005.5Z"), Some(1_795_012_205_500));
        assert_eq!(generalized_time_ms(b"20261118143005.123456Z"), Some(1_795_012_205_123));
        assert_eq!(generalized_time_ms(b"20240229000000Z").map(|m| m % 86_400_000), Some(0));
        for bad in [&b""[..], b"2026111814300Z", b"20261118143005", b"20261318143005Z", b"20230229000000Z", b"20261118143005.Z", b"20261118143005+0100", b"19691231235959Z", b"2026111814300\xffZ"] {
            assert_eq!(generalized_time_ms(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn lengths() {
        assert!(matches!(read(&[0x30, 0x80, 0, 0]), Err(Error::BadLength)));
        assert!(matches!(read(&[0x30, 0x85, 0, 0, 0, 0, 1]), Err(Error::BadLength)));
        assert!(matches!(read(&[0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF]), Err(Error::Truncated)));
        assert!(matches!(read(&[0x1F, 0x01, 0x00]), Err(Error::BadTag)));
        let (t, rest) = read(&[0x04, 0x81, 0x01, 0xAB, 0xCD]).unwrap();
        assert_eq!((t.tag, t.content, rest), (4, &[0xAB][..], &[0xCD][..]));
    }

    #[test]
    fn integers() {
        assert!(same_integer(&[0, 0, 5], &[5]));
        assert!(same_integer(&[], &[0]));
        assert!(!same_integer(&[5, 0], &[5]));
    }
}
