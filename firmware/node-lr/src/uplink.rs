//! ESP-NOW uplink (feature `espnow`): hub frames to the gateway, the hub's
//! commands back. The same `airlink` scheme as node-ble's uplink (batches
//! of whole `link` frames, unicast to the gateway learned from its beacons,
//! sealed with the kit key when the build has one), simplified: this node's
//! Wi-Fi radio is its own (the LR2021 does the listening), so Wi-Fi stays
//! on the gateway's channel and may send at any time.
//!
//! Built with the kit key (`tools/link.key`, docs/ESPNOW.md "Securing the
//! link"), batches are sealed and only gateway packets with a good tag and
//! a fresh stamp are believed. Without a key it runs open and says so at
//! boot, as every other node does.

use airlink::auth;
use embassy_time::{Duration, Instant, Timer};
use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo};
use portable_atomic::{AtomicU32, Ordering::Relaxed};

use crate::out::{self, COUNTERS};
use crate::state;

/// The gateway's channel (firmware/gateway `CHANNEL`).
pub const CHANNEL: u8 = 6;
const HOLD: Duration = Duration::from_millis(50);
const GIVE_UP: Duration = Duration::from_secs(2);
const GATEWAY_TIMEOUT: Duration = Duration::from_secs(30);
/// Receive polling period; also the resolution of a `TimeSync` arrival
/// stamp (ms-level jitter is harmless next to 400 ms slots).
const POLL: Duration = Duration::from_millis(2);

pub static PACKETS: AtomicU32 = AtomicU32::new(0);
pub static FRAMES: AtomicU32 = AtomicU32::new(0);
pub static FAILED: AtomicU32 = AtomicU32::new(0);
pub static DROPPED_BATCHES: AtomicU32 = AtomicU32::new(0);
pub static BEACONS: AtomicU32 = AtomicU32::new(0);
pub static COMMANDS: AtomicU32 = AtomicU32::new(0);
pub static OVERSIZE: AtomicU32 = AtomicU32::new(0);

type Frame = heapless::Vec<u8, { link::MAX_FRAME }>;

fn rng() -> u32 {
    esp_hal::rng::Rng::new().random()
}

fn learn(now: &EspNow, gateway: &mut Option<([u8; 6], Instant)>, src: [u8; 6]) {
    if gateway.is_none_or(|(m, _)| m != src) {
        if !now.peer_exists(&src) {
            let _ = now.add_peer(PeerInfo {
                interface: EspNowWifiInterface::Station,
                peer_address: src,
                lmk: None,
                channel: None,
                encrypt: false,
            });
        }
        out::line(format_args!(
            r#"{{"t":"uplink","gateway":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","ch":{CHANNEL}}}"#,
            src[0], src[1], src[2], src[3], src[4], src[5]
        ));
    }
    *gateway = Some((src, Instant::now()));
}

#[embassy_executor::task]
pub async fn run(mut now: EspNow) {
    let me = esp_radio::wifi::Interface::station().mac_address();
    let mut auth = auth::Key::kit().map(|k| auth::Node::new(k, rng));
    let mut rejects = auth::NodeRejects::default();
    if auth.is_some() {
        out::line(format_args!(r#"{{"t":"uplink","auth":"{}"}}"#, auth::KEY_TAG));
    } else {
        out::line(format_args!(
            r#"{{"t":"warn","what":"link_key","auth":"{}","note":"built WITHOUT a link key: ESP-NOW reports and hub commands are not authenticated (docs/ESPNOW.md 'Securing the link')"}}"#,
            auth::KEY_TAG
        ));
    }
    let mut batch = airlink::Batch::new();
    let mut batch_since = Instant::now();
    let mut seq: u16 = 0;
    let mut gateway: Option<([u8; 6], Instant)> = None;
    let mut cmd_seq: Option<([u8; 6], u16)> = None;
    let mut acc = Frame::new();
    let mut acc_bad = false;
    let mut queue: heapless::Deque<Frame, 8> = heapless::Deque::new();
    let mut chunk = [0u8; 256];
    let mut last_stats = Instant::now();

    loop {
        while let Some(got) = now.receive() {
            let at_us = state::now_us();
            let Some(pkt) = airlink::parse(got.data()) else { continue };
            let src = got.info.src_address;
            if pkt.kind == airlink::Kind::Frames {
                continue; // another node's batch
            }
            let v = auth::node_accept(auth.as_mut(), &src, got.data(), pkt.kind, Instant::now().as_millis());
            let beacon_ok = pkt.kind == airlink::Kind::Beacon && auth::proves_gateway(v);
            if !rejects.take(v) && !beacon_ok {
                continue;
            }
            match pkt.kind {
                airlink::Kind::Beacon => {
                    BEACONS.fetch_add(1, Relaxed);
                    learn(&now, &mut gateway, src);
                }
                airlink::Kind::Command => {
                    if cmd_seq == Some((src, pkt.seq)) {
                        continue; // a radio retry we already have
                    }
                    cmd_seq = Some((src, pkt.seq));
                    COMMANDS.fetch_add(1, Relaxed);
                    learn(&now, &mut gateway, src);
                    let mut dec = link::CommandDecoder::new();
                    for f in airlink::frames(&pkt) {
                        dec.feed(f, |r| {
                            if let Ok(c) = r {
                                state::on_command(c, at_us);
                            }
                        });
                    }
                }
                airlink::Kind::Frames => {}
            }
        }
        if gateway.is_some_and(|(_, t)| t.elapsed() > GATEWAY_TIMEOUT) {
            gateway = None;
        }

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
            if let Some((mac, _)) = gateway {
                let frames = batch.frames();
                let r = match auth.as_mut() {
                    None => now.send_async(&mac, batch.packet(seq)).await,
                    Some(a) => match a.next_up() {
                        Some(st) => now.send_async(&mac, batch.sealed(seq, &a.key, &me, st)).await,
                        None => Err(esp_radio::esp_now::EspNowError::SendFailed),
                    },
                };
                if r.is_ok() {
                    gateway = Some((mac, Instant::now()));
                    seq = seq.wrapping_add(1);
                    PACKETS.fetch_add(1, Relaxed);
                    FRAMES.fetch_add(u32::from(frames), Relaxed);
                    batch.clear();
                } else {
                    FAILED.fetch_add(1, Relaxed);
                }
            }
            if !batch.is_empty() && batch_since.elapsed() > GIVE_UP {
                DROPPED_BATCHES.fetch_add(1, Relaxed);
                batch.clear();
                seq = seq.wrapping_add(1);
            }
        }

        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            out::line(format_args!(
                r#"{{"t":"uplink","gw":{},"beacons":{},"cmds":{},"pkts":{},"frames":{},"failed":{},"dropped":{},"oversize":{}}}"#,
                gateway.is_some(),
                BEACONS.load(Relaxed),
                COMMANDS.load(Relaxed),
                PACKETS.load(Relaxed),
                FRAMES.load(Relaxed),
                FAILED.load(Relaxed),
                DROPPED_BATCHES.load(Relaxed),
                OVERSIZE.load(Relaxed),
            ));
            if rejects != auth::NodeRejects::default() {
                let r = rejects.by_reason;
                out::line(format_args!(
                    r#"{{"t":"link_auth","bad_tag":{},"replay":{},"unauth":{},"stale":{},"late":{},"dups":{}}}"#,
                    r[0], r[1], r[2], r[3], r[4], rejects.dups
                ));
            }
        }
        Timer::after(POLL).await;
    }
}
