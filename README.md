# Rotorscope

An affordable, receive-only detector for drones that broadcast Remote ID, built from several
ESP32 chips with firmware written in Rust. See [PLAN.md](PLAN.md) for requirements and phases,
and [RESEARCH.md](RESEARCH.md) for the survey behind them.

## Status (2026-11-09)

| Piece | Where | State |
|---|---|---|
| Remote ID decoder and encoder | `crates/rid` | Done. Matches the official opendroneid-core-c field for field across hundreds of thousands of random cases, and survives millions of garbage frames. Also decodes DJI legacy Wi-Fi DroneID and GB 46750 (China). |
| Node-to-hub wire format | `crates/link` | Done (postcard + COBS over UART). |
| Hub track logic | `crates/tracker` | Done: merges by UAS ID across radios (2.4 and 5 GHz Wi-Fi kept apart), Vincenty range/bearing, JSON / TAK CoT / CSV output. Runs on the hub. |
| Hub firmware | `firmware/hub` | S3 runs on the bench: node links, hotspot + phone page, CoT, USB, plus GPS (I2C, PPS), banded 320x240 screen, buttons, buzzer, SD CSV log and battery on the second core. With nothing attached each reports absent and the rest runs; untested against a real GPS, panel or card. |
| Wi-Fi listener (2.4 GHz S3, 5 GHz C5) | `firmware/node-wifi` | S3 verified on hardware: sniffs and hops channels for 60 s with no drops. C5 builds only. |
| Bluetooth listener (legacy + Long Range) | `firmware/node-ble` | S3 verified: decoded a BT4 Remote ID broadcast from a laptop. C5 builds only. Long Range not yet heard on air. |
| Test transmitter | `firmware/rid-beacon` | S3 verified: the laptop received the Wi-Fi beacon and BT4 adverts. NAN, BT5 extended and Long Range transmit with no errors but haven't been heard by a receiver yet. C5 (5 GHz) builds only. |
| Laptop viewer | `tools/rid-view` | Done. Reads node USB output, files or stdin; can send TAK CoT; has `--demo`. |

**Verified end to end:** laptop Bluetooth broadcasting a test Remote ID → S3 running
`node-ble` → `rid-view` showing the drone.

**Next, once the second S3 and the C5s arrive:** run `rid-beacon` on one board and the
listeners on another. That proves Wi-Fi NAN, BT5 Long Range, and (on the C5) 5 GHz.

## Building

Host crates and tools:

```bash
cargo test
```

```bash
cargo run --release -p rid-view -- --demo
```

Firmware: each `firmware/*` folder is its own project with a README. The S3 is an Xtensa
chip, so it needs Espressif's toolchain (`espup install`, then `. ~/export-esp.sh`). The C5
builds on stable Rust.

Flash and watch a board with `tools/dev-board.sh <elf> [seconds]`. It holds a lock so that
parallel agents take turns.

## Conduct

This project only listens. `rid-beacon` exists purely to test the receivers on the bench. It
uses an obviously fake identity and the lowest transmit power, and must not be used to
impersonate an aircraft.

## License

Not yet chosen (see PLAN.md, open question 2). `third_party/opendroneid-core-c` is
Apache-2.0 and is used only by host-side tests.
