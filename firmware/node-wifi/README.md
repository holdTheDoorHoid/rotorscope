# node-wifi: Wi-Fi Remote ID listener

One source tree, two nodes:

| Node | Chip | Band | Channel plan |
|---|---|---|---|
| A | ESP32-S3 | 2.4 GHz | 6 (150 ms) → 1 (50 ms) → 6 (150 ms) → 11 (50 ms): ~75% on 6 |
| C | ESP32-C5 | 5 GHz | 149 (150 ms) alternating with 153/157/161/165 (50 ms each): ~75% on 149. Set `INCLUDE_UNII1` in `src/chip.rs` to add 36–48. |

It listens in promiscuous mode (receive-only), finds Remote ID beacons (vendor element
`FA:0B:BC`/`0x0D`) and NAN action frames (to `51:6F:9A:01:00:00`) with `crates/rid`, and reports each
hit on USB as a JSON line and to the hub as a `link::Frame`.

## Build

The chip is chosen by the directory you build from. Each holds only the toolchain pin and cargo
config; both share `Cargo.toml` and `src/`, so cargo finds the package one level up.

```sh
# Node A, ESP32-S3 (Xtensa: Espressif's `esp` toolchain from espup)
. ~/export-esp.sh
cd firmware/node-wifi/s3 && cargo fw
#   -> firmware/node-wifi/target/xtensa-esp32s3-none-elf/release/node-wifi

# Node C, ESP32-C5 (RISC-V: stable Rust)
cd firmware/node-wifi/c5 && cargo fw
#   -> firmware/node-wifi/target/riscv32imac-unknown-none-elf/release/node-wifi
```

`cargo fw` is an alias for `cargo build --release --features esp32s3` (or `esp32c5`). Building
from the wrong directory, or without a chip feature, stops in `build.rs` with a message saying so.

Flash on the bench with `tools/dev-board.sh <elf> [seconds]` (it holds the shared board lock).

## Outputs

### USB (USB-Serial-JTAG): JSON lines

Each line is one JSON object with a `"t"` tag. The ESP-IDF second-stage bootloader prints a few
`I (..) boot:` lines at reset, and a panic prints a backtrace: readers should skip lines that do
not start with `{`. Diagnostic logging is off (`LOG_LEVEL` in `src/main.rs`) to keep the stream clean.

```json
{"t":"hello","fw":"node-wifi","ver":"0.1.0","chip":"esp32s3","role":"Wifi24","proto":1,"fcs":4,"plan":[[6,150],[1,50],[6,150],[11,50]]}
{"t":"stats","up_us":59266007,"ch":6,"seen":3131,"fps":62,"rid":0,"drop":0,"qmax":1,"other":0,"trunc":12,"ch_err":0,"hub_drop":0,"iefit":[2886,0,0],"per_ch":[[6,2652],[1,410],[11,69]]}
{"t":"rid","tr":"WifiBeacon","ch":6,"rssi":-60,"mac":"aa:bb:cc:dd:ee:ff","ctr":12,"up_us":123456,"msgs":[{"BasicId":{...}},{"Location":{...}},"0d1f..."]}
{"t":"err","what":"set_channel","ch":149,"e":"..."}
```

- `rid`: one per Remote ID frame. `msgs` holds each message decoded by `rid` (serde shape of
  `rid::message::Message`), or its 25 bytes as hex if the decoder rejected it. If the payload could
  not be split into messages at all, `msgs` is empty and `err` names the `rid::Error`.
- `stats`, once a second. Totals since boot unless noted:
  `seen` management frames, `fps` of them in the last second, `rid` Remote ID frames,
  `drop` frames lost to a full queue, `qmax` most queue slots in use in the last second (of 32),
  `other` non-management callbacks that leaked through the filter, `trunc` frames over 512 bytes
  (kept truncated), `ch_err` failed channel changes, `hub_drop` hub frames lost,
  `per_ch` frames per channel, and `iefit` (below).
- `err`: something the node could not do. A `set_channel` error is printed once per plan entry,
  then only counted in `ch_err`.

### Hub UART: `link::Frame`

postcard + COBS via `link::encode`, on UART1 TX at 921600 baud (`chip::HUB_BAUD`):
S3 GPIO43, C5 GPIO11 (the boards' TX pins; see `hub_tx_pin!` in `src/chip.rs`). `Hello` every 5 s,
`Stats` every second, `Detection` (raw messages) per hit. On the S3 the ROM bootloader also prints
on GPIO43 for a moment after reset; the hub's decoder discards that as a corrupt frame.

## Design notes

- **The receive callback only copies.** It runs on the Wi-Fi driver's thread. It drops anything
  that is not a management frame (checking frame control itself, as well as setting the driver's
  management-only filter), copies up to 512 bytes into one of 32 static slots, and passes the slot's
  index to the `drain` task. A full queue is counted in `drop`, never waited on.
- **Cheap pre-check before decoding.** `drain` only calls `rid::wifi::extract` on beacons containing
  the `FA 0B BC 0D` byte sequence and on action frames addressed to the NAN Remote ID address.
- **FCS: `PromiscuousPkt.data` ends with 4 bytes that are not part of the frame.** esp-radio sizes
  `data` by `sig_len`, which includes the FCS slot on every MAC generation. `rx::RxFrame::mpdu()`
  drops them before anything parses the frame (`chip::FCS_LEN`). Checked on hardware: on the S3,
  2,886 of 2,886 beacons in a 60 s run had their element list end exactly at `sig_len - 4`
  (`iefit:[2886,0,0]`). On the C5, DRONK measured the same (the MPDU ends at `sig_len - 4`; the
  4 bytes are not a valid CRC). The `iefit` counter keeps checking: `[at sig_len-4, at sig_len,
  neither]`. If the middle number ever dominates, `FCS_LEN` is wrong for that chip/driver.
- **Country and band.** Both chips call `esp_wifi_set_country_code("US")` directly: esp-radio
  1.0.0-beta.1's own `CountryInfo` path passes a zero 5 GHz channel mask, and upstream's default
  is moving to "01" (world-safe), which rejects channel 149. The C5 also sets the band to 5 GHz
  explicitly before the first channel change. These two calls (and the promiscuous filter) go
  straight to the Wi-Fi blob through `esp-wifi-sys-*`, because esp-radio does not wrap them yet.
- **esp-radio version.** `=1.0.0-beta.1`. beta.2 is yanked on crates.io (its only change fixed
  ESP32-S31 builds).

## Status

- ESP32-S3: verified on the bench (60 s runs: no drops, queue never above 1 slot, no panics, hop plan
  followed). No Remote ID source was available yet, so the decode/report path has not seen a
  real frame on hardware.
- ESP32-C5: builds; not yet run on hardware. First boot should show no `set_channel` errors and
  `per_ch` rising on 149.
