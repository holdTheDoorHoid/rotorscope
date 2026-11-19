//! A small, fixed export bundle for the time-stamp tests: two boots
//! uploaded as two batches, every value deterministic (the test key signs
//! with RFC 6979 nonces), so the access-log imprint it yields is always the
//! same and canned tokens made for it (tests/fixtures/tsa) stay valid.

use evidence::verify::bundle::{self, AuditLine, Batch, Bundle, Device, Timestamp};
use evidence::verify::hex;

use super::unit::Unit;

/// The audit entry the canned tokens stamp: the second upload.
pub const STAMPED_N: u64 = 2;
/// When the server logged each upload (server clock, ms).
pub const UPLOAD_MS: [u64; 2] = [1_795_000_000_000, 1_795_000_600_000];

pub struct Built {
    pub bundle: Bundle,
    /// Hash of each audit entry.
    pub hashes: Vec<[u8; 32]>,
}

impl Built {
    pub fn imprint(&self, n: u64) -> [u8; 32] {
        bundle::audit_imprint(&self.bundle.device.key_id, n, &self.hashes[n as usize])
    }

    /// An access-log stamp of entry `n` holding `token` (a reply as received).
    pub fn stamp(&self, n: u64, authority: &str, token: &[u8], nonce: &str) -> Timestamp {
        Timestamp {
            kind: "rfc3161".into(),
            authority: authority.into(),
            imprint: hex(&self.imprint(n)),
            covers: bundle::COVERS_AUDIT_HEAD.into(),
            audit_n: Some(n),
            audit_hash: Some(hex(&self.hashes[n as usize])),
            queued_utc_ms: Some(UPLOAD_MS[1]),
            requested_utc_ms: Some(UPLOAD_MS[1] + 1000),
            nonce: Some(nonce.into()),
            token: evidence::b64::encode_vec(token),
            ..Timestamp::default()
        }
    }

    /// Add `t` and the audit entry the server writes for it.
    pub fn add_stamp(&mut self, t: Timestamp) {
        let token = evidence::b64::decode_vec(&t.token).unwrap();
        let detail = serde_json::json!({ "authority": t.authority, "covers": t.covers, "audit_n": t.audit_n, "imprint": t.imprint, "token_sha256": hex(&evidence::sha256(&token)) });
        self.push("stamper", "timestamp", detail, UPLOAD_MS[1] + 2000);
        self.bundle.timestamps.push(t);
    }

    pub fn push(&mut self, actor: &str, action: &str, detail: serde_json::Value, utc_ms: u64) {
        let n = self.bundle.audit.len() as u64;
        let e = bundle::AuditEntry { n, utc_ms, actor: actor.into(), action: action.into(), device: self.bundle.device.key_id.clone(), detail };
        let entry = serde_json::to_string(&e).unwrap();
        let prev = self.hashes.last().copied().unwrap_or([0; 32]);
        let h = bundle::audit_hash(&prev, &entry);
        self.hashes.push(h);
        self.bundle.audit.push(AuditLine { entry, hash: hex(&h) });
    }

    pub fn json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&self.bundle).unwrap()
    }
}

pub fn build() -> Built {
    let mut u = Unit::new();
    u.utc_base = Some(1_794_990_000_000);
    let a = u.boot(1, 6, 0, "first boot");
    let b = u.boot(2, 6, 0, "second boot");
    let pk = evidence::Signer::public_key(&u.signer);
    let key_id = std::str::from_utf8(&evidence::fingerprint(&pk)).unwrap().to_string();
    let mut built = Built {
        bundle: Bundle {
            format: bundle::FORMAT.into(),
            created_utc_ms: 1_795_100_000_000,
            server: "test".into(),
            device: Device { key_id: key_id.clone(), public_key: hex(&pk), name: "fixture unit".into(), registered_utc_ms: 1_794_900_000_000 },
            batches: Vec::new(),
            audit: Vec::new(),
            firmware_hashes: vec![hex(&[0xF1; 32])],
            timestamps: Vec::new(),
            stamping: Some(bundle::Stamping { enabled: true, authorities: vec!["https://tsa-a.test/tsr".into(), "https://tsa-b.test/tsr".into()], ..Default::default() }),
        },
        hashes: Vec::new(),
    };
    built.push("t", "register", serde_json::json!({ "public_key": hex(&pk), "name": "fixture unit", "key_id": key_id }), 1_794_900_000_000);
    for (i, bytes) in [a.bytes, b.bytes].into_iter().enumerate() {
        let n = i as u64 + 1;
        let sha = hex(&evidence::sha256(&bytes));
        built.push("uploader", "upload", serde_json::json!({ "batch": n, "sha256": sha, "bytes": bytes.len() }), UPLOAD_MS[i]);
        built.bundle.batches.push(Batch { n, received_utc_ms: UPLOAD_MS[i], sha256: sha, records: evidence::b64::encode_vec(&bytes) });
    }
    built
}
