//! UBX configuration frames for the MAX-M10S (CFG-VALSET / CFG-VALGET / MON-VER).
//!
//! All key IDs and enum values below were checked against **u-blox M10 SPG 5.10 Interface
//! description, protocol version 34.10, document UBX-21035062 R03** (the firmware on the
//! MAX-M10S), section 4.9 "Configuration items" and the UBX-CFG-VALSET/VALGET sections 3.10.4,
//! 3.10.5.

use crate::parser::Event;
use crate::ubx::{
    self, CLS_CFG, CLS_MON, ID_CFG_VALGET, ID_CFG_VALSET, ID_MON_VER, key_size,
};

/// Configuration key IDs (UBX-21035062 R03, section 4.9). The storage size is encoded in bits
/// 30..28 of each key (1 = L bool, 2 = 1 byte, 3 = 2 bytes, 4 = 4 bytes, 5 = 8 bytes).
pub mod keys {
    /// U1: output rate of UBX-NAV-PVT on I2C (0 = off, n = every n-th epoch).
    pub const MSGOUT_UBX_NAV_PVT_I2C: u32 = 0x2091_0006;
    /// U1: output rate of UBX-NAV-PVT on UART1.
    pub const MSGOUT_UBX_NAV_PVT_UART1: u32 = 0x2091_0007;

    /// L: UBX is an input protocol on I2C.
    pub const I2CINPROT_UBX: u32 = 0x1071_0001;
    /// L: UBX is an output protocol on I2C.
    pub const I2COUTPROT_UBX: u32 = 0x1072_0001;
    /// L: NMEA is an output protocol on I2C.
    pub const I2COUTPROT_NMEA: u32 = 0x1072_0002;
    /// L: UBX is an input protocol on UART1.
    pub const UART1INPROT_UBX: u32 = 0x1073_0001;
    /// L: UBX is an output protocol on UART1.
    pub const UART1OUTPROT_UBX: u32 = 0x1074_0001;
    /// L: NMEA is an output protocol on UART1.
    pub const UART1OUTPROT_NMEA: u32 = 0x1074_0002;
    /// U4: UART1 baud rate (firmware default 38400; the MAX-M10S module ships configured
    /// for 9600 NMEA).
    pub const UART1_BAUDRATE: u32 = 0x4052_0001;

    /// E1: dynamic platform model (0 = PORT portable, 2 = STAT, 3 = PED, 4 = AUTOMOT, ...).
    pub const NAVSPG_DYNMODEL: u32 = 0x2011_0021;

    /// E1: time pulse defined by period (0) or frequency (1).
    pub const TP_PULSE_DEF: u32 = 0x2005_0023;
    /// E1: time pulse length defined as duty ratio (0) or length in us (1).
    pub const TP_PULSE_LENGTH_DEF: u32 = 0x2005_0030;
    /// U4: period in us (used when `PULSE_DEF` = period).
    pub const TP_PERIOD_TP1: u32 = 0x4005_0002;
    /// U4: period in us once GNSS time is valid.
    pub const TP_PERIOD_LOCK_TP1: u32 = 0x4005_0003;
    /// U4: pulse length in us before lock (0 = no pulse).
    pub const TP_LEN_TP1: u32 = 0x4005_0004;
    /// U4: pulse length in us once locked.
    pub const TP_LEN_LOCK_TP1: u32 = 0x4005_0005;
    /// L: enable time pulse 1.
    pub const TP_TP1_ENA: u32 = 0x1005_0007;
    /// L: synchronise to GNSS time.
    pub const TP_SYNC_GNSS_TP1: u32 = 0x1005_0008;
    /// L: use the `*_LOCK_*` parameters once GNSS time is valid.
    pub const TP_USE_LOCKED_TP1: u32 = 0x1005_0009;
    /// L: align the pulse to the top of the second.
    pub const TP_ALIGN_TO_TOW_TP1: u32 = 0x1005_000a;
    /// L: polarity (1 = rising edge at top of second).
    pub const TP_POL_TP1: u32 = 0x1005_000b;
    /// E1: time grid (0 = UTC, 1 = GPS, ...).
    pub const TP_TIMEGRID_TP1: u32 = 0x2005_000c;
}

/// Layer bit: RAM (takes effect now).
pub const LAYER_RAM: u8 = 0x01;
/// Layer bit: battery-backed RAM (survives power-off while the backup supply is up).
pub const LAYER_BBR: u8 = 0x02;
/// Layer bit: flash (only on flash-equipped variants; not used by default).
pub const LAYER_FLASH: u8 = 0x04;

/// Why a frame could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// Output buffer too small.
    BufferTooSmall,
    /// More than 64 key/value pairs (the receiver's limit for one message).
    TooManyItems,
    /// A [`Value`] doesn't match the storage size encoded in its key.
    ValueMismatch,
}

/// A configuration value, to be matched with its key's storage size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    /// `L` (keys with size code 1).
    Bool(bool),
    /// `U1` / `E1` (size code 2).
    U8(u8),
    /// `U2` (size code 3).
    U16(u16),
    /// `U4` (size code 4).
    U32(u32),
}

impl Value {
    fn fits(self, key: u32) -> bool {
        matches!(
            (self, (key >> 28) & 7),
            (Value::Bool(_), 1) | (Value::U8(_), 2) | (Value::U16(_), 3) | (Value::U32(_), 4)
        )
    }
    fn write(self, out: &mut [u8]) -> usize {
        match self {
            Value::Bool(b) => {
                out[0] = b as u8;
                1
            }
            Value::U8(v) => {
                out[0] = v;
                1
            }
            Value::U16(v) => {
                out[..2].copy_from_slice(&v.to_le_bytes());
                2
            }
            Value::U32(v) => {
                out[..4].copy_from_slice(&v.to_le_bytes());
                4
            }
        }
    }
}

/// Build a UBX-CFG-VALSET (version 0, transactionless) frame. `layers` is an OR of
/// `LAYER_*`. Returns the frame length.
///
/// Payload: `version=0x00, layers, reserved[2]=0, then (key: u32 LE, value: 1/2/4 bytes LE)*`.
pub fn valset(
    out: &mut [u8],
    layers: u8,
    items: &[(u32, Value)],
) -> Result<usize, BuildError> {
    if items.len() > 64 {
        return Err(BuildError::TooManyItems);
    }
    let mut plen = 4;
    for &(k, v) in items {
        if !v.fits(k) {
            return Err(BuildError::ValueMismatch);
        }
        plen += 4 + key_size(k).unwrap_or(0);
    }
    if out.len() < plen + 8 {
        return Err(BuildError::BufferTooSmall);
    }
    let p = &mut out[6..];
    p[0] = 0; // version
    p[1] = layers;
    p[2] = 0;
    p[3] = 0;
    let mut o = 4;
    for &(k, v) in items {
        p[o..o + 4].copy_from_slice(&k.to_le_bytes());
        o += 4;
        o += v.write(&mut p[o..]);
    }
    seal(out, CLS_CFG, ID_CFG_VALSET, plen)
}

/// Build a UBX-CFG-VALGET poll for up to 64 keys from `layer` (0 RAM, 1 BBR, 2 flash,
/// 7 default). The reply arrives as [`Event::ValGet`].
pub fn valget(out: &mut [u8], layer: u8, position: u16, keys: &[u32]) -> Result<usize, BuildError> {
    if keys.len() > 64 {
        return Err(BuildError::TooManyItems);
    }
    let plen = 4 + 4 * keys.len();
    if out.len() < plen + 8 {
        return Err(BuildError::BufferTooSmall);
    }
    let p = &mut out[6..];
    p[0] = 0; // version
    p[1] = layer;
    p[2..4].copy_from_slice(&position.to_le_bytes());
    for (i, k) in keys.iter().enumerate() {
        p[4 + 4 * i..8 + 4 * i].copy_from_slice(&k.to_le_bytes());
    }
    seal(out, CLS_CFG, ID_CFG_VALGET, plen)
}

/// Build a UBX-MON-VER poll (empty payload). The reply arrives as [`Event::MonVer`].
pub fn poll_mon_ver(out: &mut [u8]) -> Result<usize, BuildError> {
    ubx::build_frame(out, CLS_MON, ID_MON_VER, &[]).ok_or(BuildError::BufferTooSmall)
}

/// Fill in header, length and checksum for a payload already written at `out[6..]`.
fn seal(out: &mut [u8], class: u8, id: u8, plen: usize) -> Result<usize, BuildError> {
    out[0] = ubx::SYNC1;
    out[1] = ubx::SYNC2;
    out[2] = class;
    out[3] = id;
    out[4..6].copy_from_slice(&(plen as u16).to_le_bytes());
    let (a, b) = ubx::fletcher(&out[2..6 + plen]);
    out[6 + plen] = a;
    out[7 + plen] = b;
    Ok(plen + 8)
}

/// Largest frame [`configure_sequence`] produces.
pub const MAX_CONFIG_FRAME: usize = 96;

/// Options for the boot configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigOptions {
    /// Which layers to write. Default RAM + BBR.
    pub layers: u8,
    /// New UART1 baud rate, or `None` to leave it (use `None` on an I2C-only board).
    /// Default `Some(115200)`.
    pub baud: Option<u32>,
    /// PPS pulse width once locked, microseconds. Default 100 000 (100 ms).
    pub pps_len_us: u32,
}

impl Default for ConfigOptions {
    fn default() -> Self {
        ConfigOptions { layers: LAYER_RAM | LAYER_BBR, baud: Some(115_200), pps_len_us: 100_000 }
    }
}

/// One configuration frame ready to send, plus how to confirm it.
#[derive(Clone, Copy, Debug)]
pub struct ConfigFrame {
    /// Human-readable step name for logs.
    pub name: &'static str,
    /// If set, the receiver's UART1 speed changes to this after the frame is processed: wait
    /// for the TX FIFO to drain, switch the host UART to this baud, call
    /// [`crate::Parser::reset`], then expect NAV-PVT at the new rate. The ACK for this step
    /// may be lost in the speed change, so confirm it by seeing a valid frame instead.
    pub baud_after: Option<u32>,
    buf: [u8; MAX_CONFIG_FRAME],
    len: usize,
}

impl ConfigFrame {
    /// The bytes to send.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Does this event answer this frame? `Some(true)` = ACK, `Some(false)` = NAK,
    /// `None` = unrelated. All steps are CFG-VALSET, which the receiver ACKs by class/id only,
    /// so send one step, wait for its ACK/NAK (the receiver answers within 1 s), then the next.
    pub fn confirmed_by(&self, ev: &Event) -> Option<bool> {
        match *ev {
            Event::Ack { class, id } if class == CLS_CFG && id == ID_CFG_VALSET => Some(true),
            Event::Nak { class, id } if class == CLS_CFG && id == ID_CFG_VALSET => Some(false),
            _ => None,
        }
    }
}

/// Iterator over the boot configuration steps. See [`configure_sequence`].
#[derive(Clone, Copy, Debug)]
pub struct ConfigSequence {
    opts: ConfigOptions,
    idx: u8,
}

/// The boot configuration with default options (UART1 at 115200, RAM + BBR).
pub fn configure_sequence() -> ConfigSequence {
    ConfigSequence::with_options(ConfigOptions::default())
}

impl ConfigSequence {
    /// A sequence with custom options.
    pub fn with_options(opts: ConfigOptions) -> Self {
        ConfigSequence { opts, idx: 0 }
    }
}

fn frame(
    name: &'static str,
    layers: u8,
    items: &[(u32, Value)],
    baud_after: Option<u32>,
) -> ConfigFrame {
    let mut f = ConfigFrame { name, baud_after, buf: [0; MAX_CONFIG_FRAME], len: 0 };
    // Items are compile-time constants sized to fit; a failure here is a programming error
    // caught by the unit tests.
    f.len = valset(&mut f.buf, layers, items).unwrap_or(0);
    f
}

impl Iterator for ConfigSequence {
    type Item = ConfigFrame;

    /// Steps, in order:
    /// 1. `nav-pvt-out`: NAV-PVT 1 Hz on UART1 and I2C. Done first so the output exists
    ///    before NMEA is switched off.
    /// 2. `protocols`: UBX in+out on, NMEA out off, on UART1 and I2C.
    /// 3. `dynmodel`: portable.
    /// 4. `timepulse`: TP1 on, 1 Hz, GNSS-synchronised, aligned to the UTC second, rising
    ///    edge on the second, 100 ms wide once locked and **no pulse before lock**.
    /// 5. `baud` (only if `opts.baud` is set): UART1 baud rate. Last, see `baud_after`.
    fn next(&mut self) -> Option<ConfigFrame> {
        use keys::*;
        let o = self.opts;
        let f = match self.idx {
            0 => frame(
                "nav-pvt-out",
                o.layers,
                &[
                    (MSGOUT_UBX_NAV_PVT_UART1, Value::U8(1)),
                    (MSGOUT_UBX_NAV_PVT_I2C, Value::U8(1)),
                ],
                None,
            ),
            1 => frame(
                "protocols",
                o.layers,
                &[
                    (UART1INPROT_UBX, Value::Bool(true)),
                    (UART1OUTPROT_UBX, Value::Bool(true)),
                    (UART1OUTPROT_NMEA, Value::Bool(false)),
                    (I2CINPROT_UBX, Value::Bool(true)),
                    (I2COUTPROT_UBX, Value::Bool(true)),
                    (I2COUTPROT_NMEA, Value::Bool(false)),
                ],
                None,
            ),
            2 => frame("dynmodel", o.layers, &[(NAVSPG_DYNMODEL, Value::U8(0))], None),
            3 => frame(
                "timepulse",
                o.layers,
                &[
                    (TP_PULSE_DEF, Value::U8(0)),        // PERIOD
                    (TP_PULSE_LENGTH_DEF, Value::U8(1)), // LENGTH
                    (TP_PERIOD_TP1, Value::U32(1_000_000)),
                    (TP_PERIOD_LOCK_TP1, Value::U32(1_000_000)),
                    (TP_LEN_TP1, Value::U32(0)),
                    (TP_LEN_LOCK_TP1, Value::U32(o.pps_len_us)),
                    (TP_TP1_ENA, Value::Bool(true)),
                    (TP_SYNC_GNSS_TP1, Value::Bool(true)),
                    (TP_USE_LOCKED_TP1, Value::Bool(true)),
                    (TP_ALIGN_TO_TOW_TP1, Value::Bool(true)),
                    (TP_POL_TP1, Value::Bool(true)),
                    (TP_TIMEGRID_TP1, Value::U8(0)), // UTC
                ],
                None,
            ),
            4 => match o.baud {
                Some(b) => frame("baud", o.layers, &[(UART1_BAUDRATE, Value::U32(b))], Some(b)),
                None => return None,
            },
            _ => return None,
        };
        self.idx += 1;
        Some(f)
    }
}
