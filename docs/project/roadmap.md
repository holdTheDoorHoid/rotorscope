# Roadmap

**Level: 🟢 Beginner** · What has recently been finished, what is being worked on, and what comes
next, in plain words and with no promised dates.

The full engineering plan, phase by phase, is
[PLAN.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/PLAN.md). For exactly how far each
piece has been proven today, see [Status](status.md).

```mermaid
flowchart LR
    a[Bench prototype] --> b[Wired kit on the bench]
    b --> c[Tests against real drones]
    c --> d[Kit hardware and build guide]
    d --> e[Kits ship open]
```

## Recently landed

The project started on 2026-11-09, so this is nearly all of it. Everything below is on `main`.

- **2026-11-09.** The Remote ID decoder, Wi-Fi and Bluetooth listener firmware, the test
  transmitter, the laptop viewer, the first hub (an ESP32-S3 with a phone page), the DJI DroneID
  decoder on a computer, recognition of open-source digital FPV links, and ADSBee support.
  Licensed GPL-3.0-or-later.
- **2026-11-11.** A laptop can listen for Bluetooth Remote ID itself, and a cheap Sonoff USB
  dongle can be turned into a Bluetooth 5 receiver.
- **2026-11-12.** The ESP32-C5 passed every listening job, including 5 GHz Wi-Fi and Bluetooth
  Long Range, so every node can be one kind of board. The C5 first captured raw radio samples.
- **2026-11-14.** The new hub on an ESP32-P4 touch-screen board; the ESP-NOW gateway; the hub
  telling each node what to listen to; the first test drone shown end to end; a real ADSBee
  showed real aircraft.
- **2026-11-15.** GPS and ADS-B on the P4 (emulated), the web head firmware (not yet run with the
  P4), FAA rule flags with an offline airspace map, the evidence log, DJI radio fingerprints, and
  Rotorscope's own ADS-B decoder.
- **2026-11-16.** DJI O4 decoding (and decryption with a key the owner supplies), DJI radio
  fingerprints tuned against public recordings, and output to WarDragon / DragonSync.
- **2026-11-17.** A signed link between boards; alarms for Wi-Fi attacks, jamming, spoofed drones
  and GPS spoofing; the wired bus (built, not yet wired); the LR2021 aero node and decoders for UAT, OGN and
  FLARM; control-link detection; attacks on drones; a warning when a drone is near a crewed
  aircraft.
- **2026-11-18.** Security hardening: every board reboots itself after a crash, and all but the
  test transmitter after a hang; frames a
  board may not send are refused; builds refuse to go out without a link key or with test
  features switched on by accident; a hotspot password and request checks for the phone page;
  defences against floods of fake drones and against a rival claiming a real drone's identity;
  stricter trust in GPS time; a stricter evidence verifier; a [security policy](security.md).

## In progress

These are being worked on but are **not on `main` and not yet available**.

- **Sound detection** (not yet available). A microphone on the hub listens for the sound of
  rotors, to notice drones that send no radio signal the kit can read. It works on recordings and
  with simulated sound on the chip; it is not yet proven with a real drone.
- **Stronger link keys** (not yet available). Each node gets its own key derived from a kit
  master key, the hub checks everything, and the gateway holds no secret. The design is accepted
  and being built.
- **Evidence hardening** (not yet available). Threat episodes, boot continuity and build
  identity recorded in the log, and a tamper-resistant secure-element chip on the hub to hold the
  signing key, to make the log stronger evidence. In progress; the hardware is not yet in hand.
- **Secure boot and flash encryption trial** (not yet available). Not started.
- **Sealing your own unit** (not yet available). Kits are intended to ship open, with a tool the
  buyer runs to seal their own unit.

## Next

- **Wire the nodes to the hub** (PLAN.md Phase 5a). The wired bus is built and simulated; it now
  needs wiring and its bench checklist, including measuring how much listening time it wins back
  over ESP-NOW. See [the wired bus in depth](../WIREBUS.md).
- **A phone page for the P4 kit.** The web head firmware first has to fit in memory again (see
  [Status](status.md#known-gaps)), then be wired to the P4 and tested.
- **A real GPS and the real ADSBee on the P4**, replacing the chip's own emulation.
- **LR2021 hardware** (Phase 9 and 14b/14d). The control-link and aero node firmware is waiting for
  an LR2021 board, then a bench test against real ExpressLRS, mLRS and SiK transmitters.
- **Real drones.** Live tests against real Remote ID, real DJI drones (a used Mini 2 is the
  cheapest reference, props off and motors armed, or a visit to a flying field) and real FPV
  equipment. Also the first measured range figures.

## Later

- **Kit hardware** (Phase 5): a carrier board, antennas, battery, case, a browser flasher that
  updates every chip, and a build guide. Done when someone else builds one from the guide.
- **LoRa mesh backhaul** (Phase 8): several units share detections through a stock Meshtastic or
  MeshCore board (with an SX1262 LoRa radio) used as a modem, never on the same radio that listens.
- **Other drone links** (Phase 11): telling Walksnail from DJI FPV and Autel from DJI, or picking
  out Skydio, needs recordings nobody has published yet; the HDZero check has run only on made-up signals.
- **More of the evidence plan** (Phase 13): automatic outside time-stamps, upload through the web
  head, retention settings and real server accounts.
- **More aircraft and FPV coverage**: an RX5808 receiver for video channels above 5810 MHz
  (Phase 7), and optionally an RTL-SDR for raw aircraft signals (Phase 14c).
- **Fixed-sensor outputs** such as MQTT and Home Assistant (requirement O5).
- **Later ideas** from the plan: an nRF24 radio for toy-drone protocols if Wi-Fi matching proves
  not enough; an RTL-SDR waterfall beside the laptop viewer; time sync across nodes with the GPS
  pulse; the P4 board's own Wi-Fi chip for the phone page instead of a separate web head.

## Open questions

- **Legal review before kits are sold**, especially for showing DJI DroneID by default and
  decoding FLARM content. See
  [Law, safety and ethics](../start/law-and-ethics.md).
- **Some FAA line-of-sight defaults** rest on two studies seen only in summary.
- **DroneID altitude units** (feet or metres) are not settled from the public recordings.

Next: [FAQ](faq.md)
