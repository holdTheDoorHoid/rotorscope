//! Control-link scanning (docs/CONTROL-LINKS.md): the radio task hands the
//! LR2021 to this module while the plan's entry is `AeroMode::Control`.
//!
//! What to listen to, in what order, and what to send is decided in
//! `lr2021cfg::ctrl` (host tested); recognising packets is
//! `ctrlwatch::recognise`. This file only runs the jobs on the chip:
//!
//! - **Listen**: apply one (multi-SF) receive configuration, hand every
//!   packet to `recognise` with the step it came in on, stretch the dwell
//!   once something is recognised.
//! - **Sweep**: RSSI-only, retune across a band bin by bin, keep each bin's
//!   strongest reading in the band's energy map; close the window and, for
//!   a hopper, time its bursts on one of its channels.
//! - **Follow** (ExpressLRS, only with content decoding on): park on, or hop
//!   with, one link for up to `ctrl::FOLLOW_US`, decoding its packets. It
//!   ends early once no packet but a sync checks for a few passes
//!   (`ctrl::Follow::end_reason`), and `ctrl::FollowGate` limits how often
//!   follows start (per link and overall) and caps following to half of
//!   the control time: anyone can send a sync that checks, and nothing
//!   else is scanned while following.
//!
//! - **Contested links**: every recognised packet also goes to
//!   `ctrlwatch::contested::Watch`, which looks for a link heard from more
//!   transmitters than it can have (docs/DRONE-ATTACKS.md); its findings go
//!   to the hub as `Frame::Threat` of kind `ControlContested`
//!   (`ctrl::contested_threat`), the link's identifier only with `read_ids`.
//!
//! The opt-ins come from the plan (`AeroMode::ControlFlags`, both off by
//! default) and go to `recognise` and `Follow` unchanged. Receive only:
//! every command comes from `lr2021cfg`, which has no transmit command.
//!
//! **Hostile MAVLink** (`mavwatch`, docs/DRONE-ATTACKS.md): with content
//! decoding on, the MAVLink bytes of every SiK and mLRS packet go through
//! `mavwatch::Watch`, which reassembles frames that span packets. A finding
//! goes to the hub as a `Threat` (`MavlinkAttack`) followed by a
//! `MavlinkEvidence` frame with the MAVLink frame that caused it. This runs
//! here rather than on the hub because the hub only gets a packet's first
//! `ctrlwatch::MAX_RAW` (96) bytes, at most `CONTROL_PER_S` packets a
//! second: most of the frames the scan needs (camera and video URIs, FTP,
//! anything split across two SiK packets) never reach it whole. With
//! `control ids` off every SiK (and every mLRS) link shares one stream and
//! the link-integrity checks are off (`mavwatch::Link::distinct`).

use ctrlwatch::energy::{Activity, BurstTimer};
use ctrlwatch::{Config, ControlContent, ControlSighting, Modulation, Region, RxMeta, Sightings};
use embassy_time::{Duration, Instant, with_timeout};
use heapless::Vec;
use link::Frame;
use lr2021cfg::cmd;
use lr2021cfg::config::{self, Modem, RxConfig as LrConfig};
use lr2021cfg::ctrl::{
    self, BURST_DWELL_MS, CONTROL_PER_S, ContentGate, EnergyGate, EnergyMaps, Follow, Job,
    FollowGate, MAX_CONTESTED, MAX_GROUP, MAX_SIGHTINGS, SIGHTING_MAX_AGE_US, SUMMARY_GAP_US,
    SWEEP_DWELL_US,
};
use lr2021cfg::node::RateLimit;
use portable_atomic::Ordering::Relaxed;

use crate::board;
use crate::lr::{Error, Lr};
use crate::out::{self, COUNTERS, bump};
use crate::state::{self, PLAN};

/// Longest single wait, so summaries go out and a new plan is noticed.
const WAIT_US: u64 = 100_000;
/// Summaries / expiry this often.
const HOUSEKEEPING_US: u64 = 500_000;

fn now_us() -> u64 {
    Instant::now().as_micros()
}

/// Everything the control scan keeps between plan slots. Big (the energy
/// maps are ~18 KB): lives in a `StaticCell`.
pub struct Control {
    scanner: ctrl::Scanner,
    maps: EnergyMaps,
    energy_gate: EnergyGate,
    table: Sightings<MAX_SIGHTINGS>,
    content_gate: ContentGate,
    limit: RateLimit,
    follow: Option<(Follow, Option<ctrlwatch::LinkId>)>,
    gate: FollowGate,
    contested: ctrlwatch::contested::Watch<MAX_CONTESTED>,
    /// The opt-ins of the current plan entry (for reports made in
    /// housekeeping).
    cfg: Config,
    applied: Option<LrConfig>,
    last_housekeeping_us: u64,
    fifo: [u8; 256],
    /// Hostile-MAVLink scan (~8 KB), content decoding only.
    mav: mavwatch::Watch,
    mav_buf: [u8; 258],
}

impl Control {
    pub const fn new() -> Self {
        Self {
            scanner: ctrl::Scanner::new(0, 0),
            maps: EnergyMaps::new(),
            energy_gate: EnergyGate::new(),
            table: Sightings::new(),
            content_gate: ContentGate::new(),
            limit: RateLimit::new(CONTROL_PER_S, 0),
            follow: None,
            gate: FollowGate::new(),
            contested: ctrlwatch::contested::Watch::new(),
            cfg: Config {
                read_ids: false,
                decode_content: false,
            },
            applied: None,
            last_housekeeping_us: 0,
            fifo: [0; 256],
            mav: mavwatch::Watch::new(),
            mav_buf: [0; 258],
        }
    }

    /// The radio was reconfigured by someone else (aircraft mode, reset).
    pub fn forget_radio(&mut self) {
        self.applied = None;
    }

    /// Run the control scan until `until_us` (the plan slot's end, or a
    /// short slice when the entry stays), a new plan arrives, or the radio
    /// fails.
    pub async fn run(
        &mut self,
        lr: &mut Lr,
        regions: u16,
        energy: u16,
        cfg: Config,
        until_us: u64,
    ) -> Result<(), Error> {
        self.scanner.set_masks(regions, energy);
        self.cfg = cfg;
        self.gate.begin_slot(now_us(), until_us);
        if !cfg.decode_content && self.follow.take().is_some() {
            // Content was turned off: stop following now.
            self.gate.stopped(now_us());
        }
        while now_us() < until_us && !PLAN.signaled() {
            if let Some((f, _)) = &self.follow {
                let now = now_us();
                let why = f
                    .end_reason(now)
                    .or_else(|| (!self.gate.may_continue(now)).then_some("share"));
                if let Some(why) = why {
                    out::line(format_args!(
                        r#"{{"t":"follow_end","why":"{why}","packets":{},"recovered":{}}}"#,
                        f.packets, f.recovered
                    ));
                    self.follow = None;
                    if why == "share" {
                        bump(&COUNTERS.follows_cut);
                        self.gate.cut_off(now);
                    } else {
                        self.gate.stopped(now);
                    }
                } else {
                    self.follow_until(lr, cfg, until_us).await?;
                    continue;
                }
            }
            let Some(job) = self.scanner.current() else {
                return Ok(());
            };
            let done = match &job {
                Job::Listen { steps } => self.listen(lr, steps, cfg, until_us).await?,
                Job::Sweep {
                    region,
                    bin_hz,
                    window_ms,
                } => {
                    self.sweep(lr, *region, *bin_hz, *window_ms, until_us)
                        .await?
                }
            };
            if done {
                self.scanner.done();
            }
        }
        Ok(())
    }

    fn apply(&mut self, lr: &mut Lr, c: &LrConfig) -> Result<(), Error> {
        let same_modem = self
            .applied
            .is_some_and(|a| a.modem == c.modem && a.path() == c.path());
        if same_modem {
            if self.applied.is_some_and(|a| a.freq_hz == c.freq_hz) {
                return Ok(());
            }
            bump(&COUNTERS.retunes);
            lr.run(&config::retune(c.freq_hz))?;
        } else {
            lr.run(&config::apply(&board::LR2021, c))?;
        }
        self.applied = Some(*c);
        state::set_status(|s| s.freq_khz = c.freq_hz / 1000);
        Ok(())
    }

    /// Every half second: due summaries to the hub, forget silent links.
    fn housekeeping(&mut self) {
        let now = now_us();
        if now.saturating_sub(self.last_housekeeping_us) < HOUSEKEEPING_US {
            return;
        }
        self.last_housekeeping_us = now;
        // The follower's share of control time accrues while scanning.
        self.gate.tick(now);
        self.table.take_due(now, SUMMARY_GAP_US, |s| {
            bump(&COUNTERS.ctrl_reports);
            out::line(format_args!(
                r#"{{"t":"ctrl","sys":"{:?}","region":"{:?}","f":{},"conf":"{:?}","rssi":{},"peak":{},"pkts":{},"confirmed":{},"id":{}}}"#,
                s.last.system,
                s.last.region,
                s.last.freq_hz / 1000,
                s.best,
                s.last.rssi_dbm,
                s.peak_rssi_dbm,
                s.packets,
                s.confirmed,
                s.last.id.is_some()
            ));
            out::hub_frame(&Frame::Control(ctrl::summary(s)));
        });
        self.table.expire(now, SIGHTING_MAX_AGE_US);
        self.contested.take_due(now, &self.cfg, |r| {
            bump(&COUNTERS.ctrl_reports);
            out::line(format_args!(
                r#"{{"t":"contested","sys":"{:?}","region":"{:?}","f":{},"tx":{},"rule":"{}","events":{},"id":{}}}"#,
                r.system,
                r.region,
                r.freq_hz / 1000,
                r.transmitters,
                ctrlwatch::contested::rule_name(r.rules),
                r.events,
                r.id.is_some()
            ));
            out::hub_frame(&Frame::Threat(ctrl::contested_threat(r, now)));
        });
        self.contested.expire(now);
    }

    /// One packet's content (and raw bytes) to the hub, within the gates.
    fn send_content<const N: usize>(
        &mut self,
        s: &ControlSighting,
        mut content: Vec<ControlContent, N>,
        raw: Option<&[u8]>,
    ) {
        let now = s.t_us;
        let has_items = self.content_gate.filter(s.system, &mut content, now);
        if !has_items && raw.is_none() {
            return;
        }
        if !self.limit.allow(now) {
            bump(&COUNTERS.ctrl_dropped);
            return;
        }
        let counts = self
            .table
            .iter()
            .find(|x| x.last.system == s.system && x.last.region == s.region && x.last.id == s.id)
            .copied();
        ctrl::content_reports(s, counts.as_ref(), &content, raw, |r| {
            bump(&COUNTERS.ctrl_reports);
            out::hub_frame(&Frame::Control(r));
        });
    }

    /// Wait for a packet until `deadline_us` (at most `WAIT_US`); read it.
    /// Returns the bytes' length and meta, or `None` on timeout / no RX.
    async fn next_packet(
        &mut self,
        lr: &mut Lr,
        modem: &Modem,
        deadline_us: u64,
    ) -> Result<Option<(usize, RxMeta, u8)>, Error> {
        let wait = deadline_us.saturating_sub(now_us()).min(WAIT_US);
        let got = lr.irq.is_high()
            || with_timeout(Duration::from_micros(wait), lr.irq.wait_for_high())
                .await
                .is_ok();
        if !got {
            return Ok(None);
        }
        let t = now_us();
        if lr.irq_status()? & cmd::irq::RX_DONE == 0 {
            return Ok(None);
        }
        let n = (lr.packet_len()? as usize).min(self.fifo.len());
        lr.read_fifo(&mut self.fifo[..n])?;
        let rsp = lr.packet_status(&config::packet_status_req(modem))?;
        let (rssi, snr, det) = match modem {
            Modem::Lora { .. } => cmd::parse_lora_status(&rsp),
            _ => (cmd::parse_fsk_rssi(&rsp), 0, 1),
        };
        bump(&COUNTERS.packets);
        bump(&COUNTERS.ctrl_packets);
        Ok(Some((
            n,
            RxMeta {
                rssi_dbm: i16::from(rssi),
                snr_db: snr,
                t_us: t,
            },
            det,
        )))
    }

    /// A packet-plan job. True when it ran its full dwell.
    async fn listen(
        &mut self,
        lr: &mut Lr,
        steps: &Vec<ctrlwatch::ScanStep, MAX_GROUP>,
        cfg: Config,
        until_us: u64,
    ) -> Result<bool, Error> {
        let Some(c) = ctrl::lr_group(steps) else {
            // Nothing the chip can take (FLRC): skip it.
            return Ok(true);
        };
        self.apply(lr, &c)?;
        let start = now_us();
        let job = Job::Listen {
            steps: steps.clone(),
        };
        let mut end = start + u64::from(job.dwell_ms()) * 1000;
        let mut extended = false;
        loop {
            self.housekeeping();
            let now = now_us();
            if now >= end {
                return Ok(true);
            }
            if now >= until_us || PLAN.signaled() {
                return Ok(false);
            }
            let Some((n, meta, det)) = self.next_packet(lr, &c.modem, end.min(until_us)).await?
            else {
                continue;
            };
            let step = steps.get(ctrl::detector_index(det)).unwrap_or(&steps[0]);
            let bytes = &self.fifo[..n];
            let Some(r) = ctrlwatch::recognise(bytes, &step.config, meta, &cfg) else {
                continue;
            };
            bump(&COUNTERS.ctrl_recognised);
            self.table.record(&r.sighting);
            self.contested.observe(bytes, &step.config, meta, &cfg);
            if !extended {
                extended = true;
                end = start + u64::from(job.extend_ms()) * 1000;
            }
            let follow = (cfg.decode_content
                && r.sighting.system == ctrlwatch::System::ExpressLrs
                && r.sighting.confidence == ctrlwatch::Confidence::Confirmed
                && self.follow.is_none())
            .then(|| Follow::start(&cfg, bytes, &step.config, meta.t_us))
            .flatten();
            if cfg.decode_content {
                scan_mavlink(&mut self.mav, &mut self.mav_buf, bytes, &step.config, &r.sighting);
                let raw: Vec<u8, { ctrlwatch::MAX_RAW }> = r.raw.clone().unwrap_or_default();
                self.send_content(
                    &r.sighting,
                    r.content,
                    (!raw.is_empty()).then_some(raw.as_slice()),
                );
            }
            if let Some(f) = follow {
                // Rate limits and the share (anyone can send a sync that
                // checks): a refused follow leaves the scan as it was.
                if let Err(e) = self.gate.try_start(&f, now_us()) {
                    bump(&COUNTERS.follows_refused);
                    out::line(format_args!(r#"{{"t":"follow_refused","why":"{}"}}"#, e.name()));
                    continue;
                }
                out::line(format_args!(
                    r#"{{"t":"follow","rate":"{}","f":{}}}"#,
                    f.rate().info().name,
                    step.config.freq_hz / 1000
                ));
                bump(&COUNTERS.follows);
                self.follow = Some((f, r.sighting.id));
                // The step heard what it was there for.
                return Ok(true);
            }
        }
    }

    /// Follow one ExpressLRS link until it expires, `until_us`, or a new plan.
    async fn follow_until(&mut self, lr: &mut Lr, cfg: Config, until_us: u64) -> Result<(), Error> {
        loop {
            self.housekeeping();
            let now = now_us();
            let Some((f, _)) = self.follow.as_mut() else {
                return Ok(());
            };
            // `run` ends the follow (time, quiet or share).
            if f.expired(now) || now >= until_us || PLAN.signaled() || !self.gate.may_continue(now) {
                return Ok(());
            }
            let Some(t) = f.target(now) else {
                self.follow = None;
                return Ok(());
            };
            let interval = u64::from(f.rate().info().interval_us);
            let Some(c) = ctrl::lr_config(&t.config) else {
                self.follow = None;
                return Ok(());
            };
            self.apply(lr, &c)?;
            // Hopping: until just after the predicted packet; parked: a while.
            let deadline = match t.at_us {
                Some(at) => at + interval / 2 + 500,
                None => now + WAIT_US,
            }
            .min(until_us);
            let Some((n, meta, _)) = self.next_packet(lr, &c.modem, deadline).await? else {
                continue;
            };
            let Some((f, id)) = self.follow.as_mut() else {
                return Ok(());
            };
            let id = *id;
            let Some(got) = f.process(&self.fifo[..n], meta, t.channel) else {
                continue;
            };
            if got.is_sync {
                self.contested.observe(&self.fifo[..n], &t.config, meta, &cfg);
            }
            bump(&COUNTERS.ctrl_recognised);
            let s = ControlSighting {
                system: ctrlwatch::System::ExpressLrs,
                region: t.config.region,
                probe: t.config.probe,
                freq_hz: t.config.freq_hz,
                rssi_dbm: meta.rssi_dbm,
                snr_db: meta.snr_db,
                t_us: meta.t_us,
                confidence: ctrlwatch::Confidence::Confirmed,
                direction: got.direction,
                id,
                version: Some(f.f.version.id()),
            };
            self.table.record(&s);
            if cfg.decode_content {
                let mut raw = [0u8; ctrlwatch::MAX_RAW];
                let k = n.min(raw.len());
                raw[..k].copy_from_slice(&self.fifo[..k]);
                self.send_content(&s, got.content, Some(&raw[..k]));
            }
        }
    }

    /// One energy window of `region`'s band. True when the window closed.
    async fn sweep(
        &mut self,
        lr: &mut Lr,
        region: Region,
        bin_hz: u32,
        window_ms: u32,
        until_us: u64,
    ) -> Result<bool, Error> {
        let start = now_us();
        let end = start + u64::from(window_ms) * 1000;
        let mut first = true;
        'window: loop {
            for step in ctrlwatch::scan::energy_plan(region, bin_hz) {
                let now = now_us();
                if now >= until_us || PLAN.signaled() {
                    return Ok(false);
                }
                if now >= end && !first {
                    break 'window;
                }
                let Some(c) = ctrl::lr_config(&step.config) else {
                    continue;
                };
                self.apply(lr, &c)?;
                let peak = self.peak_rssi(lr, SWEEP_DWELL_US).await?;
                self.maps.get(region).add(step.config.freq_hz, peak);
            }
            first = false;
            if now_us() >= end {
                break;
            }
        }
        bump(&COUNTERS.sweeps);
        let rep = self.maps.get(region).end_window();
        let burst = match rep.activity {
            Activity::Hopper(h) => {
                // Time the bursts on its lowest busy channel.
                let rx = ctrlwatch::RxConfig {
                    freq_hz: h.low_hz,
                    modulation: Modulation::Energy { rx_bw_hz: bin_hz },
                    probe: ctrlwatch::Probe::Energy,
                    region,
                };
                match ctrl::lr_config(&rx) {
                    Some(c) => {
                        self.apply(lr, &c)?;
                        self.burst_dwell(lr, until_us).await?
                    }
                    None => None,
                }
            }
            _ => None,
        };
        let now = now_us();
        if self.energy_gate.due(region, &rep, now) {
            let f = ctrl::energy_frame(region, &rep, burst.as_ref(), now);
            out::line(format_args!(
                r#"{{"t":"energy","region":"{region:?}","windows":{},"activity":"{}","steady":{}}}"#,
                rep.windows,
                match rep.activity {
                    Activity::Quiet => "quiet",
                    Activity::Steady => "steady",
                    Activity::Background => "background",
                    Activity::Hopper(_) => "hopper",
                },
                rep.steady.len()
            ));
            bump(&COUNTERS.ctrl_reports);
            out::hub_frame(&Frame::ControlEnergy(f));
        }
        Ok(true)
    }

    /// The strongest instantaneous RSSI over `dwell_us` (the first reading
    /// after a retune is skipped: the receiver is still settling).
    async fn peak_rssi(&mut self, lr: &mut Lr, dwell_us: u32) -> Result<i16, Error> {
        let t0 = now_us();
        let mut peak = i16::MIN;
        let mut n = 0u32;
        while now_us().saturating_sub(t0) < u64::from(dwell_us) {
            let r = i16::from(lr.rssi_inst()?);
            if n > 0 {
                peak = peak.max(r);
            }
            n += 1;
            embassy_futures::yield_now().await;
        }
        Ok(if n > 1 { peak } else { -127 })
    }

    /// Fast RSSI reads on one channel for `BURST_DWELL_MS`: burst lengths
    /// and the packet interval.
    async fn burst_dwell(
        &mut self,
        lr: &mut Lr,
        until_us: u64,
    ) -> Result<Option<ctrlwatch::energy::BurstSummary>, Error> {
        let mut bt: BurstTimer<64> = BurstTimer::new();
        let t0 = now_us();
        let end = (t0 + u64::from(BURST_DWELL_MS) * 1000).min(until_us);
        while now_us() < end && !PLAN.signaled() {
            let r = i16::from(lr.rssi_inst()?);
            bt.add(r, now_us());
            embassy_futures::yield_now().await;
        }
        Ok(bt.summary())
    }
}

/// The control counters for the stats line: packets, recognised, reports,
/// dropped by the rate limit, energy windows, follows.
pub fn stats() -> (u32, u32, u32, u32, u32, u32) {
    let c = &COUNTERS;
    (
        c.ctrl_packets.load(Relaxed),
        c.ctrl_recognised.load(Relaxed),
        c.ctrl_reports.load(Relaxed),
        c.ctrl_dropped.load(Relaxed),
        c.sweeps.load(Relaxed),
        c.follows.load(Relaxed),
    )
}

/// Feed one SiK / mLRS packet's MAVLink bytes to `mavwatch` and send what
/// it finds: a `Threat` the hub's threat table takes as any other, then the
/// offending frame for the evidence log. Only called with content decoding
/// on.
fn scan_mavlink(
    mav: &mut mavwatch::Watch,
    buf: &mut [u8; 258],
    packet: &[u8],
    rx: &ctrlwatch::RxConfig,
    s: &ControlSighting,
) {
    let Some((data, dir)) = ctrlwatch::serial_bytes(packet, rx, buf) else {
        return;
    };
    // The link's own ID, only when the hub turned ID reading on (it is in
    // the sighting only then); it stays in this node's memory as a stream
    // key and is never sent.
    let id = match s.id {
        Some(ctrlwatch::LinkId::SikNetId(n)) => Some(u32::from(n)),
        Some(ctrlwatch::LinkId::MlrsSyncWord(w)) => Some(u32::from(w)),
        _ => None,
    };
    let key = mavwatch::StreamKey {
        system: s.system as u8,
        id,
        dir: match dir {
            ctrlwatch::Direction::Uplink => 1,
            ctrlwatch::Direction::Downlink => 2,
            ctrlwatch::Direction::Unknown => 0,
        },
    };
    let now_ms = s.t_us / 1000;
    mav.feed(key, data, now_ms, &mut |f, fr| {
        bump(&COUNTERS.mav_findings);
        let addr = |(sys, comp): (u8, u8)| [0, 0, 0, 0, sys, comp];
        let threat = link::ThreatSighting {
            kind: link::ThreatKind::MavlinkAttack,
            channel: 0,
            freq: s.freq_hz / 100_000,
            width_khz: 0,
            level: s.rssi_dbm.clamp(-128, 127) as i8,
            count: f.hits,
            window_ms: now_ms.saturating_sub(f.first_ms).min(u64::from(u32::MAX)) as u32,
            source: addr((f.sysid, f.compid)),
            target: f.target.map_or([0; 6], addr),
            detail: f.kind.code(),
            ssid: f.excerpt.clone(),
            uptime_us: s.t_us,
        };
        out::hub_frame(&Frame::Threat(threat));
        let n = fr.bytes.len().min(link::MAX_MAVLINK_EVIDENCE);
        out::hub_frame(&Frame::MavlinkEvidence(link::MavlinkEvidence {
            detail: f.kind.code(),
            sysid: f.sysid,
            compid: f.compid,
            msgid: f.msgid,
            truncated: fr.bytes.len() > n,
            uptime_us: s.t_us,
            frame: Vec::from_slice(&fr.bytes[..n]).unwrap_or_default(),
        }));
        out::line(format_args!(
            r#"{{"t":"mavlink_attack","kind":{},"sys":{},"comp":{},"msg":{},"hits":{}}}"#,
            f.kind.code(),
            f.sysid,
            f.compid,
            f.msgid,
            f.hits
        ));
    });
}
