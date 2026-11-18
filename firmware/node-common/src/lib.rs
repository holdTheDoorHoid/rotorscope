//! Which images this board holds, and switching to another one.
//!
//! Partition layout (`tools/partitions-node.csv`): app slots `ota_0`..`ota_2`
//! (node-wifi, node-ble, node-sdr by convention, but each slot's image is
//! recognised by the project name in its app descriptor, not by its slot),
//! `otadata` (which slot the ESP-IDF bootloader starts) and `rsjob`, one 4 KB
//! sector holding the assignment a node switched images for.
//!
//! The switch: write the job to `rsjob`, point `otadata` at the target slot
//! (`esp-bootloader-esp-idf`'s OTA support), software reset. The bootloader
//! starts the other image, which reads `rsjob` and starts in the assigned
//! role with the assigned plan, reporting the plan id the hub sent. A board
//! flashed with a single image (no `otadata`) still works: it reports only
//! its own roles and refuses to switch.
//!
//! Flash wear: `rsjob` is erased once per image switch; the hub's coordinator
//! changes a node's role at most once a minute.
#![no_std]

#[cfg(feature = "wirebus")]
pub mod bus;
pub mod fault;

use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::{
    self, AppPartitionSubType, DataPartitionSubType, FlashStorage, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use link::{Assignment, NodeRole};
use nodeimage::{HEAD_LEN, Image, JOB_LABEL, JOB_MAX};

/// One app slot.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    pub app: AppPartitionSubType,
    pub offset: u32,
    /// What is in it; `None` for an empty (erased) or unknown slot.
    pub image: Option<Image>,
    pub booted: bool,
}

/// Why a switch did not happen. The node keeps running as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchError {
    /// No slot holds an image that can take this role.
    NoImage,
    /// The table has no `otadata` / fewer than two OTA slots (single-image flash).
    NoOta,
    /// The `rsjob` partition is missing or the job did not encode.
    NoJobSector,
    /// A flash operation failed.
    Flash,
}

impl SwitchError {
    pub const fn as_str(self) -> &'static str {
        match self {
            SwitchError::NoImage => "no image for role",
            SwitchError::NoOta => "no OTA slots",
            SwitchError::NoJobSector => "no rsjob sector",
            SwitchError::Flash => "flash error",
        }
    }
}

pub struct Images<'d> {
    flash: FlashStorage<'d>,
    pub slots: heapless::Vec<Slot, 8>,
    job_offset: Option<u32>,
    /// Roles this board can take: the running image's own roles plus those
    /// of every other image it holds. Report as `Frame::Status::can`.
    pub can: u8,
    /// The partition table has OTA data and at least two app slots.
    pub ota: bool,
}

impl<'d> Images<'d> {
    /// Read the partition table and the head of every app slot. `own_roles`
    /// is what the running image itself can do (it knows better than the
    /// table: an S3 node-wifi has no 5 GHz).
    pub fn scan(flash: esp_hal::peripherals::FLASH<'d>, own_roles: u8) -> Self {
        let mut flash = FlashStorage::new(flash);
        let mut slots = heapless::Vec::new();
        let mut job_offset = None;
        let mut has_otadata = false;
        let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
        let mut found: heapless::Vec<(AppPartitionSubType, u32, bool), 8> = heapless::Vec::new();
        if let Ok(pt) = partitions::read_partition_table(&mut flash, &mut buf) {
            let booted = pt.booted_partition().ok().flatten().map(|p| p.offset());
            for p in pt.iter() {
                // `partition_type` panics on subtypes it does not know; only
                // look at the raw types we care about.
                match (p.raw_type(), p.raw_subtype()) {
                    (0, _) => {
                        if let PartitionType::App(app) = p.partition_type() {
                            let _ = found.push((app, p.offset(), booted == Some(p.offset())));
                        }
                    }
                    (1, 0) => has_otadata = true,
                    (1, s) if s == DataPartitionSubType::Undefined as u8 && p.label_as_str() == JOB_LABEL => {
                        job_offset = Some(p.offset());
                    }
                    _ => {}
                }
            }
        }
        let mut can = own_roles;
        let ota_slots = found.iter().filter(|s| !matches!(s.0, AppPartitionSubType::Factory | AppPartitionSubType::Test)).count();
        for (app, offset, booted) in found {
            let mut head = [0u8; HEAD_LEN];
            let image = flash_read(&mut flash, offset, &mut head).ok().and_then(|()| nodeimage::image_of(&head));
            if !booted {
                can |= image.map_or(0, Image::roles);
            }
            let _ = slots.push(Slot { app, offset, image, booted });
        }
        Self { flash, slots, job_offset, can, ota: has_otadata && ota_slots >= 2 }
    }

    /// The job this image was switched in for, if any (left in place: after a
    /// power cycle the same image starts with the same job; a newer command
    /// from the hub replaces it in memory).
    pub fn stored_job(&mut self) -> Option<Assignment> {
        let at = self.job_offset?;
        let mut raw = [0u8; JOB_MAX];
        flash_read(&mut self.flash, at, &mut raw).ok()?;
        nodeimage::decode_job(&raw)
    }

    /// The slot to start for `role`, if this board holds an image for it.
    pub fn slot_for(&self, role: NodeRole) -> Option<Slot> {
        self.slots
            .iter()
            .copied()
            .find(|s| !s.booted && s.image.is_some_and(|i| i.roles() & role.bit() != 0))
    }

    /// Store `job`, make its image the boot image and reset. Returns only on
    /// failure, before anything that would change what boots next.
    pub fn switch(&mut self, job: &Assignment) -> SwitchError {
        let Some(slot) = self.slot_for(job.role) else { return SwitchError::NoImage };
        if !self.ota {
            return SwitchError::NoOta;
        }
        let Some(at) = self.job_offset else { return SwitchError::NoJobSector };
        let mut rec = [0u8; JOB_MAX];
        let Some(raw) = nodeimage::encode_job(job, &mut rec) else { return SwitchError::NoJobSector };
        // esp-storage's `write` erases the sector and read-modify-writes it.
        if self.flash.write(at, raw).is_err() {
            return SwitchError::Flash;
        }
        let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
        let selected = OtaUpdater::new(&mut self.flash, &mut buf)
            .and_then(|mut u| u.ota_data().and_then(|mut d| d.set_current_app_partition(slot.app)));
        if selected.is_err() {
            return SwitchError::Flash;
        }
        esp_hal::system::software_reset()
    }
}

fn flash_read(flash: &mut FlashStorage<'_>, at: u32, buf: &mut [u8]) -> Result<(), ()> {
    flash.read(at, buf).map_err(|_| ())
}

/// Bench only: the `can` mask a `ROTORSCOPE_BENCH_CAN` build reports, so
/// that the hub's coordinator can be steered to one role with a single node.
/// Pass `option_env!("ROTORSCOPE_BENCH_CAN")` (evaluated in the image's own
/// crate): unset = every role the board has (`0xff`, no restriction); `ble`,
/// `wifi5` or `wifi24` = only that role. Any other value stops the build.
/// Every image on one board must be built with the same value, or the board
/// flips between them. Never for a deployed node.
pub const fn bench_can_mask(v: Option<&str>) -> u8 {
    const fn eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }
    match v {
        None => 0xff,
        Some(s) if eq(s.as_bytes(), b"ble") => link::NodeRole::Ble.bit(),
        Some(s) if eq(s.as_bytes(), b"wifi5") => link::NodeRole::Wifi5.bit(),
        Some(s) if eq(s.as_bytes(), b"wifi24") => link::NodeRole::Wifi24.bit(),
        Some(_) => panic!("ROTORSCOPE_BENCH_CAN must be ble, wifi5 or wifi24"),
    }
}
