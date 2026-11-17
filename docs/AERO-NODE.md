# The aero node (LR2021): aircraft signals the ADSBee does not hear

**Status (2026-11-17): written and host-tested, never run on hardware.** No LR2021 board was
in hand. Everything below about how it behaves on air is what the code is meant to do, not
what has been seen.

## In plain words

- **What it is.** A small receiver box: an ESP32-C3 microcontroller with a Semtech LR2021
  radio chip, reporting to the hub like the other nodes (over ESP-NOW through the gateway,
  or over a wire).
- **What it hears.** One of three things at a time, chosen by the hub:
  1. **1090 MHz ADS-B** (airliners and most aircraft): position, identity and speed
     messages, the same signal the ADSBee hears. Useful as a second ear in another spot.
  2. **978 MHz UAT** (US light aircraft below 18,000 ft): the ADSBee 1090 cannot hear this.
  3. **FLARM and OGN trackers** (gliders, light aircraft, some helicopters and drones) on
     902-928 MHz in the US (868 MHz in Europe). These hop to a new channel twice a second.
     The node uses the hub's clock (GPS time) to jump to the right channel at the right
     moment, so it hears every transmission instead of about 1 in 65.
- **What it sends.** The raw messages, unopened. The hub decodes them. The node only checks
  each message is intact (so noise is not sent on), and sends at most 200 a second.
- **What it never does.** It never transmits on 1090, 978, 902-928 or 868 MHz. There is no
  code that can make the LR2021 transmit at all. Its only transmissions are its normal link
  to the hub, like every other node.
- **FLARM content.** FLARM's own message contents are proprietary and FLARM has objected to
  third-party decoding (PLAN.md 14d). The node does not decode them: it checks FLARM's
  checksum and passes the scrambled bytes on. Whether the hub decodes them is a separate
  decision; presence alone (an aircraft with a FLARM is near) needs no decoding.
- **Without the hub's time** (no time message for 10 seconds), FLARM/OGN mode cannot follow
  the hops. The node then sits on one channel, still hears the transmissions that happen to
  land there, and tells the hub it is "parked".

### Not verified (needs the hardware)

- That any of it receives anything. Every setting is copied from SoftRF, which runs on
  LR2021 boards, but four chip details could not be confirmed without the datasheet's full
  register map or a board; see "Assumptions" below.
- The pin wiring (a proposal, see below) and the module's TCXO voltage and interrupt pin.
- Hopping accuracy over ESP-NOW (expected to be a few milliseconds; slots are 400 ms).
- The 978 MHz and FLARM sensitivity of an LR2021 module with its stock antenna.

### Which board to buy

In order of preference (PLAN.md Phase 9):

1. An **ExpressLRS-style ESP32-C3 + LR2021 receiver** (GEPRC LINKFLOW LR20, BlackSheep ES4,
   or a "generic C3 LR2021" receiver) once one is on sale. Everything is on one board; its pin
   wiring comes from its ExpressLRS hardware target file and goes in `src/board.rs`.
2. For the bench now: a **Waveshare Core2021** or **Elecrow LR2021** module wired to a spare
   **ESP32-C3** board (SuperMini or DevKitM-1).

An antenna for the band you want matters more than anything else: a 1090 MHz antenna will
hear little on 915 MHz and the reverse.

### Wiring an LR2021 module to an ESP32-C3

All pins live in **`firmware/node-lr/src/board.rs`**, and only there.

| Signal              | ESP32-C3 | LR2021 module              |
|---------------------|----------|----------------------------|
| SPI clock           | GPIO4    | SCK                        |
| SPI MISO            | GPIO5    | MISO                       |
| SPI MOSI            | GPIO6    | MOSI                       |
| SPI chip select     | GPIO7    | NSS                        |
| Busy                | GPIO3    | BUSY                       |
| Interrupt           | GPIO10   | DIO9 (check your module)   |
| Reset               | GPIO1    | NRESET                     |
| 3.3 V, ground       | 3V3, GND | VCC, GND                   |
| Hub TX (wired)      | GPIO21   | (to the hub's RX)          |
| Hub RX              | GPIO20   | (from the hub's TX)        |
| Bus TX (wired bus)  | GPIO0    | (shared return wire, open-drain) |

GPIO2, 8 and 9 are avoided: they decide how the C3 boots, and a radio module pulling them
the wrong way at power-on would stop it starting.

### Building

```
cd firmware/node-lr
cargo build --release                     # wired UART to the hub
cargo build --release --features espnow   # ESP-NOW to the gateway (uses tools/link.key if present)
cargo build --release --features wirebus  # the wired bus (docs/WIREBUS.md)
```

Output: `target/riscv32imc-unknown-none-elf/release/node-lr`. Sizes on 2026-11-17: 427 KB of
code (wired), 431 KB (wired bus), 863 KB (ESP-NOW, with the Wi-Fi driver); a C3 has 4 MB of
flash. No flashing script handles this board yet (`tools/flash-node.sh` is for the C5
multi-image nodes); flash with `espflash flash --chip esp32c3` once there is a board.

Without `tools/link.key` the ESP-NOW build runs unauthenticated and says so at boot (a
`"what":"link_key"` warning line), exactly like the other nodes.

## Technical detail

### Pieces

| Piece | What | Tests |
|---|---|---|
| `crates/aerohop` | OGN/FLARM frequency plans, `FreqHopHash`, slot schedule, hub-synced UTC clock | 15 |
| `crates/lr2021cfg` | LR2021 commands (receive only), generic `RxConfig`, aero roles, Manchester / DF17 / CRC checks, plan cursor, watchdog, rate limit | 21 |
| `crates/link` v11 | `Frame::Air`, `Frame::AeroStatus`, `Command::TimeSync`, `NodeRole::Aero`, `AeroMode` | 4 new |
| `crates/wirebus` | node keeps the latest `TimeSync` (a broadcast) with its arrival time | 1 new |
| `firmware/node-lr` | the ESP32-C3 firmware | builds; no hardware tests |

### Driver choice

The `lr2021` crate (0.14, MIT) is the best public description of the chip's commands, but it
cannot be a dependency here: it switches on `embassy-time`'s `defmt` and `tick-hz-32_768`
features, and every Rotorscope node runs `esp-rtos`, which fixes the tick at 1 MHz. The two
cannot be linked into one image. So `crates/lr2021cfg/src/cmd.rs` ports the ~30 command
encoders the node needs from it (MIT, credited), and `firmware/node-lr/src/lr.rs` is a
~130-line blocking SPI driver (BUSY handshake, command, response, FIFO read). The command
module has no transmit command at all (no SetTx, PA, TX power or TX FIFO write); a test
checks for the opcodes, and the RF-switch command can only set receive positions.

### Link protocol (v11, all appended; v10 frames encode byte for byte as before)

- `Frame::Air(AirFrame { proto, freq_khz, rssi, utc_ms: Option<u64>, uptime_us, raw: Vec<u8, 64> })`,
  `proto` = `AirProto::{Es1090, Uat978, Flarm, Ogn}`:
  - `Es1090`: the 14-byte DF17 frame, CRC clean (`modes::crc::syndrome == 0`).
  - `Uat978`: the 48 bytes after the sync word, uncorrected (the hub runs Reed-Solomon).
  - `Flarm`: 26 bytes = SoftRF's `RxBuffer` after Manchester decoding (24 payload, still
    scrambled, + CRC-16), CRC checked.
  - `Ogn`: 26 bytes (20 payload + 6 LDPC parity), Manchester decoded, not LDPC checked;
    dropped if more than 12 of 208 chip pairs are invalid.
  - `utc_ms`: the node's UTC estimate at reception, `None` without time.
- `Frame::AeroStatus(AeroStatus { mode, freq_khz, time: AeroTime, parked, radio_ok,
  rx_restarts, rejected, rate_dropped })`: with every Hello (5 s) and at once when mode,
  time state, parking or radio state change. `time` = `Unknown | Hub | HubPps`.
- `Command::TimeSync { utc_ms, pps }`: hub to node, about once a second. The node takes the
  arrival as "now". A broadcast on the wired bus (no `To` needed).
- `NodeRole::Aero` (bit 0x40 in `can`). Its `Assignment::plan` entries are
  `(AeroMode::code(), dwell_ms)`:
  - `10900` = 1090 ES, `9780` = UAT 978 (frequency in 100 kHz units, as raw-capture plans);
  - bit 31 set = FLARM/OGN hopping: bits 0-7 = region (SoftRF `RF_BAND_*`: 1 Europe,
    2 USA/Canada, 3 Australia/S. America, ...), bit 8 = follow OGN instead of FLARM;
  - several entries time-share in order (e.g. 1090 for 20 s, then 978 for 10 s); one entry
    or a dwell of 0 stays; empty = 1090. Unknown codes are skipped (and counted), so new
    plan kinds (a control-link scan) can be appended as new codes.
- `Stats` from this node: `frames_seen` = packets the radio delivered, `rid_found` =
  `Frame::Air` sent, `dropped` = rate-limit drops + hub-pipe drops.

### Radio settings (from SoftRF `radiolib.cpp`, LR20xx section)

| Mode | Modulation | Rate | Deviation | RX BW | Sync | Length |
|---|---|---|---|---|---|---|
| 1090 ES | OOK, NRZ | 2 Mchip/s | - | 3076 kHz | 0x95 (8 bits) | 27 bytes |
| UAT 978 | 2-FSK BT0.5 | 1041.667 kbit/s | ±312.5 kHz | 3076 kHz | AC DD A4 E2 | 48 bytes |
| FLARM | GFSK BT0.5 | 100 kchip/s | ±50 kHz | 307.7 kHz | 55 99 A5 A9 55 66 65 96 | 52 bytes |
| OGN | GFSK BT0.5 | 100 kchip/s | ±50 kHz | 307.7 kHz | AA 66 55 A5 96 99 96 5A | 52 bytes |

1090 extras as SoftRF: OOK preamble detector pattern 0x0285, automatic gain, detection
threshold -80, register 0xF30150 low byte = 0x18. The 1090 trick ("OPT_DF17 1"): the sync
word 0x95 is the Manchester chips of DF17's first nibble, so the chip only wakes for
DF16/17-looking frames; the node prepends 0x95 to the 27 bytes, decodes the 28 chip bytes
with inverted Manchester into 14 bytes, and keeps DF17 frames whose CRC is clean.

The 1090 watchdog is SoftRF's: every 30 s, if nothing arrived in the last 15 s, restart
reception (SoftRF found the LR20xx OOK receiver can go quiet).

### Hopping

`aerohop::plan` is the OGN tracker's `freqplan.h` (GPLv3, via SoftRF; credited): US plan
902.2 MHz + n x 400 kHz, 65 channels; `channel = FreqHopHash((second << 1) + slot) % 65`, OGN
trackers one channel up in slot 0 and on FLARM's slot-0 channel in slot 1. Slots are
400-800 ms and 800-1200 ms after each UTC second (SoftRF's Legacy / OGNTP descriptors); the
node retunes at 200 ms (the silent gap) and 800 ms. Hash values are checked against
hand-worked arithmetic and an independent script.

UTC comes from `Command::TimeSync`; `aerohop::clock` measures the node crystal's rate against
the hub over 20 s-10 min baselines (bridging gaps to ~1 ms) and declares the time unknown
after 10 s without a sync.

### Assumptions about the LR2021 not confirmed here

1. **OOK detection threshold encoding.** SoftRF calls RadioLib's `setOokDetectionThreshold(-80)`;
   its register encoding is not public. The node writes the `lr2021` crate's `set_ook_thr(-80)`
   encoding (register 0xF30E14 bits 26:20). Could be wrong; one constant
   (`roles::ES1090_OOK_THRESHOLD`) and one line to change.
2. **OOK detector pattern length field.** SoftRF passes 16 for pattern 0x0285; the field is 4
   bits, and the `lr2021` crate writes 15 for the same pattern, so the node writes 15
   (length - 1).
3. **Sync word bit order** for OOK and FSK: sent most significant bit first, which is what
   SoftRF's decode implies. The FSK packet engine used is "FSK legacy" (packet type 2), the
   one the 0x0240-0x0247 commands configure.
4. **RX boost level and TCXO** (`board.rs`): SoftRF asks for RadioLib's "boosted gain" (value
   not visible); the node uses the maximum (7). The TCXO voltage (3.3 V) and start time
   (~5 ms) are from SoftRF's ExpressLRS-style board and must be checked per module.

### Room for the control-link scan (Phase 9)

`lr2021cfg::config::RxConfig` is generic: a frequency (the sub-GHz or 2.4 GHz path follows
from it) and a `Modem` (`Ook`, `Fsk`, `Lora`, or `RssiOnly` for energy dwells read with
`GetRssiInst`). The aero roles are four such configs; a scan plan is a list of more, applied
with the same `config::apply`. FLRC is not encoded yet (its opcodes are in the `lr2021`
crate's `cmd_flrc.rs`). A new plan kind is a new `AeroMode` code (or a new role), appended.
