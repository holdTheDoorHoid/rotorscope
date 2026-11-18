//! The USB console of `node-sdr`: one command per line.
//!
//! ```text
//! help                      list commands
//! stats                     one stats line now
//! phy                       PHY version, ABI check, register read-back
//! freq <MHz>                retune (whole MHz; 2200-2700, 5150-5950)
//! offset <kHz>              fine LO offset, -2500..2500 (default 0)
//! gain <index>              forced RX gain index
//! win clip|fold5|fold       Path B 4-bit window (default clip)
//! lpf <0-63>|off            analog low-pass capacitor code (default 12)
//! dcal                      re-measure the receiver DC offset at this LO
//! dcal ref                  re-measure at the PHY's reference LOs (as at boot)
//! dcal auto on|off          re-measure on every retune (default on)
//! edge rise|fall            PARL_IO sampling edge
//! pins xiao|alt             debug-bus loopback pads (route.rs)
//! swap on|off               I/Q orientation: Path A takes I from the low
//!                           field, Path B routes the low bits to the I nibble
//! trig <dB> [min_us max_us] trigger ratio over floor, burst length gate
//! mode detect|hold|idle     run the trigger/decoder loop; hold = also stop
//!                           and keep the next accepted burst for `dump`
//! snap                      Path A: one 16,380-word 10-bit snapshot at 20 MS/s
//! grab                      Path B: freeze the whole 40 MS/s ring now
//! level                     snap + gain/window advice, then resume
//! decode                    decode the held capture
//! dump [raw|c16] [b64|hex]  stream the held capture (see rawcap::dump)
//! resend <first> [last]     resend lines of the last dump
//! reveal on|off             print serial numbers unredacted
//! resume                    drop the held capture, back to the ring
//! bootloader                stop the radio, reboot into the ROM download mode
//! ```

use crate::dump::Enc;
use crate::iq::Window;

/// What the capture loop does with the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Trigger, freeze, measure, decode, report, resume.
    Detect,
    /// As `Detect`, but keep the ring frozen after the next accepted burst
    /// so it can be dumped raw; `resume` (or `mode detect`) continues.
    Hold,
    /// Ring stopped, nothing triggers (snapshots and dumps still work).
    Idle,
}

impl Mode {
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Detect => "detect",
            Mode::Hold => "hold",
            Mode::Idle => "idle",
        }
    }
}

/// What `dcal` does (receive DC-offset calibration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dcal {
    /// Measure now at the current LO.
    Now,
    /// Measure at the PHY's own reference LOs, as its boot calibration does
    /// (for before/after comparisons).
    Ref,
    /// Measure automatically at boot and on every frequency change.
    Auto(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Stats,
    Phy,
    Freq(u16),
    OffsetKhz(i32),
    /// true = the alternative pad set.
    Pins(bool),
    Gain(u8),
    Win(Window),
    /// `None` = leave the PHY's own calibration.
    Lpf(Option<u8>),
    Dcal(Dcal),
    /// true = sample on the falling edge.
    Edge(bool),
    Swap(bool),
    Trig { ratio_db: u8, min_us: Option<u32>, max_us: Option<u32> },
    Mode(Mode),
    Snap,
    Grab,
    Level,
    Decode,
    /// `raw`: the held capture as captured; else the decoder's `c16` input.
    Dump { raw: bool, enc: Enc },
    Resend { first: u32, last: u32 },
    Reveal(bool),
    Resume,
    Bootloader,
}

impl Command {
    /// A bring-up or test command: it retunes or parks the receiver,
    /// changes its calibration, stops detection, captures and dumps raw
    /// samples, un-redacts serial numbers or drops the chip into its ROM
    /// download mode. node-sdr accepts these only in a `bringup` build (a
    /// bench build, docs/BENCH.md "Bench builds"); a fielded node keeps
    /// `help`, `stats` and `phy`, which only report.
    pub const fn bringup_only(&self) -> bool {
        !matches!(self, Command::Help | Command::Stats | Command::Phy)
    }
}

/// Lowest and highest LO the C5 has been measured to receive on
/// (RESEARCH.md "Can a C5 do it?": 2200-2650 and 5180-5800 MHz measured;
/// C5VRX tunes to 5945). Requests outside are refused, not clamped.
pub const BANDS_MHZ: [(u16, u16); 2] = [(2200, 2700), (5150, 5950)];

pub fn freq_ok(mhz: u16) -> bool {
    BANDS_MHZ.iter().any(|&(a, b)| (a..=b).contains(&mhz))
}

fn on_off(s: Option<&str>) -> Result<bool, &'static str> {
    match s {
        Some("on" | "1" | "true") => Ok(true),
        Some("off" | "0" | "false") => Ok(false),
        _ => Err("expected on|off"),
    }
}

fn num<T: core::str::FromStr>(s: Option<&str>, what: &'static str) -> Result<T, &'static str> {
    s.and_then(|s| s.parse().ok()).ok_or(what)
}

/// Parse one console line. `Ok(None)` for a blank line.
pub fn parse(line: &str) -> Result<Option<Command>, &'static str> {
    let mut w = line.split_ascii_whitespace();
    let Some(verb) = w.next() else { return Ok(None) };
    let a = w.next();
    let b = w.next();
    let c = w.next();
    let cmd = match verb {
        "help" | "?" => Command::Help,
        "stats" => Command::Stats,
        "phy" => Command::Phy,
        "freq" => {
            let f: u16 = num(a, "freq <MHz>")?;
            if !freq_ok(f) {
                return Err("freq outside 2200-2700 / 5150-5950 MHz");
            }
            Command::Freq(f)
        }
        "offset" => {
            let k: i32 = num(a, "offset <kHz -2500..2500>")?;
            if !(-2500..=2500).contains(&k) {
                return Err("offset <kHz -2500..2500>");
            }
            Command::OffsetKhz(k)
        }
        "pins" => match a {
            Some("xiao") => Command::Pins(false),
            Some("alt") => Command::Pins(true),
            _ => return Err("pins xiao|alt"),
        },
        "gain" => Command::Gain(num(a, "gain <index 0-95>").and_then(|g: u8| {
            if g <= 95 { Ok(g) } else { Err("gain <index 0-95>") }
        })?),
        "win" => Command::Win(a.and_then(Window::parse).ok_or("win clip|fold5|fold")?),
        "lpf" => match a {
            Some("off") => Command::Lpf(None),
            _ => Command::Lpf(Some(num(a, "lpf <0-63>|off").and_then(|c: u8| {
                if c <= 63 { Ok(c) } else { Err("lpf <0-63>|off") }
            })?)),
        },
        "dcal" => match (a, b) {
            (None, _) => Command::Dcal(Dcal::Now),
            (Some("ref"), None) => Command::Dcal(Dcal::Ref),
            (Some("auto"), v) => Command::Dcal(Dcal::Auto(on_off(v)?)),
            _ => return Err("dcal [ref | auto on|off]"),
        },
        "edge" => match a {
            Some("rise") => Command::Edge(false),
            Some("fall") => Command::Edge(true),
            _ => return Err("edge rise|fall"),
        },
        "swap" => Command::Swap(on_off(a)?),
        "trig" => {
            let ratio_db: u8 = num(a, "trig <dB 1-30> [min_us max_us]")?;
            if !(1..=30).contains(&ratio_db) {
                return Err("trig <dB 1-30> [min_us max_us]");
            }
            let (min_us, max_us) = match (b, c) {
                (None, None) => (None, None),
                (Some(_), Some(_)) => {
                    let (x, y): (u32, u32) = (num(b, "min_us")?, num(c, "max_us")?);
                    if x >= y || y > 1400 {
                        return Err("need min_us < max_us <= 1400");
                    }
                    (Some(x), Some(y))
                }
                _ => return Err("trig <dB> [min_us max_us]"),
            };
            Command::Trig { ratio_db, min_us, max_us }
        }
        "mode" => match a {
            Some("detect") => Command::Mode(Mode::Detect),
            Some("hold") => Command::Mode(Mode::Hold),
            Some("idle") => Command::Mode(Mode::Idle),
            _ => return Err("mode detect|hold|idle"),
        },
        "snap" => Command::Snap,
        "grab" => Command::Grab,
        "level" => Command::Level,
        "decode" => Command::Decode,
        "dump" => {
            let mut raw = true;
            let mut enc = Enc::B64;
            for t in [a, b].into_iter().flatten() {
                match t {
                    "raw" => raw = true,
                    "c16" => raw = false,
                    _ => enc = Enc::parse(t).ok_or("dump [raw|c16] [b64|hex]")?,
                }
            }
            Command::Dump { raw, enc }
        }
        "resend" => {
            let first: u32 = num(a, "resend <first> [last]")?;
            let last: u32 = if b.is_some() { num(b, "resend <first> [last]")? } else { first };
            if last < first {
                return Err("resend: last < first");
            }
            Command::Resend { first, last }
        }
        "reveal" => Command::Reveal(on_off(a)?),
        "resume" => Command::Resume,
        "bootloader" => Command::Bootloader,
        _ => return Err("unknown command (help)"),
    };
    Ok(Some(cmd))
}

/// Power ratio for `trig <dB>` as the trigger's Q8 factor.
pub fn ratio_q8(db: u8) -> u32 {
    // 256 * 10^(db/10): 10^0.1 = 1.258925 per dB, carried in Q20.
    let mut r: u64 = 1 << 20;
    for _ in 0..db {
        r = r * 1_258_925 / 1_000_000;
    }
    ((r + (1 << 11)) >> 12) as u32
}

/// Accumulates console bytes into lines.
pub struct LineBuf {
    buf: [u8; 96],
    n: usize,
    overflow: bool,
}

impl Default for LineBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl LineBuf {
    pub const fn new() -> Self {
        LineBuf { buf: [0; 96], n: 0, overflow: false }
    }

    /// Feed one byte; returns a complete line on `\r` or `\n` (an over-long
    /// line is dropped whole and reported as `Err`).
    pub fn push(&mut self, b: u8) -> Option<Result<&str, &'static str>> {
        match b {
            b'\r' | b'\n' => {
                let n = core::mem::take(&mut self.n);
                if core::mem::take(&mut self.overflow) {
                    return Some(Err("line too long"));
                }
                if n == 0 {
                    return None;
                }
                Some(core::str::from_utf8(&self.buf[..n]).map_err(|_| "not UTF-8"))
            }
            // Backspace / delete.
            8 | 127 => {
                self.n = self.n.saturating_sub(1);
                None
            }
            _ => {
                if self.n < self.buf.len() {
                    self.buf[self.n] = b;
                    self.n += 1;
                } else {
                    self.overflow = true;
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse("  "), Ok(None));
        assert_eq!(parse("freq 5756"), Ok(Some(Command::Freq(5756))));
        assert!(parse("freq 900").is_err());
        assert!(parse("freq 5756.5").is_err());
        assert_eq!(parse("gain 60"), Ok(Some(Command::Gain(60))));
        assert_eq!(parse("offset -500"), Ok(Some(Command::OffsetKhz(-500))));
        assert!(parse("offset 3000").is_err());
        assert_eq!(parse("pins alt"), Ok(Some(Command::Pins(true))));
        assert!(parse("gain 200").is_err());
        assert_eq!(parse("win fold"), Ok(Some(Command::Win(Window::FOLD))));
        assert_eq!(parse("lpf off"), Ok(Some(Command::Lpf(None))));
        assert_eq!(parse("lpf 12"), Ok(Some(Command::Lpf(Some(12)))));
        assert!(parse("lpf 64").is_err());
        assert_eq!(parse("trig 6"), Ok(Some(Command::Trig { ratio_db: 6, min_us: None, max_us: None })));
        assert_eq!(
            parse("trig 8 400 900"),
            Ok(Some(Command::Trig { ratio_db: 8, min_us: Some(400), max_us: Some(900) }))
        );
        assert!(parse("trig 8 900 400").is_err());
        assert!(parse("trig 8 400").is_err());
        assert_eq!(parse("dump"), Ok(Some(Command::Dump { raw: true, enc: Enc::B64 })));
        assert_eq!(parse("dump c16 hex"), Ok(Some(Command::Dump { raw: false, enc: Enc::Hex })));
        assert_eq!(parse("dump hex"), Ok(Some(Command::Dump { raw: true, enc: Enc::Hex })));
        assert!(parse("dump zip").is_err());
        assert_eq!(parse("resend 4"), Ok(Some(Command::Resend { first: 4, last: 4 })));
        assert_eq!(parse("resend 4 9"), Ok(Some(Command::Resend { first: 4, last: 9 })));
        assert!(parse("resend 9 4").is_err());
        assert_eq!(parse("dcal"), Ok(Some(Command::Dcal(Dcal::Now))));
        assert_eq!(parse("dcal ref"), Ok(Some(Command::Dcal(Dcal::Ref))));
        assert_eq!(parse("dcal auto off"), Ok(Some(Command::Dcal(Dcal::Auto(false)))));
        assert!(parse("dcal auto").is_err());
        assert!(parse("dcal 5756").is_err());
        assert_eq!(parse("reveal on"), Ok(Some(Command::Reveal(true))));
        assert_eq!(parse("edge fall"), Ok(Some(Command::Edge(true))));
        assert_eq!(parse("mode idle"), Ok(Some(Command::Mode(Mode::Idle))));
        assert_eq!(parse("mode hold"), Ok(Some(Command::Mode(Mode::Hold))));
        assert!(parse("transmit").is_err());
    }

    #[test]
    fn fielded_nodes_keep_only_the_reports() {
        for l in ["help", "?", "stats", "phy"] {
            assert!(!parse(l).unwrap().unwrap().bringup_only(), "{l}");
        }
        for l in [
            "freq 5756", "offset 10", "pins alt", "gain 60", "win fold", "lpf 12", "dcal", "dcal auto off",
            "edge fall", "swap on", "trig 6", "mode idle", "mode hold", "mode detect", "snap", "grab", "level",
            "decode", "dump", "resend 1", "reveal on", "reveal off", "resume", "bootloader",
        ] {
            assert!(parse(l).unwrap().unwrap().bringup_only(), "{l}");
        }
    }

    #[test]
    fn ratio_table() {
        assert_eq!(ratio_q8(0), 256);
        assert!((1015..=1025).contains(&ratio_q8(6)), "{}", ratio_q8(6));
        assert!((2550..=2565).contains(&ratio_q8(10)), "{}", ratio_q8(10));
    }

    #[test]
    fn line_buffer() {
        let mut l = LineBuf::new();
        let mut got = vec![];
        for &b in b"gaim\x08n 5\r\n\nphy\n" {
            if let Some(r) = l.push(b) {
                got.push(r.map(|s| s.to_string()));
            }
        }
        assert_eq!(got, vec![Ok("gain 5".to_string()), Ok("phy".to_string())]);
        for _ in 0..200 {
            assert!(l.push(b'x').is_none());
        }
        assert_eq!(l.push(b'\n'), Some(Err("line too long")));
        assert!(l.push(b'\n').is_none());
    }
}
