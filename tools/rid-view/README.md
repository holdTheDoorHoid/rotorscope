# rid-view

Laptop viewer: a live table of Remote ID drones from Rotorscope nodes (their USB JSON lines),
recorded files, stdin, a laptop Bluetooth adapter, an ADSBee, or `--demo`. It runs the same
`tracker` code as the hub. `rid-view --help` lists every option.

## Laptop Bluetooth as a Remote ID sensor (`--ble`)

Any Linux laptop adapter can listen for Bluetooth Remote ID through BlueZ. It needs no root and
no special group, because it talks to BlueZ over D-Bus (pure-Rust `zbus`, so no libdbus either).

```bash
cargo run --release -p rid-view -- --ble hci1 --own-pos 39.9526,-75.1652
cargo run --release -p rid-view -- --ble hci1 --once --duration 20   # one table after 20 s
```

- `--ble` asks BlueZ for LE discovery with `DuplicateData` on, and reads each advert's `0xFFFA`
  service data (app code `0x0D`). The advert goes through `rid::ble::extract`, the parser
  `node-ble` uses. It reaches the tracker as transport `BleLegacy`, with RSSI, address and
  counter.
- **BT4 only.** A Bluetooth 4.x adapter hears legacy adverts only, not BT5 extended adverts or
  Long Range (coded PHY). Both laptop adapters here are BT4: the internal Intel (`hci0`) and
  the Sena Parani UD100 (`hci1`, CSR8510, class 1, external antenna). Use a `node-ble` board
  for BT5.
- It combines with node inputs, `--adsbee` and `--demo`. Repeat `--ble` to use several
  adapters; each one counts as a node.
- **BlueZ discovery is an active scan.** The adapter sends scan requests to scannable
  devices nearby. Remote ID adverts are non-scannable, so a drone never gets one. Nothing is
  connected or paired.
- The node line shows BlueZ's per-second counts: `adv` is advert events from BlueZ, `rid` is
  Remote ID adverts, `devices` is how many devices have been heard.
- Errors spell out what is wrong: an adapter BlueZ doesn't know (with the names it does
  know), an adapter that is powered off (with how to turn it on), or BlueZ not running.
- A BlueZ device property only changes when the advert's content changes. Repeats of the
  same advert are therefore not counted. Every Remote ID advert carries a new counter, so
  none of those are lost.

## Bluetooth 5 with a Sonoff dongle (`--sniffle`)

A Sonoff Zigbee 3.0 USB Dongle Plus (CC2652P) flashed with NCC Group's Sniffle
firmware hears what a BT4 laptop adapter cannot: BT5 extended adverts and Long
Range (coded PHY). `docs/SONOFF.md` has the flashing steps (with
`tools/cc26xx-flash`) and the restore.

```bash
cargo run --release -p rid-view -- --sniffle /dev/serial/by-id/usb-ITead_Sonoff_Zigbee_3.0_USB_Dongle_Plus_...-if00-port0
cargo run --release -p rid-view -- --sniffle "$PORT" --sniffle-mode coded   # Long Range
```

- Our own host for Sniffle's serial protocol (`src/sniffle.rs`): base64 lines
  at 2 Mbaud. It sets up a **passive** capture on primary channel 37 that
  follows aux pointers, with no MAC filter and CRC checking on, and checks the
  link with a marker echo. It never sends a scan, advertise or connect
  command.
- Each packet comes with its channel, PHY, PDU type and RSSI, so the transport
  is exact. Legacy PDUs on 1M are `BleLegacy` (`B4`). AUX_ADV_IND on 1M or 2M
  is `BleExtended` (`B5`). Coded PHY, or anything heard while listening on a
  coded primary, is `BleLongRange` (`LR`). Remote ID service data goes through
  `rid::ble::extract`. An AUX_ADV_IND without AdvA takes the address its
  ADV_EXT_IND announced, matched by AdvDataInfo.
- **Modes** (`--sniffle-mode`): `1m` (default; legacy + extended), `coded` (Long
  Range only), `alt` (switches every 3 s). One radio does one primary PHY at a
  time; use two dongles to cover both continuously.
- The node line shows `adv` (all adverts heard per second), `rid` (Remote ID
  ones), `phy`, and the firmware version from the handshake.
- **Measured 2026-11-11:** `rid-ble-beacon hci0` → Sonoff, about 7.2 Remote ID
  adverts per second (217 in 30 s), RSSI −37 to −40 dBm, every field decoded,
  all labelled `B4`. Extended and Long Range decode is unit-tested with
  constructed packets only; nothing here transmits BT5 (`hci0` is BT 4.2,
  `hci1` BT 4.0).

## Laptop test transmitter (`tools/rid-ble-beacon`)

`rid-ble-beacon` sends Bluetooth Remote ID test adverts from the laptop. It is for the
**bench only**, under the same conduct rules as `firmware/rid-beacon`.

```bash
cargo run --release -p rid-ble-beacon -- hci0 --secs 60
RID_TEST_LAT=40.0 RID_TEST_LON=-75.2 cargo run --release -p rid-ble-beacon -- hci0
```

- **What it sends.** It registers a BlueZ "broadcast" (non-connectable) LE advertisement
  carrying the firmware beacon's obviously fake drone: `TESTDRONE0000000001` /
  `TEST-OPERATOR` / `BENCH TEST-NOT AIRCRAFT`. The drone flies a 50 m circle around
  `RID_TEST_LAT`/`RID_TEST_LON`; this tool reads them when it runs, while the firmware reads
  them at build time. The default centre is Philadelphia City Hall. `rid_tx.rs` and
  `flight.rs` are compiled in from `firmware/rid-beacon`, so the two transmitters cannot
  drift apart.
- **How often.** It cycles Basic ID, Location, Self-ID, System and Operator ID like a BT4
  broadcaster, changing the message every 500 ms (`--interval-ms`) and adding one to the
  counter each time. The change is made in place, with a D-Bus `PropertiesChanged`.
- **Layout.** The advert is exactly 31 bytes: `1E 16 FA FF 0D <ctr> <25-byte message>`. There
  is no room for Flags, a name or TX power, so the advert has empty `Includes` and **no
  `Discoverable` property at all**. On BlueZ 5.72, even `Discoverable=false` makes the kernel
  set aside 3 bytes for Flags. The advert then counts as too long and BlueZ refuses it, logging
  "Advertising data too long". An adapter with BR/EDR disabled may do the same.
- **The address changes with every message (a BlueZ/kernel limit).** Each change makes the
  kernel restart advertising. For a non-connectable advert it then picks a fresh
  non-resolvable private address, and a BT4 controller has no per-set address to keep one.
  Receivers therefore see each message from a new address. Basic ID adverts all merge into one
  `TESTDRONE0000000001` track by UAS ID. Location, Self-ID, System and Operator ID adverts
  arrive with no ID beside them, so each shows up as its own short-lived track keyed by
  address. A real BT4 broadcaster keeps one address. The firmware `rid-beacon` keeps one too
  (`C2:52:49:44:00:53`), so use it to test merging across message types.
- **Measured 2026-11-11, both adapters in one laptop:**

  | Direction | Remote ID adverts received | RSSI |
  |---|---|---|
  | `hci0` (Intel) → `hci1` (UD100) | about 7.2 per second | −27 to −33 dBm |
  | `hci1` (UD100) → `hci0` (Intel) | about 6 per second | −41 to −47 dBm |

  At a 500 ms message interval that is about two new messages a second. BlueZ reports each
  repeat of the same message too (a 100 ms advertising interval), which is why the rates are
  higher. Every field decoded both ways.
- **TX power.** It asks for the minimum power only when BlueZ reports `CanSetTxPower`. BT4
  controllers send at a fixed power, so the tool prints that it cannot lower it.
- **Stopping.** Ctrl-C or `--secs` withdraws the advert. If the process dies, BlueZ drops the
  advert by itself.
