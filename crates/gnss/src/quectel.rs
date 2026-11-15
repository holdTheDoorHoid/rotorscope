//! Quectel LC29H(AA) support: the `$PAIR` / `$PQTM` proprietary NMEA commands.
//!
//! The LC29H speaks NMEA 0183 (V4.10 field layout by default) plus Quectel's own command
//! sentences. It does **not** speak UBX. Every command is an ordinary NMEA sentence with the
//! usual XOR checksum; see [`command`].
//!
//! Facts used here, all from the *LC29H Series & LC79H(AL) GNSS Protocol Specification*
//! (Quectel, V1.3, 2024-03-12):
//!
//! * UART default 115200 baud (all LC29H variants), fix rate 1 Hz, and GGA, GLL, GSA, GSV,
//!   RMC, VTG on by default. So RMC + GGA arrive with no configuration at all.
//! * `$PAIR<nnn>,...` commands are answered first by `$PAIR001,<nnn>,<result>`
//!   ([`AckResult`]); a query (e.g. `PAIR051`) then sends its own reply line.
//! * `$PQTM...` commands are answered directly by `$PQTM<name>,OK...` or
//!   `$PQTM<name>,ERROR,<code>`; there is no `PAIR001` for them.
//! * `$PQTMVERNO` (no arguments) returns `$PQTMVERNO,<version>,<date>,<time>`; an AA unit's
//!   version string starts with `LC29HAA`. It is read-only, which makes it the probe.
//! * `$PAIR062,<type>,<rate>` sets one sentence's output rate (type 0 GGA, 1 GLL, 2 GSA, 3 GSV,
//!   4 RMC, 5 VTG, 6 ZDA, 7 GRS, 8 GST; rate = every n-th fix, 0 = off).
//! * `$PQTMCFGPPS,W,1,<enable>,<width ms>,<mode>,<polarity>,<interval>` configures 1PPS
//!   (mode 1 always / 2 2D fix / 3 3D fix; polarity 1 = active high; interval 0 = every
//!   second). Supported by the AA variant. The default mode before configuration was not
//!   confirmed, which is why [`Lc29hSequence`] sets it explicitly.
//!
//! Settings are written to RAM only (no `PAIR513` save): the hub configures the module on every
//! boot, so nothing in the module's flash is ever changed.

use crate::nmea::xor_checksum;

/// Longest command or reply this module builds or keeps.
pub const MAX_COMMAND: usize = 64;

/// Build `$<body>*HH\r\n` into `out`. `body` must not contain `$` or `*`.
/// Returns the length, or `None` if `out` is too small or `body` is not allowed.
pub fn command(out: &mut [u8], body: &str) -> Option<usize> {
    let b = body.as_bytes();
    if b.iter().any(|&c| c == b'$' || c == b'*' || !(0x20..=0x7e).contains(&c)) {
        return None;
    }
    let n = b.len() + 6;
    if out.len() < n {
        return None;
    }
    let cs = xor_checksum(b);
    out[0] = b'$';
    out[1..1 + b.len()].copy_from_slice(b);
    let hex = b"0123456789ABCDEF";
    out[1 + b.len()] = b'*';
    out[2 + b.len()] = hex[usize::from(cs >> 4)];
    out[3 + b.len()] = hex[usize::from(cs & 15)];
    out[4 + b.len()] = b'\r';
    out[5 + b.len()] = b'\n';
    Some(n)
}

/// `$PAIR001` result codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckResult {
    /// 0: accepted.
    Ok,
    /// 1: being processed; a final answer follows.
    Processing,
    /// 2: failed.
    Failed,
    /// 3: command not supported by this module or firmware.
    Unsupported,
    /// 4: parameter error (out of range, wrong count, bad checksum).
    BadParameter,
    /// 5: the GNSS engine is busy; try again.
    Busy,
    /// Any other number.
    Other(u8),
}

impl AckResult {
    fn from_code(c: u32) -> Self {
        match c {
            0 => AckResult::Ok,
            1 => AckResult::Processing,
            2 => AckResult::Failed,
            3 => AckResult::Unsupported,
            4 => AckResult::BadParameter,
            5 => AckResult::Busy,
            n => AckResult::Other(n.min(255) as u8),
        }
    }
}

/// A short ASCII string kept inline (no alloc).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Text<const N: usize> {
    buf: [u8; N],
    len: u8,
}

impl<const N: usize> Text<N> {
    fn new(s: &[u8]) -> Self {
        let mut t = Text { buf: [0; N], len: 0 };
        let n = s.len().min(N);
        t.buf[..n].copy_from_slice(&s[..n]);
        t.len = n as u8;
        t
    }

    /// As `&str` (the parser only stores printable ASCII).
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Debug for Text<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

/// The module's answer to `$PQTMVERNO`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    /// e.g. `LC29HAANR01A04S`.
    pub version: Text<24>,
    /// Build date and time as sent, e.g. `2022/11/04` and `16:39:48`.
    pub date: Text<10>,
    /// Build time.
    pub time: Text<8>,
}

impl Version {
    /// An LC29H of any variant.
    pub fn is_lc29h(&self) -> bool {
        self.version.as_str().starts_with("LC29H")
    }

    /// The LC29H(AA) specifically (the dual-band, PPS-capable variant this hub targets).
    pub fn is_lc29h_aa(&self) -> bool {
        self.version.as_str().starts_with("LC29HAA")
    }
}

/// The current 1PPS configuration, from `$PQTMCFGPPS,OK,...` (reply to a read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PpsConfig {
    /// PPS output on.
    pub enabled: bool,
    /// Pulse width, ms.
    pub width_ms: u16,
    /// 1 always, 2 with a 2D fix, 3 with a 3D fix.
    pub mode: u8,
    /// True: active high.
    pub active_high: bool,
    /// 0 every second, 1 odd seconds, 2 even seconds.
    pub interval: u8,
}

/// A decoded Quectel proprietary sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// `$PAIR001,<cmd>,<result>`: the answer to a `$PAIR<cmd>` command.
    PairAck {
        /// The command number answered, e.g. 62 for `PAIR062`.
        cmd: u16,
        /// Its result.
        result: AckResult,
    },
    /// `$PQTMVERNO,<version>,<date>,<time>`.
    Version(Version),
    /// `$PQTM<name>,OK` (with or without further fields). `name` without the `PQTM` prefix.
    PqtmOk {
        /// e.g. `CFGPPS`.
        name: Text<12>,
    },
    /// `$PQTM<name>,ERROR,<code>`.
    PqtmError {
        /// e.g. `CFGPPS`.
        name: Text<12>,
        /// The module's error code (0 if missing).
        code: u16,
    },
    /// `$PQTMCFGPPS,OK,1,...`: reply to a PPS read.
    Pps(PpsConfig),
}

fn num(f: &[u8]) -> Option<u32> {
    if f.is_empty() || f.len() > 9 || !f.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(f.iter().fold(0u32, |a, &c| a * 10 + u32::from(c - b'0')))
}

/// Decode a checksum-verified sentence body (between `$` and `*`). `None` if it is not a
/// Quectel sentence we understand.
pub(crate) fn decode(body: &[u8]) -> Option<Reply> {
    let mut f: [&[u8]; 10] = [&[]; 10];
    let mut n = 0;
    for part in body.split(|&c| c == b',') {
        if n == f.len() {
            break;
        }
        f[n] = part;
        n += 1;
    }
    let f = &f[..n];
    let head = f[0];
    if head == b"PAIR001" {
        if f.len() < 3 {
            return None;
        }
        let cmd = num(f[1])?;
        let res = num(f[2])?;
        return Some(Reply::PairAck { cmd: cmd.min(u32::from(u16::MAX)) as u16, result: AckResult::from_code(res) });
    }
    let name = head.strip_prefix(b"PQTM")?;
    if name == b"VERNO" && f.len() >= 4 && f[1] != b"ERROR" {
        return Some(Reply::Version(Version { version: Text::new(f[1]), date: Text::new(f[2]), time: Text::new(f[3]) }));
    }
    match f.get(1).copied() {
        Some(b"OK") => {
            if name == b"CFGPPS" && f.len() >= 8 {
                return Some(Reply::Pps(PpsConfig {
                    enabled: num(f[3])? == 1,
                    width_ms: num(f[4])?.min(9999) as u16,
                    mode: num(f[5])?.min(255) as u8,
                    active_high: num(f[6])? == 1,
                    interval: num(f[7])?.min(255) as u8,
                }));
            }
            Some(Reply::PqtmOk { name: Text::new(name) })
        }
        Some(b"ERROR") => Some(Reply::PqtmError {
            name: Text::new(name),
            code: f.get(2).and_then(|c| num(c)).unwrap_or(0).min(u32::from(u16::MAX)) as u16,
        }),
        _ => None,
    }
}

/// True for a sentence body this module handles (`PAIR...` / `PQTM...`).
pub(crate) fn is_quectel(body: &[u8]) -> bool {
    body.starts_with(b"PAIR") || body.starts_with(b"PQTM")
}

/// What answers a [`QuectelCommand`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// A `$PAIR001,<n>,...` for this command number.
    PairAck(u16),
    /// `$PQTM<name>,OK` / `,ERROR` with this name (no `PQTM` prefix).
    Pqtm(&'static str),
}

/// One Quectel command sentence ready to send, plus how to confirm it.
#[derive(Clone, Copy, Debug)]
pub struct QuectelCommand {
    /// Human-readable step name for logs.
    pub name: &'static str,
    /// What the module answers with.
    pub expect: Expect,
    buf: [u8; MAX_COMMAND],
    len: usize,
}

impl QuectelCommand {
    /// Build from the sentence body (no `$`, no checksum), e.g. `"PAIR062,0,1"`.
    pub fn new(name: &'static str, body: &str, expect: Expect) -> Self {
        let mut c = QuectelCommand { name, expect, buf: [0; MAX_COMMAND], len: 0 };
        c.len = command(&mut c.buf, body).unwrap_or(0);
        c
    }

    /// The bytes to send.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// `Some(true)` accepted, `Some(false)` refused, `None` unrelated (or "processing": keep
    /// waiting).
    pub fn confirmed_by(&self, ev: &crate::Event) -> Option<bool> {
        let crate::Event::Quectel(r) = ev else { return None };
        match (self.expect, r) {
            (Expect::PairAck(want), Reply::PairAck { cmd, result }) if *cmd == want => match result {
                AckResult::Ok => Some(true),
                AckResult::Processing => None,
                _ => Some(false),
            },
            (Expect::Pqtm(want), Reply::PqtmOk { name }) if name.as_str() == want => Some(true),
            (Expect::Pqtm(want), Reply::PqtmError { name, .. }) if name.as_str() == want => Some(false),
            (Expect::Pqtm("CFGPPS"), Reply::Pps(_)) => Some(true),
            _ => None,
        }
    }
}

/// The `$PQTMVERNO` probe: read-only, answered by [`Reply::Version`].
pub fn version_probe() -> QuectelCommand {
    QuectelCommand::new("verno", "PQTMVERNO", Expect::Pqtm("VERNO"))
}

/// Sentence types for `PAIR062`.
pub mod sentence {
    /// GGA.
    pub const GGA: u8 = 0;
    /// GLL.
    pub const GLL: u8 = 1;
    /// GSA.
    pub const GSA: u8 = 2;
    /// GSV.
    pub const GSV: u8 = 3;
    /// RMC.
    pub const RMC: u8 = 4;
    /// VTG.
    pub const VTG: u8 = 5;
}

/// Options for [`Lc29hSequence`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lc29hOptions {
    /// PPS pulse width, ms (1..=999). Default 100.
    pub pps_width_ms: u16,
    /// Keep GSV on (satellites in view per constellation and band, for the health view).
    /// Default true. GSV is the bulk of the NMEA output (~1 KB/s on a dual-band sky).
    pub gsv: bool,
}

impl Default for Lc29hOptions {
    fn default() -> Self {
        Lc29hOptions { pps_width_ms: 100, gsv: true }
    }
}

/// The LC29H boot configuration, one command at a time; send each, wait for
/// [`QuectelCommand::confirmed_by`], then the next.
///
/// Steps: GGA, RMC and GSA every fix (GSA gives the 2D/3D mode and satellites used), GSV per
/// [`Lc29hOptions::gsv`], GLL and VTG off (they repeat GGA/RMC), then 1PPS on: every second,
/// active high, [`Lc29hOptions::pps_width_ms`] wide, **only with a 3D fix** (so an edge always
/// belongs to a trustworthy UTC second). The fix rate stays at the 1 Hz default (changing it
/// needs a save and restart on this module). RAM only; nothing is saved to the module's flash.
#[derive(Clone, Copy, Debug)]
pub struct Lc29hSequence {
    opts: Lc29hOptions,
    idx: u8,
}

impl Lc29hSequence {
    /// A sequence with these options.
    pub fn new(opts: Lc29hOptions) -> Self {
        Lc29hSequence { opts, idx: 0 }
    }
}

impl Iterator for Lc29hSequence {
    type Item = QuectelCommand;

    fn next(&mut self) -> Option<QuectelCommand> {
        let mut body: heapless_str::Buf = heapless_str::Buf::new();
        let o = self.opts;
        let rate = |b: &mut heapless_str::Buf, t: u8, r: u8| {
            b.push_str("PAIR062,");
            b.push_u32(u32::from(t));
            b.push_str(",");
            b.push_u32(u32::from(r));
        };
        let (name, expect) = match self.idx {
            0 => {
                rate(&mut body, sentence::GGA, 1);
                ("gga-on", Expect::PairAck(62))
            }
            1 => {
                rate(&mut body, sentence::RMC, 1);
                ("rmc-on", Expect::PairAck(62))
            }
            2 => {
                rate(&mut body, sentence::GSA, 1);
                ("gsa-on", Expect::PairAck(62))
            }
            3 => {
                rate(&mut body, sentence::GSV, u8::from(o.gsv));
                (if o.gsv { "gsv-on" } else { "gsv-off" }, Expect::PairAck(62))
            }
            4 => {
                rate(&mut body, sentence::GLL, 0);
                ("gll-off", Expect::PairAck(62))
            }
            5 => {
                rate(&mut body, sentence::VTG, 0);
                ("vtg-off", Expect::PairAck(62))
            }
            6 => {
                body.push_str("PQTMCFGPPS,W,1,1,");
                body.push_u32(u32::from(o.pps_width_ms.clamp(1, 999)));
                body.push_str(",3,1,0");
                ("pps", Expect::Pqtm("CFGPPS"))
            }
            _ => return None,
        };
        self.idx += 1;
        Some(QuectelCommand::new(name, body.as_str(), expect))
    }
}

/// A tiny fixed-capacity string builder (the crate has no dependencies).
mod heapless_str {
    pub struct Buf {
        b: [u8; 48],
        n: usize,
    }

    impl Buf {
        pub fn new() -> Self {
            Buf { b: [0; 48], n: 0 }
        }
        pub fn push_str(&mut self, s: &str) {
            for &c in s.as_bytes() {
                if self.n < self.b.len() {
                    self.b[self.n] = c;
                    self.n += 1;
                }
            }
        }
        pub fn push_u32(&mut self, mut v: u32) {
            let mut d = [0u8; 10];
            let mut i = d.len();
            loop {
                i -= 1;
                d[i] = b'0' + (v % 10) as u8;
                v /= 10;
                if v == 0 {
                    break;
                }
            }
            for &c in &d[i..] {
                if self.n < self.b.len() {
                    self.b[self.n] = c;
                    self.n += 1;
                }
            }
        }
        pub fn as_str(&self) -> &str {
            core::str::from_utf8(&self.b[..self.n]).unwrap_or("")
        }
    }
}
