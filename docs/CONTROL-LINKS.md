# Control-link detection (Phase 9, `crates/ctrlwatch`)

## In plain language

The LR2021 node listens for the radio links people use to fly drones and
model aircraft: the stick commands going up and the telemetry coming down.

**What it can tell you**

- *A control link is on the air here*, which system it is, which band and
  rate, how strong, and how sure it is.
- For **ExpressLRS** (the most common open FPV control link) it can be
  **sure** without knowing the pilot's binding phrase. Once in a while every
  ELRS transmitter sends a "sync" packet on a fixed channel; that packet
  carries part of the link's ID and its checksum is built from exactly that
  part, so the node can check it. A packet that passes is a confirmed ELRS
  link.
- **mLRS**, **SiK/RFD900** telemetry radios and **OpenLRSng** are also
  confirmed from single packets (each carries its own checksum).
- **Crossfire, Tracer, Ghost, FrSky and FlySky** are closed systems. The
  node only sees their energy: which channels light up, how far apart, how
  often. That gives a list of likely systems, not a certain answer.

**What it can't tell you**

- ExpressLRS in its fastest racing modes (2.4 GHz "F1000/F500/D500/D250" and
  the "K1000" FSK modes) cannot be received at all without the binding
  phrase, because the radio's own sync word is made from it. The LR2021 has
  the right modem (FLRC) but still needs the phrase. These show up as energy
  only (a 1 MHz grid of 80 channels with very short bursts every 1-2 ms is
  distinctive).
- mLRS's FLRC and FSK modes: the same, energy only.
- Encrypted links (SiK with AES, mLRS with privacy on) are detected, but
  their content is reported as "encrypted". Nothing tries to break it.
- Where the pilot is. Some links carry the drone's GPS position in their
  telemetry; that is decoded when content decoding is on (below).

**How it avoids false alarms**

- *Packets*: a sighting is only **Confirmed** when the packet passes the
  system's own checksum. Packets that merely have the right size on the
  right settings are **Likely**, and a Likely link is not reported until it
  has been heard at least three times, with a decent signal-to-noise ratio
  (weak "packets" at the edge of the noise are usually the radio
  triggering on nothing). Tests feed random packets through each
  recogniser (200,000 for ELRS, tens of thousands for the others); at most
  one in a few hundred thousand passes as an ELRS sync, none as mLRS, SiK or
  OpenLRSng.
- *Energy*: Philadelphia's 902-928 MHz band is full of smart electricity,
  gas and water meters (themselves frequency hoppers), LoRaWAN gateways and
  sensors. The energy map learns what is normally there, channel by
  channel, over minutes, and only looks at activity above that. A drone
  link is one transmitter, so all its channels arrive at about the same
  strength; a meter network is hundreds of transmitters at different
  distances, so its levels are all over the place. And a control link sends
  on a strict clock (ELRS every 4, 5, 10, 20 or 40 ms; FlySky every
  3.85 ms), while meters send when they have something to say. Only a
  pattern that fits all three is called a hopper, and only one whose timing
  fits gets a high score.
- Steady carriers (something that never stops transmitting) are listed
  separately and never mistaken for a hopping link.

**The legal line**

A 2020 DOJ/FAA/FCC/DHS advisory warns that monitoring drone control links
can fall under the Wiretap Act and the Pen/Trap statute. So (how to turn
the two opt-ins on: "The two settings" below):

- **By default** the node reports presence and energy only: which system,
  where in the band, how strong, how sure. No identifiers, no content, no
  raw packets are kept.
- **Identifiers are opt-in** (`Config::read_ids`): ELRS UID bytes, the SiK
  NET ID, the mLRS frame sync word and the OpenLRSng "magic". Without the
  flag, the ELRS recogniser still *uses* the UID bytes in the packet for a
  moment to check its checksum, but does not keep or report them.
- **Content is opt-in, separately** (`Config::decode_content`). The owner
  enabled content decoding on 2026-11-17 on their stated permission. With
  it on, the node decodes stick/switch positions, telemetry (battery, link
  quality, flight mode, attitude, altitude) and GPS positions, keeps the raw
  packet for the evidence log, and can "follow" an ExpressLRS link's hops.
  Recovering the missing bytes of an ELRS link ID from observations
  rebuilds an identifier, so it needs **both** flags.
- **No encryption is broken.** Encrypted payloads are reported as such.
- **Receive-only.** There is no transmit code of any kind.

**Second transmitter on a link.** The node also checks whether a link is
heard from more radios than it can have (a second ExpressLRS transmitter on
one UID, a third SiK radio on one NET ID: what SiKW00F-style injection and
ExpressLRS hijacking look like) and reports it as a `ControlContested`
threat. How, its thresholds and its limits: docs/DRONE-ATTACKS.md, "Second
transmitter on a control link". It uses identifiers inside the node only, and
its MAVLink check only runs with content decoding on.

## On the node and the hub (2026-11-17, branch `ctrl-node`)

**Status: written, host-tested and built for both boards; never run with an LR2021.** No LR2021
board was in hand. Everything about how it behaves on air is what the code is meant to do.

### In plain language

- **Who listens.** The same LR2021 "aero" node that hears aircraft (docs/AERO-NODE.md). Its
  radio does one thing at a time, so the hub shares it out:
  - **One LR node:** aircraft and control links take turns, about **half the time each**. The
    aircraft turns (UAT 10 s, FLARM 10 s, OGN 5 s, and 1090 MHz 10 s when there is no ADSBee)
    are followed by one control-link turn as long as all of them together (25 s, or 35 s).
    Aircraft beacons repeat every second or so, so 25 s away loses little; a connected
    ExpressLRS link sends its sync packet every 5 s, so a 25 s turn gets through several scan
    steps.
  - **Two LR nodes:** one does aircraft only, the other control links only.
  - **More:** half of them (rounded down) do control links; two control nodes split the bands
    (900 MHz and 433 MHz on one, 2.4 GHz on the other).
  - The console command `a` can still pin every LR node to one thing; "control links only" is
    now one of its choices.
- **What it scans** (US default): ExpressLRS, mLRS, SiK and OpenLRSng on 902-928 MHz, the US
  433 MHz band and 2.4 GHz, plus an energy-only look at 863-870 MHz (European gear brought over).
  Between every two packet steps it sweeps one band for energy (round robin), which is how
  Crossfire, Tracer, Ghost, FrSky and FlySky show up (as "probably X"). A full pass of every
  step takes about 5 minutes of control time; with half-time sharing, about 10 minutes.
- **What reaches the hub.**
  - A line per link every 2 seconds while it is heard: which system, band, rate, how sure,
    signal, packet counts.
  - An energy report per band when a hopper or steady carrier shows up (otherwise one every
    30 s): the hopping pattern and the systems it could be.
  - With content decoding on (below): stick positions, telemetry (battery, link quality, flight
    mode, altitude) and GPS positions, plus the raw packet for the evidence log.
- **What the hub does with it.**
  - **SIGNALS page:** a CONTROL table at the top: system, band, how sure ("sure" = the packet
    passed the system's own checksum, "likely", "energy" = a pattern only), signal, and a detail
    column: stick positions or telemetry when decoded, else the rate; for an energy hopper, its
    channel count, spacing and packet interval.
  - **A drone position from a link's telemetry** (content decoding on, an ExpressLRS/mLRS/SiK
    link carrying GPS) becomes an ordinary drone on the radar and the list, named after the link
    ("ExpressLRS link 915"), and counts in the drone-near-aircraft warning like any drone. A home
    point is kept as the drone's home.
  - **Evidence log:** every control frame and energy report is logged raw, like every other
    detection (kinds "control" and "control energy").
  - **JSON lines** on the console: `{"t":"control"}` per link, `{"t":"control_energy"}` per band,
    and a `{"t":"control_stats"}` line with the counts and the two settings.

### The two settings, and how to turn them on

Both are **off by default**, on every hub, and the node obeys the hub: a node never reads IDs or
content unless the hub's plan tells it to (an older node that does not know the setting simply
never does).

| Setting | Off (default) | On |
|---|---|---|
| `control ids` | which system, how sure, signal | also the link's identifier (ExpressLRS UID bytes, SiK NET ID, mLRS sync word, OpenLRSng magic); links with different IDs are told apart |
| `control content` | no content, no raw packets; Wi-Fi nodes never look inside data frames | stick positions, telemetry, GPS positions, raw packets for evidence; ExpressLRS "follow" (stays with one link for 20 s to decode it); the hostile-MAVLink scan, on the LR2021 node's SiK / mLRS links **and on MAVLink over open Wi-Fi** (every Wi-Fi node) |

The owner stated on 2026-11-17 that they have permission for both. Turn them on per kit:

- **On the SD card:** add lines to `ROTORSCP/SITE.TXT` (later lines win):
  ```
  control ids on
  control content on
  ```
- **From the console:** press `L`, type `control content on` (or `off`, or `control ids on`),
  press Enter. The line is appended to `SITE.TXT`, so it survives a restart.

Every change, from the card or the console, is printed (`control links: read IDs on, decode
content on`) and logged in the evidence log as a Setting event. Rebuilding an ExpressLRS link's
missing ID bytes from what the node hears (needed to hop along with a link) needs **both**.
Encrypted links stay encrypted: nothing tries to break them.

**`control content on` also turns on the hostile-MAVLink scan** (docs/DRONE-ATTACKS.md,
"Hostile MAVLink telemetry"): the node reads the MAVLink on SiK and mLRS links for
ground-station exploit payloads and shell access, and raises a "Hostile drone telemetry" alarm.
Its link-integrity checks (injected, replayed or unsigned frames) also need `control ids on`.
Since link protocol 13 the same setting also turns the scan on in **every Wi-Fi node**, for
MAVLink sent over open (unprotected) Wi-Fi: telemetry bridges, Wi-Fi hobby drones, UDP 14550
and friends. The hub puts the setting in the Wi-Fi nodes' plans too, so turning it on or off
reaches them within a couple of seconds, and a node that restarts gets it again within 5 s.
There the integrity checks do not need `control ids` (a Wi-Fi link is told apart by its
network's address, which every beacon already shows). Details: docs/DRONE-ATTACKS.md,
"Hostile MAVLink telemetry", "Over Wi-Fi".

### Protocol (link protocol 12)

- `Frame::Control(ControlReport)`: one sighting (`part` 0: the 2-second summary, counts only) or
  one packet's decoded content (`part` 1, 2, ...: two items per frame, the raw packet in part 1).
  The largest is 246 bytes.
- `Frame::ControlEnergy`: one band's energy window.
- Hub to node: plan entries `AeroMode::Control { regions, energy }` (code bit 30, two region
  masks) and `AeroMode::ControlFlags { read_ids, decode_content }` (bit 29, settings only).
- Appended, so every protocol 11 frame and command is byte-for-byte the same (tested against a
  hash taken from the v11 code).

### How it is built

- `crates/lr2021cfg/src/ctrl.rs` (host-tested): ctrlwatch receive settings as chip settings
  (LoRa, GFSK fixed-length read, RSSI-only; FLRC refused because ctrlwatch's FLRC needs the
  binding phrase), multi-SF groups on the side detectors, the scan order, energy maps and
  frames, report splitting and rate limits, ExpressLRS follow mode.
- `firmware/node-lr/src/control.rs`: runs those jobs on the chip.
- `crates/coverage/src/plans.rs`: the time split above.
- `crates/tracker/src/ctrllinks.rs`: the hub's table (4 links, 3 bands; kept apart from the
  tracker, in PSRAM on the P4 hub next to the aero frame queue),
  positions to drone tracks, JSON lines; `crates/tracker/src/site.rs`: the `control` lines.
- Receive only: a test runs every scan, sweep and follow setting through the chip layer and
  finds no transmit command, and checks the node's radio code builds no command bytes itself.

### Not verified (needs hardware)

- Everything about the chip, as before, plus: multi-SF side detectors with implicit header and
  the 0x12 sync word on each (the side-detector sync byte format is taken as the main one's),
  which detector bit the packet status sets, RSSI readings while the FSK engine waits for a
  sync word that never comes (the sweep), and how fast retunes really are.
- ExpressLRS follow-mode timing on the C3 (retunes inside a 4 ms packet interval).
- Whether a 2 ms bin dwell catches a 1-ms ELRS burst often enough for the energy map; the
  thresholds were tuned on synthetic data.

### Bench checklist (P4 hub, no LR2021 needed)

1. Boot: no new errors; `hub-p4: site file: ... control links: IDs off, content off`.
2. Console `L`, `control content on`, Enter: `hub-p4: site (console): control content on`, then
   within 2 s `hub-p4: control links: read IDs off, decode content on (LR2021 nodes' plans)`;
   the card's `SITE.TXT` gains the line; the evidence log has two Setting events.
3. The `{"t":"control_stats"}` line appears once a setting is on, with `"content":true`.
4. Restart with the card in: the setting is back on (read from `SITE.TXT`), printed at mount.
5. With an aero node attached (later, with hardware): its assignment line (`hub-p4: assign ...
   -> Aero plan ...`) has one entry more than the aircraft turns (the control turn), and one
   more again with an opt-in on (the settings entry). Without one, the coverage tests
   (`cargo test -p coverage`) show the same plans.
6. `control content off`: printed and logged again; the plan loses its settings entry.
7. The SIGNALS page shows the CONTROL table ("none heard") above RF.

## How the node uses the crate

```rust
use ctrlwatch::{scan, recognise, Config, Region, System, Sightings};

let cfg = Config::default();                  // presence only
let mut table: Sightings<16> = Sightings::new();
for step in scan::plan(System::ExpressLrs, Region::Fcc915) {
    radio.apply(&step.config);                // map RxConfig -> LR2021 calls
    // listen step.dwell_ms; if anything recognised, up to step.extend_ms
    for (bytes, meta) in radio.packets() {    // meta: RxMeta { rssi_dbm, snr_db, t_us }
        if let Some(r) = recognise(&bytes, &step.config, meta, &cfg) {
            table.record(&r.sighting);
        }
    }
}
table.take_due(now_us, 2_000_000, |s| send_to_hub(s));
```

- `RxConfig { freq_hz, modulation, probe, region }`, `modulation` one of:
  - `LoRa { sf, bw, cr, preamble_symbols, sync_word, header: Implicit{len} | Explicit, crc_on, invert_iq, ldro }`
    (all control links here: sync word 0x12, implicit header, LoRa CRC off)
  - `Flrc { bitrate_bps, cr, bt_x10, sync_word: Option<u32>, payload_len }` (`None` = needs the phrase)
  - `Gfsk { bitrate_bps, fdev_hz, rx_bw_hz, preamble_bits, sync_word, sync_len_bits, read_len, radio_whitening, radio_crc }`
    (SiK/OpenLRSng: fixed-length read after sync 0x2DD4, whitening and CRC
    off in the radio; the crate de-whitens and checks in software)
  - `Energy { rx_bw_hz }` (RSSI only)
- `scan::plan(system, region)` and `scan::energy_plan(region, bin_hz)` are
  iterators of `ScanStep { config, dwell_ms, extend_ms }`.
  `scan::multi_sf_compatible(a, b)` says when two consecutive steps can be
  one LR2021 multi-SF receive (plans put them next to each other: ELRS 900
  200/100/50/25 Hz share one group).
- `recognise(bytes, &RxConfig, RxMeta, &Config) -> Option<Recognised>`;
  `Recognised { sighting: ControlSighting, content: Vec<ControlContent>, raw: Option<Vec<u8>> }`;
  `ControlSighting { system, region, probe, freq_hz, rssi_dbm, snr_db, t_us, confidence, direction, id: Option<LinkId>, version }`.
- `ControlContent::{Channels, Telemetry, Position, Opaque}`. `Position`
  is WGS-84 in 1e-7 degrees with altitude and its reference, speed,
  heading, satellites and fix type, and `kind` (aircraft or home point),
  ready for the tracker.
- Energy: `EnergyMap::<N>::for_region(region, bin_hz)`, `.add(freq_hz, rssi_dbm)`
  per sample, `.end_window()` every few seconds -> `EnergyReport { activity:
  Quiet | Steady | Background | Hopper(HopSet), steady, windows }`.
  `BurstTimer::<M>` takes fast RSSI reads on one frequency (`.add(rssi, t_us)`,
  `.summary()`), and `energy::classify(&hop_set, Some(&burst_summary))`
  returns scored candidates.
- ELRS follow mode: `elrs::Follower::from_uid(&cfg, uid, version, region, rate)`
  (UID from `elrs::uid_from_phrase("...")`) or `Follower::from_sync(&cfg,
  &sync_info, region, rate, t_us)`; then `next_rx(now_us) -> (t_us, freq_hz,
  RxConfig)`, `process(bytes, meta, channel) -> Followed { nonce,
  fhss_index, direction, is_sync, content }`, and `recover()` for the
  missing UID bytes.

## Per system

### ExpressLRS

Sources: github.com/ExpressLRS/ExpressLRS (GPLv3), master of 2026-11 plus
tags 3.6.3 and 4.1.0. `src/src/common.cpp` (air-rate tables for SX127x,
SX1280, LR1121, LR2021), `src/include/common.h` (rate enum),
`src/lib/OTA/OTA.{h,cpp}` (packet layout, CRC, channel packing),
`src/lib/FHSS/{FHSS.cpp,random.cpp}` (domains, sync channel, hop table),
`src/lib/StubbornReceiver/`, `src/src/{tx,rx}_main.cpp` (sync and telemetry
timing), `src/python/binary_configurator.py` (phrase -> UID).

- **Sync channel**: the middle of the domain's list: `count/2` in 4.x,
  `count/2 + 1` in 3.x. FCC915 (40 channels, 903.5-926.9 MHz, 600 kHz):
  915.5 MHz (4.x), 916.1 MHz (3.x). ISM2G4 (80 channels, 2400.4-2479.4):
  2440.4 / 2441.4 MHz. AU915, EU868 (start moved 865.275 -> 863.275 MHz in
  4.x), IN866, AU433, EU433, US433, US433W and TH920 (master only) are in
  the table too.
- **Air rates** (LoRa, all with sync word 0x12, implicit header, no LoRa
  CRC, 8-byte OTA4 or 13-byte OTA8 payloads): 900 MHz BW500: 25 Hz SF9,
  50 Hz SF8, 100 Hz SF7, 100 Hz Full SF6 CR4/8, 200 Hz SF6, 200 Hz Full SF5,
  250 Hz SF5 CR4/8, D50, and the LR1121/LR2021 dual-band halves. 2.4 GHz
  BW812 (long interleaving CRs): 50/150/250/500 Hz, 100/333 Hz Full, X150 /
  X100 Full. FLRC (F1000, F500, D500, D250) and GFSK (K1000) are listed but
  energy-only. On 2.4 GHz the IQ polarity is `UID[5] & 1`; plans try both.
  Below 1 GHz ELRS forces normal IQ.
- **SYNC packet** (4.x): `type=2 | fhssIndex | nonce | rfRateEnum |
  flags(switchMode, tlmRatio, gemini, otaProtocol, spare) | UID4 | UID5`.
  3.x: `fhssIndex | nonce | flags(switchMode, tlmRatio, rateIndex) | UID3 |
  UID4 | UID5`. CRC: 14-bit (poly 0x2E57) for OTA4, 16-bit (0x3D65) for
  OTA8, MSB first, initialiser `(UID4<<8 | UID5) ^ (4<<8)` (4.x) or `^ 3`
  (3.x); 4.x data packets also XOR the nonce in, SYNC packets do not.
- **Checks without the phrase**: CRC under the carried UID bytes (Model
  Match XORs the low 6 bits of the sent UID5, so up to 64 candidates; on
  2.4 GHz the IQ polarity halves that), hop index a multiple of the channel
  count, 4.x rate enum valid and spare bit clear, OTA8 spare bytes zero.
  Random-packet odds about 2^-18 (4.x OTA4). A 3.x OTA4 sync that only
  checks with Model Match is kept at Likely (fewer checks, odds ~2^-13).
- **Timing**: sync packets every 5 s on a connected link, every 0.6 s
  (900) when disconnected. Other packets land on the sync channel once per
  pass through the table (FCC915 at 250 Hz: 4 packets every 640 ms). Plan
  dwell is two passes (1-7 s), stretched to 5.5 s after any hit.
- **Follow mode**: the hop table is ELRS's LCG-shuffled list seeded from
  `UID2..UID5` (`OtaGetUidSeed`); the channel for nonce `n` is
  `table[f0 + n/H - n0/H]` from a sync's `(fhssIndex f0, nonce n0)`, `H` the
  hop interval. With the phrase, everything is known. From a sync alone, UID4
  and UID5 (and UID3 in 3.x) are known, which is enough to check and decode
  every packet on a channel the node parks on; the hop table also needs UID2
  (and UID3 in 4.x). The follower records `(hop index, channel)` for each
  packet it checks on other channels; `recover()` searches 128 (3.x) or
  32,768 (4.x) candidates. UID2's top bit is dropped by ELRS's random
  generator, so it never matters for the hop table (it does for the FLRC
  sync word). About 2 observations settle 3.x on FCC915, 4 settle 4.x.
  UID0 and UID1 are never needed.
- **Content**: RC: OTA4 Hybrid (switch index in the packet) and Wide
  (switch index from the nonce), OTA8 8/12/16 channels. Telemetry: the
  downlink's chunked stream (LINKSTATS + DATA packets, package index 1, 2, ...
  0) is reassembled into CRSF frames: GPS (0x02), battery (0x08), baro
  altitude (0x09), vario (0x07), link statistics (0x14), attitude (0x1E),
  flight mode (0x21); in MAVLink link mode the frames carry MAVLink.

### mLRS

Source: github.com/olliw42/mLRS (GPLv3): `mLRS/Common/{frame_types.h,
frames.h, fhss.h, setup.h, sx-drivers/*}`.

91-byte frames both ways, LoRa SF5/SF6 BW500 CR4/5 (900) or SF5/6/7 BW812
(2.4), implicit header, no LoRa CRC, normal IQ, default sync word. A frame
starts with a 2-byte frame sync word (CRC of the bind phrase plus offsets;
the opt-in ID) and ends with an X.25 CRC over the first 89 bytes, so a frame
is confirmed with nothing from the bind phrase. Uplink frames carry 16 (or
32) RC channels and serial bytes; downlink frames carry serial bytes,
usually MAVLink. Channel lists: FCC 902.4 MHz + 0.6 MHz x 43, EU868 863.275
+ 0.525 x 10, 2.4 GHz 2401-2480 MHz. mLRS hops over all channels, so a
parked node hears roughly 1 frame in 40. FLRC 111 Hz and FSK 50 Hz modes
use the bind phrase as the radio sync word: energy only. Privacy (2+):
uplink header CRC left empty -> reported encrypted; downlink encryption is
not visible from outside -> "unparsed" if no MAVLink is found. "mavlinkX"
compressed link mode is not decoded.

### SiK / RFD900

Source: github.com/ArduPilot/SiK (BSD-2-Clause): `Firmware/radio/
{radio_443x.c, tdm.c, freq_hopping.c, main.c, parameters.c}`.

Si443x GFSK, sync 0x2D 0xD4, header = NET ID (default 25; the opt-in ID),
length byte, data, hardware CRC-16 (0x8005), PN9 whitening on. Default 64
kbit/s; 915 MHz boards hop over 50 channels 250 kHz apart in 915-928 MHz,
offset by a NET-ID-derived amount. The data is serial bytes (MAVLink) plus
a 2-byte TDM trailer. Not verified without hardware: PN9 bit order (taken
as TI CC1101's, same polynomial and seed), CRC seed (both 0 and 0xFFFF
accepted), and the NET ID frequency offset (SDCC's 16-bit `int` overflows in
`r_rand()*625`; implemented as wrapping). ECC (Golay) mode is energy-only.
AES-encrypted links are detected; their payload shows as "unparsed".

### OpenLRSng

Source: github.com/openLRSng/openLRSng (GPLv3): `openLRSng/{binding.h,
common.h, RFM.h, TX.h}`.

RFM22B GFSK (4800-125000 bit/s; default 19200), no whitening, sync 0x2DD4,
4-byte header = `rf_magic` with the last byte XORed with the hop index,
length, packet, CCITT CRC. Packet size names the channel layout (7-21 bytes
uplink, 9 bytes telemetry), so channels decode without the bind data.
Defaults: 435 MHz base, 50 kHz spacing, hops 22,10,19,34,49,41, magic
0xDEADFEED, but binding randomises the magic and usually the hops.

### Closed systems (energy and timing only)

| System | Band | What is known | Source quality |
|---|---|---|---|
| TBS Crossfire | 868 / 915 | 150 Hz and 50 Hz packet rates | vendor docs; channel plan not public |
| FrSky R9 / ACCESS 900 | 868 / 915 | rates assumed ~9/18 ms | estimate |
| TBS Tracer | 2.4 | rate assumed 250 Hz | estimate |
| ImmersionRC Ghost | 2.4 | rates assumed 250/500 Hz | estimate |
| FrSky D16 / ACCESS 2.4 | 2.4 | 47 hop channels, 9 ms frames | public reverse engineering |
| FlySky AFHDS 2A | 2.4 | 16 hop channels, 3.85 ms frames | public reverse engineering |

Distinguishable by energy: ELRS 2.4 FLRC/FSK (1-2 ms, sub-ms bursts, 1 MHz
grid), ELRS 900 vs mLRS 900 (burst length), SiK (250 kHz grid, irregular
bursts), AFHDS 2A and FrSky D16 by their periods on a dwell. Not
distinguishable: Crossfire vs ELRS 900 LoRa vs R9 at similar rates; Ghost
vs Tracer vs ELRS 2.4 LoRa. These come back as a candidate list.

## Not verifiable without hardware

- Everything was tested against packets built by test-only encoders written
  from each project's source; none against a real transmitter yet.
- LR2021 specifics: that its LoRa sync word 0x12 matches SX127x/SX1280
  "private" links, that the long-interleaving CRs and BW812 match the
  SX1280, multi-SF receive with these settings, GFSK fixed-length reads
  after a 16-bit sync with whitening off.
- SiK: PN9 bit order, CRC seed, NET ID offset (above).
- Follow-mode timing on real clocks (re-anchoring on each checked packet
  is meant to absorb crystal drift).
- Energy thresholds (10 dB hit margin, 8 dB one-emitter spread, background
  time constants) were tuned on synthetic data only; they need a session
  with real meters in the band.
