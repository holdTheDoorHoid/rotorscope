//! Finding Remote ID inside raw 802.11 management frames.
//!
//! Two carriers: a beacon carrying a vendor-specific element (OUI `FA:0B:BC`,
//! type `0x0D`), and a NAN Service Discovery action frame sent to
//! `51:6F:9A:01:00:00`.
//!
//! The NAN layout follows core-c's `odid_wifi_build_message_pack_nan_action_frame`,
//! but parsing is by structure rather than fixed offsets: NAN attributes are
//! walked until the Service Descriptor attribute with the Remote ID service ID
//! is found, and its optional fields are skipped per its Service Control bits.

use crate::Transport;

/// ASD-STAN OUI used for the beacon vendor element.
pub const BEACON_OUI: [u8; 3] = [0xFA, 0x0B, 0xBC];
/// Vendor element OUI type for Remote ID.
pub const BEACON_OUI_TYPE: u8 = 0x0D;
/// Destination address of NAN Remote ID action frames (NAN Network ID).
pub const NAN_DEST: [u8; 6] = [0x51, 0x6F, 0x9A, 0x01, 0x00, 0x00];
/// Wi-Fi Alliance OUI, used by NAN.
pub const WFA_OUI: [u8; 3] = [0x50, 0x6F, 0x9A];
/// NAN OUI type in the public action frame header and in NAN vendor elements.
pub const NAN_OUI_TYPE: u8 = 0x13;
/// NAN service ID of Remote ID: the first six bytes of SHA-256("org.opendroneid.remoteid").
pub const NAN_SERVICE_ID: [u8; 6] = [0x88, 0x69, 0x19, 0x9D, 0x92, 0x09];
/// NAN cluster ID ASTM F3411 fixes for Remote ID (used as BSSID).
pub const NAN_CLUSTER_ID: [u8; 6] = [0x50, 0x6F, 0x9A, 0x01, 0x00, 0xFF];

/// A Remote ID payload located inside one 802.11 frame. Borrowed, no copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WifiRid<'a> {
    pub transport: Transport,
    /// Transmitter address (802.11 addr2).
    pub src: [u8; 6],
    /// The F3411 message counter, incremented by the sender per transmission.
    pub counter: u8,
    /// One raw message or a Message Pack; feed to [`crate::message::raw_messages`].
    pub payload: &'a [u8],
}

pub(crate) const SUBTYPE_BEACON: u16 = 8;
const SUBTYPE_ACTION: u16 = 13;

/// An unprotected 802.11 management frame split into its parts.
pub(crate) struct Mgmt<'a> {
    pub subtype: u16,
    pub header: &'a [u8],
    pub body: &'a [u8],
    /// addr2, the transmitter.
    pub src: [u8; 6],
}

impl<'a> Mgmt<'a> {
    pub fn parse(frame: &'a [u8]) -> Option<Self> {
        let fc = u16::from_le_bytes([*frame.first()?, *frame.get(1)?]);
        // Protocol version 0, type 0 (management), not protected.
        if fc & 0x000F != 0 || fc & 0x4000 != 0 {
            return None;
        }
        // The Order bit on a management frame means a 4-byte HT Control field follows.
        let header_len = if fc & 0x8000 != 0 { 28 } else { 24 };
        let header = frame.get(..header_len)?;
        let src = header.get(10..16)?.try_into().ok()?;
        Some(Self {
            subtype: (fc >> 4) & 0x0F,
            header,
            body: &frame[header_len..],
            src,
        })
    }

    /// The information elements of a beacon (after its 12 fixed bytes).
    pub fn beacon_ies(&self) -> Option<&'a [u8]> {
        if self.subtype != SUBTYPE_BEACON {
            return None;
        }
        self.body.get(12..)
    }
}

/// `frame` starts at the 802.11 frame-control field and excludes the FCS.
/// Returns `None` for anything that is not a Remote ID beacon or NAN frame.
pub fn extract(frame: &[u8]) -> Option<WifiRid<'_>> {
    let Mgmt {
        subtype,
        header,
        body,
        src,
    } = Mgmt::parse(frame)?;
    let (transport, counter, payload) = match subtype {
        SUBTYPE_BEACON => {
            let (counter, payload) = beacon_payload(body)?;
            (Transport::WifiBeacon, counter, payload)
        }
        SUBTYPE_ACTION if header.get(4..10)? == NAN_DEST => {
            let (counter, payload) = nan_payload(body)?;
            (Transport::WifiNan, counter, payload)
        }
        _ => return None,
    };
    Some(WifiRid {
        transport,
        src,
        counter,
        payload,
    })
}

/// Iterate the information elements of a beacon/probe body, stopping at the
/// first one that runs past the end.
pub fn elements(ies: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut rest = ies;
    core::iter::from_fn(move || {
        let (&id, after) = rest.split_first()?;
        let (&len, after) = after.split_first()?;
        let body = after.get(..usize::from(len))?;
        rest = &after[usize::from(len)..];
        Some((id, body))
    })
}

/// Beacon body: 12 bytes of fixed fields (timestamp, interval, capability), then IEs.
fn beacon_payload(body: &[u8]) -> Option<(u8, &[u8])> {
    elements(body.get(12..)?).find_map(|(id, data)| {
        if id != 221 || data.get(..3)? != BEACON_OUI || *data.get(3)? != BEACON_OUI_TYPE {
            return None;
        }
        Some((*data.get(4)?, data.get(5..)?))
    })
}

/// Iterate NAN attributes (1-byte ID, 2-byte little-endian length, body).
fn nan_attributes(attrs: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut rest = attrs;
    core::iter::from_fn(move || {
        let id = *rest.first()?;
        let len = usize::from(u16::from_le_bytes([*rest.get(1)?, *rest.get(2)?]));
        let body = rest.get(3..3 + len)?;
        rest = &rest[3 + len..];
        Some((id, body))
    })
}

const NAN_ATTR_SERVICE_DESCRIPTOR: u8 = 0x03;

/// NAN body: public action header (category 4, action 9, WFA OUI, type 0x13),
/// then NAN attributes.
fn nan_payload(body: &[u8]) -> Option<(u8, &[u8])> {
    if body.get(..6)? != [0x04, 0x09, WFA_OUI[0], WFA_OUI[1], WFA_OUI[2], NAN_OUI_TYPE] {
        return None;
    }
    nan_attributes(&body[6..]).find_map(|(id, attr)| {
        if id != NAN_ATTR_SERVICE_DESCRIPTOR || attr.get(..6)? != NAN_SERVICE_ID {
            return None;
        }
        let service_info = service_descriptor_info(attr)?;
        let (&counter, pack) = service_info.split_first()?;
        Some((counter, pack))
    })
}

/// The Service Info field of a NAN Service Descriptor attribute body.
///
/// Layout after the 6-byte service ID: instance ID, requestor instance ID,
/// service control, then optional fields in this order, each present only if
/// its control bit is set: binding bitmap (bit 5, 2 bytes), matching filter
/// (bit 2, length-prefixed), service response filter (bit 3, length-prefixed),
/// service info (bit 4, length-prefixed).
fn service_descriptor_info(attr: &[u8]) -> Option<&[u8]> {
    let control = *attr.get(8)?;
    let mut rest = attr.get(9..)?;
    let skip_prefixed = |rest: &mut &[u8]| -> Option<()> {
        let (&len, after) = rest.split_first()?;
        *rest = after.get(usize::from(len)..)?;
        Some(())
    };
    if control & 0x20 != 0 {
        rest = rest.get(2..)?;
    }
    if control & 0x04 != 0 {
        skip_prefixed(&mut rest)?;
    }
    if control & 0x08 != 0 {
        skip_prefixed(&mut rest)?;
    }
    if control & 0x10 == 0 {
        return None;
    }
    let (&len, after) = rest.split_first()?;
    after.get(..usize::from(len))
}
