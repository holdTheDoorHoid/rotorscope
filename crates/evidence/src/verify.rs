//! Checking a log (host side): every link, signature, sequence number and
//! checkpoint, plus a re-decode of every detection's raw bytes. Used by
//! `tools/evidence-verify` and by `tools/evidence-server` on arrival.
//!
//! Findings have a level:
//! - **Fail**: the log is not what the unit wrote (a record was edited,
//!   removed, reordered or added; a signature does not match; a file was
//!   cut or extended after the unit moved on).
//! - **Gap**: nothing contradicts the log, but part of it cannot be
//!   vouched for (records after the last signed checkpoint; records a
//!   server never received; a file not supplied).
//! - **Note**: worth knowing (the key class the log claims, records with no
//!   GPS time, a boot event that names bench features).
//!
//! **Who signed.** A log names its own key (boot event, every checkpoint),
//! so a log that checks against the key it names proves only that it is
//! self-consistent: anyone can generate a key and write such a log. The
//! verdict is VERIFIED only when the key came from outside the log
//! ([`Options::expect_key`], [`Options::trusted_keys`]). The key class byte
//! is shown as what the log *claims*; it proves nothing by itself. The
//! published test key is refused unless [`Options::allow_test_key`].
//!
//! **What no verdict proves:** that the log is complete. Records after the
//! last one supplied, newer files, a reformatted card or a boot never
//! uploaded leave no trace in what is checked ([`NOT_PROVEN`]).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::format::{self, Body, CheckpointBody, EventCode, Kind, ParseError, Record};
use crate::sign::{self, KeyClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Note,
    Gap,
    Fail,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Note => "NOTE",
            Level::Gap => "GAP",
            Level::Fail => "FAIL",
        }
    }
}

/// Where a finding is: input (file or batch), byte offset, chain and
/// sequence number, whichever are known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Where {
    pub source: String,
    pub offset: Option<usize>,
    pub boot: Option<[u8; 8]>,
    pub seq: Option<u64>,
}

impl std::fmt::Display for Where {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if !self.source.is_empty() {
            parts.push(self.source.clone());
        }
        if let Some(o) = self.offset {
            parts.push(format!("byte {o}"));
        }
        if let Some(b) = self.boot {
            parts.push(format!("boot {}", hex(&b)));
        }
        if let Some(s) = self.seq {
            parts.push(format!("record #{s}"));
        }
        write!(f, "{}", parts.join(", "))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub at: Where,
    pub what: String,
}

/// One input: a log file from the card, a batch, or a whole stream.
pub struct Input {
    pub name: String,
    pub bytes: Vec<u8>,
    /// For card files named `EVnnnnnn.EVL`: the number.
    pub file_no: Option<u32>,
}

impl Input {
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let name = name.into();
        let file_no = file_number(&name);
        Self { name, bytes, file_no }
    }
}

/// `EV000012.EVL` (any directory, any case) -> 12.
pub fn file_number(path: &str) -> Option<u32> {
    let base = path.rsplit(['/', '\\']).next()?.to_ascii_uppercase();
    let stem = base.strip_suffix(".EVL")?;
    let digits = stem.strip_prefix("EV")?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok())?
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// A missing run of sequence numbers, or a missing card file, is a Gap
    /// rather than a Fail. A gap cannot be told apart from records that were
    /// removed, so this is for records collected from several places.
    pub gaps_allowed: bool,
    /// A signing key confirmed outside the log (`--key`, or the key a
    /// server registered for the device). Checkpoints signed by any other
    /// key fail.
    pub expect_key: Option<[u8; 65]>,
    /// More keys confirmed outside the log, each with a label (`--keys`).
    /// With [`expect_key`](Self::expect_key), the accepted set is the union.
    pub trusted_keys: Vec<([u8; 65], String)>,
    /// Accept the published test key ([`sign::public_test_key`]). For this
    /// project's own tests only: anyone can sign with that key.
    pub allow_test_key: bool,
}

impl Options {
    /// Any key was supplied from outside the log.
    pub fn outside_keys(&self) -> bool {
        self.expect_key.is_some() || !self.trusted_keys.is_empty()
    }

    /// The label of an outside key, if `k` is one.
    fn label_of(&self, k: &[u8; 65]) -> Option<String> {
        if self.expect_key.as_ref() == Some(k) {
            return Some("the key given with --key".into());
        }
        self.trusted_keys.iter().find(|(t, _)| t == k).map(|(_, l)| l.clone())
    }
}

/// Printed with every verdict: what the checker cannot see.
pub const NOT_PROVEN: &str = "NOT PROVEN: that the log is complete. Records after the last one checked, newer files, a reformatted card or a boot never uploaded leave no trace here.";

/// What a chain trusted through [`Options::allow_test_key`] is labelled.
pub const TEST_KEY_LABEL: &str = "the PUBLISHED TEST KEY (accepted by --allow-test-key; anyone can sign with it)";

/// What one chain (one boot of one unit) holds.
#[derive(Debug, Clone, Default)]
pub struct ChainSummary {
    pub boot: [u8; 8],
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub records: u64,
    pub detections: u64,
    pub events: u64,
    pub checkpoints: u64,
    /// Highest sequence number a valid checkpoint covers (records below it).
    pub signed_through: Option<u64>,
    pub key: Option<[u8; 65]>,
    /// The key class byte the log claims (boot event / checkpoint). Written
    /// by whoever signed: proves nothing by itself.
    pub key_class: Option<u8>,
    /// Set when a checkpoint's signature checked against a key supplied from
    /// outside the log: that key's label. `None`: the signer was not checked.
    pub trusted_as: Option<String>,
    pub fw_hash: Option<[u8; 32]>,
    pub utc_first: Option<u64>,
    pub utc_last: Option<u64>,
    pub no_time: u64,
    pub free_running: u64,
    pub no_position: u64,
    pub manual_position: u64,
    pub missing: Vec<(u64, u64)>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub chains: Vec<ChainSummary>,
    /// Every record that parsed, in input order, with its input index and
    /// byte offset (for listing and for the server's map).
    pub records: Vec<(usize, usize, OwnedRecord)>,
}

/// A record's bytes plus the fields a caller usually wants.
#[derive(Debug, Clone)]
pub struct OwnedRecord {
    pub bytes: Vec<u8>,
    pub boot: [u8; 8],
    pub seq: u64,
    pub kind: Kind,
    pub hash: [u8; 32],
}

impl OwnedRecord {
    pub fn parse(&self) -> Record<'_> {
        format::parse(&self.bytes).expect("parsed before")
    }
}

impl Report {
    pub fn worst(&self) -> Option<Level> {
        self.findings.iter().map(|f| f.level).max()
    }

    /// Every chain's signatures checked against a key supplied from
    /// outside the log.
    pub fn signer_checked(&self) -> bool {
        !self.chains.is_empty() && self.chains.iter().all(|c| c.trusted_as.is_some())
    }

    /// 0 verified; 1 failed; 3 signer checked but gaps or unsigned records;
    /// 4 self-consistent but the signer was not checked against an outside
    /// key (anyone could have made the log). 2 is the tool's "could not run".
    pub fn exit_code(&self) -> i32 {
        match self.worst() {
            Some(Level::Fail) => 1,
            _ if !self.signer_checked() => 4,
            Some(Level::Gap) => 3,
            _ => 0,
        }
    }

    /// The one-line verdict for [`exit_code`](Self::exit_code). Always print
    /// [`NOT_PROVEN`] with it.
    pub fn verdict(&self) -> &'static str {
        match self.exit_code() {
            0 => "VERIFIED: every record links, every signature checks against a key you supplied, and a signature covers every record checked.",
            1 => "FAILED: the log is not what its signer wrote, or it cannot be accepted. See the FAIL lines above.",
            3 => "SIGNER CHECKED, WITH GAPS: what is signed checks against a key you supplied, but some records are missing or not covered by a signature (see GAP lines). Missing records may have been removed; that cannot be told apart from records never written or never received.",
            _ => "SELF-CONSISTENT, SIGNER NOT CHECKED: anyone could have made this log. The key it was checked against came from the log itself; give the expected key with --key HEX or --keys FILE.",
        }
    }

    fn add(&mut self, level: Level, at: Where, what: impl Into<String>) {
        self.findings.push(Finding { level, at, what: what.into() });
    }
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// UTC milliseconds as `2026-11-15T14:03:07.250Z`.
pub fn fmt_utc(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", sod / 3600, sod / 60 % 60, sod % 60, ms % 1000)
}

struct ChainState {
    sum: ChainSummary,
    /// Sequence number expected next, and the hash it must link to.
    next: u64,
    head: [u8; 32],
    /// Hash of each record by sequence number (to check duplicates and
    /// links across gaps).
    hashes: BTreeMap<u64, [u8; 32]>,
    last_uptime: u64,
    last_utc: Option<(u64, u8)>,
    clock_event_since: bool,
    last_checkpoint_uptime: Option<u64>,
    last_signed_end: Option<u64>,
    /// The key was refused (test key, or not an outside key): said once.
    key_refused: bool,
}

/// The bench features a boot event's text names (`...; bench: a,b`, as
/// the P4 hub writes it), or `None` for a normal build (`bench: none`) or a
/// text that names none. The list ends at the next `;` (the hub writes
/// `...; bench: none; audio clips off at boot`).
pub fn boot_bench_list(text: &[u8]) -> Option<&str> {
    let text = core::str::from_utf8(text).ok()?;
    let rest = text.rsplit_once("bench: ")?.1;
    let list = rest.split(';').next().unwrap_or("").trim();
    (!list.is_empty() && list != "none").then_some(list)
}

/// Check `inputs` in order. Card files should be given in file-number
/// order; batches in upload order.
pub fn verify(inputs: &[Input], opts: &Options) -> Report {
    let mut rep = Report::default();
    let mut chains: Vec<ChainState> = Vec::new();
    let mut index: HashMap<[u8; 8], usize> = HashMap::new();
    // (file number) -> (length, last complete record hash, ends incomplete)
    let mut file_tails: HashMap<u32, (usize, Option<[u8; 32]>, bool)> = HashMap::new();
    let mut mounted_claims: Vec<(Where, u32, u32, u32, [u8; 32], u8)> = Vec::new();

    for (ii, input) in inputs.iter().enumerate() {
        let at0 = |off: usize| Where { source: input.name.clone(), offset: Some(off), boot: None, seq: None };
        let mut last_hash = None;
        let mut incomplete = false;
        let mut at = 0usize;
        while at < input.bytes.len() {
            let r = match format::parse(&input.bytes[at..]) {
                Ok(r) => r,
                Err(e) => {
                    let rest = input.bytes.len() - at;
                    let cut = matches!(e, ParseError::Truncated) && format::peek_len(&input.bytes[at..]).map_or(rest < 8, |n| n > rest);
                    if cut {
                        incomplete = true;
                        rep.add(
                            Level::Gap,
                            at0(at),
                            format!("the input ends with {rest} bytes of an incomplete record (power cut or card removed while writing; the unit rewrites that record whole in its next file)"),
                        );
                        break;
                    }
                    rep.add(Level::Fail, at0(at), format!("not a valid record: {e}"));
                    // Resynchronise at the next record marker, to report what follows.
                    match input.bytes[at + 1..].windows(4).position(|w| w == format::MAGIC) {
                        Some(p) => {
                            at += 1 + p;
                            continue;
                        }
                        None => break,
                    }
                }
            };
            let len = r.header.len;
            let hash = r.hash();
            last_hash = Some(hash);
            let ci = *index.entry(r.header.boot_id).or_insert_with(|| {
                chains.push(ChainState {
                    sum: ChainSummary { boot: r.header.boot_id, ..Default::default() },
                    next: 0,
                    head: [0; 32],
                    hashes: BTreeMap::new(),
                    last_uptime: 0,
                    last_utc: None,
                    clock_event_since: false,
                    last_checkpoint_uptime: None,
                    last_signed_end: None,
                    key_refused: false,
                });
                chains.len() - 1
            });
            let w = Where { source: input.name.clone(), offset: Some(at), boot: Some(r.header.boot_id), seq: Some(r.header.seq) };
            if let Some(claim) = check_record(&mut rep, &mut chains[ci], &r, hash, &w, opts) {
                mounted_claims.push((w.clone(), claim.0, claim.1, claim.2, claim.3, claim.4));
            }
            rep.records.push((ii, at, OwnedRecord { bytes: r.bytes.to_vec(), boot: r.header.boot_id, seq: r.header.seq, kind: r.header.kind, hash }));
            at += len;
        }
        if let Some(n) = input.file_no {
            file_tails.insert(n, (input.bytes.len(), last_hash, incomplete));
        }
    }

    // A file not supplied may have been deleted: a failure unless gaps are
    // allowed, and even then it cannot be told apart from one never written.
    let (missing_level, missing_why) = if opts.gaps_allowed {
        (Level::Gap, "; its records may have been removed, which cannot be told apart from records never written")
    } else {
        (Level::Fail, " (a missing file fails unless --allow-gaps)")
    };
    // Each card file names the one before it.
    for (w, this_no, prev_no, prev_len, prev_hash, prev_status) in mounted_claims {
        if prev_no == 0 || prev_status == 0 {
            continue;
        }
        let Some(&(len, last, incomplete)) = file_tails.get(&prev_no) else {
            if !file_tails.is_empty() {
                rep.add(missing_level, w, format!("file EV{this_no:06}.EVL says the file before it was EV{prev_no:06}.EVL ({prev_len} bytes), which was not supplied{missing_why}"));
            }
            continue;
        };
        if len != prev_len as usize {
            rep.add(
                Level::Fail,
                w.clone(),
                format!(
                    "EV{prev_no:06}.EVL is {len} bytes now but was {prev_len} bytes when the unit opened EV{this_no:06}.EVL: it was {} afterwards",
                    if len > prev_len as usize { "added to" } else { "cut short" }
                ),
            );
        } else if prev_status == 3 {
            rep.add(Level::Gap, w, format!("the unit could not read the end of EV{prev_no:06}.EVL when it opened the next file"));
        } else if last != Some(prev_hash) {
            rep.add(Level::Fail, w, format!("the last record of EV{prev_no:06}.EVL is not the one the unit saw when it opened EV{this_no:06}.EVL (changed afterwards)"));
        } else if (prev_status == 2) != incomplete {
            rep.add(Level::Fail, w, format!("EV{prev_no:06}.EVL's ending changed after the unit opened the next file"));
        }
    }
    let mut nos: Vec<u32> = file_tails.keys().copied().collect();
    nos.sort();
    for p in nos.windows(2) {
        if p[1] != p[0] + 1 {
            rep.add(
                missing_level,
                Where { source: format!("EV{:06}.EVL", p[1]), ..Default::default() },
                format!("files EV{:06}..EV{:06} were not supplied{missing_why}", p[0] + 1, p[1] - 1),
            );
        }
    }

    for c in &mut chains {
        let w = Where { boot: Some(c.sum.boot), ..Default::default() };
        let last = c.sum.last_seq.unwrap_or(0);
        match c.last_signed_end {
            None => rep.add(Level::Gap, w.clone(), format!("no signed checkpoint: none of this chain's {} records is covered by a signature", c.sum.records)),
            Some(end) if end < last => rep.add(
                Level::Gap,
                Where { seq: Some(end + 1), ..w.clone() },
                format!("records #{}..#{last} come after the last signed checkpoint: they link to the chain but no signature covers them yet", end + 1),
            ),
            _ => {}
        }
        c.sum.signed_through = c.last_signed_end;
        if let Some(k) = c.sum.key_class {
            let (name, note) = KeyClass::from_u8(k).map_or(("unknown", "unknown key class"), |k| (k.name(), k.note()));
            rep.add(
                Level::Note,
                w.clone(),
                format!("key class: the log claims {name} (\"{note}\"). Whoever signed wrote that byte; it proves nothing by itself"),
            );
        }
        if c.sum.trusted_as.as_deref() == Some(TEST_KEY_LABEL) {
            rep.add(
                Level::Note,
                w.clone(),
                "signed with the published test key: accepted only because the test key was allowed. Its secret is in this project's source code; anyone can produce such a log",
            );
        }
        if c.sum.no_time > 0 {
            rep.add(Level::Note, w.clone(), format!("{} records have no UTC time (no GPS time yet): they carry uptime only, marked time source \"none\"", c.sum.no_time));
        }
        if c.sum.no_position > 0 {
            rep.add(Level::Note, w.clone(), format!("{} records have no hub position (no GPS fix and none typed in)", c.sum.no_position));
        }
        if c.sum.manual_position > 0 {
            rep.add(Level::Note, w, format!("{} records carry a typed-in (manual) hub position, not a GPS fix", c.sum.manual_position));
        }
    }
    if rep.records.is_empty() && !rep.findings.iter().any(|f| f.level == Level::Fail) {
        rep.add(Level::Fail, Where::default(), "no records: nothing to check");
    }
    rep.chains = chains.into_iter().map(|c| c.sum).collect();
    rep
}

/// Returns a card-mounted claim to check after all inputs are read:
/// (this file, previous file, previous length, previous last hash, status).
fn check_record(rep: &mut Report, c: &mut ChainState, r: &Record<'_>, hash: [u8; 32], w: &Where, opts: &Options) -> Option<(u32, u32, u32, [u8; 32], u8)> {
    let h = &r.header;
    // A card-mounted claim is checked whatever else is wrong with the record.
    let claim = match r.body {
        Body::Event(e) if e.code == EventCode::CardMounted as u16 && e.data.len() >= 45 => {
            let u = |i: usize| u32::from_le_bytes(e.data[i..i + 4].try_into().unwrap());
            Some((u(0), u(4), u(8), e.data[12..44].try_into().unwrap(), e.data[44]))
        }
        _ => None,
    };
    // A counter that has run to the end of u64 is not a real log (it would
    // take millennia): refuse rather than let `seq + 1` wrap to 0, which would
    // make the next record #0 look like its successor.
    let Some(next) = h.seq.checked_add(1) else {
        rep.add(
            Level::Fail,
            w.clone(),
            format!(
                "record number #{} cannot be real (the counter would wrap)",
                h.seq
            ),
        );
        return claim;
    };
    // Order and links.
    if h.seq < c.next {
        match c.hashes.get(&h.seq) {
            Some(prev) if *prev == hash => {
                rep.add(Level::Note, w.clone(), "the same record again (duplicate copy, ignored)");
                return claim;
            }
            Some(_) => {
                rep.add(Level::Fail, w.clone(), format!("a different record #{} appeared earlier: records were changed or reordered", h.seq));
                return claim;
            }
            None => {
                rep.add(Level::Fail, w.clone(), format!("record #{} is out of order (expected #{} next)", h.seq, c.next));
                return claim;
            }
        }
    }
    if h.seq > c.next {
        let (a, b) = (c.next, h.seq - 1);
        let level = if opts.gaps_allowed { Level::Gap } else { Level::Fail };
        let what = if opts.gaps_allowed {
            format!("records #{a}..#{b} are missing: they may have been removed, which cannot be told apart from records never received; record #{} cannot be linked to the one before it", h.seq)
        } else {
            format!("records #{a}..#{b} are missing: removed from the log (the sequence jumps from #{} to #{})", a.saturating_sub(1), h.seq)
        };
        rep.add(level, w.clone(), what);
        c.sum.missing.push((a, b));
    } else if h.prev != c.head {
        rep.add(
            Level::Fail,
            w.clone(),
            if h.seq == 0 {
                "the first record of a chain must have an all-zero previous hash".to_string()
            } else {
                format!("record #{} does not link to record #{}: one of them was changed, or records were removed or reordered", h.seq, h.seq - 1)
            },
        );
    }
    c.next = next;
    c.head = hash;
    c.hashes.insert(h.seq, hash);
    c.sum.records += 1;
    c.sum.first_seq = Some(c.sum.first_seq.map_or(h.seq, |f| f.min(h.seq)));
    c.sum.last_seq = Some(h.seq);

    // Header sanity.
    match c.sum.fw_hash {
        None => c.sum.fw_hash = Some(h.fw_hash),
        Some(f) if f != h.fw_hash => rep.add(Level::Fail, w.clone(), "firmware hash changed within one boot"),
        _ => {}
    }
    if h.uptime_ms < c.last_uptime {
        rep.add(Level::Fail, w.clone(), format!("uptime went backwards ({} ms after {} ms)", h.uptime_ms, c.last_uptime));
    }
    c.last_uptime = h.uptime_ms;
    match h.utc_ms {
        None => c.sum.no_time += 1,
        Some(u) => {
            if h.time_source == format::TimeSource::FreeRunning as u8 {
                c.sum.free_running += 1;
            }
            c.sum.utc_first.get_or_insert(u);
            c.sum.utc_last = Some(u);
            if let Some((prev, _)) = c.last_utc
                && u + 2000 < prev
                && !c.clock_event_since
            {
                rep.add(Level::Note, w.clone(), format!("UTC stepped back {} ms with no clock event logged", prev - u));
            }
            c.last_utc = Some((u, h.time_source));
            c.clock_event_since = false;
        }
    }
    if h.utc_ms.is_none() && h.time_source != 0 {
        rep.add(Level::Fail, w.clone(), "a time source is given but the UTC field is empty");
    }
    match h.pos_source {
        0 => c.sum.no_position += 1,
        2 => c.sum.manual_position += 1,
        _ => {}
    }

    match r.body {
        Body::Event(e) => {
            c.sum.events += 1;
            let code = EventCode::from_u16(e.code);
            if h.seq == 0 && code != Some(EventCode::Boot) {
                rep.add(Level::Fail, w.clone(), "record #0 must be the boot event");
            }
            if h.seq != 0 && code == Some(EventCode::Boot) {
                rep.add(Level::Fail, w.clone(), "a boot event that is not record #0");
            }
            match code {
                None => rep.add(Level::Note, w.clone(), format!("unknown event code {} (newer firmware?)", e.code)),
                Some(EventCode::Boot) => {
                    if e.data.len() < 1 + format::PUBKEY_LEN {
                        rep.add(Level::Fail, w.clone(), "boot event too short to hold the key");
                    } else {
                        c.sum.key_class = Some(e.data[0]);
                        c.sum.key = Some(e.data[1..1 + format::PUBKEY_LEN].try_into().unwrap());
                    }
                    if let Some(list) = boot_bench_list(e.text) {
                        rep.add(
                            Level::Note,
                            w.clone(),
                            format!("the boot event says a BENCH build wrote this log (bench: {list}): such a build can fake detections, positions or time, and its records are not field evidence"),
                        );
                    }
                }
                Some(EventCode::Clock) => c.clock_event_since = true,
                _ => {}
            }
        }
        Body::Detection(d) => {
            c.sum.detections += 1;
            check_detection(rep, &d, w);
        }
        Body::Checkpoint(cp) => {
            c.sum.checkpoints += 1;
            if check_checkpoint(rep, c, &cp, h.seq, w, opts) {
                c.last_signed_end = Some(h.seq);
            }
            if let Some(t) = c.last_checkpoint_uptime
                && h.uptime_ms.saturating_sub(t) > 150_000
            {
                rep.add(Level::Note, w.clone(), format!("{} s since the previous checkpoint (expected about 60 s): the card was out, or checkpoints are missing", (h.uptime_ms - t) / 1000));
            }
            c.last_checkpoint_uptime = Some(h.uptime_ms);
        }
    }
    claim
}

fn check_detection(rep: &mut Report, d: &format::DetectionBody<'_>, w: &Where) {
    match crate::summary::decode_raw(d.raw_format, d.raw) {
        Err(e) => rep.add(Level::Fail, w.clone(), format!("raw bytes do not decode: {e}")),
        Ok((info, summary, _)) => {
            let mut diffs = Vec::new();
            if info.frame_kind != d.frame_kind {
                diffs.push("frame kind");
            }
            if info.band != d.band {
                diffs.push("band");
            }
            if info.channel != d.channel {
                diffs.push("channel");
            }
            if info.freq_100khz != d.freq_100khz {
                diffs.push("frequency");
            }
            if info.rssi != d.rssi {
                diffs.push("signal strength");
            }
            if summary.as_bytes() != d.summary {
                diffs.push("decode summary");
            }
            if !diffs.is_empty() {
                rep.add(Level::Fail, w.clone(), format!("stored fields differ from a fresh decode of the raw bytes: {}", diffs.join(", ")));
            }
        }
    }
}

fn check_checkpoint(rep: &mut Report, c: &mut ChainState, cp: &CheckpointBody<'_>, seq: u64, w: &Where, opts: &Options) -> bool {
    let mut ok = true;
    if cp.count != seq {
        rep.add(Level::Fail, w.clone(), format!("checkpoint count {} does not equal its own position #{seq}", cp.count));
        ok = false;
    }
    let digest = crate::sha256(cp.signed);
    if sign::is_high_s(&cp.signature) {
        rep.add(Level::Fail, w.clone(), "checkpoint signature is in the high-S form: a lax checker would accept it, but it is not what a signer writes (a valid signature turned into a second one), so it is refused");
        return false;
    }
    if !sign::verify_digest(&cp.pubkey, &digest, &cp.signature) {
        rep.add(Level::Fail, w.clone(), "checkpoint signature does not verify: the checkpoint (or the chain head it signs) was changed");
        return false;
    }
    match c.sum.key {
        Some(k) if k != cp.pubkey => {
            rep.add(Level::Fail, w.clone(), "checkpoint signed with a different key from the one the boot event names");
            ok = false;
        }
        None => {
            c.sum.key = Some(cp.pubkey);
            c.sum.key_class.get_or_insert(cp.key_class);
        }
        _ => {}
    }
    // Who signed: only a key from outside the log can say.
    if cp.pubkey == sign::public_test_key() {
        if opts.allow_test_key {
            c.sum.trusted_as = Some(TEST_KEY_LABEL.into());
        } else {
            if !c.key_refused {
                rep.add(
                    Level::Fail,
                    w.clone(),
                    "signed with the published test key: its secret is in this project's source code, so anyone can produce such a log. It is never accepted as evidence (this project's own tests opt in with --allow-test-key)",
                );
                c.key_refused = true;
            }
            ok = false;
        }
    } else if let Some(label) = opts.label_of(&cp.pubkey) {
        c.sum.trusted_as = Some(label);
    } else if opts.outside_keys() {
        if !c.key_refused {
            rep.add(
                Level::Fail,
                w.clone(),
                format!(
                    "checkpoint signed by key {}, which is not one of the keys you supplied (--key/--keys, or the key registered for this device)",
                    std::str::from_utf8(&crate::fingerprint(&cp.pubkey)).unwrap_or("?")
                ),
            );
            c.key_refused = true;
        }
        ok = false;
    }
    if c.sum.key_class.is_some_and(|k| k != cp.key_class) {
        rep.add(Level::Fail, w.clone(), "checkpoint key class differs from the boot event's");
        ok = false;
    }
    if let Some(class) = KeyClass::from_u8(cp.key_class)
        && cp.note != class.note().as_bytes()
    {
        rep.add(Level::Note, w.clone(), "checkpoint note does not match its key class");
    }
    ok
}

/// Text from a record, safe to print: invalid UTF-8 and control characters
/// (an ESC would drive the terminal) become `?`/U+FFFD and `?`. The summary
/// and event text come off the air.
fn printable(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// One line per record, for `evidence-verify --list`.
pub fn describe(r: &Record<'_>) -> String {
    let h = &r.header;
    let mut s = format!("#{:<6} {:>9.3}s ", h.seq, h.uptime_ms as f64 / 1000.0);
    match h.utc_ms {
        Some(u) => {
            let _ = write!(s, "{} [{}]", fmt_utc(u), format::TimeSource::from_u8(h.time_source).map_or("?", |t| t.name()));
        }
        None => s.push_str("NO-UTC                   [none]   "),
    }
    match h.pos_source {
        0 => {
            let _ = write!(s, " hub=NO-POSITION(gps {})", format::gps_state_name(h.gps_state));
        }
        p => {
            let _ = write!(
                s,
                " hub={:.6},{:.6}{}",
                h.lat_e7 as f64 / 1e7,
                h.lon_e7 as f64 / 1e7,
                if p == 2 { "(manual)" } else { "" }
            );
        }
    }
    match r.body {
        Body::Event(e) => {
            let _ = write!(
                s,
                " EVENT {} {}",
                EventCode::from_u16(e.code).map_or("?", |c| c.name()),
                printable(e.text)
            );
        }
        Body::Detection(d) => {
            let _ = write!(
                s,
                " DETECTION node={} {}",
                hex(&d.node),
                printable(d.summary)
            );
        }
        Body::Checkpoint(c) => {
            let _ = write!(
                s,
                " CHECKPOINT {} count={} key={} ({})",
                format::CheckpointReason::name(c.reason),
                c.count,
                std::str::from_utf8(&crate::fingerprint(&c.pubkey)).unwrap_or("?"),
                printable(c.note)
            );
        }
    }
    s
}

pub mod bundle {
    //! The export bundle: what a server hands over for one device. JSON so
    //! anyone can read it; the records inside are the exact uploaded bytes
    //! (base64). See docs/EVIDENCE-FORMAT.md "Export bundle".

    use serde::{Deserialize, Serialize};

    pub const FORMAT: &str = "rotorscope-evidence-bundle/1";

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Bundle {
        pub format: String,
        pub created_utc_ms: u64,
        pub server: String,
        pub device: Device,
        /// Every batch the server stored for the device, in arrival order.
        pub batches: Vec<Batch>,
        /// The device's whole audit chain on the server, oldest first.
        pub audit: Vec<AuditLine>,
        /// Firmware hashes seen in the records (SHA-256 of the ELF), hex.
        pub firmware_hashes: Vec<String>,
        /// Outside time-stamps over chain heads, if any.
        pub timestamps: Vec<Timestamp>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Device {
        pub key_id: String,
        /// 65-byte SEC1 uncompressed public key, hex.
        pub public_key: String,
        pub name: String,
        pub registered_utc_ms: u64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Batch {
        pub n: u64,
        pub received_utc_ms: u64,
        /// SHA-256 of the batch bytes (the decoded `records`), hex.
        pub sha256: String,
        /// The batch exactly as uploaded: records back to back, base64.
        pub records: String,
    }

    /// One audit entry: `entry` is a JSON object kept as the exact string
    /// that was hashed; `hash` = SHA-256(previous hash bytes || entry bytes),
    /// hex; the first entry's previous hash is 32 zero bytes.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct AuditLine {
        pub entry: String,
        pub hash: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct AuditEntry {
        pub n: u64,
        pub utc_ms: u64,
        pub actor: String,
        /// register, upload, view, export, annotate, timestamp
        pub action: String,
        pub device: String,
        pub detail: serde_json::Value,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Timestamp {
        /// "rfc3161" or "opentimestamps".
        pub kind: String,
        pub authority: String,
        /// What was stamped: SHA-256 over boot id || seq (LE u64) || head.
        pub imprint: String,
        pub boot: String,
        pub seq: u64,
        pub head: String,
        /// The authority's reply, base64.
        pub token: String,
    }

    pub fn audit_hash(prev: &[u8; 32], entry: &str) -> [u8; 32] {
        let mut b = Vec::with_capacity(32 + entry.len());
        b.extend_from_slice(prev);
        b.extend_from_slice(entry.as_bytes());
        crate::sha256(&b)
    }

    /// What a chain-head time-stamp covers.
    pub fn head_imprint(boot: &[u8; 8], seq: u64, head: &[u8; 32]) -> [u8; 32] {
        let mut b = Vec::with_capacity(48);
        b.extend_from_slice(boot);
        b.extend_from_slice(&seq.to_le_bytes());
        b.extend_from_slice(head);
        crate::sha256(&b)
    }
}

/// Check a bundle: its audit chain, that its batches are exactly the
/// uploaded ones, then the records. The device key the bundle names is the
/// server's word, not an outside key: the signer is only checked against
/// `opts`' keys. Missing runs between batches fail unless
/// `opts.gaps_allowed`.
pub fn verify_bundle(json: &[u8], opts: &Options) -> Report {
    use bundle::*;
    let mut rep = Report::default();
    let w = |s: &str| Where { source: s.to_string(), ..Default::default() };
    let b: Bundle = match serde_json::from_slice(json) {
        Ok(b) => b,
        Err(e) => {
            rep.add(Level::Fail, w("bundle"), format!("not a bundle: {e}"));
            return rep;
        }
    };
    if b.format != FORMAT {
        rep.add(Level::Fail, w("bundle"), format!("unknown bundle format {:?}", b.format));
    }
    let key: Option<[u8; 65]> = unhex(&b.device.public_key).and_then(|v| v.try_into().ok());
    if key.is_none() {
        rep.add(Level::Fail, w("bundle device"), "public key is not 65 hex bytes");
    }
    if let Some(k) = key
        && std::str::from_utf8(&crate::fingerprint(&k)).ok() != Some(b.device.key_id.as_str())
    {
        rep.add(Level::Fail, w("bundle device"), "key id does not match the public key");
    }
    if let Some(k) = key
        && opts.outside_keys()
        && opts.label_of(&k).is_none()
    {
        rep.add(Level::Fail, w("bundle device"), "the bundle names a device key that is not one of the keys you supplied");
    }
    // Audit chain.
    let mut prev = [0u8; 32];
    let mut uploads: Vec<(u64, String)> = Vec::new();
    for (i, line) in b.audit.iter().enumerate() {
        let at = w(&format!("audit entry {i}"));
        let h = audit_hash(&prev, &line.entry);
        if hex(&h) != line.hash {
            rep.add(Level::Fail, at.clone(), "audit entry does not hash to its stored value, or does not link to the entry before it (edited, removed or reordered)");
        }
        prev = unhex(&line.hash).and_then(|v| v.try_into().ok()).unwrap_or(h);
        match serde_json::from_str::<AuditEntry>(&line.entry) {
            Ok(e) => {
                if e.n != i as u64 {
                    rep.add(Level::Fail, at.clone(), format!("audit entry numbered {} at position {i}", e.n));
                }
                if e.device != b.device.key_id {
                    rep.add(Level::Fail, at.clone(), "audit entry for another device");
                }
                if e.action == "upload" {
                    let n = e.detail.get("batch").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
                    let sha = e.detail.get("sha256").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    uploads.push((n, sha));
                }
                if i == 0 && e.action != "register" {
                    rep.add(Level::Fail, at, "the audit chain must start with the device's registration");
                }
            }
            Err(e) => rep.add(Level::Fail, at, format!("audit entry is not valid: {e}")),
        }
    }
    // Batches are exactly the logged uploads.
    let mut inputs = Vec::new();
    for (i, batch) in b.batches.iter().enumerate() {
        let name = format!("batch {}", batch.n);
        let Some(bytes) = crate::b64::decode_vec(&batch.records) else {
            rep.add(Level::Fail, w(&name), "records are not valid base64");
            continue;
        };
        let sha = hex(&crate::sha256(&bytes));
        if sha != batch.sha256 {
            rep.add(Level::Fail, w(&name), "batch bytes do not hash to the batch's SHA-256 (records edited, removed or reordered)");
        }
        match uploads.get(i) {
            Some((n, s)) if *n == batch.n && *s == sha => {}
            Some(_) => rep.add(Level::Fail, w(&name), "batch does not match the audit log's upload entry at this position"),
            None => rep.add(Level::Fail, w(&name), "batch has no upload entry in the audit log"),
        }
        // The server only accepts contiguous runs: a hole inside a batch was
        // made after upload.
        let mut prev: Option<([u8; 8], u64)> = None;
        for (off, r) in format::records(&bytes) {
            let Ok(r) = r else { break };
            if let Some((b, s)) = prev
                && b == r.header.boot_id
                && s.checked_add(1) != Some(r.header.seq)
            {
                rep.add(
                    Level::Fail,
                    Where { source: name.clone(), offset: Some(off), boot: Some(b), seq: Some(r.header.seq) },
                    format!("inside one uploaded batch record #{} follows #{s}: records were removed or reordered after upload", r.header.seq),
                );
            }
            prev = Some((r.header.boot_id, r.header.seq));
        }
        inputs.push(Input { name, bytes, file_no: None });
    }
    if uploads.len() > b.batches.len() {
        rep.add(
            Level::Fail,
            w("bundle"),
            format!("the audit log records {} uploads but the bundle holds {} batches: batches were removed", uploads.len(), b.batches.len()),
        );
    }
    // A gap filled later arrives in a later batch: check the records in
    // chain order (each batch holds one boot's contiguous run; their arrival
    // order was checked against the audit log above).
    inputs.sort_by_key(|i| format::parse(&i.bytes).map(|r| (r.header.boot_id, r.header.seq)).unwrap_or(([0xFF; 8], u64::MAX)));
    let inner = verify(&inputs, opts);
    rep.findings.extend(inner.findings);
    rep.chains = inner.chains;
    rep.records = inner.records;
    for c in &rep.chains {
        if let (Some(ck), Some(bk)) = (c.key, key)
            && ck != bk
        {
            rep.findings.push(Finding {
                level: Level::Fail,
                at: Where { boot: Some(c.boot), ..Default::default() },
                what: "this chain is signed by a key other than the device key the bundle names".into(),
            });
        }
    }
    for c in &rep.chains {
        if let Some(f) = c.fw_hash
            && !b.firmware_hashes.contains(&hex(&f))
        {
            rep.findings.push(Finding {
                level: Level::Fail,
                at: Where { boot: Some(c.boot), ..Default::default() },
                what: "the bundle's firmware hash list does not name this chain's firmware".into(),
            });
        }
    }
    for t in &b.timestamps {
        let ok = unhex(&t.boot).and_then(|v| <[u8; 8]>::try_from(v).ok()).zip(unhex(&t.head).and_then(|v| <[u8; 32]>::try_from(v).ok()));
        let Some((boot, head)) = ok else {
            rep.add(Level::Fail, w("timestamp"), "time-stamp names a malformed chain head");
            continue;
        };
        if hex(&head_imprint(&boot, t.seq, &head)) != t.imprint {
            rep.add(Level::Fail, w("timestamp"), "time-stamp imprint does not match the chain head it names");
        }
        let known = rep.records.iter().any(|(_, _, r)| r.boot == boot && r.seq == t.seq && r.hash == head);
        if !known {
            rep.add(Level::Fail, w("timestamp"), format!("time-stamped chain head (record #{}) is not in the bundle", t.seq));
        } else {
            rep.add(Level::Note, w("timestamp"), format!("{} time-stamp from {} over record #{}: check the token with the authority's tools (e.g. openssl ts -verify)", t.kind, t.authority, t.seq));
        }
    }
    rep.findings.sort_by_key(|f| std::cmp::Reverse(f.level));
    rep
}
