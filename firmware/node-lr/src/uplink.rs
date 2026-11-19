//! ESP-NOW uplink (feature `espnow`): hub frames to the gateway, the hub's
//! commands back. The same `airlink` scheme as node-ble's uplink (batches
//! of whole `link` frames, unicast to the gateway learned from its beacons),
//! simplified: this node's Wi-Fi radio is its own (the LR2021 does the
//! listening), so Wi-Fi stays on the gateway's channel
//! (`airlink::GATEWAY_CHANNEL`) and may send at any time. Its plan steers
//! only the LR2021, so no plan can keep it from the gateway: it has no deaf
//! fall-back to run.
//!
//! The link (docs/LINK-KEYS-DESIGN.md, package 13g) is the shared glue in
//! `node_common::linkauth` / `nodeimage::uplink`: the key comes from the
//! board's `rskey` partition (`tools/partitions-single.csv`), never from the
//! image. Keyed, every attempt is sealed afresh and only gateway packets the
//! link verifies are believed; a Command holding a `TimeSync` gets the
//! link's 0.5 s lateness rule, and `aerohop`'s three-agreeing-syncs rule
//! still applies on top. An open record runs the old unauthenticated link;
//! an unprovisioned node sends no reports, only a warning and an
//! `Unprovisioned` announcement every 10 s (the XIAO ESP32-C3 has no user
//! LED to blink).

use embassy_time::{Duration, Instant, Timer};
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow, EspNowWifiInterface, PeerInfo};
use portable_atomic::{AtomicU32, Ordering::Relaxed};

use node_common::linkauth::{self, Rx};

use crate::out::{self, COUNTERS};
use crate::state;

const HOLD: Duration = Duration::from_millis(50);
const GIVE_UP: Duration = Duration::from_secs(2);
/// Receive polling period; also the resolution of a `TimeSync` arrival
/// stamp (ms-level jitter is harmless next to 400 ms slots).
const POLL: Duration = Duration::from_millis(2);

/// Frames longer than `link::MAX_FRAME` found in the pipe (dropped).
pub static OVERSIZE: AtomicU32 = AtomicU32::new(0);

type Frame = heapless::Vec<u8, { link::MAX_FRAME }>;

fn say(l: &str) {
    out::line(format_args!("{l}"));
}

#[embassy_executor::task]
pub async fn run(mut now: EspNow, ks: node_common::keystore::Keystore) {
    // After `WifiController::new`: the TRNG is running now, or never.
    let mut up = linkauth::start(ks, say);
    let mut batch = airlink::Batch::new();
    let mut batch_since = Instant::now();
    let mut seq: u16 = 0;
    let mut acc = Frame::new();
    let mut acc_bad = false;
    let mut queue: heapless::Deque<Frame, 8> = heapless::Deque::new();
    let mut chunk = [0u8; 256];
    let mut last_stats = Instant::now();
    let mut announce = [0u8; airlink::v3::UNPROVISIONED_LEN];

    loop {
        while let Some(got) = now.receive() {
            // The arrival instant, before the tag check, for a TimeSync.
            let at_us = state::now_us();
            if let Rx::Commands(p) = up.receive(&got.info.src_address, got.data(), linkauth::now_ms()) {
                linkauth::uplink::commands(p, |c| state::on_command(c, at_us));
            }
        }
        if let Some((mac, _)) = up.take_new_gateway() {
            if !now.peer_exists(&mac) {
                let _ = now.add_peer(PeerInfo { interface: EspNowWifiInterface::Station, peer_address: mac, lmk: None, channel: None, encrypt: false });
            }
            out::line(format_args!(r#"{{"t":"uplink","gateway":"{}","ch":{}}}"#, linkauth::MacText(mac), airlink::GATEWAY_CHANNEL));
        }
        linkauth::publish(&up);

        // Whole frames from the pipe...
        while let Ok(n) = out::UART.try_read(&mut chunk) {
            for &b in &chunk[..n] {
                if acc_bad {
                    acc_bad = b != 0;
                    continue;
                }
                if acc.push(b).is_err() {
                    OVERSIZE.fetch_add(1, Relaxed);
                    acc.clear();
                    acc_bad = b != 0;
                } else if b == 0 {
                    let f = core::mem::take(&mut acc);
                    if f.len() > 1 && queue.push_back(f).is_err() {
                        out::bump(&COUNTERS.uart_drops);
                    }
                }
            }
            if queue.is_full() {
                break;
            }
        }

        if !up.may_send() {
            // No reports over ESP-NOW (counted); every 10 s the warning and
            // one announcement.
            let n = queue.len() as u32;
            queue.clear();
            up.not_sent(n);
            if let Some(p) = linkauth::blocked_tick(&mut up, linkauth::IMAGE_LR, "node-lr", crate::FIRMWARE, &mut announce, &say) {
                let _ = now.send_async(&BROADCAST_ADDRESS, p).await;
            }
        } else {
            // ...packed into the batch.
            while let Some(f) = queue.front() {
                if batch.is_empty() {
                    batch_since = Instant::now();
                }
                if !batch.push(f) {
                    break;
                }
                queue.pop_front();
            }

            let due = !batch.is_empty() && (!queue.is_empty() || batch_since.elapsed() >= HOLD);
            if due {
                let t = linkauth::now_ms();
                if let Some((mac, _)) = up.gateway(t) {
                    let frames = batch.frames();
                    // Keyed: sealed afresh for this attempt; `None` until the
                    // node knows a hub epoch.
                    if let Some(pkt) = up.packet(&mut batch, seq, t) {
                        let acked = now.send_async(&mac, pkt).await.is_ok();
                        up.sent(acked, frames, linkauth::now_ms());
                        if acked {
                            seq = seq.wrapping_add(1);
                            batch.clear();
                        }
                    }
                }
                if !batch.is_empty() && batch_since.elapsed() > GIVE_UP {
                    up.given_up();
                    batch.clear();
                    seq = seq.wrapping_add(1);
                }
            }
        }

        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            let c = up.counts;
            out::line(format_args!(
                r#"{{"t":"uplink","gw":{},"auth":"{}","heard":{},"beacons":{},"syncs":{},"cmds":{},"cmd_dups":{},"pkts":{},"frames":{},"failed":{},"dropped":{},"not_sent":{},"announced":{},"oversize":{}}}"#,
                up.gateway_known(),
                up.state_name(),
                c.heard,
                c.beacons,
                c.syncs,
                c.commands,
                c.rejects.dups,
                c.batches_sent,
                c.frames_sent,
                c.failed(),
                c.batches_given_up,
                c.not_sent,
                c.announcements,
                OVERSIZE.load(Relaxed),
            ));
            linkauth::say_rejects(&c, &say);
        }
        Timer::after(POLL).await;
    }
}
