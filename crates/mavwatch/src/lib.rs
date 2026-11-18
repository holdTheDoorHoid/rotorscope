//! Hostile MAVLink on a drone's telemetry link (docs/DRONE-ATTACKS.md,
//! "Hostile MAVLink telemetry"). Receive-only and passive: this crate reads
//! frames someone else sent; it builds nothing to send (the `encode`
//! feature is a byte builder for tests).
//!
//! # What it looks for
//!
//! Three families, each a [`Kind`] whose number is the `detail` of a
//! `link::ThreatKind::MavlinkAttack` threat:
//!
//! - **Ground-station exploit payloads** (github.com/nicholasaleks/
//!   infected-drones): a vehicle, or anyone injecting on its link, handing
//!   the ground station a URI with path traversal or a local scheme, a
//!   GStreamer pipeline, Mission Planner link markup, a hostile MAVLink FTP
//!   listing or offset, or absurd parameter values.
//! - **Known autopilot holes** (OWASP drone security top 10, 2026): remote
//!   shell access through SERIAL_CONTROL and its out-of-bounds count,
//!   non-finite PARAM_SET values, over-long FTP paths, and the signing key
//!   itself going over the air.
//! - **Link integrity**: unsigned commands slipped between signed ones,
//!   two senders sharing one system/component ID, replayed signed frames.
//!
//! | detail | [`Kind`] | fires on |
//! |---|---|---|
//! | 1 | `UriTraversal` | one frame |
//! | 2 | `UriScheme` | one frame |
//! | 3 | `CameraName` | one frame |
//! | 4 | `VideoPipeline` | one frame |
//! | 5 | `StatusLink` | one frame (or two, when the markup is split) |
//! | 6 | `StatusControl` | 2 messages in 60 s |
//! | 7 | `FtpTraversal` | one frame |
//! | 8 | `FtpOffset` | one frame |
//! | 9 | `FtpMalformed` | one frame |
//! | 10 | `ParamTotal` | one frame |
//! | 11 | `ParamFlood` | ~3,100 distinct names from one system in 10 min |
//! | 12 | `ParamSetNonFinite` | one frame |
//! | 13 | `SerialControl` | one frame |
//! | 14 | `SerialControlMalformed` | one frame |
//! | 15 | `SetupSigning` | one frame |
//! | 16 | `SigningDowngrade` | 2 unsigned commands in 30 s, signed frames in the last 10 s |
//! | 17 | `SeqInterleave` | 10 switches between two live sequence tracks in 10 s, 5+ frames each |
//! | 18 | `SignedReplay` | 2 stale signed timestamps in 60 s |
//! | 19 | `OddLength` | one frame: a payload length no conforming sender uses (still analysed, cut or zero-filled, as lenient parsers decode it) |
//!
//! The one-frame kinds look for shapes that no working autopilot, camera
//! or companion computer sends (a camera definition at `file:///...`, a
//! listing entry named `../../x`): one is not a benign oddity. The kinds
//! that a fault could imitate (garbled text, sequence jumps, timestamps)
//! need repeats. Thresholds and reasons are on the constants in
//! [`analyse`].
//!
//! Only frames whose checksum verifies with their message's CRC_EXTRA are
//! looked at ([`frame`]), so radio noise never becomes a finding. Any
//! payload length is accepted, as the MAVLink C library and pymavlink
//! accept it ([`frame`] module docs). Frames
//! heard twice within [`analyse::RESEND_MS`] (radio resends) count once.
//!
//! # What it cannot see
//!
//! - Signatures are not verified (that needs the link's key): a signed
//!   frame is taken as signed by whoever holds the key.
//! - Encrypted links (SiK AES, mLRS with encryption) carry no readable
//!   MAVLink.
//! - The attack's effect on a ground station: this crate sees the bait,
//!   not whether anyone took it.
//! - Files fetched over IP (a `https://` camera definition's content, a
//!   decompression bomb behind a normal-looking `.xz` URI, MAVSDK-03).
//!
//! `no_std`, no allocation, fixed memory ([`Watch`] is about 8 KB).
#![cfg_attr(not(test), no_std)]

pub mod analyse;
pub mod crc;
pub mod frame;
pub mod msg;
pub mod stream;
pub mod text;

pub use analyse::{Analyser, Finding, Kind, Link, Pressure};
pub use frame::{Frame, Parse, Signature, frames, parse_at};
pub use stream::{Stream, StreamKey, Watch};

#[cfg(test)]
mod tests;
