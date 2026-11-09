//! Finding Remote ID inside BLE advertising data.
//!
//! Remote ID rides in an AD structure of type Service Data (0x16), UUID
//! `0xFFFA`, application code `0x0D`, then a message counter, then the payload.

/// AD type: Service Data, 16-bit UUID.
pub const AD_SERVICE_DATA_16: u8 = 0x16;
/// The ASTM Remote ID service UUID, as transmitted (little-endian `0xFFFA`).
pub const UUID: [u8; 2] = [0xFA, 0xFF];
/// Application code that follows the UUID.
pub const APP_CODE: u8 = 0x0D;

/// A Remote ID payload located inside one advertisement. Borrowed, no copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BleRid<'a> {
    pub counter: u8,
    /// One raw message (legacy) or a Message Pack (extended / Long Range).
    pub payload: &'a [u8],
}

/// Iterate AD structures as `(type, data)`. Stops at a zero length byte (the
/// rest is padding) or at a structure that runs past the end.
pub fn ad_structures(adv_data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut rest = adv_data;
    core::iter::from_fn(move || {
        let (&len, after) = rest.split_first()?;
        let len = usize::from(len);
        if len == 0 {
            return None;
        }
        let structure = after.get(..len)?;
        rest = &after[len..];
        let (&ad_type, data) = structure.split_first()?;
        Some((ad_type, data))
    })
}

/// `adv_data` is the advertising data payload (a sequence of length-type-value
/// AD structures), not including the PDU header or advertiser address.
/// Returns the first Remote ID service data structure found.
pub fn extract(adv_data: &[u8]) -> Option<BleRid<'_>> {
    ad_structures(adv_data).find_map(|(ad_type, data)| {
        if ad_type != AD_SERVICE_DATA_16 || data.get(..2)? != UUID || *data.get(2)? != APP_CODE {
            return None;
        }
        Some(BleRid {
            counter: *data.get(3)?,
            payload: data.get(4..)?,
        })
    })
}
