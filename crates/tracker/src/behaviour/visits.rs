//! Repeat visitors (threat rule 17, slice 16d): which drones and which
//! pilot spots were seen at the protected places, and on which days.
//!
//! **What is kept, and nothing more** (owner decision 2026-11-18: 30 days,
//! wipeable from the hub):
//! - for a drone: a 64-bit fingerprint of its ID (serial number, CAA
//!   registration, or DJI serial), not the ID itself, and the UTC days it
//!   was seen at or near a place;
//! - for a pilot spot: the 25 m square of the map the pilot stood in while
//!   the drone was at a place, and the UTC days.
//!
//! No times of day, no paths, no drone positions. A day is a UTC day number
//! (days since 1970-01-01). Days older than [`KEEP_DAYS`] are dropped every
//! day and when the file is read. Session IDs and UTM IDs are new every
//! flight, so they are not kept (they could never repeat). Friends are not
//! kept either.
//!
//! **Size.** [`IDS_PER_RING`] drone entries and [`SPOTS_PER_RING`] pilot
//! spots per drone path ring: 256 and 128 on the P4 hub (16 bytes each,
//! 6 KB); none on hubs without path history. When the table is full, the
//! entry seen least recently makes room.
//!
//! **A repeat visitor** is a drone ID, or a pilot spot (the square and the
//! two rings of squares round it: any two pilot positions within 50 m
//! match, never two more than about 100 m apart), seen on [`REPEAT_DAYS`] or more
//! different days within the last [`WINDOW_DAYS`] days, today included.
//!
//! **File** (`ROTORSCP/VISITS.DAT` on the P4 hub; written by
//! [`Visits::write`], read back by [`Loader`]): a 16-byte header, one
//! 16-byte record per entry, then a 4-byte checksum, all little-endian.
//!
//! | Bytes | Header |
//! |---|---|
//! | 0-3 | `RSVT` |
//! | 4 | version, [`VERSION`] |
//! | 5 | 0 |
//! | 6-7 | drone entries |
//! | 8-9 | pilot-spot entries |
//! | 10-15 | 0 |
//!
//! | Bytes | Record |
//! |---|---|
//! | 0 | 1 drone, 2 pilot spot |
//! | 1 | 0 |
//! | 2-3 | the newest day seen (UTC day number) |
//! | 4-7 | days seen: bit `i` is the newest day minus `i` |
//! | 8-15 | drone: the ID's fingerprint (u64); spot: the square, north then east (two i32) |
//!
//! The checksum is FNV-1a (32 bits) of every byte before it. A file with
//! another version, a wrong length or a wrong checksum is discarded whole
//! (the caller logs it); nothing in it can crash the reader.

/// Days of history kept.
pub const KEEP_DAYS: u16 = 30;
/// A repeat visitor is seen on [`REPEAT_DAYS`] different days within this
/// many days.
pub const WINDOW_DAYS: u16 = 14;
pub const REPEAT_DAYS: u32 = 3;
/// Size of a pilot-spot square, metres.
pub const SPOT_GRID_M: f64 = 25.0;
/// Squares this many apart (each way) are the same pilot spot: 50 m.
pub const SPOT_NEAR_CELLS: i32 = 2;
/// Drone entries per drone path ring.
pub const IDS_PER_RING: usize = 4;
/// Pilot-spot entries per drone path ring.
pub const SPOTS_PER_RING: usize = 2;
/// The file format's version.
pub const VERSION: u8 = 1;
pub const MAGIC: [u8; 4] = *b"RSVT";
const HEADER: usize = 16;
const RECORD: usize = 16;
const TRAILER: usize = 4;
const KIND_ID: u8 = 1;
const KIND_SPOT: u8 = 2;
const MS_PER_DAY: u64 = 86_400_000;
/// Metres per degree of latitude (as in [`super::places`]).
const M_PER_DEG: f64 = 111_195.0;

/// The UTC day number of a UTC time (Unix ms).
pub fn day_of(utc_ms: u64) -> u16 {
    (utc_ms / MS_PER_DAY).min(u64::from(u16::MAX)) as u16
}

/// The 25 m square a position falls in: (north, east) square numbers.
pub type Cell = (i32, i32);

/// The square of the map a pilot stands in.
pub fn spot_cell(lat: f64, lon: f64) -> Cell {
    let n = libm::floor(lat * M_PER_DEG / SPOT_GRID_M);
    let e = libm::floor(lon * M_PER_DEG * libm::cos(lat.to_radians()) / SPOT_GRID_M);
    (n as i32, e as i32)
}

/// FNV-1a, 64 bits: the fingerprint kept for a drone ID.
pub fn fingerprint(id_type: u8, id: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in core::iter::once(&id_type).chain(id) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h | 1
}

fn fnv32(mut h: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}
const FNV32_START: u32 = 0x811c_9dc5;

/// The days one entry was seen on: the newest, and the 31 before it as
/// bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Days {
    last: u16,
    /// Bit `i`: seen on day `last - i`. Bit 0 is always set unless empty.
    bits: u32,
}

impl Days {
    fn is_empty(&self) -> bool {
        self.bits == 0
    }

    /// Mark day `d`; true when it was not marked before.
    fn see(&mut self, d: u16) -> bool {
        if self.bits == 0 {
            *self = Days { last: d, bits: 1 };
            return true;
        }
        if d > self.last {
            let shift = u32::from(d - self.last);
            self.bits = if shift >= 32 { 0 } else { self.bits << shift } | 1;
            self.last = d;
            return true;
        }
        let back = u32::from(self.last - d);
        if back >= 32 {
            return false;
        }
        let had = (self.bits >> back) & 1 != 0;
        self.bits |= 1 << back;
        !had
    }

    fn has(&self, d: u16) -> bool {
        d <= self.last && u32::from(self.last - d) < 32 && (self.bits >> (self.last - d)) & 1 != 0
    }

    /// The days within [`WINDOW_DAYS`] ending `today`, as bits (bit `i`:
    /// `today - i`).
    fn window(&self, today: u16) -> u32 {
        let mut m = 0;
        for i in 0..WINDOW_DAYS {
            if today.checked_sub(i).is_some_and(|d| self.has(d)) {
                m |= 1 << i;
            }
        }
        m
    }

    /// Drop the days older than [`KEEP_DAYS`] before `today`.
    fn expire(&mut self, today: u16) {
        let oldest = today.saturating_sub(KEEP_DAYS - 1);
        if self.last < oldest {
            self.bits = 0;
            return;
        }
        let keep = u32::from(self.last - oldest) + 1;
        if keep < 32 {
            self.bits &= (1u32 << keep) - 1;
        }
    }

    fn merge(&mut self, o: Days) -> bool {
        let mut changed = false;
        for i in 0..32u16 {
            if (o.bits >> i) & 1 != 0
                && let Some(d) = o.last.checked_sub(i)
            {
                changed |= self.see(d);
            }
        }
        changed
    }

    /// Days marked.
    pub fn count(&self) -> u32 {
        self.bits.count_ones()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct IdEntry {
    key: u64,
    days: Days,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SpotEntry {
    cell: Cell,
    days: Days,
}

/// The visit table. Sized by `P`, the tracker's path rings: nothing at all
/// on hubs that keep no path history.
#[derive(Debug, Clone)]
pub struct Visits<const P: usize> {
    ids: [[IdEntry; IDS_PER_RING]; P],
    spots: [[SpotEntry; SPOTS_PER_RING]; P],
    /// Changed since the last [`Visits::take_dirty`] (a new day for an
    /// entry, an entry added, dropped or wiped).
    dirty: bool,
}

impl<const P: usize> Default for Visits<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// The slot to use in a full or partly full table: a free one, else the
/// one seen least recently (fewest days among equals).
fn room<T>(t: &[T], days: impl Fn(&T) -> Days) -> Option<usize> {
    if let Some(i) = t.iter().position(|e| days(e).is_empty()) {
        return Some(i);
    }
    (0..t.len()).min_by_key(|&i| (days(&t[i]).last, days(&t[i]).count()))
}

impl<const P: usize> Visits<P> {
    const NO_ID: [IdEntry; IDS_PER_RING] = [IdEntry { key: 0, days: Days { last: 0, bits: 0 } }; IDS_PER_RING];
    const NO_SPOT: [SpotEntry; SPOTS_PER_RING] = [SpotEntry { cell: (0, 0), days: Days { last: 0, bits: 0 } }; SPOTS_PER_RING];

    pub const fn new() -> Self {
        Self { ids: [Self::NO_ID; P], spots: [Self::NO_SPOT; P], dirty: false }
    }

    /// Whether the table can hold anything (a hub with path history).
    pub const fn enabled() -> bool {
        P > 0
    }

    /// A drone ID (its [`fingerprint`]) seen at a place on `day`.
    pub fn see_id(&mut self, key: u64, day: u16) {
        self.merge_id(key, Days { last: day, bits: 1 });
    }

    fn merge_id(&mut self, key: u64, d: Days) {
        let t = self.ids.as_flattened_mut();
        let i = match t.iter().position(|e| !e.days.is_empty() && e.key == key) {
            Some(i) => i,
            None => {
                let Some(i) = room(t, |e| e.days) else { return };
                t[i] = IdEntry { key, days: Days::default() };
                self.dirty = true;
                i
            }
        };
        self.dirty |= t[i].days.merge(d);
    }

    /// A pilot standing in square `cell` while its drone was at a place.
    pub fn see_spot(&mut self, cell: Cell, day: u16) {
        self.merge_spot(cell, Days { last: day, bits: 1 });
    }

    fn merge_spot(&mut self, cell: Cell, d: Days) {
        let t = self.spots.as_flattened_mut();
        let i = match t.iter().position(|e| !e.days.is_empty() && e.cell == cell) {
            Some(i) => i,
            None => {
                let Some(i) = room(t, |e| e.days) else { return };
                t[i] = SpotEntry { cell, days: Days::default() };
                self.dirty = true;
                i
            }
        };
        self.dirty |= t[i].days.merge(d);
    }

    /// Different days within [`WINDOW_DAYS`] ending `today` this drone ID
    /// was seen at a place.
    pub fn id_days(&self, key: u64, today: u16) -> u32 {
        self.ids
            .as_flattened()
            .iter()
            .find(|e| !e.days.is_empty() && e.key == key)
            .map_or(0, |e| e.days.window(today).count_ones())
    }

    /// Different days within [`WINDOW_DAYS`] ending `today` a pilot stood
    /// in this square or one up to two squares from it (within 50 m) while
    /// a drone was at a place.
    pub fn spot_days(&self, cell: Cell, today: u16) -> u32 {
        let mut m = 0;
        for e in self.spots.as_flattened().iter().filter(|e| !e.days.is_empty()) {
            if (e.cell.0 - cell.0).abs() <= SPOT_NEAR_CELLS && (e.cell.1 - cell.1).abs() <= SPOT_NEAR_CELLS {
                m |= e.days.window(today);
            }
        }
        m.count_ones()
    }

    /// Drop days older than [`KEEP_DAYS`]; entries left with none are
    /// freed. Returns how many entries were freed.
    pub fn prune(&mut self, today: u16) -> usize {
        let mut freed = 0;
        for d in self.ids.as_flattened_mut().iter_mut().map(|e| &mut e.days).chain(self.spots.as_flattened_mut().iter_mut().map(|e| &mut e.days)) {
            if d.is_empty() {
                continue;
            }
            let before = *d;
            d.expire(today);
            if *d != before {
                self.dirty = true;
            }
            if d.is_empty() {
                freed += 1;
            }
        }
        freed
    }

    /// Drone entries and pilot spots held.
    pub fn counts(&self) -> (usize, usize) {
        (
            self.ids.as_flattened().iter().filter(|e| !e.days.is_empty()).count(),
            self.spots.as_flattened().iter().filter(|e| !e.days.is_empty()).count(),
        )
    }

    /// Forget everything (the operator's wipe, `visits off`, or a file that
    /// could not be read). Returns what was held.
    pub fn clear(&mut self) -> (usize, usize) {
        let held = self.counts();
        *self = Self { dirty: true, ..Self::new() };
        held
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Whether it changed since last asked (then it needs writing).
    pub fn take_dirty(&mut self) -> bool {
        core::mem::take(&mut self.dirty)
    }

    /// The file's length.
    pub fn file_len(&self) -> usize {
        let (a, b) = self.counts();
        HEADER + RECORD * (a + b) + TRAILER
    }

    /// Write the file in pieces of up to 512 bytes; `out` returns false to
    /// stop (a write failed). True when every piece was taken.
    #[inline(never)]
    pub fn write(&self, mut out: impl FnMut(&[u8]) -> bool) -> bool {
        let mut buf = [0u8; 512];
        let mut n = 0;
        let mut hash = FNV32_START;
        let (a, b) = self.counts();
        let mut head = [0u8; HEADER];
        head[..4].copy_from_slice(&MAGIC);
        head[4] = VERSION;
        head[6..8].copy_from_slice(&(a as u16).to_le_bytes());
        head[8..10].copy_from_slice(&(b as u16).to_le_bytes());
        let ids = self.ids.as_flattened().iter().filter(|e| !e.days.is_empty()).map(|e| {
            let mut r = [0u8; RECORD];
            r[0] = KIND_ID;
            r[2..4].copy_from_slice(&e.days.last.to_le_bytes());
            r[4..8].copy_from_slice(&e.days.bits.to_le_bytes());
            r[8..16].copy_from_slice(&e.key.to_le_bytes());
            r
        });
        let spots = self.spots.as_flattened().iter().filter(|e| !e.days.is_empty()).map(|e| {
            let mut r = [0u8; RECORD];
            r[0] = KIND_SPOT;
            r[2..4].copy_from_slice(&e.days.last.to_le_bytes());
            r[4..8].copy_from_slice(&e.days.bits.to_le_bytes());
            r[8..12].copy_from_slice(&e.cell.0.to_le_bytes());
            r[12..16].copy_from_slice(&e.cell.1.to_le_bytes());
            r
        });
        for r in core::iter::once(head).chain(ids).chain(spots) {
            hash = fnv32(hash, &r);
            buf[n..n + RECORD].copy_from_slice(&r);
            n += RECORD;
            if n == buf.len() {
                if !out(&buf) {
                    return false;
                }
                n = 0;
            }
        }
        buf[n..n + TRAILER].copy_from_slice(&hash.to_le_bytes());
        out(&buf[..n + TRAILER])
    }
}

/// Why a visits file was not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    /// Not a visits file (wrong first four bytes).
    NotOurs,
    /// Written by another version of the firmware.
    Version(u8),
    /// Shorter than its header says.
    Short,
    /// Longer than its header says.
    Long,
    /// The checksum does not match: damaged.
    Checksum,
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoadError::NotOurs => f.write_str("not a visits file"),
            LoadError::Version(v) => write!(f, "file version {v}, this firmware reads version {VERSION}"),
            LoadError::Short => f.write_str("shorter than its header says (cut off)"),
            LoadError::Long => f.write_str("longer than its header says"),
            LoadError::Checksum => f.write_str("checksum does not match (damaged)"),
        }
    }
}

/// What a good file held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loaded {
    pub ids: usize,
    pub spots: usize,
}

/// Reads a visits file fed in pieces of any size ([`Loader::feed`]), adding
/// its entries to a table as they come (merged with what the table holds).
/// If the file turns out bad ([`Loader::finish`]), the whole table is
/// cleared: half a damaged file is never kept.
#[derive(Debug, Clone)]
pub struct Loader {
    rec: [u8; RECORD],
    have: usize,
    /// Bytes taken so far.
    at: usize,
    /// The file's length, once the header is read.
    len: Option<usize>,
    hash: u32,
    err: Option<LoadError>,
    ids: usize,
    spots: usize,
}

impl Default for Loader {
    fn default() -> Self {
        Self::new()
    }
}

impl Loader {
    pub const fn new() -> Self {
        Self { rec: [0; RECORD], have: 0, at: 0, len: None, hash: FNV32_START, err: None, ids: 0, spots: 0 }
    }

    /// The next piece of the file.
    pub fn feed<const P: usize>(&mut self, v: &mut Visits<P>, bytes: &[u8]) {
        for &b in bytes {
            if self.err.is_some() {
                return;
            }
            self.byte(v, b);
        }
    }

    fn byte<const P: usize>(&mut self, v: &mut Visits<P>, b: u8) {
        if let Some(len) = self.len
            && self.at >= len
        {
            self.err = Some(LoadError::Long);
            return;
        }
        self.at += 1;
        // The trailer is not part of the checksum.
        let body = self.len.is_none_or(|len| self.at <= len - TRAILER);
        if body {
            self.hash = fnv32(self.hash, &[b]);
        }
        self.rec[self.have] = b;
        self.have += 1;
        if self.len.is_none() && self.have == HEADER {
            self.have = 0;
            let h = self.rec;
            if h[..4] != MAGIC {
                self.err = Some(LoadError::NotOurs);
                return;
            }
            if h[4] != VERSION {
                self.err = Some(LoadError::Version(h[4]));
                return;
            }
            let n = usize::from(u16::from_le_bytes([h[6], h[7]])) + usize::from(u16::from_le_bytes([h[8], h[9]]));
            self.len = Some(HEADER + RECORD * n + TRAILER);
            return;
        }
        let Some(len) = self.len else { return };
        if body && self.have == RECORD {
            self.have = 0;
            self.record(v);
        } else if !body && self.at == len {
            self.have = 0;
            if u32::from_le_bytes([self.rec[0], self.rec[1], self.rec[2], self.rec[3]]) != self.hash {
                self.err = Some(LoadError::Checksum);
            }
        }
    }

    fn record<const P: usize>(&mut self, v: &mut Visits<P>) {
        let r = &self.rec;
        let last = u16::from_le_bytes([r[2], r[3]]);
        let bits = u32::from_le_bytes([r[4], r[5], r[6], r[7]]);
        if bits & 1 == 0 {
            // Not something this firmware writes: skipped.
            return;
        }
        let days = Days { last, bits };
        match r[0] {
            KIND_ID => {
                let key = u64::from_le_bytes([r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15]]);
                let dirty = v.dirty;
                v.merge_id(key, days);
                v.dirty = dirty;
                self.ids += 1;
            }
            KIND_SPOT => {
                let n = i32::from_le_bytes([r[8], r[9], r[10], r[11]]);
                let e = i32::from_le_bytes([r[12], r[13], r[14], r[15]]);
                let dirty = v.dirty;
                v.merge_spot((n, e), days);
                v.dirty = dirty;
                self.spots += 1;
            }
            _ => {}
        }
    }

    /// The file ended. A bad file clears the table (and marks it to be
    /// written again, so the bad file is replaced). With `today` known,
    /// days older than [`KEEP_DAYS`] are dropped at once.
    pub fn finish<const P: usize>(self, v: &mut Visits<P>, today: Option<u16>) -> Result<Loaded, LoadError> {
        let err = match (self.err, self.len) {
            (Some(e), _) => Some(e),
            (None, None) if self.at == 0 => Some(LoadError::Short),
            (None, None) => Some(if self.at < 4 || self.rec[..self.at.min(4)] == MAGIC[..self.at.min(4)] { LoadError::Short } else { LoadError::NotOurs }),
            (None, Some(len)) if self.at < len => Some(LoadError::Short),
            _ => None,
        };
        if let Some(e) = err {
            v.clear();
            return Err(e);
        }
        if let Some(d) = today {
            v.prune(d);
        }
        Ok(Loaded { ids: self.ids, spots: self.spots })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    const DAY: u16 = 20_775; // 2026-11-18

    fn file<const P: usize>(v: &Visits<P>) -> Vec<u8> {
        let mut f = Vec::new();
        assert!(v.write(|c| {
            f.extend_from_slice(c);
            true
        }));
        assert_eq!(f.len(), v.file_len());
        f
    }

    fn load<const P: usize>(bytes: &[u8], piece: usize, today: Option<u16>) -> (Visits<P>, Result<Loaded, LoadError>) {
        let mut v = Visits::<P>::new();
        let mut l = Loader::new();
        for c in bytes.chunks(piece.max(1)) {
            l.feed(&mut v, c);
        }
        let r = l.finish(&mut v, today);
        (v, r)
    }

    #[test]
    fn days_are_counted_within_two_weeks_and_kept_thirty() {
        let mut d = Days::default();
        assert!(d.see(DAY - 20));
        assert!(d.see(DAY - 10));
        assert!(!d.see(DAY - 10), "the same day twice counts once");
        assert!(d.see(DAY - 5));
        assert!(d.see(DAY - 12), "an older day still counts");
        assert_eq!(d.window(DAY).count_ones(), 3, "-5, -10, -12 (not -20)");
        assert_eq!(d.count(), 4);
        d.expire(DAY + 15);
        // Day + 15 keeps back to day - 14: -12, -10, -5 stay.
        assert_eq!(d.count(), 3);
        d.expire(DAY + 30);
        assert_eq!(d.count(), 0, "everything older than 30 days is gone");
        // A day far ahead (clock jump) starts afresh.
        let mut e = Days { last: DAY, bits: 1 };
        e.see(DAY + 40);
        assert_eq!((e.last, e.bits), (DAY + 40, 1));
    }

    #[test]
    fn ids_and_spots_repeat_and_the_table_makes_room() {
        // Two rings: eight drone entries, four pilot spots.
        let mut v = Visits::<2>::new();
        let k = fingerprint(1, b"SN-1");
        for d in [DAY - 13, DAY - 7, DAY] {
            v.see_id(k, d);
        }
        assert_eq!(v.id_days(k, DAY), 3);
        assert_eq!(v.id_days(k, DAY + 1), 2, "day - 13 left the window");
        // Pilot spots: two squares away counts, three squares away not.
        let c = spot_cell(39.9521, -75.1638);
        v.see_spot(c, DAY - 2);
        v.see_spot((c.0 + 1, c.1 - 2), DAY - 1);
        v.see_spot((c.0 + 3, c.1), DAY);
        assert_eq!(v.spot_days(c, DAY), 2);
        assert_eq!(v.spot_days((c.0 + 1, c.1), DAY), 3);
        // Eight IDs fit; a ninth replaces the one seen longest ago.
        for i in 2u8..=8 {
            v.see_id(fingerprint(1, &[b'X', i]), if i == 2 { DAY - 9 } else { DAY - 1 });
        }
        assert_eq!(v.counts().0, 8);
        v.see_id(fingerprint(1, b"NEW"), DAY);
        assert_eq!(v.counts().0, 8);
        assert_eq!(v.id_days(fingerprint(1, &[b'X', 2]), DAY), 0, "the oldest went");
        assert_eq!(v.id_days(k, DAY), 3, "the busy one stayed");
        // Pruning: after 31 more days nothing is left.
        assert!(v.take_dirty());
        assert_eq!(v.prune(DAY + 31), 11);
        assert_eq!(v.counts(), (0, 0));
        assert!(v.dirty());
        // A spot 30 m away is the next square or the one after: still found.
        let a = spot_cell(39.9521, -75.1638);
        let (lat2, lon2) = super::super::places::Frame::at(39.9521, -75.1638).latlon(30.0, 0.0);
        let b = spot_cell(lat2, lon2);
        assert!((a.1 - b.1).abs() <= 2 && a.0 == b.0, "{a:?} {b:?}");
        // No ring, no table.
        assert!(core::mem::size_of::<Visits<0>>() <= 8);
        assert!(!Visits::<0>::enabled());
        let mut none = Visits::<0>::new();
        none.see_id(k, DAY);
        assert_eq!(none.id_days(k, DAY), 0);
    }

    #[test]
    fn the_file_round_trips_in_pieces_of_any_size() {
        let mut v = Visits::<64>::new();
        for i in 0..300u32 {
            v.see_id(fingerprint(1, &i.to_le_bytes()), DAY - (i % 20) as u16);
        }
        for i in 0..40 {
            v.see_spot((1000 + i, -3000 - i), DAY - (i % 3) as u16);
        }
        v.see_id(fingerprint(1, b"SN-1"), DAY - 3);
        v.see_id(fingerprint(1, b"SN-1"), DAY);
        let f = file(&v);
        assert_eq!(f.len(), 16 + 16 * (256 + 40) + 4, "a full table: about 4.8 KB");
        assert!(f.len() > 512, "spans several pieces");
        for piece in [1, 7, 16, 512, 10_000] {
            let (w, r) = load::<64>(&f, piece, Some(DAY));
            assert_eq!(r, Ok(Loaded { ids: 256, spots: 40 }), "piece {piece}");
            assert_eq!(w.counts(), v.counts());
            assert_eq!(w.id_days(fingerprint(1, b"SN-1"), DAY), 2);
            assert_eq!(w.spot_days((1000, -3000), DAY), v.spot_days((1000, -3000), DAY));
            assert!(!w.dirty(), "just read: nothing new to write");
        }
        // Read into a smaller table: it keeps what fits.
        let (w, r) = load::<8>(&f, 512, None);
        assert!(r.is_ok());
        assert_eq!(w.counts(), (32, 16));
        // An empty table is a header and a checksum.
        let e = file(&Visits::<4>::new());
        assert_eq!(e.len(), 20);
        assert_eq!(load::<4>(&e, 512, None).1, Ok(Loaded { ids: 0, spots: 0 }));
        // Read with today far on: everything is too old and dropped.
        let (w, _) = load::<64>(&f, 512, Some(DAY + 60));
        assert_eq!(w.counts(), (0, 0));
    }

    #[test]
    fn a_damaged_or_foreign_file_is_discarded_whole() {
        let mut v = Visits::<8>::new();
        for i in 0..20u32 {
            v.see_id(fingerprint(1, &i.to_le_bytes()), DAY);
        }
        let good = file(&v);
        // Something seen since boot, before the card was read.
        let since_boot = |w: &mut Visits<8>| w.see_id(fingerprint(1, b"TODAY"), DAY);
        let mut bad = Vec::new();
        let mut flip = good.clone();
        flip[100] ^= 0x40;
        bad.push((flip, LoadError::Checksum));
        let mut cut = good.clone();
        cut.truncate(good.len() - 9);
        bad.push((cut, LoadError::Short));
        let mut long = good.clone();
        long.push(0);
        bad.push((long, LoadError::Long));
        let mut old = good.clone();
        old[4] = 0;
        bad.push((old, LoadError::Version(0)));
        let mut newer = good.clone();
        newer[4] = 9;
        bad.push((newer, LoadError::Version(9)));
        bad.push((b"node 02:00:00:00:00:18 39.95 -75.16\n".to_vec(), LoadError::NotOurs));
        bad.push((Vec::new(), LoadError::Short));
        bad.push((b"RSV".to_vec(), LoadError::Short));
        // Counts claiming far more than is there.
        let mut huge = good[..16].to_vec();
        huge[6] = 0xff;
        huge[7] = 0xff;
        bad.push((huge, LoadError::Short));
        for (bytes, why) in bad {
            for piece in [1, 512] {
                let mut w = Visits::<8>::new();
                since_boot(&mut w);
                let mut l = Loader::new();
                for c in bytes.chunks(piece) {
                    l.feed(&mut w, c);
                }
                assert_eq!(l.finish(&mut w, Some(DAY)), Err(why), "{bytes:?}");
                assert_eq!(w.counts(), (0, 0), "nothing of a bad file is kept");
                assert!(w.dirty(), "so the bad file gets replaced");
            }
        }
        // Random bytes never panic.
        let mut seed = 1u32;
        for _ in 0..200 {
            let n = (seed % 700) as usize;
            let mut junk = Vec::new();
            for _ in 0..n {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                junk.push((seed >> 16) as u8);
            }
            if n > 6 && seed % 2 == 0 {
                junk[..4].copy_from_slice(&MAGIC);
                junk[4] = VERSION;
            }
            let _ = load::<8>(&junk, 13, Some(DAY));
        }
    }
}
