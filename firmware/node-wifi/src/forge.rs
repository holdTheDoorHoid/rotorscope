//! Bench only (`forgetest`, docs/BENCH.md "Origin checks", and the node
//! rows of docs/LINK-KEYS-DESIGN.md section 9). Type on USB, + Enter.
//!
//! **Frames this node must never originate**, sent through its normal hub
//! queue (sealed with its own key like any other report, so they reach the
//! hub, whose origin checks must refuse them):
//!
//! - `forge hubfix`: a `HubFix` placing the kit at 0 N, 0 E with a clock in
//!   2001 (what an attacker would use to move the web head's map and clock);
//! - `forge csbee`: an ADS-B line for a made-up aircraft;
//! - `forge flags`: an FAA flag report;
//! - `forge via`: a `Via` naming 02:00:00:00:00:99, then a `Stats` (the
//!   frame the `Via` would steal);
//! - `forge hello`: a `Hello` claiming the gateway role, "gw key FAKE";
//! - `forge threat`: a `ForgedLink` report naming 02:00:00:00:00:99;
//! - `forge join`: a wired-bus `BusJoin`.
//!
//! Expected: the hub refuses each (`forged_link`, `detail` 5, naming this
//! node); nothing reaches the tracker or the web head. With per-node keys the
//! gateway holds no key and no longer reads frames, so the hub's own origin
//! check is the one that refuses them (the gateway's old `refused` counter
//! does not move); `via` stays meaningful because the hub must not take a
//! `Via` from inside a node's batch.
//!
//! **A captured node** (ESP-NOW builds): this build behaves as a node an
//! attacker holds, which knows its own key only. Everything goes out through
//! the board's normal ESP-NOW path, on the gateway channel; nothing beyond
//! the kit's own ESP-NOW traffic is transmitted.
//!
//! - `forge claim <mac>`: sets the station MAC to `<mac>` (another node's,
//!   `esp_wifi_set_mac` on the running driver), sends five batches sealed
//!   with this node's own key, and sets the MAC back. If the driver refuses
//!   the change nothing is sent (it would go out as this node's own, genuine
//!   batch) and the line says so (untested on hardware). Expected: hub
//!   `bad_tag` for `<mac>`'s slot +5, nothing from this node under `<mac>`,
//!   `<mac>` keeps reporting.
//! - `forge cmd <mac>`: a unicast Command to node `<mac>` (an `Assign` with a
//!   plan without channel 6), sealed with this node's own key. Expected: the
//!   victim's `link_auth` `bad_tag` +1, its plan unchanged.
//! - `forge beacon`: a broadcast beacon for all 16 slots with made-up tags
//!   (this node has no key that makes a beacon tag for anyone). Expected:
//!   the other nodes' `bad_tag` +1, their gateway address unchanged.
//! - `forge hold <s>`: seals a batch now, sends it `s` seconds later (normal
//!   sending is paused meanwhile, so the held batch is the newest the hub
//!   sees and is judged on its age, not as a replay). Expected for `s` >= 3:
//!   hub `late_uplink` +1, nothing from it in the tracker.
//! - `forge spam <n>`: bad-tag batches as this node, `n` a second (at most
//!   1000), for at most 60 s; `forge spam 0` stops. Sent only while the
//!   hopper is on channel 6, so a hopping plan sends fewer. Expected: gateway
//!   `over_budget` for this node, other nodes' delivery unchanged.
//!
//! Each line prints `{"t":"forge",...}`.

use embassy_time::{Duration, Timer};

use crate::report::to_hub;

const VICTIM: [u8; 6] = [0x02, 0, 0, 0, 0, 0x99];

fn frame(what: &str) -> Option<link::Frame> {
    use link::Frame;
    Some(match what {
        "hubfix" => Frame::HubFix(link::HubFix {
            state: link::GnssState::Fix3d,
            sats: Some(12),
            position: Some(link::HubPosition { lat_e7: 0, lon_e7: 0, alt_hae_mm: Some(0), age_ms: 0 }),
            utc_ms: Some(978_307_200_000),
            utc_pps: true,
            held: false,
            utc_held: false,
        }),
        "csbee" => Frame::Csbee(heapless::Vec::from_slice(b"#A:FA4E00,,,,,,,,,,,,,,,,0000\r\n").ok()?),
        "flags" => Frame::Flags(link::FlagReport {
            who: heapless::String::try_from("FORGED").ok()?,
            level: 3,
            items: heapless::Vec::new(),
            pack_date: 0,
        }),
        "via" => Frame::Via { node: VICTIM, rssi: -20 },
        "hello" => Frame::Hello {
            role: link::NodeRole::Gateway,
            protocol: link::PROTOCOL_VERSION,
            firmware: heapless::String::try_from("gw key FAKE").ok()?,
        },
        "threat" => Frame::Threat(link::ThreatSighting {
            kind: link::ThreatKind::ForgedLink,
            channel: 6,
            freq: 24_370,
            width_khz: 0,
            level: -20,
            count: 99,
            window_ms: 1000,
            source: VICTIM,
            target: [0; 6],
            detail: 1,
            ssid: heapless::String::new(),
            uptime_us: 0,
        }),
        "join" => Frame::BusJoin { mac: VICTIM, want: 1, check: 0 },
        _ => return None,
    })
}

/// `aa:bb:cc:dd:ee:ff`.
#[cfg_attr(not(feature = "espnow"), allow(dead_code))]
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut m = [0u8; 6];
    let mut parts = s.split(':');
    for b in &mut m {
        *b = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(m)
}

/// A captured-node attack for the uplink task to carry out.
#[cfg(feature = "espnow")]
#[derive(Clone, Copy)]
pub enum Op {
    Claim([u8; 6]),
    Cmd([u8; 6]),
    Beacon,
    Hold(u8),
    Spam(u16),
}

#[cfg(feature = "espnow")]
pub static OPS: embassy_sync::channel::Channel<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, Op, 2> =
    embassy_sync::channel::Channel::new();

#[cfg(feature = "espnow")]
fn air_op(cmd: &str) -> Option<Result<Op, &'static str>> {
    let mut w = cmd.split_whitespace();
    let op = w.next()?;
    let arg = w.next();
    Some(match op {
        "claim" | "cmd" => match arg.and_then(parse_mac) {
            Some(m) if op == "claim" => Ok(Op::Claim(m)),
            Some(m) => Ok(Op::Cmd(m)),
            None => Err("needs a MAC aa:bb:cc:dd:ee:ff"),
        },
        "beacon" => Ok(Op::Beacon),
        "hold" => match arg.and_then(|a| a.parse::<u8>().ok()) {
            Some(s) if (1..=30).contains(&s) => Ok(Op::Hold(s)),
            _ => Err("needs seconds 1-30"),
        },
        "spam" => match arg.and_then(|a| a.parse::<u16>().ok()) {
            Some(n) if n <= 1000 => Ok(Op::Spam(n)),
            _ => Err("needs a rate 0-1000 per second"),
        },
        _ => return None,
    })
}

pub async fn console(mut read: impl FnMut(&mut [u8]) -> usize) -> ! {
    let mut line: heapless::Vec<u8, 48> = heapless::Vec::new();
    let mut buf = [0u8; 32];
    crate::usb::usb_line!(
        r#"{{"t":"forge","note":"FORGE TEST BUILD: type 'forge hubfix|csbee|flags|via|hello|threat|join' or (ESP-NOW) 'forge claim <mac>|cmd <mac>|beacon|hold <s>|spam <n>' + Enter"}}"#
    );
    loop {
        Timer::after(Duration::from_millis(50)).await;
        let n = read(&mut buf);
        for &b in &buf[..n] {
            if b != b'\r' && b != b'\n' {
                if line.push(b).is_err() {
                    line.clear();
                }
                continue;
            }
            let cmd = core::str::from_utf8(&line).unwrap_or("").trim();
            if let Some(what) = cmd.strip_prefix("forge ") {
                let what = what.trim();
                #[cfg(feature = "espnow")]
                if let Some(op) = air_op(what) {
                    match op {
                        Ok(op) => {
                            if OPS.try_send(op).is_err() {
                                crate::usb::usb_line!(r#"{{"t":"forge","err":"busy: an earlier forge is still waiting for channel 6"}}"#);
                            }
                        }
                        Err(e) => crate::usb::usb_line!(r#"{{"t":"forge","err":"{e}"}}"#),
                    }
                    line.clear();
                    continue;
                }
                match frame(what) {
                    Some(f) => {
                        to_hub(&f);
                        if matches!(f, link::Frame::Via { .. }) {
                            // The frame the Via would claim for the victim.
                            to_hub(&link::Frame::Stats {
                                uptime_us: 0,
                                frames_seen: 0,
                                rid_found: 0,
                                dropped: 0,
                                by_transport: heapless::Vec::new(),
                            });
                        }
                        crate::usb::usb_line!(r#"{{"t":"forge","sent":"{}"}}"#, what);
                    }
                    None => crate::usb::usb_line!(
                        r#"{{"t":"forge","err":"unknown; hubfix|csbee|flags|via|hello|threat|join|claim <mac>|cmd <mac>|beacon|hold <s>|spam <n>"}}"#
                    ),
                }
            } else if !cmd.is_empty() {
                crate::usb::usb_line!(r#"{{"t":"forge","err":"type 'forge <what>'"}}"#);
            }
            line.clear();
        }
    }
}

// ------------------------------------------------------- captured node --

#[cfg(feature = "espnow")]
pub use air::Air;

#[cfg(feature = "espnow")]
mod air {
    use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow};
    use node_common::linkauth::{self, Link, MacText};

    use super::{OPS, Op};
    use crate::uplink::{add_peer, send};

    /// A batch holding one harmless `Stats` frame.
    fn stats_batch(b: &mut airlink::Batch) {
        b.clear();
        let mut enc = [0u8; link::MAX_FRAME];
        let f = link::Frame::Stats { uptime_us: 0, frames_seen: 0, rid_found: 0, dropped: 0, by_transport: heapless::Vec::new() };
        if let Ok(w) = link::encode(&f, &mut enc) {
            b.push(w);
        }
    }

    fn set_station_mac(mac: &[u8; 6]) -> i32 {
        use crate::chip::sys;
        // SAFETY: Wi-Fi is initialised; `mac` outlives the call.
        unsafe { sys::esp_wifi_set_mac(sys::wifi_interface_t_WIFI_IF_STA, mac.as_ptr()) }
    }

    /// The captured-node state the uplink task carries (bench builds only:
    /// two 1 KB buffers in the task's static state).
    pub struct Air {
        scratch: airlink::Batch,
        hold: Option<(heapless::Vec<u8, { airlink::MAX_PACKET }>, u64)>,
        /// Rate, start (ms), sent so far.
        spam: Option<(u16, u64, u32)>,
        seq: u16,
    }

    impl Air {
        pub fn new() -> Self {
            Air { scratch: airlink::Batch::new(), hold: None, spam: None, seq: 0x8000 }
        }

        fn seq(&mut self) -> u16 {
            self.seq = self.seq.wrapping_add(1);
            self.seq
        }

        /// One pass of the uplink loop. True while normal sending must
        /// pause (a held batch is waiting).
        pub async fn step(&mut self, now: &mut EspNow, up: &mut Link, tuned: u8) -> bool {
            let t = linkauth::now_ms();
            let gw = up.gateway(t);
            let on = gw.is_some_and(|(_, ch)| ch == tuned);
            if on && let Ok(op) = OPS.try_receive() {
                self.start(op, now, up, t).await;
            }
            if let Some((p, at)) = &self.hold
                && on
                && t >= *at
                && let Some((mac, _)) = gw
            {
                let acked = send(now, &mac, p).await;
                crate::usb::usb_line!(r#"{{"t":"forge","sent":"hold","len":{},"acked":{acked}}}"#, p.len());
                self.hold = None;
            }
            if let Some((rate, since, sent)) = self.spam {
                let elapsed = t.saturating_sub(since);
                if rate == 0 || elapsed > 60_000 {
                    crate::usb::usb_line!(r#"{{"t":"forge","spam":"done","sent":{sent}}}"#);
                    self.spam = None;
                } else if on && let Some((mac, _)) = gw {
                    let due = (elapsed * u64::from(rate) / 1000) as u32;
                    let mut n = 0;
                    let mut sent = sent;
                    while sent < due && n < 8 {
                        let s = self.seq();
                        stats_batch(&mut self.scratch);
                        let Some(p) = up.packet(&mut self.scratch, s, t) else { break };
                        let mut bad: heapless::Vec<u8, { airlink::MAX_PACKET }> = heapless::Vec::new();
                        let _ = bad.extend_from_slice(p);
                        if let Some(last) = bad.last_mut() {
                            *last ^= 0x01;
                        }
                        send(now, &mac, &bad).await;
                        sent += 1;
                        n += 1;
                    }
                    self.spam = Some((rate, since, sent));
                }
            }
            self.hold.is_some()
        }

        async fn start(&mut self, op: Op, now: &mut EspNow, up: &mut Link, t: u64) {
            let Some((gw, ch)) = up.gateway(t) else {
                crate::usb::usb_line!(r#"{{"t":"forge","err":"no gateway yet"}}"#);
                return;
            };
            match op {
                Op::Claim(mac) => {
                    let me = node_common::keystore::efuse_mac();
                    let e_set = set_station_mac(&mac);
                    if e_set != 0 {
                        // Sent from our own MAC it would be a genuine batch
                        // of ours: nothing is sent.
                        crate::usb::usb_line!(
                            r#"{{"t":"forge","err":"claim","set_mac":{e_set},"note":"the driver refused the station MAC change while running: nothing sent"}}"#
                        );
                        return;
                    }
                    let mut acked = 0;
                    let mut made = 0;
                    for _ in 0..5 {
                        let s = self.seq();
                        stats_batch(&mut self.scratch);
                        let Some(p) = up.packet(&mut self.scratch, s, linkauth::now_ms()) else { break };
                        made += 1;
                        if send(now, &gw, p).await {
                            acked += 1;
                        }
                    }
                    let e_back = set_station_mac(&me);
                    crate::usb::usb_line!(
                        r#"{{"t":"forge","sent":"claim","as":"{}","made":{made},"acked":{acked},"set_mac":{e_set},"restore_mac":{e_back},"note":"sealed with this node's own key; set_mac 0 = the claim went out as that MAC"}}"#,
                        MacText(mac)
                    );
                }
                Op::Cmd(victim) => {
                    // An Assign moving the victim off channel 6: harmful if
                    // it were ever taken.
                    let mut enc = [0u8; link::MAX_FRAME];
                    let a = link::Command::Assign(link::Assignment {
                        role: link::NodeRole::Wifi24,
                        plan: heapless::Vec::from_slice(&[(1, 1000), (11, 1000)]).unwrap_or_default(),
                        id: 0xF0F0,
                    });
                    self.scratch.clear();
                    if let Ok(w) = link::encode_command(&a, &mut enc) {
                        self.scratch.push(w);
                    }
                    let s = self.seq();
                    // This node can seal only with its own keys: a Frames
                    // packet under its K_up, relabelled as a Command.
                    let Some(p) = up.packet(&mut self.scratch, s, t) else {
                        crate::usb::usb_line!(r#"{{"t":"forge","err":"no hub epoch yet"}}"#);
                        return;
                    };
                    let mut pkt: heapless::Vec<u8, { airlink::MAX_PACKET }> = heapless::Vec::new();
                    let _ = pkt.extend_from_slice(p);
                    pkt[3] = airlink::v3::Kind::Command as u8;
                    add_peer(now, &victim);
                    let acked = send(now, &victim, &pkt).await;
                    crate::usb::usb_line!(r#"{{"t":"forge","sent":"cmd","to":"{}","acked":{acked}}}"#, MacText(victim));
                }
                Op::Beacon => {
                    let n = SLOTS;
                    let mut b = [0u8; airlink::v3::BEACON_MAX];
                    let len = airlink::v3::beacon_len(n);
                    b[..2].copy_from_slice(&airlink::MAGIC);
                    b[2] = airlink::v3::VERSION_V3;
                    b[3] = airlink::v3::Kind::Beacon as u8;
                    let s = self.seq();
                    b[4..6].copy_from_slice(&s.to_le_bytes());
                    b[6] = ch;
                    b[7] = n as u8;
                    let node = up.node();
                    let epoch = node.and_then(|n| n.epoch()).unwrap_or(1);
                    let clock = node.and_then(|n| n.estimate(t)).unwrap_or(0).min(u64::from(u32::MAX)) as u32;
                    b[8..12].copy_from_slice(&epoch.to_le_bytes());
                    b[12..16].copy_from_slice(&clock.wrapping_add(10).to_le_bytes());
                    // session and counter stay 0; the tags are made up.
                    b[airlink::v3::BEACON_FIXED..len].fill(0xA5);
                    send(now, &BROADCAST_ADDRESS, &b[..len]).await;
                    crate::usb::usb_line!(r#"{{"t":"forge","sent":"beacon","slots":{n},"epoch":{epoch}}}"#);
                }
                Op::Hold(secs) => {
                    let s = self.seq();
                    stats_batch(&mut self.scratch);
                    let Some(p) = up.packet(&mut self.scratch, s, t) else {
                        crate::usb::usb_line!(r#"{{"t":"forge","err":"no hub epoch yet"}}"#);
                        return;
                    };
                    let mut v = heapless::Vec::new();
                    let _ = v.extend_from_slice(p);
                    self.hold = Some((v, t + u64::from(secs) * 1000));
                    crate::usb::usb_line!(r#"{{"t":"forge","held":{secs},"note":"sealed now, sent in {secs} s; normal sending paused until then"}}"#);
                }
                Op::Spam(rate) => {
                    if rate == 0 {
                        if let Some(sp) = self.spam.as_mut() {
                            sp.0 = 0;
                        }
                    } else {
                        self.spam = Some((rate, t, 0));
                        crate::usb::usb_line!(r#"{{"t":"forge","spam":{rate},"note":"bad-tag batches as this node, for at most 60 s; 'forge spam 0' stops"}}"#);
                    }
                }
            }
        }
    }

    /// Every roster slot (`kitkeys::SLOTS`).
    const SLOTS: usize = 16;
}
