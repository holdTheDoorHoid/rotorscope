# Hard-won lessons

**Level: 🔴 Expert** · The traps this project has already fallen into, what they looked like, why
they happened and how they were fixed, so nobody has to find them again.

Every item here is recorded somewhere in the repository: a bring-up log, a README, a code comment
or a commit message, named under **Read more**. Dates are when the item was found. Board names
such as `c5b` are one bench's labels, not part numbers.

Each item has four parts: **Symptom** (what you see), **Cause**, **Fix** (or the rule that
follows), **Read more**.

## Crate versions and pins

**esp-radio is pinned to `=1.0.0-beta.1`.**
- Symptom: a dependency update pulls a newer esp-radio, or fails to.
- Cause: 1.0.0-beta.2 is yanked on crates.io (its only change fixed ESP32-S31 builds).
- Fix: every firmware that uses esp-radio (all but `hub-p4`) pins `esp-radio = "=1.0.0-beta.1"`.
  Do not loosen it without checking.
- Read more: `firmware/node-wifi/README.md`, `firmware/rid-beacon/README.md` ("Limits and workarounds found").

**ESP32-C5 Bluetooth sends nothing while Wi-Fi is started.**
- Symptom (2026-11-12): on a C5 `rid-beacon`, the Bluetooth controller accepts every command and
  reports advertising as running, but nothing goes out on the air. Bluetooth-only firmware was fine.
- Cause: the radio front-end clocks are shared by Wi-Fi, Bluetooth and 802.15.4. ESP-IDF counts
  their users; upstream esp-radio's `enable_wifi(false)` switches them off unconditionally, and the
  Wi-Fi blob calls it whenever Wi-Fi goes idle.
- Fix: `firmware/patches/esp-radio`, a copy of the crates.io release with one file changed
  (`src/radio_clocks/clocks_ll/esp32c5.rs`): a "who needs the front end" mask. Only
  `firmware/rid-beacon` uses it (its `[patch.crates-io]`). Remove it once upstream has a fix.
- Read more: `firmware/patches/README.md` (with the register values before and after).

**bt-hci 0.10.1 breaks every extended-advertising data command.**
- Symptom: "Invalid HCI Command Parameters" on every LE Set Extended Advertising Data; the
  parameter, enable and clear commands succeed.
- Cause: its async encoder writes a slice's length prefix as `len + 1`. Fixed in bt-hci 0.11, but
  trouble-host 0.8 pins 0.10.
- Fix: `firmware/rid-beacon/src/hci_fix.rs` encodes each packet with the correct blocking encoder.
  Anything else that advertises through trouble-host 0.8 will hit the same bug.
- Read more: `firmware/rid-beacon/README.md`.

**5 GHz channel 149 refused.**
- Symptom: setting a 5 GHz channel fails.
- Cause: esp-radio 1.0.0-beta.1's own country setting passes a zero 5 GHz channel mask, and
  upstream's default is moving to the world-safe "01", which rejects channel 149.
- Fix: both chips call `esp_wifi_set_country_code("US")` directly, and the C5 sets the band to
  5 GHz before its first channel change.
- Read more: `firmware/node-wifi/README.md`.

**Every firmware folder has its own `Cargo.lock`.**
- Symptom: after a shared crate gains a dependency, a firmware's lock file changes on its next
  build (commits `a78b3f3`, `e2d271f`, `1671600`; the last says "Cargo rewrote it on the first
  build").
- Cause: firmware projects sit outside the root workspace, each with its own lock file.
- Fix: when you change a shared crate's dependencies, build every firmware that uses it and commit
  their lock files with the change.
- Read more: root `Cargo.toml`, [A tour of the repository](repo-tour.md).

**The C5 PHY blob is checked by hash.**
- Symptom: `node-sdr`'s `build.rs` panics with a SHA-256 mismatch, or the board prints
  `"what":"phy_abi"`, `abi_ok:false`.
- Cause: `node-sdr` calls 16 private entry points of the closed C5 PHY library, verified against
  one exact version (`edb400d6`, shipped by `esp-wifi-sys-esp32c5 =0.3.0`). A cargo update could
  change it.
- Fix: do not override (`NODE_SDR_ALLOW_UNVERIFIED_PHY=1` exists but is loud); re-check the ABI.
  At boot the firmware refuses every private PHY call if the version string differs.
- Read more: `firmware/node-sdr/README.md` ("PHY ABI"), `firmware/node-sdr/BRINGUP.md`.

**The S3 hub firmware is over its RAM budget.**
- Symptom: `firmware/hub` for the S3 (also the P4's web head) fails to link at `stack.x`.
- Cause: static RAM is 18,088 bytes over as of 2026-11-18; the security work added 712 bytes, the
  origin sort 32 more, on top of an overage that was already there.
- Fix: not fixed yet. Measure from the linker error's addresses, as the README does.
- Read more: `firmware/hub/README.md` (security check 14).

## Pins and drivers

**S3 hub pins to avoid.** 26-37 (flash and octal PSRAM), 0, 3, 45 and 46 (strapping pins), 19 and
20 (USB), and 43 and 44, because the ROM bootloader prints there at reset and that output would
reach a node. The C5 hub's buttons sit on strapping pins. Read more: `firmware/hub/README.md` ("Pins").

**esp-hal 1.2 has no LEDC driver for the C5.** On a C5 hub the backlight is only on or off and the
buzzer must be an active one. Read more: `firmware/hub/README.md`.

**Check P4 socket holes with a meter.** The P4's GPS PPS (GPIO29) and ADSBee input (GPIO30) use
holes of the wireless-module socket, numbered from Elecrow's wiki. Keep any radio module out of
the socket while they are in use. Read more: `firmware/hub-p4/BRINGUP.md`.

## ESP32-C5

**It drops off USB as its firmware starts.**
- Symptom: a console reader dies a second after a reset or flash.
- Cause: the C5's USB-Serial-JTAG re-enumerates as the app starts (the S3 does not).
- Fix: `tools/dev-board.sh` waits up to 10 s for the by-id path and reopens it without resetting
  again (commit `17892d7`). Any tool of your own must do the same.
- Read more: `tools/dev-board.sh`, [Bench tests](../BENCH.md).

**A `rid-beacon` C5 refuses to reuse an advertising set.**
- Symptom: after switching transports, the C5 controller rejects reusing handle 0 as a legacy set.
- Cause: trouble-host only disables sets on stop, and the controller still holds the coded set's
  134-byte data.
- Fix: clear all sets before each `advertise_ext` (commit `17892d7`). The C5 build also raises the
  controller's defaults of 1 advertising set and 31-byte extended data to 4 sets and 251 bytes.
- Read more: `firmware/rid-beacon/README.md`.

**The receive DC offset rails the ADC at high gain.**
- Symptom (2026-11-12, `node-sdr`): DC offset grows with forced gain until one ADC sits at its rail
  (gain 75 and up); it moves between boots and between I and Q. Digital DC removal cannot rescue a
  railed ADC.
- Cause: the PHY measures its 5 GHz receive DC tables once at boot, and on these boards that table
  is unusable (one repeated value). Forcing the gain selects entries with those bad trims, and
  nothing re-measures them.
- Fix: `phy::dc_recal` re-runs the receive DC calibration at boot and on every retune, at the
  tuned frequency (receive-only: no transmit chain, no tone). Gain default is now 83.
- Read more: `firmware/node-sdr/BRINGUP-RESULTS.md` ("Step 6 again, after the DC fix").

**A C5 running `node-sdr` may not take the next flash.**
- Cause: the modem domain keeps running across the USB reset.
- Fix: in a `bringup` build, type `bootloader` first; otherwise hold BOOT while plugging in.
- Read more: `firmware/node-sdr/BRINGUP.md`.

**Take the GDMA module out of reset explicitly** when enabling it (commit `5b72d65`, `node-sdr`).

## ESP32-P4

**Pre-v3 silicon.**
- Symptom: upstream esp-hal does not support the board at all.
- Cause: the Elecrow board carries P4 revision v1.3; upstream esp-hal supports v3.0 and newer
  (different interrupt controller, clocks and ROM).
- Fix: `tools/p4-prepare.sh` applies the community pre-v3 patch to one pinned esp-hal commit.
  `ESP_HAL_CONFIG_MIN_CHIP_REVISION = "103"` must stay in `firmware/hub-p4/.cargo/config.toml`:
  unset, esp-hal assumes v3.0 and the image **traps on the first lock**. Pre-v3 and v3 images are
  not interchangeable, so the build goes to its own `target/rev103`.
- Read more: `firmware/hub-p4/BRINGUP.md`.

**The stack lived inside the L2 cache.**
- Symptom (2026-11-14): after an unrelated change, the PSRAM test faulted (load access fault,
  address 0); earlier, bit-banged I2C failed after PSRAM init, and raising a supply voltage
  seemed to cure it (a red herring).
- Cause: the patched esp-hal's memory map assumed the v3 layout, with the L2 cache at the bottom of
  L2 memory. On pre-v3 silicon the cache is at the **top**, so the whole default stack sat in cache
  RAM, and sweeping 32 MB of PSRAM through the cache overwrote it.
- Fix: `firmware/patches/esp-hal-p4-pre-v3/rotorscope-pre-v3-memory.patch` uses ESP-IDF's pre-v3
  split; `ESP_HAL_CONFIG_L2_CACHE_SIZE = "256KB"` must match what the bootloader programs. The boot
  log prints the split and the stack pointer, and `main` asserts it.
- Read more: `firmware/hub-p4/BRINGUP.md` ("Stack and the L2 cache").

**The stack overflowed with an SD card in.**
- Symptom (2026-11-17): a panic about 8 s after boot, only with a card inserted.
- Cause: `main`'s frame is about 65 KB and only about 7 KB of stack was left; the SD mount plus an
  interrupt hit the stack guard. Earlier the same day, merging the wired bus had shrunk the stack to
  65 KB and it overflowed while mounting the card.
- Fix: the 64 KB tracker moved to PSRAM (the stack grew to 136,160 B; without PSRAM the hub stops at
  boot with a message), and `SITE.TXT` is parsed 256 bytes at a time.
- Read more: commits `775a61b` and `8a5ae6e`.

**Other P4 traps.**

| Symptom | Cause | Fix | Read more |
|---|---|---|---|
| Everything runs slowly | esp-hal defaults the P4 to 90 MHz | `CpuClock::max()`, 360 MHz (3.4 times faster drawing) | `BRINGUP.md` |
| esp-hal's I2C controller: every transfer times out | not identified | bit-banged I2C on GPIO45/46 | `BRINGUP.md` step 4 |
| No SD driver | the patched esp-hal has no SDMMC driver for the P4 | bit-banged SD native mode, `crates/sdnative` | `BRINGUP.md` "microSD log" |
| Gateway bytes lost while drawing | the UART FIFO lasts 1.4 ms; a screen band takes about 6.5 ms and a touch read about 3 ms | UART interrupt into an 8 KB ring | `BRINGUP.md` |
| Flashing fails with "Corrupt data" | the board's CH340 USB chip at 921600 baud | flash at 460800 | `BRINGUP.md` |
| Which second-stage bootloader? | espflash's bundled P4 bootloader is not used; Elecrow's (ESP-IDF 5.4.2) is known good on v1.3 | flash Elecrow's bootloader and partition table from the board's factory backup | `BRINGUP.md` |
| RGB panel registers do not match the PAC | the esp32p4 PAC is the v3 map | register offsets from ESP-IDF 5.4.2, the pre-v3 map | `BRINGUP.md` "RGB panel" |
| Touch "hold for Back" never fired | Select fired on finger-down | wait for release or the hold time | commit `76b54d1` |
| Bench P4 image flashed to a real unit | the P4 ELF carries no `bench-build:` tag, and no tool reads P4 ELFs | the boot line names the bench features and the screen is coded to show **BENCH BUILD** (not yet checked by eye); check before fielding | [Bench tests](../BENCH.md) "Bench builds" |

**The onboard ESP32-C6 has no ESP-NOW.** Its factory esp-hosted firmware lacks it, and the C6 has
no UART or boot pads, so a bad replacement image could leave the board without a radio. Not done
without the owner's explicit go-ahead; a C5 gateway is used instead. Read more:
`firmware/hub-p4/BRINGUP.md` ("C6 and ESP-NOW").

## Bluetooth

**Node resets under Long Range traffic.**
- Symptom (2026-11-14): `node-ble` with ESP-NOW reset in the controller
  (`ASSERT r_ble_lll_mmgmt_block_cnt_check:669`), 4 times in 120 s, while the beacon sent all three
  Bluetooth transports.
- Cause: the controller ending a scan by its own duration while Long Range (coded) reports are in
  flight. Bigger event buffers did not help.
- Fix: no controller duration; the host stops each 800 ms scan itself. 0 resets in 180 s.
- Read more: [Bench tests](../BENCH.md) ("Bug found and fixed: Bluetooth node reset under Long Range traffic").

**Legacy and extended advertising commands cannot be mixed.** Once any extended command has been
sent, the controller answers legacy commands with "Command Disallowed", so `rid-beacon` sends BT4 as
a legacy-PDU set through the extended interface. Read more: `firmware/rid-beacon/README.md`.

**BlueZ on the laptop.**
- `duplicate-data on` is required to see rotating Remote ID messages; without it the controller
  reports one advert per address.
- BlueZ hides a device that never advertises "discoverable"; Remote ID adverts carry no Flags
  (`rid-beacon`'s `RID_BLE_DISCOVERABLE=1` adds a plain advert for checking only).
- On BlueZ 5.72 even `Discoverable=false` reserves 3 bytes for Flags, so a full 31-byte Remote ID
  advert is "too long" and refused: `rid-ble-beacon` sets no `Discoverable` property at all.
- Each message change makes the kernel pick a fresh private address, so `rid-ble-beacon`'s
  non-ID messages show up as separate short tracks. Use the firmware `rid-beacon` to test merging.
- A Bluetooth 4 adapter cannot hear BT5 extended or Long Range at all.
- Read more: `tools/rid-view/README.md`, `firmware/rid-beacon/README.md`.

**The Sonoff dongle.** Older units with a CP2102 (not CP2102N) bridge cannot run at 2 Mbaud;
Sniffle's `_1M` build talks at a different speed and `rid-view` does not support it. On the real
CC2652P a bank erase writes back some configuration fields so the chip keeps its bootloader; the
blank check allows for that (commit `0c92025`). Read more: [SONOFF.md](../SONOFF.md).

## Wi-Fi promiscuous mode

**Received frames end with 4 extra bytes.**
- Cause: esp-radio sizes `PromiscuousPkt.data` by `sig_len`, which includes the FCS slot on every
  MAC generation; the 4 bytes are not a valid CRC on the C5.
- Fix: `RxFrame::mpdu()` drops them before parsing. The `iefit` counter keeps checking; if its
  middle number dominates, the length is wrong for that chip.
- Read more: `firmware/node-wifi/README.md`.

**A busy channel can starve the node.** Data frames never use the receive queue: the callback
classifies them on the spot, so data traffic cannot crowd Remote ID out of the 32 slots. If `drop`
rises on a busy channel, the callback count is the cost. Under a flood, printing with interrupts
masked (`esp_println`) was replaced by an async USB writer, and each received frame is clamped to
one driver buffer before any read (commits `f423dd4`, `19637da`; the first is compile-checked
only). Read more:
`firmware/node-wifi/README.md`, [Bench tests](../BENCH.md) ("Frame flood").

**Unknowns, not yet traps.** Whether the receivers deliver LDPC- or STBC-coded frames (wfb-ng's
default video profile) or frames with group or missing addresses is unverified. Control frames are
not passed by the filter at all, so links that send their header in RTS frames are not seen.
Frames the radio cannot decode are never seen: a miss, not a false alarm. Read more:
`firmware/node-wifi/README.md`.

## Raw capture timing (`node-sdr`)

**Nothing printed while the ring runs may take more than about a millisecond.**
- Symptom (2026-11-12): hundreds of skipped capture blocks a second, the loop a whole ring behind.
- Cause: the once-a-second `stats` line (~500 bytes) took 1.6 ms to print, longer than the 1.43 ms
  ring, and esp-println prints inside a critical section, holding off the DMA interrupt for more
  than a lap.
- Fix: long lines go out in 128-byte pieces with capture work between them; the walker also
  recovers a node whose length was lost (`wb_lost`). 0 skipped blocks in ten minutes. **Rule: no
  more than about 300 bytes at a time while capturing.**
- Read more: `firmware/node-sdr/BRINGUP-RESULTS.md` ("Loop stalls found and fixed").

**Every trigger rise froze the ring.** Each full look at a window stopped capture for about 4 ms,
so Wi-Fi traffic left the node blind 30 % of the time. Gating rises by length before stopping the
ring, and cheaper looks, raised capture time from 70 % to 96 %. Read more: `BRINGUP-RESULTS.md`
("Trigger re-tune").

**Burst length overflowed `u32`.**
- Symptom (2026-11-14): every real DroneID burst (570-640 µs) measured far too short (tens of µs
  or less) and was rejected before decoding.
- Cause: `(n * SEG) as u32 * 1_000_000` wraps past about 107 µs; the live path had never seen a
  DroneID-length burst.
- Fix: `u64`. Found by `tools/droneid-hil`, which is why it exists.
- Read more: `BRINGUP-RESULTS.md` ("Three bugs found and fixed").

**The main stack overflowed into the capture bank, silently.**
- Symptom: 1-2 of 13 identical bursts failing CRC at random.
- Cause: with the capture loop inline in `main`, the decoder ran about 12 KiB past the stack's
  bottom into its own input. esp-hal's stack guard word did not fire: a large frame stepped over it.
- Fix: the capture loop is its own task with static state; `ESP_HAL_CONFIG_ENSURE_MAIN_STACK_MINIMUM`
  demands 40,000 bytes. Nothing catches an overflow at run time: watch `stack_free` (floor 4.5 KiB
  after `hil train`) and the HIL `intact` check.
- Read more: `firmware/node-sdr/README.md` ("Stack").

**Other capture traps.** The FPV level meter once counted the receiver's DC residual, so flat noise
read 8-16 dB "loud" and produced false `hdzero` / `fpv_carrier` sightings (fixed: recalibrate
within 8 MHz, subtract each probe's DC). A 2 dBm Wi-Fi transmitter a metre away clips the 4-bit
window even at gain 60, so gain 83 suits distant drones but close transmitters clip. If the board
ever reports `txq_off:false`, **stop**: the radio's transmit queues are back on, and receive-only is
a hard rule. Read more: `BRINGUP-RESULTS.md`, `BRINGUP.md`.

## USB and serial

| Symptom | Cause | Fix | Read more |
|---|---|---|---|
| A typed `dump` lost everything after about 4 KB | Linux holds only 4 KB of unread serial input; the script slept after each key without reading, so the board's output stalled and was dropped | `dev-board.sh` reads while it types; dump lines wait up to 250 ms per FIFO load | `BRINGUP-RESULTS.md` "Dumps over a busy USB hub" |
| The board lock was reported not to keep two runs apart | most likely (not proven): the lock lived in `/tmp`, and a sandboxed program can have a private `/tmp` | locks in `~/.local/state/rotorscope/locks/` | [Bench tests](../BENCH.md#how-the-lock-works-for-the-curious) |
| The wrong board answered | `/dev/ttyACMn` numbers change on replug | always the `/dev/serial/by-id/` path | `tools/dev-board.sh` |
| The P4 rebooted when you opened its console | the CH340 resets it on every open, even with DTR/RTS low | expect a fresh boot in every log | `firmware/hub-p4/BRINGUP.md` |

## SD cards

- The P4's slot is wired for **SD native 1-bit mode only** (no chip select), so SPI mode is
  impossible; the bus is bit-banged at about 0.94 MHz. Read more: `firmware/hub-p4/BRINGUP.md`.
- `embedded-sdmmc` reads FAT16 and FAT32 only. An exFAT card is reported and left alone. The hub
  never formats, never deletes and never writes outside its own folder.
- On the S3 hub's shared SPI bus, a card must never see a fast clock before it is in SPI mode, so
  the bus starts at the SD init speed (`firmware/hub/src/bus.rs`).

## Keys and builds

- **A malformed `ROTORSCOPE_DEV_KEY` used to fall back to the published test key without a word.**
  Now the P4 build stops (`firmware/hub-p4/build.rs`).
- **A firmware build without a link key used to build an open kit.** Now it stops; open kits need
  `ROTORSCOPE_LINK_KEY_FILE=none` (commit `da58633`).
- **A mixed kit does not half-work.** A node without the key is ignored by a keyed gateway and
  reported as an attack; a keyed node never finds an unkeyed gateway. Reflash every ESP-NOW board
  when the key changes. Read more: [ESP-NOW in depth](../ESPNOW.md#securing-the-link).
- **A per-checkout symlink was committed twice** (`firmware/patches/esp-hal-p4-pre-v3/upstream`;
  commits `def319b`, `19efa54`): the ignore rule matched only a directory, and merging the link
  replaced the main checkout's with one pointing at itself. `.gitignore` now ignores it as a file
  or a link.

## Host tools

- **A replayed recording never confirmed a drone.** The tracker confirms a drone only when it is
  heard again a second later, and a file arrived in milliseconds. `rid-view` now runs recordings on
  a clock driven by the lines' own `up_us` (commit `c57709c`).
- **ADSBee 0.9.0 leaves "updated" bits set** on lines that only repeat old values, so a quiet
  aircraft looked current. The `adsb` crate judges "heard" by message counts only (commit `4e6ffe7`).
- **The bench GPS emulator raised PPS late.** The `gpsloop` bench build raised its PPS pad from a
  main loop that pauses for SD writes, so the edge was more than 5 ms late and every scenario saw
  "GPS time jumped". A rig artefact, not a hub bug: the emulated edge now carries its scheduled time
  (commit `3e31354`).

## Testing

- **A green `cargo test` does not mean the DroneID recordings were decoded.** Without
  `tools/fetch-droneid-samples.sh` those tests print `SKIPPED` and pass.
- **`cargo test` rewrites `crates/screen/snapshots/*.png`.** Check the pictures and commit them
  with a screen change; the documentation copies them.
- **Unoptimised DSP tests take minutes**, so the root `Cargo.toml` builds five DSP crates at
  `opt-level = 2` even in tests. Keep that when adding heavy tests.
- **The bench judge's sample logs are synthetic**, written by hand from the documented formats. If
  a real capture differs, add it to `tools/bench/samples/`.
- **"Compile-checked only" is common.** Much of the 2026-11-18 security work has not run on
  hardware; [Bench tests](../BENCH.md) lists what is still to check, section by section.

**Next:** [Contributing](contributing.md)

## Where this lives in the code

- `firmware/patches/README.md`, `firmware/patches/esp-radio/`, `firmware/patches/esp-hal-p4-pre-v3/`
- `firmware/hub-p4/BRINGUP.md`, `firmware/hub-p4/.cargo/config.toml`
- `firmware/node-sdr/BRINGUP.md`, `firmware/node-sdr/BRINGUP-RESULTS.md`, `firmware/node-sdr/README.md`
- `firmware/node-wifi/README.md`, `firmware/rid-beacon/README.md`, `firmware/hub/README.md`
- `tools/dev-board.sh`, `tools/rid-view/README.md`, `docs/BENCH.md`
- `git log`: the commits named above
