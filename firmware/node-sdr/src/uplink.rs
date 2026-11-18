//! ESP-NOW hub uplink (feature `espnow`): the only transmitter in node-sdr.
//!
//! The radio is the capture front end, so it cannot listen and send at
//! once. Between captures (at a hop boundary when the node is walking a
//! plan) the main loop opens a short **uplink window**:
//!
//! 1. stop the ring and the dump engine (the capture path is idle);
//! 2. restart the Wi-Fi driver (`esp_wifi_stop` + `esp_wifi_start`), which
//!    hands the PHY back to the driver in its ordinary state: our forced
//!    gain, disabled AGC, forced-off TX chain and stopped queues are all
//!    undone by the driver's own PHY bring-up, through documented calls;
//! 3. tune to the gateway's channel (2.4 GHz ch 6), learn the gateway from its
//!    beacon if needed, send one `airlink` batch of queued link frames
//!    (unicast: the radio's ACK and retries apply), and listen
//!    [`LINGER`] for the gateway's reply (a hub `Command::Assign`, which the
//!    gateway sends as soon as it hears from a node);
//! 4. give the radio back to the capture path ([`Radio::resume`]): tune,
//!    re-measure the DC offset, force gain and AGC off, power the TX chain
//!    down, stop the MAC transmit queues and **read them back**. A window
//!    whose read-back is not receive-only is counted (`relock_bad`) and the
//!    capture loop refuses to run until a clean re-lock (see `main`).
//!
//! Outside a window nothing here runs and no frame is queued to the MAC.
//! Cost per window: see README.md "Hub link" (measured on c5b).
//!
//! Built with the kit key (`tools/link.key`, docs/ESPNOW.md "Securing the
//! link"), batches are sealed and only gateway packets with a good tag and a
//! fresh stamp are believed (`airlink::auth`): a forged or replayed `Assign`
//! cannot retune the capture. Without a key it runs open, as before, and says
//! so at boot.

use airlink::auth;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_println::println;
use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo};
use esp_wifi_sys_esp32c5::include as sys;
use link::Assignment;

use crate::hub::Hub;
use crate::radio::Radio;

/// The gateway's channel (firmware/gateway, docs/ESPNOW.md).
pub const CHANNEL: u8 = 6;
/// Fewest microseconds between windows when only sightings are waiting
/// (keepalives come every 5 s anyway).
const MIN_GAP_US: u64 = 1_000_000;
/// How long to listen for a beacon when no gateway is known (it beacons
/// every 500 ms).
const DISCOVER: Duration = Duration::from_millis(1200);
/// Forget the gateway after this long without a beacon or an ACK.
const GATEWAY_STALE_US: u64 = 30_000_000;
/// Listen after a send for the gateway's command reply.
const LINGER: Duration = Duration::from_millis(40);
/// Longest wait for one send's ACK (or failure) report.
const SEND_TIMEOUT: Duration = Duration::from_millis(300);
/// A batch is retried in this many windows, then dropped.
const TRIES: u8 = 3;

fn now_us() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_micros()
}

#[derive(Default, Clone, Copy)]
pub struct Counters {
    pub windows: u32,
    pub sent: u32,
    pub acked: u32,
    pub failed: u32,
    pub frames: u32,
    pub dropped_batches: u32,
    pub beacons: u32,
    pub cmd_pkts: u32,
    pub no_gateway: u32,
    pub relock_bad: u32,
    pub restart_err: u32,
    pub us_total: u64,
    pub us_max: u32,
    pub us_last: u32,
    /// Gateway packets not taken (keyed builds), by reason.
    pub auth: auth::NodeRejects,
}

/// A fresh random session per boot (hardware RNG; true random with the radio on).
fn rng() -> u32 {
    esp_hal::rng::Rng::new().random()
}

pub struct Uplink {
    now: EspNow,
    gw: Option<[u8; 6]>,
    gw_seen_us: u64,
    seq: u16,
    batch: airlink::Batch,
    batch_tries: u8,
    last_us: u64,
    last_cmd_seq: Option<u16>,
    /// Keyed builds: session, counter and gateway epoch; `None` = open.
    auth: Option<auth::Node>,
    me: [u8; 6],
    pub c: Counters,
}

impl Uplink {
    pub fn new(now: EspNow) -> Self {
        let auth = auth::Key::kit().map(|k| auth::Node::new(k, rng));
        if auth.is_some() {
            println!(r#"{{"t":"uplink","auth":"{}"}}"#, auth::KEY_TAG);
        } else {
            println!(
                r#"{{"t":"warn","what":"link_key","auth":"{}","note":"built WITHOUT a link key: ESP-NOW reports and hub commands are not authenticated (docs/ESPNOW.md 'Securing the link')"}}"#,
                auth::KEY_TAG
            );
        }
        Uplink {
            auth,
            me: esp_radio::wifi::Interface::station().mac_address(),
            now,
            gw: None,
            gw_seen_us: 0,
            seq: 0,
            batch: airlink::Batch::new(),
            batch_tries: 0,
            last_us: 0,
            last_cmd_seq: None,
            c: Counters::default(),
        }
    }

    /// Something to send, and the last window long enough ago.
    pub fn due(&self, hub: &Hub) -> bool {
        (hub.len() > 0 || !self.batch.is_empty()) && now_us() >= self.last_us + MIN_GAP_US
    }

    /// Handle everything received so far: beacons (learn the gateway) and
    /// command packets (hub assignments).
    fn drain_rx(&mut self, hub: &mut Hub, assign: &mut Option<Assignment>) {
        while let Some(got) = self.now.receive() {
            let Some(p) = airlink::parse(got.data()) else { continue };
            if p.kind == airlink::Kind::Frames {
                continue; // another node's batch
            }
            // Keyed: tag and freshness before anything else looks at it.
            let v = auth::node_accept(self.auth.as_mut(), &got.info.src_address, got.data(), p.kind, now_us() / 1000);
            // A genuine beacon of another epoch (the gateway rebooted) still
            // names our gateway: sending to it earns the sync that moves us
            // to its new epoch. Nothing else is taken from it.
            let beacon_ok = p.kind == airlink::Kind::Beacon && auth::proves_gateway(v);
            if !self.c.auth.take(v) && !beacon_ok {
                continue;
            }
            match p.kind {
                airlink::Kind::Beacon => {
                    self.c.beacons += 1;
                    let mac = got.info.src_address;
                    if self.gw != Some(mac) {
                        if !self.now.peer_exists(&mac) {
                            let _ = self.now.add_peer(PeerInfo {
                                interface: EspNowWifiInterface::Station,
                                peer_address: mac,
                                lmk: None,
                                channel: None,
                                encrypt: false,
                            });
                        }
                        println!(
                            r#"{{"t":"uplink","gateway":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","ch":{}}}"#,
                            mac[0],
                            mac[1],
                            mac[2],
                            mac[3],
                            mac[4],
                            mac[5],
                            airlink::beacon_channel(&p).unwrap_or(0)
                        );
                        self.gw = Some(mac);
                    }
                    self.gw_seen_us = now_us();
                }
                airlink::Kind::Command => {
                    self.c.cmd_pkts += 1;
                    if self.last_cmd_seq == Some(p.seq) {
                        continue; // a retry we already have
                    }
                    self.last_cmd_seq = Some(p.seq);
                    for f in airlink::frames(&p) {
                        if let Some(a) = hub.commands(f) {
                            *assign = Some(a);
                        }
                    }
                }
                airlink::Kind::Frames => {}
            }
        }
    }

    /// One uplink window; see the module docs. Returns a hub assignment if
    /// one arrived.
    pub async fn window(&mut self, radio: &mut Radio, hub: &mut Hub) -> Option<Assignment> {
        let t0 = now_us();
        self.c.windows += 1;
        radio.suspend();
        // SAFETY: plain calls into the running driver; nothing else uses it
        // while the main task is here.
        let (e_stop, e_start, e_ch) = unsafe {
            sys::esp_wifi_set_promiscuous(false);
            let a = sys::esp_wifi_stop();
            let b = sys::esp_wifi_start();
            let c = sys::esp_wifi_set_channel(CHANNEL, sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE);
            (a, b, c)
        };
        if e_stop != 0 || e_start != 0 || e_ch != 0 {
            self.c.restart_err += 1;
            println!(r#"{{"t":"err","what":"uplink_wifi","e":[{e_stop},{e_start},{e_ch}]}}"#);
        }
        let mut assign = None;
        self.drain_rx(hub, &mut assign);
        if self.gw.is_none() || now_us() > self.gw_seen_us + GATEWAY_STALE_US {
            self.gw = None;
            let until = Instant::now() + DISCOVER;
            while self.gw.is_none() && Instant::now() < until {
                Timer::after(Duration::from_millis(5)).await;
                self.drain_rx(hub, &mut assign);
            }
        }
        if let Some(gw) = self.gw {
            while let Some(f) = hub.front() {
                if !self.batch.fits(f) {
                    break;
                }
                let f = hub.pop().unwrap_or_default();
                self.batch.push(&f);
            }
            // Keyed: no stamp until a gateway epoch is known (or after a
            // counter wrap, until a newer gateway clock): try next window.
            let stamp = self.auth.as_mut().map(|a| a.next_up());
            if !self.batch.is_empty() && stamp != Some(None) {
                let frames = self.batch.frames();
                let pkt = match (&self.auth, stamp.flatten()) {
                    (Some(a), Some(st)) => self.batch.sealed(self.seq, &a.key, &self.me, st),
                    _ => self.batch.packet(self.seq),
                };
                self.c.sent += 1;
                match with_timeout(SEND_TIMEOUT, self.now.send_async(&gw, pkt)).await {
                    Ok(Ok(())) => {
                        self.c.acked += 1;
                        self.c.frames += frames as u32;
                        self.seq = self.seq.wrapping_add(1);
                        self.batch.clear();
                        self.batch_tries = 0;
                        self.gw_seen_us = now_us();
                    }
                    _ => {
                        self.c.failed += 1;
                        self.batch_tries += 1;
                        if self.batch_tries >= TRIES {
                            self.c.dropped_batches += 1;
                            self.batch.clear();
                            self.batch_tries = 0;
                            // The gateway may have had it; never reuse its number.
                            self.seq = self.seq.wrapping_add(1);
                        }
                    }
                }
            }
            let until = Instant::now() + LINGER;
            while Instant::now() < until {
                Timer::after(Duration::from_millis(2)).await;
                self.drain_rx(hub, &mut assign);
            }
        } else {
            self.c.no_gateway += 1;
        }
        // Back to capture: promiscuous with an empty filter keeps the receive
        // path powered without buffering frames (as at boot).
        // SAFETY: as above.
        unsafe {
            let filter = sys::wifi_promiscuous_filter_t { filter_mask: 0 };
            sys::esp_wifi_set_promiscuous_filter(&filter);
            sys::esp_wifi_set_promiscuous(true);
        }
        let us = (now_us() - t0) as u32;
        self.c.us_total += us as u64;
        self.c.us_max = self.c.us_max.max(us);
        self.c.us_last = us;
        self.last_us = now_us();
        // Printed while the ring is still stopped (it is ~250 bytes).
        self.print();
        if !radio.resume() {
            self.c.relock_bad += 1;
            if !radio.resume() {
                radio.halt();
                println!(r#"{{"t":"err","what":"relock","note":"receiver not back in its receive-only state twice: capture halted (mode idle)"}}"#);
            }
        }
        assign
    }

    pub fn print(&self) {
        let c = &self.c;
        println!(
            r#"{{"t":"uplink","gw":{},"windows":{},"sent":{},"acked":{},"failed":{},"frames":{},"dropped":{},"beacons":{},"cmd_pkts":{},"no_gw":{},"relock_bad":{},"restart_err":{},"us_last":{},"us_max":{},"us_avg":{},"auth_rej":[{},{},{},{},{}]}}"#,
            self.gw.is_some(),
            c.windows,
            c.sent,
            c.acked,
            c.failed,
            c.frames,
            c.dropped_batches,
            c.beacons,
            c.cmd_pkts,
            c.no_gateway,
            c.relock_bad,
            c.restart_err,
            c.us_last,
            c.us_max,
            if c.windows > 0 { c.us_total / c.windows as u64 } else { 0 },
            c.auth.by_reason[0],
            c.auth.by_reason[1],
            c.auth.by_reason[2],
            c.auth.by_reason[3],
            c.auth.by_reason[4]
        );
    }
}
