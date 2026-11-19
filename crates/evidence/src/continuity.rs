//! Continuity across boots (WP-14a, HB-3): a boot counter, and how far the
//! previous boot's chain had reached durable storage, written into the log
//! of the next boot. Given several boots' logs, the checker can then show
//! they are consecutive, and that the previous boot's log was not cut short
//! after the fact.
//!
//! **Where the counter lives** is behind [`BootCounter`], so a monotonic
//! counter in a secure element (planned; the chip is not chosen) can replace
//! the SD card state file without a format change: the `continuity` event
//! ([`Continuity`], event code 27) names its source.
//!
//! **The SD card state file** ([`FileCounter`]): two small files in the
//! evidence folder (`CHAINA.STA`, `CHAINB.STA`), written in turn, each with
//! a checksum, so a power cut while one is written leaves the other. At the
//! first card mount of a boot the newest valid one is read, the counter
//! advanced and saved *before* anything else, and the event logged. After
//! each flush of the evidence file (at most every [`PROGRESS_EVERY_MS`]) the
//! state is rewritten with this boot's durable record count and head.
//!
//! **What this cannot do.** The state file is on the card: whoever holds the
//! card can rewrite it, delete it (the counter starts again at 1, and the
//! next boot says so: [`FRESH`]), or keep a copy. So it proves the order of
//! the boots that were supplied, and that an older boot's log was not cut
//! *after a later boot recorded its head*; it cannot show that boots after
//! the newest one supplied are missing (the newest files deleted, a wiped
//! card), nor protect the newest boot's tail before the next boot starts. A
//! secure-element counter that cannot go back, read by every boot, would
//! close the first two gaps for a checker who also knows the counter's
//! current value (read off the unit).

/// "RSCS".
pub const MAGIC: [u8; 4] = *b"RSCS";
pub const VERSION: u8 = 1;
/// State file length.
pub const STATE_LEN: usize = 96;
/// The two state files, written in turn.
pub const SLOT_NAMES: [&str; 2] = ["CHAINA.STA", "CHAINB.STA"];
/// Rewrite the state at most this often (after an evidence flush).
pub const PROGRESS_EVERY_MS: u64 = 10_000;

/// `continuity` event status: the previous boot's state was found, and the
/// counter advanced from it.
pub const CONTINUED: u8 = 0;
/// No state in the store (a new or wiped card, or the state files were
/// removed): the counter starts at 1, nothing links to earlier boots.
pub const FRESH: u8 = 1;
/// State present but unreadable (corrupt in both slots): the counter
/// starts at 1.
pub const UNREADABLE: u8 = 2;
/// The new counter could not be saved: the next boot will not see it.
pub const NOT_SAVED: u8 = 3;
/// The store could not be read at all: counter unknown (0).
pub const STORE_ERROR: u8 = 4;
/// The counter comes from the unit itself (`format::boot::COUNTER_HUB_FLASH`),
/// and the card's state names a boot that is not the one just before this
/// one: boots in between ran with another card or none, or the card was in
/// another unit. The `prev_*` fields name the boot the card last held
/// (not checked as this boot's predecessor).
pub const OTHER_CARD: u8 = 5;

/// What a store holds about the latest boot that used it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    pub counter: u64,
    pub boot_id: [u8; 8],
    /// Records of that boot's chain known durably stored (its next
    /// sequence number), and the hash of the last of them (zero if none).
    pub records: u64,
    pub head: [u8; 32],
}

impl State {
    pub fn encode(&self) -> [u8; STATE_LEN] {
        let mut b = [0u8; STATE_LEN];
        b[..4].copy_from_slice(&MAGIC);
        b[4] = VERSION;
        b[8..16].copy_from_slice(&self.counter.to_le_bytes());
        b[16..24].copy_from_slice(&self.boot_id);
        b[24..32].copy_from_slice(&self.records.to_le_bytes());
        b[32..64].copy_from_slice(&self.head);
        let sum = crate::sha256(&b[..64]);
        b[64..].copy_from_slice(&sum);
        b
    }

    /// `None` unless it is a whole, unchanged state of this version (the
    /// checksum catches a torn write, not a deliberate edit).
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != STATE_LEN || b[..4] != MAGIC || b[4] != VERSION || crate::sha256(&b[..64])[..] != b[64..] {
            return None;
        }
        Some(Self {
            counter: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            boot_id: b[16..24].try_into().unwrap(),
            records: u64::from_le_bytes(b[24..32].try_into().unwrap()),
            head: b[32..64].try_into().unwrap(),
        })
    }
}

/// The `continuity` event's data (event code 27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Continuity {
    /// `format::boot::COUNTER_*`.
    pub source: u8,
    /// [`CONTINUED`], [`FRESH`], [`UNREADABLE`], [`NOT_SAVED`], [`STORE_ERROR`].
    pub status: u8,
    /// This boot's number (1 = the first the store knows of; 0 unknown).
    pub counter: u64,
    /// The boot before this one, as the store last heard of it (zero when
    /// none): its boot id, its records known durably stored, the hash of
    /// the last of them.
    pub prev_boot: [u8; 8],
    pub prev_records: u64,
    pub prev_head: [u8; 32],
}

impl Continuity {
    /// source (1), status (1), counter (u64), previous boot id (8),
    /// previous boot's records (u64), hash of its last record (32).
    pub const LEN: usize = 1 + 1 + 8 + 8 + 8 + 32;

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0] = self.source;
        b[1] = self.status;
        b[2..10].copy_from_slice(&self.counter.to_le_bytes());
        b[10..18].copy_from_slice(&self.prev_boot);
        b[18..26].copy_from_slice(&self.prev_records.to_le_bytes());
        b[26..58].copy_from_slice(&self.prev_head);
        b
    }

    /// Reads the fields it knows; longer data (a later firmware) is fine.
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::LEN {
            return None;
        }
        Some(Self {
            source: b[0],
            status: b[1],
            counter: u64::from_le_bytes(b[2..10].try_into().unwrap()),
            prev_boot: b[10..18].try_into().unwrap(),
            prev_records: u64::from_le_bytes(b[18..26].try_into().unwrap()),
            prev_head: b[26..58].try_into().unwrap(),
        })
    }

    /// Whether it names a previous boot to check against.
    pub fn has_prev(&self) -> bool {
        self.status == CONTINUED || self.status == NOT_SAVED && self.prev_boot != [0; 8]
    }

    pub fn status_name(status: u8) -> &'static str {
        match status {
            CONTINUED => "continued",
            FRESH => "fresh: no counter in the store",
            UNREADABLE => "the stored counter was unreadable",
            NOT_SAVED => "the new counter could not be saved",
            STORE_ERROR => "the counter store could not be read",
            OTHER_CARD => "the card was last used by a boot other than the one before this",
            _ => "unknown status",
        }
    }
}

/// Where a boot counter lives. Called by the hub only through this trait,
/// so a secure element can take the SD card's place.
pub trait BootCounter {
    type Error: core::fmt::Debug;
    /// `format::boot::COUNTER_*`.
    fn source(&self) -> u8;
    /// Once a boot, as soon as the store answers: read what the previous
    /// boot left, advance the counter for `boot_id` and save that before
    /// returning. `Ok(None)` when the store was already advanced for this
    /// boot (a card taken out and put back).
    fn begin(&mut self, boot_id: [u8; 8]) -> Result<Option<Continuity>, Self::Error>;
    /// This boot's chain has `records` records durably stored, the last of
    /// them hashing to `head`.
    fn progress(&mut self, records: u64, head: [u8; 32]) -> Result<(), Self::Error>;
}

/// Two numbered slots of [`STATE_LEN`] bytes (two files on the SD card).
pub trait Slots {
    type Error: core::fmt::Debug;
    /// The slot's bytes, or `Ok(None)` if it does not exist. Returns how
    /// many bytes it held (more than `buf` counts as unreadable).
    fn read(&mut self, slot: usize, buf: &mut [u8; STATE_LEN]) -> Result<Option<usize>, Self::Error>;
    fn write(&mut self, slot: usize, data: &[u8; STATE_LEN]) -> Result<(), Self::Error>;
}

/// The SD card state file logic, kept across mounts (the hub holds one per
/// boot); used through [`SlotCounter`].
#[derive(Debug, Clone, Default)]
pub struct FileCounter {
    /// This boot's state, once begun on the card now in use.
    cur: Option<State>,
    /// The slot the next write goes to.
    next_slot: usize,
}

impl FileCounter {
    pub const fn new() -> Self {
        Self { cur: None, next_slot: 0 }
    }

    /// This boot's counter on the current card, once begun.
    pub fn counter(&self) -> Option<u64> {
        self.cur.map(|s| s.counter)
    }

    /// See [`BootCounter::begin`].
    pub fn begin<S: Slots>(&mut self, slots: &mut S, boot_id: [u8; 8], source: u8) -> Result<Option<Continuity>, S::Error> {
        let mut found: [Option<State>; 2] = [None, None];
        let mut any = false;
        for (i, f) in found.iter_mut().enumerate() {
            let mut buf = [0u8; STATE_LEN];
            if let Some(n) = slots.read(i, &mut buf)? {
                any = true;
                if n == STATE_LEN {
                    *f = State::decode(&buf);
                }
            }
        }
        // The newest: higher counter, then more records.
        let newest = (0..2).filter_map(|i| found[i].map(|s| (i, s))).max_by_key(|(_, s)| (s.counter, s.records));
        if let Some((slot, s)) = newest
            && s.boot_id == boot_id
        {
            // This boot already began on this card (it was put back).
            self.cur = Some(s);
            self.next_slot = 1 - slot;
            return Ok(None);
        }
        let (status, prev) = match newest {
            Some((_, s)) => (CONTINUED, s),
            None if any => (UNREADABLE, State::default()),
            None => (FRESH, State::default()),
        };
        let counter = prev.counter.saturating_add(1);
        let me = State { counter, boot_id, records: 0, head: [0; 32] };
        // Write over the older slot (or the corrupt one), keep the newest.
        self.next_slot = newest.map_or(0, |(slot, _)| 1 - slot);
        let saved = self.write(slots, me);
        self.cur = Some(me);
        Ok(Some(Continuity {
            source,
            status: if saved.is_ok() { status } else { NOT_SAVED },
            counter,
            prev_boot: prev.boot_id,
            prev_records: prev.records,
            prev_head: prev.head,
        }))
    }

    /// See [`BootCounter::progress`]. Nothing to do before `begin`.
    pub fn progress<S: Slots>(&mut self, slots: &mut S, records: u64, head: [u8; 32]) -> Result<(), S::Error> {
        let Some(mut s) = self.cur else { return Ok(()) };
        s.records = records;
        s.head = head;
        self.write(slots, s)?;
        self.cur = Some(s);
        Ok(())
    }

    fn write<S: Slots>(&mut self, slots: &mut S, s: State) -> Result<(), S::Error> {
        slots.write(self.next_slot, &s.encode())?;
        self.next_slot = 1 - self.next_slot;
        Ok(())
    }
}

/// A [`FileCounter`] bound to the card it uses now: the [`BootCounter`].
pub struct SlotCounter<'a, S: Slots> {
    pub slots: &'a mut S,
    pub state: &'a mut FileCounter,
}

impl<S: Slots> BootCounter for SlotCounter<'_, S> {
    type Error = S::Error;
    fn source(&self) -> u8 {
        crate::format::boot::COUNTER_SD_FILE
    }
    fn begin(&mut self, boot_id: [u8; 8]) -> Result<Option<Continuity>, S::Error> {
        let source = self.source();
        self.state.begin(self.slots, boot_id, source)
    }
    fn progress(&mut self, records: u64, head: [u8; 32]) -> Result<(), S::Error> {
        self.state.progress(self.slots, records, head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two slots in memory; `fail_writes` makes every write fail; `tear`
    /// leaves the next write half done.
    #[derive(Default)]
    struct Mem {
        slot: [Option<std::vec::Vec<u8>>; 2],
        fail_writes: bool,
        tear: bool,
        writes: u32,
    }

    impl Slots for Mem {
        type Error = ();
        fn read(&mut self, slot: usize, buf: &mut [u8; STATE_LEN]) -> Result<Option<usize>, ()> {
            Ok(self.slot[slot].as_ref().map(|v| {
                let n = v.len().min(STATE_LEN);
                buf[..n].copy_from_slice(&v[..n]);
                v.len()
            }))
        }
        fn write(&mut self, slot: usize, data: &[u8; STATE_LEN]) -> Result<(), ()> {
            if self.fail_writes {
                return Err(());
            }
            self.writes += 1;
            self.slot[slot] = Some(if core::mem::take(&mut self.tear) { data[..40].to_vec() } else { data.to_vec() });
            Ok(())
        }
    }

    fn boot(m: &mut Mem, id: u8) -> (FileCounter, Option<Continuity>) {
        let mut f = FileCounter::new();
        let c = SlotCounter { slots: m, state: &mut f }.begin([id; 8]).unwrap();
        (f, c)
    }

    #[test]
    fn counts_boots_and_names_the_previous_head() {
        let mut m = Mem::default();
        let (mut f, c) = boot(&mut m, 1);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_boot, c.prev_records), (FRESH, 1, [0; 8], 0));
        assert_eq!(c.source, crate::format::boot::COUNTER_SD_FILE);
        SlotCounter { slots: &mut m, state: &mut f }.progress(10, [0xAA; 32]).unwrap();
        SlotCounter { slots: &mut m, state: &mut f }.progress(25, [0xBB; 32]).unwrap();
        let (_, c) = boot(&mut m, 2);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_boot, c.prev_records, c.prev_head), (CONTINUED, 2, [1; 8], 25, [0xBB; 32]));
        // Round trip through the event bytes.
        assert_eq!(Continuity::decode(&c.encode()), Some(c));
        let (_, c) = boot(&mut m, 3);
        assert_eq!(c.unwrap().counter, 3);
    }

    #[test]
    fn a_card_put_back_in_the_same_boot_is_not_a_new_boot() {
        let mut m = Mem::default();
        let (mut f, c) = boot(&mut m, 1);
        assert!(c.is_some());
        SlotCounter { slots: &mut m, state: &mut f }.progress(5, [1; 32]).unwrap();
        let c = SlotCounter { slots: &mut m, state: &mut f }.begin([1; 8]).unwrap();
        assert_eq!(c, None);
        assert_eq!(f.counter(), Some(1));
        // Progress carries on from where it was.
        SlotCounter { slots: &mut m, state: &mut f }.progress(9, [2; 32]).unwrap();
        let (_, c) = boot(&mut m, 2);
        assert_eq!(c.unwrap().prev_records, 9);
    }

    #[test]
    fn a_torn_write_leaves_the_other_slot() {
        let mut m = Mem::default();
        let (mut f, _) = boot(&mut m, 1);
        SlotCounter { slots: &mut m, state: &mut f }.progress(7, [7; 32]).unwrap();
        m.tear = true;
        SlotCounter { slots: &mut m, state: &mut f }.progress(8, [8; 32]).unwrap();
        // Power cut mid-write: the next boot still continues, from 7.
        let (_, c) = boot(&mut m, 2);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_records, c.prev_head), (CONTINUED, 2, 7, [7; 32]));
    }

    #[test]
    fn wiped_corrupt_and_unwritable_stores_say_so() {
        let mut m = Mem::default();
        boot(&mut m, 1);
        // Both slots garbled: counter restarts, and says why.
        m.slot = [Some(vec![0x55; STATE_LEN]), Some(vec![1, 2, 3])];
        let (_, c) = boot(&mut m, 2);
        assert_eq!(c.unwrap().status, UNREADABLE);
        // Removed: fresh.
        m.slot = [None, None];
        let (_, c) = boot(&mut m, 3);
        assert_eq!((c.unwrap().status, c.unwrap().counter), (FRESH, 1));
        // A card that refuses writes: the event still comes, flagged.
        m.fail_writes = true;
        let (_, c) = boot(&mut m, 4);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_boot), (NOT_SAVED, 2, [3; 8]));
    }

    #[test]
    fn writes_alternate_slots() {
        let mut m = Mem::default();
        let (mut f, _) = boot(&mut m, 1);
        for r in 1..=5 {
            SlotCounter { slots: &mut m, state: &mut f }.progress(r, [r as u8; 32]).unwrap();
            let a = m.slot[0].as_deref().and_then(State::decode).map(|s| s.records);
            let b = m.slot[1].as_deref().and_then(State::decode).map(|s| s.records);
            // The two slots hold the latest two states.
            assert_eq!(a.max(b), Some(r));
            assert_eq!(a.min(b), Some(r - 1));
        }
        assert_eq!(m.writes, 6);
    }
}
