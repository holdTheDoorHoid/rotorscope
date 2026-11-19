//! At boot: the hub's kit records (`rskit`) and boot counter (`rsboot`),
//! read once, and the per-slot keys derived once through a
//! [`kitkeys::KeySource`] (docs/LINK-KEYS-DESIGN.md 4.2, 12.3).
//!
//! Only the key source sees the master: [`read_kit`] hands the master record
//! back as a [`SoftKeySource`] (the master bytes are wiped as it is made),
//! and [`load_slots`] asks it for one node key per live roster slot and keeps
//! only the keyed HMAC states inside `airlink::v3::Hub`. A secure element
//! replaces [`SoftKeySource`] without touching anything here.
//!
//! The boot counter is appended once per boot ([`advance_boot_counter`]),
//! before the link starts: its value is this boot's epoch. A value whose
//! write could not be confirmed is never used: the next boot could hand it
//! out again, and an epoch must never repeat (a recording from the earlier
//! boot would then be current).

use airlink::v3::Hub;
use kitkeys::hub::{self, Master, MasterState, RosterState};
use kitkeys::roster::Roster;
use kitkeys::{KeySource, KitId, SLOTS, SoftKeySource, bootlog};

use crate::flash::{Flash, FlashError, Window};
use crate::parttable::{self, PartError};

/// What the kit records say, for the console and the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitState {
    /// Keyed: `slots` live roster entries loaded, `bad` refused (their key
    /// did not match the roster's check value).
    Keyed { kit_id: KitId, slots: u8, bad: u8 },
    /// Provisioned open on purpose: unauthenticated link.
    Open,
    /// No `rskit` partition in the table (flashed with an older table).
    NoPartition(PartError),
    /// Both master copies erased: never provisioned.
    Missing,
    /// No good master copy, or a master whose kit id does not add up.
    Damaged,
    /// A keyed master record with no master in it (secure element) on a hub
    /// that has no element yet.
    NoKeySource,
    /// Keyed master, but the roster record is damaged.
    RosterDamaged { kit_id: KitId },
    /// Keyed master, but the roster was made for another kit.
    OtherKit { kit_id: KitId, roster_kit: KitId },
}

impl KitState {
    pub fn name(&self) -> &'static str {
        match self {
            KitState::Keyed { .. } => "keyed",
            KitState::Open => "open",
            KitState::NoPartition(_) => "no_partition",
            KitState::Missing => "missing",
            KitState::Damaged => "damaged",
            KitState::NoKeySource => "no_key_source",
            KitState::RosterDamaged { .. } => "roster_damaged",
            KitState::OtherKit { .. } => "other_kit",
        }
    }

    /// The link may run keyed (it still needs a boot counter).
    pub fn keyed(&self) -> bool {
        matches!(self, KitState::Keyed { .. })
    }
}

/// The records as read, before any key is derived. Read once at boot;
/// no heap to box into.
#[allow(clippy::large_enum_variant)]
pub enum KitRead {
    Keyed { kit_id: KitId, source: SoftKeySource, roster: RosterRead },
    Open,
    Problem(KitState),
}

/// The roster record's outcome for a keyed master.
#[allow(clippy::large_enum_variant)]
pub enum RosterRead {
    Good(Roster),
    /// Erased: a keyed hub with no nodes yet.
    Empty,
    Damaged,
    OtherKit(KitId),
}

/// Read `rskit` through `flash`, given the partition table bytes. `buf`
/// takes one sector at a time (it held the master: it is wiped before
/// returning).
pub fn read_kit<F: Flash>(flash: &mut F, table: &[u8], buf: &mut [u8; hub::SECTOR_LEN]) -> Result<KitRead, FlashError> {
    let part = match parttable::data_partition(table, parttable::RSKIT, hub::PARTITION_LEN as u32) {
        Ok(p) => p,
        Err(e) => return Ok(KitRead::Problem(KitState::NoPartition(e))),
    };
    let mut w = Window::new(flash, part.offset, part.size)?;
    let r = (|| {
        w.read(0, buf)?;
        let master = hub::read_master(&buf[..]);
        let (kit_id, master) = match master {
            MasterState::Good(Master::Open) => return Ok(KitRead::Open),
            MasterState::Missing => return Ok(KitRead::Problem(KitState::Missing)),
            MasterState::Damaged => return Ok(KitRead::Problem(KitState::Damaged)),
            MasterState::Good(Master::Keyed { kit_id, master: None }) => {
                let _ = kit_id;
                return Ok(KitRead::Problem(KitState::NoKeySource));
            }
            MasterState::Good(Master::Keyed { kit_id, master: Some(m) }) => (kit_id, m),
        };
        let source = SoftKeySource::new(master);
        w.read(hub::ROSTER_SECTOR, buf)?;
        let roster = match hub::read_roster(&buf[..], kit_id) {
            RosterState::Good(r) => RosterRead::Good(r),
            RosterState::Missing => RosterRead::Empty,
            RosterState::Damaged => RosterRead::Damaged,
            RosterState::OtherKit(k) => RosterRead::OtherKit(k),
        };
        Ok(KitRead::Keyed { kit_id, source, roster })
    })();
    wipe(buf);
    r
}

fn wipe(b: &mut [u8]) {
    b.fill(0);
    core::hint::black_box(&*b);
}

/// Load every live roster slot into `hub`: derive its node key through the
/// key source, check it against the roster's `key_check`, keep only the
/// derived link keys. Returns the state to show.
pub fn load_slots<S: KeySource>(src: &mut S, kit_id: KitId, roster: &RosterRead, hub: &mut Hub) -> Result<KitState, S::Error> {
    let roster = match roster {
        RosterRead::Good(r) => r,
        RosterRead::Empty => return Ok(KitState::Keyed { kit_id, slots: 0, bad: 0 }),
        RosterRead::Damaged => return Ok(KitState::RosterDamaged { kit_id }),
        RosterRead::OtherKit(k) => return Ok(KitState::OtherKit { kit_id, roster_kit: *k }),
    };
    let (mut slots, mut bad) = (0u8, 0u8);
    for e in roster.entries().filter(|e| !e.revoked) {
        match kitkeys::derive_checked(src, &e.ident(), e.key_check)? {
            Ok(k) if usize::from(e.slot) < SLOTS && hub.set_slot(e.slot, e.mac, &k) => slots += 1,
            _ => bad += 1,
        }
    }
    Ok(if bad > 0 && slots == 0 { KitState::OtherKit { kit_id, roster_kit: kit_id } } else { KitState::Keyed { kit_id, slots, bad } })
}

/// What happened to the boot counter this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counter {
    /// `value` written at `offset` inside `rsboot` and read back: this
    /// boot's epoch. `erased`: a sector was erased first (the log wrapped).
    Written { value: u32, offset: usize, erased: bool },
    /// Writes are off in this build: the latest value, NOT usable as an
    /// epoch (it was some earlier boot's).
    ReadOnly { latest: u32 },
    NoPartition(PartError),
    /// Never seeded (`link-key hub` writes it).
    Missing,
    Damaged,
    /// The counter reached its end: provision the hub again.
    Exhausted,
    /// The write (or erase) failed, or did not read back: nothing usable.
    WriteFailed { value: u32, why: WriteFail },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteFail {
    Flash(FlashError),
    /// The place the log chose is not erased.
    NotErased,
    /// After the write, the log does not read as the new value.
    ReadBack,
    /// After erasing the other sector, the latest value was gone.
    LostLatest,
}

impl Counter {
    /// This boot's epoch, only when it is safely recorded.
    pub fn epoch(&self) -> Option<u32> {
        match *self {
            Counter::Written { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Counter::Written { .. } => "written",
            Counter::ReadOnly { .. } => "read_only",
            Counter::NoPartition(_) => "no_partition",
            Counter::Missing => "missing",
            Counter::Damaged => "damaged",
            Counter::Exhausted => "exhausted",
            Counter::WriteFailed { .. } => "write_failed",
        }
    }
}

/// Read `rsboot`, and (with `write`) append this boot's value and confirm
/// it by reading the whole log back. `buf` holds the partition.
pub fn advance_boot_counter<F: Flash>(
    flash: &mut F,
    table: &[u8],
    buf: &mut [u8; bootlog::PARTITION_LEN],
    write: bool,
) -> Result<Counter, FlashError> {
    let part = match parttable::data_partition(table, parttable::RSBOOT, bootlog::PARTITION_LEN as u32) {
        Ok(p) => p,
        Err(e) => return Ok(Counter::NoPartition(e)),
    };
    let mut w = Window::new(flash, part.offset, part.size)?;
    w.read(0, buf)?;
    let (latest, next) = match bootlog::read(&buf[..]) {
        bootlog::State::Good { latest, next } => (latest, next),
        bootlog::State::Missing => return Ok(Counter::Missing),
        bootlog::State::Damaged => return Ok(Counter::Damaged),
        bootlog::State::Exhausted { .. } => return Ok(Counter::Exhausted),
    };
    if !write {
        return Ok(Counter::ReadOnly { latest });
    }
    let value = next.value;
    let fail = |why| Ok(Counter::WriteFailed { value, why });
    if let Some(s) = next.erase_sector {
        if let Err(e) = w.erase_sector(s * bootlog::SECTOR_LEN) {
            return fail(WriteFail::Flash(e));
        }
        w.read(0, buf)?;
        if !matches!(bootlog::read(&buf[..]), bootlog::State::Good { latest: l, .. } if l == latest) {
            return fail(WriteFail::LostLatest);
        }
    }
    let at = next.offset;
    if buf[at..at + bootlog::ENTRY_LEN].iter().any(|&b| b != 0xFF) {
        return fail(WriteFail::NotErased);
    }
    if let Err(e) = w.write(at, &bootlog::entry(value)) {
        return fail(WriteFail::Flash(e));
    }
    w.read(0, buf)?;
    match bootlog::read(&buf[..]) {
        bootlog::State::Good { latest, .. } if latest == value => {
            Ok(Counter::Written { value, offset: at, erased: next.erase_sector.is_some() })
        }
        _ => fail(WriteFail::ReadBack),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flash::fake::MemFlash;
    use crate::parttable::tests::hub_table;
    use kitkeys::roster::Entry;
    use kitkeys::{KeyCheck, MasterKey, NodeIdent};

    pub const RSKIT_AT: u32 = 0xC10000;
    pub const RSBOOT_AT: u32 = 0xC12000;

    pub fn master(b: u8) -> MasterKey {
        MasterKey::from_bytes([b; 32]).unwrap()
    }

    /// A roster of `macs` under the test master `[b; 32]`.
    pub fn roster_for(b: u8, macs: &[[u8; 6]]) -> Roster {
        let mut src = SoftKeySource::new(master(b));
        let mut r = Roster::new();
        for mac in macs {
            r.provision(&mut src, None, *mac).unwrap().unwrap();
        }
        r
    }

    pub fn flash_with(rskit: &[u8], rsboot: &[u8]) -> MemFlash {
        let t = hub_table();
        MemFlash::with(&[(0x8000, &t), (RSKIT_AT, rskit), (RSBOOT_AT, rsboot)])
    }

    pub fn keyed_image(m: &MasterKey, roster: &Roster) -> std::vec::Vec<u8> {
        hub::kit_image(&hub::encode_master_keyed(Some(m), KitId::default()), &hub::encode_roster(m.kit_id(), roster)).0.to_vec()
    }

    #[test]
    fn keyed_kit_loads_its_slots_and_only_them() {
        let m = master(7);
        let macs = [[2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2], [2, 0, 0, 0, 0, 3]];
        let mut roster = roster_for(7, &macs);
        roster.revoke(1).unwrap();
        let mut f = flash_with(&keyed_image(&m, &roster), &bootlog::seed_image(100));
        let mut buf = [0u8; hub::SECTOR_LEN];
        let KitRead::Keyed { kit_id, mut source, roster: rr } = read_kit(&mut f, &hub_table(), &mut buf).unwrap() else { panic!() };
        assert_eq!(kit_id, m.kit_id());
        assert!(buf.iter().all(|&b| b == 0), "the buffer that held the master is wiped");
        let mut h = Hub::new(1);
        let st = load_slots(&mut source, kit_id, &rr, &mut h).unwrap();
        assert_eq!(st, KitState::Keyed { kit_id, slots: 2, bad: 0 });
        assert_eq!(h.slot_of(&macs[0]), Some(0));
        assert_eq!(h.slot_of(&macs[1]), None, "revoked");
        assert_eq!(h.slot_of(&macs[2]), Some(2));
    }

    #[test]
    fn open_missing_damaged_and_old_table() {
        let mut buf = [0u8; hub::SECTOR_LEN];
        let open = hub::kit_image(&hub::encode_master_open(), &hub::encode_roster(KitId::default(), &Roster::new())).0.to_vec();
        let mut f = flash_with(&open, &[0xFF; 0x2000]);
        assert!(matches!(read_kit(&mut f, &hub_table(), &mut buf).unwrap(), KitRead::Open));
        let mut f = flash_with(&[0xFF; 0x2000], &[0xFF; 0x2000]);
        assert!(matches!(read_kit(&mut f, &hub_table(), &mut buf).unwrap(), KitRead::Problem(KitState::Missing)));
        let m = master(9);
        let mut img = keyed_image(&m, &Roster::new());
        img[20] ^= 1;
        img[0x800 + 20] ^= 1;
        let mut f = flash_with(&img, &[0xFF; 0x2000]);
        assert!(matches!(read_kit(&mut f, &hub_table(), &mut buf).unwrap(), KitRead::Problem(KitState::Damaged)));
        // Element-backed master record, no element: refused.
        let el = hub::kit_image(&hub::encode_master_keyed(None, KitId([1, 2, 3, 4])), &hub::encode_roster(KitId([1, 2, 3, 4]), &Roster::new())).0.to_vec();
        let mut f = flash_with(&el, &[0xFF; 0x2000]);
        assert!(matches!(read_kit(&mut f, &hub_table(), &mut buf).unwrap(), KitRead::Problem(KitState::NoKeySource)));
        // Elecrow's table: no rskit, nothing read.
        let t = crate::parttable::tests::table(&[crate::parttable::tests::entry("factory", 0, 0, 0x10000, 0xC00000)]);
        let mut f = MemFlash::with(&[(0x8000, &t)]);
        assert!(matches!(read_kit(&mut f, &t, &mut buf).unwrap(), KitRead::Problem(KitState::NoPartition(PartError::Missing))));
    }

    #[test]
    fn roster_from_another_kit_or_damaged() {
        let m = master(7);
        let other = master(8);
        let roster = roster_for(8, &[[2, 0, 0, 0, 0, 1]]);
        let img = hub::kit_image(&hub::encode_master_keyed(Some(&m), KitId::default()), &hub::encode_roster(other.kit_id(), &roster)).0.to_vec();
        let mut f = flash_with(&img, &[0xFF; 0x2000]);
        let mut buf = [0u8; hub::SECTOR_LEN];
        let KitRead::Keyed { kit_id, mut source, roster: rr } = read_kit(&mut f, &hub_table(), &mut buf).unwrap() else { panic!() };
        let mut h = Hub::new(1);
        assert_eq!(load_slots(&mut source, kit_id, &rr, &mut h).unwrap(), KitState::OtherKit { kit_id, roster_kit: other.kit_id() });
        // A roster carrying this kit's id but check values from another
        // master (hand-made): every slot refused, shown as another kit's.
        let mut forged = Roster::new();
        forged.insert(Entry { slot: 0, generation: 1, mac: [2, 0, 0, 0, 0, 1], key_check: KeyCheck([1, 2, 3, 4]), revoked: false }).unwrap();
        let rr = RosterRead::Good(forged);
        let mut h = Hub::new(1);
        assert!(matches!(load_slots(&mut source, kit_id, &rr, &mut h).unwrap(), KitState::OtherKit { .. }));
        assert_eq!(h.slot_of(&[2, 0, 0, 0, 0, 1]), None);
        // Damaged roster copies.
        let mut img = keyed_image(&m, &roster_for(7, &[[2, 0, 0, 0, 0, 1]]));
        img[0x1000 + 30] ^= 1;
        img[0x1800 + 30] ^= 1;
        let mut f = flash_with(&img, &[0xFF; 0x2000]);
        let KitRead::Keyed { kit_id, mut source, roster: rr } = read_kit(&mut f, &hub_table(), &mut buf).unwrap() else { panic!() };
        assert_eq!(load_slots(&mut source, kit_id, &rr, &mut h).unwrap(), KitState::RosterDamaged { kit_id });
        let _ = NodeIdent { slot: 0, generation: 1, mac: [0; 6] };
    }

    #[test]
    fn boot_counter_appends_one_per_boot() {
        let mut f = flash_with(&[0xFF; 0x2000], &bootlog::seed_image(1000));
        let t = hub_table();
        let mut buf = [0u8; bootlog::PARTITION_LEN];
        for i in 1..=5u32 {
            let c = advance_boot_counter(&mut f, &t, &mut buf, true).unwrap();
            assert_eq!(c, Counter::Written { value: 1000 + i, offset: i as usize * 8, erased: false });
            assert_eq!(c.epoch(), Some(1000 + i));
        }
        // Every write landed inside rsboot, nothing else was touched.
        assert!(f.writes.iter().all(|&(a, n)| a >= RSBOOT_AT && a as usize + n <= RSBOOT_AT as usize + 0x2000));
        assert!(f.erases.is_empty());
        // Read-only builds report the latest and give no epoch.
        let c = advance_boot_counter(&mut f, &t, &mut buf, false).unwrap();
        assert_eq!(c, Counter::ReadOnly { latest: 1005 });
        assert_eq!(c.epoch(), None);
    }

    #[test]
    fn boot_counter_wraps_into_the_other_sector() {
        // Sector 0 full: the next boot erases sector 1 and writes there; the
        // latest value stays readable throughout.
        let mut img = [0xFFu8; bootlog::PARTITION_LEN];
        for i in 0..bootlog::ENTRIES_PER_SECTOR {
            img[i * 8..i * 8 + 8].copy_from_slice(&bootlog::entry(5000 + i as u32));
        }
        img[0x1000..0x1008].copy_from_slice(&[0x12; 8]); // junk in sector 1
        let mut f = flash_with(&[0xFF; 0x2000], &img);
        let mut buf = [0u8; bootlog::PARTITION_LEN];
        let c = advance_boot_counter(&mut f, &hub_table(), &mut buf, true).unwrap();
        assert_eq!(c, Counter::Written { value: 5000 + 512, offset: 0x1000, erased: true });
        assert_eq!(f.erases, [RSBOOT_AT + 0x1000]);
        // And the next boot carries on in sector 1.
        let c = advance_boot_counter(&mut f, &hub_table(), &mut buf, true).unwrap();
        assert_eq!(c.epoch(), Some(5000 + 513));
    }

    #[test]
    fn a_failed_or_unconfirmed_write_gives_no_epoch() {
        let t = hub_table();
        let mut buf = [0u8; bootlog::PARTITION_LEN];
        let mut f = flash_with(&[0xFF; 0x2000], &bootlog::seed_image(10));
        f.fail = Some(FlashError::Io(1));
        let c = advance_boot_counter(&mut f, &t, &mut buf, true).unwrap();
        assert_eq!(c, Counter::WriteFailed { value: 11, why: WriteFail::Flash(FlashError::Io(1)) });
        assert_eq!(c.epoch(), None);
        // A write that does not read back.
        let mut f = flash_with(&[0xFF; 0x2000], &bootlog::seed_image(10));
        f.garble_next = true;
        let c = advance_boot_counter(&mut f, &t, &mut buf, true).unwrap();
        assert_eq!(c, Counter::WriteFailed { value: 11, why: WriteFail::ReadBack });
        // A torn write: refused now, and the next boot skips the torn place
        // and gets a value no boot has used.
        let mut f = flash_with(&[0xFF; 0x2000], &bootlog::seed_image(10));
        f.tear_next = true;
        let c = advance_boot_counter(&mut f, &t, &mut buf, true).unwrap();
        assert_eq!(c.epoch(), None);
        let c = advance_boot_counter(&mut f, &t, &mut buf, true).unwrap();
        assert_eq!(c, Counter::Written { value: 11, offset: 16, erased: false });
        // 11 was never handed out before (the torn boot used no epoch).
    }

    #[test]
    fn boot_counter_states() {
        let t = hub_table();
        let mut buf = [0u8; bootlog::PARTITION_LEN];
        let mut f = flash_with(&[0xFF; 0x2000], &[0xFF; 0x2000]);
        assert_eq!(advance_boot_counter(&mut f, &t, &mut buf, true).unwrap(), Counter::Missing);
        let mut f = flash_with(&[0xFF; 0x2000], &[0x00; 0x2000]);
        assert_eq!(advance_boot_counter(&mut f, &t, &mut buf, true).unwrap(), Counter::Damaged);
        let mut f = flash_with(&[0xFF; 0x2000], &bootlog::seed_image(bootlog::MAX_VALUE));
        assert_eq!(advance_boot_counter(&mut f, &t, &mut buf, true).unwrap(), Counter::Exhausted);
        assert!(f.writes.is_empty() && f.erases.is_empty());
    }
}
