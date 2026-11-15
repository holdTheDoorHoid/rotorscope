//! Possible-violation flags forwarded by a P4 hub (PLAN.md Phase 12;
//! `link::Frame::Flags`, protocol 6). This board has no airspace pack and
//! runs no rules: it shows what the P4 decided, on the phone page
//! (`GET /api/flags`) and in the drone's CoT remark.
//!
//! Tracks are numbered independently on the two boards, so a report names
//! its drone by UAS ID (or transmitter address) and is matched to this
//! board's track the same way ([`who_of`]). A report not refreshed for
//! [`FRESH_MS`] is dropped: the P4 resends every flagged track each second.
//!
//! Wording rule: "possible violation", never "illegal".

use core::fmt::Write as _;

use link::FlagReport;
use tracker::Track;

/// Reports kept (one per flagged track; the worst kept when full). Small: the
/// S3 hub has well under 1 KB of RAM to spare (measured 2026-11-15).
pub const MAX_REPORTS: usize = if cfg!(feature = "webhead") { 4 } else { 1 };
/// A report older than this is gone.
pub const FRESH_MS: u64 = 5_000;

pub struct Held {
    pub report: FlagReport,
    pub rx_ms: u64,
}

pub type Table = heapless::Vec<Held, MAX_REPORTS>;

/// A report arrived: replace the one for the same track, or add it.
pub fn on_report(table: &mut Table, report: FlagReport, now: u64) {
    table.retain(|h| now.saturating_sub(h.rx_ms) < FRESH_MS);
    if let Some(h) = table.iter_mut().find(|h| h.report.who == report.who) {
        h.report = report;
        h.rx_ms = now;
    } else if table.is_full() {
        // Keep the worst: drop the lowest level if the new one is higher.
        if let Some((i, _)) = table.iter().enumerate().min_by_key(|(_, h)| h.report.level)
            && table[i].report.level < report.level
        {
            table[i] = Held { report, rx_ms: now };
        }
    } else {
        let _ = table.push(Held { report, rx_ms: now });
    }
}

/// How the P4 names a drone track (`hub-p4/src/faa.rs` `who`): the primary
/// UAS ID as text, else the latest transmitter address as 12 hex digits.
pub fn who_of(t: &Track) -> heapless::String<24> {
    let mut s = heapless::String::new();
    match t.primary_id().and_then(|u| u.id.as_str()) {
        Some(txt) => {
            for c in txt.trim_end_matches('\0').chars() {
                if s.push(c).is_err() {
                    break;
                }
            }
        }
        None => {
            let m = t.latest_slot().map(|(_, l)| l.mac).unwrap_or_default();
            let _ = write!(s, "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
        }
    }
    s
}

/// The CoT remark addition for a drone: "Possible violation: Warning HEIGHT
/// 920 ft AGL (14 CFR 107.51(b) ...); ..." (level 1 and up only; empty if none).
pub fn remark(table: &Table, t: &Track, now: u64) -> heapless::String<400> {
    let mut out = heapless::String::new();
    let who = who_of(t);
    let Some(h) = table.iter().find(|h| h.report.who == who && now.saturating_sub(h.rx_ms) < FRESH_MS) else { return out };
    let mut first = true;
    for i in h.report.items.iter().filter(|i| i.level >= 1) {
        let text = faarules::text::item_text(i.rule, i.area, i.level, i.value);
        let sep = if first { "; Possible violation (hub cannot see waivers/LAANC): " } else { "; " };
        if out.len() + sep.len() + text.len() > out.capacity() {
            break;
        }
        let _ = out.push_str(sep);
        let _ = out.push_str(&text);
        first = false;
    }
    out
}

/// `GET /api/flags` is `{"flags":[` + the reports + `]}`, each report
/// `{"who":"...","level":2,"age_ms":300,"pack_date":1794700000,
/// "items":[{"level":2,"secs":41,"text":"..."}]}`, streamed one report per
/// chunk (`http.rs`). Report `i` into `buf`: `None` past the last one,
/// `Some(0)` for a stale report or one that does not fit.
pub fn report_json(table: &Table, i: usize, now: u64, first: bool, buf: &mut [u8]) -> Option<usize> {
    struct W<'a> {
        buf: &'a mut [u8],
        len: usize,
        full: bool,
    }
    impl core::fmt::Write for W<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            if self.len + s.len() > self.buf.len() {
                self.full = true;
                return Err(core::fmt::Error);
            }
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            Ok(())
        }
    }
    let h = table.get(i)?;
    if now.saturating_sub(h.rx_ms) >= FRESH_MS {
        return Some(0);
    }
    let r = &h.report;
    let mut w = W { buf, len: 0, full: false };
    let _ = write!(w, "{}{{\"who\":\"", if first { "" } else { "," });
    for c in r.who.chars() {
        let _ = w.write_char(if c == '"' || c == '\\' || c.is_control() { '?' } else { c });
    }
    let _ = write!(w, "\",\"level\":{},\"age_ms\":{},\"pack_date\":{},\"items\":[", r.level, now.saturating_sub(h.rx_ms), r.pack_date);
    for (k, it) in r.items.iter().enumerate() {
        let text = faarules::text::item_text(it.rule, it.area, it.level, it.value);
        let _ = write!(w, "{}{{\"level\":{},\"secs\":{},\"text\":\"{}\"}}", if k == 0 { "" } else { "," }, it.level, it.secs, text.as_str());
    }
    let _ = w.write_str("]}");
    Some(if w.full { 0 } else { w.len })
}
