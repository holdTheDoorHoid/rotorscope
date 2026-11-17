//! What radio nodes tell the hub.
//!
//! Each frame is a [`Frame`] serialized with postcard and COBS-delimited (a
//! zero byte ends every frame), so a reader that joins mid-stream resyncs at
//! the next zero. Nodes send *raw* Remote ID messages; the hub decodes them
//! with the `rid` crate. Open FPV / telemetry links (wfb-ng, OpenHD, RubyFPV,
//! DroneBridge) are classified on the node by the `openlink` crate and sent
//! as [`Frame::OpenLink`] sightings, rate-limited per link.
#![cfg_attr(not(test), no_std)]

pub mod forward;

/// The control-link types `Frame::Control` carries (protocol 12).
pub use ctrlwatch;

use heapless::Vec;
use rid::{MAX_PACK, RawMessage, Transport};
use serde::{Deserialize, Serialize};

/// Bump when a change would make an old hub misread a new node.
///
/// - 1: Hello, Detection, Stats.
/// - 2: adds [`Frame::OpenLink`]. Appended as the last variant, so v1 frames
///   encode exactly as before; a v1 hub counts a v2 `OpenLink` as corrupt and
///   carries on.
/// - 3: adds [`Frame::Via`], written by an ESP-NOW gateway (appended likewise;
///   older hubs count it as corrupt and still read the relayed frame).
/// - 4: adds [`Frame::Status`], [`Frame::DroneId`], [`Frame::Rf`],
///   [`Frame::DjiWifi`], [`Frame::WifiDrone`] (node to hub) and the hub-to-node
///   [`Command`] stream; [`NodeRole`] gains `DroneId` and `Gateway`.
/// - 5: adds [`Frame::HubFix`] and [`Frame::Csbee`], sent by a hub to a "web
///   head" (a second board that serves the phone page; see
///   `firmware/hub-p4/WEBHEAD.md`). Appended likewise; nodes never send them.
/// - 6: adds [`Frame::Flags`] (hub to web head: possible FAA rule violations,
///   PLAN.md Phase 12). Appended likewise.
/// - 7: [`RfKind`] gains `DjiVideo`, `DjiO4Video`, `DjiFpvAirUnit` and
///   `DjiController` (PLAN.md Phase 11b, `crates/djilink`). Appended, so
///   every v6 `Rf` frame encodes exactly as before; a v6 hub counts an `Rf`
///   frame carrying a new kind as corrupt and carries on.
/// - 8: [`DroneIdRecord`] gains `O4` (record type, key ID and the raw
///   CRC-valid logical object, PLAN.md Phase 10, docs/DRONEID-O4.md).
///   Appended, so every v7 `DroneId` frame encodes exactly as before; a v7
///   hub counts a `DroneId` frame carrying an `O4` record as corrupt and
///   carries on. Nodes from v8 send `O4` instead of `Encrypted`.
/// - 9: adds [`Frame::Threat`]: attacks on Wi-Fi or Bluetooth (deauth floods,
///   beacon spam, evil twins...) and jamming on drone bands, as seen by a
///   node. Appended, so every v8 frame encodes exactly as before; a v8 hub
///   counts a `Threat` frame as corrupt and carries on.
/// - 10: the wired bus (`crates/wirebus`, docs/WIREBUS.md): many nodes on one
///   hub UART, polled in turn. Adds [`Command::Poll`], [`Command::JoinSlot`]
///   and [`Command::Address`] (hub to nodes) and [`Frame::BusJoin`] and
///   [`Frame::BusDone`] (node to hub). All appended, so every v9 frame and
///   command encodes exactly as before; a v9 node ignores the new commands
///   (its decoder reports them as corrupt) and a v9 hub counts the new frames
///   as corrupt and carries on.
/// - 11: aircraft signals from a receive-only LR2021 node (`firmware/node-lr`,
///   docs/AERO-NODE.md). Adds [`Frame::Air`] (one raw 1090ES / UAT / FLARM /
///   OGN frame) and [`Frame::AeroStatus`] (node to hub), [`Command::TimeSync`]
///   (hub to node: UTC for hopping) and [`NodeRole::Aero`], whose
///   [`Assignment::plan`] entries are [`AeroMode`] codes. All appended, so
///   every v10 frame, command and role encodes exactly as before; a v10 hub
///   counts the new frames as corrupt and a v10 node ignores `TimeSync`.
/// - 12: drone control links on the same LR2021 node (`crates/ctrlwatch`,
///   docs/CONTROL-LINKS.md). Adds [`Frame::Control`] (one control-link
///   sighting, with decoded content and the raw packet only when the hub
///   has turned content decoding on) and [`Frame::ControlEnergy`] (the
///   node's energy map of one band: hoppers and steady carriers). Hub to
///   node, two new [`AeroMode`] codes: [`AeroMode::Control`] (scan for
///   control links) and [`AeroMode::ControlFlags`] (the two opt-ins). All
///   appended, so every v11 frame and command encodes exactly as before; a
///   v11 hub counts the new frames as corrupt, and a v11 node skips the new
///   plan codes as unknown (and so never reads IDs or content).
pub const PROTOCOL_VERSION: u8 = 12;

/// Longest CSBee line [`Frame::Csbee`] carries (`adsb::MAX_LINE`; the link
/// crate does not depend on `adsb`, so the number is repeated here and
/// checked in the hub firmware).
pub const MAX_CSBEE_LINE: usize = 200;

/// Largest encoded frame, COBS overhead included. Size buffers with this.
pub const MAX_FRAME: usize = 320;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRole {
    /// 2.4 GHz Wi-Fi listener: Remote ID, DJI legacy DroneID, open FPV links, Wi-Fi drones.
    Wifi24,
    /// 5 GHz Wi-Fi listener, same frames.
    Wifi5,
    /// Bluetooth Remote ID (legacy, extended, Long Range).
    Ble,
    /// Raw-capture scanner for analog FPV video and RF fingerprints (HDZero,
    /// LTE-style links), reporting [`Frame::Rf`].
    Fpv,
    /// Protocol 4. Raw-capture DJI OcuSync DroneID receiver ([`Frame::DroneId`]).
    DroneId,
    /// Protocol 4. An ESP-NOW gateway: relays other nodes, listens to nothing itself.
    Gateway,
    /// Protocol 11. Aircraft signals on an LR2021 radio (receive only):
    /// 1090 MHz extended squitter, 978 MHz UAT, or FLARM / OGN trackers
    /// followed through their frequency hops. The plan picks which
    /// ([`AeroMode`]). Sends [`Frame::Air`] and [`Frame::AeroStatus`].
    Aero,
}

impl NodeRole {
    /// Bit for this role in [`Frame::Status`]'s `can` mask.
    pub const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Protocol 4. What a node should listen to, sent by the hub ([`Command::Assign`]).
/// The node applies it at once if its running image has `role`, otherwise it
/// switches image (a reboot of a few seconds) and applies it after boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub role: NodeRole,
    /// Wi-Fi roles: channel numbers with dwell in ms, visited in order and
    /// repeated. Raw-capture roles: centre frequencies in units of 100 kHz
    /// (5756.5 MHz = 57565) with dwell in ms. Empty = the role's built-in plan.
    pub plan: Vec<(u32, u16), 24>,
    /// Changes whenever the hub changes the plan, echoed in [`Frame::Status`]
    /// so the hub knows the node has applied it.
    pub id: u16,
}

/// Protocol 11. What an [`NodeRole::Aero`] node listens to: one
/// [`Assignment::plan`] entry is `(AeroMode::code(), dwell_ms)`. Several
/// entries are visited in order, each for its dwell, and repeated (e.g.
/// 1090 and 978 MHz time-shared on one radio); a single entry, or a dwell of
/// 0, stays. An empty plan is the node's built-in default (1090 MHz).
///
/// Codes: single-frequency modes are their frequency in 100 kHz units, as
/// in raw-capture plans (10900 = 1090 MHz extended squitter, 9780 = 978 MHz
/// UAT). Hopping modes set bit 31, carry the region in bits 0-7 (SoftRF's
/// `RF_BAND_*` numbers: 1 Europe, 2 USA/Canada, 3 Australia/South America,
/// ...) and bit 8 = follow OGN trackers' channel instead of FLARM's.
///
/// Protocol 12 adds two codes:
/// - [`AeroMode::Control`]: bit 30 set, bits 0-9 a mask of
///   `ctrlwatch::Region::ALL` indices (bit 0 FCC915, 1 AU915, 2 EU868,
///   3 IN866, 4 AU433, 5 EU433, 6 US433, 7 US433W, 8 TH920, 9 ISM 2.4 GHz):
///   scan those regions' packet plans and sweep their bands for drone
///   control links for the entry's dwell; bits 10-19 the same mask for
///   bands swept for energy only (no packet plans).
/// - [`AeroMode::ControlFlags`]: bit 29 set, bit 0 = read link IDs, bit 1 =
///   decode content. Not a mode: the node takes the flags from it and skips
///   it when walking the plan (its dwell is ignored). No such entry = both
///   off, which is also what a node that does not know the code does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeroMode {
    Es1090,
    Uat978,
    /// Follow FLARM's (or, with `ogn`, OGN trackers') hops in `band`.
    /// Needs UTC from [`Command::TimeSync`]; without it the node parks on
    /// one channel and says so in [`Frame::AeroStatus`].
    FlarmHop { band: u8, ogn: bool },
    /// Protocol 12: scan these regions for control links (`regions`: bit
    /// `i` = `ctrlwatch::Region::ALL[i]`; packet plans and energy sweeps)
    /// and sweep the `energy` regions' bands for energy only.
    Control { regions: u16, energy: u16 },
    /// Protocol 12: the control-link opt-ins (both off unless the hub's
    /// owner turned them on).
    ControlFlags { read_ids: bool, decode_content: bool },
}

impl AeroMode {
    pub const ES1090_CODE: u32 = 10_900;
    pub const UAT978_CODE: u32 = 9_780;
    pub const HOP_FLAG: u32 = 1 << 31;
    pub const HOP_OGN: u32 = 1 << 8;
    pub const CONTROL_FLAG: u32 = 1 << 30;
    /// Region bits a [`AeroMode::Control`] code may carry.
    pub const CONTROL_REGIONS: u32 = 0x3FF;
    pub const FLAGS_FLAG: u32 = 1 << 29;
    pub const FLAG_READ_IDS: u32 = 1;
    pub const FLAG_DECODE_CONTENT: u32 = 2;

    pub const fn code(self) -> u32 {
        match self {
            AeroMode::Es1090 => Self::ES1090_CODE,
            AeroMode::Uat978 => Self::UAT978_CODE,
            AeroMode::FlarmHop { band, ogn } => Self::HOP_FLAG | if ogn { Self::HOP_OGN } else { 0 } | band as u32,
            AeroMode::Control { regions, energy } => {
                Self::CONTROL_FLAG | (regions as u32 & Self::CONTROL_REGIONS) | ((energy as u32 & Self::CONTROL_REGIONS) << 10)
            }
            AeroMode::ControlFlags { read_ids, decode_content } => {
                Self::FLAGS_FLAG
                    | if read_ids { Self::FLAG_READ_IDS } else { 0 }
                    | if decode_content { Self::FLAG_DECODE_CONTENT } else { 0 }
            }
        }
    }

    /// `None` for a code this protocol version does not define.
    pub const fn from_code(code: u32) -> Option<AeroMode> {
        match code {
            Self::ES1090_CODE => Some(AeroMode::Es1090),
            Self::UAT978_CODE => Some(AeroMode::Uat978),
            c if c & Self::HOP_FLAG != 0 && c & !(Self::HOP_FLAG | Self::HOP_OGN | 0xFF) == 0 => {
                Some(AeroMode::FlarmHop { band: c as u8, ogn: c & Self::HOP_OGN != 0 })
            }
            c if c & Self::CONTROL_FLAG != 0 && c & !(Self::CONTROL_FLAG | Self::CONTROL_REGIONS | (Self::CONTROL_REGIONS << 10)) == 0 => {
                Some(AeroMode::Control {
                    regions: (c & Self::CONTROL_REGIONS) as u16,
                    energy: ((c >> 10) & Self::CONTROL_REGIONS) as u16,
                })
            }
            c if c & Self::FLAGS_FLAG != 0 && c & !(Self::FLAGS_FLAG | 3) == 0 => Some(AeroMode::ControlFlags {
                read_ids: c & Self::FLAG_READ_IDS != 0,
                decode_content: c & Self::FLAG_DECODE_CONTENT != 0,
            }),
            _ => None,
        }
    }

    /// True for [`AeroMode::ControlFlags`]: settings, not something to
    /// listen to.
    pub const fn is_flags(self) -> bool {
        matches!(self, AeroMode::ControlFlags { .. })
    }

    /// A plan entry: this mode for `dwell_ms` (0 = for good).
    pub const fn entry(self, dwell_ms: u16) -> (u32, u16) {
        (self.code(), dwell_ms)
    }
}

/// Protocol 4. Hub-to-node messages, framed exactly like [`Frame`] (postcard +
/// COBS) on the reverse direction of a wired link or through an ESP-NOW
/// gateway. Keep new variants last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Listen as described. Idempotent: resent every few seconds.
    Assign(Assignment),
    /// Gateway only: the next command is for the node with this station
    /// address (the reverse of [`Frame::Via`]). Protocol 10: on the wired
    /// bus too, where every node hears every command; a node takes the
    /// command after a `To` naming its own address.
    To { node: [u8; 6] },
    /// Protocol 10, wired bus: "node `addr`, go ahead". Only that node
    /// answers, at once, with its queued frames (whole frames, at most
    /// `budget` bytes unless the first frame alone is longer) and then a
    /// [`Frame::BusDone`]. `ack` is the sequence number of the node's last
    /// answer the hub accepted (0: none since the address was given), so a
    /// node resends an answer the hub did not get. `epoch` names this hub
    /// boot: a node holding an address from another epoch must join again.
    Poll { addr: u8, epoch: u16, ack: u8, budget: u16 },
    /// Protocol 10, wired bus: "anyone new?". A node without an address (of
    /// this `epoch`) answers with a [`Frame::BusJoin`] in one of `slots`
    /// slots of `slot_us` microseconds, picked at random, counted from the
    /// end of this command.
    JoinSlot { epoch: u16, slots: u8, slot_us: u16 },
    /// Protocol 10, wired bus: the node with station address `mac` is bus
    /// address `addr` for this `epoch`. `check` is `wirebus::crc` over the
    /// other fields (a garbled grant must never bind the wrong node).
    Address { mac: [u8; 6], addr: u8, epoch: u16, check: u16 },
    /// Protocol 11: "it is now `utc_ms`" (Unix milliseconds), about once a
    /// second, for nodes that need UTC (FLARM / OGN hopping, [`AeroMode`]).
    /// The node takes the command's arrival as that instant. `pps`: the
    /// hub's UTC is anchored to a GPS PPS edge. On the wired bus and through
    /// a gateway it is a broadcast (no [`Command::To`] needed); a node
    /// that does not use it ignores it.
    TimeSync { utc_ms: u64, pps: bool },
}

/// Protocol 4. One DJI OcuSync DroneID burst, decoded on the node. Raw units
/// as sent by the drone (see `crates/droneid` `FlightInfo`); the hub scales.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DroneIdSighting {
    /// Burst centre, 100 kHz units (5756.5 MHz = 57565).
    pub freq: u32,
    pub rssi: i8,
    pub snr_db: i8,
    pub uptime_us: u64,
    pub record: DroneIdRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DroneIdRecord {
    /// OcuSync 2/3 plaintext flight record.
    Flight {
        serial: [u8; 16],
        lat_raw: i32,
        lon_raw: i32,
        altitude_raw: i16,
        height_raw: i16,
        v_north: i16,
        v_east: i16,
        v_up: i16,
        yaw_raw: i16,
        pilot_lat_raw: i32,
        pilot_lon_raw: i32,
        home_lat_raw: i32,
        home_lon_raw: i32,
        product_type: u8,
    },
    /// O4 encrypted record, protocol 4-7 nodes: the session key ID only.
    Encrypted { key_id: [u8; 4] },
    /// A burst matched the DroneID reference pattern but did not decode
    /// (weak, clipped or an unknown layout): presence only.
    Detected,
    /// Protocol 8. An O4 record that passed every structural check of the
    /// spec (CRC-24A, logical-object boundary and CRC-16, marker, exact core
    /// length, zero padding). `key_id` is the cleartext session identifier;
    /// `object` is the whole logical object (`length + 3` bytes, at most
    /// 173), ciphertext included, exactly as received, so the hub can keep
    /// it as evidence and a decryption layer can work from it.
    O4 { kind: O4Kind, key_id: [u8; 4], object: Vec<u8, 173> },
}

/// Protocol 8. Which O4 record a [`DroneIdRecord::O4`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum O4Kind {
    /// Selector 0x13, `CRYP`: the session-key record. In the one model
    /// studied (Mini 5 Pro) it starts after a GNSS fix.
    Cryp,
    /// Selector 0x10, `INFP`, 128-byte core: flight telemetry. Starts after
    /// takeoff on that model.
    Infp10,
    /// Selector 0x11, `INFP`, 160-byte core.
    Infp11,
}

impl O4Kind {
    pub fn name(self) -> &'static str {
        match self {
            O4Kind::Cryp => "CRYP",
            O4Kind::Infp10 => "INFP-10",
            O4Kind::Infp11 => "INFP-11",
        }
    }
}

/// Protocol 4. What a raw-capture scanner recognised on one frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RfKind {
    /// Analog FM video: the 15.6/15.7 kHz line rate was found.
    AnalogVideo,
    /// Strong carrier on an analog FPV channel, line rate not (yet) confirmed.
    FpvCarrier,
    /// HDZero: continuous, 17 or 27 MHz wide, on its 37 MHz grid.
    Hdzero,
    /// LTE-style OFDM (15 kHz spacing): DJI, Walksnail, Autel, Herelink.
    LteStyle,
    /// Wi-Fi OFDM at half or quarter clock (Skydio narrow modes).
    WifiNarrow,
    /// Unclassified energy above the floor.
    Energy,
    /// Protocol 7. DJI-style video downlink (OcuSync drone, O3 air unit,
    /// DJI FPV), generation not determined: 15 kHz OFDM with DJI's 4.7 µs
    /// prefix, 9 MHz or wider, on DJI's x.5 MHz raster when the centre is
    /// measurable. Autel uses the same numerology (RFUAV EVO Nano; its
    /// off-raster bursts are not reported as this kind).
    DjiVideo,
    /// Protocol 7. DJI O4 video: 30 kHz OFDM of video width on any band
    /// (measured on the RFUAV Mini 4 Pro and Avata 2 recordings), or the
    /// 15 kHz shape on 5.170-5.250 GHz, a band only O4 products list for
    /// video (Autel EVO Lite/Max also file it).
    DjiO4Video,
    /// Protocol 7. The same on a whole-MHz DJI FPV (V1/Vista) channel;
    /// Walksnail reportedly shares the channel plan.
    DjiFpvAirUnit,
    /// Protocol 7. DJI controller uplink: narrow (1.1/2.2 MHz) 15 kHz OFDM
    /// bursts (OcuSync, O3, DJI FPV) or ~4.4 MHz 30 kHz bursts (O4),
    /// confirmed hopping between centres. `freq` is the latest burst's
    /// centre.
    DjiController,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RfSighting {
    pub kind: RfKind,
    /// Centre, 100 kHz units.
    pub freq: u32,
    /// Occupied width, kHz (0 if unknown).
    pub width_khz: u16,
    /// Level above the noise floor, dB.
    pub snr_db: i8,
    /// Fraction of the dwell the signal was on, percent.
    pub duty_pct: u8,
    /// Measured line rate for analog video, Hz (0 if none).
    pub line_hz: u16,
    pub uptime_us: u64,
}

/// Protocol 4. A Wi-Fi network that looks like a drone (maker address prefix
/// or network name), from a Wi-Fi node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WifiDroneSighting {
    /// Index into the hub's maker table (`crates/wifidrone`).
    pub maker: u8,
    pub mac: [u8; 6],
    pub ssid: heapless::String<32>,
    pub channel: u8,
    pub rssi: i8,
    pub uptime_us: u64,
}

/// Protocol 9. What kind of hostile or disruptive radio activity a node saw.
/// Not a drone: something attacking the radios around it (or ours), or
/// drowning out the bands drones use. Keep new kinds last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreatKind {
    /// 802.11 deauthentication frames (subtype 12) well above the
    /// background rate. `detail` = the most common reason code.
    Deauth,
    /// 802.11 disassociation frames (subtype 10), likewise.
    Disassoc,
    /// Many networks appearing at once, usually from random addresses
    /// (beacon spam). `detail` = distinct names in the window.
    BeaconFlood,
    /// A known network name from a second transmitter that does not match
    /// the first (different security, channel or vendor). `ssid` = the
    /// name, `source` = the newcomer, `target` = the original.
    EvilTwin,
    /// One transmitter beaconing or answering probes for many network names
    /// (Karma / Wi-Fi Pineapple style). `detail` = distinct names.
    Karma,
    /// Channel Switch Announcements sent to move clients off a channel.
    /// `detail` = the channel they are told to move to.
    ChannelSwitch,
    /// Authentication / association request flood at one access point.
    AuthFlood,
    /// Probe request flood. `detail` = distinct source addresses.
    ProbeFlood,
    /// A known attack tool's own signature (Pwnagotchi beacon, ESP32 attack
    /// firmware...). `detail` = index into the tool table on the hub.
    AttackTool,
    /// Bluetooth advertising spam (pairing pop-up floods, random addresses).
    /// `detail` = distinct addresses in the window.
    BleSpam,
    /// Noise floor raised across a wide span (barrage jamming).
    /// `level` = rise above the learned floor, dB.
    /// Raw-capture nodes (`crates/jamwatch`): `level` is the median rise of
    /// the raised hops, `freq`/`width_khz` the span of raised hops plus one
    /// capture's passband, `count` the raised hops, `detail` = percent of
    /// the band's recent hops raised.
    JamBroadband,
    /// A strong unmodulated or narrow tone sitting on a band.
    /// Raw-capture nodes: `freq` = the tone (latest seen), `detail` = how
    /// many distinct tone frequencies (1 MHz apart) were seen in the window.
    JamTone,
    /// A carrier sweeping across the band (chirp jammer). `detail` = sweep
    /// rate, MHz per second.
    /// Raw-capture nodes: the median rate over the dwells that saw it;
    /// `level` = how far its loudest probes stand over the floor.
    JamSweep,
    /// Strong pulsed or reactive interference (on only part of the time).
    /// Raw-capture nodes: `detail` = median percent of probes loud (the
    /// duty cycle), `level` = the pulses' rise over the floor, dB.
    JamPulsed,
    /// The node can hear almost nothing it normally hears while the band is
    /// loud: it is effectively blind. `detail` = percent of the usual
    /// frame rate still arriving.
    /// Raw-capture nodes (`node-sdr`, `crates/jamwatch`) measure the floor,
    /// not a frame rate: `detail` = estimated percent of free-space
    /// detection range left, `100 * 10^(-level / 20)` where `level` = the
    /// median floor rise (dB) over the band's recent hops (hops holding a
    /// recognised link excluded; a clipping front end counts as 20 dB).
    /// Sent when that median reaches 12 dB (25 % of range left or less),
    /// whatever the cause. `freq`/`width_khz` = the band's hops plus one
    /// capture's passband.
    Blinded,
    /// Frames claiming to come from one of our own nodes failed
    /// authentication (ESP-NOW gateway): someone is injecting fake nodes or
    /// commands. `source` = the claimed node, `count` = rejected frames.
    ///
    /// Sent by a gateway built with the kit link key (`crates/airlink`
    /// `auth`, docs/ESPNOW.md "Securing the link"), at most once a second per
    /// claimed address and reason while it lasts, with no `Via` before it
    /// (it is the gateway's own frame). `source` = the claimed node address
    /// (all zero: more addresses than the gateway's table, e.g. random ones),
    /// `target` = the gateway's own address, `channel` 6 / `freq` 24370,
    /// `level` = strongest RSSI of the rejected packets, `window_ms` = the
    /// window `count` covers. `detail` = why (`airlink::auth::Reject`):
    /// - 1 **bad tag**: the authentication tag does not match: a forgery (or
    ///   a packet made with another key);
    /// - 2 **replay**: a genuine packet, but older than one already taken
    ///   from that node: a recording played back;
    /// - 3 **unauthenticated**: no tag at all: a node built without the key
    ///   (a keyed gateway accepts keyed nodes only), or a forgery that does
    ///   not try;
    /// - 4 **stale**: a genuine packet from before the gateway's last
    ///   restart, more than a minute after it (expected, and not reported,
    ///   for the few seconds a node takes to resync after a restart).
    ForgedLink,
}

impl ThreatKind {
    pub fn name(self) -> &'static str {
        use ThreatKind::*;
        match self {
            Deauth => "deauth",
            Disassoc => "disassoc",
            BeaconFlood => "beacon_flood",
            EvilTwin => "evil_twin",
            Karma => "karma",
            ChannelSwitch => "channel_switch",
            AuthFlood => "auth_flood",
            ProbeFlood => "probe_flood",
            AttackTool => "attack_tool",
            BleSpam => "ble_spam",
            JamBroadband => "jam_broadband",
            JamTone => "jam_tone",
            JamSweep => "jam_sweep",
            JamPulsed => "jam_pulsed",
            Blinded => "blinded",
            ForgedLink => "forged_link",
        }
    }

    /// Jamming rather than a protocol attack.
    pub fn is_jamming(self) -> bool {
        use ThreatKind::*;
        matches!(self, JamBroadband | JamTone | JamSweep | JamPulsed | Blinded)
    }
}

/// Protocol 9. One threat, summarised over a window (sent at most about
/// once a second per kind and target while it lasts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreatSighting {
    pub kind: ThreatKind,
    /// Wi-Fi channel number (Wi-Fi kinds) or BLE advertising channel, 0 if
    /// unknown.
    pub channel: u8,
    /// Centre frequency, 100 kHz units; 0 if unknown. Raw-capture (jamming)
    /// kinds always set it.
    pub freq: u32,
    /// Affected width, kHz; 0 if unknown.
    pub width_khz: u32,
    /// Frame kinds: strongest RSSI in the window, dBm. Jamming kinds: rise
    /// above the node's learned floor, dB.
    pub level: i8,
    /// Frames, bursts or dwells counted in the window.
    pub count: u32,
    /// Length of the window, ms.
    pub window_ms: u32,
    /// Transmitter address (display order); all zero when unknown or when
    /// the frames came from many random addresses.
    pub source: [u8; 6],
    /// Who is being hit: a BSSID or station, all 0xFF for broadcast, all zero
    /// if not applicable.
    pub target: [u8; 6],
    /// Kind-specific number (see [`ThreatKind`]).
    pub detail: u32,
    /// Network name involved (evil twin, Karma, tool), may be empty.
    pub ssid: heapless::String<32>,
    pub uptime_us: u64,
}

/// Protocol 11. Which aircraft signal a [`Frame::Air`] carries, and so what
/// its `raw` bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AirProto {
    /// 1090 MHz Mode S extended squitter: the 14-byte DF17 frame, parity
    /// included, CRC-checked on the node (syndrome 0).
    Es1090,
    /// 978 MHz UAT: the 48 bytes after the sync word as received (a long
    /// ADS-B frame with its Reed-Solomon parity, or a short frame and what
    /// followed it), not corrected; the hub runs the Reed-Solomon decoder.
    Uat978,
    /// FLARM (SoftRF's "Legacy"): the 26 Manchester-decoded bytes (24
    /// payload, still scrambled, then the CRC-16), CRC-checked on the node.
    Flarm,
    /// OGN tracker (OGNTP): the 26 Manchester-decoded bytes (20 payload, 6
    /// LDPC parity), not checked on the node.
    Ogn,
}

/// Longest [`AirFrame::raw`].
pub const MAX_AIR_RAW: usize = 64;

/// Protocol 11. One aircraft frame heard by an aero node, raw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AirFrame {
    pub proto: AirProto,
    /// Frequency the radio was tuned to, kHz.
    pub freq_khz: u32,
    /// Packet RSSI, dBm.
    pub rssi: i8,
    /// The node's estimate of UTC at reception (Unix ms) from
    /// [`Command::TimeSync`]; `None` if it has no time.
    pub utc_ms: Option<u64>,
    /// Node uptime at reception.
    pub uptime_us: u64,
    pub raw: Vec<u8, MAX_AIR_RAW>,
}

/// Protocol 11. How an aero node knows the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AeroTime {
    /// No [`Command::TimeSync`] for 10 s (or ever).
    Unknown,
    /// Synced to the hub, whose clock is not PPS-anchored.
    Hub,
    /// Synced to the hub, whose clock is PPS-anchored.
    HubPps,
}

/// Protocol 11. An aero node's state, sent with every Hello and whenever it
/// changes (mode, time state, parking).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AeroStatus {
    /// The [`AeroMode::code`] running now (0: none, the radio is down).
    pub mode: u32,
    /// Where the radio is (or was last) tuned, kHz.
    pub freq_khz: u32,
    pub time: AeroTime,
    /// A hopping mode that is NOT hopping: no time, so parked on
    /// `freq_khz`, hearing only the slots that happen to use it.
    pub parked: bool,
    /// The LR2021 answered at boot (version read back). False: no radio, or
    /// it stopped answering; nothing is being received.
    pub radio_ok: bool,
    /// Times the 1090 MHz receive watchdog restarted reception.
    pub rx_restarts: u32,
    /// Packets dropped by the node's checks (CRC, Manchester, length), total.
    pub rejected: u32,
    /// [`Frame::Air`] not sent because of the rate limit, total.
    pub rate_dropped: u32,
}

/// Protocol 12. A region's bit in an [`AeroMode::Control`] mask: its index
/// in `ctrlwatch::Region::ALL`.
pub const fn control_region_bit(r: ctrlwatch::Region) -> u16 {
    use ctrlwatch::Region as R;
    1 << match r {
        R::Fcc915 => 0,
        R::Au915 => 1,
        R::Eu868 => 2,
        R::In866 => 3,
        R::Au433 => 4,
        R::Eu433 => 5,
        R::Us433 => 6,
        R::Us433W => 7,
        R::Th920 => 8,
        R::Ism2g4 => 9,
    }
}

/// Protocol 12. Content items one [`ControlReport`] carries; a packet with
/// more is sent as several reports (`part` 1, 2, ...), the raw bytes in the
/// first.
pub const MAX_CONTROL_CONTENT: usize = 2;

/// Protocol 12. One drone control link (ExpressLRS, mLRS, SiK, OpenLRSng)
/// heard by an LR2021 node, built from `ctrlwatch` (docs/CONTROL-LINKS.md).
///
/// Two kinds, told apart by `part`:
/// - `part == 0`: the link's running summary, at most every 2 s per link
///   while it is heard (`ctrlwatch::Sightings`): the latest packet's
///   sighting, best confidence, counts, strongest signal. No content, no
///   raw bytes.
/// - `part >= 1`: one packet's decoded content, only when the hub has
///   turned content decoding on ([`AeroMode::ControlFlags`]). `raw` (the
///   packet as received, for the evidence log) rides in part 1.
///
/// `sighting.id` is `None` unless the hub turned ID reading on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlReport {
    pub sighting: ctrlwatch::ControlSighting,
    pub best: ctrlwatch::Confidence,
    /// Packets counted on this link since the node started tracking it
    /// (running totals, like [`OpenLinkSighting::packets`]).
    pub packets: u32,
    pub confirmed: u32,
    pub peak_rssi: i16,
    pub part: u8,
    pub content: Vec<ctrlwatch::ControlContent, MAX_CONTROL_CONTENT>,
    pub raw: Option<Vec<u8, { ctrlwatch::MAX_RAW }>>,
}

/// Protocol 12. A hopping transmitter in an energy map, with the systems
/// it could be (`ctrlwatch::energy::classify`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HopperReport {
    pub low_khz: u32,
    pub high_khz: u32,
    pub channels: u16,
    pub spacing_khz: Option<u32>,
    pub occupied_khz: u32,
    pub rssi: i8,
    pub spread_db: i8,
    /// Burst timing from a dwell on one of its channels: median burst
    /// length and packet interval, µs, and how periodic (%).
    pub burst_us: Option<u32>,
    pub interval_us: Option<u32>,
    pub periodic_pct: u8,
    /// Candidate systems with their scores (0-100), best first.
    pub candidates: Vec<(ctrlwatch::System, u8), 4>,
}

/// Protocol 12. What one energy window found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EnergyActivity {
    Quiet,
    /// Only steady carriers.
    Steady,
    /// Hopping energy that does not look like one transmitter (meters,
    /// LoRaWAN).
    Background,
    Hopper(HopperReport),
}

/// Protocol 12. One energy window of one band: sent when a hopper or a
/// steady carrier is seen, and otherwise about every 30 s per band.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlEnergy {
    pub region: ctrlwatch::Region,
    /// Windows the node's map has seen (its background needs ~20).
    pub windows: u32,
    pub activity: EnergyActivity,
    /// Steady carriers: (kHz, dBm).
    pub steady: Vec<(u32, i8), 4>,
    pub uptime_us: u64,
}

/// Protocol 5. A GPS receiver's state, as a hub reports it to a web head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GnssState {
    /// No receiver answering.
    Absent,
    /// Receiver present, no fix.
    Searching,
    Fix2d,
    Fix3d,
}

/// Protocol 5. A usable GPS position (never a typed-in one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubPosition {
    /// Degrees x 1e7 (1.1 cm steps, finer than any GPS fix).
    pub lat_e7: i32,
    pub lon_e7: i32,
    /// Height above the WGS-84 ellipsoid, millimetres; `None` for a 2D fix.
    pub alt_hae_mm: Option<i32>,
    /// How old the fix was when this frame was built, milliseconds.
    pub age_ms: u32,
}

/// Protocol 5. The sending hub's own GPS: what a web head needs to compute
/// distances and to stamp CoT events, without a receiver of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubFix {
    pub state: GnssState,
    /// Satellites used (or tracked while searching), if known.
    pub sats: Option<u8>,
    /// The hub's latest usable fix, if it is still fresh.
    pub position: Option<HubPosition>,
    /// UTC (Unix milliseconds) at the moment the frame was built, if known.
    /// The frame reaches the web head a few milliseconds later (queue + wire).
    pub utc_ms: Option<u64>,
    /// The hub's UTC is anchored to a GPS PPS edge (sub-millisecond there).
    pub utc_pps: bool,
}

/// Protocol 6. One flag on [`FlagReport`]: the rule and level, and the one
/// number that says most (see `faarules::text::item_text`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FlagItem {
    /// `faarules::Rule` as u8.
    pub rule: u8,
    /// `airspace::AreaKind` as u8, 0 for none.
    pub area: u8,
    /// 0 Note, 1 Watch, 2 Warning, 3 Alert.
    pub level: u8,
    /// Height above ground (m), distance (m), terrain over the sight line
    /// (m), sun elevation (deg)... per rule; 0 when none.
    pub value: f32,
    /// How long it has held, seconds.
    pub secs: u16,
}

/// Most items one [`FlagReport`] carries (the worst ones).
pub const MAX_FLAG_ITEMS: usize = 6;

/// Protocol 6, hub to web head, about once a second per flagged track: the
/// hub's possible-violation flags for one drone or aircraft. Tracks are
/// numbered independently on the two boards, so `who` names it the way both
/// can: a drone's primary UAS ID as text (or its transmitter address as 12
/// hex digits when it has no ID yet), or `ICAO-xxxxxx` for an aircraft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlagReport {
    pub who: heapless::String<24>,
    /// The worst level.
    pub level: u8,
    pub items: Vec<FlagItem, MAX_FLAG_ITEMS>,
    /// Airspace pack build date (Unix s); 0 = the hub has no pack.
    pub pack_date: u32,
}

/// One Remote ID reception: everything one frame or advertisement carried.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub transport: Transport,
    /// Wi-Fi channel number, or BLE advertising channel (37-39) when known, else 0.
    pub channel: u8,
    pub rssi: i8,
    /// Wi-Fi transmitter address or BLE advertiser address, in display order
    /// (most significant byte first, as printed `aa:bb:..`).
    pub mac: [u8; 6],
    /// F3411 message counter from the frame.
    pub counter: u8,
    /// Node uptime when the frame arrived.
    pub uptime_us: u64,
    pub messages: Vec<RawMessage, MAX_PACK>,
}

/// An open FPV / telemetry link heard by a Wi-Fi node: the latest frame of
/// one stream of one link (system, ID, port, direction; see
/// `openlink::Sightings`), sent at most about once a second per stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenLinkSighting {
    /// System, cleartext identifier (link / vehicle / comm ID, if any),
    /// port, direction and transmitter address (display order).
    pub link: openlink::OpenLink,
    /// Wi-Fi channel number it was heard on, 0 if unknown.
    pub channel: u8,
    /// RSSI of the most recent frame, dBm.
    pub rssi: i8,
    /// Node uptime when the most recent frame arrived.
    pub uptime_us: u64,
    /// Frames the node has counted on this stream since it started tracking
    /// it. A running total: it restarts if the node reboots or had to drop
    /// the stream from its table, so take differences with care.
    pub packets: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// Sent at boot and every few seconds, so a hub that restarts learns who is attached.
    Hello {
        role: NodeRole,
        protocol: u8,
        firmware: heapless::String<16>,
    },
    Detection(Detection),
    /// Once a second: frames seen vs. Remote ID found, for the health screen.
    /// Every count is a running total since boot; the hub takes differences.
    Stats {
        uptime_us: u64,
        frames_seen: u32,
        rid_found: u32,
        dropped: u32,
        /// Frames seen per transport, for nodes that can tell them apart (the
        /// BLE node splits legacy / extended / Long Range). Empty otherwise.
        by_transport: Vec<(Transport, u32), 5>,
    },
    /// Protocol 2.
    OpenLink(OpenLinkSighting),
    /// Protocol 3, from an ESP-NOW gateway only: the next frame on this link
    /// was relayed from the node with this station address, heard at this
    /// RSSI. One gateway link carries many nodes, so it precedes every relayed
    /// frame; a hub attributes exactly one following frame to `node`. A marker
    /// rather than a wrapper, so relayed frames keep their `MAX_FRAME` budget.
    /// Keep new variants last: postcard tags variants by index.
    Via { node: [u8; 6], rssi: i8 },
    /// Protocol 4: what the node can do and what it is doing, sent with every
    /// Hello and whenever it changes. `can` is a mask of [`NodeRole::bit`].
    /// `plan` is the [`Assignment::id`] it applied (0 = its built-in plan).
    Status { role: NodeRole, can: u8, plan: u16 },
    /// Protocol 4.
    DroneId(DroneIdSighting),
    /// Protocol 4.
    Rf(RfSighting),
    /// Protocol 4: a DJI legacy Wi-Fi DroneID vendor element (OUI 26:37:12),
    /// raw; the hub decodes it with `rid::dji`.
    DjiWifi {
        channel: u8,
        rssi: i8,
        mac: [u8; 6],
        uptime_us: u64,
        element: Vec<u8, 255>,
    },
    /// Protocol 4.
    WifiDrone(WifiDroneSighting),
    /// Protocol 5, hub to web head only, about once a second: the hub's own
    /// GPS position, receiver state and UTC.
    HubFix(HubFix),
    /// Protocol 5, hub to web head only: one CSBee line from the hub's
    /// ADSBee 1090 (`crates/adsb`), CRC and line end included, exactly as
    /// the ADSBee prints it. The web head parses it with `adsb::parse`.
    Csbee(Vec<u8, MAX_CSBEE_LINE>),
    /// Protocol 6, hub to web head only: possible FAA rule violations on one
    /// track (PLAN.md Phase 12).
    Flags(FlagReport),
    /// Protocol 9: an attack or jamming seen by a node.
    Threat(ThreatSighting),
    /// Protocol 10, wired bus: a node without an address answering a
    /// [`Command::JoinSlot`]. `want` is the address it held last (0: none),
    /// so the hub can give it back; `check` is `wirebus::crc` over `mac` and
    /// `want` (two joins in the same slot collide on the wire, and a garbled
    /// join must not bind a made-up address).
    BusJoin { mac: [u8; 6], want: u8, check: u16 },
    /// Protocol 10, wired bus: the end of a node's answer to a
    /// [`Command::Poll`]. `seq` numbers the answer (1-255, 0 = the answer
    /// carried no frames); `more` = frames are still queued; `crc` is
    /// `wirebus::crc` over every byte of the answer before this frame, then
    /// `addr`, `seq` and `more`. The hub keeps an answer's frames only when
    /// the CRC matches.
    BusDone { addr: u8, seq: u8, more: bool, crc: u16 },
    /// Protocol 11: one aircraft frame from an aero node.
    Air(AirFrame),
    /// Protocol 11: an aero node's state.
    AeroStatus(AeroStatus),
    /// Protocol 12: a drone control link heard by an LR2021 node.
    Control(ControlReport),
    /// Protocol 12: an LR2021 node's energy map of one band.
    ControlEnergy(ControlEnergy),
}

/// Serialize `frame` with postcard and COBS-encode it into `buf`, including
/// the trailing zero delimiter. Returns the used part of `buf`; send it as is.
pub fn encode<'a>(frame: &Frame, buf: &'a mut [u8]) -> Result<&'a mut [u8], postcard::Error> {
    postcard::to_slice_cobs(frame, buf)
}

/// Serialize a hub-to-node [`Command`] the same way as [`encode`].
pub fn encode_command<'a>(cmd: &Command, buf: &'a mut [u8]) -> Result<&'a mut [u8], postcard::Error> {
    postcard::to_slice_cobs(cmd, buf)
}

/// Why a chunk of the byte stream did not become a [`Frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// More than [`MAX_FRAME`] bytes arrived without a delimiter; they were dropped.
    Overflow,
    /// A delimited chunk was not a valid COBS + postcard [`Frame`] (corruption,
    /// a reader that joined mid-frame, or a peer on another protocol version).
    Corrupt,
}

/// Streaming decoder for the byte stream from one UART: feed it whatever
/// arrived, it hands back each complete frame. Fixed `MAX_FRAME` buffer, no
/// allocation. Zero bytes between frames (resync padding) are ignored.
pub type Decoder = StreamDecoder<Frame>;

/// Decoder for the hub-to-node [`Command`] stream.
pub type CommandDecoder = StreamDecoder<Command>;

pub struct StreamDecoder<T> {
    acc: postcard::accumulator::CobsAccumulator<MAX_FRAME>,
    /// True while the accumulator holds part of a frame.
    pending: bool,
    _t: core::marker::PhantomData<T>,
}

impl<T: for<'de> Deserialize<'de>> Default for StreamDecoder<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: for<'de> Deserialize<'de>> StreamDecoder<T> {
    pub const fn new() -> Self {
        Self {
            acc: postcard::accumulator::CobsAccumulator::new(),
            pending: false,
            _t: core::marker::PhantomData,
        }
    }

    /// Consume `input`, calling `on_frame` once per complete frame or error,
    /// in stream order. Partial frames are kept for the next call.
    pub fn feed(&mut self, mut input: &[u8], mut on_frame: impl FnMut(Result<T, LinkError>)) {
        use postcard::accumulator::FeedResult;
        loop {
            if !self.pending {
                let start = input.iter().position(|&b| b != 0).unwrap_or(input.len());
                input = &input[start..];
            }
            if input.is_empty() {
                return;
            }
            let (result, rest) = match self.acc.feed::<T>(input) {
                FeedResult::Consumed => {
                    self.pending = true;
                    return;
                }
                FeedResult::OverFull(rest) => (Err(LinkError::Overflow), rest),
                FeedResult::DeserError(rest) => (Err(LinkError::Corrupt), rest),
                FeedResult::Success { data, remaining } => (Ok(data), remaining),
            };
            // An overflow without a delimiter leaves us mid-frame.
            self.pending = matches!(result, Err(LinkError::Overflow)) && !rest.is_empty();
            on_frame(result);
            input = rest;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(fill: u8, n: usize) -> Detection {
        let mut messages = Vec::new();
        for _ in 0..n {
            messages.push([fill; rid::MESSAGE_SIZE]).unwrap();
        }
        Detection {
            transport: Transport::BleLongRange,
            channel: 255,
            rssi: i8::MIN,
            mac: [fill; 6],
            counter: 255,
            uptime_us: u64::MAX,
            messages,
        }
    }

    fn sighting(fill: u8) -> OpenLinkSighting {
        OpenLinkSighting {
            link: openlink::OpenLink {
                system: openlink::System::DroneBridge,
                id: if fill == 0 { None } else { Some(u32::MAX) },
                port: fill,
                direction: openlink::Direction::Unknown,
                tx: [fill; 6],
            },
            channel: fill,
            rssi: i8::MIN,
            uptime_us: if fill == 0 { 0 } else { u64::MAX },
            packets: if fill == 0 { 0 } else { u32::MAX },
        }
    }

    fn samples() -> [Frame; 21] {
        [
            Frame::Hello {
                role: NodeRole::Wifi5,
                protocol: PROTOCOL_VERSION,
                firmware: heapless::String::try_from("0.1.0-abcdef1234").unwrap(),
            },
            Frame::Detection(detection(0xFF, MAX_PACK)),
            Frame::Detection(detection(0x00, MAX_PACK)),
            Frame::Detection(detection(0x5A, 1)),
            Frame::Stats {
                uptime_us: u64::MAX,
                frames_seen: u32::MAX,
                rid_found: 7,
                dropped: 0,
                by_transport: [
                    (Transport::BleLegacy, u32::MAX),
                    (Transport::BleExtended, u32::MAX),
                    (Transport::BleLongRange, u32::MAX),
                    (Transport::WifiBeacon, u32::MAX),
                    (Transport::WifiNan, u32::MAX),
                ]
                .into_iter()
                .collect(),
            },
            Frame::OpenLink(sighting(0xFF)),
            Frame::OpenLink(sighting(0x00)),
            Frame::Via { node: [0xFF; 6], rssi: i8::MIN },
            Frame::Status { role: NodeRole::Gateway, can: u8::MAX, plan: u16::MAX },
            Frame::DroneId(DroneIdSighting {
                freq: u32::MAX,
                rssi: i8::MIN,
                snr_db: i8::MIN,
                uptime_us: u64::MAX,
                record: DroneIdRecord::Flight {
                    serial: [0xFF; 16],
                    lat_raw: i32::MIN,
                    lon_raw: i32::MIN,
                    altitude_raw: i16::MIN,
                    height_raw: i16::MIN,
                    v_north: i16::MIN,
                    v_east: i16::MIN,
                    v_up: i16::MIN,
                    yaw_raw: i16::MIN,
                    pilot_lat_raw: i32::MIN,
                    pilot_lon_raw: i32::MIN,
                    home_lat_raw: i32::MIN,
                    home_lon_raw: i32::MIN,
                    product_type: u8::MAX,
                },
            }),
            Frame::DroneId(DroneIdSighting {
                freq: 57565,
                rssi: -70,
                snr_db: 12,
                uptime_us: 1,
                record: DroneIdRecord::Encrypted { key_id: [1, 2, 3, 4] },
            }),
            Frame::Rf(RfSighting {
                kind: RfKind::AnalogVideo,
                freq: u32::MAX,
                width_khz: u16::MAX,
                snr_db: i8::MIN,
                duty_pct: u8::MAX,
                line_hz: u16::MAX,
                uptime_us: u64::MAX,
            }),
            Frame::DjiWifi {
                channel: u8::MAX,
                rssi: i8::MIN,
                mac: [0xFF; 6],
                uptime_us: u64::MAX,
                element: [0xFF; 255].into_iter().collect(),
            },
            Frame::WifiDrone(WifiDroneSighting {
                maker: u8::MAX,
                mac: [0xFF; 6],
                ssid: heapless::String::try_from("TELLO-ABCDEFGHIJKLMNOPQRSTUVWXYZ").unwrap(),
                channel: u8::MAX,
                rssi: i8::MIN,
                uptime_us: u64::MAX,
            }),
            Frame::Status { role: NodeRole::Wifi24, can: 0, plan: 0 },
            Frame::HubFix(HubFix {
                state: GnssState::Fix3d,
                sats: Some(u8::MAX),
                position: Some(HubPosition {
                    lat_e7: i32::MIN,
                    lon_e7: i32::MIN,
                    alt_hae_mm: Some(i32::MIN),
                    age_ms: u32::MAX,
                }),
                utc_ms: Some(u64::MAX),
                utc_pps: true,
            }),
            Frame::HubFix(HubFix {
                state: GnssState::Absent,
                sats: None,
                position: None,
                utc_ms: None,
                utc_pps: false,
            }),
            Frame::Csbee([0xFF; MAX_CSBEE_LINE].into_iter().collect()),
            Frame::Csbee(b"#S:2,0,0,0,0,0,0,0,0,0,0,ABCD\r\n".iter().copied().collect()),
            Frame::Flags(FlagReport {
                who: heapless::String::try_from("ABCDEFGHIJKLMNOPQRSTUVWX").unwrap(),
                level: u8::MAX,
                items: core::iter::repeat_n(FlagItem { rule: u8::MAX, area: u8::MAX, level: u8::MAX, value: f32::MAX, secs: u16::MAX }, MAX_FLAG_ITEMS).collect(),
                pack_date: u32::MAX,
            }),
            // Protocol 8: a full-size O4 object at the widest varints.
            Frame::DroneId(DroneIdSighting {
                freq: u32::MAX,
                rssi: i8::MIN,
                snr_db: i8::MIN,
                uptime_us: u64::MAX,
                record: DroneIdRecord::O4 { kind: O4Kind::Infp11, key_id: [0xFF; 4], object: [0xFF; 173].into_iter().collect() },
            }),
        ]
    }

    fn commands() -> [Command; 6] {
        let mut plan = Vec::new();
        while plan.push((u32::MAX, u16::MAX)).is_ok() {}
        [
            Command::Assign(Assignment { role: NodeRole::DroneId, plan, id: u16::MAX }),
            Command::Assign(Assignment { role: NodeRole::Wifi24, plan: Vec::new(), id: 1 }),
            Command::To { node: [0xFF; 6] },
            Command::Poll { addr: u8::MAX, epoch: u16::MAX, ack: u8::MAX, budget: u16::MAX },
            Command::JoinSlot { epoch: u16::MAX, slots: u8::MAX, slot_us: u16::MAX },
            Command::Address { mac: [0xFF; 6], addr: u8::MAX, epoch: u16::MAX, check: u16::MAX },
        ]
    }

    /// Protocol 10 appended the bus commands (tags 2-4) and frames (tags 14
    /// and 15); the older tags did not move, and the bus messages are small.
    #[test]
    fn protocol_10_bus_append_only() {
        let mut buf = [0u8; MAX_FRAME];
        let tags: std::vec::Vec<u8> = commands().iter().map(|c| postcard::to_slice(c, &mut buf).unwrap()[0]).collect();
        assert_eq!(tags, [0, 0, 1, 2, 3, 4]);
        // The v9 command encodings are byte for byte what they were.
        assert_eq!(postcard::to_slice(&Command::To { node: [1, 2, 3, 4, 5, 6] }, &mut buf).unwrap(), [1, 1, 2, 3, 4, 5, 6]);
        let join = Frame::BusJoin { mac: [0xFF; 6], want: u8::MAX, check: u16::MAX };
        let done = Frame::BusDone { addr: u8::MAX, seq: u8::MAX, more: true, crc: u16::MAX };
        assert_eq!(postcard::to_slice(&join, &mut buf).unwrap()[0], 14);
        assert_eq!(postcard::to_slice(&done, &mut buf).unwrap()[0], 15);
        let mut dec = Decoder::new();
        for f in [join, done] {
            let wire = encode(&f, &mut buf).unwrap();
            assert!(wire.len() <= 16, "{} bytes", wire.len());
            let mut got = None;
            dec.feed(wire, |r| got = Some(r));
            assert_eq!(got, Some(Ok(f)));
        }
        let mut cmd = CommandDecoder::new();
        for c in &commands()[3..] {
            let wire = encode_command(c, &mut buf).unwrap();
            assert!(wire.len() <= 16, "{} bytes", wire.len());
            let mut got = None;
            cmd.feed(wire, |r| got = Some(r));
            assert_eq!(got.as_ref(), Some(&Ok(c.clone())));
        }
        // A v9 decoder (no bus variants) sees these as corrupt and resyncs:
        // modelled by feeding a tag past the last v9 command.
        assert!(postcard::from_bytes::<Command>(&[5]).is_err());
    }

    fn air(proto: AirProto, n: usize, fill: u8) -> AirFrame {
        AirFrame {
            proto,
            freq_khz: u32::MAX,
            rssi: i8::MIN,
            utc_ms: Some(u64::MAX),
            uptime_us: u64::MAX,
            raw: core::iter::repeat_n(fill, n).collect(),
        }
    }

    /// Protocol 11 appended `Frame::Air` (16), `Frame::AeroStatus` (17),
    /// `Command::TimeSync` (5) and `NodeRole::Aero` (6). Every v10 frame and
    /// command encodes byte for byte as before: the FNV-1a hash of all the
    /// v10 samples and commands below was taken from the v10 code.
    #[test]
    fn protocol_11_aero_append_only() {
        let mut buf = [0u8; 1024];
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut fnv = |bytes: &[u8]| {
            for &b in bytes {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        for mut f in samples() {
            // The sample Hello carries the current version; hash it as v10 sent it.
            if let Frame::Hello { protocol, .. } = &mut f {
                *protocol = 10;
            }
            fnv(encode(&f, &mut buf).unwrap());
        }
        for c in commands() {
            fnv(encode_command(&c, &mut buf).unwrap());
        }
        assert_eq!(h, 0x4dee_7867_75cb_0a74, "a v10 frame or command changed encoding");
        // Spot checks, worked by hand.
        let st = Frame::Status { role: NodeRole::Gateway, can: 1, plan: 2 };
        assert_eq!(postcard::to_slice(&st, &mut buf).unwrap(), [5, 5, 1, 2]);
        let a = Command::Assign(Assignment { role: NodeRole::Ble, plan: [(9780, 500)].into_iter().collect(), id: 3 });
        assert_eq!(postcard::to_slice(&a, &mut buf).unwrap(), [0, 2, 1, 0xB4, 0x4C, 0xF4, 0x03, 3]);

        // New tags.
        let f = Frame::Air(air(AirProto::Es1090, 14, 0x8D));
        assert_eq!(postcard::to_slice(&f, &mut buf).unwrap()[0], 16);
        let s = Frame::AeroStatus(AeroStatus {
            mode: AeroMode::FlarmHop { band: 2, ogn: false }.code(),
            freq_khz: 915_000,
            time: AeroTime::Unknown,
            parked: true,
            radio_ok: true,
            rx_restarts: 0,
            rejected: 0,
            rate_dropped: 0,
        });
        assert_eq!(postcard::to_slice(&s, &mut buf).unwrap()[0], 17);
        let t = Command::TimeSync { utc_ms: 1_763_337_600_123, pps: true };
        assert_eq!(postcard::to_slice(&t, &mut buf).unwrap()[0], 5);
        assert_eq!(NodeRole::Aero as u8, 6);
        assert_eq!(NodeRole::Aero.bit(), 0x40);
        assert_eq!(postcard::to_slice(&NodeRole::Aero, &mut buf).unwrap(), [6]);
        for p in 0..4u8 {
            let _: AirProto = postcard::from_bytes(&[p]).unwrap();
        }
        assert!(postcard::from_bytes::<AirProto>(&[4]).is_err());

        // Round trips, through the stream decoders.
        let mut dec = Decoder::new();
        for f in [f, s, Frame::Air(AirFrame { utc_ms: None, ..air(AirProto::Ogn, 26, 0) })] {
            let mut got = None;
            dec.feed(encode(&f, &mut buf).unwrap(), |r| got = Some(r));
            assert_eq!(got, Some(Ok(f)));
        }
        let mut cdec = CommandDecoder::new();
        let mut got = None;
        cdec.feed(encode_command(&t, &mut buf).unwrap(), |r| got = Some(r));
        assert_eq!(got, Some(Ok(t)));
        // An Aero assignment with every kind of entry.
        let modes = [
            AeroMode::Es1090.entry(0),
            AeroMode::Uat978.entry(u16::MAX),
            AeroMode::FlarmHop { band: 2, ogn: true }.entry(10_000),
            AeroMode::FlarmHop { band: 255, ogn: false }.entry(1),
        ];
        let assign = Command::Assign(Assignment { role: NodeRole::Aero, plan: modes.into_iter().collect(), id: 9 });
        let mut got = None;
        cdec.feed(encode_command(&assign, &mut buf).unwrap(), |r| got = Some(r));
        assert_eq!(got, Some(Ok(assign)));
    }

    /// The largest `Air` frame (64 raw bytes, widest varints) is small
    /// enough that a dozen fit one ESP-NOW batch, and far below `MAX_FRAME`.
    #[test]
    fn largest_air_frame_fits() {
        let mut buf = [0u8; 1024];
        let mut worst = 0;
        for p in [AirProto::Es1090, AirProto::Uat978, AirProto::Flarm, AirProto::Ogn] {
            for fill in [0x00, 0xFF] {
                worst = worst.max(encode(&Frame::Air(air(p, MAX_AIR_RAW, fill)), &mut buf).unwrap().len());
            }
        }
        std::println!("largest air frame: {worst} bytes");
        assert!(worst <= 100, "{worst}");
        assert!(worst <= MAX_FRAME);
        // A typical 1090 frame: 14 raw bytes.
        let es = Frame::Air(AirFrame {
            proto: AirProto::Es1090,
            freq_khz: 1_090_000,
            rssi: -71,
            utc_ms: Some(1_763_337_600_123),
            uptime_us: 3_600_000_000,
            raw: [0x8D; 14].into_iter().collect(),
        });
        assert!(encode(&es, &mut buf).unwrap().len() <= 40);
        let st = Frame::AeroStatus(AeroStatus {
            mode: u32::MAX,
            freq_khz: u32::MAX,
            time: AeroTime::HubPps,
            parked: true,
            radio_ok: true,
            rx_restarts: u32::MAX,
            rejected: u32::MAX,
            rate_dropped: u32::MAX,
        });
        assert!(encode(&st, &mut buf).unwrap().len() <= 40);
    }

    #[test]
    fn aero_mode_codes() {
        assert_eq!(AeroMode::Es1090.code(), 10_900);
        assert_eq!(AeroMode::Uat978.code(), 9_780);
        assert_eq!(AeroMode::FlarmHop { band: 2, ogn: false }.code(), 0x8000_0002);
        assert_eq!(AeroMode::FlarmHop { band: 1, ogn: true }.code(), 0x8000_0101);
        for m in [AeroMode::Es1090, AeroMode::Uat978, AeroMode::FlarmHop { band: 3, ogn: true }, AeroMode::FlarmHop { band: 0, ogn: false }] {
            assert_eq!(AeroMode::from_code(m.code()), Some(m));
        }
        for bad in [0, 9_000, 10_901, 0x8000_0200, 0xC000_0002, 0x8001_0002] {
            assert_eq!(AeroMode::from_code(bad), None, "{bad:#x}");
        }
    }

    #[test]
    fn commands_round_trip_and_fit() {
        let mut dec = CommandDecoder::new();
        for c in commands() {
            let mut buf = [0u8; MAX_FRAME];
            let wire = encode_command(&c, &mut buf).unwrap();
            std::println!("command {} bytes", wire.len());
            let mut got = None;
            dec.feed(wire, |r| got = Some(r));
            assert_eq!(got, Some(Ok(c)));
        }
    }

    /// Protocol 1-4 tags did not move; protocol 5 took 10 and 11, 6 took 12.
    #[test]
    fn protocol_4_and_5_append_only() {
        let mut buf = [0u8; MAX_FRAME];
        let tags: std::vec::Vec<u8> = samples()
            .iter()
            .map(|f| postcard::to_slice(f, &mut buf).unwrap()[0])
            .collect();
        assert_eq!(tags, [0, 1, 1, 1, 2, 3, 3, 4, 5, 6, 6, 7, 8, 9, 5, 10, 10, 11, 11, 12, 6]);
        assert_eq!(NodeRole::Fpv as u8, 3);
        assert_eq!(NodeRole::Gateway.bit(), 1 << 5);
    }

    /// Protocol 8 appended `DroneIdRecord::O4`: the older records keep their
    /// postcard indices, so a v7 `DroneId` frame encodes byte for byte as before.
    #[test]
    fn protocol_8_droneid_record_append_only() {
        let mut buf = [0u8; MAX_FRAME];
        let tag = |r: DroneIdRecord, buf: &mut [u8]| postcard::to_slice(&r, buf).unwrap()[0];
        assert_eq!(tag(DroneIdRecord::Encrypted { key_id: [0; 4] }, &mut buf), 1);
        assert_eq!(tag(DroneIdRecord::Detected, &mut buf), 2);
        let o4 = DroneIdRecord::O4 { kind: O4Kind::Cryp, key_id: [1, 2, 3, 4], object: [7u8; 173].into_iter().collect() };
        assert_eq!(tag(o4.clone(), &mut buf), 3);
        assert_eq!(postcard::to_slice(&O4Kind::Infp11, &mut buf).unwrap(), [2]);
        let f = Frame::DroneId(DroneIdSighting { freq: 58165, rssi: -60, snr_db: 9, uptime_us: 5, record: o4 });
        let mut dec = Decoder::new();
        let mut got = None;
        dec.feed(encode(&f, &mut buf).unwrap(), |r| got = Some(r));
        assert_eq!(got, Some(Ok(f)));
    }

    /// Protocol 7 appended four `RfKind`s; the six older ones keep their
    /// postcard indices, so a v6 `Rf` frame encodes byte for byte as before.
    #[test]
    fn protocol_7_rf_kinds_append_only() {
        let kinds = [
            RfKind::AnalogVideo,
            RfKind::FpvCarrier,
            RfKind::Hdzero,
            RfKind::LteStyle,
            RfKind::WifiNarrow,
            RfKind::Energy,
            RfKind::DjiVideo,
            RfKind::DjiO4Video,
            RfKind::DjiFpvAirUnit,
            RfKind::DjiController,
        ];
        let mut buf = [0u8; MAX_FRAME];
        let mut dec = Decoder::new();
        for (i, kind) in kinds.into_iter().enumerate() {
            assert_eq!(postcard::to_slice(&kind, &mut buf).unwrap(), [i as u8]);
            let f = Frame::Rf(RfSighting {
                kind,
                freq: 57_945,
                width_khz: 18_000,
                snr_db: 14,
                duty_pct: 90,
                line_hz: 0,
                uptime_us: 1,
            });
            let mut got = None;
            dec.feed(encode(&f, &mut buf).unwrap(), |r| got = Some(r));
            assert_eq!(got, Some(Ok(f)));
        }
    }

    /// Protocol 9 appended `Frame::Threat` as tag 13; every kind round-trips
    /// and the largest fits a frame.
    #[test]
    fn protocol_9_threat_appended() {
        let mut buf = [0u8; MAX_FRAME];
        let mut dec = Decoder::new();
        for k in 0..=15u8 {
            let kind: ThreatKind = postcard::from_bytes(&[k]).unwrap();
            let f = Frame::Threat(ThreatSighting {
                kind,
                channel: u8::MAX,
                freq: u32::MAX,
                width_khz: u32::MAX,
                level: i8::MIN,
                count: u32::MAX,
                window_ms: u32::MAX,
                source: [0xFF; 6],
                target: [0xFF; 6],
                detail: u32::MAX,
                ssid: heapless::String::try_from("x".repeat(32).as_str()).unwrap(),
                uptime_us: u64::MAX,
            });
            let wire = encode(&f, &mut buf).unwrap();
            assert!(wire.len() < MAX_FRAME);
            assert_eq!(postcard::to_slice(&f, &mut [0u8; MAX_FRAME]).unwrap()[0], 13);
            let mut got = None;
            dec.feed(wire, |r| got = Some(r));
            assert_eq!(got, Some(Ok(f)));
        }
        assert!(postcard::from_bytes::<ThreatKind>(&[16]).is_err());
        assert!(ThreatKind::Blinded.is_jamming() && !ThreatKind::Deauth.is_jamming());
    }

    #[test]
    fn round_trip() {
        let mut dec = Decoder::new();
        for f in samples() {
            let mut buf = [0u8; MAX_FRAME];
            let wire = encode(&f, &mut buf).unwrap();
            assert_eq!(wire.last(), Some(&0));
            assert!(!wire[..wire.len() - 1].contains(&0));
            let mut got = None;
            dec.feed(wire, |r| got = Some(r));
            assert_eq!(got, Some(Ok(f)));
        }
    }

    /// The worst case: a full pack of non-zero bytes and every integer at its
    /// widest varint. All-0xFF content maximises both COBS and varint size.
    #[test]
    fn largest_detection_fits() {
        let mut big = [0u8; 1024];
        let mut worst = 0;
        for f in samples() {
            worst = worst.max(encode(&f, &mut big).unwrap().len());
        }
        std::println!("largest encoded frame: {worst} bytes");
        let mut buf = [0u8; MAX_FRAME];
        let sighting = encode(&samples()[5], &mut buf).unwrap().len();
        std::println!("largest open-link sighting: {sighting} bytes");
        assert!(
            sighting <= 48,
            "{sighting}: keep sightings small, they can be frequent"
        );
        assert!(worst <= MAX_FRAME, "{worst} > {MAX_FRAME}");
        let mut exact = [0u8; MAX_FRAME];
        assert!(encode(&samples()[1], &mut exact).is_ok());
    }

    /// Frames split at every possible point, run together, and mixed with
    /// junk still come out in order.
    #[test]
    fn streaming_split_and_resync() {
        let mut stream = std::vec::Vec::new();
        stream.extend_from_slice(&[0x13, 0x37, 0x42]); // tail of a frame we joined mid-way
        stream.push(0);
        stream.extend_from_slice(&[0, 0]); // idle padding
        for f in samples() {
            let mut buf = [0u8; MAX_FRAME];
            stream.extend_from_slice(encode(&f, &mut buf).unwrap());
        }
        for split in 0..stream.len() {
            let mut dec = Decoder::new();
            let mut out = std::vec::Vec::new();
            dec.feed(&stream[..split], |r| out.push(r));
            dec.feed(&stream[split..], |r| out.push(r));
            assert_eq!(out[0], Err(LinkError::Corrupt), "split {split}");
            let frames: std::vec::Vec<_> = out[1..].to_vec();
            assert_eq!(frames, samples().map(Ok).to_vec(), "split {split}");
        }
        // Byte at a time.
        let mut dec = Decoder::new();
        let mut n = 0;
        for b in &stream {
            dec.feed(core::slice::from_ref(b), |r| n += r.is_ok() as usize);
        }
        assert_eq!(n, samples().len());
    }

    /// The variants that existed in protocol 1 encode exactly as they did:
    /// their postcard tags are 0, 1 and 2, and OpenLink got the next one.
    #[test]
    fn protocol_1_frames_unchanged() {
        let mut buf = [0u8; MAX_FRAME];
        for (f, tag) in samples().iter().zip([0, 1, 1, 1, 2, 3, 3]) {
            let raw = postcard::to_slice(f, &mut buf).unwrap();
            assert_eq!(raw[0], tag, "{f:?}");
        }
        let real = OpenLinkSighting {
            link: openlink::OpenLink {
                system: openlink::System::WfbNg,
                id: Some(openlink::WFB_DEFAULT_LINK_ID),
                port: 0,
                direction: openlink::Direction::AirToGround,
                tx: [0x57, 0x42, 0x75, 0x05, 0xD6, 0x00],
            },
            channel: 161,
            rssi: -63,
            uptime_us: 12_345_678,
            packets: 4321,
        };
        let mut dec = Decoder::new();
        let mut got = None;
        dec.feed(
            encode(&Frame::OpenLink(real.clone()), &mut buf).unwrap(),
            |r| got = Some(r),
        );
        assert_eq!(got, Some(Ok(Frame::OpenLink(real))));
    }

    #[test]
    fn overflow_then_recovery() {
        let mut dec = Decoder::new();
        let mut out = std::vec::Vec::new();
        dec.feed(&[0x55; MAX_FRAME + 40], |r| out.push(r));
        dec.feed(&[0x55, 0x00], |r| out.push(r));
        let mut buf = [0u8; MAX_FRAME];
        let f = samples()[3].clone();
        dec.feed(encode(&f, &mut buf).unwrap(), |r| out.push(r));
        assert_eq!(out.first(), Some(&Err(LinkError::Overflow)));
        assert_eq!(out.last(), Some(&Ok(f)));
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut dec = Decoder::new();
        let mut x: u64 = 0x1234_5678_9ABC_DEF0;
        let mut chunk = [0u8; 97];
        for _ in 0..20_000 {
            for b in chunk.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = if x.is_multiple_of(11) { 0 } else { x as u8 };
            }
            dec.feed(&chunk[..(x % 98) as usize], |_| {});
        }
    }

    /// Protocol 12 appended `Frame::Control` (18), `Frame::ControlEnergy`
    /// (19) and two `AeroMode` codes. Every v11 frame and command encodes
    /// byte for byte as before: this hash of v11 samples was taken from the
    /// v11 code (HEAD d10b2e6), with the hub's Hello saying 11.
    #[test]
    fn protocol_12_control_append_only() {
        let mut buf = [0u8; 1024];
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut fnv = |bytes: &[u8]| {
            for &b in bytes {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        for mut f in samples() {
            if let Frame::Hello { protocol, .. } = &mut f {
                *protocol = 11;
            }
            fnv(encode(&f, &mut buf).unwrap());
        }
        for c in commands() {
            fnv(encode_command(&c, &mut buf).unwrap());
        }
        for p in [AirProto::Es1090, AirProto::Uat978, AirProto::Flarm, AirProto::Ogn] {
            fnv(encode(&Frame::Air(air(p, 26, 0xA5)), &mut buf).unwrap());
        }
        let st = AeroStatus {
            mode: AeroMode::FlarmHop { band: 2, ogn: true }.code(),
            freq_khz: 915_000,
            time: AeroTime::HubPps,
            parked: true,
            radio_ok: true,
            rx_restarts: 3,
            rejected: 4,
            rate_dropped: 5,
        };
        fnv(encode(&Frame::AeroStatus(st), &mut buf).unwrap());
        fnv(encode_command(&Command::TimeSync { utc_ms: 1_763_337_600_123, pps: true }, &mut buf).unwrap());
        let plan = [AeroMode::Es1090.entry(0), AeroMode::Uat978.entry(10_000), AeroMode::FlarmHop { band: 2, ogn: false }.entry(10_000)];
        fnv(encode_command(&Command::Assign(Assignment { role: NodeRole::Aero, plan: plan.into_iter().collect(), id: 7 }), &mut buf).unwrap());
        assert_eq!(h, 0x0e41_4e95_f735_c415, "a v11 frame or command changed encoding");

        // New tags.
        let c = Frame::Control(control_report(true, 1));
        assert_eq!(postcard::to_slice(&c, &mut buf).unwrap()[0], 18);
        let e = Frame::ControlEnergy(control_energy(true));
        assert_eq!(postcard::to_slice(&e, &mut buf).unwrap()[0], 19);
        // Round trips through the stream decoder.
        let mut dec = Decoder::new();
        for f in [c, e, Frame::Control(control_report(false, 0)), Frame::ControlEnergy(control_energy(false))] {
            let mut got = None;
            dec.feed(encode(&f, &mut buf).unwrap(), |r| got = Some(r));
            assert_eq!(got, Some(Ok(f)));
        }
        // A control plan round-trips as an Assign.
        let plan = [
            AeroMode::Uat978.entry(10_000),
            AeroMode::Control { regions: 0x3FF, energy: 0x3FF }.entry(25_000),
            AeroMode::ControlFlags { read_ids: true, decode_content: true }.entry(0),
        ];
        let a = Command::Assign(Assignment { role: NodeRole::Aero, plan: plan.into_iter().collect(), id: 12 });
        let mut cdec = CommandDecoder::new();
        let mut got = None;
        cdec.feed(encode_command(&a, &mut buf).unwrap(), |r| got = Some(r));
        assert_eq!(got, Some(Ok(a)));
    }

    fn control_report(full: bool, part: u8) -> ControlReport {
        use ctrlwatch::content::{AltRef, Channels, Position, PositionKind, PositionSource};
        let sighting = ctrlwatch::ControlSighting {
            system: ctrlwatch::System::FlyskyAfhds2a,
            region: ctrlwatch::Region::Ism2g4,
            probe: ctrlwatch::Probe::Elrs { rate: ctrlwatch::elrs::ElrsRate::L900_25, version: ctrlwatch::elrs::OtaVersion::V3 },
            freq_hz: u32::MAX,
            rssi_dbm: i16::MIN,
            snr_db: i8::MIN,
            t_us: u64::MAX,
            confidence: ctrlwatch::Confidence::Confirmed,
            direction: ctrlwatch::Direction::Downlink,
            id: full.then_some(ctrlwatch::LinkId::ElrsUid { uid3: Some(0xFF), uid4: 0xFF, uid5: 0xFF, uid5_real: 0xFF }),
            version: Some(u8::MAX),
        };
        let mut content = Vec::new();
        if full {
            let mut ch = Channels { us: [u16::MAX; 16], present: u32::MAX, armed: Some(true) };
            ch.set(0, 0xFFFF);
            let _ = content.push(ctrlwatch::ControlContent::Channels(ch));
            let _ = content.push(ctrlwatch::ControlContent::Position(Position {
                kind: PositionKind::Home,
                lat_e7: i32::MIN,
                lon_e7: i32::MIN,
                alt_mm: Some((i32::MIN, AltRef::Home)),
                rel_alt_mm: Some(i32::MIN),
                ground_speed_cms: Some(u16::MAX),
                heading_cdeg: Some(u16::MAX),
                satellites: Some(u8::MAX),
                fix_type: Some(u8::MAX),
                source: PositionSource::MavHomePosition,
            }));
        }
        ControlReport {
            sighting,
            best: ctrlwatch::Confidence::Likely,
            packets: if full { u32::MAX } else { 1 },
            confirmed: if full { u32::MAX } else { 0 },
            peak_rssi: i16::MIN,
            part,
            content,
            raw: full.then(|| core::iter::repeat_n(0xFF, ctrlwatch::MAX_RAW).collect()),
        }
    }

    fn control_energy(full: bool) -> ControlEnergy {
        let activity = if full {
            EnergyActivity::Hopper(HopperReport {
                low_khz: u32::MAX,
                high_khz: u32::MAX,
                channels: u16::MAX,
                spacing_khz: Some(u32::MAX),
                occupied_khz: u32::MAX,
                rssi: i8::MIN,
                spread_db: i8::MIN,
                burst_us: Some(u32::MAX),
                interval_us: Some(u32::MAX),
                periodic_pct: u8::MAX,
                candidates: core::iter::repeat_n((ctrlwatch::System::FlyskyAfhds2a, u8::MAX), 4).collect(),
            })
        } else {
            EnergyActivity::Background
        };
        ControlEnergy {
            region: ctrlwatch::Region::Ism2g4,
            windows: u32::MAX,
            activity,
            steady: core::iter::repeat_n((u32::MAX, i8::MIN), if full { 4 } else { 0 }).collect(),
            uptime_us: u64::MAX,
        }
    }

    /// The largest control frames fit `MAX_FRAME`, and the new variants do
    /// not make `Frame` (and so every hub queue of frames) bigger.
    #[test]
    fn largest_control_frames_fit() {
        let mut buf = [0u8; 1024];
        let worst = [
            Frame::Control(control_report(true, 1)),
            Frame::ControlEnergy(control_energy(true)),
        ]
        .iter()
        .map(|f| encode(f, &mut buf).unwrap().len())
        .max()
        .unwrap();
        std::println!("largest control frame: {worst} bytes; Frame {} bytes", core::mem::size_of::<Frame>());
        assert!(worst <= MAX_FRAME, "{worst} > {MAX_FRAME}");
        // A summary (no content, no raw) is small: it can be frequent.
        let s = encode(&Frame::Control(control_report(false, 0)), &mut buf).unwrap().len();
        assert!(s <= 48, "{s}");
        assert!(core::mem::size_of::<Frame>() <= 288, "Frame grew: {}", core::mem::size_of::<Frame>());
    }

    #[test]
    fn control_mode_codes() {
        assert_eq!(AeroMode::Control { regions: 1, energy: 0 }.code(), 0x4000_0001);
        assert_eq!(AeroMode::Control { regions: 0, energy: 4 }.code(), 0x4000_1000);
        assert_eq!(AeroMode::Control { regions: 0xFFFF, energy: 0xFFFF }.code(), 0x400F_FFFF, "only defined region bits");
        assert_eq!(AeroMode::ControlFlags { read_ids: false, decode_content: false }.code(), 0x2000_0000);
        assert_eq!(AeroMode::ControlFlags { read_ids: true, decode_content: true }.code(), 0x2000_0003);
        for m in [
            AeroMode::Control { regions: 0, energy: 0 },
            AeroMode::Control { regions: 0x241, energy: 0x004 },
            AeroMode::ControlFlags { read_ids: true, decode_content: false },
            AeroMode::ControlFlags { read_ids: false, decode_content: true },
        ] {
            assert_eq!(AeroMode::from_code(m.code()), Some(m));
        }
        for bad in [0x4010_0000, 0x2000_0004, 0x6000_0001, 0xC000_0002] {
            assert_eq!(AeroMode::from_code(bad), None, "{bad:#x}");
        }
        assert!(AeroMode::ControlFlags { read_ids: false, decode_content: false }.is_flags());
        // The documented bit order is ctrlwatch's Region::ALL.
        use ctrlwatch::Region as R;
        let order = [R::Fcc915, R::Au915, R::Eu868, R::In866, R::Au433, R::Eu433, R::Us433, R::Us433W, R::Th920, R::Ism2g4];
        assert_eq!(order, R::ALL);
        for (i, r) in order.into_iter().enumerate() {
            assert_eq!(control_region_bit(r), 1 << i, "{r:?}");
        }
        assert!(!AeroMode::Control { regions: 1, energy: 0 }.is_flags());
    }
}
