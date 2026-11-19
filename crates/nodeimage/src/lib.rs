//! Which firmware images a node holds, which roles they give it, and the job
//! a node carries across an image switch.
//!
//! A C5 node's flash holds several app images side by side in ESP-IDF OTA
//! slots (`tools/partitions-node.csv`): node-wifi, node-ble and, when built,
//! node-sdr. The running image reads the first bytes of every app slot,
//! recognises each image by the project name in its ESP-IDF app descriptor
//! ([`project_name`]), and reports the union of their roles to the hub as
//! `link::Frame::Status::can`. When the hub assigns a role the running image
//! cannot do, the image writes the assignment to a one-sector partition
//! ([`encode_job`]), marks the other slot bootable and resets; the new image
//! reads the job back ([`decode_job`]) and starts with it.
//!
//! Also the node's half of the per-node link keys (docs/LINK-KEYS-DESIGN.md):
//! finding the `rskey` provisioning partition ([`keystore`]) and the link
//! logic every ESP-NOW image shares ([`uplink`], feature `uplink`).
//!
//! Pure logic only (no flash access), so it is tested on the host; the flash
//! glue is `firmware/node-common`.
#![cfg_attr(not(test), no_std)]

pub mod deaf;
pub mod keystore;
pub mod switchlimit;
#[cfg(any(test, feature = "uplink"))]
pub mod uplink;

use link::{Assignment, NodeRole};

/// First byte of an ESP-IDF app image.
pub const IMAGE_MAGIC: u8 = 0xE9;
/// `esp_app_desc_t::magic_word`.
pub const APP_DESC_MAGIC: u32 = 0xABCD_5432;
/// The app descriptor starts the first segment: 24-byte image header, then the
/// segment's 8-byte header (load address, length).
pub const APP_DESC_OFFSET: usize = 24 + 8;
/// Bytes of an image to read to find its project name: up to the end of
/// `esp_app_desc_t::project_name` (magic, secure version, 2 reserved words,
/// 32-byte version, 32-byte name).
pub const HEAD_LEN: usize = APP_DESC_OFFSET + 16 + 32 + 32;

/// Label of the one-sector data partition that holds the job across a switch.
pub const JOB_LABEL: &str = "rsjob";

/// The firmware images a node can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// `firmware/node-wifi`: Wi-Fi 2.4 or 5 GHz, chosen at run time on a C5.
    Wifi,
    /// `firmware/node-ble`: Bluetooth Remote ID.
    Ble,
    /// `firmware/node-sdr`: raw capture (DroneID bursts, FPV / RF scan).
    Sdr,
}

impl Image {
    pub const ALL: [Image; 3] = [Image::Wifi, Image::Ble, Image::Sdr];

    /// The Cargo package name, which `esp_app_desc!()` puts in the descriptor.
    pub const fn project(self) -> &'static str {
        match self {
            Image::Wifi => "node-wifi",
            Image::Ble => "node-ble",
            Image::Sdr => "node-sdr",
        }
    }

    /// Roles this image can take, as a mask of [`NodeRole::bit`]. node-wifi
    /// counts as both bands: on a C5 the band is a run-time choice. (An S3
    /// node-wifi is 2.4 GHz only, but an S3 node never switches images and
    /// reports its own mask.)
    pub const fn roles(self) -> u8 {
        match self {
            Image::Wifi => NodeRole::Wifi24.bit() | NodeRole::Wifi5.bit(),
            Image::Ble => NodeRole::Ble.bit(),
            Image::Sdr => NodeRole::DroneId.bit() | NodeRole::Fpv.bit(),
        }
    }

    pub fn from_project(name: &str) -> Option<Image> {
        Image::ALL.into_iter().find(|i| i.project() == name)
    }

    /// The image that does `role`, if any (a gateway is its own firmware).
    pub fn for_role(role: NodeRole) -> Option<Image> {
        Image::ALL.into_iter().find(|i| i.roles() & role.bit() != 0)
    }
}

/// The project name in an app image's descriptor, given the image's first
/// [`HEAD_LEN`] bytes (or more). `None` for erased flash, a non-image, or an
/// image without a descriptor.
pub fn project_name(head: &[u8]) -> Option<&str> {
    if *head.first()? != IMAGE_MAGIC {
        return None;
    }
    let desc = head.get(APP_DESC_OFFSET..HEAD_LEN)?;
    if u32::from_le_bytes(desc[..4].try_into().ok()?) != APP_DESC_MAGIC {
        return None;
    }
    let name = &desc[16 + 32..16 + 64];
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    core::str::from_utf8(&name[..end]).ok().filter(|s| !s.is_empty())
}

/// The image in a slot, from its first bytes.
pub fn image_of(head: &[u8]) -> Option<Image> {
    project_name(head).and_then(Image::from_project)
}

// ------------------------------------------------------------------ job --

/// Start of a job record. `RSJ2` adds one byte after the length: the
/// switch streak ([`switchlimit`]).
pub const JOB_MAGIC: [u8; 4] = *b"RSJ2";
/// The record before the streak byte. Still read: as streak 0 (it was
/// written by firmware that had no limit; the limit starts from scratch).
pub const JOB_MAGIC_V1: [u8; 4] = *b"RSJ1";
/// Largest record: magic, length, streak, postcard `Assignment` (at most
/// ~200 bytes for 24 hops), CRC-32.
pub const JOB_MAX: usize = 256;

/// A job read back from `rsjob`: the assignment the node switched for, and
/// the switch streak the switching image wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredJob {
    pub job: Assignment,
    pub streak: u8,
}

/// Serialize `job` into `buf`: magic `RSJ2`, payload length (u16 LE),
/// `streak` (u8), postcard payload, CRC-32 (IEEE, LE) over everything
/// before it. Returns the bytes to write (pad to the flash write size with
/// 0xFF; trailing bytes are ignored).
pub fn encode_job<'a>(job: &Assignment, streak: u8, buf: &'a mut [u8; JOB_MAX]) -> Option<&'a [u8]> {
    buf[..4].copy_from_slice(&JOB_MAGIC);
    let n = postcard::to_slice(job, &mut buf[7..JOB_MAX - 4]).ok()?.len();
    buf[4..6].copy_from_slice(&(n as u16).to_le_bytes());
    buf[6] = streak;
    let crc = crc32(&buf[..7 + n]);
    buf[7 + n..11 + n].copy_from_slice(&crc.to_le_bytes());
    Some(&buf[..11 + n])
}

/// Read a job record back: `RSJ2`, or an older `RSJ1` (no streak byte; read
/// as streak 0). `None` for erased flash or anything damaged.
pub fn decode_job(raw: &[u8]) -> Option<StoredJob> {
    let head = match raw.get(..4)? {
        m if m == JOB_MAGIC => 7,
        m if m == JOB_MAGIC_V1 => 6,
        _ => return None,
    };
    let n = usize::from(u16::from_le_bytes(raw.get(4..6)?.try_into().ok()?));
    let body = raw.get(..head + n)?;
    let crc = u32::from_le_bytes(raw.get(head + n..head + n + 4)?.try_into().ok()?);
    if crc32(body) != crc {
        return None;
    }
    let streak = if head == 7 { body[6] } else { 0 };
    Some(StoredJob { job: postcard::from_bytes(&body[head..]).ok()?, streak })
}

/// CRC-32 (IEEE 802.3, reflected, as zlib). Bitwise: the record is tiny.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(name: &str) -> std::vec::Vec<u8> {
        let mut h = std::vec![0u8; HEAD_LEN + 16];
        h[0] = IMAGE_MAGIC;
        h[APP_DESC_OFFSET..APP_DESC_OFFSET + 4].copy_from_slice(&APP_DESC_MAGIC.to_le_bytes());
        h[APP_DESC_OFFSET + 16..APP_DESC_OFFSET + 21].copy_from_slice(b"0.1.0");
        let at = APP_DESC_OFFSET + 48;
        h[at..at + name.len()].copy_from_slice(name.as_bytes());
        h
    }

    #[test]
    fn recognises_images() {
        assert_eq!(image_of(&head("node-wifi")), Some(Image::Wifi));
        assert_eq!(image_of(&head("node-ble")), Some(Image::Ble));
        assert_eq!(image_of(&head("node-sdr")), Some(Image::Sdr));
        assert_eq!(project_name(&head("gateway")), Some("gateway"));
        assert_eq!(image_of(&head("gateway")), None);
        assert_eq!(image_of(&[0xFF; HEAD_LEN]), None, "erased slot");
        let mut bad = head("node-ble");
        bad[APP_DESC_OFFSET] ^= 1;
        assert_eq!(image_of(&bad), None, "no descriptor");
        assert_eq!(image_of(&head("node-ble")[..HEAD_LEN - 1]), None, "too short");
        // A 32-byte name with no terminator still reads.
        let long = "x".repeat(32);
        assert_eq!(project_name(&head(&long)), Some(long.as_str()));
    }

    #[test]
    fn roles_cover_every_assignable_role_once() {
        let all = Image::ALL.iter().fold(0u8, |m, i| {
            assert_eq!(m & i.roles(), 0, "{i:?} overlaps");
            m | i.roles()
        });
        for r in [NodeRole::Wifi24, NodeRole::Wifi5, NodeRole::Ble, NodeRole::DroneId, NodeRole::Fpv] {
            assert!(all & r.bit() != 0);
            assert!(Image::for_role(r).unwrap().roles() & r.bit() != 0);
        }
        assert_eq!(Image::for_role(NodeRole::Gateway), None);
        assert_eq!(Image::for_role(NodeRole::Ble), Some(Image::Ble));
        assert_eq!(Image::for_role(NodeRole::Wifi5), Some(Image::Wifi));
    }

    #[test]
    fn job_round_trip_and_damage() {
        let mut plan = heapless::Vec::new();
        while plan.push((u32::MAX, u16::MAX)).is_ok() {}
        let big = Assignment { role: NodeRole::DroneId, plan, id: u16::MAX };
        let small = Assignment { role: NodeRole::Ble, plan: heapless::Vec::new(), id: 0x1234 };
        for (job, streak) in [(big, 0xFE), (small, 3)] {
            let mut buf = [0u8; JOB_MAX];
            let raw = encode_job(&job, streak, &mut buf).unwrap().to_vec();
            assert!(raw.len() <= JOB_MAX);
            assert_eq!(raw[..4], *b"RSJ2");
            let mut sector = std::vec![0xFFu8; 4096];
            sector[..raw.len()].copy_from_slice(&raw);
            assert_eq!(decode_job(&sector), Some(StoredJob { job: job.clone(), streak }));
            for i in 0..raw.len() {
                let mut d = sector.clone();
                d[i] ^= 0x40;
                assert_eq!(decode_job(&d), None, "flip at {i}");
            }
        }
        assert_eq!(decode_job(&[0xFF; 4096]), None);
        assert_eq!(decode_job(&[]), None);
    }

    /// An `RSJ1` record (written by firmware from before the switch limit)
    /// is still read, as streak 0.
    #[test]
    fn old_rsj1_record_reads_as_streak_0() {
        let job = Assignment { role: NodeRole::Wifi5, plan: heapless::Vec::from_slice(&[(149, 150), (6, 40)]).unwrap(), id: 77 };
        // Exactly what the RSJ1 encoder wrote.
        let mut raw = std::vec::Vec::from(*b"RSJ1");
        let mut pbuf = [0u8; 256];
        let payload = postcard::to_slice(&job, &mut pbuf).unwrap();
        raw.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        raw.extend_from_slice(&payload);
        let crc = crc32(&raw);
        raw.extend_from_slice(&crc.to_le_bytes());
        raw.resize(4096, 0xFF);
        assert_eq!(decode_job(&raw), Some(StoredJob { job, streak: 0 }));
        raw[7] ^= 1;
        assert_eq!(decode_job(&raw), None, "damaged RSJ1");
    }

    #[test]
    fn crc_matches_zlib() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
