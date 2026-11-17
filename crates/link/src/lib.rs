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
pub const PROTOCOL_VERSION: u8 = 9;

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

/// Protocol 4. Hub-to-node messages, framed exactly like [`Frame`] (postcard +
/// COBS) on the reverse direction of a wired link or through an ESP-NOW
/// gateway. Keep new variants last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Listen as described. Idempotent: resent every few seconds.
    Assign(Assignment),
    /// Gateway only: the next command is for the node with this station
    /// address (the reverse of [`Frame::Via`]).
    To { node: [u8; 6] },
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

    fn commands() -> [Command; 3] {
        let mut plan = Vec::new();
        while plan.push((u32::MAX, u16::MAX)).is_ok() {}
        [
            Command::Assign(Assignment { role: NodeRole::DroneId, plan, id: u16::MAX }),
            Command::Assign(Assignment { role: NodeRole::Wifi24, plan: Vec::new(), id: 1 }),
            Command::To { node: [0xFF; 6] },
        ]
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
}
