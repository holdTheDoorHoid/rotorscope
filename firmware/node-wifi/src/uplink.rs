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

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use embassy_time::{Duration, Instant, Timer};
use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo, PhyMode, RateConfig, WifiPhyRate};

use crate::report::HUB_QUEUE;
use crate::rx::TUNED_CHANNEL;

/// Batch window. Longer packs more frames per packet (less airtime overhead)
/// but delays every detection by up to this much.
const HOLD: Duration = Duration::from_millis(50);
/// Retry a failed batch for this long, then drop it.
const GIVE_UP: Duration = Duration::from_secs(2);
/// Forget a gateway not heard from for this long (stop sending).
/// Forget a gateway neither heard (beacon) nor reached (ACKed send) for this
/// long. Generous: a node that is on channel 6 only 40 ms of a second (5 GHz
/// plan) or only 100 ms of it (Bluetooth) catches few of the twice-a-second
/// beacons, and each ACK proves the gateway is there just as well.
const GATEWAY_TIMEOUT: Duration = Duration::from_secs(30);
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

pub static PACKETS: AtomicU32 = AtomicU32::new(0);
pub static FRAMES: AtomicU32 = AtomicU32::new(0);
pub static BYTES: AtomicU32 = AtomicU32::new(0);
/// Sends the radio reported as not delivered (no ACK after its retries).
pub static FAILED: AtomicU32 = AtomicU32::new(0);
/// Batches dropped after GIVE_UP (or for want of a gateway).
pub static DROPPED_BATCHES: AtomicU32 = AtomicU32::new(0);
pub static BEACONS: AtomicU32 = AtomicU32::new(0);
/// Microseconds spent waiting for sends to complete (radio busy transmitting).
pub static SEND_US: AtomicU32 = AtomicU32::new(0);

/// Command packets from the gateway, and repeats of one already taken (a
/// retry whose ACK was lost).
pub static COMMANDS: AtomicU32 = AtomicU32::new(0);
pub static COMMAND_DUPS: AtomicU32 = AtomicU32::new(0);

/// A command packet: drop a repeat (same gateway, same sequence number), else
/// decode its `link::Command`s and queue each `Assign` for `role::control`.
fn on_command(last: &mut Option<([u8; 6], u16)>, src: [u8; 6], pkt: &airlink::Packet<'_>) {
    if *last == Some((src, pkt.seq)) {
        COMMAND_DUPS.fetch_add(1, Relaxed);
        return;
    }
    *last = Some((src, pkt.seq));
    COMMANDS.fetch_add(1, Relaxed);
    let mut dec = link::CommandDecoder::new();
    for f in airlink::frames(pkt) {
        dec.feed(f, |r| {
            if let Ok(link::Command::Assign(a)) = r {
                // Full: an older command is still queued; the hub resends.
                let _ = crate::role::COMMANDS.try_send(a);
            }
        });
    }
}

/// Called by the hopper before each retune.
pub async fn wait_idle() {
    let t = Instant::now();
    while BUSY.load(Relaxed) && t.elapsed() < Duration::from_millis(30) {
        Timer::after(Duration::from_millis(1)).await;
    }
}

#[embassy_executor::task]
pub async fn run(mut now: EspNow) {
    let mut batch = airlink::Batch::new();
    let mut batch_since = Instant::now();
    let mut seq: u16 = 0;
    let mut gateway: Option<([u8; 6], u8, Instant)> = None;
    let mut last_stats = Instant::now();
    if RATE.is_some_and(|r| r.starts_with("LR")) {
        let e = enable_lr();
        esp_println::println!(r#"{{"t":"uplink","lr_protocol":{}}}"#, e == 0);
    }
    let mut pending: Option<heapless::Vec<u8, { link::MAX_FRAME }>> = None;
    let mut cmd_seq: Option<([u8; 6], u16)> = None;

    loop {
        // Beacons: learn (or refresh) the gateway. Commands: hand to `role`.
        while let Some(got) = now.receive() {
            let Some(pkt) = airlink::parse(got.data()) else { continue };
            if pkt.kind == airlink::Kind::Command {
                on_command(&mut cmd_seq, got.info.src_address, &pkt);
                // From the gateway: proof it is there (a 5 GHz plan is on 6
                // only 40 ms at a time and may miss its beacons for a while).
                if let Some((m, ch, _)) = gateway
                    && m == got.info.src_address
                {
                    gateway = Some((m, ch, Instant::now()));
                }
                continue;
            }
            let Some(ch) = airlink::beacon_channel(&pkt) else { continue };
            BEACONS.fetch_add(1, Relaxed);
            let mac = got.info.src_address;
            if gateway.is_none_or(|(m, _, _)| m != mac) {
                if !now.peer_exists(&mac) {
                    let _ = now.add_peer(PeerInfo {
                        interface: EspNowWifiInterface::Station,
                        peer_address: mac,
                        lmk: None,
                        channel: None,
                        encrypt: false,
                    });
                }
                let rate_ok = match RATE.map(|r| (r, rate_config(r))) {
                    None => "default",
                    Some((_, None)) => "unknown",
                    Some((_, Some(cfg))) => {
                        if now.set_peer_rate(&mac, cfg).is_ok() { "set" } else { "refused" }
                    }
                };
                esp_println::println!(
                    r#"{{"t":"uplink","gateway":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","ch":{ch},"rate":"{}","rate_cfg":"{rate_ok}"}}"#,
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], RATE.unwrap_or("1M")
                );
            }
            gateway = Some((mac, ch, Instant::now()));
        }
        if gateway.is_some_and(|(_, _, t)| t.elapsed() > GATEWAY_TIMEOUT) {
            gateway = None;
        }

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

        let due = !batch.is_empty() && (pending.is_some() || batch_since.elapsed() >= HOLD);
        if due {
            match gateway {
                Some((mac, ch, _)) if TUNED_CHANNEL.load(Relaxed) == ch => {
                    BUSY.store(true, Relaxed);
                    let t0 = Instant::now();
                    let frames = batch.frames();
                    let len = batch.payload_len();
                    let r = now.send_async(&mac, batch.packet(seq)).await;
                    BUSY.store(false, Relaxed);
                    SEND_US.fetch_add(t0.elapsed().as_micros() as u32, Relaxed);
                    if r.is_ok() {
                        gateway = Some((mac, ch, Instant::now()));
                        // Only on success: a retry of a batch whose ACK was lost
                        // reuses the number, so the gateway drops the duplicate.
                        seq = seq.wrapping_add(1);
                        PACKETS.fetch_add(1, Relaxed);
                        FRAMES.fetch_add(u32::from(frames), Relaxed);
                        BYTES.fetch_add(len as u32, Relaxed);
                        batch.clear();
                    } else {
                        FAILED.fetch_add(1, Relaxed);
                    }
                }
                _ => {}
            }
            if !batch.is_empty() && batch_since.elapsed() > GIVE_UP {
                DROPPED_BATCHES.fetch_add(1, Relaxed);
                batch.clear();
                // The gateway may have had it after all; never reuse its number.
                seq = seq.wrapping_add(1);
            }
        }

        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            esp_println::println!(
                r#"{{"t":"uplink","gw":{},"beacons":{},"cmds":{},"cmd_dups":{},"pkts":{},"frames":{},"bytes":{},"failed":{},"dropped":{},"send_us":{}}}"#,
                gateway.is_some(),
                BEACONS.load(Relaxed),
                COMMANDS.load(Relaxed),
                COMMAND_DUPS.load(Relaxed),
                PACKETS.load(Relaxed),
                FRAMES.load(Relaxed),
                BYTES.load(Relaxed),
                FAILED.load(Relaxed),
                DROPPED_BATCHES.load(Relaxed),
                SEND_US.load(Relaxed)
            );
        }
        Timer::after(POLL).await;
    }
}
