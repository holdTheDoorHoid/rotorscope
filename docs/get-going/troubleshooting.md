# Troubleshooting

**Level: 🟢 Beginner to 🟡 Intermediate** · What to do when something in Step 1, 2 or 3 does not
work, gathered from problems actually met on the developer's bench.

Every entry here comes from the repository: a bench note, a script's own error message, or a
fix in the project's history. If your problem is not here, see [Asking for help](#asking-for-help)
at the end.

> [!TIP]
> Most of the scripts say what is wrong in plain words, and many say what to type next. Read
> the last few lines of output before anything else.

## Quick table

| Symptom | Likely cause | What to do |
|---|---|---|
| `cargo` refuses the project or its `Cargo.toml` | Your Rust is too old for the project's Rust edition (2024) | Update Rust with `rustup`, then try again. [Step 1](#step-1-the-demo) |
| A firmware build stops with `airlink: no ESP-NOW link key` | No kit key yet | Make one: `cargo run -p link-key -- new`. [Link key](#the-link-key) |
| `tools/dev-board.sh --register` says no Espressif boards are attached | Charge-only cable, or the board is not plugged in | Use a USB-C **data** cable. [Boards and USB](#boards-and-usb) |
| `unknown board c5a` | The name is not in `tools/boards.local` | `tools/dev-board.sh --register`. [Boards and USB](#boards-and-usb) |
| A C5 vanishes from USB for a moment as it starts | Normal: it re-connects as its program starts | Nothing; the tools wait up to 10 s. [Boards and USB](#boards-and-usb) |
| A board restarts whenever you open its console | Normal for these boards, and the P4 always does it | Expect a fresh boot each time. [Boards and USB](#boards-and-usb) |
| `busy` or a long wait before flashing | Another program holds that board's lock | `tools/dev-board.sh --list` shows who. [Boards and USB](#boards-and-usb) |
| `rid-view` shows an empty table | No drone nearby (normal), or the test signal is on another band or channel | [Nothing in rid-view](#nothing-shows-in-rid-view) |
| Laptop Bluetooth (`--ble`) hears nothing | BlueZ not running, adapter off, or a Bluetooth 5 signal | [Laptop Bluetooth](#laptop-bluetooth-hears-nothing) |
| `flash-node.sh` refuses to flash | No key, images with different keys, or a bench image | [Flashing nodes](#flashing-nodes) |
| The P4 hub will not build | `tools/p4-prepare.sh` not run, or its folder is not clean | [The P4 hub](#the-p4-hub) |
| P4 flashing fails with "Corrupt data" | 921600 baud is too fast for the P4's USB chip | Flash at 460800 (`-B 460800`). [The P4 hub](#the-p4-hub) |
| The hub's Health page says `no radio nodes reported` | Gateway wiring, DIP switch, or a key mismatch | [Hub shows no nodes](#the-hub-shows-no-nodes) |
| A node's job stays `sent`, never `running` | The node lacks the image for that job | [A node misbehaves](#a-node-is-there-but-misbehaves) |
| `OPEN LINK` on the hub's screen | The gateway was built without a link key | [Link key](#the-link-key) |
| `NO GPS` or `NO FIX` stays on | GPS wiring or power, or no fix yet | [No GPS fix](#no-gps-fix) |
| `no SD` in the top bar | No card, or a card not formatted FAT32 | [SD card](#sd-card-not-seen) |
| No aircraft from the ADSBee | ADSBee not set to send on its serial header | [ADSBee](#no-aircraft-from-the-adsbee) |
| The web head does not link | Known problem: over the S3's memory (2026-11-18) | [Web head](#the-web-head-does-not-link) |

## Step 1: the demo

- **`cargo` complains about the edition or the manifest.** The project uses Rust edition 2024
  (`Cargo.toml` at the top of the repository). An old Rust does not understand it. Update Rust
  with `rustup` and try again ([Set up your machine](../dev/setup.md)).
- **Run it from the top folder of the repository.** The demo is
  `cargo run --release -p rid-view -- --demo`; `-p rid-view` only works inside the project.
- **You do not need a link key for the demo or for `cargo test`.** Only firmware builds need
  one. If you see the link-key message, you are building firmware, not the demo.

## Boards and USB

- **No board found.** `tools/dev-board.sh --register` looks for Espressif boards under
  `/dev/serial/by-id/`. If it says none are attached, use a USB-C data cable (many cables only
  charge), plug the board straight into the computer, and try again. Boards sometimes need to
  be unplugged and plugged back in.
- **`unknown board`.** The name you typed is not in `tools/boards.local`. Register the board
  (plug in one at a time so you know which is which), or check the spelling against
  `tools/dev-board.sh --list`.
- **The board says `ABSENT` in `--list`.** It is registered but not plugged in, or it is on a
  cable that does not carry data.
- **A C5 drops off USB as it starts.** Its USB connection restarts for about a second when its
  program starts. The scripts wait for it to come back (up to 10 s) and print
  `USB re-enumerated (1), reopened`. If you see `left USB and did not come back`, unplug it and
  plug it in again.
- **Opening a console resets the board.** `tools/dev-board.sh` resets a board on purpose before
  it reads, so you see it from the start; a board's first `hello` line then says
  `"reset":"usb"`, which is not a fault. The **P4 hub restarts every time its console port is
  opened**, by any program, even with the port's control lines held low. The test transmitter
  also restarts (about a second) when its console opens.
- **`busy` or a ten-minute wait.** Another run, or another program, holds that board's lock.
  `tools/dev-board.sh --list` shows `BUSY` and who holds it. Wait for it. If you think the
  holder crashed, find out before forcing anything.
- **Do not flash the bench C5 boards with `espflash` directly.** It skips the lock, so two
  programs could write the same board at once. (The P4 is the exception: see below.)
- **Output stops after a few kilobytes** in a terminal program of your own: Linux keeps only
  about 4 KB of unread serial input, and a board's output stalls and is lost when the reader
  pauses. `tools/dev-board.sh` keeps reading while it types for this reason.

## Nothing shows in rid-view

1. **An empty table is normal.** Unless a drone that sends Remote ID flies near you, there is
   nothing to hear. Use the test transmitter to check your setup.
2. **Band and channel.** The C5 test transmitter starts on **5 GHz channel 149**; press `b` on
   its console for 2.4 GHz channel 6. A 2.4 GHz listener will not hear it on 149, and a node
   built for 5 GHz will not hear it on 6.
3. **"Listener alive but heard no ..."** in a bench test: both boards work but the signal did
   not arrive. Move them closer, about a metre apart and away from the laptop, and try again.
4. **Your laptop's Wi-Fi list does not show the test beacon's `RID-TEST` network.** A laptop's
   scan stays on each channel only briefly and rarely catches a once-a-second beacon. Build the
   transmitter with `RID_BEACON_HZ=10` when checking with a laptop. Rotorscope's own listeners
   listen continuously and are not affected.

More on the viewer is in [The laptop viewer, rid-view](../use/rid-view.md).

## Laptop Bluetooth hears nothing

- **`BlueZ is not running`**: start your computer's Bluetooth service.
- **`... is powered off`**: turn the adapter on with `bluetoothctl` (`select` its address, then
  `power on`). If it stays off, check `rfkill list`.
- **The adapter name is wrong**: the error lists the names BlueZ knows (`hci0`, `hci1`, ...).
- **The drone or test signal uses Bluetooth 5.** A Bluetooth 4 adapter, which most laptops
  have, hears only legacy adverts, never Bluetooth 5 extended or Long Range. Use a node running
  `node-ble`, or a Sonoff dongle ([A Sonoff dongle as a Bluetooth 5 receiver](../SONOFF.md)).
- **Duplicate filtering.** By default BlueZ reports one advert per device, which hides Remote
  ID's rotating messages. `rid-view --ble` turns BlueZ's `DuplicateData` on for you. If you
  check with `bluetoothctl` instead, turn on `duplicate-data` in its scan menu first, and build
  the test transmitter with `RID_BLE_DISCOVERABLE=1`, because BlueZ hides devices that never
  advertise as discoverable.
- **Repeated identical adverts are not counted.** BlueZ signals only when an advert's content
  changes. Every Remote ID advert carries a new counter, so none of those are lost.

**Sonoff dongle:** `could not reach the ROM bootloader` means something else has the port open
(close Home Assistant or Zigbee2MQTT, unplug and replug). `no Sniffle messages at 2000000
baud` means the dongle still runs Zigbee, or you flashed Sniffle's `_1M` build. Details in
[A Sonoff dongle as a Bluetooth 5 receiver](../SONOFF.md#if-something-goes-wrong).

## The link key

- **Build stops with `airlink: no ESP-NOW link key, and this is a firmware build`.** Every
  ESP-NOW firmware (gateway and nodes) needs the kit key. Make it once with
  `cargo run -p link-key -- new`, then rebuild and reflash the gateway and every node. Building
  in a second copy of the repository needs the file there too, or
  `ROTORSCOPE_LINK_KEY_FILE=/path/to/link.key` in front of the command.
- **`not a link key (expected exactly 64 hex digits)`**: the key file was edited or damaged.
  Restore your backup. If you have none, make a new key with `new --force` and reflash
  everything.
- **`ROTORSCOPE_LINK_KEY_FILE=...: no such file`**: the path you gave is wrong.
- **`link-key new` says the file already exists.** Keep it: every board of the kit uses it.
  Replace it only on purpose (`new --force`), for example after losing a board.
- **`dev-board: WARNING: ... built WITHOUT a link key` or `has link key 'x' but the kit key is
  'y'`**: that image will not talk to the rest of the kit. Rebuild it.
- **`OPEN LINK` on the hub's screen**: the gateway's last hello says it has no key. Anyone
  nearby could inject fake nodes. Rebuild the gateway with the key.

## Flashing nodes

`tools/flash-node.sh` refuses, rather than flashing something that will not work:

| Message | Meaning | Fix |
|---|---|---|
| `no link key (...): refusing to build an unauthenticated node` | No `tools/link.key` | Make the key, or pass `--open` for an open kit on purpose |
| `ROTORSCOPE_LINK_KEY_FILE=none asks for an open kit: pass --open to confirm` | You asked for no key without saying so twice | Add `--open`, or unset the variable |
| `the images carry different link keys` | Usually a node-sdr image built before you made the key | Rebuild node-sdr (`cargo fw-now` in `firmware/node-sdr/c5`) and pass it in `NODE_SDR_ELF` |
| `... is a BENCH build (...); refusing without ROTORSCOPE_BENCH=1` | A test-only image | Rebuild without the bench feature, or set `ROTORSCOPE_BENCH=1` if you really mean to test |
| `... is esp32s3; multi-image nodes are ESP32-C5 only` | Wrong board name | Use a C5's name |
| `NODE_BUS and NODE_WIRED are two different wirings; set one` | Both set | Pick one |
| `... is not a wired-bus build` | A non-bus image for a bus node | Build every image with `wirebus` |
| `no node-sdr ELF at ...; slot ota_2 erased` | Not an error: you did not build node-sdr | The node will not offer DroneID or FPV. Build it if you want them |

## The P4 hub

- **Build fails because esp-hal cannot be found.** Run `tools/p4-prepare.sh` first; the hub
  builds against the patched copy it makes.
- **`... exists but is not the prepared tree; remove it and rerun`**: the folder
  `firmware/patches/esp-hal-p4-pre-v3/upstream/` is from something else. Delete it and run the
  script again.
- **`patch SHA-256 ... != manifest ...`**: the patch file does not match its recorded checksum.
  Do not build: get a clean copy of the repository.
- **A bench feature stops the build.** Every bench-only feature needs `ROTORSCOPE_BENCH=1`. A
  malformed `ROTORSCOPE_DEV_KEY` also stops the build, on purpose.
- **Flashing fails with "Corrupt data".** Use 460800 baud; 921600 fails on this board's USB
  chip.
- **The screen stays dark after flashing.** On the bench the P4 started by itself after
  flashing, but it may need its reset button on some runs.
- **The P4 restarts when you open its console.** It always does. See
  [Boards and USB](#boards-and-usb).

## The hub shows no nodes

Work through these in order:

1. **DIP switch 1 on the P4 must select UART1.** Otherwise the UART1 header's pins go to the
   wireless-module socket.
2. **The gateway must be on the UART1 header**, not UART3-IN. They were swapped on 2026-11-17;
   older notes and older hub firmware use the other header.
3. **TX to RX, RX to TX, and a ground wire.** Gateway D6 to the header's RX (GPIO48), gateway D7
   to its TX (GPIO47) ([Wiring](kit-wiring.md#1-gateway-to-hub-required)).
4. **Is the gateway running?** On its own USB it prints a `"t":"gw"` line every second and a
   `"t":"node"` line per node. No node lines means the nodes are not reaching it.
5. **Same key everywhere?** The gateway's `hello` line and each node's `uplink` line show a key
   ID; they must match. A node built without the key, or with an old one, shows up at the
   gateway only as rejected packets (`unauth`, `bad_tag`). A few `stale` right after the
   gateway restarts are normal.
6. **Gateway and wired-bus nodes on the same wires do not work.** The gateway talks whenever it
   likes and garbles the bus. Use one or the other.
7. **More than 16 nodes.** The hub's node table holds 16; a 17th is refused (`table_full`)
   until a silent node's slot frees up after a minute.

The hub's console `hub` line has a `link` object with counts of corrupt and refused frames, and
`"gw":"gw key <ID>"` when the gateway's hello arrived. When a gateway is attached, the hub
also prints `node link carries an unpolled stream`: that is normal, it means the hub
recognised a gateway rather than the wired bus.

### A node is there but misbehaves

- **`NOT RESPONDING`**: the hub has not heard from it recently. It may be restarting into
  another image (a few seconds), or have restarted after a fault: its next `hello` line says
  why (`"reset":"watchdog"`, `"software"`, `"brownout"`, ...).
- **The job stays `sent`, never `running`.** The node did not take the job. A node without the
  node-sdr image cannot do DroneID or FPV; a node built from an old image that does not report
  its abilities is only ever given Wi-Fi 2.4; a bench build that offers only one job is only
  ever given that one.

## No GPS fix

> [!NOTE]
> No real GPS module has been connected to the P4 yet (as of 2026-11-18). These checks come
> from the code and from the on-chip emulation, not from bench experience with a module.

- **The Health page says `no receiver found`** (and the top bar `NO GPS`): the hub heard no
  valid data at 115200, 9600 or 38400 baud. Check that the GPS TX goes to the UART3-IN
  header's RX pin (GPIO28), that it has power from a **3.3 V** pin (the UART3-IN header's own
  power pin is 5 V), and ground. The hub keeps trying, so a module plugged in later is found.
- **A couple of UART errors at start-up** are normal: they are bytes from a 9600 baud module
  heard while the hub listened at 115200.
- **`no fix, N satellites`**: the module works but has no fix yet.
- **No PPS**: the hub sets the LC29H(AA) to pulse only with a 3D fix; check the PPS wire to
  GPIO29 on the wireless-module socket.
- **`no trusted position` or `no trusted time`**: the hub saw signs of GPS jamming or spoofing,
  or a jump it cannot explain, and is holding its last trusted values. If you have really
  moved the hub, use **Hub moved** on the Settings page. See
  [GPS jamming and spoofing at the hub](../GNSS-INTEGRITY.md).

## SD card not seen

- **`no SD` in the top bar.** The hub looks for a card every 3 seconds. Push the card fully in.
- **The console says `sd card is not FAT16/FAT32 (partition type 0x07 = exFAT/NTFS)`.** The
  card is exFAT, as most cards over 32 GB are. Reformat it as FAT32 on a computer; the hub never
  formats a card.
- **Any card error** drops the hub back to looking for a card; put it back and a new file
  starts.
- **The strip says `NO EVIDENCE (no SD): N waiting`** (built, not yet checked by eye): evidence
  records are queued until a card appears.
- **Log files are numbered (`LOG00012.CSV`), not dated.** Dated names need GPS time; the hub
  waits up to 90 seconds after boot for it, then numbers the file.

## No aircraft from the ADSBee

- **Set the ADSBee first, over its own USB**: `AT+PROTOCOL_OUT=COMMS_UART,CSBEE`. The hub cannot
  set it. Those settings are lost when the ADSBee is unplugged unless you save them
  (`AT+SETTINGS=SAVE`); see [ADSBee 1090](../ADSBEE.md).
- **Check the wire**: ADSBee TX to GPIO30 on the wireless-module socket, plus ground. Its
  serial speed must be 115200 (the ADSBee's default).
- The hub has not yet been tried with a real ADSBee (2026-11-18).

## The web head does not link

As of 2026-11-18 the S3 web-head firmware stops at the link step: it needs about 18 KB more
static RAM than the ESP32-S3 has. This is a known problem, not something wrong on your
machine. There is no phone page until it is fixed.

## Asking for help

Open an issue on the project's GitHub page:
<https://github.com/holdTheDoorHoid/rotorscope/issues>. A useful report says:

- which board and firmware, with the commit (`git rev-parse --short HEAD`);
- the exact command you ran, and its last lines of output;
- what you expected, and what happened instead.

You may include the link key's **ID** (eight hex digits). Never paste `tools/link.key` itself,
or anything that identifies your home.

**Security problems go privately, not in an issue:** a way to forge traffic between boards,
crash a board by radio, or alter the logs unnoticed. Use the repository's **Security** tab,
**Report a vulnerability**, as described in the [Security policy](../project/security.md).

## Where this lives in the code

- `tools/dev-board.sh`, `tools/flash-node.sh`, `tools/p4-prepare.sh`: the messages quoted above
- `crates/airlink/build.rs`, `crates/airlink/src/keysrc.rs`: the link-key messages
- `tools/rid-view/src/bluez.rs`: the laptop Bluetooth messages
- `firmware/hub-p4/src/sd.rs`, `src/gps.rs`: the SD card and GPS
- `docs/BENCH.md` "When something fails", `firmware/hub-p4/BRINGUP.md`,
  `firmware/node-sdr/BRINGUP-RESULTS.md`: the bench records

**Next:** back to [Choose your path](README.md)
