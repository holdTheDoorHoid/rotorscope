# Aircraft signals beyond 1090 MHz: UAT, FLARM and its open relatives

Research for PLAN.md Phase 14b, 14c and 14d (2026-11-15). Plain-language summary first, then
the detail and sources. 14a (our own 1090 MHz decoder) is built; so, since 2026-11-17, are the
UAT, OGN and FLARM decoders (see [Built](#built-2026-11-17)), not yet tried on real radio frames.

## Summary

- **UAT (978 MHz)** is the US-only ADS-B system used by most small private aircraft below
  18,000 ft. It matters more for Rotorscope's purpose (low aircraft near people) than anything
  else on this page. Its message format is fully published, our 14a design extends to it
  (same aircraft table, same output record), and the error correction is a standard
  Reed-Solomon code. **It needs a 978 MHz receiver**: the ADSBee 1090U (the owner's unit is a
  1090 without the 978 radio), or a cheap RTL-SDR 978 dongle on a laptop.
- **FLARM** (gliders, some helicopters and light aircraft) is common in Europe, rare in the
  US. In the US it hops across 902-928 MHz. In May 2026 FLARM published its radio protocol
  (the "FLARM FAMP Public Protocol") with a free licence for **non-commercial, receive-only**
  use; commercial use needs a paid licence. **The owner reports having permission to use the
  FLARM legacy keys (2026-11-17)**, so Rotorscope now decodes FLARM legacy packets (receive
  only, cargo feature `legacy`, off by default). The keys are **not** in the repository and
  never will be: they are loaded from a file at run time (see below). **Never transmit FLARM.**
- **OGN tracker protocol, FANET, ADS-L** are open, but almost unused in the US. ADS-L is
  European by design (868 MHz only). All three can be received by an LR2021 or an SX1262.
- **Recommendation:** build **UAT first** (biggest US payoff, open format, cheap hardware).
  Then, if the owner wants gliders and paragliders, an SX1262 or LR2021 role that hears
  OGN/FANET content and FLARM presence on 902-928 MHz. RTL-SDR on the P4 (14c) is not worth
  it now.

## 14b. UAT on 978 MHz

**What it is.** The Universal Access Transceiver, defined by RTCA DO-282B and ICAO Annex 10
(the ACP working-group "UAT SARPs" appendix is a free summary [1]). FAA rules let aircraft
that stay below 18,000 ft (Class A) meet the 2020 ADS-B mandate with UAT instead of 1090 ES,
so most US general aviation light aircraft and many helicopters carry UAT, not 1090 [2].

**Radio.** One channel, 978 MHz, binary continuous-phase FSK at 1.041667 Mbit/s [1]. Each
aircraft sends one ADS-B message per second in its own time slot (the second is divided into
a ground-uplink part and an aircraft part).

**Messages** (DO-282B, as implemented by the open-source `dump978` decoder):
- 36-bit synchronisation word (different for aircraft and ground uplink messages);
- **Basic ADS-B**: 18 bytes of data + 12 bytes Reed-Solomon parity = 30 bytes (RS(30,18) over
  GF(256)), corrects up to 6 bad bytes;
- **Long ADS-B**: 34 bytes + 14 parity = 48 bytes (RS(48,34)), corrects up to 7 bad bytes;
- **Ground uplink** (FIS-B weather, TIS-B traffic): 6 interleaved RS(92,72) blocks, 432 data bytes.
- Payload: header with a 24-bit address and a 3-bit address qualifier (ICAO, self-assigned,
  TIS-B, surface vehicle...), then a **state vector with plain latitude/longitude** (24 bits
  each, no CPR pairing needed), altitude (25 ft), velocities, then mode-status and auxiliary
  fields (callsign, emitter category, emergency, integrity) depending on the payload type.

**Does our 14a design extend?** Yes, cleanly:
- The table (`crates/modes::table`) keys aircraft by address and already emits the
  `adsb::Aircraft` record, which has a `Source::Uat` and an address-qualifier field (the
  ADSBee's CSBee `#U:` lines use it). A `crates/uat` crate would decode a 30- or 48-byte
  message into the same per-aircraft updates; positions are simpler than 1090 (no CPR).
- New code: a Reed-Solomon decoder (Berlekamp-Massey + Chien + Forney over GF(256), a few
  hundred lines, well documented) and the payload field tables. Roughly 3-5 days with tests.
- The ADSBee 1090U reports UAT frames already FEC-corrected in its RAW output
  (`#UAT*-<hex>;(dBm,bits corrected,counter)`) and in Beast (its own UAT frame type). So, as
  with 14a, we could decode its raw UAT frames on the hub without our own Reed-Solomon at
  first, and add RS only for an SDR source (14c).

**Hardware.**
- **ADSBee 1090U**: the 1090 receiver plus a TI CC1312 sub-GHz radio for 978 MHz and an
  ESP32 [3][4]. Same UART/Beast/CSBee interfaces as the owner's 1090, so the P4 wiring and
  `crates/modes` input code are reused. Price was not posted where we could find it
  (pantsforbirds.com lists it; check there). The `m1090` module can be populated with the
  CC1312 on a carrier board [3].
- **Laptop alternative**: a FlightAware 978 MHz USB dongle or any RTL-SDR with `dump978`
  (roughly $20-40). Good for collecting test recordings before buying a 1090U.

## 14d. FLARM

**What it is.** A collision-warning system for gliders, light aircraft and helicopters.
Mandatory or near-universal on European gliders; in the US it is voluntary, used by soaring
clubs and contest pilots (e.g. a Texas club with PowerFLARM in all 10 gliders and its tow
planes) [5]. FLARM boxes in the US do not transmit 1090 ES unless the aircraft also has a
transponder, so a glider with only FLARM is invisible to our ADS-B decoder.

**Radio.**
- Bands: 868.2/868.4 MHz in Europe; **902-928 MHz in North America** [6].
- US channel plan as implemented by the open-source SoftRF project (a FLARM-compatible
  community device): **65 channels from 902.2 MHz, 400 kHz apart**, with a pseudo-random hop
  every slot derived from the time; OGN trackers in the US hop on neighbouring channels [7].
  (Europe: 2 fixed channels, 200 kHz apart.) This is SoftRF's reading, not FLARM's document.
- Modulation: 2-GFSK, 100 kchip/s, Manchester-coded (50 kbit/s data), ±50 kHz deviation. EASA's
  ADS-L "M-band" specification adopts exactly these parameters to stay compatible with
  existing FLARM-family hardware [8], and community reverse engineering agrees [9].

**Protocol, obfuscation, encryption.** FLARM's air protocol was proprietary and scrambled
(versions 4 in 2008 and 6 in 2015 were reverse-engineered despite this) [10]. FLARM's EULA
says the radio protocol and messages are protected by copyright, trademark and patent, and
cites the Cybercrime Convention [11]. Third-party apps that show FLARM traffic (e.g.
SkyDemon) do it **under licence** [12]. **New in 2026:** FLARM published the **FLARM FAMP
Public Protocol (FFPP)**, the specification of its current traffic messages, v1.0 on
2026-05-20 (v1.01 2026-05-29), with a "worldwide, perpetual, royalty-free" licence for
**non-commercial** use (academic, research, amateur, private), **receive-only**; transmitting,
commercial and military use are excluded and need a licence from FLARM [13][14]. It uses
randomised sender addresses for privacy and negotiates message versions between devices.
We could not download the PDF (the link returned 404 on 2026-11-15), so we have not checked
whether it covers the 902-928 MHz hop sequence and whether Annex 1 limits publishing decoders.

**FLARM and OGN.** The Open Glider Network (volunteer ground stations feeding a public map)
decodes FLARM. FLARM says it objected on "legal and IP aspects" in 2014/2015, then worked with
OGN and kept "OGN's decoding staying interoperable through all major updates" [15]; OGN honours
FLARM's no-tracking flag. That is an arrangement with one non-commercial project, not a general
permission.

**Decision for Rotorscope (owner, 2026-11-17):** the owner reports having permission to use
the FLARM legacy keys, and asked for FLARM legacy decoding. It is built (`crates/flarm`,
feature `legacy`), with these rules:
1. **Receive only. Never transmit FLARM.** The crate has no transmit path; the encoders in its
   tests exist only to make test vectors and are never in a firmware build.
2. **The keys are never published.** The permission covers using the keys, not publishing
   them, and the repository is public. No key constant is in the source; they are read at run
   time from `ROTORSCP/FLARMKEY.TXT` on the hub's SD card (format below), and are never
   committed, printed, logged, or sent between boards (the same rule as the O4 key).
3. **Off by default.** The hub opts in with the `legacy` feature and a key file; without the
   file, FLARM stays presence-only as before.
4. If Rotorscope units are ever sold, re-check the commercial terms (FLARM's FFPP licence and
   the key permission) before shipping FLARM decoding in them.

## Built (2026-11-17)

Three decoders now turn raw radio frames into the same aircraft record the ADSBee and our
1090 MHz decoder produce, so these aircraft appear in the existing aircraft table, on the
screen, in the SD log, USB JSON, the phone page and CoT, labelled **UAT**, **OGN** or
**FLARM**. They are host-tested only: **no real radio frame has been through them yet**; the
LR2021 radio node that will deliver the frames is being written separately.

### `crates/uat`: UAT on 978 MHz

**In plain words:** takes one 978 MHz ADS-B message as the radio heard it, repairs up to 6
damaged bytes (basic message) or 7 (long message) using the message's built-in error-correcting
code, throws it away if it is worse than that, and reads out who the aircraft is, where, how
high, how fast, which way, climbing or descending, its call sign or squawk, its type and any
emergency.

- Input: the bytes after the 36-bit sync word, parity included. Capture 48 bytes every time:
  the decoder tries a long frame, then a basic one (as dump978).
- Reed-Solomon RS(30,18) and RS(48,34) over GF(256) (polynomial 0x187, first root alpha^120),
  written fresh. Stricter than dump978's library: an error "found" in the unused (shortened)
  part of the code rejects the frame. A rejected frame is left untouched.
- Payload: header (24-bit address, address qualifier), state vector (latitude/longitude,
  barometric or geometric altitude, north/east velocity or ground speed and track, vertical
  rate, TIS-B site), mode status (call sign, or squawk when the CSID bit says so; emitter
  category; emergency; UAT version), auxiliary state vector (the second altitude).
- Ground uplink (FIS-B weather) is not decoded.
- Hub API:
  ```rust
  uat::decode(frame: &mut [u8]) -> Result<uat::Decoded, uat::Error>  // >= 30 bytes, 48 ideal
  uat::Decoded::aircraft(&self) -> adsb::Aircraft                      // Source::Uat
  uat::correct_adsb_frame(frame: &mut [u8]) -> Result<(FrameKind, u8), Error>
  ```
- 22 host tests (frames built by a test-only encoder; error correction at every count up to
  the limit, rejection beyond it, noise rejection, every field).

### `crates/flarm`, module `ogn`: OGN trackers

**In plain words:** OGN trackers are open-source glider/paraglider beacons. This checks the
packet's 48 parity checks (any failure: thrown away), undoes the published "whitening"
scramble, and reads position, GPS and pressure altitude, climb, speed, heading, aircraft type
and address type. Encrypted packets (only their owner can read them) and status packets are
skipped, as SoftRF does.

- Input: the 26 bytes after Manchester decoding (20 data + 6 LDPC parity), as SoftRF's driver
  delivers them.
- Hub API: `flarm::ogn::decode(packet: &[u8]) -> Result<flarm::Decoded, flarm::ogn::Error>`;
  `Decoded.aircraft` is the `adsb::Aircraft` (`Source::Ogn`), with the address type, aircraft
  type, stealth flag and relay count beside it.
- OGN has no air/ground bit; "airborne" is judged by speed against FLARM's per-type thresholds.
  OGN altitude is GPS height above mean sea level (about 30 m different from the ellipsoid
  height the record nominally holds; harmless for "how high").
- 13 host tests (packets built by a test-only encoder using the OGN code's generator table,
  checked against its separate parity-check table).

### `crates/flarm`, module `legacy` (feature `legacy`): FLARM

**In plain words:** FLARM boxes scramble their packets with keys that change with the time and
the aircraft's address, and send only the low digits of their position. With the key file, the
hub's GPS time and its own position, this unscrambles FLARM version 6 and 7 position packets
(as the open-source SoftRF does) and reads address, position, height, speed, direction, climb,
aircraft type, and the "don't track me" and "stealth" flags.

- Input: the 24 payload bytes after Manchester decoding, or 26 with the CRC (then checked).
- Needs the receiver's UTC time and position (`flarm::legacy::Receiver`). A wrong time gives
  a wrong key: v6 then usually fails its parity bit; v7 has no check of its own, so do not feed
  v7 packets in without a GNSS time.
- Aircraft that set **no-track** should be shown on the unit but not logged or relayed off it
  (OGN honours the same flag). `Decoded.no_track` carries it; the hub decides.
- Hub API:
  ```rust
  let keys = flarm::Keys::parse(&text)?;              // text of ROTORSCP/FLARMKEY.TXT
  flarm::legacy::decode(packet: &[u8], keys: &flarm::Keys, rx: &flarm::legacy::Receiver)
      -> Result<flarm::Decoded, flarm::legacy::Error>  // Source::Flarm
  ```
- 14 host tests with **made-up keys** (algorithm round trips through a test-only port of
  SoftRF's encoder), plus one real-key test that runs only when `FLARM_KEYS_FILE` names a key
  file (it passed locally on 2026-11-17 with keys extracted from a SoftRF checkout).

### FLARMKEY.TXT (the key file)

```text
# FLARM legacy keys. Never commit, print, log or relay this file.
key1 = w0 w1 w2 w3 w4 w5 w6 w7    # 8 words
key2 = w
key3 = w
key4 = w
key5 = w0 w1 w2 w3                # 4 words
```

Each `w` is a 32-bit word in hex (optional `0x`), separated by spaces or commas; `#` starts a
comment; every name exactly once. The names are SoftRF's `LEGACY_KEY1`..`LEGACY_KEY5`.

- Make it with `cargo run -p flarm-keys -- <SoftRF checkout>`: writes `tools/flarm.keys`
  (git-ignored, readable only by you), never prints a value, and refuses to write anywhere git
  would not ignore. Copy it to the hub's SD card as `ROTORSCP/FLARMKEY.TXT`.
- **Rules:** the keys are never committed (`tools/flarm.keys`, `FLARMKEY.TXT` and
  `*.flarmkeys` are in `.gitignore`), never printed or logged (`flarm::Keys` prints as
  `Keys(<redacted>)`, parse errors name only line numbers), and never sent between boards.

### Not ported from SoftRF, and why

- OGN **encrypted** packets: only the owner's key can open them; SoftRF skips them too.
- OGN **status/meteo** packets and the OGN relay logic: no aircraft position in them.
- OGN **soft-decision LDPC repair**: SoftRF accepts only packets with zero failed checks; so
  do we.
- SoftRF's v7 **timestamp check** is disabled in SoftRF (`#if 0`); we follow it. FLARM v6/v7
  **interleaving** and **time slots** are transmit-side; nothing transmit-side is ported.
- **ADS-L, FANET, P3I** protocols: not asked for here; ADS-L would reuse `modes::crc`.
- dump978's **uplink** (FIS-B) decoding, and Phil Karn's LGPL Reed-Solomon library (ours is
  written fresh).

### Credit

Ported under Rotorscope's GPL-3.0-or-later, with thanks:
- **dump978** by Oliver Jowett (GPL-2.0-or-later), as carried in SoftRF: UAT field layout and
  frame-type logic.
- **SoftRF** by Linar Yusupov (GPL-3.0-or-later), <https://github.com/lyusupov/SoftRF>:
  `OGNTP.cpp`, `Legacy.cpp` (legacy decoder by Stanislaw Pusep).
- **OGN tracker** library by Pawel Jalocha (GPL-3.0), as carried in SoftRF: packet layout,
  variable-resolution codes, whitening and the LDPC tables.

## Open relatives

| | OGN tracker protocol | FANET | ADS-L (EASA) |
|---|---|---|---|
| Who uses it | OGN trackers (gliders, tow planes, some paragliders) | Paragliders, hang gliders (Skytraxx and others), ground weather stations | New EU e-conspicuity standard for light aircraft and drones (U-space) |
| Spec | Open (OGN project; SoftRF and `ogn-tracker` source) [16] | Open text spec in the FANET repository [17] | Open EASA document "ADS-L 4 SRD-860", Issue 4 [8] |
| Band (EU) | 868.2/868.4 MHz | 868.2 MHz | 868.2/868.4 MHz (M-band), 869.525 MHz (O-band) |
| Band (US) | 902-928 MHz, hopping alongside FLARM [7] | **920.8 MHz, 500 kHz bandwidth** [17] | None: defined for the European SRD-860 band only [8] |
| Modulation | 2-GFSK 100 kchip/s Manchester (FLARM-like) | LoRa SF7, CR 4/5-4/8, 250 kHz EU / 500 kHz US, sync word 0xF1 | M-band: 2-GFSK 100 kchip/s, Manchester, ±50 kHz, BT 0.5. O-band: 2-GFSK 38.4 kchip/s, ±10 kHz |
| Integrity / secrecy | Format public through its open-source firmware | No encryption; optional signature (truncated SHA-1 with a shared key) | CRC-24 (**the same polynomial as Mode S**), XXTEA "scrambling" with a published algorithm, all-zero key allowed; optional Ed25519 signature |
| US use | A few dozen OGN ground stations (forum figure: 35 in the US vs 1,450 in Europe, plan for 150) [18] | Very little | None |

**Can our radios receive them?**
- **SX1262**: GFSK up to 300 kbit/s with programmable deviation, bandwidth and sync word, so
  FLARM-family 100 kchip/s Manchester GFSK (decode Manchester in software) and LoRa SF7 at
  500 kHz (FANET US) are both within its range. One channel at a time; for FLARM/OGN in the US
  it would need to follow the hop sequence or park on one channel and hear 1/65 of the traffic.
- **LR2021** (Phase 9 node): GFSK and LoRa over 150-960 MHz, wider receive bandwidth (up to
  about 3 MHz) so several 400 kHz FLARM channels fit in one look for presence detection, and
  multi-SF LoRa. It can do everything the SX1262 can here.
- ADS-L's CRC-24 is literally Mode S's, so `crates/modes::crc` can be reused.

## 14c. RTL-SDR on the P4 (feasibility note)

- An RTL2832U dongle at 2-2.4 MS/s sends 4-5 MB/s over USB 2.0 high speed bulk transfers. The
  P4 has a USB 2.0 high-speed OTG host, but our esp-hal fork has **no USB host driver** for it
  on the pre-v3 chip, and there is no Rust RTL2832U driver for bare metal. Writing both is the
  bulk of the work (weeks).
- CPU: a 1090 MHz preamble detector and demodulator at 2 MS/s is light work for a 360-400 MHz
  dual-core P4 (dump1090 runs on a Pi Zero), and UAT at 2.08 MS/s plus Reed-Solomon is similar.
- Value: it would also hear Mode A/C replies (no address) and give our own signal levels, and
  one dongle could do 978 *or* 1090, not both.
- **Verdict:** not worth it now. Do the demodulator on the laptop first if it is ever wanted
  (same Rust code feeding `crates/modes`), and buy an ADSBee 1090U for UAT on the hub.

## What to buy

1. **ADSBee 1090U** (UAT + 1090 in one unit, same interfaces), if the owner wants UAT on the
   hub. Price: check pantsforbirds.com.
2. Optional, cheap: an RTL-SDR or FlightAware 978 dongle (~$20-40) to record UAT on the laptop
   before the 1090U arrives.
3. For FLARM-family presence or OGN/FANET content: an SX1262 board (Heltec V3 class, ~$20), or
   the Phase 9 LR2021 node when it exists. No FLARM device is needed to start; OGN and FANET
   transmitters are rare in the US, so test sources would be our own SX1262 sending OGN/FANET
   frames on the bench.

## Sources

1. ICAO ACP WGW01, Appendix B, UAT SARPs (Annex 10 UAT technical provisions):
   <https://antena.fe.uni-lj.si/literatura/Razno/Avionika/uat/ACP-WGW01-Report%20on%20A.I.1-Appendix%20B%20-%20UAT%20SARPs.pdf>
2. 14 CFR 91.225 (ADS-B Out: 1090 ES required in Class A; UAT or 1090 ES elsewhere):
   <https://www.ecfr.gov/current/title-14/chapter-I/subchapter-F/part-91/subpart-C/section-91.225>
3. ADSBee firmware and hardware repository (1090U: RP2040, ESP32, CC1312):
   <https://github.com/CoolNamesAllTaken/adsbee>
4. RTL-SDR.com, "ADSBee: ADS-B and UAT reception and decoding on an RP2040":
   <https://www.rtl-sdr.com/adsbee-ads-b-and-uat-reception-and-decoding-on-an-rp2040-microcontroller/>
5. rec.aviation.soaring thread on PowerFLARM installation in US clubs (2022):
   <https://groups.google.com/g/rec.aviation.soaring/c/tjBvvMahlDE>
6. FLARM FTD-105 Horizon XC integration manual (regional bands, North America 902-928 MHz):
   <https://www.flarm.com/wp-content/uploads/2024/04/FTD-105-Horizon-XC-Integration-Manual-v1.06-1.pdf>
7. SoftRF source, `libraries/OGN/freqplan.h` (US: 902.2 MHz base, 400 kHz, 65 channels, hopping):
   <https://github.com/lyusupov/SoftRF>
8. EASA, Technical Specification for ADS-L transmissions using SRD-860 (ADS-L 4 SRD-860),
   ED Decision 2022/024/R, Issue 4: <https://www.easa.europa.eu/en/downloads/137503/en>
9. Osmocom-SDR mailing list, FLARM 868 MHz decoding thread (2013):
   <https://lists.osmocom.org/pipermail/osmocom-sdr/2013-December/001120.html>
10. Wikipedia, "FLARM" (protocol history, legal discussion; secondary):
    <https://en.wikipedia.org/wiki/FLARM>
11. FLARM end-user licence agreement: <https://flarm.com/eula>
12. SkyDemon, FLARM decoding (licensed, subscription): <https://www.skydemon.aero/store/flarmdecoding.aspx>
13. FLARM, "Publishing the FLARM FAMP Public Protocol":
    <https://www.flarm.com/en/blog/publishing-the-flarm-famp-public-protocol/>
14. FLARM FAMP Public Protocol page: <https://www.flarm.com/en/integration/flarm-famp-public-protocol/>
15. FLARM, "OGN network wins EASA GA safety award":
    <https://www.flarm.com/en/blog/ogn-network-wins-easa-ga-safety-award/>
16. Open Glider Network: <https://www.glidernet.org/>, tracker firmware
    <https://github.com/glidernet/ogn-tracker>
17. FANET protocol description (`protocol.txt`) in the FANET reference firmware:
    <https://github.com/3s1d/fanet-stm32>
18. rec.aviation.soaring thread on US OGN coverage (forum figures, may be out of date):
    <https://groups.google.com/g/rec.aviation.soaring/c/3ocP8GnTuyQ>
