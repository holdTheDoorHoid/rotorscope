# Credits and licences

**Level: 🟢 Beginner** · Rotorscope's licence in plain words, the other people's code inside the
repository, the projects and papers the work builds on, and the rule for reusing anyone's code.

## Rotorscope's licence

Rotorscope is licensed under the **GNU General Public License, version 3 or later**
(GPL-3.0-or-later), chosen on 2026-11-09. The full text is in
[LICENSE](https://github.com/holdTheDoorHoid/rotorscope/blob/main/LICENSE).

In plain words, as the README puts it: anyone may use, change and sell Rotorscope, including as
kits, but whoever ships a changed version must publish their changes under the same licence.

> [!NOTE]
> This page describes licences; it is not legal advice. Where a licence matters to you, read its
> full text.

## Other people's code in the repository

| What | Where | Licence | Used for |
|---|---|---|---|
| **opendroneid-core-c**, the official Open Drone ID library, including its GB 46750 (China) variant | [`third_party/opendroneid-core-c`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/third_party/opendroneid-core-c) | Apache-2.0 (kept with its own licence file) | Host-side tests only: `crates/odid-ref` compares Rotorscope's Remote ID decoder with it. It is never built into firmware. |
| **esp-radio 1.0.0-beta.1**, Espressif's Rust radio driver, with one file changed | [`firmware/patches/esp-radio`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/patches/esp-radio) | MIT OR Apache-2.0 | `rid-beacon` only. The change fixes the C5's shared radio front-end clocks so Bluetooth transmits while Wi-Fi runs. To be removed when upstream carries a fix ([README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/patches/README.md)). |
| **ESP32-P4 pre-v3 patch** for esp-hal: the community kit naoNao89/esp32p4-pre-v3-kit 0.1.3 | [`firmware/patches/esp-hal-p4-pre-v3`](https://github.com/holdTheDoorHoid/rotorscope/tree/main/firmware/patches/esp-hal-p4-pre-v3) | MIT/Apache-2.0; the ROM linker tables inside it come from ESP-IDF under Apache-2.0 | Lets the hub run on the P4 board's early chip revision (v1.3), which upstream esp-hal does not support. Exact origins and checksums are in [PROVENANCE.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/patches/esp-hal-p4-pre-v3/PROVENANCE.md). `tools/p4-prepare.sh` checks the patch's SHA-256, then applies it and Rotorscope's own memory-map fix to a pinned esp-hal checkout, which is not committed. |

### Code ported into Rotorscope's own crates

These parts were translated into Rust from other open-source projects, keeping the credit in the
source files. Each licence is compatible with GPL-3.0-or-later.

| From | Licence | Ported into | What |
|---|---|---|---|
| [ExpressLRS](https://github.com/ExpressLRS/ExpressLRS) | GPL-3.0 | `crates/ctrlwatch` (`elrs.rs`) | Air-rate tables, packet layouts, sync channel and hop sequence |
| [SoftRF](https://github.com/lyusupov/SoftRF) by Linar Yusupov (legacy decoder by Stanislaw Pusep) | GPL-3.0-or-later | `crates/flarm`, `crates/lr2021cfg` | FLARM legacy and OGN decoding, radio settings for the aero roles |
| OGN tracker library by Pawel Jalocha et al., as carried in SoftRF (cited as [pjalocha/ogn-tracker](https://github.com/pjalocha/ogn-tracker) in `crates/aerohop` and as [glidernet/ogn-tracker](https://github.com/glidernet/ogn-tracker) in [Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md#sources)) | GPL-3.0 | `crates/flarm`, `crates/aerohop` | OGN packet layout, the error-correction tables, the FLARM/OGN frequency plan |
| dump978 by Oliver Jowett, as carried in SoftRF | GPL-2.0-or-later | `crates/uat` | UAT field layout and frame types |
| [lr2021](https://github.com/TheClams/lr2021) crate by TheClams | MIT | `crates/lr2021cfg` | LR2021 command opcodes and field layouts (receive commands only) |
| ESP-IDF v5.4.2 `esp_lcd_panel_rgb.c` (Espressif) | Apache-2.0 | `firmware/hub-p4/src/rgb.rs` | A register-level port of the RGB panel driver |

The control-link recognisers also follow the published frame formats of
[mLRS](https://github.com/olliw42/mLRS) (GPL-3.0), [SiK](https://github.com/ArduPilot/SiK)
(BSD-2-Clause) and [OpenLRSng](https://github.com/openLRSng/openLRSng) (GPL-3.0), as their module
headers record. FLARM keys are never in the repository: see
"Downloaded on demand" below.

## Projects and papers the work builds on

"Facts only" means the project was read for how a signal or chip works, and Rotorscope's code was
written fresh from those facts. Nothing is copied from these.

### The C5 raw-radio work (facts only)

- [Twotoz/C5VRX](https://github.com/Twotoz/C5VRX) ([project page](https://twotoz.github.io/C5VRX/))
  by Twotoz and the C5VRX contributors (GPL-3.0-only): the receive-only set-up, the radio call order, the debug-bus lanes and the
  XIAO pad set. C5VRX asks for credit where its findings are reused; this, the README and the
  [node-sdr README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/node-sdr/README.md)
  are that credit. Because nothing was copied, no file in Rotorscope is GPL-3.0-only.
- [cyber-murmel/esp-sdr-zephyr](https://github.com/cyber-murmel/esp-sdr-zephyr)
  (GPL-3.0-or-later): the C5 sample-capture registers and the capture-ring set-up.
- [ESPARGOS/esp-sdr](https://github.com/ESPARGOS/esp-sdr) (GPL-3.0-or-later): the sample word
  layout and the C5 filter table.
- [CapnBry/widscan58](https://github.com/CapnBry/widscan58): the idea of checking the C5 radio
  library's version before using its private calls (its `check_phy_abi.sh`).

### DJI DroneID and DJI links (facts only)

`crates/droneid` uses these for protocol facts only (layouts, constants, the published scrambler
example); it contains no code from any of them.

- [RUB-SysSec/DroneSecurity](https://github.com/RUB-SysSec/DroneSecurity) and the paper by
  N. Schiller et al., "Drone Security and the Mysterious Case of DJI's DroneID", NDSS 2023
  ([PDF](https://www.ndss-symposium.org/wp-content/uploads/2023-217-paper.pdf)): the DroneID
  receiver chain, and the two public recordings Rotorscope tests against.
- [proto17/dji_droneid](https://github.com/proto17/dji_droneid) (MIT) and
  [anarkiwi/samples2djidroneid](https://github.com/anarkiwi/samples2djidroneid) (Apache-2.0):
  decoding facts, and a decoder to cross-check against.
- [EdwardBlair/dji-droneid-spec](https://github.com/EdwardBlair/dji-droneid-spec): the O4 record
  and radio description, and its published test vectors. It has no licence, so none of its text
  or code is copied.
- [luyii-code-1/dji-ocusync-droneid-research](https://github.com/luyii-code-1/dji-ocusync-droneid-research)
  (GPL-3.0): model behaviour and frequency reports.
- The reverse-engineering notes, FCC filings, papers and DJI specification pages behind the DJI
  link fingerprints are listed one by one in [DJI radio links in depth](../DJI-LINKS.md#sources).

### Remote ID

- [opendroneid](https://github.com/opendroneid/opendroneid-core-c): the reference decoder and its
  test vectors, Rotorscope's ground truth.
- Receivers surveyed while planning: [orecchino-esp32](https://github.com/isaacbentley/orecchino-esp32),
  [DRONK](https://github.com/probablytheguy/DRONK) (which documented the C5's frame-length quirk and
  heard a DJI Avata on 5 GHz channel 149), [drone-mesh-mapper](https://github.com/colonelpanichacks/drone-mesh-mapper),
  [RID_Scanner](https://github.com/andylee77/RID_Scanner), [friendorfoe](https://github.com/lnxgod/friendorfoe),
  [pocket-rid](https://github.com/siercks/pocket-rid) and [ArduRemoteID](https://github.com/ArduPilot/ArduRemoteID).
  Their findings are summarised in
  [RESEARCH.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/RESEARCH.md).
  ArduRemoteID (GPL-2.0) and RID_Scanner (no licence) may not be copied, and were not.

### Bluetooth, aircraft and outputs

- [Sniffle](https://github.com/nccgroup/Sniffle) by NCC Group (GPL-3.0): the Bluetooth 5 sniffer
  firmware Rotorscope loads onto a Sonoff dongle. You download it yourself; it is not in the
  repository. Rotorscope's own `tools/cc26xx-flash` writes it. See [A Sonoff dongle](../SONOFF.md).
- ADSBee by Pants for Birds (GPL-3.0): the receiver whose CSBee output `crates/adsb` reads. Its
  repository is cited as [PantsForBirds/adsbee](https://github.com/PantsForBirds/adsbee) in
  `crates/adsb` and RESEARCH.md, and as
  [CoolNamesAllTaken/adsbee](https://github.com/CoolNamesAllTaken/adsbee) in
  [Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md#sources).
- *The 1090 Megahertz Riddle* by Junzi Sun (TU Delft OPEN, 2021, [mode-s.org](https://mode-s.org/1090mhz/))
  and the pyModeS test suite: the Mode S formats and the example frames in `crates/modes`' tests.
  No code is copied from either.
- [DragonSync](https://github.com/alphafox02/DragonSync) (Apache-2.0): only its field names and
  message shapes are used, so `rid-view` can feed it. None of its code is copied.

### Attacks, spoofing, jamming and GPS

The deep dives cite every tool, paper and standard they draw on:
[Spoofed drones](../SPOOFING.md#sources), [Attacks on drones](../DRONE-ATTACKS.md),
[Jamming](../JAMMING.md#sources), [GPS jamming and spoofing at the hub](../GNSS-INTEGRITY.md#sources)
and [Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md#sources).

### FAA flags

The visual-line-of-sight defaults cite Li, Jia, Peng and Gan, "Line-of-sight in operating a small
unmanned aerial vehicle", *Applied Ergonomics* 81, and a study in *Applied Sciences* 10(16):5501
(2020). Both were seen only as abstracts or search summaries, as `crates/faarules` records.

## Downloaded on demand, never committed

| What | How it arrives | Licence |
|---|---|---|
| Two DJI DroneID recordings from RUB-SysSec/DroneSecurity | `tools/fetch-droneid-samples.sh`, pinned to one commit and checked by SHA-256, into the ignored `testdata/` folder | AGPL-3.0 |
| RFUAV drone recordings (Hugging Face `kitofrank/RFUAV`) | Downloaded one at a time for `tools/rfuav-check`; only derived numbers are kept, locally | [DJI-LINKS.md](../DJI-LINKS.md) records the dataset as Apache-2.0; RESEARCH.md records the code as Apache-2.0 and the data's licence as unstated, so the recordings are used for local checks only |
| Airspace, terrain and urban-area data | `tools/airspace-pack` downloads FAA ADDS, USGS 3DEP, NOAA NGS GEOID18 and US Census TIGERweb data into a pack for the SD card | US government public data; the plan records the FAA and USGS data as public domain |
| Sniffle firmware for the Sonoff dongle | You download the release file yourself | GPL-3.0 |
| FLARM keys | `tools/flarm-keys` extracts them from your own SoftRF checkout into a file git ignores; the hub reads them from the SD card | The repository is public, so the keys must never be published |
| DJI O4 key | Supplied by the owner on the SD card | No public key exists; it never lives in the repository |
| Mermaid, the diagram library for this website | `tools/docs-build.sh`, pinned to version 11.4.1 and checked by SHA-256 | — |

## The rule for reusing other people's code

The repository states it in [PLAN.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/PLAN.md)
("Risks to watch", item 4) and RESEARCH.md, and the source files follow it:

- Because Rotorscope is GPL-3.0-or-later, code from GPL-3.0 projects (such as C5VRX and the
  ADSBee firmware) may be ported or reused directly, **keeping their copyright notices**.
- Third-party code must be GPL-3.0-compatible: Apache-2.0, MIT and BSD are fine;
  **GPL-2.0-only is not**.
- Code with **no licence** may not be copied. It can be read for facts, and Rotorscope's code is
  then written fresh, as with the O4 specification.
- Where a project is read for facts only, the source file says so and credits it.

Next: [Security policy](security.md)
