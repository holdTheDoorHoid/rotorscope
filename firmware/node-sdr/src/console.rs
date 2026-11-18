//! node-sdr's own console commands (on top of `rawcap::cmd`'s capture
//! commands), and the hardware-in-the-loop sample feed.
//!
//! ```text
//! role droneid|fpv          switch job, built-in plan; held against hub
//!                           assignments until `role auto`
//! role auto                 follow the hub's assignments again
//! hop on|off                walk the plan / stay parked (`freq` parks)
//! plan                      print the plan
//! assign droneid|fpv <id> [<f100k>:<ms> ...]
//!                           apply a plan exactly as a hub Command::Assign
//! fpv verbose on|off        print every FPV dwell, quiet ones too
//! jam                       jamming watch: counters, each hop's floor
//! edge on|off               FPV: visit one off-plan channel per round
//! uplink [now]              uplink counters / open a window now (espnow)
//! @F <offset hex> <base64>  HIL: write sample bytes into the staging buffer
//! hil run <len> <crc32 hex> HIL: check the staged bytes and run them through
//!                           the live capture path (Radio::hil_run)
//! fault panic|hang          `faulttest` builds only: panic, or spin in the
//!                           capture loop until the watchdog resets the chip
//! ```

use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering::Relaxed};

use link::{Assignment, NodeRole};
use rawcap::cmd::{self, Command};

/// HIL staging: one Path B grab (`radio::GRAB_BYTES`).
pub const STAGE_LEN: usize = rawcap::ring::RING_BYTES - rawcap::ring::NODE_BYTES;

static STAGE: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
/// `@F` lines accepted and refused since boot.
pub static FED: AtomicU32 = AtomicU32::new(0);
pub static FEED_ERR: AtomicU32 = AtomicU32::new(0);

/// # Safety
/// `p` must point at `STAGE_LEN` bytes nothing else uses, for the whole run.
pub unsafe fn set_stage(p: *mut u8) {
    STAGE.store(p, Relaxed);
}

/// The first `len` staged bytes.
pub fn stage(len: usize) -> Option<&'static [u8]> {
    let p = STAGE.load(Relaxed);
    if p.is_null() || len > STAGE_LEN {
        return None;
    }
    // SAFETY: see `set_stage`; the console is not writing (a `hil run` is
    // only queued after its `@F` lines were handled, in order).
    Some(unsafe { core::slice::from_raw_parts(p, len) })
}

pub enum Cmd {
    Raw(Command),
    Local(Local),
}

pub enum Local {
    /// `None` = `role auto`.
    Role(Option<NodeRole>),
    Hop(bool),
    Plan,
    FpvVerbose(bool),
    Jam,
    Edge(bool),
    /// true = open a window now.
    Uplink(bool),
    Assign(Assignment),
    HilRun { len: usize, crc: u32 },
    /// `faulttest` builds only: true = panic, false = hang.
    #[cfg(feature = "faulttest")]
    Fault(bool),
}

fn on_off(s: Option<&str>) -> Result<bool, &'static str> {
    match s {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err("expected on|off"),
    }
}

fn role(s: Option<&str>) -> Result<NodeRole, &'static str> {
    match s {
        Some("droneid") => Ok(NodeRole::DroneId),
        Some("fpv") => Ok(NodeRole::Fpv),
        _ => Err("expected droneid|fpv"),
    }
}

/// One console line: a node-sdr command, else a rawcap capture command.
pub fn parse(line: &str) -> Result<Option<Cmd>, &'static str> {
    let mut w = line.split_ascii_whitespace();
    let local = match w.next() {
        Some("role") => {
            let r = w.next();
            if r == Some("auto") { Local::Role(None) } else { Local::Role(Some(role(r)?)) }
        }
        Some("hop") => Local::Hop(on_off(w.next())?),
        Some("plan") => Local::Plan,
        Some("fpv") => match w.next() {
            Some("verbose") => Local::FpvVerbose(on_off(w.next())?),
            _ => return Err("fpv verbose on|off"),
        },
        Some("jam") => Local::Jam,
        Some("edge") => Local::Edge(on_off(w.next())?),
        Some("uplink") => Local::Uplink(w.next() == Some("now")),
        Some("assign") => {
            let r = role(w.next())?;
            let id = w.next().and_then(|v| v.parse().ok()).ok_or("assign <role> <id> [f:ms ...]")?;
            let mut plan = heapless::Vec::new();
            for hop in w {
                let (f, ms) = hop.split_once(':').ok_or("hop is <f100k>:<ms>")?;
                let f = f.parse().map_err(|_| "bad frequency")?;
                let ms = ms.parse().map_err(|_| "bad dwell")?;
                plan.push((f, ms)).map_err(|_| "at most 24 hops")?;
            }
            Local::Assign(Assignment { role: r, plan, id })
        }
        Some("hil") => match w.next() {
            Some("run") => {
                let len = w.next().and_then(|v| v.parse().ok()).ok_or("hil run <len> <crc32 hex>")?;
                let crc = w.next().and_then(|v| u32::from_str_radix(v, 16).ok()).ok_or("hil run <len> <crc32 hex>")?;
                Local::HilRun { len, crc }
            }
            _ => return Err("hil run <len> <crc32 hex>"),
        },
        #[cfg(feature = "faulttest")]
        Some("fault") => match w.next() {
            Some("panic") => Local::Fault(true),
            Some("hang") => Local::Fault(false),
            _ => return Err("fault panic|hang"),
        },
        _ => return cmd::parse(line).map(|c| c.map(Cmd::Raw)),
    };
    Ok(Some(Cmd::Local(local)))
}

fn b64(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

/// Standard base64 (with `=` padding) into `out`; the byte count.
fn decode_b64(s: &[u8], out: &mut [u8]) -> Option<usize> {
    let s = s.strip_suffix(b"==").or_else(|| s.strip_suffix(b"=")).unwrap_or(s);
    let mut n = 0;
    for chunk in s.chunks(4) {
        let mut v = 0u32;
        for (k, &c) in chunk.iter().enumerate() {
            v |= b64(c)? << (18 - 6 * k);
        }
        let bytes = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        for k in 0..bytes {
            *out.get_mut(n)? = (v >> (16 - 8 * k)) as u8;
            n += 1;
        }
    }
    Some(n)
}

/// `@F <offset hex> <base64>`: write into the staging buffer. Errors are
/// only counted (the `hil run` CRC check catches any loss).
pub fn feed_line(l: &str) {
    let mut w = l[3..].split_ascii_whitespace();
    let (Some(off), Some(data)) = (w.next(), w.next()) else {
        FEED_ERR.fetch_add(1, Relaxed);
        return;
    };
    let mut tmp = [0u8; 72];
    let (Ok(off), Some(n)) = (usize::from_str_radix(off, 16), decode_b64(data.as_bytes(), &mut tmp)) else {
        FEED_ERR.fetch_add(1, Relaxed);
        return;
    };
    let p = STAGE.load(Relaxed);
    if p.is_null() || off + n > STAGE_LEN {
        FEED_ERR.fetch_add(1, Relaxed);
        return;
    }
    // SAFETY: inside the staging buffer (checked); only this task writes it.
    unsafe { core::ptr::copy_nonoverlapping(tmp.as_ptr(), p.add(off), n) };
    FED.fetch_add(1, Relaxed);
}
