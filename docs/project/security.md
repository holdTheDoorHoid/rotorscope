# Security policy

**Level: 🟢 Beginner** · How to report a security problem privately, what counts, and how
Rotorscope protects itself today.

The full policy is
[SECURITY.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/SECURITY.md). This page
summarises it.

## Pre-release software

Rotorscope has no tagged releases yet. The only supported code is the tip of the `main` branch.

## How to report a problem

Please report security problems **privately**, not in a public issue:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**
   (<https://github.com/holdTheDoorHoid/rotorscope/security/advisories/new>).

A useful report says:

- which board and firmware it concerns, with the commit from `git rev-parse --short HEAD`;
- what you sent or did;
- what happened, and what you expected to happen.

A capture file or a short script that reproduces it helps a great deal. This is a small project:
expect a first reply within about two weeks. Once a fix has landed you are welcome to publish your
findings, and you will be credited unless you ask not to be.

## What is in scope

Anything in the repository: the firmware for the nodes, gateway, hub and web head, the shared
crates, and the host tools. For example:

- radio input that crashes, hangs or blinds a board;
- forged or replayed traffic accepted on the link between boards;
- problems in the hub's hotspot or phone page;
- ways to alter the hub's logs without the checker noticing.

## Known limits, not bugs

SECURITY.md lists these as properties of the signals and hardware rather than things to report:

- Remote ID, DroneID, ADS-B, FLARM and the control links carry no authentication, so anyone can
  broadcast a forged one. Rotorscope shows what was claimed on the air; it cannot prove who sent
  it.
- Someone holding a board and a USB cable can read or replace its firmware: secure boot and flash
  encryption are not enabled.
- Jamming a receiver stops it receiving.

## How Rotorscope protects itself

Each of these is described in full on the page it links to, and each says how far it has been
proven, in the words of [Status](status.md).

- **A signed link between boards.** Every ESP-NOW packet between a node and the gateway carries a
  tag made with the kit's own key, and forged or replayed packets are dropped and reported.
  **Proven on the air**: the bench kit runs keyed builds (2026-11-18). See
  [ESP-NOW in depth](../ESPNOW.md).
- **Forged frames refused at the gateway.** Each kind of report may only come from the kind of
  board allowed to send it; the gateway refuses the rest and counts them, and the hub checks again.
  **Proven on the air**: on 2026-11-18 seven kinds of forbidden frame sent by a test node were all
  refused at the gateway. See [ESP-NOW in depth](../ESPNOW.md) and
  [Wire protocol reference](../dev/protocol.md).
- **The kit's keys never in the repository.** The kit's link key lives in `tools/link.key` on the
  builder's computer and never goes into git; firmware builds without a key are refused unless
  deliberately made open (the flashing tools' refusal was checked on 2026-11-18). Owner-supplied
  keys for DJI O4 and FLARM stay on the SD card. See [ESP-NOW in depth](../ESPNOW.md) and
  [Credits and licences](credits.md).
- **Bench features cannot slip into a real build.** Builds that fake drones, floods, crashes or GPS
  are refused unless `ROTORSCOPE_BENCH=1` is set (checked 2026-11-18). A bench build names itself
  in the evidence log (seen on 2026-11-18) and on screen (not yet checked by eye). See
  [Bench tests](../BENCH.md).
- **Watchdogs.** Every board reboots itself after a crash, and every board except the test
  transmitter has a hang watchdog that restarts it if its main work stops. **Proven on the air**
  for the P4 hub, the gateway and the C5 Wi-Fi and raw-capture nodes (2026-11-18); **not yet
  checked** on the Bluetooth node, the LR2021 node or any S3 board. See [Bench tests](../BENCH.md).
- **A tamper-evident evidence log.** Each record carries the fingerprint of the one before, and the
  hub signs the chain every minute, so changed, reordered or removed records in the middle show up
  in the verifier. **Proven on the air**, but not evidence-grade: the hub signs with the published
  **test key** unless a **development key** is supplied when the firmware is built, and every
  checkpoint and the screen say which. A development key shows records have not changed since they
  were signed, not which unit signed them. The verifier counts a log signed with the published
  test key as a failure unless told it is a test. See [The evidence log](../EVIDENCE.md).
- **Alarms are claims, not commands.** A single over-the-air frame never confirms a drone, floods
  of fake identities raise their own alarm, and a rival claiming a real drone's identity marks it
  DISPUTED. **Simulated only**, with floods of made-up drones injected on the bench. See
  [Spoofed drones](../SPOOFING.md).

For how far each of these has been tested on hardware, see [Status](status.md).
