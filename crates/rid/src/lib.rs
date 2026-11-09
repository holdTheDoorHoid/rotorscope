//! Remote ID decoding with no allocator and no `std`.
//!
//! Three layers, each usable on its own:
//! - [`wifi::extract`] / [`ble::extract`] find the Remote ID payload inside a raw
//!   802.11 frame or a BLE advertisement.
//! - [`message::raw_messages`] splits a payload (one message or a Message Pack)
//!   into 25-byte raw messages.
//! - [`message::decode`] turns one raw message into a typed [`message::Message`].
//! - [`encode`] (feature `encode`) is the inverse, for test transmitters.
//!
//! Two non-ASTM formats live alongside: [`dji`] (DJI's legacy Wi-Fi DroneID
//! beacons) and [`cn`] (China's GB 46750-2025 broadcast packet).
//!
//! Nodes forward *raw* messages to the hub; decoding happens wherever it is
//! needed, always with this crate, so every chip agrees on what a byte means.
#![no_std]

/// Defines a code enum with named values and a `Reserved(u8)` escape for any
/// value the standard does not (yet) assign.
macro_rules! code_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident = $val:literal,)* }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize))]
        pub enum $name {
            $($(#[$vm])* $var,)*
            /// A value the standard reserves or leaves for private use.
            Reserved(u8),
        }
        impl $name {
            /// Decode the wire code.
            pub fn from_code(code: u8) -> Self {
                match code {
                    $($val => Self::$var,)*
                    other => Self::Reserved(other),
                }
            }
            /// The wire code.
            pub fn code(self) -> u8 {
                match self {
                    $(Self::$var => $val,)*
                    Self::Reserved(other) => other,
                }
            }
        }
    };
}

pub mod ble;
pub mod cn;
pub mod dji;
#[cfg(feature = "encode")]
pub mod encode;
pub mod message;
pub mod wifi;

/// Every ASTM F3411 message is exactly this long.
pub const MESSAGE_SIZE: usize = 25;
/// The most messages a Message Pack can carry.
pub const MAX_PACK: usize = 9;

pub type RawMessage = [u8; MESSAGE_SIZE];

/// Which over-the-air method carried a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Transport {
    WifiBeacon,
    WifiNan,
    /// Bluetooth 4 legacy advertising (single message, no packs).
    BleLegacy,
    /// Bluetooth 5 extended advertising on the 1M PHY.
    BleExtended,
    /// Bluetooth 5 Long Range (coded PHY).
    BleLongRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Error {
    /// Input shorter than its own length fields claim.
    Truncated,
    /// Message type nibble is not one F3411 defines.
    UnknownType(u8),
    /// Message Pack header is inconsistent (bad size or count).
    BadPack,
    /// A field holds a value the standard reserves or forbids.
    Invalid,
    /// (Encoding only) the output buffer cannot hold the result.
    BufferTooSmall,
}
