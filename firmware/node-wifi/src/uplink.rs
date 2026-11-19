//! ESP-NOW uplink (feature `espnow`): hub frames to a gateway instead of the UART.
//!
//! Frames from `report::HUB_QUEUE` are packed into `airlink` batches and sent
//! unicast to the gateway, whose address is learned from its beacons. A batch
//! goes out when it is full or [`HOLD`] old, and only while the hopper is on
//! the gateway's channel; the hopper waits for a send in flight to finish
//! before it retunes ([`wait_idle`]), so a send is never cut off mid-air.
//! Unicast gets the radio's ACK and retries; a batch that still fails stays
//! queued and is retried, and is dropped only after [`GIVE_UP`].
//!
//! Latency budget: up to HOLD for batching plus up to the 100 ms the 2.4 GHz
//! plan spends off channel 6, so a detection reaches the gateway in well
//! under a quarter of a second.
//!
//! The link (docs/LINK-KEYS-DESIGN.md, package 13g) is the shared glue in
//! `node_common::linkauth` / `nodeimage::uplink`: the key comes from the
//! board's `rskey` partition, read at boot (`main`), never from the image.
//! Keyed, every attempt is sealed afresh (version 3) and only gateway
//! packets the link verifies are believed; an open record runs the old
//! unauthenticated link; a missing, damaged or wrong-board record (or no
//! hardware random source) sends no reports at all, only an `Unprovisioned`
//! announcement and a USB warning every 10 s, and blinks the user LED (C5).

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use embassy_time::{Duration, Instant, Timer};
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow, EspNowWifiInterface, PeerInfo, PhyMode, RateConfig, WifiPhyRate};
use node_common::linkauth::{self, Led, Rx};

use crate::report::HUB_QUEUE;
use crate::rx::TUNED_CHANNEL;

/// Batch window. Longer packs more frames per packet (less airtime overhead)
/// but delays every detection by up to this much.
const HOLD: Duration = Duration::from_millis(50);
/// Retry a failed batch for this long, then drop it.
const GIVE_UP: Duration = Duration::from_secs(2);
/// How often the task looks at the queue and the channel.
const POLL: Duration = Duration::from_millis(5);

/// PHY rate toward the gateway, chosen at build time with
/// `ROTORSCOPE_ESPNOW_RATE` (1M, 11M, 6M, 24M, 54M, MCS7, LR250K, LR500K).
/// Unset: the driver default, 1 Mbit/s 802.11b, the longest reach of the
/// standard rates. Faster rates spend less time on air per packet (more time
/// listening for drones) but need a stronger signal. LR is Espressif's
/// proprietary long-range mode; the gateway enables it in its protocol set.
pub const RATE: Option<&str> = option_env!("ROTORSCOPE_ESPNOW_RATE");

/// Add Espressif Long Range to the station's 2.4 GHz protocols (keeping b/g/n),
/// so LR packets are understood; ordinary rates are unaffected.
fn enable_lr() -> i32 {
    let bits = (crate::chip::sys::WIFI_PROTOCOL_11B | crate::chip::sys::WIFI_PROTOCOL_11G | crate::chip::sys::WIFI_PROTOCOL_11N | crate::chip::sys::WIFI_PROTOCOL_LR) as u8;
    // SAFETY: Wi-Fi is initialised; plain value arguments.
    unsafe { crate::chip::sys::esp_wifi_set_protocol(crate::chip::sys::wifi_interface_t_WIFI_IF_STA, bits) }
}

fn rate_config(name: &str) -> Option<RateConfig> {
    let (phy_mode, rate) = match name {
        "1M" => (PhyMode::_11b, WifiPhyRate::Rate1mL),
        "11M" => (PhyMode::_11b, WifiPhyRate::Rate11mS),
        "6M" => (PhyMode::_11g, WifiPhyRate::Rate6m),
        "24M" => (PhyMode::_11g, WifiPhyRate::Rate24m),
        "54M" => (PhyMode::_11g, WifiPhyRate::Rate54m),
        "MCS7" => (PhyMode::Ht20, WifiPhyRate::RateMcs7Lgi),
        "LR250K" => (PhyMode::Lr, WifiPhyRate::RateLora250k),
        "LR500K" => (PhyMode::Lr, WifiPhyRate::RateLora500k),
        _ => return None,
    };
    Some(RateConfig { phy_mode, rate, ersu: false, dcm: false })
}

/// A send is in flight: the hopper must not retune yet.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Payload bytes in acknowledged batches, and microseconds spent waiting
/// for sends to complete (radio busy transmitting). The rest of the
/// accounting is the link's (`linkauth::counts`).
pub static BYTES: AtomicU32 = AtomicU32::new(0);
pub static SEND_US: AtomicU32 = AtomicU32::new(0);

/// Called by the hopper before each retune.
pub async fn wait_idle() {
    let t = Instant::now();
    while BUSY.load(Relaxed) && t.elapsed() < Duration::from_millis(30) {
        Timer::after(Duration::from_millis(1)).await;
    }
}

fn say(l: &str) {
    crate::usb::line(l.as_bytes());
}

/// Send `data` to `to` with the hopper held off; true if the radio got an ACK
/// (always false for broadcast, which gets none).
pub(crate) async fn send(now: &mut EspNow, to: &[u8; 6], data: &[u8]) -> bool {
    BUSY.store(true, Relaxed);
    let t0 = Instant::now();
    let r = now.send_async(to, data).await;
    BUSY.store(false, Relaxed);
    SEND_US.fetch_add(t0.elapsed().as_micros() as u32, Relaxed);
    r.is_ok()
}

/// Add `mac` as a unicast peer if it is not one yet.
pub(crate) fn add_peer(now: &EspNow, mac: &[u8; 6]) {
    if !now.peer_exists(mac) {
        let _ = now.add_peer(PeerInfo { interface: EspNowWifiInterface::Station, peer_address: *mac, lmk: None, channel: None, encrypt: false });
    }
}

#[embassy_executor::task]
pub async fn run(mut now: EspNow, ks: node_common::keystore::Keystore, mut led: Led) {
    // After `WifiController::new`: the TRNG is running now, or never.
    let mut up = linkauth::start(ks, say);
    let mut batch = airlink::Batch::new();
    let mut batch_since = Instant::now();
    let mut seq: u16 = 0;
    let mut last_stats = Instant::now();
    if RATE.is_some_and(|r| r.starts_with("LR")) {
        let e = enable_lr();
        crate::usb::usb_line!(r#"{{"t":"uplink","lr_protocol":{}}}"#, e == 0);
    }
    let mut pending: Option<heapless::Vec<u8, { link::MAX_FRAME }>> = None;
    let mut announce = [0u8; airlink::v3::UNPROVISIONED_LEN];
    #[cfg(feature = "forgetest")]
    let mut forge = crate::forge::Air::new();

    loop {
        // Beacons and syncs: learn (or refresh) the gateway. Commands: hand
        // the Assigns to `role`. The link checks everything first.
        while let Some(got) = now.receive() {
            if let Rx::Commands(p) = up.receive(&got.info.src_address, got.data(), linkauth::now_ms()) {
                linkauth::uplink::commands(p, |c| {
                    if let link::Command::Assign(a) = c {
                        // Full: an older command is still queued; the hub resends.
                        let _ = crate::role::COMMANDS.try_send(a);
                    }
                });
            }
        }
        if let Some((mac, ch)) = up.take_new_gateway() {
            add_peer(&now, &mac);
            let rate_ok = match RATE.map(|r| (r, rate_config(r))) {
                None => "default",
                Some((_, None)) => "unknown",
                Some((_, Some(cfg))) => {
                    if now.set_peer_rate(&mac, cfg).is_ok() { "set" } else { "refused" }
                }
            };
            crate::usb::usb_line!(
                r#"{{"t":"uplink","gateway":"{}","ch":{ch},"rate":"{}","rate_cfg":"{rate_ok}"}}"#,
                linkauth::MacText(mac),
                RATE.unwrap_or("1M")
            );
        }
        linkauth::publish(&up);
        led.update(up.blocked().is_some());
        let tuned = TUNED_CHANNEL.load(Relaxed);

        if !up.may_send() {
            // No reports over ESP-NOW: empty the queue (counted), and every
            // 10 s, on the gateway channel, the warning and one announcement.
            while HUB_QUEUE.try_receive().is_ok() {
                up.not_sent(1);
            }
            if tuned == airlink::GATEWAY_CHANNEL
                && let Some(p) = linkauth::blocked_tick(&mut up, linkauth::IMAGE_WIFI, "node-wifi", env!("CARGO_PKG_VERSION"), &mut announce, &say)
            {
                send(&mut now, &BROADCAST_ADDRESS, p).await;
            }
        } else {
            // Fill the batch: first a frame left over from last time, then the queue.
            loop {
                let frame = match pending.take() {
                    Some(f) => f,
                    None => match HUB_QUEUE.try_receive() {
                        Ok(f) => f,
                        Err(_) => break,
                    },
                };
                if batch.is_empty() {
                    batch_since = Instant::now();
                }
                if !batch.push(&frame) {
                    // Full: this frame starts the next batch, after a send.
                    pending = Some(frame);
                    break;
                }
            }

            #[cfg(feature = "forgetest")]
            let paused = forge.step(&mut now, &mut up, tuned).await;
            #[cfg(not(feature = "forgetest"))]
            let paused = false;

            let due = !batch.is_empty() && (pending.is_some() || batch_since.elapsed() >= HOLD);
            if due && !paused {
                let t = linkauth::now_ms();
                if let Some((mac, ch)) = up.gateway(t)
                    && tuned == ch
                {
                    let frames = batch.frames();
                    let len = batch.payload_len();
                    // Keyed: sealed afresh for this attempt; `None` until the
                    // node knows a hub epoch (it has not heard a beacon yet).
                    if let Some(pkt) = up.packet(&mut batch, seq, t) {
                        let acked = send(&mut now, &mac, pkt).await;
                        up.sent(acked, frames, linkauth::now_ms());
                        if acked {
                            // Only on success: a retry of a batch whose ACK was lost
                            // reuses the number, so the gateway drops the duplicate.
                            seq = seq.wrapping_add(1);
                            BYTES.fetch_add(len as u32, Relaxed);
                            batch.clear();
                        }
                    }
                }
                if !batch.is_empty() && batch_since.elapsed() > GIVE_UP {
                    up.given_up();
                    batch.clear();
                    // The gateway may have had it after all; never reuse its number.
                    seq = seq.wrapping_add(1);
                }
            }
        }

        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            let t = linkauth::now_ms();
            let c = up.counts;
            let (epoch, proven, relinks) = up.node().map_or((0, false, 0), |n| (n.epoch().unwrap_or(0), n.proven(), n.relinks()));
            crate::usb::usb_line!(
                r#"{{"t":"uplink","gw":{},"auth":"{}","epoch":{epoch},"proven":{proven},"relinks":{relinks},"heard":{},"beacons":{},"syncs":{},"cmds":{},"cmd_dups":{},"pkts":{},"frames":{},"bytes":{},"failed":{},"dropped":{},"not_sent":{},"announced":{},"send_us":{}}}"#,
                up.gateway(t).is_some(),
                up.state_name(),
                c.heard,
                c.beacons,
                c.syncs,
                c.commands,
                c.rejects.dups,
                c.batches_sent,
                c.frames_sent,
                BYTES.load(Relaxed),
                c.failed(),
                c.batches_given_up,
                c.not_sent,
                c.announcements,
                SEND_US.load(Relaxed)
            );
            linkauth::say_rejects(&c, &say);
        }
        Timer::after(POLL).await;
    }
}
