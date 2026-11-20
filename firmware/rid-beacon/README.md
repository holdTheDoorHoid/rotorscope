# rid-beacon — bench Remote ID test transmitter

The project owns no drone and no Remote ID broadcast module, and no commercial module is
confirmed to transmit on 5 GHz. This firmware is the known signal every receiver
(`node-wifi`, `node-ble`, the 5 GHz node) is tested against.

- **ESP32-S3:** Wi-Fi on 2.4 GHz channel 6, plus Bluetooth.
- **ESP32-C5:** Wi-Fi on 5 GHz channel 149 by default (2.4 GHz on request), plus Bluetooth.

Bare-metal Rust: `esp-hal` 1.2, `esp-radio` 1.0.0-beta.1, `trouble-host` 0.8, embassy.
All Remote ID bytes come from the shared `rid` crate (`rid::encode`), the same crate the
receivers decode with. The firmware has no encoder of its own.

It can also send **fake open digital FPV link frames** (wfb-ng/OpenIPC, OpenHD, RubyFPV,
DroneBridge), off until a console key turns one on. Those bytes come from `openlink::build`, the
same crate `node-wifi` classifies with. See "Fake open FPV links" below.

## Conduct (non-negotiable)

This is a **bench** transmitter. Metres, not kilometres.

- **Obviously fake identity.** UAS ID `TESTDRONE0000000001`, operator ID `TEST-OPERATOR`,
  Self-ID `BENCH TEST-NOT AIRCRAFT`, SSID `RID-TEST`.
  - The Self-ID field holds 23 bytes, so "BENCH TEST - NOT AN AIRCRAFT" (28) is shortened.
  - The UAS ID is sent as type "serial number" but is not a valid ANSI/CTA-2063-A serial.
    Its length character says 13 characters and 14 follow.
- **Never imitate a real aircraft, serial, registration or manufacturer.** Do not change the
  constants in `src/rid_tx.rs` to anything resembling one.
  - The Wi-Fi source address is `02:52:49:44:00:53` (S3) or `…:43` (C5). It is locally
    administered, so it cannot collide with any vendor's OUI.
  - Bluetooth uses the static random address `C2:52:49:44:00:53` / `…:43`.
- **Lowest practical TX power, by default:**
  - **Wi-Fi:** `set_max_tx_power(8)`, which is **2 dBm**. That is the lowest value esp-radio
    accepts (range 8–84 in 0.25 dBm units; the library default is 20, or 5 dBm). It is
    re-applied after every band switch.
  - **BLE:** the controller default is **−15 dBm** (`TxPower::N15`, esp-radio's lowest; the
    library default is +9 dBm). Each advertising set also asks for −16 dBm, and the controller
    picks its nearest supported level.
  - Measured on the bench: the laptop saw the beacon at about −60 dBm and BLE at −68 to
    −80 dBm, from roughly a metre away.
- **Simulated position.** The aircraft flies a 50 m radius circle once every 60 s, clockwise
  from due north, at about 5.2 m/s, 50 m above ground, with status "airborne".
  A bench build with `scripted` can fly the threat-level paths instead (see "Console").
  - The operator stands fixed at the centre.
  - The centre is set at build time with `RID_TEST_LAT` / `RID_TEST_LON`. The default is
    Philadelphia City Hall (39.9526, −75.1652).
  - There is no GPS or clock, so both timestamps (Location and System) are sent as "unknown",
    which F3411 allows. (Uptime used to stand in, and every hub with GPS time flagged the track
    `clock_skew`.) The laptop BLE beacon (`tools/rid-ble-beacon`) shares this code and has a
    real clock, so its Location timestamp is the true seconds after the hour.
- **Fake open FPV links: header shape only, obviously fake IDs, off at boot.**
  - Approved by the owner 2026-11-14 for bench testing, on the same terms as the Remote ID modes.
  - IDs all start `7E57` ("TEST"): wfb-ng link ID `7e57ed` (not the real default `7505d6`),
    RubyFPV vehicle ID `7e570001`, DroneBridge comm ID `7e`, OpenHD nonce high byte `7e` (a real
    OpenHD nonce never reaches it). The body is the text `ROTORSCOPE BENCH TEST - NOT A REAL LINK`.
  - Defined once, in `crates/openlink/src/build.rs`. Do not change them to a real link's values.
  - **Never DJI.** This firmware must not send DJI DroneID of any kind: not OcuSync DroneID, not
    the DJI Wi-Fi vendor element (OUI `26:37:12`).
- **Indoors, on the bench, only while testing.** Press `x` on the console to silence every
  transport without a reset.

## What it transmits

All five transports are on by default. Each can be toggled live (see "Console" below) or
defaulted off with the `DEFAULT_*` consts in `src/control.rs`.

| Transport | Payload | Rate | Counter increments per |
|---|---|---|---|
| Wi-Fi **beacon**, SSID `RID-TEST`, ASTM vendor IE (`FA:0B:BC` / `0x0D`) | Message Pack: Basic ID, Location, Self-ID, System, Operator ID | 1 Hz (`RID_BEACON_HZ`) | frame |
| Wi-Fi **NAN** service-discovery action frame to `51:6F:9A:01:00:00`, preceded by core-c's NAN sync beacon | same pack | 1 Hz, 500 ms after the beacon | frame |
| **BLE 4 legacy**, non-connectable, service data `0xFFFA` / `0x0D` | one message per advert, rotating through the 5 types | payload changes every 200 ms (each type at 1 Hz); advert interval 100 ms | payload |
| **BLE 5 extended**, 1M primary + secondary PHY | the pack (134-byte AD) | payload 1 Hz; interval 200 ms | payload |
| **BLE 5 Long Range**, coded PHY primary + secondary | the pack | payload 1 Hz; interval 300 ms | payload |

**Wi-Fi channel.**
- S3: 2.4 GHz channel 6.
- C5: 5 GHz channel 149 by default. Country **"US" is set explicitly**, because a recent esp-hal
  change moves the default to "01", which rejects 149. Console `b` toggles the C5 between
  149 and 2.4 GHz channel 6; keys `6` `7` `8` `9` pick channel 6, 149, 161 or 165. Every Wi-Fi
  transmission (beacon, NAN, fake open link) uses the same channel.

## Fake open FPV links

One at a time, off at boot, Wi-Fi only, at the same 2 dBm. While one is on it sends **20 frames a
second** on the current channel (real video links send hundreds). `x` turns it off with
everything else; typing its key again turns it off alone. `a` (all on) does not turn it on.

| Key | System | Frame (built by `openlink::build::fake_frame`) | Fake ID |
|---|---|---|---|
| `f` | wfb-ng / OpenIPC | data `08 01`, A1 broadcast, A2 = A3 = `57:42:7e:57:ed:00` (port 0, video down), packet type 1 | link ID `7e57ed` |
| `o` | OpenHD | data `08 01`, A2 = `01:<nonce>:02`, A3 = `01:7e:00:00:00:02` (air side, port 2) | none in clear |
| `r` | RubyFPV | data `08 01`, A1 = `ff:..` (downlink port 0x0F), A2 = A3 = `13:12:34:56:78:90`, unencrypted packet header | vehicle ID `7e570001` |
| `d` | DroneBridge | `08 00 00 00`, direction 3 (to ground), port 2 (telemetry), length 48, no MAC header | comm ID `7e` |

Frames go out through `esp_wifi_80211_tx` with the driver's sequence numbering **off**: the
builder writes those bytes, and in a DroneBridge frame they are payload.

**What this cannot imitate.** The ESP32 raw-TX API (`esp_wifi_80211_tx`, rate set by
`esp_wifi_config_80211_tx`) chooses only the PHY mode and rate. It has no switch for **LDPC** or
**STBC**, which wfb-ng uses by default for video. Our frames are plain BCC-coded OFDM at the
driver's default raw rate. So a PASS here proves the header classifier and the receive path,
not that a C5 decodes wfb-ng's default LDPC/STBC video. That needs real OpenIPC hardware.

**How Wi-Fi is sent.** Wi-Fi uses raw 802.11 TX (`Sniffer::send_raw_frame`, which is
`esp_wifi_80211_tx`) on the station interface.
- The driver is started in station mode and never associates.
- Frames go out on whatever channel the radio is parked on.
- The driver fills in the sequence number.

**BLE counters.** A BLE "transmission" in the counts is one new payload (one counter value)
handed to the controller. The controller repeats each payload at the advertising interval,
and the firmware cannot see individual air packets. Each counter value therefore goes out
several times, which is normal for Remote ID.

## Build

One source tree, two chips, chosen by Cargo feature:

```sh
cd firmware/rid-beacon

# ESP32-S3 (Xtensa, Espressif toolchain)
. ~/export-esp.sh
cargo build --release
# -> target/xtensa-esp32s3-none-elf/release/rid-beacon

# ESP32-C5 (RISC-V, stock stable toolchain)
cargo +stable build --release --target riscv32imac-unknown-none-elf \
      --no-default-features --features esp32c5
# -> target/riscv32imac-unknown-none-elf/release/rid-beacon
```

Build-time options (environment variables; changing one triggers a rebuild):

| Variable | Default | Meaning |
|---|---|---|
| `RID_TEST_LAT`, `RID_TEST_LON` | 39.9526, −75.1652 | Centre of the circle and the operator position. If a value is invalid, the default is used and a warning is logged. |
| `RID_BEACON_HZ` | `1` | Beacons per second: 1, 2, 5 or 10. Use **10** when testing with a laptop (see below). |
| `RID_BLE_DISCOVERABLE` | unset | Set it to `1` to add a plain legacy advert (Flags + name `RID-TEST`) from the same address. BlueZ hides any device that never advertises "discoverable", and Remote ID adverts carry no Flags. This is a verification aid only, not part of F3411. |

Flash with the shared, locked helper. Never use espflash directly on the shared bench:

```sh
tools/dev-board.sh firmware/rid-beacon/target/xtensa-esp32s3-none-elf/release/rid-beacon 40
```

## Console (USB-Serial-JTAG)

**Output.**
- One JSON line per second:
  `{"t":"tx","up":5,"band":"2.4","ch":6,"beacon":1,"nan":1,"ble4":5,"ble5":1,"ble_lr":1,"open_sys":"off","open":0,"fail":0,"on":"WNLEC","lat":…,"lon":…,"hdg":122}`
  - The transport fields count transmissions in the last second. `open_sys` is the fake open
    link that is on (`wfb-ng`, `openhd`, `rubyfpv`, `dronebridge` or `off`), `open` its frames.
  - In `on`, an upper-case letter means the transport is enabled, lower-case means disabled,
    and `-` means the controller refused it at boot.
- At boot:
  - one `{"t":"boot",…}` line with the identity, powers, addresses and centre;
  - one `{"t":"ble_probe","tx":…,"ok":…}` line per BLE transport. Each BLE set is tried alone
    once, so a PHY the controller refuses only takes out that one transport.
- Log lines begin with `INFO -` / `ERROR -`. JSON consumers should keep only lines that start
  with `{`.

**Input: single characters.** No newline is needed, and case does not matter.

| Key | Action |
|---|---|
| `w` | toggle Wi-Fi beacon |
| `n` | toggle Wi-Fi NAN |
| `l` | toggle BLE 4 legacy |
| `e` | toggle BLE 5 extended (1M) |
| `c` | toggle BLE 5 Long Range (coded) |
| `a` | all on |
| `x` | all off (silence), fake open links included |
| `b` | C5 only: toggle 5 GHz ch 149 ↔ 2.4 GHz ch 6 |
| `f` / `o` / `r` / `d` | fake wfb-ng / OpenHD / RubyFPV / DroneBridge link on (one at a time) or off |
| `6` | Wi-Fi channel 6 (2.4 GHz) |
| `7` / `8` / `9` | C5 only: Wi-Fi channel 149 / 161 / 165 |
| `?` / `h` | help (JSON) |

Each command echoes a `{"t":"cmd",…}` line.

**Scripted flights (bench builds only).** Built with `ROTORSCOPE_BENCH=1` and the feature
`scripted` (`build.rs` refuses the feature without it, and the image carries
`bench-build:scripted`, so `tools/dev-board.sh` refuses to flash it without it too), keys `0` to `5`
change the flight; the circle is flown until one is pressed. The paths (`src/script.rs`) are the
threat-level bench flights of `docs/THREAT-PROFILES.md`, round the same centre:

| Key | Path |
|---|---|
| `0` | the circle (as above) |
| `1` | beeline: from 800 m south straight at the centre at 12 m/s, 50 m up coming down to 30 m, then hovering over it |
| `2` | fast inbound: from 1500 m east at 30 m/s, 100 m up coming down to 20 m, then hovering over the centre |
| `3` | straight pass: west to east 300 m north of the centre at 15 m/s, 100 m up, 2 km, then hovering at the end |
| `4` | low hover: over the centre, down from 40 m at 2 m/s to 10 m, then still |
| `5` | drop: 30 m east, 20 m north of the centre, 50 m up; from second 40 down 30 m at 3 m/s, 5 s, back up |

A key starts its path from the beginning (again, if it is the one flying). Only the Location and
System messages change: position, speed, direction, height above ground (the geodetic height is
the ground plus it) and climb rate; the scripted paths send **no pilot position** (unknown), as
the host test flights of hostile drones do. Identity, transports, rates and power are the same.
Once a second a `{"t":"path","path":"beeline","s":12,"lat":…,"lon":…,"h":…,"v":…,"hdg":…,"vs":…}`
line says where it is. `crates/tracker/src/behaviour/tests_bench.rs` flies this very file through
the tracker on the host.

```sh
(cd firmware/rid-beacon && ROTORSCOPE_BENCH=1 cargo +stable build --release --locked --target riscv32imac-unknown-none-elf --no-default-features --features esp32c5,scripted --target-dir target-scripted)
```
- To send keys, use any serial terminal on the board's USB port.
- `tools/dev-board.sh --send KEYS` types keys after the reset; the bench runner uses it.

## Verified on hardware (ESP32-S3 rev 0.2, 2026-11-09)

**Firmware.**
- It boots and reports all five transports every second with zero failures.
- At boot, the controller accepts all three BLE sets, including **coded PHY**.

**Wi-Fi beacon.**
- Command: `nmcli -f SSID,BSSID,CHAN,SIGNAL dev wifi list --rescan yes`.
- With `RID_BEACON_HZ=10`, the laptop shows `RID-TEST  02:52:49:44:00:53  6  77`.
- At the default 1 Hz it did **not** appear in 5 rescans. The laptop's active scan only
  stays on channel 6 for a few tens of milliseconds, so it rarely catches a once-a-second
  beacon.
  - Phone RID apps and our sniffer listen continuously and are not affected.
  - Use 10 Hz when testing with a laptop.

**BLE 4 legacy.**
- Built with `RID_BLE_DISCOVERABLE=1` and checked with
  `bluetoothctl` (`menu scan` → `duplicate-data on` → `back` → `scan on`), then
  `bluetoothctl info C2:52:49:44:00:53`.
- It shows `ServiceData.0000fffa-0000-1000-8000-00805f9b34fb` with the rotating messages.
  For example, `0d 41 02 12 54 45 53 54 44 52 4f 4e 45 …` decodes as app code 0x0D,
  counter 0x41, Basic ID, serial number, `TESTDRONE0000000001`.
- `duplicate-data on` is required. Without it, the laptop's controller (Intel, Bluetooth 4.2)
  reports one advert per address and drops the rotating messages.

**Not verified on air.**
- The laptop's controller is Bluetooth 4.2. It cannot receive extended advertising at all,
  so **BLE 5 extended and Long Range were not verified on air**. They are only verified as
  accepted by the controller.
- NAN was not checked from the laptop either. That needs a monitor-mode sniffer or `node-wifi`.
- *Update 2026-11-18: superseded on 2026-11-12 (docs/BENCH.md): the S3's BLE 5 extended and Long
  Range were heard by a C5 `node-ble`, and its 2.4 GHz NAN by a C5 `node-wifi` built `cargo fw24`
  (`s3-to-c5-wifi24-nan`).*

**C5:** see `docs/BENCH.md` for the Remote ID results. Fake open FPV links (2026-11-14): all four
systems, on channels 6, 149, 161 and 165, heard by a C5 `node-wifi` (16/16 PASS, 0 failed sends,
about −44 to −38 dBm at a metre).

## Limits and workarounds found

1. **bt-hci 0.10.1 bug.** This bug breaks every extended-advertising data command.
   - `impl WriteHci for &[u8]` writes the length prefix as `len + 1` in its *async* encoder.
     esp-radio's transport uses that encoder.
   - Every LE Set Extended Advertising Data command therefore fails with "Invalid HCI Command
     Parameters". The parameters, enable and clear commands all succeed, because they carry
     no slice.
   - It is fixed in bt-hci 0.11, but trouble-host 0.8 pins 0.10.
   - `src/hci_fix.rs` wraps the transport and encodes each packet with the correct blocking
     encoder. Remove it when trouble-host moves to bt-hci ≥ 0.11.
   - `node-ble` only scans, so it should not hit this. Anything else here that *advertises*
     through trouble-host 0.8 will.
2. **Legacy and extended HCI commands cannot be mixed.** Once any extended advertising
   command has been sent, the controller answers legacy advertising commands with "Command
   Disallowed".
   - That is why BLE 4 is sent as a legacy-PDU set through the extended API, alongside the
     other two sets.
3. **The ESP32-C5 controller defaults to 1 advertising set and 31-byte extended data.** The C5
   build raises these to 4 sets (`multi_adv_instances`) and 251 bytes (`ext_adv_max_size`).
   This is untested until a C5 is on the bench. *Update 2026-11-18: tested on 2026-11-12: a C5's
   BLE 5 extended and Long Range were heard by a second C5 and by the S3 (docs/BENCH.md).*
4. **Wi-Fi and BLE share one radio (`coex`).** Wi-Fi uses modem-sleep power save (as ESP-IDF
   requires with Bluetooth on), so the coexistence arbiter has idle windows to give to BLE.
5. **esp-radio beta.2 is yanked**, so this pins `=1.0.0-beta.1`.


## Bench tests

Two-board tests that flash and judge this firmware: `tools/bench/run.py --list` (see `docs/BENCH.md`). Pick a transport on the beacon with the console keys above; the runner types them for you.
