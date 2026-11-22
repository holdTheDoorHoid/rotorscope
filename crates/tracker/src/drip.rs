//! DRIP (RFC 9374 / RFC 9575) authentication, verified with the `remoteid`
//! crate: Links, Wrappers, Manifests and Frames that arrive whole in one
//! message pack (Wi-Fi, Bluetooth 5). Keys are learned from Links (the
//! child DET is bound to its Host Identity by the ORCHID hash), so a later
//! Wrapper or Manifest from that DET is checked without any configured key.

use remoteid::auth::{AuthContext, AuthResult, AuthScheme, DripEd25519, FixedKeyStore};
use remoteid::drip::{AssembledAuth, AuthAssembler, Det};
use rid::RawMessage;

use crate::Track;
use crate::spoof::Check;

/// Aircraft and authority keys the hub remembers (oldest learned replaced).
pub const MAX_KEYS: usize = 16;

pub type KeyStore = FixedKeyStore<MAX_KEYS>;

/// The best a track's authentication messages have shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DripStatus {
    /// No complete DRIP message checked.
    #[default]
    None,
    /// A DRIP message whose key the hub does not have yet.
    Unverified,
    /// A signature verified under the aircraft's own (self-certified) key.
    Valid,
    /// A signature verified under a key that chains to a trusted anchor.
    Trusted,
}

impl DripStatus {
    pub const fn text(self) -> &'static str {
        match self {
            DripStatus::None => "message present, not checked",
            DripStatus::Unverified => "DRIP, key not yet known",
            DripStatus::Valid => "DRIP signature valid",
            DripStatus::Trusted => "DRIP signature valid, trusted key",
        }
    }
}

/// Checks the authentication pages of one frame against the track.
pub(crate) fn check(t: &mut Track, raw: &[RawMessage], keys: &mut KeyStore, now_unix: Option<u64>) {
    let mut pages: heapless::Vec<remoteid::Auth, { rid::MAX_PACK }> = heapless::Vec::new();
    let mut dets: heapless::Vec<Det, { rid::MAX_PACK }> = heapless::Vec::new();
    for m in raw {
        match remoteid::Message::decode(m) {
            Ok(remoteid::Message::Auth(a)) => drop(pages.push(a)),
            Ok(remoteid::Message::BasicId(b)) => {
                if let Some(d) = Det::from_basic_id(&b) {
                    let _ = dets.push(d);
                }
            }
            _ => {}
        }
    }
    let mut msg = AssembledAuth::default();
    if pages.is_empty() || !AuthAssembler::push_pack(&pages, &mut msg) {
        return;
    }
    let ctx = AuthContext {
        uas_dets: &dets,
        recent_messages: raw,
        pack_messages: raw,
        now_unix,
        skew_s: 30,
        ..AuthContext::default()
    };
    let s = &mut t.spoof;
    let status = match DripEd25519.verify(&msg, &ctx, keys) {
        AuthResult::Valid { trusted: true, .. } => DripStatus::Trusted,
        AuthResult::Valid { .. } => DripStatus::Valid,
        AuthResult::Unverifiable => DripStatus::Unverified,
        AuthResult::Invalid(reason) => {
            s.raise(Check::AuthInvalid, 90, reason as i32);
            return;
        }
        AuthResult::NotHandled | AuthResult::Unsupported => return,
    };
    s.drip = s.drip.max(status);
}
