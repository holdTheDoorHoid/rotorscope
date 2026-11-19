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

The link between boards has limits of its own, recorded in
[ESP-NOW in depth](../ESPNOW.md#what-a-capture-gives-away): a captured node gives away that node's
key (revoke its slot); the hub holds the kit master, so **a captured hub is a captured kit** until a
secure element is fitted; the wired bus is not authenticated; and the fix for CVE-2025-52471 in
Espressif's radio libraries is established by where the libraries come from
(`tools/check-wifi-blobs.sh` checks their hashes), with a patched esp-radio in every radio firmware
as a second line (`firmware/patches/README.md`).

## How Rotorscope protects itself

Each of these is described in full on the page it links to, and each says how far it has been
proven, in the words of [Status](status.md).

- **A signed link between boards.** On a sealed kit every ESP-NOW packet between a node and the
  hub carries a tag made with that node's own key, and forged, replayed or late packets are
  dropped and reported; the gateway in between holds no key. Kits ship open and are sealed with
  `link-key`. Before the 2026-11-18 cut-over, with one key for the whole kit, this was **proven on
  the air** (the bench kit ran keyed builds); the per-node design is **simulated only**
  (tested on a computer and built for the boards). See [ESP-NOW in depth](../ESPNOW.md#securing-the-link).
- **Forged frames refused.** Each kind of report may only come from the kind of board allowed to
  send it; the hub refuses the rest, counts them and raises an alarm, and the web head checks
  again. **Proven on the air** before the cut-over: on 2026-11-18 seven kinds of forbidden frame
  sent by a test node were all refused at the gateway, which did this check then. See
  [ESP-NOW in depth](../ESPNOW.md) and [Wire protocol reference](../dev/protocol.md).
- **The kit's keys never in the repository or in a firmware image.** The kit master lives in
  `tools/kit.key` on the owner's computer and never goes into git; each node's key is written to
  its own flash by `link-key`, and no firmware build reads a key, so every image is the same for
  every kit (`tools/check-image-identity.sh` checks this). Owner-supplied keys for DJI O4 and FLARM
  stay on the SD card. See [ESP-NOW in depth](../ESPNOW.md) and [Credits and licences](credits.md).
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
