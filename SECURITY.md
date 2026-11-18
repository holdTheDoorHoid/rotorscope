# Security policy

Rotorscope is pre-release software. There are no tagged releases yet; the only
supported code is the tip of `main`.

## Reporting a vulnerability

Please report security problems privately, not in a public issue:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**
   (<https://github.com/holdTheDoorHoid/rotorscope/security/advisories/new>).

A useful report says which board and firmware it concerns (the commit from
`git rev-parse --short HEAD`), what you sent or did, what happened, and what
you expected to happen. A capture file or a short script that reproduces it
helps a great deal.

This is a small project. Expect a first reply within about two weeks. Once a
fix has landed you are welcome to publish your findings, and you will be
credited unless you ask not to be.

## What counts

In scope: anything in this repository — the firmware for the nodes, gateway,
hub and web head, the shared crates, and the host tools. Examples:

- radio input that crashes, hangs or blinds a board;
- forged or replayed traffic accepted on the link between boards;
- problems in the hub's hotspot or phone page;
- ways to alter the hub's logs without the checker noticing.

## Known limits

These are properties of the signals and hardware, not bugs to report:

- Remote ID, DroneID, ADS-B, FLARM and the control links Rotorscope listens to
  carry no authentication. Anyone can broadcast a forged one. Rotorscope shows
  what was claimed on the air; it cannot prove who sent it.
- Someone holding a board and a USB cable can read or replace its firmware.
  Secure boot and flash encryption are not enabled.
- Jamming a receiver stops it receiving.
