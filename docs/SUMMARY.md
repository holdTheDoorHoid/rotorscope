# Summary

[Welcome](README.md)

# Start here

- [What is Rotorscope?](start/what-is-rotorscope.md)
- [How drones give themselves away](start/drone-signals-101.md)
- [What it can and cannot do today](start/capabilities.md)
- [Law, safety and ethics](start/law-and-ethics.md)

# Get going

- [Choose your path](get-going/README.md)
- [Step 1: The demo, no hardware](get-going/step-1-demo.md)
- [Step 2: One board, a real receiver](get-going/step-2-one-board.md)
- [Step 3: The full kit](get-going/step-3-full-kit.md)
  - [Parts and tools](get-going/kit-parts.md)
  - [Wiring](get-going/kit-wiring.md)
  - [Build and flash](get-going/kit-flash.md)
  - [First power-up](get-going/kit-first-run.md)
- [Troubleshooting](get-going/troubleshooting.md)

# Using Rotorscope

- [The screen](use/screen.md)
- [Reading a detection](use/reading-detections.md)
- [Alarms and warnings](use/alarms.md)
- [FAA rule flags](use/faa-flags.md)
- [Crewed aircraft](use/aircraft.md)
- [Phone page and TAK](use/phone-and-tak.md)
- [The laptop viewer, rid-view](use/rid-view.md)
- [SD card and logs](use/sd-card.md)
- [Site settings (SITE.TXT)](SITE.md)
- [The evidence log](EVIDENCE.md)

# How it works

- [The big picture](how/architecture.md)
- [Nodes, gateway and hub](how/nodes-gateway-hub.md)
- [The link between boards](how/link.md)
  - [ESP-NOW in depth](ESPNOW.md)
  - [The wired bus in depth](WIREBUS.md)
- [The tracker](how/tracker.md)
- [Who listens where: coverage](how/coverage.md)
- [Position and time](how/position-and-time.md)

# What it detects

- [Signal catalogue](SIGNALS.md)
- [Remote ID](detect/remote-id.md)
- [DJI DroneID](detect/dji-droneid.md)
  - [O4 DroneID in depth](DRONEID-O4.md)
  - [DJI radio links in depth](DJI-LINKS.md)
- [FPV video links](detect/fpv.md)
- [Control links](CONTROL-LINKS.md)
- [Crewed-aircraft signals](detect/aircraft-signals.md)
  - [ADSBee 1090](ADSBEE.md)
  - [Beyond 1090 MHz: UAT and FLARM](AIRCRAFT-SIGNALS.md)
  - [The aero node](AERO-NODE.md)
  - [Drone near a crewed aircraft](AIRCRAFT-CONFLICT.md)
- [Attacks and interference](detect/threats.md)
  - [Spoofed drones](SPOOFING.md)
  - [Wi-Fi and Bluetooth attacks](WIFI-ATTACKS.md)
  - [Jamming](JAMMING.md)
  - [Attacks on drones](DRONE-ATTACKS.md)
  - [GPS jamming and spoofing at the hub](GNSS-INTEGRITY.md)

# Hardware

- [Boards and chips](hardware/boards.md)
- [Pins and wiring reference](hardware/pinouts.md)
- [A Sonoff dongle as a Bluetooth 5 receiver](SONOFF.md)

# Developer guide

- [A tour of the repository](dev/repo-tour.md)
- [Set up your machine](dev/setup.md)
- [Build, test and flash](dev/build-test-flash.md)
- [Crate reference](dev/crates.md)
- [Firmware reference](dev/firmware.md)
- [Tools reference](dev/tools.md)
- [Wire protocol reference](dev/protocol.md)
- [Bench tests](BENCH.md)
- [Evidence log format](EVIDENCE-FORMAT.md)
- [Hard-won lessons](dev/gotchas.md)
- [Contributing](dev/contributing.md)
- [Writing these docs](dev/docs-style.md)

# Project

- [Status](project/status.md)
- [Roadmap](project/roadmap.md)
- [FAQ](project/faq.md)
- [Glossary](project/glossary.md)
- [Credits and licences](project/credits.md)
- [Security policy](project/security.md)
