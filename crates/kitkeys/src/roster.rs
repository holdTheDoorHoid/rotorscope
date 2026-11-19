//! The kit roster: who is in which slot (0..15) at which generation, and
//! whether the slot is revoked. Holds no secrets: an entry grants nothing
//! without the matching node key. The computer keeps it in
//! `tools/kit.roster`; the hub gets a copy in `rskit` ([`crate::hub`]).
//!
//! Rules:
//! - a new node takes the lowest free slot at generation 1;
//! - provisioning an occupied slot again (replacement board, suspected
//!   leak) bumps its generation, so the new key is unrelated to the old one,
//!   and clears `revoked`;
//! - one MAC is in at most one live (not revoked) slot;
//! - revoking keeps the entry (and its generation), so the slot never goes
//!   back to an old generation.

use crate::{KeyCheck, KeySource, Mac, NodeIdent, SLOTS};

/// One slot's line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub slot: u8,
    pub generation: u16,
    pub mac: Mac,
    pub key_check: KeyCheck,
    pub revoked: bool,
}

impl Entry {
    pub fn ident(&self) -> NodeIdent {
        NodeIdent { slot: self.slot, generation: self.generation, mac: self.mac }
    }
}

/// Up to 16 entries, indexed by slot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roster {
    slots: [Option<Entry>; SLOTS],
}

/// Why a roster change was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterError {
    /// Slot number 16 or more.
    NoSuchSlot(u8),
    /// Every slot is taken (revoke one and re-provision it with `--slot`).
    Full,
    /// This MAC already holds a live slot; re-key that slot instead.
    MacInUse { slot: u8 },
    /// The slot has never been provisioned.
    Empty(u8),
    /// The generation counter of this slot is used up.
    GenerationExhausted(u8),
    /// An entry whose `slot` field disagrees with its position, or a
    /// duplicate (only from [`Roster::insert`]).
    Inconsistent(u8),
}

impl core::fmt::Display for RosterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RosterError::NoSuchSlot(s) => write!(f, "slot {s} does not exist (slots are 0 to 15)"),
            RosterError::Full => f.write_str("all 16 slots are taken; revoke one and re-provision it with --slot"),
            RosterError::MacInUse { slot } => {
                write!(f, "this board is already in slot {slot}; to give it a new key use --slot {slot}")
            }
            RosterError::Empty(s) => write!(f, "slot {s} is empty"),
            RosterError::GenerationExhausted(s) => write!(f, "slot {s} has used every generation; use another slot"),
            RosterError::Inconsistent(s) => write!(f, "roster entry for slot {s} is inconsistent"),
        }
    }
}

impl Roster {
    pub const fn new() -> Self {
        Roster { slots: [None; SLOTS] }
    }

    pub fn get(&self, slot: u8) -> Option<&Entry> {
        self.slots.get(usize::from(slot))?.as_ref()
    }

    /// Entries in slot order.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.slots.iter().flatten()
    }

    pub fn len(&self) -> usize {
        self.entries().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The lowest slot never provisioned.
    pub fn next_free_slot(&self) -> Option<u8> {
        self.slots.iter().position(Option::is_none).map(|i| i as u8)
    }

    /// The live (not revoked) slot holding `mac`.
    pub fn live_slot_of(&self, mac: &Mac) -> Option<u8> {
        self.entries().find(|e| !e.revoked && e.mac == *mac).map(|e| e.slot)
    }

    /// Put an entry read back from a file or record. Refuses a duplicate slot.
    pub fn insert(&mut self, e: Entry) -> Result<(), RosterError> {
        let i = usize::from(e.slot);
        let cell = self.slots.get_mut(i).ok_or(RosterError::NoSuchSlot(e.slot))?;
        if cell.is_some() {
            return Err(RosterError::Inconsistent(e.slot));
        }
        *cell = Some(e);
        Ok(())
    }

    /// Choose the identity for provisioning `mac` (`slot`: re-key that slot
    /// at the next generation; `None`: the next free slot at generation 1).
    /// Changes nothing; [`Roster::provision`] does.
    pub fn plan(&self, slot: Option<u8>, mac: Mac) -> Result<NodeIdent, RosterError> {
        if let Some(s) = self.live_slot_of(&mac)
            && slot != Some(s)
        {
            return Err(RosterError::MacInUse { slot: s });
        }
        match slot {
            None => {
                let s = self.next_free_slot().ok_or(RosterError::Full)?;
                Ok(NodeIdent { slot: s, generation: 1, mac })
            }
            Some(s) => {
                if usize::from(s) >= SLOTS {
                    return Err(RosterError::NoSuchSlot(s));
                }
                let generation = match self.get(s) {
                    None => 1,
                    Some(e) => e.generation.checked_add(1).ok_or(RosterError::GenerationExhausted(s))?,
                };
                Ok(NodeIdent { slot: s, generation, mac })
            }
        }
    }

    /// Provision: plan, derive the key's check value, record the entry.
    /// Returns the identity to derive the node key for.
    pub fn provision<S: KeySource>(
        &mut self,
        src: &mut S,
        slot: Option<u8>,
        mac: Mac,
    ) -> Result<Result<NodeIdent, RosterError>, S::Error> {
        let ident = match self.plan(slot, mac) {
            Ok(i) => i,
            Err(e) => return Ok(Err(e)),
        };
        let key_check = src.derive(&ident)?.check();
        self.slots[usize::from(ident.slot)] =
            Some(Entry { slot: ident.slot, generation: ident.generation, mac, key_check, revoked: false });
        Ok(Ok(ident))
    }

    /// Revoke a slot: the hub stops accepting it (after `hub-roster`).
    pub fn revoke(&mut self, slot: u8) -> Result<(), RosterError> {
        let cell = self.slots.get_mut(usize::from(slot)).ok_or(RosterError::NoSuchSlot(slot))?;
        match cell {
            None => Err(RosterError::Empty(slot)),
            Some(e) => {
                e.revoked = true;
                Ok(())
            }
        }
    }
}
