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
//! 3. tune to the gateway's channel (`airlink::GATEWAY_CHANNEL`, 2.4 GHz
//!    ch 6), learn the gateway from its beacon if needed, send one `airlink`
//!    batch of queued link frames (unicast: the radio's ACK and retries
//!    apply), and listen [`LINGER`] for the gateway's reply (a hub
//!    `Command::Assign`, which the gateway sends as soon as it hears from a
//!    node);
//! 4. give the radio back to the capture path ([`Radio::resume`]): tune,
//!    re-measure the DC offset, force gain and AGC off, power the TX chain
//!    down, stop the MAC transmit queues and **read them back**. A window
//!    whose read-back is not receive-only is counted (`relock_bad`) and the
//!    capture loop refuses to run until a clean re-lock (see `main`).
//!
//! Outside a window nothing here runs and no frame is queued to the MAC.
//! Cost per window: see README.md "Hub link" (measured on c5b).
//!
//! The link (docs/LINK-KEYS-DESIGN.md, package 13g) is the shared glue in
//! `node_common::linkauth` / `nodeimage::uplink`: the key comes from the
//! board's `rskey` partition, never from the image. Keyed, every attempt is
//! sealed afresh and only gateway packets the link verifies are believed: a
//! forged or replayed `Assign` cannot retune the capture. An open record
//! runs the old unauthenticated link; an unprovisioned node sends no
//! reports, only a warning and an `Unprovisioned` announcement every 10 s
//! (from a window; the board's LED pad is the capture bus's, so it does not
//! blink).
//!
//! **Deaf fall-back** ([`Uplink::deaf_tick`], WP-13 section 7.2). A window
//! opens only at a hop boundary (or, parked, when frames wait), and a hub
//! plan's dwell can be up to 65.5 s (`u16` ms), longer than the 60 s the
//! link and the hub allow. So this node can be kept from the gateway by a
//! plan: with no gateway packet heard for 60 s on a hub plan it goes back
//! to its built-in plan, and a plan id that turns out to blame is refused
//! for a while (`node_common::DeafWatch`, as on node-wifi).

use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_println::println;
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow, EspNowWifiInterface, PeerInfo};
use esp_wifi_sys_esp32c5::include as sys;
use link::Assignment;
use node_common::linkauth::{self, Link, Rx};

use crate::hub::Hub;
use crate::radio::Radio;

/// Fewest microseconds between windows when only sightings are waiting
/// (keepalives come every 5 s anyway).
const MIN_GAP_US: u64 = 1_000_000;
/// How long to listen for a beacon when no gateway is known (it beacons
/// every 500 ms).
const DISCOVER: Duration = Duration::from_millis(1200);
/// Listen after a send for the gateway's command reply.
const LINGER: Duration = Duration::from_millis(40);
/// Longest wait for one send's ACK (or failure) report.
const SEND_TIMEOUT: Duration = Duration::from_millis(300);
/// A batch is retried in this many windows, then dropped.
const TRIES: u8 = 3;

fn now_us() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_micros()
}

fn say(l: &str) {
    println!("{l}");
}

#[derive(Default, Clone, Copy)]
pub struct Counters {
    pub windows: u32,
    pub no_gateway: u32,
    pub relock_bad: u32,
    pub restart_err: u32,
    pub us_total: u64,
    pub us_max: u32,
    pub us_last: u32,
}

pub struct Uplink {
    now: EspNow,
    seq: u16,
    batch: airlink::Batch,
    batch_tries: u8,
    last_us: u64,
    /// The link: mode (keyed / open / blocked), gateway, delivery accounting.
    link: Link,
    announce: [u8; airlink::v3::UNPROVISIONED_LEN],
    /// The deaf fall-back, and the link's `heard` count it last saw.
    deaf: node_common::DeafWatch,
    heard: u32,
    pub c: Counters,
}

impl Uplink {
    /// After the Wi-Fi controller started: the link takes the random
    /// source now, and prints its boot line.
    pub fn new(now: EspNow, ks: node_common::keystore::Keystore) -> Self {
        let link = linkauth::start(ks, say);
        Uplink {
            now,
            seq: 0,
            batch: airlink::Batch::new(),
            batch_tries: 0,
            last_us: 0,
            heard: link.counts.heard,
            link,
            announce: [0; airlink::v3::UNPROVISIONED_LEN],
            deaf: node_common::DeafWatch::new(linkauth::now_ms()),
            c: Counters::default(),
        }
    }

    /// Something to send (or, unprovisioned, to announce), and the last
    /// window long enough ago.
    pub fn due(&self, hub: &Hub) -> bool {
        (hub.len() > 0 || !self.batch.is_empty()) && now_us() >= self.last_us + MIN_GAP_US
    }

    /// Handle everything received so far: beacons and syncs (learn the
    /// gateway) and command packets (hub assignments).
    #[inline(never)]
    fn drain_rx(&mut self, hub: &mut Hub, assign: &mut Option<Assignment>) {
        while let Some(got) = self.now.receive() {
            if let Rx::Commands(p) = self.link.receive(&got.info.src_address, got.data(), linkauth::now_ms()) {
                for f in airlink::v3::frames(p) {
                    if let Some(a) = hub.commands(f) {
                        *assign = Some(a);
                    }
                }
            }
        }
        if let Some((mac, ch)) = self.link.take_new_gateway() {
            if !self.now.peer_exists(&mac) {
                let _ = self.now.add_peer(PeerInfo { interface: EspNowWifiInterface::Station, peer_address: mac, lmk: None, channel: None, encrypt: false });
            }
            println!(r#"{{"t":"uplink","gateway":"{}","ch":{ch}}}"#, linkauth::MacText(mac));
        }
        linkauth::publish(&self.link);
    }

    /// One uplink window; see the module docs. Returns a hub assignment if
    /// one arrived (already checked against the deaf fall-back's refusals).
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
            let c = sys::esp_wifi_set_channel(airlink::GATEWAY_CHANNEL, sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE);
            (a, b, c)
        };
        if e_stop != 0 || e_start != 0 || e_ch != 0 {
            self.c.restart_err += 1;
            println!(r#"{{"t":"err","what":"uplink_wifi","e":[{e_stop},{e_start},{e_ch}]}}"#);
        }
        let mut assign = None;
        if !self.link.may_send() {
            // Unprovisioned: nothing is sent or taken; the queued frames are
            // thrown away (counted), and every 10 s one announcement.
            let mut n = 0;
            while hub.pop().is_some() {
                n += 1;
            }
            self.batch.clear();
            self.link.not_sent(n);
            if let Some(p) = linkauth::blocked_tick(&mut self.link, linkauth::IMAGE_SDR, "node-sdr", env!("CARGO_PKG_VERSION"), &mut self.announce, &say) {
                let _ = with_timeout(SEND_TIMEOUT, self.now.send_async(&BROADCAST_ADDRESS, p)).await;
            }
        } else {
            self.drain_rx(hub, &mut assign);
            if self.link.gateway(linkauth::now_ms()).is_none() {
                let until = Instant::now() + DISCOVER;
                while self.link.gateway(linkauth::now_ms()).is_none() && Instant::now() < until {
                    Timer::after(Duration::from_millis(5)).await;
                    self.drain_rx(hub, &mut assign);
                }
            }
            if let Some((gw, _)) = self.link.gateway(linkauth::now_ms()) {
                while let Some(f) = hub.front() {
                    if !self.batch.fits(f) {
                        break;
                    }
                    let f = hub.pop().unwrap_or_default();
                    self.batch.push(&f);
                }
                if !self.batch.is_empty() {
                    let frames = self.batch.frames();
                    // Keyed: sealed afresh for this attempt; `None` until the
                    // node knows a hub epoch (try next window).
                    if let Some(pkt) = self.link.packet(&mut self.batch, self.seq, linkauth::now_ms()) {
                        let acked = matches!(with_timeout(SEND_TIMEOUT, self.now.send_async(&gw, pkt)).await, Ok(Ok(())));
                        self.link.sent(acked, frames, linkauth::now_ms());
                        if acked {
                            self.seq = self.seq.wrapping_add(1);
                            self.batch.clear();
                            self.batch_tries = 0;
                        }
                    }
                    if !self.batch.is_empty() {
                        self.batch_tries += 1;
                        if self.batch_tries >= TRIES {
                            self.link.given_up();
                            self.batch.clear();
                            self.batch_tries = 0;
                            // The gateway may have had it; never reuse its number.
                            self.seq = self.seq.wrapping_add(1);
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
        // A hub plan id the deaf fall-back blamed is refused for a while.
        assign.filter(|a| !self.refuses(a))
    }

    /// The deaf fall-back's refusal of a hub `Assign` (counted, said on USB).
    #[inline(never)]
    fn refuses(&mut self, a: &Assignment) -> bool {
        let now = linkauth::now_ms();
        if !self.deaf.refuse_assign(now, a.id) {
            return false;
        }
        node_common::set_deaf_counts(self.deaf.counts());
        println!(
            r#"{{"t":"err","what":"assign","why":"plan id made this node deaf","plan":{},"refused_s":{}}}"#,
            a.id,
            self.deaf.refused_for_ms(now, a.id) / 1000
        );
        true
    }

    /// Once a second from the capture loop: feed the deaf watch what the
    /// link heard, and on a fall-back return to the built-in plan (plan id
    /// 0). The link itself is left alone: it relinks by itself.
    #[inline(never)]
    pub fn deaf_tick(&mut self, radio: &mut Radio, hub: &mut Hub) {
        use node_common::DeafEvent;
        let now = linkauth::now_ms();
        let h = linkauth::heard();
        let mut events: [Option<DeafEvent>; 2] = [None, None];
        if h != self.heard {
            self.heard = h;
            events[0] = self.deaf.heard(now);
        }
        // A console-pinned job is the operator's, not the hub's.
        let plan = if radio.pinned { 0 } else { radio.plan_id };
        events[1] = self.deaf.check(now, plan);
        node_common::set_deaf_counts(self.deaf.counts());
        for e in events.into_iter().flatten() {
            match e {
                DeafEvent::FellBack { plan } => {
                    radio.builtin_plan();
                    hub.set(radio.role, radio.plan_id);
                    println!(
                        r#"{{"t":"deaf","verdict":"fell_back","plan":{plan},"note":"no gateway packet for {} s on this plan: built-in plan; gateway heard within {} s blames the plan"}}"#,
                        node_common::DEAF_MS / 1000,
                        node_common::PROBATION_MS / 1000
                    );
                }
                DeafEvent::Blamed { plan, refuse_ms } => {
                    println!(r#"{{"t":"deaf","verdict":"blamed","plan":{plan},"refused_s":{}}}"#, refuse_ms / 1000);
                }
                DeafEvent::Outage { plan } => {
                    println!(r#"{{"t":"deaf","verdict":"outage","plan":{plan},"note":"gateway away, plan not blamed"}}"#);
                }
            }
        }
    }

    #[inline(never)]
    pub fn print(&self) {
        let c = &self.c;
        let l = &self.link.counts;
        println!(
            r#"{{"t":"uplink","gw":{},"auth":"{}","windows":{},"sent":{},"acked":{},"failed":{},"frames":{},"dropped":{},"not_sent":{},"announced":{},"heard":{},"beacons":{},"syncs":{},"cmd_pkts":{},"cmd_dups":{},"no_gw":{},"relock_bad":{},"restart_err":{},"us_last":{},"us_max":{},"us_avg":{}}}"#,
            self.link.gateway_known(),
            self.link.state_name(),
            c.windows,
            l.send_attempts,
            l.batches_sent,
            l.failed(),
            l.frames_sent,
            l.batches_given_up,
            l.not_sent,
            l.announcements,
            l.heard,
            l.beacons,
            l.syncs,
            l.commands,
            l.rejects.dups,
            c.no_gateway,
            c.relock_bad,
            c.restart_err,
            c.us_last,
            c.us_max,
            if c.windows > 0 { c.us_total / c.windows as u64 } else { 0 }
        );
        linkauth::say_rejects(l, &say);
    }
}
