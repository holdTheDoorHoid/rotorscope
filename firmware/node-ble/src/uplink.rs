//! ESP-NOW uplink (feature `espnow`): hub frames to the gateway instead of the
//! UART, and the hub's commands back. Same `airlink` scheme as node-wifi's
//! uplink (batches of whole `link` frames, unicast to the gateway learned
//! from its beacons, retried for 2 s, sequence numbers for the gateway's loss
//! count).
//!
//! The difference: this node does not hop. Wi-Fi is parked on the gateway's
//! channel (6) for good, and the C5's coexistence scheduler shares the one
//! radio between the Bluetooth scan and Wi-Fi. Wi-Fi only needs the antenna
//! to send a batch (a few ms, about twice a second) and to hear the gateway's
//! beacons and commands, so most of the time goes to Bluetooth. What it costs
//! the scan is measured in README.md "ESP-NOW".
//!
//! Frames come from `out::UART`, the byte pipe the UART task drains in a
//! wired build: whole COBS frames, split here on their zero delimiters.
//!
//! Built with the kit key (`tools/link.key`, docs/ESPNOW.md "Securing the
//! link"), batches are sealed and only gateway packets with a good tag and a
//! fresh stamp are believed (`airlink::auth`). Without a key it runs open,
//! as before, and says so at boot.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use airlink::auth;
use embassy_time::{Duration, Instant, Timer};
use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo};

use crate::out;

/// The gateway's channel (firmware/gateway `CHANNEL`).
pub const CHANNEL: u8 = 6;
const HOLD: Duration = Duration::from_millis(50);
const GIVE_UP: Duration = Duration::from_secs(2);
/// Forget a gateway neither heard (beacon) nor reached (ACKed send) for this
/// long. Generous: a node that is on channel 6 only 40 ms of a second (5 GHz
/// plan) or only 100 ms of it (Bluetooth) catches few of the twice-a-second
/// beacons, and each ACK proves the gateway is there just as well.
const GATEWAY_TIMEOUT: Duration = Duration::from_secs(30);
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
pub static PACKETS: AtomicU32 = AtomicU32::new(0);
pub static FRAMES: AtomicU32 = AtomicU32::new(0);
pub static FAILED: AtomicU32 = AtomicU32::new(0);
pub static DROPPED_BATCHES: AtomicU32 = AtomicU32::new(0);
pub static BEACONS: AtomicU32 = AtomicU32::new(0);
pub static COMMANDS: AtomicU32 = AtomicU32::new(0);
pub static COMMAND_DUPS: AtomicU32 = AtomicU32::new(0);
/// Frames longer than `link::MAX_FRAME` found in the pipe (dropped).
pub static OVERSIZE: AtomicU32 = AtomicU32::new(0);
pub static SEND_US: AtomicU32 = AtomicU32::new(0);

type Frame = heapless::Vec<u8, { link::MAX_FRAME }>;

/// A fresh random session per boot (hardware RNG; true random with the radio on).
fn rng() -> u32 {
    esp_hal::rng::Rng::new().random()
}

/// One JSON line on USB.
fn usb_line(args: core::fmt::Arguments<'_>) {
    let mut s: heapless::String<256> = heapless::String::new();
    let _ = s.write_fmt(args);
    let _ = s.push('\n');
    out::emit(&out::USB, s.as_bytes(), &crate::adv::COUNTERS.usb_drops);
}

/// `src` is the gateway: add it as a peer the first time, and note it heard.
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
        let mut s: heapless::String<96> = heapless::String::new();
        let _ = write!(
            s,
            r#"{{"t":"uplink","gateway":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","ch":{CHANNEL}}}"#,
            src[0], src[1], src[2], src[3], src[4], src[5]
        );
        let _ = s.push('\n');
        out::emit(&out::USB, s.as_bytes(), &crate::adv::COUNTERS.usb_drops);
    }
    *gateway = Some((src, Instant::now()));
}

#[embassy_executor::task]
pub async fn run(mut now: EspNow) {
    let me = esp_radio::wifi::Interface::station().mac_address();
    let mut auth = auth::Key::kit().map(|k| auth::Node::new(k, rng));
    let mut rejects = auth::NodeRejects::default();
    if auth.is_some() {
        usb_line(format_args!(r#"{{"t":"uplink","auth":"{}"}}"#, auth::KEY_TAG));
    } else {
        usb_line(format_args!(
            r#"{{"t":"warn","what":"link_key","auth":"{}","note":"built WITHOUT a link key: ESP-NOW reports and hub commands are not authenticated (docs/ESPNOW.md 'Securing the link')"}}"#,
            auth::KEY_TAG
        ));
    }
    let mut batch = airlink::Batch::new();
    let mut batch_since = Instant::now();
    let mut seq: u16 = 0;
    let mut gateway: Option<([u8; 6], Instant)> = None;
    let mut last_stats = Instant::now();
    let mut cmd_seq: Option<([u8; 6], u16)> = None;
    // The frame being assembled from the pipe, and whole frames waiting for
    // room in a batch.
    let mut acc = Frame::new();
    let mut acc_bad = false;
    let mut queue: heapless::Deque<Frame, 8> = heapless::Deque::new();
    let mut chunk = [0u8; 256];
    let mut line = [0u8; 320];

    loop {
        while let Some(got) = now.receive() {
            let Some(pkt) = airlink::parse(got.data()) else { continue };
            let src = got.info.src_address;
            if pkt.kind == airlink::Kind::Frames {
                continue; // another node's batch
            }
            // Keyed: tag and freshness before anything else looks at it.
            let v = auth::node_accept(auth.as_mut(), &src, got.data(), pkt.kind, Instant::now().as_millis());
            // A genuine beacon of another epoch (the gateway rebooted) still
            // names our gateway: sending to it earns the sync that moves us
            // to its new epoch. Nothing else is taken from it.
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
                        COMMAND_DUPS.fetch_add(1, Relaxed);
                        continue;
                    }
                    cmd_seq = Some((src, pkt.seq));
                    COMMANDS.fetch_add(1, Relaxed);
                    // Commands come from the gateway too. The scan leaves Wi-Fi
                    // only short gaps, and the gateway's unicast commands (radio
                    // retries) get through them far more often than its
                    // broadcast beacons: on the bench, commands but no beacon
                    // for over a minute.
                    learn(&now, &mut gateway, src);
                    let mut dec = link::CommandDecoder::new();
                    for f in airlink::frames(&pkt) {
                        dec.feed(f, |r| {
                            if let Ok(link::Command::Assign(a)) = r {
                                let _ = crate::role::COMMANDS.try_send(a);
                            }
                        });
                    }
                }
                airlink::Kind::Frames => {} // another node's batch
            }
        }
        if gateway.is_some_and(|(_, t)| t.elapsed() > GATEWAY_TIMEOUT) {
            gateway = None;
        }

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
            if let Some((mac, _)) = gateway {
                TRIES.fetch_add(1, Relaxed);
                let t0 = Instant::now();
                let frames = batch.frames();
                // Keyed: no stamp until a gateway epoch is known (or after
                // a counter wrap, until a newer gateway clock).
                let r = match auth.as_mut() {
                    None => now.send_async(&mac, batch.packet(seq)).await,
                    Some(a) => match a.next_up() {
                        Some(st) => now.send_async(&mac, batch.sealed(seq, &a.key, &me, st)).await,
                        None => Err(esp_radio::esp_now::EspNowError::SendFailed),
                    },
                };
                SEND_US.fetch_add(t0.elapsed().as_micros() as u32, Relaxed);
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
            out::json_line(
                &out::UplinkLine {
                    t: "uplink",
                    gw: gateway.is_some(),
                    beacons: BEACONS.load(Relaxed),
                    cmds: COMMANDS.load(Relaxed),
                    cmd_dups: COMMAND_DUPS.load(Relaxed),
                    pkts: PACKETS.load(Relaxed),
                    frames: FRAMES.load(Relaxed),
                    failed: FAILED.load(Relaxed),
                    dropped: DROPPED_BATCHES.load(Relaxed),
                    oversize: OVERSIZE.load(Relaxed),
                    gaps: GAPS.load(Relaxed),
                    tries: TRIES.load(Relaxed),
                    send_us: SEND_US.load(Relaxed),
                },
                &mut line,
            );
            if rejects != auth::NodeRejects::default() {
                let r = rejects.by_reason;
                usb_line(format_args!(
                    r#"{{"t":"link_auth","bad_tag":{},"replay":{},"unauth":{},"stale":{},"late":{},"dups":{}}}"#,
                    r[0], r[1], r[2], r[3], r[4], rejects.dups
                ));
            }
        }
        Timer::after(POLL).await;
    }
}
