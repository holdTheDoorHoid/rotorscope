# Attacks on drones and their operators (link protocol 13)

Detectors for attacks aimed at drones, their links and the people flying them, as opposed to
attacks on the detector's own radios (docs/WIFI-ATTACKS.md, docs/JAMMING.md). Each detector has
its own section below; each raises one `link::ThreatKind`, which the hub keeps in its threat
table, shows on screen (the threat badge and the console's `THREAT` line), writes to the
`{"t":"threat"}` JSON stream and logs in the evidence log.

## Hostile MAVLink telemetry

`ThreatKind::MavlinkAttack`, detector `crates/mavwatch`, run on the LR2021 node
(`firmware/node-lr/src/control.rs`). Branch `mavwatch`, 2026-11-17.

**Status: written and host-tested (31 mavwatch tests, plus tests in ctrlwatch, tracker and
evidence); both firmware images build; never run against a real telemetry radio.**

### In plain language

Many drones (ArduPilot, PX4) talk to the pilot's laptop or tablet over a telemetry radio
(SiK/RFD900, or mLRS) using a language called MAVLink. Security researchers have shown in 2026
that the *drone side* of that conversation can attack the *laptop*: a drone, or anyone
transmitting on its radio link, can send messages that make common ground-station programs
(QGroundControl, Mission Planner, MAVSDK, mavros) overwrite files, open malicious links,
fill the disk or crash. Other published holes go the other way, letting someone on the link
open a command shell on the drone.

When the operator has turned on `control content on` (see docs/CONTROL-LINKS.md, "The two
settings"), the LR2021 node now reads the MAVLink on the telemetry links it hears and looks for
those attacks. It raises an alarm called **"Hostile drone telemetry"**, with a number saying
which attack it looks like (the table below) and a short excerpt (for example the poisoned web
address).

**What it can tell you**

- That someone on a drone's telemetry link sent bait for a known ground-station exploit (a
  booby-trapped file address, a fake web link, a poisoned file listing, impossible settings).
- That someone is trying to reach a drone's command shell.
- With `control ids on` as well: that a drone's link has a second, uninvited transmitter on it
  (two senders using one drone's identity, commands without the signature the real pilot uses,
  or old signed messages being played back).

**What it can't tell you**

- Whether the attack worked. It sees the bait, not whether a ground station took it.
- Anything on encrypted links (SiK with AES, mLRS with encryption on).
- Attacks carried over the internet rather than the radio (a normal-looking web address that
  serves a harmful file).
- Who is transmitting. MAVLink "system IDs" are claimed, not proven.

**How it avoids false alarms**

- Only messages that pass MAVLink's own checksum are looked at, so radio noise never counts.
- The single-message alarms look for things a working drone never sends, such as a camera
  description at a `file:///` address or a file listing entry named `../../x`.
- Things a fault could cause (a garbled status message, jumps in message numbering) need
  repeats within a time window before they count.
- A packet the radio repeats is counted once.
- The link-integrity checks only run when the node can tell links apart (`control ids on`):
  otherwise two honest drones flying nearby, both with the default ID 1, would look like an
  intruder.
- **Your own console counts.** If you use your ground station's MAVLink shell or "serial
  passthrough" over the telemetry radio, kind 13 fires. Mute it ("mute alarms" on the
  screen, or the site line `mute mavlink_attack source=00:00:00:00:ff:be` for a ground station
  with system ID 255, component 190): the rule silences that sender's MAVLink alarms.

### The setting it needs

**Off unless `control content on`.** The scan reads message content, so it obeys the same
opt-in as decoding positions and telemetry (docs/CONTROL-LINKS.md). Kinds 16-18 also need
`control ids on`. Nothing about the scan is transmitted; the node stays receive-only.

### Kinds (`detail` on the threat)

| detail | name | what it means | fires on | source |
|---|---|---|---|---|
| 1 | URI traversal | a camera definition / component metadata / video URI with `../`, `..\` or percent-encoded forms (`%2e%2e/`, `%252e`, `%c0%ae`) | one message | QGC-01, QGC-02, MAVSDK-02 |
| 2 | URI scheme | such a URI with a scheme off the field's list (`file://`, `qrc:`; allowed: `http`, `https`, `mftp`, `mavlinkftp`, for video also `rtsp(s)`, `udp`, `tcp`, `rtp` or a port number), a bare absolute or UNC path, or control characters | one message | QGC-02, MAVSDK-01 |
| 3 | camera name | CAMERA_INFORMATION vendor or model name containing `/`, `\`, `..` or control characters (QGC builds a file path from them) | one message | QGC-01 |
| 4 | video pipeline | VIDEO_STREAM_INFORMATION URI starting `gst:` or containing ` ! ` (GStreamer element syntax) | one message | MP-02 |
| 5 | status link | STATUSTEXT containing Mission Planner's `[link;target;label]` markup, also when split across two messages | one message | MP-03 |
| 6 | status control | STATUSTEXT with control characters (terminal escapes, etc.; tab/newline/CR allowed) | 2 messages within 60 s | DRONEKIT-01 (text handed to apps), defensive |
| 7 | FTP traversal | a MAVLink FTP directory listing whose entry name contains `/`, `\` or `..` (`.` and `..` as whole entries are normal) | one message | QGC-03, MP-01, MAVROS-02 PoC |
| 8 | FTP offset | a MAVLink FTP read acknowledgement at offset 0xF000_0000 (3.75 GiB) or more | one message | QGC-03 |
| 9 | FTP malformed | FTP size field over 239; open acknowledgement longer than 4 bytes; write acknowledgement claiming over 239 bytes written; request path of 200 bytes or more | one message | MAVROS-02; OWASP (bound FTP paths) |
| 10 | rally/fence count | PARAM_VALUE `RALLY_TOTAL` or `FENCE_TOTAL` not a whole number 0-255 (NaN, 3.5, 1e30, -1) | one message | MP-04 |
| 11 | parameter flood | about 3,100 distinct parameter names from one system within 10 minutes | threshold | MAVROS-01 |
| 12 | non-finite PARAM_SET | PARAM_SET of a float parameter to NaN or infinity | one message | OWASP (typed-payload validation) |
| 13 | serial access | SERIAL_CONTROL on the air at all (remote shell, PX4 NSH) | one message | CVE-2026-1579 |
| 14 | serial overflow | SERIAL_CONTROL whose `count` exceeds its 70-byte data field | one message | CVE-2026-38971 |
| 15 | signing key on air | SETUP_SIGNING over the radio: the link's secret key in the clear, or someone re-keying the drone. The key is never copied into the alarm | one message | MAVLink signing guide (keys go over USB) |
| 16 | signing downgrade | unsigned commands from a system that sent signed frames on the same link in the last 10 s | 2 within 30 s | CVE-2020-10283 |
| 17 | injected frames | two interleaved sequence-number streams for one system/component | 10 switches between two live streams within 10 s, 5+ frames each | injection (SiKW00F) |
| 18 | replayed signatures | a signed frame whose timestamp is not newer than one already seen on the same signing stream | 2 within 60 s | MAVLink signing spec |

"Commands" for kind 16: SET_MODE, PARAM_SET, MISSION_COUNT, MANUAL_CONTROL,
RC_CHANNELS_OVERRIDE, MISSION_ITEM_INT, COMMAND_INT, COMMAND_LONG, FILE_TRANSFER_PROTOCOL,
SERIAL_CONTROL, SETUP_SIGNING.

### Thresholds and why

Every number is a constant in `crates/mavwatch/src/analyse.rs`, documented there.

- **One message is enough** for kinds 1-5, 7-10 and 12-15: each looks for a shape that a
  working autopilot, camera or companion computer does not send. What *is* normal was taken
  from the field documentation and from real firmware: PX4 publishes its component metadata as
  `mftp://[;comp=1]/etc/extras/component_general.json.xz` (an absolute path inside `mftp://`
  is fine), cameras use `http://` or `mftp://` definitions, ArduPilot's `RALLY_TOTAL` and
  `FENCE_TOTAL` are 8-bit parameters.
- **Status text with control characters: 2 in 60 s.** One garbled message from a buggy
  component is the "benign oddity"; a run of terminal escapes is deliberate.
- **FTP offset 0xF000_0000.** FAT32, the SD card format autopilots use, cannot hold a file that
  big; the published proof of concept uses 0xFFFF_FFF0.
- **FTP path 200 bytes.** Real autopilot paths are short (`/APM/LOGS/00000042.BIN`); ArduPilot's
  FTP data field is 239 bytes.
- **Parameter flood: ~3,100 names in 10 minutes** (2,600 bits set in an 8,192-bit set per
  system). ArduPilot has about 1,300-1,800 parameters and PX4 about 1,100; a re-download on
  reconnect repeats the same names and adds nothing.
- **Signing downgrade: 2 unsigned commands in 30 s, with signed frames in the last 10 s.** An
  operator who switches signing off stops sending signed frames; 10 s later their unsigned
  commands no longer count.
- **Injected frames: 10 switches in 10 s, 5 frames on each stream; a stream silent for 2 s is
  dropped.** A lossy link or a reboot causes one or two switches; an injector running alongside
  the real sender causes one per injected frame.
- **Replay: 2 in 60 s.** Byte-identical frames within 3 s are one frame heard twice (SiK resends
  unacknowledged packets), so a replay faster than 3 s is not seen.
- **Reports**: a finding that continues is reported again at most every 2 s (the hub's threat
  stays active for 10 s after a report).

### How it works

- `crates/mavwatch` (no_std, heapless, about 8 KB fixed): a CRC-checked MAVLink v1/v2 scanner
  for 27 messages (CRC_EXTRA computed from mavlink/mavlink `common.xml`, commit 2a8f8cf, and
  checked against pymavlink's published values), signed-frame parsing, reassembly of frames that
  span radio packets (`Watch`), and the analyser. `ctrlwatch::mavlink` now uses the same
  scanner.
- `ctrlwatch::serial_bytes`: the MAVLink bytes of one SiK packet (de-whitened, CRC-checked, TDM
  trailer and command packets removed) or one mLRS frame (unencrypted, with its direction).
- **Node, not hub.** The hub only receives a packet's first 96 bytes, at most 20 packets a
  second; most of what the scan needs (camera and video URIs, FTP, any frame split across two
  SiK packets) never reaches it whole. The node sees every packet in full. The scan runs inside
  the node's existing control scan, only with content decoding on.
- **Link IDs.** With `control ids on`, each SiK NET ID / mLRS sync word gets its own stream and
  the integrity kinds run. The ID stays in the node as a stream key; it is not sent with the
  alarm.
- **What reaches the hub**: a `Frame::Threat` (`MavlinkAttack`, `detail` = kind, `source` =
  `00:00:00:00:<sysid>:<compid>` of the sender, `target` the same for the addressee when the
  message names one, `ssid` = the excerpt, `freq`/`level` = the packet's frequency and signal),
  then a `Frame::MavlinkEvidence` with the MAVLink frame itself (up to 255 bytes), which the hub
  logs raw as evidence (frame kind `MAVLINK`, 10). The node also prints a
  `{"t":"mavlink_attack"}` line on its own console.
- **Hub**: the threat table keeps one alarm per sender and kind (`crates/tracker/src/
  threats.rs`); a mute silences that sender. No new hub code runs per packet, so the P4 stack is
  unchanged.

### Sources

- infected-drones (github.com/nicholasaleks/infected-drones), finding READMEs and PoCs read for
  exact payload shapes: QGC-01, QGC-02, QGC-03, MP-01, MP-02, MP-03, MP-04, MAVSDK-01/02/03,
  MAVROS-01, MAVROS-02, DRONEKIT-01. The PoCs were read, never run.
- OWASP drone security top 10 (2026 revision): CVE-2026-1579 (PX4 SERIAL_CONTROL shell without
  signing), CVE-2026-38971 (ArduPilot SERIAL_CONTROL out-of-bounds read), CVE-2020-10283
  (MAVLink signing downgrade); the recommendations to reject non-finite PARAM_SET values and
  bound FTP path lengths.
- MAVLink message definitions (mavlink/mavlink `message_definitions/v1.0/{minimal,standard,
  common}.xml`), the MAVLink FTP and signing guides on mavlink.io.

### Not verified (needs hardware)

- Everything on air: no real SiK or mLRS link carrying MAVLink has been through the node.
- Whether SiK packets on air split MAVLink frames the way the reassembly assumes (it handles
  any split, and loses a frame when a packet in the middle is missed).
- With `control ids` off, SiK uplink and downlink packets of every link share one stream; a
  frame split across packets from two directions is lost (never misread: the checksum fails).
- How often real telemetry trips the thresholds: they come from the documented behaviour of
  ArduPilot and PX4 and from synthetic tests, not from recordings. In particular kind 6 on
  third-party components with sloppy text, and kind 17 on very lossy links.
- No ground station was attacked to confirm that the frames these tests build are the ones the
  PoCs send byte for byte; they were built from the READMEs' field values.
- Not covered: decompression bombs behind normal-looking `.xz` URIs (MAVSDK-03), malicious
  DataFlash logs (QGC-04), anything delivered over IP rather than the radio, MAVLink over open
  Wi-Fi telemetry bridges (UDP 14550; not yet forwarded by the Wi-Fi nodes).
