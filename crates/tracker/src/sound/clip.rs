//! Audio clips: when the hub may save one, how long it is, and how much
//! audio a card may hold (PLAN.md Phase 15d; owner decisions 2026-11-18).
//! Everything here is pure so it is tested on a laptop; the P4 hub
//! (`firmware/hub-p4/src/ear.rs`, `sd.rs`) only carries it out.
//!
//! - **Off by default.** Clips need `audio clips on` in the site file or
//!   from the operator (`tracker::site::SiteState::audio_clips_on`).
//! - **Only what would alert.** A clip starts at the moment a sound contact
//!   raises its alert ([`SoundContact::alert_due`]: a drone-like kind, never
//!   "Aircraft heard", heard for [`ANNOUNCE_AFTER_MS`]) and reaches back to
//!   [`PRE_ROLL_MS`] before the contact began, so the onset is kept.
//! - **Capped.** [`CLIPS_PER_HOUR`] clips in any hour, [`AUDIO_MAX_BYTES`]
//!   of audio on the card, and never into the last [`RESERVE_BYTES`] of free
//!   space, which belong to the evidence log. What is refused is counted
//!   ([`Refused`]).

use heapless::Vec;

use super::{ANNOUNCE_AFTER_MS, SoundContact};

/// Samples a second: the hub's microphone, 16 kHz, one channel.
pub const SAMPLES_PER_S: u64 = 16_000;
/// Bytes of sound a second (16-bit samples).
pub const BYTES_PER_S: u64 = 2 * SAMPLES_PER_S;
/// A WAV file's header (`acoustic::wav::HEADER_LEN`).
pub const WAV_HEADER: u64 = 44;

/// Sound kept from before the contact began (owner: a pre-roll is
/// acceptable when clips are on).
pub const PRE_ROLL_MS: u64 = 5_000;
/// Sound kept after the contact stops being heard.
pub const POST_ROLL_MS: u64 = 2_000;
/// Longest clip, pre-roll included.
pub const CLIP_MAX_MS: u64 = 60_000;
/// How far before the alert a clip may begin: the [`ANNOUNCE_AFTER_MS`] the
/// contact had to last, the one-second report it began in, and the
/// pre-roll, plus a second of slack. A contact that alerts late (it was an
/// aircraft first, then a drone was heard on the radio) does not get its
/// whole earlier stretch saved.
pub const REACH_BACK_MAX_MS: u64 = ANNOUNCE_AFTER_MS + 1_000 + PRE_ROLL_MS + 1_000;
/// The largest clip file.
pub const CLIP_MAX_BYTES: u64 = WAV_HEADER + CLIP_MAX_MS * BYTES_PER_S / 1000;

/// Clips started in any 60 minutes. A drone visit is one contact, or a few
/// when it moves in and out of hearing; six covers that. A sound that keeps
/// fooling the detector (a siren, a vacuum cleaner, a rotor sound played
/// from a phone) then costs at most six clips, about 11 MB, an hour.
pub const CLIPS_PER_HOUR: usize = 6;
pub const HOUR_MS: u64 = 3_600_000;
/// Audio the card may hold under `ROTORSCP/AUDIO`, all files counted:
/// 256 MiB is about 140 full-length clips (2.3 hours of sound), several
/// weeks of genuine visits, and it bounds how much of people's sound a lost
/// or stolen card can carry.
pub const AUDIO_MAX_BYTES: u64 = 256 << 20;
/// Free space a clip never uses: the evidence log and the CSV log need it.
/// 1 GiB is weeks of both at a busy site, so the logs never stop because
/// of audio. (A card smaller than about 1.3 GB never saves a clip.)
pub const RESERVE_BYTES: u64 = 1 << 30;

/// Whether a clip may start now for this contact: clips are on and the
/// contact is at its alert point. Called for the contacts the tracker has
/// just alerted on, so a clip and its `sound` evidence entry go together.
pub fn may_start(clips_on: bool, c: &SoundContact, now_ms: u64) -> bool {
    clips_on && c.alert_due(now_ms)
}

/// The first sample of a clip that starts at sample `head` for a contact
/// heard for `contact_ms`: the pre-roll before the contact began, at most
/// [`REACH_BACK_MAX_MS`] back, and never before `tape_from` (the first
/// sample on the tape: none is kept from before clips were turned on).
pub fn first_sample(head: u64, contact_ms: u64, tape_from: u64) -> u64 {
    let back_ms = contact_ms.saturating_add(1_000 + PRE_ROLL_MS).min(REACH_BACK_MAX_MS);
    head.saturating_sub(back_ms * SAMPLES_PER_S / 1000).max(tape_from)
}

/// Once a second while a clip is open: whether it goes on. Only while the
/// sound is heard and its contact still alerts: a contact that turns out
/// to be a tracked aircraft (its kind only ever strengthens, and "Aircraft
/// heard" outranks "no radio") is not recorded further. `contact`: `None`
/// when the tracker has dropped it.
pub fn keep_going(heard: bool, contact: Option<&SoundContact>) -> bool {
    heard && contact.is_some_and(|c| c.kind.alerts())
}

/// Once a second while a clip is open: its end (one past the last sample),
/// once known. `heard`: the detector heard the sound for most of the last
/// second.
pub fn end_sample(from: u64, head: u64, heard: bool) -> Option<u64> {
    let max = from + CLIP_MAX_MS * SAMPLES_PER_S / 1000;
    if !heard {
        Some((head + POST_ROLL_MS * SAMPLES_PER_S / 1000).min(max))
    } else if head >= max {
        Some(max)
    } else {
        None
    }
}

/// Why a clip was not saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No card recording.
    NoCard,
    /// [`CLIPS_PER_HOUR`] clips already started in the last hour.
    HourCap,
    /// The card already holds [`AUDIO_MAX_BYTES`] of audio.
    AudioCap,
    /// Free space would fall under [`RESERVE_BYTES`].
    LowSpace,
    /// The card does not say how much space is free (FAT16, or a FAT32
    /// card whose free count is unknown): no clip, to be safe.
    SpaceUnknown,
}

impl Refusal {
    pub const fn text(self) -> &'static str {
        match self {
            Refusal::NoCard => "no card",
            Refusal::HourCap => "hourly clip limit reached",
            Refusal::AudioCap => "audio limit on the card reached",
            Refusal::LowSpace => "card space kept for the evidence log",
            Refusal::SpaceUnknown => "card free space unknown",
        }
    }
}

/// Clips refused since boot, by reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Refused {
    pub no_card: u32,
    pub hour: u32,
    pub audio_cap: u32,
    pub low_space: u32,
    pub space_unknown: u32,
    /// Clips ended early because the card filled up while they were written.
    pub cut_short: u32,
}

impl Refused {
    /// Every clip that was wanted and not (wholly) saved.
    pub fn total(&self) -> u32 {
        [self.no_card, self.hour, self.audio_cap, self.low_space, self.space_unknown, self.cut_short]
            .iter()
            .fold(0u32, |a, &b| a.saturating_add(b))
    }

    fn count(&mut self, r: Refusal) {
        let n = match r {
            Refusal::NoCard => &mut self.no_card,
            Refusal::HourCap => &mut self.hour,
            Refusal::AudioCap => &mut self.audio_cap,
            Refusal::LowSpace => &mut self.low_space,
            Refusal::SpaceUnknown => &mut self.space_unknown,
        };
        *n = n.saturating_add(1);
    }
}

/// What the card holds, as far as clips are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Room {
    /// Free bytes on the card (`None`: unknown).
    pub free_bytes: Option<u64>,
    /// Bytes already in `ROTORSCP/AUDIO`.
    pub audio_bytes: u64,
}

impl Room {
    /// Whether `bytes` more of audio may go on the card.
    pub fn fits(&self, bytes: u64) -> Result<(), Refusal> {
        let free = self.free_bytes.ok_or(Refusal::SpaceUnknown)?;
        if self.audio_bytes.saturating_add(bytes) > AUDIO_MAX_BYTES {
            return Err(Refusal::AudioCap);
        }
        if free < RESERVE_BYTES.saturating_add(bytes) {
            return Err(Refusal::LowSpace);
        }
        Ok(())
    }

    /// The room left after `written` more bytes of audio.
    pub fn after(&self, written: u64) -> Room {
        Room {
            free_bytes: self.free_bytes.map(|f| f.saturating_sub(written)),
            audio_bytes: self.audio_bytes.saturating_add(written),
        }
    }
}

/// The hourly limit and the refusal counts. One per hub.
#[derive(Debug, Clone, Default)]
pub struct Budget {
    /// When the clips of the last hour started (ms since boot).
    starts: Vec<u64, CLIPS_PER_HOUR>,
    pub refused: Refused,
}

impl Budget {
    pub const fn new() -> Self {
        Self { starts: Vec::new(), refused: Refused { no_card: 0, hour: 0, audio_cap: 0, low_space: 0, space_unknown: 0, cut_short: 0 } }
    }

    /// Ask to start a clip at `now_ms` on a card in state `room` (`None`: no
    /// card). The whole largest clip must fit, so a clip that starts is
    /// never the one that eats into the reserve. A refusal is counted.
    pub fn try_start(&mut self, now_ms: u64, room: Option<Room>) -> Result<(), Refusal> {
        let r = self.check(now_ms, room);
        match r {
            Ok(()) => {
                let _ = self.starts.push(now_ms);
            }
            Err(e) => self.refused.count(e),
        }
        r
    }

    fn check(&mut self, now_ms: u64, room: Option<Room>) -> Result<(), Refusal> {
        let room = room.ok_or(Refusal::NoCard)?;
        self.starts.retain(|&s| now_ms.saturating_sub(s) < HOUR_MS);
        if self.starts.is_full() {
            return Err(Refusal::HourCap);
        }
        room.fits(CLIP_MAX_BYTES)
    }

    /// A clip being written: whether its next `bytes` may go on the card,
    /// given the room when it started and the bytes written since. When
    /// not, the clip is ended where it is and counted as cut short.
    pub fn may_continue(&mut self, at_start: Room, written: u64, bytes: u64) -> bool {
        let ok = at_start.after(written).fits(bytes).is_ok();
        if !ok {
            self.refused.cut_short = self.refused.cut_short.saturating_add(1);
        }
        ok
    }
}

/// Free space on a FAT32 card, from its own numbers: the partition table,
/// the boot sector and the FSInfo sector (which the hub's FAT library keeps
/// up to date on every flush). Each function takes one 512-byte sector.
pub mod card {
    fn u16_at(b: &[u8; 512], at: usize) -> u32 {
        u32::from(u16::from_le_bytes([b[at], b[at + 1]]))
    }
    fn u32_at(b: &[u8; 512], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    /// Where a FAT32 volume keeps its free count, and its cluster size.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Fat32 {
        /// Block number of the FSInfo sector.
        pub fsinfo_block: u32,
        pub cluster_bytes: u32,
        /// Data clusters on the volume.
        pub clusters: u32,
    }

    /// The first partition's start block from the MBR (block 0), when it is
    /// a FAT32 partition (types 0x0B, 0x0C), as the hub mounts it.
    pub fn partition_start(mbr: &[u8; 512]) -> Option<u32> {
        if mbr[510] != 0x55 || mbr[511] != 0xAA || !matches!(mbr[446 + 4], 0x0B | 0x0C) {
            return None;
        }
        let lba = u32_at(mbr, 446 + 8);
        (lba != 0).then_some(lba)
    }

    /// Read a FAT32 boot sector at block `start`.
    pub fn fat32(boot: &[u8; 512], start: u32) -> Option<Fat32> {
        if boot[510] != 0x55 || boot[511] != 0xAA || u16_at(boot, 11) != 512 {
            return None;
        }
        let spc = u32::from(boot[13]);
        let (reserved, fats) = (u16_at(boot, 14), u32::from(boot[16]));
        let (fat16_size, fat32_size) = (u16_at(boot, 22), u32_at(boot, 36));
        let (total, fsinfo) = (u32_at(boot, 32), u16_at(boot, 48));
        if !spc.is_power_of_two() || fats == 0 || fat16_size != 0 || fat32_size == 0 || fsinfo == 0 || fsinfo >= reserved {
            return None;
        }
        let data = reserved.checked_add(fats.checked_mul(fat32_size)?)?;
        let clusters = total.checked_sub(data)? / spc;
        Some(Fat32 { fsinfo_block: start.checked_add(fsinfo)?, cluster_bytes: spc * 512, clusters })
    }

    /// Free bytes from the FSInfo sector; `None` when the sector is not
    /// one, the count is "unknown" (0xFFFFFFFF), or it is more than the
    /// volume holds.
    pub fn free_bytes(info: &[u8; 512], fat: &Fat32) -> Option<u64> {
        if u32_at(info, 0) != 0x4161_5252 || u32_at(info, 484) != 0x6141_7272 || u32_at(info, 508) != 0xAA55_0000 {
            return None;
        }
        let free = u32_at(info, 488);
        (free <= fat.clusters).then(|| u64::from(free) * u64::from(fat.cluster_bytes))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::card::*;
    use super::*;
    use crate::Tracker;
    use crate::sound::{SoundKind, SoundReport};

    fn heard() -> SoundReport {
        SoundReport { heard: true, pitch_hz: 200, confidence: 80, level_dbfs: -30, small_props: false }
    }

    /// A contact heard every second from 1 s to `to_s`.
    fn contact(to_s: u64) -> (Tracker, SoundContact) {
        let mut tr: Tracker = Tracker::default();
        let mut id = None;
        for s in 1..=to_s {
            id = tr.ingest_sound(&heard(), s * 1000);
        }
        let c = *tr.get_sound(id.unwrap()).unwrap();
        (tr, c)
    }

    const ROOMY: Room = Room { free_bytes: Some(8 << 30), audio_bytes: 0 };

    // ---- When a clip may start (the old firmware clipped on the first
    // heard second of any contact, whether or not clips were asked for).

    #[test]
    fn no_clip_while_clips_are_off() {
        let (_, c) = contact(10);
        assert!(c.alert_due(10_000));
        assert!(!may_start(false, &c, 10_000));
        assert!(may_start(true, &c, 10_000));
    }

    #[test]
    fn no_clip_before_the_alert_threshold() {
        for to in 1..=5 {
            let (_, c) = contact(to);
            assert!(!may_start(true, &c, to * 1000), "{} s heard: too short", to - 1);
        }
        let (_, c) = contact(6);
        assert!(may_start(true, &c, 6_000), "5 s heard: alerts, so it may clip");
    }

    #[test]
    fn no_clip_for_an_aircraft() {
        let (_, mut c) = contact(20);
        c.kind = SoundKind::Aircraft;
        assert!(!may_start(true, &c, 20_000));
        c.kind = SoundKind::WithRadio;
        assert!(may_start(true, &c, 20_000));
    }

    #[test]
    fn no_clip_once_the_contact_has_gone_quiet() {
        let (_, c) = contact(10);
        assert!(!may_start(true, &c, 10_000 + crate::sound::CONTACT_GAP_MS));
    }

    /// The clip starts exactly when the tracker alerts, never earlier.
    #[test]
    fn clip_and_alert_agree_second_by_second() {
        let mut tr: Tracker = Tracker::default();
        for s in 1..=12u64 {
            let id = tr.ingest_sound(&heard(), s * 1000).unwrap();
            let c = *tr.get_sound(id).unwrap();
            let alerted = tr.take_new_sound(s * 1000) == Some(id);
            assert_eq!(may_start(true, &c, s * 1000), s >= 6, "second {s}");
            if alerted {
                assert!(may_start(true, &c, s * 1000));
            }
        }
    }

    #[test]
    fn a_clip_stops_when_its_contact_turns_out_to_be_an_aircraft() {
        let (_, mut c) = contact(10);
        assert!(keep_going(true, Some(&c)));
        assert!(!keep_going(false, Some(&c)), "not heard");
        assert!(!keep_going(true, None), "contact gone");
        c.kind = SoundKind::Aircraft;
        assert!(!keep_going(true, Some(&c)));
    }

    // ---- What the clip covers.

    #[test]
    fn the_clip_reaches_back_to_the_onset_plus_pre_roll() {
        let rate = SAMPLES_PER_S;
        let head = 1_000 * rate;
        // Heard for 5 s: the contact began 6 s of sound ago (the first
        // report covers the second before it); plus 5 s of pre-roll.
        assert_eq!(first_sample(head, 5_000, 0), head - 11 * rate);
        // A contact that alerted late reaches back no more than the limit.
        assert_eq!(first_sample(head, 40_000, 0), head - REACH_BACK_MAX_MS * rate / 1000);
        assert_eq!(REACH_BACK_MAX_MS, 12_000);
        // Nothing from before the tape started (clips just turned on).
        assert_eq!(first_sample(head, 5_000, head - 2 * rate), head - 2 * rate);
        assert_eq!(first_sample(3 * rate, 5_000, 0), 0);
    }

    #[test]
    fn the_clip_ends_after_the_post_roll_or_at_the_maximum() {
        let rate = SAMPLES_PER_S;
        assert_eq!(end_sample(0, 20 * rate, true), None);
        assert_eq!(end_sample(0, 20 * rate, false), Some(22 * rate));
        assert_eq!(end_sample(0, 59 * rate, false), Some(60 * rate));
        assert_eq!(end_sample(0, 60 * rate, true), Some(60 * rate));
        assert_eq!(CLIP_MAX_BYTES, 44 + 60 * 32_000);
    }

    // ---- Caps (the old firmware had none).

    #[test]
    fn at_most_six_clips_in_any_hour() {
        let mut b = Budget::new();
        for i in 0..CLIPS_PER_HOUR as u64 {
            assert_eq!(b.try_start(i * 60_000, Some(ROOMY)), Ok(()));
        }
        assert_eq!(b.try_start(10 * 60_000, Some(ROOMY)), Err(Refusal::HourCap));
        assert_eq!(b.try_start(59 * 60_000, Some(ROOMY)), Err(Refusal::HourCap));
        assert_eq!(b.refused.hour, 2);
        // The first clip (at 0) is an hour old: one more may start.
        assert_eq!(b.try_start(HOUR_MS, Some(ROOMY)), Ok(()));
        assert_eq!(b.try_start(HOUR_MS + 1, Some(ROOMY)), Err(Refusal::HourCap));
        assert_eq!(b.refused.total(), 3);
    }

    #[test]
    fn total_audio_on_the_card_is_capped() {
        let mut b = Budget::new();
        let nearly = Room { free_bytes: Some(64 << 30), audio_bytes: AUDIO_MAX_BYTES - CLIP_MAX_BYTES };
        assert_eq!(b.try_start(0, Some(nearly)), Ok(()));
        let full = Room { audio_bytes: AUDIO_MAX_BYTES - CLIP_MAX_BYTES + 1, ..nearly };
        assert_eq!(b.try_start(1, Some(full)), Err(Refusal::AudioCap));
        assert_eq!(b.refused.audio_cap, 1);
    }

    #[test]
    fn clips_never_use_the_evidence_reserve() {
        let mut b = Budget::new();
        let just = Room { free_bytes: Some(RESERVE_BYTES + CLIP_MAX_BYTES), audio_bytes: 0 };
        assert_eq!(b.try_start(0, Some(just)), Ok(()));
        let short = Room { free_bytes: Some(RESERVE_BYTES + CLIP_MAX_BYTES - 1), audio_bytes: 0 };
        assert_eq!(b.try_start(1, Some(short)), Err(Refusal::LowSpace));
        assert_eq!(b.try_start(2, Some(Room { free_bytes: Some(0), audio_bytes: 0 })), Err(Refusal::LowSpace));
        assert_eq!(b.try_start(3, Some(Room { free_bytes: None, audio_bytes: 0 })), Err(Refusal::SpaceUnknown));
        assert_eq!(b.try_start(4, None), Err(Refusal::NoCard));
        assert_eq!((b.refused.low_space, b.refused.space_unknown, b.refused.no_card), (2, 1, 1));
        assert_eq!(b.refused.total(), 4);
    }

    #[test]
    fn a_clip_stops_when_the_card_fills_while_it_is_written() {
        let mut b = Budget::new();
        let start = Room { free_bytes: Some(RESERVE_BYTES + 10_000), audio_bytes: 0 };
        assert!(b.may_continue(start, 0, 512));
        assert!(b.may_continue(start, 9_488, 512));
        assert!(!b.may_continue(start, 9_489, 512));
        assert_eq!(b.refused.cut_short, 1);
        let capped = Room { free_bytes: Some(64 << 30), audio_bytes: AUDIO_MAX_BYTES - 1_000 };
        assert!(b.may_continue(capped, 488, 512));
        assert!(!b.may_continue(capped, 489, 512));
    }

    // ---- Free space from the card's own sectors.

    fn sectors(free: u32) -> ([u8; 512], [u8; 512], [u8; 512]) {
        let mut mbr = [0u8; 512];
        mbr[446 + 4] = 0x0C;
        mbr[446 + 8..446 + 12].copy_from_slice(&8192u32.to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        let mut boot = [0u8; 512];
        boot[11..13].copy_from_slice(&512u16.to_le_bytes());
        boot[13] = 64; // 32 KiB clusters
        boot[14..16].copy_from_slice(&32u16.to_le_bytes());
        boot[16] = 2;
        boot[32..36].copy_from_slice(&(62_333_952u32 - 8192).to_le_bytes()); // a "32 GB" card
        boot[36..40].copy_from_slice(&7_597u32.to_le_bytes());
        boot[48..50].copy_from_slice(&1u16.to_le_bytes());
        boot[510] = 0x55;
        boot[511] = 0xAA;
        let mut info = [0u8; 512];
        info[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
        info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
        info[488..492].copy_from_slice(&free.to_le_bytes());
        info[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
        (mbr, boot, info)
    }

    #[test]
    fn free_space_from_a_fat32_card() {
        let (mbr, boot, info) = sectors(100_000);
        let start = partition_start(&mbr).unwrap();
        assert_eq!(start, 8192);
        let fat = fat32(&boot, start).unwrap();
        assert_eq!(fat.fsinfo_block, 8193);
        assert_eq!(fat.cluster_bytes, 32 * 1024);
        assert_eq!(fat.clusters, (62_333_952 - 8192 - 32 - 2 * 7_597) / 64);
        assert_eq!(free_bytes(&info, &fat), Some(100_000 * 32 * 1024));
    }

    #[test]
    fn unknown_or_odd_free_space_is_unknown() {
        let (mut mbr, mut boot, _) = sectors(0);
        let fat = fat32(&boot, 8192).unwrap();
        assert_eq!(free_bytes(&sectors(u32::MAX).2, &fat), None, "0xFFFFFFFF means unknown");
        assert_eq!(free_bytes(&sectors(fat.clusters + 1).2, &fat), None, "more than the volume");
        let mut bad = sectors(5).2;
        bad[0] = 0;
        assert_eq!(free_bytes(&bad, &fat), None);
        // FAT16 has no FSInfo.
        boot[22] = 1;
        assert_eq!(fat32(&boot, 8192), None);
        mbr[446 + 4] = 0x06;
        assert_eq!(partition_start(&mbr), None);
        mbr[446 + 4] = 0x0B;
        mbr[511] = 0;
        assert_eq!(partition_start(&mbr), None);
    }
}
