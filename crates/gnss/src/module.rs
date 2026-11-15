//! Which receiver is attached, and its boot configuration as one sequence of steps.
//!
//! Two modules are supported:
//!
//! | | u-blox MAX-M10S | Quectel LC29H(AA) |
//! |---|---|---|
//! | Bands | L1 | L1 + L5 |
//! | Default UART | 9600, NMEA | 115200, NMEA |
//! | Commands | UBX (binary) | `$PAIR` / `$PQTM` (NMEA text) |
//! | What the hub reads | UBX NAV-PVT | NMEA GGA + RMC (+ GSA, GSV) |
//! | PPS | TIMEPULSE, configured by UBX | 1PPS, configured by `PQTMCFGPPS` |
//!
//! **Detection.** [`probe_bytes`] is a UBX MON-VER poll followed by `$PQTMVERNO`. Each module
//! answers its own and ignores the other (the LC29H does not speak UBX; the M10 does not know
//! `PQTM`). Feed the replies through the parser and call [`Module::from_event`]: a
//! `MonVer` means u-blox, a Quectel reply means LC29H. Neither within a few seconds while NMEA
//! is flowing: [`Module::Nmea`], a generic NMEA receiver, used as is with no configuration.

use crate::config::{ConfigFrame, ConfigOptions, ConfigSequence};
use crate::parser::Event;
use crate::quectel::{self, Lc29hOptions, Lc29hSequence, QuectelCommand, Reply};

/// An attached GNSS module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Module {
    /// u-blox M10 (MAX-M10S): answered MON-VER.
    UbloxM10,
    /// Quectel LC29H (any variant; the AA is the one with PPS + dual band).
    QuectelLc29h,
    /// Something that sends NMEA but answered neither probe. Used read-only.
    Nmea,
}

impl Module {
    /// What a parser event says about the module, if anything.
    pub fn from_event(ev: &Event) -> Option<Module> {
        match ev {
            Event::MonVer(_) | Event::Ack { .. } | Event::Nak { .. } => Some(Module::UbloxM10),
            Event::Quectel(Reply::Version(v)) if v.is_lc29h() => Some(Module::QuectelLc29h),
            // Some other Quectel (an LC76G, say) still speaks PAIR: treat it as one.
            Event::Quectel(_) => Some(Module::QuectelLc29h),
            _ => None,
        }
    }

    /// Short name for logs and JSON.
    pub fn name(self) -> &'static str {
        match self {
            Module::UbloxM10 => "max-m10s",
            Module::QuectelLc29h => "lc29h",
            Module::Nmea => "nmea",
        }
    }

    /// The boot configuration for this module.
    pub fn sequence(self, ubx: ConfigOptions, quectel: Lc29hOptions) -> Steps {
        match self {
            Module::UbloxM10 => Steps::Ubx(ConfigSequence::with_options(ubx)),
            Module::QuectelLc29h => Steps::Quectel(Lc29hSequence::new(quectel)),
            Module::Nmea => Steps::None,
        }
    }
}

/// The detection probe: a UBX MON-VER poll, then `$PQTMVERNO\r\n`. About 25 bytes. Returns the
/// length written (0 if `out` is under 32 bytes).
pub fn probe_bytes(out: &mut [u8]) -> usize {
    if out.len() < 32 {
        return 0;
    }
    let n = crate::config::poll_mon_ver(out).unwrap_or(0);
    let q = quectel::version_probe();
    let b = q.bytes();
    out[n..n + b.len()].copy_from_slice(b);
    n + b.len()
}

/// One configuration step for either module.
#[derive(Clone, Copy, Debug)]
pub enum Step {
    /// A UBX CFG-VALSET (u-blox).
    Ubx(ConfigFrame),
    /// A `$PAIR` / `$PQTM` command (Quectel).
    Quectel(QuectelCommand),
}

impl Step {
    /// The bytes to send.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Step::Ubx(f) => f.bytes(),
            Step::Quectel(c) => c.bytes(),
        }
    }

    /// Step name for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Step::Ubx(f) => f.name,
            Step::Quectel(c) => c.name,
        }
    }

    /// u-blox baud step: see [`ConfigFrame::baud_after`].
    pub fn baud_after(&self) -> Option<u32> {
        match self {
            Step::Ubx(f) => f.baud_after,
            Step::Quectel(_) => None,
        }
    }

    /// `Some(true)` accepted, `Some(false)` refused, `None` not an answer to this step.
    pub fn confirmed_by(&self, ev: &Event) -> Option<bool> {
        match self {
            Step::Ubx(f) => f.confirmed_by(ev),
            Step::Quectel(c) => c.confirmed_by(ev),
        }
    }
}

/// Iterator of [`Step`]s; see [`Module::sequence`].
#[derive(Clone, Copy, Debug)]
pub enum Steps {
    /// u-blox.
    Ubx(ConfigSequence),
    /// Quectel.
    Quectel(Lc29hSequence),
    /// Nothing to configure.
    None,
}

impl Iterator for Steps {
    type Item = Step;

    fn next(&mut self) -> Option<Step> {
        match self {
            Steps::Ubx(s) => s.next().map(Step::Ubx),
            Steps::Quectel(s) => s.next().map(Step::Quectel),
            Steps::None => None,
        }
    }
}
