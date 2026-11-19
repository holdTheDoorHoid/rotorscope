//! OpenTimestamps calendars on the wire, and the second kind of
//! [`Stamper`](crate::stamp::Stamper) (WP-14d; use: docs/EVIDENCE.md,
//! "OpenTimestamps"). The proof format itself: `evidence::ots`.
//!
//! **What leaves the server**, per stamp and calendar: one POST of 32 bytes,
//! `SHA-256(imprint || 16 random bytes)`, to `<calendar>/digest`. Not even
//! the imprint leaves: the random salt (kept in the proof) hides it. Later,
//! to complete the proof, one GET per waiting proof of
//! `<calendar>/timestamp/<hex of the calendar's own commitment>` (a value
//! the calendar sent back). Fixed headers (below), no cookies, no proxy
//! from the environment, no redirects followed; the GET goes to the
//! configured calendar URL only, never to a host named inside a reply.
//!
//! **What comes back** is checked before anything is kept: a reply is at
//! most [`ots::MAX_REPLY`] bytes, must parse within the proof limits
//! (depth, items, message sizes), and must start from exactly what was sent
//! (a submit reply: lead from the sent digest to an attestation; an upgrade:
//! lead from the commitment asked for to a Bitcoin attestation). The pending
//! proof is kept at once; it is a calendar's promise, shown as such. The
//! completed proof is kept as a further line; nothing is rewritten.
//!
//! The protocol follows the reference client (python-opentimestamps,
//! `opentimestamps/calendar.py`): POST `digest` with the raw bytes, GET
//! `timestamp/<hex>`, `Accept: application/vnd.opentimestamps.v1`, reply
//! capped at 10 000 bytes, 404 meaning "not (yet) known".

use std::time::Duration;

use evidence::ots::{self, Attestation, Op, Proof, Timestamp};
use evidence::verify::{hex, url_host};

use crate::stamp::{Stamped, Stamper};
use crate::store::now_ms;
use crate::tsa;

/// Says only what the client speaks.
pub const USER_AGENT: &str = "ots-client";
/// What the reference client asks for.
pub const ACCEPT: &str = "application/vnd.opentimestamps.v1";
/// What the reference client's HTTP library (Python urllib) sends with a
/// POST body; calendars read the body whatever it says.
pub const CONTENT_TYPE: &str = "application/x-www-form-urlencoded";
/// Bytes of salt appended to the imprint before hashing (as the reference
/// client: 16).
pub const SALT_LEN: usize = 16;

/// A calendar URL: `http(s)://host[:port][/path]`, no query or fragment;
/// stored without a trailing `/`.
pub fn parse_calendar(spec: &str) -> Result<String, String> {
    let u = spec.trim().trim_end_matches('/');
    let web = u.starts_with("https://") || u.starts_with("http://");
    if !web || url_host(u).is_empty() || u.contains(['?', '#', '@', ' ', ',']) {
        return Err(format!("{spec:?}: a calendar is an http:// or https:// URL with no query, fragment or login"));
    }
    Ok(u.to_string())
}

/// The built-in calendars: only those whose operator's own pages say
/// public use is free (docs/EVIDENCE.md, "OpenTimestamps calendars"). Both
/// are run by the same person (the OpenTimestamps project), so together
/// they are one operator; they are two for availability, not independence.
pub fn default_calendars() -> Vec<String> {
    vec!["https://alice.btc.calendar.opentimestamps.org".into(), "https://bob.btc.calendar.opentimestamps.org".into()]
}

/// What is sent: SHA-256(imprint || salt).
pub fn submitted_digest(imprint: &[u8; 32], salt: &[u8; SALT_LEN]) -> [u8; 32] {
    evidence::sha256(&[imprint.as_slice(), salt].concat())
}

/// The proof kept after a submit: imprint, append salt, SHA-256, then the
/// calendar's reply. The reply must lead from the sent digest to a pending
/// attestation naming `calendar`'s own host (the only place the server will
/// ask to complete it) or to a Bitcoin attestation. A `.ots` file, as bytes.
pub fn pending_proof(calendar: &str, imprint: &[u8; 32], salt: &[u8; SALT_LEN], reply: &[u8]) -> Result<Vec<u8>, String> {
    if reply.len() > ots::MAX_REPLY {
        return Err(format!("calendar reply longer than {} bytes", ots::MAX_REPLY));
    }
    let r = ots::parse_timestamp(reply).map_err(|e| format!("calendar reply is not a timestamp: {e}"))?;
    let sent = submitted_digest(imprint, salt);
    let w = r.walk(&sent);
    if w.reached.is_empty() {
        return Err("calendar reply leads to no attestation from the digest sent".into());
    }
    let host = url_host(calendar);
    if w.bitcoin().is_empty() && !w.pending().iter().any(|(uri, _)| url_host(uri) == host) {
        return Err(format!("calendar reply holds no promise from {host} itself (only from other calendars)"));
    }
    let after_salt = Timestamp { attestations: Vec::new(), ops: vec![(Op::Sha256, r)] };
    let proof = Proof { digest: *imprint, timestamp: Timestamp { attestations: Vec::new(), ops: vec![(Op::Append(salt.to_vec()), after_salt)] } };
    checked_bytes(&proof)
}

/// Serialised, within the limits a checker applies, and parsing back to
/// the same proof.
fn checked_bytes(p: &Proof) -> Result<Vec<u8>, String> {
    let b = p.to_bytes();
    match ots::parse_proof(&b) {
        Ok(q) if q == *p => Ok(b),
        Ok(_) => Err("the proof does not read back as built".into()),
        Err(e) => Err(format!("the proof would be refused by a checker: {e}")),
    }
}

/// Complete a pending proof. For each pending attestation naming `calendar`'s
/// host, `fetch(commitment)` asks the calendar (`Ok(None)`: not yet). A reply
/// is used only if it leads from that commitment to a Bitcoin attestation.
/// `Ok(None)`: nothing new yet; `Ok(Some(proof))`: the completed proof.
pub fn upgraded_proof(token: &[u8], calendar: &str, mut fetch: impl FnMut(&[u8]) -> Result<Option<Vec<u8>>, String>) -> Result<Option<Vec<u8>>, String> {
    let mut p = ots::parse_proof(token).map_err(|e| format!("the stored proof cannot be read: {e}"))?;
    let host = url_host(calendar);
    let pending: Vec<(String, Vec<u8>)> = p.walk().pending().into_iter().filter(|(uri, _)| url_host(uri) == host).collect();
    if pending.is_empty() {
        return Err(format!("the stored proof has no pending attestation from {host}"));
    }
    let mut grafted = 0;
    for (_, commitment) in pending {
        let Some(reply) = fetch(&commitment)? else { continue };
        if reply.len() > ots::MAX_REPLY {
            return Err(format!("calendar reply longer than {} bytes", ots::MAX_REPLY));
        }
        let up = ots::parse_timestamp(&reply).map_err(|e| format!("calendar reply is not a timestamp: {e}"))?;
        if up.walk(&commitment).bitcoin().is_empty() {
            continue; // Not anchored yet (a calendar may answer with another promise).
        }
        let digest = p.digest;
        grafted += p.timestamp.graft(&digest, &commitment, &up);
    }
    if grafted == 0 {
        return Ok(None);
    }
    checked_bytes(&p).map(Some)
}

/// The proof is anchored (reaches a Bitcoin attestation); with the heights.
pub fn anchors(token: &[u8]) -> Option<Vec<u64>> {
    let p = ots::parse_proof(token).ok()?;
    let mut h: Vec<u64> = p.walk().bitcoin().into_iter().map(|(h, _)| h).collect();
    h.sort();
    h.dedup();
    Some(h)
}

/// One calendar.
pub struct Calendar {
    url: String,
    agent: ureq::Agent,
}

impl Calendar {
    pub fn new(url: String, timeout: Duration) -> Self {
        Calendar { url, agent: tsa::agent_with(timeout, USER_AGENT) }
    }

    /// POST the digest; the reply body.
    pub fn submit(&self, digest: &[u8; 32]) -> Result<Vec<u8>, String> {
        let resp = self
            .agent
            .post(&format!("{}/digest", self.url))
            .set("Accept", ACCEPT)
            .set("Content-Type", CONTENT_TYPE)
            .set("Accept-Encoding", "identity")
            .send_bytes(digest)
            .map_err(tsa::short_error)?;
        tsa::checked_body(resp, ots::MAX_REPLY)
    }

    /// GET the timestamp for `commitment`; `None` if the calendar does not
    /// have it complete yet (404).
    pub fn fetch(&self, commitment: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let r = self.agent.get(&format!("{}/timestamp/{}", self.url, hex(commitment))).set("Accept", ACCEPT).set("Accept-Encoding", "identity").call();
        match r {
            Ok(resp) => tsa::checked_body(resp, ots::MAX_REPLY).map(Some),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(tsa::short_error(e)),
        }
    }
}

impl Stamper for Calendar {
    fn authority(&self) -> &str {
        &self.url
    }

    fn kind(&self) -> &'static str {
        evidence::verify::bundle::KIND_OTS
    }

    fn stamp(&self, imprint: &[u8; 32]) -> Result<Stamped, String> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::getrandom(&mut salt).map_err(|e| e.to_string())?;
        let requested_utc_ms = now_ms();
        let reply = self.submit(&submitted_digest(imprint, &salt))?;
        let token = pending_proof(&self.url, imprint, &salt, &reply)?;
        Ok(Stamped { token, nonce: None, gen_time_ms: None, requested_utc_ms, certs: None, revocation: None, evidence_note: None })
    }

    fn upgrade(&self, token: &[u8]) -> Result<Option<Vec<u8>>, String> {
        upgraded_proof(token, &self.url, |c| self.fetch(c))
    }
}

/// For the page and the audit entry: "pending" or "anchored (Bitcoin block
/// N, not checked here)".
pub fn state(token: &[u8]) -> String {
    match anchors(token) {
        None => "unreadable".into(),
        Some(h) if h.is_empty() => "pending".into(),
        Some(h) => format!("anchored (Bitcoin block {}, claimed by the calendar; checked with evidence-verify against a header you trust)", h.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")),
    }
}

/// A pending attestation's calendar, for messages.
pub fn pending_calendars(token: &[u8]) -> Vec<String> {
    ots::parse_proof(token).map(|p| p.walk().reached.into_iter().filter_map(|(_, a)| if let Attestation::Pending { uri } = a { Some(uri) } else { None }).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_specs() {
        assert_eq!(parse_calendar(" https://cal.example/ ").unwrap(), "https://cal.example");
        assert_eq!(parse_calendar("http://127.0.0.1:9/x").unwrap(), "http://127.0.0.1:9/x");
        for bad in ["", "cal.example", "ftp://x", "https://", "https://x/?a=1", "https://x/#f", "https://u@x", "https://a,b"] {
            assert!(parse_calendar(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_submit_reply_must_start_from_what_was_sent() {
        let imprint = [7u8; 32];
        let salt = [9u8; SALT_LEN];
        let att = Attestation::Pending { uri: "https://cal.test".into() };
        let good = Timestamp::chain(vec![Op::Append(vec![1, 2]), Op::Sha256], att.clone());
        let p = pending_proof("https://cal.test", &imprint, &salt, &good.to_bytes()).unwrap();
        let w = ots::parse_proof(&p).unwrap().walk();
        assert_eq!(w.pending().len(), 1);
        // A reply whose ops cannot be replayed from the digest reaches nothing.
        let dead = Timestamp::chain(vec![Op::Keccak256], att);
        assert!(pending_proof("https://cal.test", &imprint, &salt, &dead.to_bytes()).unwrap_err().contains("no attestation"));
        assert!(pending_proof("https://cal.test", &imprint, &salt, b"<html>").is_err());
        assert!(pending_proof("https://cal.test", &imprint, &salt, &vec![0x08; ots::MAX_REPLY + 1]).unwrap_err().contains("longer than"));
        // A promise only from another calendar: the server would never ask there.
        assert!(pending_proof("https://other.test", &imprint, &salt, &good.to_bytes()).unwrap_err().contains("no promise from other.test"));
    }
}
