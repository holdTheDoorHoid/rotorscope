//! One boot counter on the hub (docs/LINK-KEYS-DESIGN.md 12, item 2): the
//! `rsboot` value that is this boot's link epoch is also the evidence log's
//! boot counter, through `evidence::continuity::BootCounter`.
//!
//! **What lives where.**
//!
//! | | where | survives a wiped or swapped card |
//! |---|---|---|
//! | the counter | the hub's flash (`rsboot`) | yes |
//! | the previous boot's id, durable record count and last record hash | the card's state files (`CHAINA.STA` / `CHAINB.STA`), as before | no: they describe that card's log |
//!
//! The state files keep their format; their `counter` field now holds the
//! flash value, so a card still says which boot last wrote to it.
//!
//! **What the `continuity` record says** (source
//! `format::boot::COUNTER_HUB_FLASH`, counter = this boot's flash value):
//!
//! | card's newest state | status | `prev_*` |
//! |---|---|---|
//! | counter one below ours | `CONTINUED` | that boot (checked by the verifier as before) |
//! | another counter (boots ran without this card, or it was in another unit) | `OTHER_CARD` | the boot the card last held |
//! | none (new or wiped card) | `FRESH` | zero; the verifier says the counter still places the boot |
//! | unreadable | `UNREADABLE` | zero |
//! | this very boot (the card was put back) | no record (`Ok(None)`), as before | |
//! | could not be saved | `NOT_SAVED` | as found |
//!
//! Without a flash counter (open kit not provisioned, `rsboot` missing or
//! not writable) it is exactly `evidence::continuity::FileCounter`: the
//! counter on the card, source `COUNTER_SD_FILE`.

use evidence::continuity::{
    BootCounter, CONTINUED, Continuity, FRESH, FileCounter, NOT_SAVED, OTHER_CARD, STATE_LEN, Slots, State,
    UNREADABLE,
};
use evidence::format::boot::{COUNTER_HUB_FLASH, COUNTER_SD_FILE};

/// The boot counter across card mounts (the hub holds one per boot).
#[derive(Debug, Clone, Default)]
pub struct HubCounter {
    /// This boot's `rsboot` value, when it was written and confirmed.
    flash: Option<u32>,
    /// The card side when there is a flash counter.
    cur: Option<State>,
    next_slot: usize,
    /// The whole thing when there is not.
    file: FileCounter,
}

impl HubCounter {
    pub const fn new() -> Self {
        Self { flash: None, cur: None, next_slot: 0, file: FileCounter::new() }
    }

    /// At boot, before any card mount: this boot's confirmed `rsboot` value.
    pub fn set_flash(&mut self, value: Option<u32>) {
        self.flash = value;
    }

    /// `format::boot::COUNTER_*` for the boot record.
    pub fn source(&self) -> u8 {
        if self.flash.is_some() { COUNTER_HUB_FLASH } else { COUNTER_SD_FILE }
    }

    /// This boot's counter, once known (the flash one at once).
    pub fn counter(&self) -> Option<u64> {
        match self.flash {
            Some(v) => Some(u64::from(v)),
            None => self.file.counter(),
        }
    }

    fn begin<S: Slots>(&mut self, slots: &mut S, boot_id: [u8; 8]) -> Result<Option<Continuity>, S::Error> {
        let Some(flash) = self.flash else { return self.file.begin(slots, boot_id, COUNTER_SD_FILE) };
        let counter = u64::from(flash);
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
        // Newest: the boot id we are matters first (a card put back), then
        // the higher counter, then more records.
        let newest = (0..2).filter_map(|i| found[i].map(|s| (i, s))).max_by_key(|(_, s)| (s.boot_id == boot_id, s.counter, s.records));
        if let Some((slot, s)) = newest
            && s.boot_id == boot_id
        {
            self.cur = Some(s);
            self.next_slot = 1 - slot;
            return Ok(None);
        }
        let (status, prev) = match newest {
            Some((_, s)) if s.counter.checked_add(1) == Some(counter) => (CONTINUED, s),
            Some((_, s)) => (OTHER_CARD, s),
            None if any => (UNREADABLE, State::default()),
            None => (FRESH, State::default()),
        };
        let me = State { counter, boot_id, records: 0, head: [0; 32] };
        self.next_slot = newest.map_or(0, |(slot, _)| 1 - slot);
        let saved = self.write(slots, me);
        self.cur = Some(me);
        Ok(Some(Continuity {
            source: COUNTER_HUB_FLASH,
            status: if saved.is_ok() { status } else { NOT_SAVED },
            counter,
            prev_boot: prev.boot_id,
            prev_records: prev.records,
            prev_head: prev.head,
        }))
    }

    fn progress<S: Slots>(&mut self, slots: &mut S, records: u64, head: [u8; 32]) -> Result<(), S::Error> {
        if self.flash.is_none() {
            return self.file.progress(slots, records, head);
        }
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

/// A [`HubCounter`] bound to the card in use now: the hub's
/// `evidence::continuity::BootCounter`.
pub struct CardCounter<'a, S: Slots> {
    pub slots: &'a mut S,
    pub state: &'a mut HubCounter,
}

impl<S: Slots> BootCounter for CardCounter<'_, S> {
    type Error = S::Error;
    fn source(&self) -> u8 {
        self.state.source()
    }
    fn begin(&mut self, boot_id: [u8; 8]) -> Result<Option<Continuity>, S::Error> {
        self.state.begin(self.slots, boot_id)
    }
    fn progress(&mut self, records: u64, head: [u8; 32]) -> Result<(), S::Error> {
        self.state.progress(self.slots, records, head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[derive(Default, Clone)]
    struct Card {
        slot: [Option<Vec<u8>>; 2],
        fail_writes: bool,
    }

    impl Slots for Card {
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
            self.slot[slot] = Some(data.to_vec());
            Ok(())
        }
    }

    /// One hub boot with flash counter `value`, card `card`: the record and
    /// the state (to report progress on).
    fn boot(card: &mut Card, value: Option<u32>, id: u8) -> (HubCounter, Option<Continuity>) {
        let mut h = HubCounter::new();
        h.set_flash(value);
        let c = CardCounter { slots: card, state: &mut h }.begin([id; 8]).unwrap();
        (h, c)
    }

    #[test]
    fn the_flash_counter_survives_a_wiped_card() {
        let mut card = Card::default();
        let (mut h, c) = boot(&mut card, Some(5000), 1);
        let c = c.unwrap();
        assert_eq!((c.source, c.status, c.counter), (COUNTER_HUB_FLASH, FRESH, 5000));
        CardCounter { slots: &mut card, state: &mut h }.progress(40, [0xAA; 32]).unwrap();
        // Next boot, same card: continued, naming the previous head.
        let (_, c) = boot(&mut card, Some(5001), 2);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_boot, c.prev_records, c.prev_head), (CONTINUED, 5001, [1; 8], 40, [0xAA; 32]));
        assert!(c.has_prev());
        // The card is wiped: the counter goes on (the SD-only counter would
        // have started again at 1).
        let mut fresh = Card::default();
        let (_, c) = boot(&mut fresh, Some(5002), 3);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_boot), (FRESH, 5002, [0; 8]));
        assert!(!c.has_prev());
    }

    #[test]
    fn a_card_that_missed_boots_or_came_from_elsewhere() {
        let mut card = Card::default();
        let (mut h, _) = boot(&mut card, Some(70), 1);
        CardCounter { slots: &mut card, state: &mut h }.progress(9, [9; 32]).unwrap();
        // Boots 71 and 72 ran with another card; this one is back at 73.
        let (_, c) = boot(&mut card, Some(73), 4);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_boot, c.prev_records), (OTHER_CARD, 73, [1; 8], 9));
        assert!(!c.has_prev(), "not checked as the predecessor");
        // A card from a unit whose counter is higher: also another card's.
        let mut other = Card::default();
        boot(&mut other, Some(900), 7);
        let (_, c) = boot(&mut other, Some(74), 5);
        assert_eq!(c.unwrap().status, OTHER_CARD);
    }

    #[test]
    fn a_card_put_back_and_a_card_that_refuses_writes() {
        let mut card = Card::default();
        let (mut h, c) = boot(&mut card, Some(10), 1);
        assert!(c.is_some());
        CardCounter { slots: &mut card, state: &mut h }.progress(3, [3; 32]).unwrap();
        // Same boot, card out and in again: no new record.
        assert_eq!(CardCounter { slots: &mut card, state: &mut h }.begin([1; 8]).unwrap(), None);
        CardCounter { slots: &mut card, state: &mut h }.progress(8, [8; 32]).unwrap();
        let (_, c) = boot(&mut card, Some(11), 2);
        assert_eq!(c.unwrap().prev_records, 8);
        let mut ro = card.clone();
        ro.fail_writes = true;
        let (_, c) = boot(&mut ro, Some(12), 3);
        let c = c.unwrap();
        assert_eq!((c.status, c.counter, c.prev_boot), (NOT_SAVED, 12, [2; 8]));
        // Garbled: unreadable, the counter still the flash one.
        let mut bad = Card { slot: [Some(std::vec![0x55; STATE_LEN]), None], fail_writes: false };
        let (_, c) = boot(&mut bad, Some(13), 4);
        assert_eq!((c.unwrap().status, c.unwrap().counter), (UNREADABLE, 13));
    }

    /// No flash counter: exactly the SD file counter of before.
    #[test]
    fn without_flash_it_is_the_card_counter() {
        let mut card = Card::default();
        let (h, c) = boot(&mut card, None, 1);
        let c = c.unwrap();
        assert_eq!((c.source, c.status, c.counter), (COUNTER_SD_FILE, FRESH, 1));
        assert_eq!(h.source(), COUNTER_SD_FILE);
        let (_, c) = boot(&mut card, None, 2);
        assert_eq!((c.unwrap().status, c.unwrap().counter), (CONTINUED, 2));
        assert_eq!(c.unwrap().source, COUNTER_SD_FILE);
    }
}
