//! The hand-off between the Wi-Fi driver's receive callback and our tasks.
//!
//! The callback runs on the driver's own thread, ahead of the radio's next
//! frame, so it does the least possible: copy a management frame into a free
//! slot and pass the slot's index on, without decoding it.
//!
//! Data frames (on when `chip::OPEN_LINKS`) take a different, slot-free path:
//! `openlink::classify` looks at their first bytes right here (almost every
//! frame is rejected on its frame-control byte, the rest on a handful of
//! address bytes), and a hit only bumps a counter in [`SIGHTINGS`]. A busy
//! channel's data traffic therefore never competes with Remote ID for the
//! 32 slots; the `open_links` task reads the table a few times a second. Slots are fixed buffers in a static pool; ownership moves by index
//! through two channels (`FREE` -> callback -> `FULL` -> task -> `FREE`), so a
//! frame is copied exactly once and nothing large lands on the driver's stack.
//!
//! Deauthentication, disassociation, authentication, (re)association and
//! probe requests never take a slot either: `wifiwatch::floods` counts them
//! from their headers right here ([`FLOODS`]) and the `threats` task reports
//! floods. Only beacons, probe responses and action frames (what Remote ID
//! and the other detectors need) are queued, so a deauth flood of thousands
//! of frames a second cannot crowd Remote ID out.
//!
//! Drone hijacking (`wifiwatch::drones`, docs/DRONE-ATTACKS.md) works the
//! same way once the drain task has recognised a drone's network: the
//! callback hands deauth / disassoc, authentication, (re)association
//! requests and responses, and the addresses of data frames, to
//! [`DRONES`]. While no drone is known ([`DRONES_ON`] false) that costs one
//! atomic load. Association responses are needed by nothing else, so they
//! are not queued either.

use core::cell::{RefCell, UnsafeCell};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_radio::wifi::sniffer::PromiscuousPkt;

use crate::chip::sys;

/// Bytes kept per frame. Remote ID beacons are ~250-400 bytes; a longer frame
/// keeps its first 512 bytes, which is enough to find a vendor element placed
/// early (all known transmitters put it right after SSID/rates).
pub const SLOT_BYTES: usize = 512;
/// Queue depth. ~16 KB of RAM; at a few hundred frames/s the drain task keeps
/// it near empty, and the depth only has to absorb bursts.
pub const SLOTS: usize = 32;

pub struct RxFrame {
    /// Node uptime when the callback ran.
    pub up_us: u64,
    /// `sig_len` as delivered, FCS slot included.
    pub raw_len: u16,
    /// Bytes actually copied into `data` (`raw_len` capped at `SLOT_BYTES`).
    pub copied: u16,
    pub rssi: i8,
    pub channel: u8,
    pub data: [u8; SLOT_BYTES],
}

impl RxFrame {
    const EMPTY: Self = Self { up_us: 0, raw_len: 0, copied: 0, rssi: 0, channel: 0, data: [0; SLOT_BYTES] };

    /// The 802.11 frame from frame control onwards, FCS removed. If the frame
    /// was longer than a slot this is the truncated prefix.
    pub fn mpdu(&self) -> &[u8] {
        let end = usize::from(self.raw_len).saturating_sub(crate::chip::FCS_LEN);
        &self.data[..end.min(usize::from(self.copied))]
    }

    pub fn truncated(&self) -> bool {
        self.copied < self.raw_len
    }
}

struct Pool([UnsafeCell<RxFrame>; SLOTS]);
// SAFETY: a slot is only touched by whoever holds its index, and an index is
// in exactly one place at a time (FREE, the callback, FULL, or a `Received`).
unsafe impl Sync for Pool {}

static POOL: Pool = Pool([const { UnsafeCell::new(RxFrame::EMPTY) }; SLOTS]);
static FREE: Channel<CriticalSectionRawMutex, u8, SLOTS> = Channel::new();
static FULL: Channel<CriticalSectionRawMutex, u8, SLOTS> = Channel::new();

/// Management frames the callback accepted (queued or dropped).
pub static MGMT_SEEN: AtomicU32 = AtomicU32::new(0);
/// Data frames the callback looked at.
pub static DATA_SEEN: AtomicU32 = AtomicU32::new(0);
/// Data frames recognised as an open FPV / telemetry link.
pub static OPEN_SEEN: AtomicU32 = AtomicU32::new(0);
/// Links (streams) the node is counting. 16 covers a few links' worth of
/// streams (wfb-ng uses up to five per link); beyond that the quietest
/// stream is replaced and counted in `Sightings::evicted`.
pub const OPEN_STREAMS: usize = 16;
/// Per-stream packet counts, written by the callback, read by `open_links`.
pub static SIGHTINGS: critical_section::Mutex<RefCell<openlink::Sightings<OPEN_STREAMS>>> =
    critical_section::Mutex::new(RefCell::new(openlink::Sightings::new()));
/// Header-only management frames (deauth, disassoc, auth, association and
/// probe requests), counted by the callback, judged by the `threats` task.
pub static FLOODS: critical_section::Mutex<RefCell<wifiwatch::floods::Floods>> =
    critical_section::Mutex::new(RefCell::new(wifiwatch::floods::Floods::new()));
/// Who is joined to the drone networks the node knows, and who is kicking
/// them off: fed by the callback, learned from beacons by the drain task,
/// judged by the `threats` task.
pub static DRONES: critical_section::Mutex<RefCell<wifiwatch::drones::DroneLinks>> =
    critical_section::Mutex::new(RefCell::new(wifiwatch::drones::DroneLinks::new()));
/// [`DRONES`] has at least one drone network: only then does the callback
/// take the lock for it.
pub static DRONES_ON: AtomicBool = AtomicBool::new(false);
/// Management frames lost because every slot was in use.
pub static DROPPED: AtomicU32 = AtomicU32::new(0);
/// Callbacks for anything else: control (or other) frames past the filter,
/// or management frames too short to have a header.
pub static OTHER: AtomicU32 = AtomicU32::new(0);
/// Most slots in use at once since the stats task last reset it.
pub static HIGH_WATER: AtomicU32 = AtomicU32::new(0);
/// The channel the hopper last tuned to; used when RX metadata has none.
pub static TUNED_CHANNEL: AtomicU8 = AtomicU8::new(0);

/// Fill the free list. Call once, before registering the callback.
pub fn init() {
    for i in 0..SLOTS as u8 {
        // Cannot fail: the channel holds exactly SLOTS entries.
        let _ = FREE.try_send(i);
    }
}

/// The promiscuous receive callback. Keep it this small.
pub fn on_frame(pkt: PromiscuousPkt<'_>) {
    let data = pkt.data;
    if pkt.frame_type == sys::wifi_promiscuous_pkt_type_t_WIFI_PKT_DATA {
        on_data(&pkt);
        return;
    }
    // Frame control byte 0: version (bits 0-1) and type (bits 2-3) both zero
    // means a management frame. Checked here as well as by the filter.
    if pkt.frame_type != sys::wifi_promiscuous_pkt_type_t_WIFI_PKT_MGMT
        || data.len() < 24
        || data[0] & 0x0F != 0
    {
        OTHER.fetch_add(1, Relaxed);
        return;
    }
    MGMT_SEEN.fetch_add(1, Relaxed);
    let channel = match pkt.rx_cntl.channel {
        0 => TUNED_CHANNEL.load(Relaxed),
        c => c.min(255) as u8,
    };

    // Header-only subtypes (and association responses): count them here,
    // never queue them.
    let subtype = data[0] >> 4;
    let drone_link = wifiwatch::mac::drone_link(subtype);
    if wifiwatch::mac::header_only(subtype) || drone_link {
        let mpdu = &data[..data.len().saturating_sub(crate::chip::FCS_LEN)];
        let rssi = pkt.rx_cntl.rssi.clamp(-128, 127) as i8;
        let now = embassy_time::Instant::now().as_micros();
        let drones = drone_link && DRONES_ON.load(Relaxed);
        critical_section::with(|cs| {
            FLOODS.borrow_ref_mut(cs).frame(mpdu, rssi, channel, now);
            if drones {
                DRONES.borrow_ref_mut(cs).frame(mpdu, rssi, channel, now);
            }
        });
        if let Some(c) = crate::PER_CHANNEL.get(usize::from(channel)) {
            c.fetch_add(1, Relaxed);
        }
        return;
    }

    let Ok(idx) = FREE.try_receive() else {
        DROPPED.fetch_add(1, Relaxed);
        return;
    };
    let in_use = (SLOTS - FREE.len()) as u32;
    HIGH_WATER.fetch_max(in_use, Relaxed);

    // SAFETY: we hold `idx`, so no one else can reach this slot.
    let slot = unsafe { &mut *POOL.0[usize::from(idx)].get() };
    let n = data.len().min(SLOT_BYTES);
    slot.data[..n].copy_from_slice(&data[..n]);
    slot.copied = n as u16;
    slot.raw_len = data.len().min(u16::MAX as usize) as u16;
    slot.rssi = pkt.rx_cntl.rssi.clamp(-128, 127) as i8;
    slot.channel = channel;
    slot.up_us = embassy_time::Instant::now().as_micros();
    // Cannot fail: FULL has room for every index.
    let _ = FULL.try_send(idx);
}

/// A data frame: classify it in place and count a hit. No slot, no queue.
fn on_data(pkt: &PromiscuousPkt<'_>) {
    DATA_SEEN.fetch_add(1, Relaxed);
    let rssi = pkt.rx_cntl.rssi.clamp(-128, 127) as i8;
    let channel = match pkt.rx_cntl.channel {
        0 => TUNED_CHANNEL.load(Relaxed),
        c => c.min(255) as u8,
    };
    let now = embassy_time::Instant::now().as_micros();
    // Who is talking to a known drone (addresses only; FCS harmless).
    if DRONES_ON.load(Relaxed) {
        critical_section::with(|cs| DRONES.borrow_ref_mut(cs).data(pkt.data, rssi, channel, now));
    }
    // `data` ends with the FCS slot, which `classify` expects. Its first test
    // is the frame-control byte, so ordinary (QoS) data costs one compare.
    let Some(link) = openlink::classify(pkt.data) else { return };
    OPEN_SEEN.fetch_add(1, Relaxed);
    critical_section::with(|cs| SIGHTINGS.borrow_ref_mut(cs).record(&link, channel, rssi, now));
}

/// A queued frame. Returning it to the pool happens on drop.
pub struct Received(u8);

impl core::ops::Deref for Received {
    type Target = RxFrame;
    fn deref(&self) -> &RxFrame {
        // SAFETY: we hold the index; the callback only writes to free slots.
        unsafe { &*POOL.0[usize::from(self.0)].get() }
    }
}

impl Drop for Received {
    fn drop(&mut self) {
        let _ = FREE.try_send(self.0);
    }
}

pub async fn next() -> Received {
    Received(FULL.receive().await)
}
