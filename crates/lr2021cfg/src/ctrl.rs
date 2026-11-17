//! The control-link scan on the LR2021 (docs/CONTROL-LINKS.md, PLAN.md
//! Phase 9): `ctrlwatch` scan plans turned into chip configurations, the
//! order the node walks them in, the ExpressLRS follow mode's tuning, and
//! the link frames the node sends. Pure logic; `firmware/node-lr` moves the
//! bytes.
//!
//! Receive only, like everything in this crate: a scan step is a receive
//! configuration ([`crate::config::apply`]) or an RSSI-only dwell; nothing
//! here can transmit.
//!
//! # The cycle
//!
//! [`Scanner`] walks, for each region the hub asked for (lowest bit first):
//! ExpressLRS, mLRS, SiK and OpenLRSng packet plans (`ctrlwatch::scan::plan`),
//! one [`Job::Listen`] per step, consecutive steps that
//! `ctrlwatch::scan::multi_sf_compatible` allows merged into one multi-SF
//! receive (up to four spreading factors at once). After every
//! [`LISTENS_PER_SWEEP`] listens comes one [`Job::Sweep`]: an energy window
//! of the next region's band (round robin), so each band's energy map keeps
//! learning its background while the packet plans run.
//!
//! # The opt-ins
//!
//! `ctrlwatch::Config` (from the plan's `AeroMode::ControlFlags` entry) goes
//! to `ctrlwatch::recognise` unchanged: with both flags off a report carries
//! presence only. [`Follow`] (ExpressLRS hop following) is only started with
//! `decode_content`, and recovering the missing UID bytes only runs with
//! `read_ids` as well, exactly as `ctrlwatch::elrs::Follower` requires.

use ctrlwatch::elrs::{self, ElrsRate, Followed, Follower, OtaVersion, Recovery};
use ctrlwatch::energy::{Activity, BurstSummary, EnergyReport};
use ctrlwatch::rx::{LoRaBw, LoRaCr, LoRaHeader};
use ctrlwatch::{
    Config, ControlContent, ControlSighting, Modulation, Probe, Region, RxMeta, ScanStep, Sighting,
    System,
};
use heapless::Vec;
use link::{ControlEnergy, ControlReport, EnergyActivity, HopperReport, MAX_CONTROL_CONTENT};

use crate::cmd::{bw, lora_bw, lora_cr, shaping};
use crate::config::{Modem, RxConfig as LrConfig};

/// Steps one multi-SF receive can hold (the main detector and three side
/// detectors).
pub const MAX_GROUP: usize = 4;
/// Packet listens between two energy windows.
pub const LISTENS_PER_SWEEP: u8 = 2;
/// One energy window: repeated sweeps of a band for this long.
pub const SWEEP_WINDOW_MS: u32 = 3_000;
/// A sweep step: listen this long on each bin, keeping the strongest
/// reading (control links are bursts of 1-10 ms).
pub const SWEEP_DWELL_US: u32 = 2_000;
/// A burst-timing dwell on one of a hopper's channels.
pub const BURST_DWELL_MS: u32 = 1_500;
/// Bins an energy map holds (the widest band, 423-440 MHz in 100 kHz bins,
/// is 170).
pub const ENERGY_BINS: usize = 192;
/// Energy maps kept at once (one per band in the region mask).
pub const ENERGY_MAPS: usize = 4;
/// Packet systems, in scan order.
pub const SYSTEMS: [System; 4] = [
    System::ExpressLrs,
    System::Mlrs,
    System::Sik,
    System::OpenLrsNg,
];
/// Links the node keeps counts for.
pub const MAX_SIGHTINGS: usize = 16;
/// A link's summary report at most this often.
pub const SUMMARY_GAP_US: u64 = 2_000_000;
/// Forget a link not heard for this long.
pub const SIGHTING_MAX_AGE_US: u64 = 120_000_000;
/// Decoded stick positions of one system at most this often (telemetry and
/// positions are rarer and always go).
pub const CHANNELS_GAP_US: u64 = 500_000;
/// `Frame::Control` frames a second, at most (all kinds together).
pub const CONTROL_PER_S: u32 = 20;
/// Without news, an energy frame per band at most this often.
pub const ENERGY_QUIET_GAP_US: u64 = 30_000_000;
/// Follow an ExpressLRS link this long after its sync packet, then go back
/// to scanning.
pub const FOLLOW_US: u64 = 20_000_000;
/// Without the hop table (UID bytes 2-3 unknown), stay on one channel this
/// long before trying another, to collect observations for recovery.
pub const PARK_US: u64 = 1_500_000;

// ------------------------------------------------------------- regions --

/// The region's bit in an `AeroMode::Control` mask.
pub fn region_bit(r: Region) -> u16 {
    link::control_region_bit(r)
}

/// Energy-map bin width for a region's band: no wider than half the
/// channel spacing the classifier needs (ELRS 900: 600 kHz; ELRS 2.4: 1 MHz
/// grid of 80 channels, so 1 MHz bins over 83.5 MHz), and few enough bins
/// for a sweep of ~0.3 s.
pub const fn energy_bin_hz(r: Region) -> u32 {
    match r {
        Region::Ism2g4 => 1_000_000,
        Region::Fcc915 | Region::Au915 | Region::Th920 => 200_000,
        Region::Eu868 | Region::In866 => 100_000,
        Region::Au433 | Region::Eu433 | Region::Us433 | Region::Us433W => 100_000,
    }
}

// --------------------------------------------------- chip configurations --

const fn lora_bw_code(b: LoRaBw) -> u8 {
    match b {
        LoRaBw::Khz125 => lora_bw::BW_125,
        LoRaBw::Khz250 => lora_bw::BW_250,
        LoRaBw::Khz500 => lora_bw::BW_500,
        LoRaBw::Khz812 => lora_bw::BW_812,
    }
}

const fn lora_cr_code(c: LoRaCr) -> u8 {
    match c {
        LoRaCr::Cr4_5 => lora_cr::CR_4_5,
        LoRaCr::Cr4_6 => lora_cr::CR_4_6,
        LoRaCr::Cr4_7 => lora_cr::CR_4_7,
        LoRaCr::Cr4_8 => lora_cr::CR_4_8,
        LoRaCr::Li4_5 => lora_cr::LI_4_5,
        LoRaCr::Li4_6 => lora_cr::LI_4_6,
        LoRaCr::Li4_8 => lora_cr::LI_4_8,
    }
}

/// A `ctrlwatch` sync word (MSB first, left-aligned in 4 bytes) as the
/// right-aligned value `cmd::set_fsk_sync_word` takes.
fn sync_value(word: [u8; 4], bits: u8) -> u64 {
    let bits = bits.min(32);
    (u32::from_be_bytes(word) as u64) >> (32 - bits as u32)
}

/// The chip configuration for one `ctrlwatch` receive configuration.
/// `None` for FLRC (ctrlwatch's FLRC configurations need the binding
/// phrase, so they are energy-only) and for a GFSK setting asking the radio
/// to de-whiten or check CRCs (the recognisers do both in software).
pub fn lr_config(rx: &ctrlwatch::RxConfig) -> Option<LrConfig> {
    let modem = match rx.modulation {
        Modulation::LoRa(l) => Modem::Lora {
            sf: l.sf,
            bw: lora_bw_code(l.bw),
            cr: lora_cr_code(l.cr),
            ldro: l.ldro,
            sync: l.sync_word,
            preamble: l.preamble_symbols,
            implicit_len: match l.header {
                LoRaHeader::Implicit { len } => Some(len),
                LoRaHeader::Explicit => None,
            },
            crc: l.crc_on,
            invert_iq: l.invert_iq,
            side: [0; 3],
        },
        Modulation::Gfsk(g) if !g.radio_whitening && !g.radio_crc => Modem::Fsk {
            bitrate: g.bitrate_bps,
            shaping: shaping::BT_0_5,
            rx_bw: bw::at_least(g.rx_bw_hz),
            fdev: g.fdev_hz,
            preamble_tx: g.preamble_bits,
            sync: sync_value(g.sync_word, g.sync_len_bits),
            sync_bits: g.sync_len_bits,
            len: g.read_len as u16,
        },
        Modulation::Energy { rx_bw_hz } => Modem::RssiOnly {
            rx_bw: bw::at_least(rx_bw_hz),
        },
        Modulation::Gfsk(_) | Modulation::Flrc(_) => return None,
    };
    Some(LrConfig {
        freq_hz: rx.freq_hz,
        modem,
    })
}

/// One receive for a group of steps: the first step's configuration with
/// the others' spreading factors on the side detectors. `None` if the
/// group is empty, the first step has no chip configuration, or a later
/// step cannot share the receive.
pub fn lr_group(steps: &[ScanStep]) -> Option<LrConfig> {
    let (first, rest) = steps.split_first()?;
    if rest.len() >= MAX_GROUP {
        return None;
    }
    let mut cfg = lr_config(&first.config)?;
    if let Modem::Lora { side, .. } = &mut cfg.modem {
        for (slot, s) in side.iter_mut().zip(rest) {
            if !ctrlwatch::scan::multi_sf_compatible(&first.config, &s.config) {
                return None;
            }
            let Modulation::LoRa(l) = s.config.modulation else {
                return None;
            };
            *slot = l.sf;
        }
    } else if !rest.is_empty() {
        return None;
    }
    Some(cfg)
}

/// Which step of a group a LoRa packet came in on, from the packet
/// status's detector bits (1 main, 2/4/8 side detectors 1-3). A status
/// with no bit set (or several) is taken as the main detector's.
pub fn detector_index(bits: u8) -> usize {
    match bits {
        2 => 1,
        4 => 2,
        8 => 3,
        _ => 0,
    }
}

// ------------------------------------------------------------- the scan --

/// One thing to do with the radio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Receive packets on these steps at once (one, or a multi-SF group)
    /// for the longest `dwell_ms`, stretched to the longest `extend_ms` once
    /// anything is recognised.
    Listen { steps: Vec<ScanStep, MAX_GROUP> },
    /// Sweep `region`'s band in `bin_hz` steps (receive bandwidth =
    /// `bin_hz`) for `window_ms`, then close the energy window.
    Sweep {
        region: Region,
        bin_hz: u32,
        window_ms: u32,
    },
}

impl Job {
    pub fn dwell_ms(&self) -> u32 {
        match self {
            Job::Listen { steps } => steps.iter().map(|s| s.dwell_ms).max().unwrap_or(0),
            Job::Sweep { window_ms, .. } => *window_ms,
        }
    }

    pub fn extend_ms(&self) -> u32 {
        match self {
            Job::Listen { steps } => steps
                .iter()
                .map(|s| s.extend_ms)
                .max()
                .unwrap_or(0)
                .max(self.dwell_ms()),
            Job::Sweep { window_ms, .. } => *window_ms,
        }
    }
}

/// Walks the control-link scan (see the module docs). [`Scanner::current`]
/// names the job at the cursor; [`Scanner::done`] moves past it once it ran
/// to its end, so a job cut short by the end of a plan slot is tried again
/// next time.
#[derive(Debug, Clone)]
pub struct Scanner {
    regions: u16,
    /// Bands swept for energy only.
    energy: u16,
    /// Cursor: region index (into `Region::ALL`), system, step.
    r: u8,
    sys: u8,
    step: u32,
    listens: u8,
    /// Next region to sweep (index into `Region::ALL`).
    sweep_r: u8,
    /// Completed passes through every packet plan.
    pub cycles: u32,
}

impl Scanner {
    /// `regions`: packet plans and energy sweeps; `energy`: sweeps only
    /// (`AeroMode::Control`'s two masks).
    pub const fn new(regions: u16, energy: u16) -> Self {
        const ALL: u16 = (1 << Region::ALL.len()) - 1;
        Self {
            regions: regions & ALL,
            energy: energy & ALL,
            r: 0,
            sys: 0,
            step: 0,
            listens: 0,
            sweep_r: 0,
            cycles: 0,
        }
    }

    pub fn masks(&self) -> (u16, u16) {
        (self.regions, self.energy)
    }

    /// New masks: the cycle starts over when they changed.
    pub fn set_masks(&mut self, regions: u16, energy: u16) {
        if Self::new(regions, energy).masks() != self.masks() {
            *self = Self::new(regions, energy);
        }
    }

    fn in_mask(&self, i: u8) -> bool {
        (i as usize) < Region::ALL.len() && self.regions & (1 << i) != 0
    }

    /// The next region in the mask at or after index `from` (wrapping).
    fn next_region(&self, from: u8) -> Option<u8> {
        let n = Region::ALL.len() as u8;
        (0..n).map(|k| (from + k) % n).find(|&i| self.in_mask(i))
    }

    /// The next band to sweep (either mask) at or after `from`.
    fn next_sweep(&self, from: u8) -> Option<u8> {
        let n = Region::ALL.len() as u8;
        (0..n)
            .map(|k| (from + k) % n)
            .find(|&i| (self.regions | self.energy) & (1 << i) != 0)
    }

    /// The packet-plan steps at the cursor, if any (not normalised).
    fn steps_at(&self) -> Option<Vec<ScanStep, MAX_GROUP>> {
        if !self.in_mask(self.r) {
            return None;
        }
        let region = Region::ALL[self.r as usize];
        let mut plan = ctrlwatch::scan::plan(*SYSTEMS.get(self.sys as usize)?, region)
            .skip(self.step as usize);
        let first = plan.next()?;
        let mut v: Vec<ScanStep, MAX_GROUP> = Vec::new();
        let _ = v.push(first);
        // Only steps the chip can take join a group; one it cannot (FLRC)
        // is listened to alone and skipped by the node.
        if lr_config(&first.config).is_some() {
            for s in plan {
                if v.is_full() || !ctrlwatch::scan::multi_sf_compatible(&first.config, &s.config) {
                    break;
                }
                let _ = v.push(s);
            }
        }
        Some(v)
    }

    /// Move the cursor to a position that has steps (or give up after one
    /// full pass: the mask has no packet plans).
    fn normalise(&mut self) -> bool {
        let total = Region::ALL.len() * SYSTEMS.len() + 1;
        for _ in 0..total {
            if self.steps_at().is_some() {
                return true;
            }
            self.step = 0;
            self.sys += 1;
            if self.sys as usize >= SYSTEMS.len() {
                self.sys = 0;
                let n = Region::ALL.len() as u8;
                let next = self.next_region(self.r + 1);
                match next {
                    Some(i) if i <= self.r => {
                        self.cycles = self.cycles.wrapping_add(1);
                        self.r = i;
                    }
                    Some(i) => self.r = i,
                    None => {
                        self.r = n;
                        return false;
                    }
                }
            }
        }
        false
    }

    /// The job at the cursor; `None` with an empty mask.
    pub fn current(&mut self) -> Option<Job> {
        if self.regions | self.energy == 0 {
            return None;
        }
        let has_packets = self.regions != 0 && {
            if !self.in_mask(self.r) {
                self.r = self.next_region(self.r).unwrap_or(0);
                self.sys = 0;
                self.step = 0;
            }
            self.normalise()
        };
        if self.listens >= LISTENS_PER_SWEEP || !has_packets {
            let i = self.next_sweep(self.sweep_r)?;
            let region = Region::ALL[i as usize];
            return Some(Job::Sweep {
                region,
                bin_hz: energy_bin_hz(region),
                window_ms: SWEEP_WINDOW_MS,
            });
        }
        Some(Job::Listen {
            steps: self.steps_at()?,
        })
    }

    /// The job [`Scanner::current`] named ran to its end.
    pub fn done(&mut self) {
        match self.current() {
            Some(Job::Sweep { region, .. }) => {
                self.listens = 0;
                let i = Region::ALL.iter().position(|r| *r == region).unwrap_or(0) as u8;
                self.sweep_r = (i + 1) % Region::ALL.len() as u8;
            }
            Some(Job::Listen { steps }) => {
                self.step += steps.len() as u32;
                self.listens = self.listens.saturating_add(1);
            }
            None => {}
        }
    }
}

// --------------------------------------------------------------- energy --

/// One band's energy map.
pub type EnergyMap = ctrlwatch::energy::EnergyMap<ENERGY_BINS>;

/// The energy maps of the bands in the mask, made on first use (the oldest
/// is dropped when a fifth band comes along).
pub struct EnergyMaps {
    maps: [Option<(Region, EnergyMap)>; ENERGY_MAPS],
}

impl Default for EnergyMaps {
    fn default() -> Self {
        Self::new()
    }
}

impl EnergyMaps {
    pub const fn new() -> Self {
        Self {
            maps: [None, None, None, None],
        }
    }

    /// The map for `region` (a new one if there is none).
    pub fn get(&mut self, region: Region) -> &mut EnergyMap {
        let i = match self
            .maps
            .iter()
            .position(|m| m.as_ref().is_some_and(|m| m.0 == region))
        {
            Some(i) => i,
            None => {
                let i = self
                    .maps
                    .iter()
                    .position(|m| m.is_none())
                    .unwrap_or_else(|| {
                        self.maps.rotate_left(1);
                        ENERGY_MAPS - 1
                    });
                self.maps[i] = Some((region, EnergyMap::for_region(region, energy_bin_hz(region))));
                i
            }
        };
        // Just filled if it was empty.
        &mut self.maps[i].as_mut().unwrap_or_else(|| unreachable!()).1
    }
}

fn khz(hz: u32) -> u32 {
    hz / 1000
}

fn dbm8(v: i16) -> i8 {
    v.clamp(i8::MIN as i16, i8::MAX as i16) as i8
}

/// The link frame for one energy window; `burst` from a dwell on one of a
/// hopper's channels, if the node made one.
pub fn energy_frame(
    region: Region,
    rep: &EnergyReport,
    burst: Option<&BurstSummary>,
    now_us: u64,
) -> ControlEnergy {
    let activity = match rep.activity {
        Activity::Quiet => EnergyActivity::Quiet,
        Activity::Steady => EnergyActivity::Steady,
        Activity::Background => EnergyActivity::Background,
        Activity::Hopper(h) => EnergyActivity::Hopper(HopperReport {
            low_khz: khz(h.low_hz),
            high_khz: khz(h.high_hz),
            channels: h.channels_seen,
            spacing_khz: h.spacing_hz.map(khz),
            occupied_khz: khz(h.occupied_bw_hz),
            rssi: dbm8(h.rssi_dbm),
            spread_db: dbm8(h.spread_db),
            burst_us: burst.map(|b| b.median_len_us),
            interval_us: burst.and_then(|b| b.interval_us),
            periodic_pct: burst.map_or(0, |b| b.periodic_pct),
            candidates: ctrlwatch::energy::classify(&h, burst)
                .iter()
                .take(4)
                .map(|c| (c.system, c.score))
                .collect(),
        }),
    };
    ControlEnergy {
        region,
        windows: rep.windows,
        activity,
        steady: rep
            .steady
            .iter()
            .take(4)
            .map(|s| (khz(s.freq_hz), dbm8(s.rssi_dbm)))
            .collect(),
        uptime_us: now_us,
    }
}

/// When an energy window is worth a frame: a hopper or steady carriers at
/// once, otherwise every [`ENERGY_QUIET_GAP_US`] per band.
#[derive(Debug, Clone, Default)]
pub struct EnergyGate {
    last: [Option<u64>; 10],
}

impl EnergyGate {
    pub const fn new() -> Self {
        Self { last: [None; 10] }
    }

    pub fn due(&mut self, region: Region, rep: &EnergyReport, now_us: u64) -> bool {
        let i = Region::ALL.iter().position(|r| *r == region).unwrap_or(0);
        let news = matches!(rep.activity, Activity::Hopper(_)) || !rep.steady.is_empty();
        let quiet_due =
            self.last[i].is_none_or(|t| now_us.saturating_sub(t) >= ENERGY_QUIET_GAP_US);
        if news || quiet_due {
            self.last[i] = Some(now_us);
            return true;
        }
        false
    }
}

// -------------------------------------------------------------- reports --

/// A link's running summary (`part` 0): counts, no content, no raw bytes.
pub fn summary(s: &Sighting) -> ControlReport {
    ControlReport {
        sighting: s.last,
        best: s.best,
        packets: s.packets,
        confirmed: s.confirmed,
        peak_rssi: s.peak_rssi_dbm,
        part: 0,
        content: Vec::new(),
        raw: None,
    }
}

/// One packet's decoded content as reports (`part` 1, 2, ...), at most
/// [`MAX_CONTROL_CONTENT`] items each, the raw bytes in the first. `counts`
/// is the link's entry in the node's table (for the running counts). With
/// no content and no raw bytes, nothing is sent.
pub fn content_reports(
    sighting: &ControlSighting,
    counts: Option<&Sighting>,
    content: &[ControlContent],
    raw: Option<&[u8]>,
    mut out: impl FnMut(ControlReport),
) {
    if content.is_empty() && raw.is_none() {
        return;
    }
    let base = ControlReport {
        sighting: *sighting,
        best: counts.map_or(sighting.confidence, |c| c.best),
        packets: counts.map_or(1, |c| c.packets),
        confirmed: counts.map_or(0, |c| c.confirmed),
        peak_rssi: counts.map_or(sighting.rssi_dbm, |c| c.peak_rssi_dbm),
        part: 1,
        content: Vec::new(),
        raw: raw.and_then(|r| Vec::from_slice(&r[..r.len().min(ctrlwatch::MAX_RAW)]).ok()),
    };
    let mut chunks = content.chunks(MAX_CONTROL_CONTENT);
    let mut part = 1u8;
    let mut first = true;
    loop {
        let items = chunks.next();
        if items.is_none() && !first {
            break;
        }
        let mut r = base.clone();
        r.part = part;
        if !first {
            r.raw = None;
        }
        for c in items.unwrap_or(&[]) {
            let _ = r.content.push(c.clone());
        }
        out(r);
        first = false;
        part = part.saturating_add(1);
    }
}

/// Keeps decoded stick positions to [`CHANNELS_GAP_US`] per system;
/// telemetry, positions and opaque markers always pass.
#[derive(Debug, Clone, Default)]
pub struct ContentGate {
    last_channels: [Option<u64>; 10],
}

impl ContentGate {
    pub const fn new() -> Self {
        Self { last_channels: [None; 10] }
    }

    /// Drop the items that should not go out now; true if anything is left.
    pub fn filter<const N: usize>(
        &mut self,
        system: System,
        content: &mut Vec<ControlContent, N>,
        now_us: u64,
    ) -> bool {
        let i = system as usize % 10;
        let due = self.last_channels[i].is_none_or(|t| now_us.saturating_sub(t) >= CHANNELS_GAP_US);
        let had = content
            .iter()
            .any(|c| matches!(c, ControlContent::Channels(_)));
        if !due {
            content.retain(|c| !matches!(c, ControlContent::Channels(_)));
        } else if had {
            self.last_channels[i] = Some(now_us);
        }
        !content.is_empty()
    }
}

// --------------------------------------------------------------- follow --

/// Where [`Follow`] wants the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowTarget {
    pub config: ctrlwatch::RxConfig,
    /// Hop-table channel index of `config.freq_hz`.
    pub channel: u8,
    /// When the next packet is due (hopping), or `None` (parked).
    pub at_us: Option<u64>,
}

/// ExpressLRS follow mode for one link, started from a checked SYNC
/// packet (`ctrlwatch::elrs::Follower::from_sync`, needs `decode_content`).
///
/// With the hop table (only after recovery, which needs `read_ids` too)
/// the node hops with the link ([`Follower::next_rx`]). Without it, it
/// parks: on the sync channel, and with `read_ids` on a different channel
/// every [`PARK_US`], so packets elsewhere in the table give the
/// observations [`Follower::recover`] needs.
#[derive(Debug, Clone)]
pub struct Follow {
    pub f: Follower,
    pub until_us: u64,
    read_ids: bool,
    count: u8,
    sync_ch: u8,
    park_k: u8,
    park_since_us: u64,
    /// Packets checked and decoded.
    pub packets: u32,
    /// Recovery attempts and successes.
    pub recoveries: u16,
    pub recovered: bool,
}

/// Observations to collect before a recovery attempt (docs: about 2
/// settle 3.x on FCC915, 4 settle 4.x).
const fn obs_needed(v: OtaVersion) -> usize {
    match v {
        OtaVersion::V3 => 2,
        OtaVersion::V4 => 4,
    }
}

impl Follow {
    /// From a packet `recognise` called a confirmed ExpressLRS SYNC on
    /// `rx`. `None` without `decode_content`, or if the packet does not
    /// parse as a SYNC of the probe's version.
    pub fn start(cfg: &Config, pkt: &[u8], rx: &ctrlwatch::RxConfig, t_us: u64) -> Option<Self> {
        let Probe::Elrs { rate, version } = rx.probe else {
            return None;
        };
        let info = rate.info();
        let iq = match rx.modulation {
            Modulation::LoRa(l) => (!info.sub_ghz).then_some(l.invert_iq),
            _ => return None,
        };
        let dom = elrs::domain(rx.region, version);
        let s = elrs::parse_sync(pkt, version, &dom, iq, info.sub_ghz)?;
        let f = Follower::from_sync(cfg, &s, rx.region, rate, t_us)?;
        Some(Self {
            f,
            until_us: t_us + FOLLOW_US,
            read_ids: cfg.read_ids,
            count: dom.count,
            sync_ch: elrs::sync_channel(rx.region, version),
            park_k: 0,
            park_since_us: t_us,
            packets: 0,
            recoveries: 0,
            recovered: false,
        })
    }

    pub fn expired(&self, now_us: u64) -> bool {
        now_us >= self.until_us
    }

    pub fn rate(&self) -> ElrsRate {
        self.f.rate
    }

    fn rx_on(&self, ch: u8) -> Option<ctrlwatch::RxConfig> {
        let dom = elrs::domain(self.f.region, self.f.version);
        elrs::lora_rx(
            self.f.rate,
            dom.freq(ch),
            self.f.invert_iq(),
            self.f.region,
            self.f.version,
        )
    }

    /// Hop-table channel of a frequency in this link's domain.
    pub fn channel_of(&self, freq_hz: u32) -> u8 {
        let dom = elrs::domain(self.f.region, self.f.version);
        let sp = dom.spread_hz().max(1);
        (freq_hz.saturating_sub(dom.start_hz) + sp / 2)
            .checked_div(sp)
            .unwrap_or(0)
            .min(dom.count as u32 - 1) as u8
    }

    /// Where to listen now.
    pub fn target(&mut self, now_us: u64) -> Option<FollowTarget> {
        if let Some((t, f, rx)) = self.f.next_rx(now_us) {
            return Some(FollowTarget {
                config: rx,
                channel: self.channel_of(f),
                at_us: Some(t),
            });
        }
        if self.read_ids && now_us.saturating_sub(self.park_since_us) >= PARK_US {
            self.park_since_us = now_us;
            self.park_k = self.park_k.wrapping_add(1);
        }
        // k = 0: the sync channel; then a stride of 7 through the others
        // (40 and 80 channels: 7 is coprime, so every channel comes round).
        let ch = if self.park_k == 0 || self.count <= 1 {
            self.sync_ch
        } else {
            let n = self.count as u32;
            let c = (self.sync_ch as u32 + self.park_k as u32 * 7) % n;
            (if c == self.sync_ch as u32 {
                (c + 1) % n
            } else {
                c
            }) as u8
        };
        Some(FollowTarget {
            config: self.rx_on(ch)?,
            channel: ch,
            at_us: None,
        })
    }

    /// A packet received while following on hop-table channel `channel`.
    /// Tries to recover the hop table once enough observations are in
    /// (only with `read_ids`; `Follower::recover` checks it too).
    pub fn process(&mut self, pkt: &[u8], meta: RxMeta, channel: u8) -> Option<Followed> {
        let r = self.f.process(pkt, meta, channel)?;
        self.packets = self.packets.wrapping_add(1);
        if self.read_ids
            && !self.f.has_sequence()
            && self.f.observations().len() >= obs_needed(self.f.version)
        {
            self.recoveries = self.recoveries.wrapping_add(1);
            if let Recovery::Unique { .. } = self.f.recover() {
                self.recovered = true;
            }
        }
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use ctrlwatch::scan;

    #[test]
    fn every_plan_step_maps_to_the_chip() {
        for region in Region::ALL {
            for sys in SYSTEMS {
                for s in scan::plan(sys, region) {
                    let c = lr_config(&s.config)
                        .unwrap_or_else(|| panic!("{sys:?} {region:?} {:?}", s.config));
                    assert_eq!(c.freq_hz, s.config.freq_hz);
                    let seq = config::apply(
                        &crate::config::Board {
                            tcxo: None,
                            irq_dio: 9,
                            rf_switch: &[],
                            rx_boost: 7,
                        },
                        &c,
                    );
                    assert!(seq.len() < seq.capacity());
                    let hf = s.config.freq_hz > 2_000_000_000;
                    assert_eq!(c.path() == config::Path::Hf, hf);
                }
            }
            for s in scan::energy_plan(region, energy_bin_hz(region)) {
                assert!(matches!(
                    lr_config(&s.config).unwrap().modem,
                    Modem::RssiOnly { .. }
                ));
            }
        }
    }

    #[test]
    fn elrs_900_multi_sf_group() {
        let steps: std::vec::Vec<_> = scan::plan(System::ExpressLrs, Region::Fcc915).collect();
        // 200/100/50/25 Hz: SF6/7/8/9, one receive.
        let g = lr_group(&steps[..4]).unwrap();
        let Modem::Lora {
            sf,
            bw,
            cr,
            side,
            implicit_len,
            crc,
            sync,
            invert_iq,
            ..
        } = g.modem
        else {
            panic!()
        };
        assert_eq!((sf, side), (6, [7, 8, 9]));
        assert_eq!(
            (bw, cr, implicit_len, crc, sync, invert_iq),
            (
                lora_bw::BW_500,
                lora_cr::CR_4_7,
                Some(8),
                false,
                0x12,
                false
            )
        );
        assert!(
            lr_group(&steps[3..5]).is_none(),
            "250 Hz is CR 4/8: not one receive"
        );
        let seq = config::apply(
            &crate::config::Board {
                tcxo: None,
                irq_dio: 9,
                rf_switch: &[],
                rx_boost: 0,
            },
            &g,
        );
        assert!(
            seq.iter()
                .any(|c| c.as_slice() == [0x02, 0x24, 0x70, 0x80, 0x90])
        );
        assert!(
            seq.iter()
                .any(|c| c.as_slice() == [0x02, 0x25, 0x12, 0x12, 0x12])
        );
        assert_eq!(
            (
                detector_index(1),
                detector_index(2),
                detector_index(4),
                detector_index(8),
                detector_index(0)
            ),
            (0, 1, 2, 3, 0)
        );
    }

    #[test]
    fn gfsk_and_energy_settings() {
        let sik = scan::plan(System::Sik, Region::Fcc915).next().unwrap();
        let Modem::Fsk {
            sync,
            sync_bits,
            len,
            bitrate,
            rx_bw,
            ..
        } = lr_config(&sik.config).unwrap().modem
        else {
            panic!()
        };
        assert_eq!((sync, sync_bits), (0x2DD4, 16));
        assert_eq!(bitrate, 64_000);
        assert!(len >= 8);
        assert!(bw::TABLE.iter().any(|e| e.1 == rx_bw));
        let e = scan::energy_plan(Region::Ism2g4, 1_000_000).next().unwrap();
        assert_eq!(
            lr_config(&e.config).unwrap().modem,
            Modem::RssiOnly { rx_bw: 200 }
        );
        assert_eq!(sync_value([0x2D, 0xD4, 0, 0], 16), 0x2DD4);
        assert_eq!(sync_value([0xAB, 0xCD, 0xEF, 0x12], 32), 0xABCD_EF12);
    }

    fn mask(rs: &[Region]) -> u16 {
        rs.iter().map(|r| region_bit(*r)).sum()
    }

    /// Walk one whole cycle; every packet step comes up once, sweeps come
    /// between, and the cycle time is what the docs say.
    #[test]
    fn scanner_cycle() {
        // The US default: packet plans on 915, 433 and 2.4 GHz, energy only
        // on 868.
        let mut sc = Scanner::new(
            mask(&[Region::Fcc915, Region::Us433, Region::Ism2g4]),
            mask(&[Region::Eu868]),
        );
        let mut listens = 0usize;
        let mut sweeps = std::vec::Vec::new();
        let mut steps = 0usize;
        let mut total_ms = 0u64;
        let mut in_row = 0;
        loop {
            let j = sc.current().unwrap();
            if sc.cycles > 0 {
                break;
            }
            total_ms += j.dwell_ms() as u64;
            match &j {
                Job::Listen { steps: s } => {
                    listens += 1;
                    steps += s.len();
                    in_row += 1;
                    assert!(in_row <= LISTENS_PER_SWEEP);
                }
                Job::Sweep { region, .. } => {
                    sweeps.push(*region);
                    in_row = 0;
                }
            }
            sc.done();
            assert!(listens < 1000);
        }
        let expect: usize = [Region::Fcc915, Region::Us433, Region::Ism2g4]
            .iter()
            .flat_map(|r| SYSTEMS.iter().map(move |s| scan::plan(*s, *r).count()))
            .sum();
        assert_eq!(steps, expect);
        assert!(listens < steps, "multi-SF groups merge steps");
        // Sweeps rotate through all four bands.
        for r in [Region::Fcc915, Region::Eu868, Region::Us433, Region::Ism2g4] {
            assert!(sweeps.contains(&r), "{r:?}");
        }
        std::println!(
            "control cycle: {listens} listens ({steps} steps), {} sweeps, {:.0} s of dwell",
            sweeps.len(),
            total_ms as f64 / 1000.0
        );
        assert!(total_ms < 600_000, "{total_ms}");
    }

    #[test]
    fn scanner_edge_cases() {
        let mut sc = Scanner::new(0, 0);
        assert_eq!(sc.current(), None);
        // TH920 has ELRS only: still works.
        let mut sc = Scanner::new(region_bit(Region::Th920), 0);
        for _ in 0..50 {
            assert!(sc.current().is_some());
            sc.done();
        }
        // Energy only: sweeps, nothing else.
        let mut sc = Scanner::new(0, mask(&[Region::Eu868, Region::Ism2g4]));
        let mut seen = std::vec::Vec::new();
        for _ in 0..4 {
            let Some(Job::Sweep { region, .. }) = sc.current() else {
                panic!()
            };
            seen.push(region);
            sc.done();
        }
        assert_eq!(
            seen,
            [Region::Eu868, Region::Ism2g4, Region::Eu868, Region::Ism2g4]
        );
        // A job not marked done comes back.
        let mut sc = Scanner::new(region_bit(Region::Fcc915), 0);
        let a = sc.current();
        assert_eq!(sc.current(), a);
        sc.done();
        assert_ne!(sc.current(), a);
        sc.set_masks(region_bit(Region::Fcc915), 0);
        assert_ne!(sc.current(), a, "same mask: no restart");
        sc.set_masks(region_bit(Region::Ism2g4), 0);
        let Some(Job::Listen { steps }) = sc.current() else {
            panic!()
        };
        assert_eq!(steps[0].config.region, Region::Ism2g4);
    }

    fn sighting(system: System) -> ControlSighting {
        ControlSighting {
            system,
            region: Region::Fcc915,
            probe: Probe::Energy,
            freq_hz: 915_500_000,
            rssi_dbm: -70,
            snr_db: 6,
            t_us: 1,
            confidence: ctrlwatch::Confidence::Confirmed,
            direction: ctrlwatch::Direction::Uplink,
            id: None,
            version: Some(4),
        }
    }

    #[test]
    fn content_split_and_gate() {
        use ctrlwatch::content::{Channels, Telemetry};
        let s = sighting(System::ExpressLrs);
        let items: std::vec::Vec<ControlContent> = (0..5)
            .map(|i| {
                if i % 2 == 0 {
                    ControlContent::Channels(Channels::default())
                } else {
                    ControlContent::Telemetry(Telemetry::Vario { cms: i })
                }
            })
            .collect();
        let mut got = std::vec::Vec::new();
        content_reports(&s, None, &items, Some(&[1, 2, 3]), |r| got.push(r));
        assert_eq!(
            got.iter()
                .map(|r| (r.part, r.content.len(), r.raw.is_some()))
                .collect::<std::vec::Vec<_>>(),
            [(1, 2, true), (2, 2, false), (3, 1, false)]
        );
        // Raw bytes alone (a confirmed packet with nothing decoded).
        let mut got = std::vec::Vec::new();
        content_reports(&s, None, &[], Some(&[9; 8]), |r| got.push(r));
        assert_eq!((got.len(), got[0].part, got[0].content.len()), (1, 1, 0));
        content_reports(&s, None, &[], None, |_| panic!("nothing to send"));
        // Every frame fits.
        let mut buf = [0u8; link::MAX_FRAME];
        content_reports(&s, None, &items, Some(&[0xFF; 200]), |r| {
            link::encode(&link::Frame::Control(r), &mut buf).unwrap();
        });

        let mut g = ContentGate::default();
        let mut v: Vec<ControlContent, 4> = items[..2].iter().cloned().collect();
        assert!(g.filter(System::ExpressLrs, &mut v, 0));
        assert_eq!(v.len(), 2);
        let mut v: Vec<ControlContent, 4> = items[..2].iter().cloned().collect();
        assert!(g.filter(System::ExpressLrs, &mut v, 100_000));
        assert_eq!(v.len(), 1, "channels dropped, telemetry kept");
        let mut v: Vec<ControlContent, 4> = items[..1].iter().cloned().collect();
        assert!(!g.filter(System::ExpressLrs, &mut v, 200_000));
        let mut v: Vec<ControlContent, 4> = items[..1].iter().cloned().collect();
        assert!(g.filter(System::Mlrs, &mut v, 200_000), "per system");
        assert!(g.filter(
            System::ExpressLrs,
            &mut items[..1].iter().cloned().collect::<Vec<_, 4>>(),
            600_000
        ));
    }

    #[test]
    fn summary_and_energy_frames() {
        let mut t: ctrlwatch::Sightings<4> = ctrlwatch::Sightings::new();
        t.record(&sighting(System::Sik));
        let mut n = 0;
        t.take_due(10, SUMMARY_GAP_US, |s| {
            let r = summary(s);
            assert_eq!((r.part, r.packets, r.content.len(), r.raw), (0, 1, 0, None));
            n += 1;
        });
        assert_eq!(n, 1);

        let mut m = EnergyMaps::new();
        for (i, r) in [
            Region::Fcc915,
            Region::Eu868,
            Region::Us433,
            Region::Ism2g4,
            Region::Au915,
        ]
        .iter()
        .enumerate()
        {
            let map = m.get(*r);
            assert_eq!(map.bin_hz, energy_bin_hz(*r));
            map.add(scan::band(*r).0 + 1000, -100 + i as i16);
        }
        assert_eq!(m.get(Region::Au915).start_hz, scan::band(Region::Au915).0);
        let rep = m.get(Region::Ism2g4).end_window();
        let f = energy_frame(Region::Ism2g4, &rep, None, 5);
        assert_eq!((f.region, f.windows, f.uptime_us), (Region::Ism2g4, 1, 5));
        let mut g = EnergyGate::default();
        assert!(g.due(Region::Ism2g4, &rep, 0));
        assert!(!g.due(Region::Ism2g4, &rep, 1_000_000));
        assert!(g.due(Region::Fcc915, &rep, 1_000_000));
        assert!(g.due(Region::Ism2g4, &rep, ENERGY_QUIET_GAP_US));
    }

    /// A 4.x OTA4 SYNC packet built by hand from the ELRS layout
    /// (docs/CONTROL-LINKS.md): type 2, fhssIndex, nonce, rate enum,
    /// flags, UID4, UID5, CRC-14 seeded from UID4/UID5 ^ (4 << 8).
    fn sync_packet(fhss: u8, nonce: u8, uid4: u8, uid5: u8) -> [u8; 8] {
        let mut p = [2u8, fhss, nonce, 5, (3 << 1), uid4, uid5, 0];
        let init = ((uid4 as u16) << 8 | uid5 as u16) ^ (4 << 8);
        let crc = ctrlwatch::crc::ELRS_CRC14.calc(&p[..7], init);
        p[0] |= ((crc >> 8) as u8) << 2;
        p[7] = crc as u8;
        p
    }

    #[test]
    fn follow_needs_content_and_parks() {
        let rx = scan::plan(System::ExpressLrs, Region::Fcc915)
            .next()
            .unwrap()
            .config;
        assert!(matches!(
            rx.probe,
            Probe::Elrs {
                rate: ElrsRate::L900_200,
                version: OtaVersion::V4
            }
        ));
        let pkt = sync_packet(40, 17, 0xAB, 0xCD);
        let meta = RxMeta {
            rssi_dbm: -60,
            snr_db: 8,
            t_us: 1_000,
        };
        let r = ctrlwatch::recognise(&pkt, &rx, meta, &Config::default()).unwrap();
        assert_eq!(r.sighting.confidence, ctrlwatch::Confidence::Confirmed);
        assert!(
            Follow::start(&Config::default(), &pkt, &rx, 1_000).is_none(),
            "content off: no following"
        );
        let content = Config {
            read_ids: false,
            decode_content: true,
        };
        let mut f = Follow::start(&content, &pkt, &rx, 1_000).unwrap();
        assert!(!f.f.has_sequence());
        // Parked on the sync channel, and stays there without read_ids.
        let t = f.target(2_000).unwrap();
        assert_eq!(
            (t.channel, t.config.freq_hz, t.at_us),
            (20, 915_500_000, None)
        );
        assert_eq!(f.target(10_000_000).unwrap().channel, 20);
        assert_eq!(f.channel_of(915_500_000), 20);
        assert_eq!(f.channel_of(903_500_000), 0);
        // Its own next sync re-anchors.
        assert!(
            f.process(
                &pkt,
                RxMeta {
                    t_us: 5_001_000,
                    ..meta
                },
                20
            )
            .is_some_and(|x| x.is_sync)
        );
        assert!(!f.expired(FOLLOW_US) && f.expired(FOLLOW_US + 1_000));
        // With read_ids it walks other channels to collect observations.
        let both = Config {
            read_ids: true,
            decode_content: true,
        };
        let mut f = Follow::start(&both, &pkt, &rx, 1_000).unwrap();
        let a = f.target(2_000).unwrap().channel;
        let b = f.target(2_000 + PARK_US).unwrap().channel;
        let c = f.target(2_000 + 2 * PARK_US).unwrap().channel;
        assert_eq!(a, 20);
        assert!(b != 20 && c != 20 && b != c);
    }

    /// Receive only, for the control scan too: no chip sequence any scan,
    /// sweep or follow configuration produces holds a transmit opcode, and
    /// the node firmware's radio code builds no command bytes of its own
    /// (every command comes from this crate).
    #[test]
    fn control_scan_is_receive_only() {
        const TX: [[u8; 2]; 8] = [[2, 0x0D], [2, 0x03], [2, 0x02], [2, 0x0F], [2, 0x0E], [2, 0x11], [2, 0x1B], [0, 0x02]];
        let board = crate::config::Board { tcxo: None, irq_dio: 9, rf_switch: &[], rx_boost: 7 };
        let mut n = 0;
        let mut check = |c: &LrConfig| {
            for seq in [config::apply(&board, c), config::retune(c.freq_hz)] {
                for cmd in &seq {
                    assert!(!TX.contains(&[cmd[0], cmd[1]]), "transmit opcode {:02x?}", &cmd[..2]);
                    n += 1;
                }
            }
        };
        for region in Region::ALL {
            let mut sc = Scanner::new(region_bit(region), region_bit(region));
            for _ in 0..200 {
                match sc.current().unwrap() {
                    Job::Listen { steps } => {
                        if let Some(c) = lr_group(&steps) {
                            check(&c);
                        }
                    }
                    Job::Sweep { region, bin_hz, .. } => {
                        for s in scan::energy_plan(region, bin_hz) {
                            check(&lr_config(&s.config).unwrap());
                        }
                    }
                }
                sc.done();
            }
        }
        assert!(n > 1000);
        // The firmware side: no opcode literals outside lr2021cfg.
        for (name, src) in [
            ("control.rs", include_str!("../../../firmware/node-lr/src/control.rs")),
            ("radio.rs", include_str!("../../../firmware/node-lr/src/radio.rs")),
            ("lr.rs", include_str!("../../../firmware/node-lr/src/lr.rs")),
        ] {
            for op in ["0x02, 0x0D", "0x02, 0x03", "0x02, 0x02", "0x02, 0x0F", "0x02, 0x0E", "0x02, 0x11", "0x02, 0x1B", "0x00, 0x02", "[2, 0x0D"] {
                assert!(!src.contains(op), "{name}: {op}");
            }
        }
    }
}
