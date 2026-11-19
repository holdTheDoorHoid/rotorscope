# Patched crates

## `esp-radio` (1.0.0-beta.1, used by every radio firmware)

Carried by `gateway`, `node-wifi`, `node-ble`, `node-sdr`, `node-lr`, `hub` (the S3 web head)
and `rid-beacon`, each through a `[patch.crates-io]` entry in its `Cargo.toml`. The P4 hub
(`hub-p4`) has no radio and does not use esp-radio.

A verbatim copy of the crates.io release with **two files changed**:
`src/radio_clocks/clocks_ll/esp32c5.rs` and `src/esp_now/mod.rs`. Check with

```sh
tools/check-wifi-blobs.sh            # also checks the radio library blobs, below
# or by hand:
diff -r ~/.cargo/registry/src/index.crates.io-*/esp-radio-1.0.0-beta.1 firmware/patches/esp-radio
```

(only those two files, plus the registry's `.cargo-ok`, `.cargo_vcs_info.json` and `Cargo.lock`,
and the three files Rotorscope adds, differ). Every changed spot is marked `ROTORSCOPE PATCH`.

The release ships no licence files although it declares `MIT OR Apache-2.0`, so the copy adds
`LICENSE-MIT` and `LICENSE-APACHE` (esp-hal's own, the repository the crate names) and a
`PROVENANCE.md` with the version, checksum, upstream commit and the two changes.

How the copy was made, so anyone can make it again: copy
`~/.cargo/registry/src/index.crates.io-*/esp-radio-1.0.0-beta.1` (the crate whose checksum is
`50247f4e91000fc87661e77254126c74acc4535f822ee247f7778807202dc938` in the lockfiles before the
patch) to `firmware/patches/esp-radio`, delete `.cargo-ok`, `.cargo_vcs_info.json` and
`Cargo.lock`, add the three files above, and apply the two changes below. `tools/check-wifi-blobs.sh` fails if anything
else differs from the registry copy, or if the registry copy is missing.

### 1. ESP-NOW receive length (CVE-2025-52471, defence in depth)

`src/esp_now/mod.rs`, `rcv_cb`. Upstream turns the blob's `data_len: i32` into a slice length
with `as usize` and no check, then copies that many bytes into a heap box. The bug fixed in
the Wi-Fi libraries (an integer underflow in the ESP-NOW receive path; advisory
GHSA-hqhh-cp47-fv5g, fixed in ESP-IDF v5.4.2 / v5.3.4 / v5.2.6 / v5.1.7) is in the blob, and
the blobs this repository pins already contain that fix (docs/LINK-KEYS-DESIGN.md section 8).
This check is the second line: a zero, negative or oversized length from any future blob bug
(or a null pointer) is dropped before it becomes a slice, and counted.

```rust
if esp_now_info.is_null() || data.is_null() || data_len <= 0 || data_len as usize > ESP_NOW_MAX_DATA_LEN_V2 {
    BAD_RX_LEN.store(BAD_RX_LEN.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    return;
}
```

`ESP_NOW_MAX_DATA_LEN_V2` is 1470; Rotorscope's packets are at most 1024 bytes
(`airlink::MAX_PACKET`). The count is `esp_radio::esp_now::bad_rx_len_drops()`; it should stay
zero. Not testable on the bench without a raw 802.11 transmitter sending malformed ESP-NOW
frames, which this project does not use; it is compile-checked on every radio firmware.

### 2. ESP32-C5 RF front-end clocks

**Bug.** On the ESP32-C5 the RF front-end ("FE") clocks in `MODEM_SYSCON.clk_conf1`
(`clk_fe_apb_en`, `clk_fe_80m_en`, `clk_fe_160m_en`) are shared by Wi-Fi, BLE and 802.15.4.
ESP-IDF reference-counts them (`modem_clock.c`: `MODEM_CLOCK_FE` is in the Wi-Fi, BLE and
802.15.4 clock-dependency sets). Upstream esp-radio's `enable_wifi(false)` clears them
unconditionally, and the Wi-Fi blob calls it (`wifi_clock_disable`) whenever Wi-Fi goes idle.
With Wi-Fi started, the FE clocks are therefore off almost all the time, and the BLE
controller accepts every HCI command and reports advertising as running, but nothing is
transmitted. Without Wi-Fi it works, which is why BLE-only firmware (`node-ble`) was fine.

**Fix.** A three-bit "who needs FE" mask (Wi-Fi, BT, 802.15.4). Each `enable_*` updates its
bit and gates the FE clocks only when no user is left. The per-protocol clocks are untouched.
A firmware that uses only one of the radios behaves exactly as with upstream.

**Evidence (2026-11-12, XIAO ESP32-C5 v1.0 `c5a`).** `clk_conf1` read back `0x3f19fb`
(FE bits 13-15 clear) with Wi-Fi started, even after `esp_wifi_stop`, against `0x3fffff`
with Wi-Fi never initialised. With the patch it reads `0x3ff9fb` and the laptop
(BlueZ, `rid-view --ble hci0`) decodes TESTDRONE0000000001 while Wi-Fi beacons run.

Since the cut-over (WP-13i) this also reaches the C5 firmware that runs Wi-Fi and BLE
together: `node-ble` built with `espnow` (BLE scanning plus the ESP-NOW uplink). Bench check:
docs/BENCH.md, "Link keys: bench checklist".

### When to remove

Change 2 can go once upstream esp-radio carries an equivalent fix (the ESP32-C6 and C61
`enable_wifi` look to have the same pattern but are untested here). Change 1 stays as long as
upstream's receive callback trusts the length. On any esp-radio or `esp-wifi-sys-*` bump:
make the copy again from the new release, re-apply what is still needed, and record the new
blob hashes in `tools/check-wifi-blobs.sh` only after checking their provenance (design
section 8).
