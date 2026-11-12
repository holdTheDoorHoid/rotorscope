# Patched crates

## `esp-radio` (1.0.0-beta.1, used by `firmware/rid-beacon` only)

A verbatim copy of the crates.io release with **one file changed**:
`src/radio_clocks/clocks_ll/esp32c5.rs`. Check with

```sh
diff -r ~/.cargo/registry/src/index.crates.io-*/esp-radio-1.0.0-beta.1 firmware/patches/esp-radio
```

(only that file, plus the registry's `.cargo-ok`, `.cargo_vcs_info.json` and `Cargo.lock`, differ).

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

**Evidence (2026-11-12, XIAO ESP32-C5 v1.0 `c5a`).** `clk_conf1` read back `0x3f19fb`
(FE bits 13-15 clear) with Wi-Fi started, even after `esp_wifi_stop`, against `0x3fffff`
with Wi-Fi never initialised. With the patch it reads `0x3ff9fb` and the laptop
(BlueZ, `rid-view --ble hci0`) decodes TESTDRONE0000000001 while Wi-Fi beacons run.

**Remove** this directory and the `[patch.crates-io]` entry in `rid-beacon/Cargo.toml` once
upstream esp-radio carries an equivalent fix (the ESP32-C6 and C61 `enable_wifi` look to have
the same pattern but are untested here).
