//! The keyed node link's state on the hub (docs/LINK-KEYS-DESIGN.md, package
//! 13e): read once at boot, then kept in PSRAM. The logic is
//! `crates/hublink` (host-tested); this file places it and talks to flash.
//!
//! **At boot** ([`boot`], before the node link's UART interrupt exists):
//!
//! 1. the partition table (flash `0x8000`) names `rskit` and `rsboot`
//!    (`tools/partitions-hub.csv`); a hub flashed with Elecrow's own table
//!    has neither, and nothing is written;
//! 2. `rskit`: the master record goes straight into a `SoftKeySource` (the
//!    only thing that ever holds the master; a secure element replaces it);
//! 3. `rsboot`: the next value is appended and read back: this boot's link
//!    epoch and the evidence log's boot counter. A value that did not read
//!    back is never used;
//! 4. every live roster slot's keys are derived once, into
//!    `airlink::v3::Hub` inside [`hublink::link::HubLink`] in PSRAM; the key
//!    source is dropped (and wiped) when this returns.
//!
//! Outcome, shown on the console and the screen strip:
//!
//! | Records | Boot counter | Link |
//! |---|---|---|
//! | keyed, roster good | written | keyed |
//! | keyed | not written (missing, damaged, used up, write failed, `rskit-readonly`) | closed: `NO BOOT COUNTER` (an epoch must never repeat) |
//! | open | any | open, `OPEN LINK` |
//! | missing, no partition, master in an element the hub lacks | any | closed: `NO KIT KEYS` |
//! | damaged (master or roster) | any | closed: `KIT RECORDS DAMAGED` |
//! | roster from another kit (or every slot's check value wrong) | any | keyed with no slots: `ROSTER FROM OTHER KIT` |
//!
//! **Bench bring-up of the flash access** (nothing here has run on the chip):
//!
//! 1. `cargo fw --features rskit-readonly` (writes off): the boot line
//!    `linkauth: rskit ...` must show the kit id and slot count that
//!    `link-key show` prints, and `rsboot read-only latest=N` the value
//!    `link-key hub` seeded. The link stays closed (`NO BOOT COUNTER`).
//! 2. `cargo fw` (default): `rsboot written N at +0xOFF, read back` with N
//!    one higher at every power cycle; `espflash read-flash 0xC12000 0x2000`
//!    then shows those entries (check with `kitkeys::bootlog::read`).
//! 3. Only after both: provision with a full sector 0 (or let 512 boots
//!    pass) to see the erase path: `erased sector 1` once, link up.
//!
//! If the ROM write does not work on this chip (step 2 fails), the fall-back
//! is the card: the epoch from the SD card's counter, which is weaker (a
//! wiped or swapped card repeats an epoch: a recording from that earlier
//! boot becomes current again for up to about 2 s of node traffic, and old
//! beacons are taken tentatively); until the secure element brings its own
//! monotonic counter. It is not built: the keyed link stays closed instead.

use airlink::v3::Hub;
use hublink::kit::{self, Counter, KitRead, KitState, RosterRead};
use hublink::link::HubLink;
use hublink::parttable;
use kitkeys::{bootlog, hub as kithub};
use link::gw::{GwDecoder, MAX_GW_FRAME};

use crate::flashrom::RomFlash;

/// Bytes the node link's receive ring holds: about 0.7 s of the gateway
/// UART at its 921,600 baud line rate (main-loop passes reach 190 ms, 600
/// ms at the SD mount; spike S0).
pub const RING: usize = 64 * 1024;
pub type Ring = heapless::Deque<u8, RING>;
/// Frames for the tracker (`links::Out`), sized so a pass over a full ring
/// of small batches does not overflow it (spike S0: 8 entries overflowed).
pub const OUT: usize = 256;
pub type Out = heapless::Deque<(u8, link::Frame), OUT>;
/// Nodes' `NodeHealth` reports waiting for the tracker (latest per node
/// wins in practice: one every 5 s per node).
pub type HealthQueue = heapless::Deque<(u8, link::NodeHealth), 32>;

/// Where [`Store`] lives in PSRAM: past the airspace pack (8..16 MB).
pub const STORE_OFFSET: usize = 16 << 20;

/// Everything of the link too big for internal RAM.
pub struct Store {
    pub ring: Ring,
    pub dec: GwDecoder<[u8; MAX_GW_FRAME]>,
    pub out: Out,
    pub health: HealthQueue,
    pub link: HubLink,
    pub kit: KitState,
    pub counter: Counter,
    table: [u8; parttable::TABLE_LEN],
    scratch: [u8; bootlog::PARTITION_LEN],
}

const _: () = assert!(crate::faa::PACK_OFFSET + crate::faa::PACK_MAX <= STORE_OFFSET);

/// The parts, each its own reference (disjoint fields of one [`Store`]).
pub struct Parts {
    pub ring: &'static mut Ring,
    pub dec: &'static mut GwDecoder<[u8; MAX_GW_FRAME]>,
    pub out: &'static mut Out,
    pub health: &'static mut HealthQueue,
    pub link: &'static mut HubLink,
    pub kit: KitState,
    pub counter: Counter,
}

/// Writes to the hub's flash: off with `rskit-readonly` (bench bring-up).
const FLASH_WRITES: bool = !cfg!(feature = "rskit-readonly");

/// Read the kit records and boot counter, set up the link state in PSRAM.
/// Once, at boot, before the node link starts. Panics without PSRAM (the
/// hub cannot run without it: the tracker lives there too).
#[inline(never)]
pub fn boot(psram: Option<(*mut u32, usize)>) -> Parts {
    let Some((base, size)) = psram else { panic!("hub-p4: no PSRAM: the node link lives there; cannot start") };
    assert!(size >= STORE_OFFSET + core::mem::size_of::<Store>(), "PSRAM too small for the node link");
    // SAFETY: [STORE_OFFSET, + size_of::<Store>()) is inside PSRAM (checked
    // above) and used by nothing else (after the airspace pack, the assert
    // above); 16 MB is aligned for any type; every field is written before
    // any reference to it is made; this runs once.
    let p = unsafe { base.cast::<u8>().add(STORE_OFFSET).cast::<Store>() };
    unsafe {
        (&raw mut (*p).ring).write(heapless::Deque::new());
        (&raw mut (*p).dec).write(GwDecoder::new([0; MAX_GW_FRAME]));
        (&raw mut (*p).out).write(heapless::Deque::new());
        (&raw mut (*p).health).write(heapless::Deque::new());
        (&raw mut (*p).link).write(HubLink::closed());
        (&raw mut (*p).table).write([0xFF; parttable::TABLE_LEN]);
        (&raw mut (*p).scratch).write([0; bootlog::PARTITION_LEN]);
    }
    // SAFETY: as above; the fields are disjoint.
    let (link, table, scratch) = unsafe { (&mut *(&raw mut (*p).link), &mut *(&raw mut (*p).table), &mut *(&raw mut (*p).scratch)) };
    let (kit, counter) = load(link, table, scratch);
    scratch.fill(0); // it held the master
    core::hint::black_box(&*scratch);
    // SAFETY: as above.
    unsafe {
        (&raw mut (*p).kit).write(kit);
        (&raw mut (*p).counter).write(counter);
        Parts {
            ring: &mut *(&raw mut (*p).ring),
            dec: &mut *(&raw mut (*p).dec),
            out: &mut *(&raw mut (*p).out),
            health: &mut *(&raw mut (*p).health),
            link,
            kit,
            counter,
        }
    }
}

fn load(link: &mut HubLink, table: &mut [u8; parttable::TABLE_LEN], scratch: &mut [u8; bootlog::PARTITION_LEN]) -> (KitState, Counter) {
    let mut flash = RomFlash { writes: FLASH_WRITES };
    if let Err(e) = hublink::flash::Flash::read(&mut flash, parttable::TABLE_ADDR, table) {
        println!("hub-p4: linkauth: partition table unreadable ({e:?}): node link closed");
        return (KitState::NoPartition(parttable::PartError::NoTable), Counter::NoPartition(parttable::PartError::NoTable));
    }
    let counter = match kit::advance_boot_counter(&mut flash, table, scratch, FLASH_WRITES) {
        Ok(c) => c,
        Err(e) => Counter::WriteFailed { value: 0, why: kit::WriteFail::Flash(e) },
    };
    print_counter(&counter);
    let sector: &mut [u8; kithub::SECTOR_LEN] = (&mut scratch[..kithub::SECTOR_LEN]).try_into().expect("8 KB scratch");
    let read = match kit::read_kit(&mut flash, table, sector) {
        Ok(r) => r,
        Err(e) => {
            println!("hub-p4: linkauth: rskit unreadable ({e:?}): node link closed");
            return (KitState::Damaged, counter);
        }
    };
    let state = match read {
        KitRead::Open => {
            link.open();
            KitState::Open
        }
        KitRead::Problem(s) => s,
        KitRead::Keyed { kit_id, mut source, roster } => match counter.epoch() {
            None => KitState::Keyed { kit_id, slots: 0, bad: 0 },
            Some(epoch) => {
                let hub: &mut Hub = link.start_keyed(epoch);
                let Ok(s) = kit::load_slots(&mut source, kit_id, &roster, hub);
                if let RosterRead::Good(r) = &roster {
                    link.slots_loaded(r.entries().filter(|e| !e.revoked).map(|e| (e.slot, e.mac)));
                }
                s
            }
        },
    };
    print_kit(&state, link, &counter);
    (state, counter)
}

fn print_counter(c: &Counter) {
    match c {
        Counter::Written { value, offset, erased } => println!(
            "hub-p4: linkauth: rsboot written {value} at +{offset:#x}{}, read back: this boot's epoch",
            if *erased { " (erased the other sector first)" } else { "" }
        ),
        Counter::ReadOnly { latest } => {
            println!("hub-p4: linkauth: rsboot read-only latest={latest} (rskit-readonly build: no epoch, keyed link stays closed)")
        }
        Counter::NoPartition(e) => println!("hub-p4: linkauth: rsboot: {}", e.name()),
        Counter::WriteFailed { value, why } => {
            println!("hub-p4: linkauth: rsboot value {value} NOT recorded ({why:?}): not used as an epoch")
        }
        other => println!("hub-p4: linkauth: rsboot {}", other.name()),
    }
}

fn print_kit(s: &KitState, link: &HubLink, c: &Counter) {
    match s {
        KitState::Keyed { kit_id, slots, bad } if c.epoch().is_some() => {
            println!("hub-p4: linkauth: rskit keyed, kit {kit_id}, {slots} node slot(s) loaded{}", if *bad > 0 { ", some refused (key check mismatch)" } else { "" });
            for slot in 0..kitkeys::SLOTS as u8 {
                if let Some(m) = link.slot_mac(slot) {
                    println!(
                        "hub-p4: linkauth: slot {slot} = node {} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                        HubLink::node_number(slot), m[0], m[1], m[2], m[3], m[4], m[5]
                    );
                }
            }
        }
        KitState::Keyed { kit_id, .. } => {
            println!("hub-p4: linkauth: rskit keyed, kit {kit_id}, but NO BOOT COUNTER this boot: keyed link CLOSED (an epoch must never repeat)")
        }
        KitState::Open => println!("hub-p4: linkauth: rskit OPEN: the ESP-NOW link is unauthenticated (provisioned open)"),
        KitState::NoPartition(e) => println!("hub-p4: linkauth: rskit: {}: node link CLOSED", e.name()),
        other => println!("hub-p4: linkauth: rskit {}: node link CLOSED", other.name()),
    }
}

/// What the screen strip says about the kit (`None`: nothing wrong).
pub fn kit_problem(kit: &KitState, counter: &Counter) -> Option<screen::KitProblem> {
    use screen::KitProblem as P;
    match kit {
        KitState::Keyed { .. } if counter.epoch().is_none() => Some(P::BootCounter),
        KitState::Keyed { bad, .. } if *bad > 0 => Some(P::OtherKit),
        KitState::Keyed { .. } | KitState::Open => None,
        KitState::NoPartition(_) | KitState::Missing | KitState::NoKeySource => Some(P::Missing),
        KitState::Damaged | KitState::RosterDamaged { .. } => Some(P::Damaged),
        KitState::OtherKit { .. } => Some(P::OtherKit),
    }
}
