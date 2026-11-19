//! ESP-NOW uplink (feature `espnow`): hub frames to the gateway instead of the
//! UART, and the hub's commands back. Same `airlink` scheme as node-wifi's
//! uplink (batches of whole `link` frames, unicast to the gateway learned
//! from its beacons, retried for 2 s, sequence numbers for the gateway's loss
//! count).
//!
//! The difference: this node does not hop. Wi-Fi is parked on the gateway's
//! channel (`airlink::GATEWAY_CHANNEL`) for good, and the C5's coexistence
//! scheduler shares the one radio between the Bluetooth scan and Wi-Fi. Wi-Fi
//! only needs the antenna to send a batch (a few ms, about twice a second)
//! and to hear the gateway's beacons and commands, so most of the time goes
//! to Bluetooth. What it costs the scan is measured in README.md "ESP-NOW".
//! Parked on the gateway channel, this node cannot be made deaf by a plan:
//! it has no deaf fall-back to run.
//!
//! Frames come from `out::UART`, the byte pipe the UART task drains in a
//! wired build: whole COBS frames, split here on their zero delimiters.
//!
//! The link (docs/LINK-KEYS-DESIGN.md, package 13g) is the shared glue in
//! `node_common::linkauth` / `nodeimage::uplink`: the key comes from the
//! board's `rskey` partition, never from the image. Keyed, every attempt is
//! sealed afresh and only gateway packets the link verifies are believed;
//! an open record runs the old unauthenticated link; an unprovisioned node
//! sends no reports, only a warning and an `Unprovisioned` announcement
//! every 10 s.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use embassy_time::{Duration, Instant, Timer};
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow, EspNowWifiInterface, PeerInfo};
use node_common::linkauth::{self, Led, Rx};

use crate::out;

const HOLD: Duration = Duration::from_millis(50);
const GIVE_UP: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(5);

/// Time-sharing the radio: the Bluetooth scan runs for `SCAN_ON_MS` (the
/// host then disables it), then stays off for `WIFI_GAP_MS`, during
/// which [`WIFI_TURN`] is set and batches go out. With both running at once,
/// the coexistence scheduler gave Wi-Fi no receive time at all on the bench
/// (zero gateway beacons heard); with 900/100 and the host cancelling the
/// scan, the node often went a minute without hearing the gateway. 800/200
/// with a controller-timed stop: a send in every gap, none failed, ~0.4
/// beacons a second heard, for ~18% fewer adverts (README.md "ESP-NOW").
/// The controller-timed stop then crashed the controller under Long Range
/// traffic (2026-11-15), so the host stops the scan again: 800/200 with a
/// host stop, 180 s with the test beacon's three BLE transports on: 0 resets,
/// 178 gaps, 220 sends, 0 failed, 44 gateway beacons and 30 hub commands heard.
pub const SCAN_ON_MS: u64 = 800;
pub const WIFI_GAP_MS: u64 = 200;
pub static WIFI_TURN: AtomicBool = AtomicBool::new(false);

/// Wi-Fi gaps given by the scan loop, and batches tried in them.
pub static GAPS: AtomicU32 = AtomicU32::new(0);
pub static TRIES: AtomicU32 = AtomicU32::new(0);
/// Frames longer than `link::MAX_FRAME` found in the pipe (dropped).
pub static OVERSIZE: AtomicU32 = AtomicU32::new(0);
pub static SEND_US: AtomicU32 = AtomicU32::new(0);

type Frame = heapless::Vec<u8, { link::MAX_FRAME }>;

/// One JSON line on USB.
fn say(l: &str) {
    let mut s: heapless::String<512> = heapless::String::new();
    let _ = s.push_str(l);
    let _ = s.push('\n');
    out::emit(&out::USB, s.as_bytes(), &crate::adv::COUNTERS.usb_drops);
}

#[embassy_executor::task]
pub async fn run(mut now: EspNow, ks: node_common::keystore::Keystore, mut led: Led) {
    // After `WifiController::new`: the random source is running now, or never.
    let mut up = linkauth::start(ks, say);
    let mut batch = airlink::Batch::new();
    let mut batch_since = Instant::now();
    let mut seq: u16 = 0;
    let mut last_stats = Instant::now();
    // The frame being assembled from the pipe, and whole frames waiting for
    // room in a batch.
    let mut acc = Frame::new();
    let mut acc_bad = false;
    let mut queue: heapless::Deque<Frame, 8> = heapless::Deque::new();
    let mut chunk = [0u8; 256];
    let mut line = [0u8; 448];
    let mut announce = [0u8; airlink::v3::UNPROVISIONED_LEN];

    loop {
        while let Some(got) = now.receive() {
            // Commands come from the gateway too. The scan leaves Wi-Fi only
            // short gaps, and the gateway's unicast commands (radio retries)
            // get through them far more often than its broadcast beacons:
            // on the bench, commands but no beacon for over a minute. The
            // link learns the gateway from any packet it verified.
            if let Rx::Commands(p) = up.receive(&got.info.src_address, got.data(), linkauth::now_ms()) {
                linkauth::uplink::commands(p, |c| {
                    if let link::Command::Assign(a) = c {
                        let _ = crate::role::COMMANDS.try_send(a);
                    }
                });
            }
        }
        if let Some((mac, _)) = up.take_new_gateway() {
            if !now.peer_exists(&mac) {
                let _ = now.add_peer(PeerInfo { interface: EspNowWifiInterface::Station, peer_address: mac, lmk: None, channel: None, encrypt: false });
            }
            let mut s: heapless::String<96> = heapless::String::new();
            let _ = write!(s, r#"{{"t":"uplink","gateway":"{}","ch":{}}}"#, linkauth::MacText(mac), airlink::GATEWAY_CHANNEL);
            say(&s);
        }
        linkauth::publish(&up);
        led.update(up.blocked().is_some());

        // Split the pipe into whole frames...
        while let Ok(n) = out::UART.try_read(&mut chunk) {
            for &b in &chunk[..n] {
                if acc_bad {
                    acc_bad = b != 0; // skip to the end of an oversized frame
                    continue;
                }
                if acc.push(b).is_err() {
                    OVERSIZE.fetch_add(1, Relaxed);
                    acc.clear();
                    acc_bad = b != 0;
                } else if b == 0 {
                    let f = core::mem::take(&mut acc);
                    if f.len() > 1 && queue.push_back(f).is_err() {
                        // The gateway is gone or slow: lose the newest.
                        crate::adv::bump(&crate::adv::COUNTERS.uart_drops);
                    }
                }
            }
            if queue.is_full() {
                break;
            }
        }

        if !up.may_send() {
            // No reports over ESP-NOW (counted); every 10 s, in a Wi-Fi gap,
            // the warning and one announcement.
            let n = queue.len() as u32;
            queue.clear();
            up.not_sent(n);
            if WIFI_TURN.load(Relaxed)
                && let Some(p) = linkauth::blocked_tick(&mut up, linkauth::IMAGE_BLE, "node-ble", crate::FIRMWARE, &mut announce, &say)
            {
                let _ = now.send_async(&BROADCAST_ADDRESS, p).await;
            }
        } else {
            // ...and pack as many as fit into the batch.
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
            if due && WIFI_TURN.load(Relaxed) {
                let t = linkauth::now_ms();
                if let Some((mac, _)) = up.gateway(t) {
                    let frames = batch.frames();
                    // Keyed: sealed afresh for this attempt; `None` until the
                    // node knows a hub epoch.
                    if let Some(pkt) = up.packet(&mut batch, seq, t) {
                        TRIES.fetch_add(1, Relaxed);
                        let t0 = Instant::now();
                        let acked = now.send_async(&mac, pkt).await.is_ok();
                        SEND_US.fetch_add(t0.elapsed().as_micros() as u32, Relaxed);
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
            out::json_line(
                &out::UplinkLine {
                    t: "uplink",
                    gw: up.gateway(linkauth::now_ms()).is_some(),
                    auth: up.state_name(),
                    heard: c.heard,
                    beacons: c.beacons,
                    syncs: c.syncs,
                    cmds: c.commands,
                    cmd_dups: c.rejects.dups,
                    pkts: c.batches_sent,
                    frames: c.frames_sent,
                    failed: c.failed(),
                    dropped: c.batches_given_up,
                    not_sent: c.not_sent,
                    announced: c.announcements,
                    oversize: OVERSIZE.load(Relaxed),
                    gaps: GAPS.load(Relaxed),
                    tries: TRIES.load(Relaxed),
                    send_us: SEND_US.load(Relaxed),
                },
                &mut line,
            );
            linkauth::say_rejects(&c, &say);
        }
        Timer::after(POLL).await;
    }
}
