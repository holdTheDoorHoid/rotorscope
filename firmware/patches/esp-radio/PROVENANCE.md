# Provenance of this copy of esp-radio

This file, and the two licence files beside it, are Rotorscope's additions; everything else is
the crates.io release with two files changed. The full description and the reasons are in
[`../README.md`](../README.md).

| | |
|---|---|
| Crate | `esp-radio` 1.0.0-beta.1, from crates.io |
| crates.io checksum (SHA-256 of the `.crate`) | `50247f4e91000fc87661e77254126c74acc4535f822ee247f7778807202dc938` |
| Upstream source | `https://github.com/esp-rs/esp-hal`, folder `esp-radio`, commit `f769fcd27056b0837febe4209f3bae78ed764561` (from the release's `.cargo_vcs_info.json`) |
| Licence | MIT OR Apache-2.0 (the crate's `Cargo.toml`) |
| Changed file 1 | `src/esp_now/mod.rs`: the ESP-NOW receive length check (CVE-2025-52471, defence in depth) |
| Changed file 2 | `src/radio_clocks/clocks_ll/esp32c5.rs`: the ESP32-C5 RF front-end clocks shared by Wi-Fi and Bluetooth |
| Removed from the release | `.cargo_vcs_info.json`, `Cargo.lock` (and the registry's `.cargo-ok`) |

Every changed spot is marked `ROTORSCOPE PATCH`. `tools/check-wifi-blobs.sh` unpacks the release
from the local cargo cache, checks its SHA-256 against the value above, and fails if this folder
differs from it anywhere else.

## The licence texts

The release ships no licence files, though its `Cargo.toml` declares `MIT OR Apache-2.0`.
`LICENSE-MIT` and `LICENSE-APACHE` here are the esp-hal repository's own licence files, the
repository the crate names, taken byte for byte from esp-hal commit
`e02f3613e9f9ba1ce00070eb387e3bf4fde2267b` (the commit `tools/p4-prepare.sh` pins for the P4
hub; the same two files are in `../esp-hal-p4-pre-v3/`).

| File | SHA-256 |
|---|---|
| `LICENSE-MIT` | `7b2165c6740592038d887d51d56d122b0b563337a7756b424f19eecc7a936ef1` |
| `LICENSE-APACHE` | `2bf4fe1a37e545c3b4f7bfaa2326c99f153af32ef8d112fbaa4bff7fb2a575bd` |

Not checked: that these two files were byte-identical at commit `f769fcd`, the commit the release
was made from. That needs a network copy of esp-hal.
