# gnss

GPS driver core for the Rotorscope hub: a u-blox **MAX-M10S** (protocol 34.x, firmware
SPG 5.10) or a Quectel **LC29H(AA)** (dual-band L1+L5, NMEA + `$PAIR`/`$PQTM` commands),
detected automatically, plus a jamming/spoofing monitor for the receiver itself
(`integrity`, see `docs/GNSS-INTEGRITY.md`). `#![no_std]`, no alloc, no unsafe, no dependencies,
103 host tests.

**Status:** the u-blox path is host-tested; the LC29H path is host-tested and both were
loop-back tested on the P4 hub against an on-chip emulator (`firmware/hub-p4/PARITY.md`).
**Neither has been run against a real module yet.**

The crate does no I/O. The hub reads bytes from UART or I2C and calls `Parser::feed`; the
config builders write UBX frames into a buffer the hub then sends.

## Use

```rust
use gnss::{Parser, Event, configure_sequence};

let mut parser = Parser::new();

// Boot: send each step, wait for its ACK/NAK, then send the next.
for step in configure_sequence() {
    link.write(step.bytes());
    // keep feeding `parser` until an event answers it:
    //   step.confirmed_by(&ev) == Some(true)  -> ACK, go on
    //   step.confirmed_by(&ev) == Some(false) -> NAK, report and stop
    // The receiver answers within 1 s. For the last step (`step.baud_after`) see below.
}

// Run: whatever the transport gave you, in any chunking.
for ev in parser.feed(&bytes) {
    if let Event::Fix(fix) = ev {
        if fix.is_usable() { /* fix.lat, fix.lon, fix.alt_ellipsoid_m ... */ }
    }
}
```

**Always run the `feed` iterator to the end** (a `for` loop does). It reads `data` lazily,
because one input byte can release more than one event (see "Resync").

### API

| Item | What it is |
|---|---|
| `Parser::new()`, `feed(&[u8]) -> Events`, `push(u8)`, `reset()`, `stats()` | The streaming parser. About 450 B of state. |
| `Event::Fix(Fix)` | From UBX NAV-PVT, or from NMEA GGA + RMC (any talker). |
| `Event::Ack{class,id}` / `Nak{class,id}` | UBX-ACK-ACK / ACK-NAK. |
| `Event::MonVer(MonVer)` | Reply to a MON-VER poll: `sw_str()`, `hw_str()`, `protver`. |
| `Event::ValGet(ValGet)` | Reply to a CFG-VALGET poll (first 8 key/values; `get(key)`). |
| `Fix` | `lat`, `lon` (f64 deg), `alt_msl_m`, `alt_ellipsoid_m`, `h_acc_m`, `v_acc_m` (`Option<f32>`), `fix: FixType`, `fix_ok`, `position_valid`, `sats_used`, `speed_mps`, `heading_deg`, `pdop`, `hdop`, `utc: Option<UtcTime>`, `time_acc_ns`, `itow_ms`, `source`. |
| `Fix::is_usable()` | Receiver says the fix is OK **and** (3D or GNSS+DR, or 2D with `h_acc_m` < 50 m). |
| `Fix::position()` | `Some((lat, lon))` only when a position exists (otherwise `lat`/`lon` are 0.0). |
| `UtcTime::unix_ms()` | `utc` is `Some` only when the receiver says both date and time are valid. |
| `config::valset / valget / poll_mon_ver` | Frame builders: `(&mut [u8], ...) -> Result<usize, BuildError>`. |
| `configure_sequence()` / `ConfigSequence::with_options(ConfigOptions)` | Iterator of `ConfigFrame` (`bytes()`, `name`, `baud_after`, `confirmed_by(&Event)`). |
| `config::keys::*` | The configuration key IDs. |
| `ddc::*` | I2C address, registers, `bytes_available()`. |
| `Parser::sats()` -> `SatTable` | The last epoch's satellites one by one (UBX NAV-SAT or NMEA GSV): number, band, elevation, azimuth, C/N0, `plausible`. |
| `Event::NavStatus` / `NavClock` / `MonRf`, `Reply::Jamming` | u-blox spoofing detector, clock bias/drift, jamming state/AGC/CW; LC29H `$PAIRSPF`. |
| `ConfigSequence::with_integrity(true)` | Adds the optional `integrity-out` and `itfm` steps (u-blox). `Module::sequence` does this, and `PAIR391,1` on the LC29H. `Step::optional()`: a refusal is not a failure. |
| `integrity::Monitor` | `on_event(&ev, parser.sats(), now_ms)` per event; `status()` -> `state` (ok / suspect jamming / jamming / suspect spoofing / spoofing), `cause`, `metrics`, `hold()`, `write_reason()`. |

Fields the source cannot provide are `None`: from NMEA there is never `h_acc_m`, `v_acc_m`,
`pdop`, `time_acc_ns` or `itow_ms`, and `alt_*` appear only with a GGA that has an altitude.
From UBX, `hdop` is `None`. `alt_ellipsoid_m` from NMEA is GGA altitude + geoid separation.

### NMEA behaviour worth knowing

* GGA and RMC are paired by their time-of-day field and merged into one `Fix`, emitted when the
  second one arrives. If the partner never comes (RMC switched off, or a corrupted line), the
  lone sentence is released as a `Fix` when the next epoch's first sentence arrives, so it
  appears one second late.
* NMEA carries no 2D/3D flag. A GGA with an altitude is reported as `ThreeD`, one without as
  `TwoD`; RMC alone is `TwoD`. Without accuracy figures a NMEA 2D fix is never `is_usable()`.
* `utc` needs an RMC with status `A` (date plus a time we trust). GGA alone gives no UTC.
* GSA's fix mode (1 none / 2 2D / 3 3D) overrides the GGA-altitude guess: a receiver that says
  2D gives a `TwoD` fix even though its GGA prints an assumed altitude. GSA's PDOP fills `pdop`.
* GSV and GSA feed the sky view (`Parser::sky()`, below). `$PAIR` / `$PQTM` replies come out
  as `Event::Quectel`. Other proprietary `$P...` sentences and every other type are ignored.

### Resync

The parser keeps one buffer and re-examines it after every failure. A bad checksum, a false
`B5 62` in noise, a truncated frame or an oversize length field costs only that frame: the
parser drops one byte, skips to the next `0xB5` or `$`, and re-checks the rest of the buffer,
so a real frame hidden inside a false one is still found. The catch is latency, not loss: a
false header claiming a long frame is only rejected once that many bytes have arrived (at
most 400, so under 4 s at 1 Hz NAV-PVT on a quiet line). `Stats` counts checksum errors and
dropped bytes.

UBX frames with a payload over `MAX_UBX_PAYLOAD` (392) are treated as garbage. NAV-PVT is
92 bytes; a MON-VER is under 300.

## Quectel LC29H(AA)

Source for everything here: Quectel *LC29H Series & LC79H(AL) GNSS Protocol Specification*
V1.3 (2024-03-12), <https://forums.quectel.com/uploads/short-url/gJTnPOK8MkEJIgLFR7QgjvrpUey.pdf>;
*LC29H GNSS Specification* V1.2,
<https://quectel.com/content/uploads/sites/3/2022/06/Quectel_LC29H_GNSS_Specification_V1.2.pdf>;
*LC29H Series Hardware Design* V1.0,
<https://forums.quectel.com/uploads/short-url/6AJSfkCWZOBEg7psU3USNgfcBqm.pdf>; Waveshare's
LC29H HAT wiki, <https://www.waveshare.com/wiki/LC29H%28XX%29_GPS/RTK_HAT>.

| Fact | Value | Source |
|---|---|---|
| UART default | 115200 baud, all variants; adjustable 9600-921600 (`PAIR864`, needs a reboot) | GNSS Specification, Waveshare |
| Frame format | 8N1 assumed: not stated in any of the documents | (unverified) |
| Sentences on by default | GGA, GLL, GSA, GSV, RMC, VTG, at the 1 Hz default fix rate (AA max 10 Hz) | Protocol spec, `PAIR062` / `PAIR050` |
| NMEA version | 4.10 layout by default: GSA ends with a system ID, GSV with a signal ID | Protocol spec |
| Talkers | GP, GL, GA, GB, GN (combined, for GGA/RMC/GSA); GSV per constellation; on the AA, QZSS rides under GP (satellites 193-199, system ID 1) | Protocol spec |
| Dual band | On by default on the AA: GPS L1 = signal 1, L5 = 8; Galileo E1 = 7, E5a = 1; BeiDou B1I = 1, B2a = 5 | Protocol spec (GSV examples) |
| Commands | `$PAIRnnn,...*CS` answered by `$PAIR001,nnn,result` (0 ok, 1 processing, 2 failed, 3 unsupported, 4 bad parameter, 5 busy); `$PQTM...` answered by `$PQTM<name>,OK` / `,ERROR,code`. NMEA XOR checksum | Protocol spec |
| Probe | `$PQTMVERNO*58`, read-only, reply `$PQTMVERNO,<ver>,<date>,<time>`; an AA's version starts `LC29HAA` | Protocol spec |
| Output rate | `$PAIR062,<type>,<n>`: type 0 GGA, 1 GLL, 2 GSA, 3 GSV, 4 RMC, 5 VTG, 6 ZDA, 7 GRS, 8 GST; n = every n-th fix, 0 = off | Protocol spec |
| PPS | Module pin 3 "1PPS"; default width 100 ms; `$PQTMCFGPPS,W,1,<on>,<ms>,<mode>,<polarity>,<interval>` (mode 1 always / 2 2D / 3 3D; polarity 1 = high; interval 0 = every second); AA only. ~100 ns accuracy, rising edge on the second | Protocol spec, Hardware Design |
| PPS default mode, UTC vs GPS alignment | Not settled by the documents; the hub sets the mode itself and anchors to the UTC second RMC names | (unverified) |
| UBX | Not supported (NMEA, PAIR/PQTM and RTCM only) | Protocol spec |
| Logic level | 2.8 V I/O domain (VOH min 2.1 V), reference design for a 3.3 V MCU; supply 3.1-3.6 V | Hardware Design |

`Lc29hSequence` (via `Module::QuectelLc29h.sequence(..)`) sends, one at a time, each confirmed:
`$PAIR062` GGA 1, RMC 1, GSA 1, GSV 1 (or 0 with `Lc29hOptions { gsv: false }`), GLL 0, VTG 0,
then `$PQTMCFGPPS,W,1,1,100,3,1,0` (PPS on, 100 ms, **3D fix only**, active high, every second).
The fix rate stays at the 1 Hz default (changing it needs a save and restart). Nothing is saved
to the module's flash: the hub configures it at every boot.

### Detecting the module

`module::probe_bytes` writes a UBX MON-VER poll followed by `$PQTMVERNO`. Each module answers
its own and ignores the other; `Module::from_event(&ev)` says which (`MonVer`/UBX ACK =
u-blox, any Quectel reply = LC29H). Neither: `Module::Nmea`, read as is. `Module::sequence`
gives the matching boot configuration as `Step`s (`bytes()`, `name()`, `baud_after()`,
`confirmed_by(&Event)`), so the hub drives either module with one loop.

### Sky view

`Parser::sky()` returns a `SkyView` built from GSV and GSA: per `System` (GPS, GLONASS,
Galileo, BeiDou, QZSS, NavIC) satellites in view, satellites with signal on the L1 band and on
the L5 band, satellites used (GSA), and the best C/N0. `dual_band()` is true while anything is
tracked on L5. Groups not refreshed for 3 epochs are forgotten. Signal IDs map to bands per
the NMEA 4.10/4.11 table (in `src/sky.rs`); without signal IDs (older NMEA) GSV counts as L1 and
a `GNGSA` line's constellation is read from the satellite numbers. Empty on a UBX-only link
(the M10 after configuration).

## Configuration (u-blox)

`configure_sequence()` yields five CFG-VALSET frames, layers RAM + BBR (so it survives a
power cycle while the backup supply is up):

| Step | Sets |
|---|---|
| `nav-pvt-out` | NAV-PVT every epoch (1 Hz) on UART1 and I2C |
| `protocols` | UBX in/out on, **NMEA out off**, on UART1 and I2C |
| `dynmodel` | Portable (0) |
| `timepulse` | TP1 enabled, period 1 000 000 us, GNSS-synchronised, aligned to the UTC second, rising edge on the second, 100 ms wide once locked, length 0 (no pulse) before lock |
| `baud` | UART1 = 115200 (skipped with `ConfigOptions { baud: None, .. }`) |

NAV-PVT is switched on before NMEA is switched off, so the stream never goes silent. Each
frame is one transactionless VALSET (< 64 pairs); a NAK means nothing in that frame was applied.

**Confirming each step.** The receiver replies with UBX-ACK-ACK / ACK-NAK carrying class and
id of the message (0x06 0x8A for every step, so it identifies the message type, not which
step: send one, wait, send the next). `ConfigFrame::confirmed_by(&Event)` returns
`Some(true)` for an ACK, `Some(false)` for a NAK.

**The baud step is last on purpose.** The receiver changes speed as soon as it processes the
frame, so the ACK may arrive at the wrong rate or not at all. After sending it: wait for the
TX FIFO to drain, set the host UART to `step.baud_after`, call `parser.reset()`, and treat the
first valid NAV-PVT as the confirmation. If none arrives within about 2 s, go back to the
old rate (RAM and BBR are both written, so a power cycle does not undo it; if the unit ends
up on a rate you didn't expect, try 9600, 38400, 115200).

To read settings back: `config::valget(&mut buf, 0, 0, &[keys::UART1_BAUDRATE, ...])` and
`config::poll_mon_ver(&mut buf)`; replies come out of the parser as `Event::ValGet` /
`Event::MonVer`.

### Source of the key IDs

All IDs were checked against the text of **u-blox M10 SPG 5.10 Interface description,
protocol version 34.10, UBX-21035062 R03** (2023-06-27), chapter 4.9 (configuration items),
sections 3.10.4/3.10.5 (VALGET/VALSET layout), 3.15.11 (NAV-PVT), 3.14.9 (MON-VER) and 3.9
(ACK). Every ID suggested in the task brief was correct. They are listed, with docs, in
`src/config.rs` (`config::keys`). Of note: the document lists the firmware default
`CFG-UART1-BAUDRATE` as 38400, whereas the MAX-M10S module is said to ship at 9600 NMEA; the
default isn't relied on anywhere.

## Wiring

### UART (the default)

| MAX-M10S | Hub | Notes |
|---|---|---|
| TXD | hub RX pin | 9600 baud 8N1 out of the box, NMEA |
| RXD | hub TX pin | needed only to configure |
| VCC / GND | 3V3 / GND | |
| TIMEPULSE | PPS GPIO (see below) | |

Start the hub's UART at 9600, parse NMEA straight away (it works without any configuration),
and run `configure_sequence()` to move to UBX at 115200.

### I2C (DDC)

Address `0x42`, SDA/SCL with pull-ups (breakouts usually have them), 100 or 400 kHz. The
MAX-M10S selects its interface with the `D_SEL` pin (open = UART, GND = I2C; check the
datasheet or your breakout's silkscreen, as this was not verifiable from the interface
description). The hub loop:

1. write `0xFD`, read 2 bytes: big-endian count of bytes waiting (`ddc::bytes_available`);
2. if non-zero, write `0xFF` and read that many bytes (in chunks if the driver has a limit);
3. `parser.feed(&chunk)`.

Never strip `0xFF` yourself: it is legal inside frames. Reading exactly the advertised count
never returns fill. Reading when nothing is waiting returns `0xFF` fill, which the parser
skips between frames anyway. Poll at 10-20 Hz at most; 1 Hz NAV-PVT does not need more.
Run the same `configure_sequence()` over I2C (with `baud: None`; the I2C protocol keys are
included in it). The 100-byte NAV-PVT fits easily in one transaction.

### PPS (TIMEPULSE)

The `timepulse` step makes one 100 ms pulse per second, rising edge exactly on the UTC
second, **only while the receiver has valid GNSS time**. No lock means no pulses; use that
as the "time sync lost" signal in the hub. Wire the TIMEPULSE pin (3.3 V logic) to a GPIO on
the hub and to one GPIO on every node chip, and timestamp the rising edge in each chip's
own timer. The pulse carries the second boundary; NAV-PVT for that second arrives a few
hundred ms later and tells the hub *which* second it was (`utc`, or `itow_ms`, which counts
whole seconds of the epoch). Each pulse minus the previous gives a free check on each chip's
clock drift. `time_acc_ns` is the receiver's own estimate of the pulse accuracy.

## Own parser, not the `ublox` crate

Decided on 2026-11-09 after reading what `ublox` 0.10 offers (docs.rs feature list):

* **NMEA.** It parses UBX only. The brief requires NMEA to work on its own (cheap M10
  boards ship in NMEA mode), so a second parser (`nmea` crate) plus glue would be needed
  anyway. Here one buffer and one state machine handle both, with one resync rule.
* **Protocol version.** Its protocol features stop at `ubx_proto33`; the MAX-M10S is 34.x. We
  only need NAV-PVT, ACK, MON-VER and CFG-VAL*, whose layouts are small and fixed (all
  verified against the 34.10 document and covered by tests).
* **Footprint and portability.** The crate pulls in a proc-macro derive for its message set
  and its generic buffer traits; ours is about 1,500 lines of source (the tests are extra), no dependencies, builds clean for
  `riscv32imac-unknown-none-elf` (and `riscv32imc`), and has no atomics or alloc, so it also
  builds for Xtensa (S3) with Espressif's toolchain (not built here; nothing in it is
  target-specific).
* **Control.** The things that matter on this product are small and specific: checksums
  verified on every byte path, bounded buffer, and "fix, but untrusted" semantics
  (`fix_ok`, `position_valid`, UTC only when valid). Writing them down once, with tests, is
  less risk than adapting a general crate.

The cost is that we maintain 4 message decoders ourselves. If more u-blox messages are needed
later (NAV-SAT for a sky view, NAV-STATUS), add them in `ubx.rs`; if a large part of the
message set is ever needed, revisit.

## Tests

`cargo test -p gnss` (103 tests: the 16 LC29H and 43 u-blox tests below, plus 36 in `src/tests_integrity.rs` and 8 in `src/timeguard.rs`). LC29H (`src/tests_lc29h.rs`, 16): a full dual-band NMEA 4.10
epoch built from the protocol specification's example sentences (checksums re-checked), the
sky view per constellation and band, QZSS under GP, ageing out a band, a lost GSV message,
2D from GSA, every talker, RMC with the 4.10 navigational-status field, no-fix output,
PAIR001/PQTMVERNO/PQTMCFGPPS replies and errors, unknown proprietary sentences, the command
builder against the manual's own sentences, the boot sequence and which replies confirm each
step, module detection, and any chunking of an interleaved stream. u-blox (43): hand-built NAV-PVT frames with independently computed
checksums (3D, 2D, no fix, negative lat/lon, invalid/partial time, unknown accuracy, heading
wrap); real NMEA sentences with computed checksums (GP/GN/GL/GA/GB, empty no-fix GGA, void
RMC, southern/eastern hemispheres, out-of-range coordinates); byte-at-a-time and 300
random chunkings that must equal the one-shot result; garbage between frames; bad UBX and
NMEA checksums; truncated frames; false headers; oversize lengths; DDC idle fill; random
noise fuzz; CFG-VALSET payload layout (version, layers, reserved, key and value little-endian);
the sequence contents; ACK confirmation. No-std check:

```
cargo build -p gnss --target riscv32imac-unknown-none-elf
```
