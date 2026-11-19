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
- The link between boards: a captured node gives away that node's own key
  (revoke its slot with `link-key revoke`); the hub holds the kit master, so
  a captured hub is a captured kit until a secure element is fitted; the
  gateway holds no key. The wired bus between boards is not authenticated.
  Details: docs/ESPNOW.md, "Securing the link".
- The fix for CVE-2025-52471 in Espressif's ESP-NOW receive path is
  established by provenance: the radio libraries every firmware links are the
  ones Espressif shipped after the fix (`tools/check-wifi-blobs.sh` checks
  their hashes), and every radio firmware also drops impossible receive
  lengths itself (firmware/patches/README.md). It has not been tested with
  malformed frames.
- Jamming a receiver stops it receiving.
