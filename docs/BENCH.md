# Bench tests: checklist

Two boards, one command, PASS or FAIL. Run everything from the project folder
(the top of the repository).

Nothing here transmits except `rid-beacon`, the test beacon. It uses a fake drone called
`TESTDRONE0000000001` at the lowest power. Keep the two boards on the desk, a metre or so apart.

## Before you start (once)

1. Plug in **one** board with its USB-C cable (the port that shows up as a serial device).
2. Run `tools/dev-board.sh --register`. It prints a name (`s3`, `s3b`, `c5a`, `c5b`, ...), the chip
   and the MAC address, and saves them in `tools/boards.local` (this file stays on your machine).
3. **Put a sticky label on the board with that name.** Unplug it.
4. Repeat for each board. Plug in one at a time, so you always know which is which.
5. When done, plug all in and run `tools/dev-board.sh --list`. Every board should say `attached`.

Names matter: `s3` and `s3b` are the two ESP32-S3 boards, `c5a` and `c5b` the two ESP32-C5 boards.
If you move a board to a different USB port nothing changes, because it is found by its MAC.

## Which board does what

| Test group | Command | Needs | Transmitter | Listener |
|---|---|---|---|---|
| Wi-Fi beacon, Wi-Fi NAN, Bluetooth 4, 5 and Long Range (2.4 GHz) | `tools/bench/run.py --group s3` | `s3`, `s3b` | `s3` | `s3b` |
| Same, 5 GHz Wi-Fi, and C5 Bluetooth | `tools/bench/run.py --group c5` | `c5a`, `c5b` | `c5a` | `c5b` |
| Mixed chips (C5 sends, S3 listens; S3 sends, C5 listens) | `tools/bench/run.py --group cross` | `s3`, `c5a`, `c5b` | see `--list` | |
| A C5 as the 2.4 GHz Wi-Fi node (`node-wifi` built `cargo fw24`) | `tools/bench/run.py c5-to-c5-wifi24-beacon c5-to-c5-wifi24-nan s3-to-c5-wifi24-beacon s3-to-c5-wifi24-nan` | `c5a`, `c5b`, `s3` | `c5a` or `s3` | `c5b` |
| Fake open FPV links (wfb-ng, OpenHD, RubyFPV, DroneBridge) on ch 149, 161, 165 and 6 | `tools/bench/run.py --group openlink` | `c5a`, `c5b` | `c5b` | `c5a` |
| Node to hub over wires | `tools/bench/run.py --group hub` | see "Hub wiring" | node | hub (`s3`) |

One test at a time: `tools/bench/run.py --list` shows every test name, then for example

```
tools/bench/run.py s3-to-s3-wifi-beacon
tools/bench/run.py s3-to-s3-ble-lr
```

Add `--dry-run` to see what a test would do without touching anything. A full test takes about
two minutes (the first build takes longer). Do not unplug a board while a test runs.

## Reading the result

The last lines are a table. For each test:

* **PASS**: the listener decoded at least 3 messages from the test beacon, with the right
  transport (and the right band for Wi-Fi). Evidence is shown: channel, signal strength, sender address.
* **FAIL**: the boards ran, but the listener did not hear the beacon the way it should. The reason is
  printed on the same line, for example "heard it as WifiBeacon, not WifiNan".
* **ERROR**: the test could not run (board not registered, not plugged in, build broke, board stuck).
  This says nothing about the radio.

The exit code is 0 only if everything passed.

First results (2026-11-12): all five `--group c5` tests and all eight `--group cross` tests PASS,
including Bluetooth Long Range in both directions and 5 GHz NAN. A C5 built as the 2.4 GHz Wi-Fi
node passes all four of its tests (beacon and NAN, from a C5 and from the S3). A C5 drops off USB for about a second as its app starts; the tools wait for it.

## Open FPV links (2026-11-14)

`--group openlink` is 16 tests: each of the four systems on each of channels 149, 161, 165 and 6.
The beacon (`c5b`) types `x`, a channel key and the system's key (for example `x9f` = channel 165,
fake wfb-ng). The listener (`c5a`) runs `node-wifi`: the 5 GHz build for 149/161/165, the 2.4 GHz
build (`cargo fw24`) for channel 6. PASS = at least 3 `"t":"open"` lines with that system, its
fake ID and that channel. The fake frames and IDs are described in `firmware/rid-beacon/README.md`.

**Result: 16 of 16 PASS** on the first full run (`bench-logs/20261114-181014`), boards about a
metre apart, beacon at 2 dBm, 0 failed sends.

| Channel | Systems | Reports (each test) | RSSI | Frames counted in 30 s |
|---|---|---|---|---|
| 149 | all four | 26-27 | −43 to −41 dBm | ~430 (149 is the listener's home channel) |
| 161 | all four | 26 | −43 to −42 dBm | ~35 (listener visits for 50 ms in 800) |
| 165 | all four | 24-27 | −45 to −44 dBm | ~34 |
| 6 | all four | 25-26 | −41 to −38 dBm | ~420 |

The two open questions from PLAN 11a:
- **Does the C5 receive channel 165 in normal Wi-Fi mode? Yes.** All four systems passed there,
  with frames received at the same rate as on 161.
- **Does it receive wfb-ng's default LDPC/STBC-coded frames? Still open.** Our transmitter cannot
  send them: the ESP32 raw-TX API sets only the PHY mode and rate, with no LDPC or STBC switch. So
  these tests send plain coded frames. Answering this needs real wfb-ng/OpenIPC hardware with its
  default video settings.

## Hub wiring (two boards)

The node's TX pin goes to the hub's RX pin, and the two grounds are joined. Three wires or fewer.
Both boards are 3.3 V; never use 5 V pins. The pins are placeholders from the code
(`firmware/hub/src/pins.rs`, `firmware/node-*/src/`). Check them against your boards' printed labels.

| Test | Node | Node TX pin | Hub RX pin (S3 hub) | Also |
|---|---|---|---|---|
| `hub-uart-s3-wifi` | `s3b` running node-wifi | GPIO43 (labelled TX / D6) | GPIO4 | GND to GND |
| `hub-uart-s3-ble` | `s3b` running node-ble | GPIO17 | GPIO6 | GND to GND |
| `hub-uart-c5-wifi` | `c5a` running node-wifi | GPIO11 (labelled TX) | GPIO15 | GND to GND |
| `hub-uart-c5-ble` | `c5a` running node-ble | GPIO11 | GPIO6 | GND to GND |

The hub (`s3`) and the node each stay plugged into the computer by USB. Power the hub and the node
from USB; do not also power them from the wire. Change the wires between tests.
The C5 pins have not been checked against a real board's labels.

PASS means the hub counted at least 10 good messages from the node on that link. If frames show up on
a different link than expected, the wire is on the wrong hub pin.

## When something fails

1. Read the reason on the FAIL line. It usually says what to check.
2. Open the raw logs. Each run saves them in `bench-logs/<date-time>/<test>/`:
   `tx.log` is the transmitter, `rx.log` the listener (`node.log` and `hub.log` for the hub tests),
   `result.json` the verdict and numbers. Look at the last few lines:
   `tail -20 bench-logs/*/s3-to-s3-ble-lr/rx.log`
3. "Listener printed nothing" or "never printed hello": check the cable (data cable, not charge-only),
   then run the test again. Boards sometimes need to be unplugged and replugged.
4. "Transmitter did not send ..." or "isolation keys not applied": the beacon itself is the problem.
5. "listener alive but heard no ...": both boards work but the radio signal did not arrive. Move the
   boards closer, away from the laptop, and try again. Wi-Fi 5 GHz and Bluetooth Long Range are the
   least tried; a real FAIL there is a finding, so save the `bench-logs/` folder.
6. "busy" or long waits: another run is using the board. `tools/dev-board.sh --list` shows who holds
   it. Wait, or if it is a crashed run, ask before forcing anything.
7. Do not run `espflash` or `cargo run` directly on these boards: they skip the lock.

## How the lock works (for the curious)

Each board has its own lock file under `~/.local/state/rotorscope/locks/`, named after the board's
MAC. Anything that flashes or reads a board takes that board's lock first, so two runs cannot flash
the same board at once, while the two boards of one test can run at the same time.
The old lock lived in `/tmp`. A sandboxed program can have its own private `/tmp`, so two programs
could each hold "the" lock without seeing each other. That is the most likely cause of the earlier
report that the lock did not serialize (not proven; it could not be reproduced without hardware).

## Link keys: bench checklist

The per-node link keys (docs/LINK-KEYS-DESIGN.md, docs/ESPNOW.md "Securing the link") are
host-tested, and this checklist was **run on hardware on 2026-11-19** ("Results, 2026-11-19" at
the end of this section: six faults found and fixed, what passed, what could not be run). It walks
the design's test plan (section 9) on the bench kit: P4 hub `p4`, gateway `c5c`, nodes `c5a` and
`c5b`. All forged traffic comes from our own boards through their normal radio path (the
`forgetest` and `gwforge` builds); no other transmitter is used. Do the parts in order: each
relies on the one before.

### 0. On the computer, no board

```sh
RUSTC_WRAPPER= cargo test --workspace --locked      # all pass
tools/check-wifi-blobs.sh                          # radio libraries and the patched esp-radio
tools/check-image-identity.sh                      # images do not depend on the kit (identical builds)
tools/dev-board.sh --self-test && tools/flash-node.sh --self-test
```

`dev-board.sh --self-test` also runs the real `espflash partition-table` over every
`tools/partitions-*.csv`: espflash 4.7.0 cannot read a table with a data subtype it has no name
for, which is why `rskey`, `rskit` and `rsboot` are `data, undefined`.

The P4's USB port is a CH340, which `--register` does not look for. Add it to
`tools/boards.local` by hand, so `tools/link-key` reaches it under the same lock as everything
else (check the name with `ls /dev/serial/by-id/`):

```text
p4  /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0  esp32p4
```

The bench kit's files are `tools/kit.key` and `tools/kit.roster` (git-ignored). Never commit them
or paste them anywhere. If an old `tools/link.key` is there, leave it: it is only needed to build
the old-firmware board of step 5.

### 1. Make the kit and provision the hub, read-only first

1. `cargo run -p link-key -- kit new` (once; it refuses to replace an existing kit). Note the
   kit id it prints.
2. Build the hub with flash writes off: `(cd firmware/hub-p4 && cargo fw --features rskit-readonly)`.
   Flash it as in docs/get-going/kit-flash.md, but with `--partition-table tools/partitions-hub.csv`
   in place of `elecrow-partitions.bin` (its first four rows are Elecrow's, then `rskit` at
   `0xC10000` and `rsboot` at `0xC12000`), keeping `--bootloader elecrow-bootloader.bin` and
   `-B 460800`.
3. `cargo run -p link-key -- hub p4 --dry-run`: it prints the `espflash read-flash` command that
   reads the old boot counter and the `espflash write-bin` commands, and writes nothing. Then
   `cargo run -p link-key -- hub p4`. Expected: "master and roster (0 entries) written", "boot
   counter seed written (random (...))" the first time, "(above the old counter)" on any later
   run whose counter still reads.
4. Each node, dry run first. Read the MAC from `tools/dev-board.sh --list` or the board's label:
   `cargo run -p link-key -- node c5a --dry-run --mac <c5a MAC> --label c5a`, then
   `cargo run -p link-key -- node c5a --label c5a`; the same for `c5b`. Then
   `cargo run -p link-key -- hub-roster p4` and `cargo run -p link-key -- show` (slots 0 and 1,
   their key checks).
5. Reset the P4. Console: `linkauth: rsboot read-only latest=<N> (rskit-readonly build: no
   epoch, keyed link stays closed)` and `linkauth: rskit keyed, kit <id>, but NO BOOT COUNTER
   this boot: keyed link CLOSED`; the strip shows `NO BOOT COUNTER: link closed` (expected in
   this build). The `2 node slot(s) loaded` line and the two `slot` lines naming the nodes' MACs
   come with the default build of step 2, once the hub has an epoch.

### 2. Let the hub write its boot counter

1. `(cd firmware/hub-p4 && cargo fw)` and flash it the same way.
2. Console: `rsboot written <N+1> at +0x..., read back: this boot's epoch`; the strip no longer
   says `NO BOOT COUNTER`. Power-cycle twice: the value goes up by one each time.
3. Optional: read the partition back with `tools/dev-board.sh --run 'espflash read-flash --port
   "$BOARD_PORT" --non-interactive 0xC12000 0x2000 /tmp/rsboot.bin' 120 p4` and check the entries
   (`kitkeys::bootlog::read`).

### 3. Gateway and nodes, normal running

1. Gateway: `cd firmware/gateway && . ../../tools/repro-env.sh && cargo fw "${REPRO_CARGO_ARGS[@]}"`,
   then `tools/dev-board.sh firmware/gateway/target/riscv32imac-unknown-none-elf/release/gateway 60 c5c`.
   Its `hello` says `"protocol":14`, `"key":"none"`; within 5 s a `"t":"roster","n":2` line.
   The P4: `node link now reads a protocol 14 gateway`.
2. Nodes: `tools/flash-node.sh c5a` and `tools/flash-node.sh c5b`. Each ends with
   `flash-node: link keyed: "auth":"kit:<id> slot:<n> gen:1 check:<c>"`, the check matching
   `link-key show`.
3. Run 5 minutes. P4 `hub-p4: gwlink` line: `pkts` and `ok` grow, `bad_tag`, `replay`, `stale`,
   `old_fw`, `not_in_roster`, `late_uplink`, `future_uplink` stay 0; `beacons` about 2 a second,
   `rosters` one per 5 s plus one at each gateway `Hello`, `cmds` and `acked` grow (the
   `Assign`s). Both nodes report, their `Status` echoes the hub's plan ids, and no counter on a
   node's `link_auth` line moves (node-ble prints the line only once one is non-zero). Every 5 s
   the hub prints `delivery node <n>: claimed=<c> received=<r>` for each node: the two numbers
   stay equal. The gateway's `"t":"gw"` line: `relayed` grows, refusals 0; `uart_drops` and
   `unlisted` hold whatever arrived before the hub's first roster (a handful, at the gateway's
   start) and do not grow. The strip has no link line. After a hub restart `stale` and
   `catching_up` each rise by one per node, with no alert.
   This is also the bench check of the patched esp-radio on C5 boards that run Wi-Fi and BLE at
   once: a node switched to the Bluetooth role (node-ble with ESP-NOW) must keep reporting adverts.

### 4. Attacks (design section 9)

Captured node: build node-wifi with `forgetest` and put it in c5a's first slot, keeping the table
(a plain `tools/dev-board.sh <elf>` would use espflash's default table, which has no `rskey`, and
the node would report `missing`):

```sh
export ROTORSCOPE_BENCH=1
(cd firmware/node-wifi/c5 && cargo build --release --features esp32c5,band24,espnow,forgetest --target-dir ../target-espnow)
tools/dev-board.sh --pt tools/partitions-node.csv --erase ota_1 --erase ota_2 \
  --app ota_0=firmware/node-wifi/target-espnow/riscv32imac-unknown-none-elf/release/node-wifi --multi 90 c5a
```

`--erase ota_1 --erase ota_2` leaves c5a with the Wi-Fi image only, so the hub cannot move it to
the Bluetooth image (which has no `forge` commands) in the middle of the table.

Commands are typed on c5a's USB (`tools/dev-board.sh --log 20 c5a --send $'forge claim <c5b MAC>\r'`).

| Row | Command | Expected |
|---|---|---|
| Speaks as another node | `forge claim <c5b MAC>` | P4 `bad_tag` +5 (c5b's slot), a `forged_link` naming c5b; nothing from c5a under c5b; c5b keeps reporting. If the line says the driver refused the MAC change, nothing was sent: note it |
| Commands another node | `forge cmd <c5b MAC>` | c5b's `link_auth` line: `bad_tag` +1; c5b's plan unchanged |
| Forges a beacon | `forge beacon` | c5b `bad_tag` +1, its gateway address unchanged |
| Withheld batch | `forge hold 5` | P4 `late_uplink` +1; nothing from that batch in the tracker |
| Flood on its own address | `forge spam 500`, then `forge spam 0` | gateway `over_budget` for c5a; c5b's `failed` and `dropped` do not move; the P4's `link` line keeps its `loops=` (about 550 a second) and a `max_gap` under 1 ms |
| Frames it may not send | `forge hubfix`, `csbee`, `flags`, `via`, `hello`, `threat`, `join` | P4 `refused=` +1 each and its own `forged_link` detail 5 naming c5a; the gateway's counters do not move; nothing in the "Must never" column of "Origin checks" happens |

Captured gateway: `ROTORSCOPE_BENCH=1` and `(cd firmware/gateway && cargo build --release --features gwforge)`,
flashed to c5c as in step 3. The P4's strip shows `BENCH GATEWAY`. Typed on c5c's USB:

| Command | Expected |
|---|---|
| `gwforge batch` | P4 `bad_tag` +1 for the first roster slot |
| `gwforge flip <c5a MAC>` | c5a `bad_tag` +1 at the next command to it; its plan unchanged |
| `gwforge replay` | P4 `dup` +1: the tool sends the last relayed packet again, which to the hub is a radio retry of the newest batch (dropped, never taken twice). An older packet would count as `replay`; that order rule is host-tested only |
| `gwforge delay 1000` | the next `TimeSync` arrives late: an aero node (none on this bench) counts `late` +1; without one, check the P4's `ts_held_max` and that nothing else changes |
| `gwforge drop <c5a MAC> 60` | within 60 s **Node reports going missing** (`link_loss`, `detail` the percent missing) for c5a on the P4, and its `delivery node` line says `LOSSY`; `gwforge drop off` ends it. On a quiet bench a node sends about 100 frames a minute, so 50 % stays just under the rule's 50 missing frames: use 60 or more |
| `gwforge drop <c5a MAC> 80 steady` | the steady pattern removes every packet that carries c5a's health report: its `delivery node` line says `last <n> s ago` and climbing, and after 30 s `link_loss` with `detail` 0; `gwforge drop off` ends it |

Then flash the normal gateway and node builds again (step 3) and unset `ROTORSCOPE_BENCH`.

### 5. Provisioning states and a mixed kit

| Check | How | Expected |
|---|---|---|
| Missing record | erase c5b's key record on purpose: `tools/dev-board.sh --run 'espflash erase-region --port "$BOARD_PORT" --non-interactive 0x611000 0x1000' 60 c5b`, reset it | c5b prints `"auth":"none","why":"missing"` every 10 s and sends no reports; its LED blinks; the P4 strip `UNPROVISIONED NODE 1`. `tools/flash-node.sh c5b` exits 1 naming `missing` |
| Damaged record | `ROTORSCOPE_BENCH=1 cargo run -p link-key -- node c5b --slot 1 --bench-corrupt`, then `hub-roster p4` | `why":"damaged"`, the same strip line |
| Back to normal | `cargo run -p link-key -- node c5b --slot 1`, `hub-roster p4` | keyed again, generation one higher; the hub's node number for c5b is unchanged |
| Revoke | `cargo run -p link-key -- revoke --slot 1`, `hub-roster p4` | the gateway drops c5b's packets as `not_in_roster` (the hub pushes only live slots), and its beacon tag is zero (c5b counts `not_for_this_slot`); provision it again afterwards |
| Old node firmware | build node-wifi from a commit before the cut-over (`6651ecd`) in a scratch checkout with the old kit key, `ROTORSCOPE_LINK_KEY_FILE=<this checkout>/tools/link.key cargo fw24-now`, flash it to c5b | Only an old node that was **already linked** keeps sending and is counted (gateway `old_firmware`, a `forged_link` detail 8 on the P4, strip `OLD FIRMWARE: 1 node`). One that starts fresh cannot read the new beacons, never finds the gateway (`"gw":false`, `pkts` 0) and is simply a node that does not report |
| Old gateway | the gateway from `6651ecd` the same way | strip `OLD FIRMWARE: gateway`; nothing relayed |
| Hub re-provisioned | `cargo run -p link-key -- hub p4` again, reset the P4 | "above the old counter"; both nodes keep reporting without a 60 s gap |

Reflash the current builds and provision c5b again before going on.

### 6. Rows with no bench tool yet

These rows of the design's test plan rely on host tests only, because the bench aid they name was
not built: the hub console `bench switchstorm` (image-switch storm: `switch_backoff_doubles_and_caps`
and the `nodeimage` tests), a hub command that sends a 24-hop 5 GHz plan and `floodtest`'s
`job deaf` (deaf plans: `coverage` and node-wifi `role` tests), and a `faulttest` hook that asks
for randomness before the radio starts (each board's boot line still shows `"rng":"radio"` on the
C5 or `"trng"` on the S3 and C3). Node-number reuse is host-only by design. Held fix: stop the GPS
emulator mid-run (docs/BENCH.md "GPS time and trust scenarios"); the phone page must show the fix
as held, not live.

### Results, 2026-11-19

Run on P4 hub `p4`, gateway `c5c`, nodes `c5a` and `c5b`, with a bench kit made for this run.
The final firmware is commit `615a725`. The logs are kept off the repository.

**Faults found and fixed during the run** (each with a test that fails without the fix, except the
first, which is a tool check):

| Found | Fix |
|---|---|
| espflash panicked on all three new partition tables (data subtypes `0x40`-`0x42`), before any board was touched | `rskey`, `rskit`, `rsboot` are `data, undefined`; `--self-test` runs the real espflash over the tables (`47c4891`) |
| The hub sealed 4 beacons a second, not 2 (1,239 in 313 s): the early beacon in front of the SD step ran on every pass | early beacon only when a slow SD step is due (`37f0d37`) |
| Every hub restart raised one "Forged node traffic" alert per node (their last batch of the old boot, `stale`) | a genuine old-boot batch from a slot not yet accepted, in the first 90 s, counts as `catching_up` and raises nothing (`c449277`) |
| `gwforge drop <mac> 80` for two minutes raised nothing: the steady pattern removed every health report, and the hub compares only when one arrives | a heard node whose reports are 30 s overdue is a `link_loss` with detail 0; `gwforge drop` scatters, `... steady` keeps the old pattern; the hub prints its delivery numbers (`ea52876`) |
| Every gateway restart raised one "Forged node traffic" alert per node (`not_in_roster`, before the first roster arrived) | before its first roster the gateway counts such packets as `unlisted` and reports nothing (`30a0d4f`) |
| With beacons exactly 500 ms apart a Bluetooth-role node (200 ms of link time a second) heard none for 74 s after an image switch; 2 restarts in 5 found no gateway in 29 s | one beacon in every 500 ms, at a varying place in it (`615a725`): 6 restarts in 6 found the gateway in 1 to 4 s |

**What passed** (final firmware unless a commit is named):

| Step | Result |
|---|---|
| 1. Hub layout and read-only build | Elecrow's bootloader takes the new table (`rskit`, `rsboot` listed as unknown data, the first four rows byte-identical to the factory table). ROM flash read works at `0xC10000`: `rskit keyed, kit <id>`, `rsboot read-only latest=<N>` |
| 2. The hub's first flash write | `rsboot written <N+2> at +0x10, read back`; +1 at each of four further boots; the partition read back holds 5 entries and erased flash after them. No crash, no damage to the application |
| 3. Clean run, 315 s (`41030d1`) and 195 s (final) | 620 of 620 and 390 of 390 packets accepted; every refusal counter 0; gateway `relayed` equal, `hub_corrupt` 0; no reset; final: 2.0 beacons a second, `claimed` equal to `received` for both nodes |
| 3. Hub restart | new epoch, both nodes proven again within 5 s without restarting; `stale` 2, `catching_up` 2, no alert |
| 4. Captured node (`forgetest` on c5a) | `claim`: the driver took the MAC change, P4 `bad_tag` +5 and a `forged_link` naming c5b's address. `cmd`, `beacon`: c5b `bad_tag` +1 each (gateway `foreign` +1 for the beacon). `hold 5`: `late_uplink` +1, detail 11. Seven hub-only frame types: `refused` +7, detail 5 naming c5a. `spam 500` for 35 s: 5,259 sent, gateway `over_budget` 3,845, 40 a second reached the hub and failed their tag; c5b `failed` 0, `dropped` unchanged; hub loop 544 passes a second (548 before), longest gap 0.5 ms |
| 4. Captured gateway (`gwforge` on c5c) | `batch`: P4 `bad_tag` +1. `flip`: c5a `bad_tag` +1, command not taken. `replay`: `dup` +1. `drop 60`: `link_loss` 59 % after 50 s. `drop 80 steady`: `link_loss` detail 0 after 45 s. Both ended by themselves after `drop off` |
| 5. Provisioning | erased record: `"auth":"missing"`, no reports, P4 `unprov` grows, `flash-node.sh` exits 1 naming it. `--bench-corrupt`: `"auth":"damaged"`. Provisioned again: generation +1, keyed. `revoke`: gateway `not_in_roster` +45 in 40 s, c5b `not_for_this_slot`, P4 `forged_link` detail 9 naming it. Old gateway against the new hub (seen at the change-over): `OLD FIRMWARE: the gateway speaks protocol 13`. `link-key hub` again: "above the old counter", c5a never stopped (longest pause 1 s) |

**Not run, or not as written**

- **The screen was not looked at.** Every strip line above (`NO BOOT COUNTER`, `OLD FIRMWARE`,
  `UNPROVISIONED NODE`, `BENCH GATEWAY`) is checked from the console counters and from the
  screen snapshots, not from the panel. Someone has to look.
- `gwforge delay`: nothing urgent is sent without an aero node, so it never fired.
- `replay` as a counter, and a cold-started old-firmware node: see the notes in the tables above.
- Section 6's rows remain host-tested only.
- **Open, not fixed:** under a scattered 60 % loss the `link_loss` alarm ended and came back
  once while the loss went on (few reports arrive, the window holds under 50 missing frames
  for a moment). Each hub restart can swap which node gets the Bluetooth role, costing each an
  image switch. The gateway's `uart_drops` counts the packets heard before its first roster.

## End-to-end through the P4 (2026-11-15)

Every signal we may legally send, through the whole chain: test transmitter (`c5b`, rid-beacon,
2 dBm Wi-Fi, -15 dBm BLE, about a metre away) -> listener node `c5a` (multi-image, ESP-NOW) ->
gateway `c5c` -> wire -> P4 hub. **PASS means the hub's own tracker table had it**, read from the
hub's console with a bench-only build (below), not from the node.

### How it was run

* **One role at a time.** With one listener node the coordinator always gives it Wi-Fi 2.4. A bench
  build switch makes the node offer only one role, so the hub assigns that role:
  `ROTORSCOPE_BENCH=1 ROTORSCOPE_BENCH_CAN=wifi24|wifi5|ble tools/flash-node.sh c5a` (`node_common::bench_can_mask`;
  both images get the same mask; any other value stops the build; unset = no restriction). Never
  for a deployed node.
* **Hub evidence.** `ROTORSCOPE_BENCH=1 cargo build --release --features benchlog` in `firmware/hub-p4`
  (`src/bench.rs`): a `bench new-slot` / `new-link` / `new-rf` / `new-wifidrone` line the moment the
  tracker gets a new drone slot (transport and band), open-link stream, RF emitter or Wi-Fi drone,
  a table summary every 5 s, and at boot a count by kind of every row in the previous SD log file.
  It prints only our test IDs (`TESTDRONE...`); link IDs only as `fake` / `none` / `other`.
* Per role: session A = hub reboot, beacon on, reflash `c5a` with the mask (role switch timing);
  session B = hub reboot, then one transmission at a time (45 s each for Remote ID, 30 s for each
  fake link, 2 s of silence between). Each hub boot writes its own SD log file; the next boot counts it.
* "Time" = from typing the key on the beacon to the hub's `new-` line. It includes the beacon's
  own restart (opening its console resets it, about 1 s), so the chain itself is faster.

### 1. Remote ID through the hub

One track all along: `TESTDRONE0000000001`, `drones=1`, `decode_err=0`, location present.

| Signal | Node role (hub plan) | Hub slot | Time | Heard by the hub (segment) | RSSI | Result |
|---|---|---|---|---|---|---|
| Wi-Fi beacon, ch 6 | Wi-Fi 2.4 (`0x1503`) | WifiBeacon / 2.4, ch 6 | 2.4 s | 26 in 45 s | -38 | PASS |
| Wi-Fi NAN, ch 6 | Wi-Fi 2.4 | WifiNan / 2.4, ch 6 | 1.6 s | 33 in 45 s | -40 | PASS |
| Wi-Fi beacon, ch 149 | Wi-Fi 5 (`0x8a0c`) | WifiBeacon / 5, ch 149 | 2.8 s | 27 in 45 s | -41 | PASS |
| Wi-Fi NAN, ch 149 | Wi-Fi 5 | WifiNan / 5, ch 149 | 2.8 s | 30 in 45 s | -41 | PASS |
| Bluetooth 4 legacy | Bluetooth (`0x6749`) | BleLegacy | 1.8 s | 158 in 45 s | -58 | PASS |
| Bluetooth 5 extended | Bluetooth | BleExtended | 2.5 s | 76 in 45 s | -57 | PASS |
| Bluetooth 5 Long Range | Bluetooth | BleLongRange | 2.6 s | 55 in 45 s | -57 | PASS |

The first legacy advert carries one message, not always the Basic ID, so its slot appears a moment
before the ID does; the next adverts fill it in on the same track. No transmission showed up in
another transport's slot.

### 2. Fake open FPV links through the hub's open-link table

All 16 PASS: each system appeared within 1.5-3.2 s of its key, with the fake ID recognised
(`id=fake`; OpenHD has no clear ID: `none`).

| Channel | Node role | wfb-ng | OpenHD | RubyFPV | DroneBridge | RSSI |
|---|---|---|---|---|---|---|
| 6 | Wi-Fi 2.4 | 1.5 s, 23 reports | 1.7 s, 25 | 1.7 s, 24 | 1.7 s, 28 | -39 |
| 149 | Wi-Fi 5 | 2.2 s, 22 | 1.8 s, 24 | 2.4 s, 22 | 2.8 s, 24 | -41 |
| 161 | Wi-Fi 5 | 3.0 s, ~19 | 3.2 s, ~18 | 1.8 s, ~21 | 2.4 s, ~19 | -43 |
| 165 | Wi-Fi 5 | 2.8 s, ~18 | 3.2 s, ~14 | 2.0 s, ~17 | 2.2 s, ~18 | -45 |

("reports" = the node's once-a-second sightings the hub received in that 30 s; the same link on a
second channel is the same table entry, so 161/165 are the increase.) One extra stream: the ch 6
wfb-ng link was also reported on **ch 1** once, while the node visited 1: at a metre the 2 dBm
signal is decoded 25 MHz off its channel, and the node labels a frame with the channel it is tuned
to. A real effect of the bench distance, not a fault; it would not happen at range.

### 3. Role switching under the hub

| Role | Flash done -> hub assigns | Node applies | First detection on the hub |
|---|---|---|---|
| Wi-Fi 2.4 (already running it) | 4.6 s (plan echoed; same image) | at once | detections never stopped |
| Wi-Fi 5 (same image) | 1.0 s | at once (0 band or channel errors) | 2.2 s after boot |
| Bluetooth (image switch) | 2.0 s | node-wifi switches image; node-ble up 1.0 s later | 4.0 s after the first boot |

Flashing itself takes 15-20 s before that. Every assignment was `applied=true` on the hub after.

### 4. DroneID (chip only)

`tools/droneid-hil` on `c5b` (node-sdr `cargo fw-now`): the 13 public-recording bursts at two
levels. 200 codes (9.4 dB): 13/13 detected, 13/13 CRC-valid, median decode 151 ms (max 158).
150 codes (6.9 dB): 13/13, 13/13, median 144 ms (max 151). Same as 2026-11-14: no regression.
**Chip only by design:** HIL sightings are never sent to the hub, so a recording can never appear
as a live drone. A real DroneID on the hub needs a real DJI drone.

### 5. FPV scanner, 2 minutes, through the hub

`c5b` pinned to FPV (`role fpv`), reporting through the gateway (32 uplink windows, 32 ACKed).
58 sweeps of the 31 reachable channels; 84 loud dwells analysed: 77 `wifi`, 7 `energy`,
**0 `analog_video`**, 0 `fpv_carrier`, 0 `hdzero`, 0 `lte_style`, 0 `wifi_narrow`. The hub's RF
table stayed empty (`rf=0` in every summary): Wi-Fi and plain energy are never sent, as designed.
PASS as a sanity check. (The hub kept asking for DroneID, `applied=false`, because of the pin.)

### 6. SD card

Each hub boot counted the previous file on the card (read back on the device, every row):

| File | Session | Rows | Test drone rows | Open-link rows | Kinds |
|---|---|---|---|---|---|
| LOG00010 | Wi-Fi 2.4, A | 138 | 137 | 0 | WifiBeacon, WifiBeacon\|WifiNan |
| LOG00011 | Wi-Fi 2.4, B | 508 | 149 | 358 | + WfbNg, OpenHd, RubyFpv, DroneBridge |
| LOG00012 | Wi-Fi 5, A | 114 | 113 | 0 | WifiNan5G, WifiBeacon5G\|WifiNan5G |
| LOG00013 | Wi-Fi 5, B | 1205 | 150 | 1054 | + WfbNg5G, OpenHd5G, RubyFpv5G, DroneBridge5G |
| LOG00014 | Bluetooth, A | 113 | 112 | 0 | BleLegacy\|BleExtended\|BleLongRange |
| LOG00016 / 17 | Bluetooth (after the fix) | 139 / 200 | 137 / 198 | 0 | BleLegacy, ...\|BleExtended\|BleLongRange |

(Each count includes the header line. The one "other drone" row in 16 and 17 is the Bluetooth
track's first second, before its Basic ID arrived: an empty ID, not a foreign drone.) Every
flush read-back OK, 0 errors, 0 dropped rows. Nothing deleted or formatted.

### 7. Health over the whole run

Ten hub sessions, about 34 minutes logged, 11,228 frames from the gateway: **2 corrupt frames,
8 UART errors** (5 in one Wi-Fi 2.4 session, 1, 2), **0 ring_full**, 0 lcd DMA errors. Loop
max_gap 6.8-7.2 ms normally (the screen band); worst 17.0 ms, at SD flushes (BRINGUP: 18.3 ms).
(Later finding, 2026-11-18: max_gap is not the loop's worst stall, because its clock restarts after the
SD mount; the SD stats line's `max_tick` is the real worst figure. Measured: about 381 ms for identify + mount, 131 ms for a pack read.)
Node uplinks: 0 failed sends.

### Bug found and fixed: Bluetooth node reset under Long Range traffic

`node-ble` with ESP-NOW reset itself in the controller,
`ASSERT r_ble_lll_mmgmt_block_cnt_check:669 0 1`, while the beacon sent all three Bluetooth
transports: 4 resets in 120 s (first run: 3 in 100 s). Isolated on `c5a`:

| Build, beacon | Resets |
|---|---|
| ESP-NOW, controller-timed 800 ms scans: all three / legacy only / extended only / Long Range only | 4 in 120 s / 0 in 90 / 0 in 90 / 1 in 90 |
| ESP-NOW, bigger HCI event buffers (24 x 257 B) | 5 in 150 s (no help) |
| Wired (no ESP-NOW, one endless scan), all three | 0 in 150 s |
| ESP-NOW, one endless scan, all three | 0 in 150 s |
| **ESP-NOW, host stops each 800 ms scan (the fix)**, all three | **0 in 180 s** (and 0 in the 100 s hub run) |

So the trigger is the controller ending a scan by its duration while Long Range (coded PHY) reports
are in flight. Fix (`firmware/node-ble/src/bin/main.rs`): no controller duration; the host drops
the scan session after `SCAN_ON_MS`, which disables the scan. Same 800/200 split. Uplink with the
fix, 180 s: 178 gaps, 220 sends, 0 failed, 44 gateway beacons and 30 hub commands heard (fewer
beacons than the 0.4/s with the controller stop, but commands still arrive). Each reset cost about
2 s of blindness; the hub track survived them.

### Not testable with our hardware

| Signal | Needs |
|---|---|
| Real DJI OcuSync / O4 DroneID on the hub | a real DJI drone (we never transmit DroneID) |
| DJI legacy Wi-Fi DroneID | a Wi-Fi DJI (Spark, Mavic Air, Mini SE); we never transmit it |
| Analog FPV video | a 5.8 GHz VTX and an amateur radio licence |
| HDZero, Walksnail | an HDZero VTX / Walksnail kit (HDZero scanning is built, synthetic tests only) |
| ExpressLRS | an LR2021 radio for the LR2021 node (`firmware/node-lr`, `crates/ctrlwatch`: built, no hardware yet) |
| Wi-Fi drone makers (Tello, Parrot, ...) | a real one; we never imitate a maker's network |
| China GB 46750 Remote ID | a test-beacon mode or a DJI on 2026 firmware |
| wfb-ng's real LDPC/STBC video | OpenIPC hardware |

## Crash recovery (2026-11-18, first hardware pass the same day)

Run on 2026-11-18 for the P4, gateway, node-wifi C5 and node-sdr C5 (steps 1 to 4 and 6, 8; step 5 not checked): see "Bench results, 2026-11-18" at the end. Not run: node-ble, node-lr, S3 boards.

Every board now reboots after a panic, and every board except `rid-beacon` has a hang watchdog
(the chip's RTC watchdog) that resets it if its main work stops. After any restart the board's
`hello` line says why it last reset: `"reset":"power_on"`, `"software"` (a panic, or a restart the
firmware chose: image switch, BLE host restart, gateway epoch roll-over), `"watchdog"`, `"usb"`
(the flasher or a monitor reset it), `"brownout"`. The P4 hub says `last reset: ...` in its
`hub-p4: hello` line and in the evidence log's boot event.

| Board | Watchdog fed by | Timeout | Panic: back after | Hang: back after |
|---|---|---|---|---|
| `gateway` | relay loop, only while the beacon loop is alive (5 s) | 10 s | ~0.5 s + boot | ~10 s |
| `node-wifi` (S3, C5, `band24`, `espnow`, `wirebus`) | channel hopper, every hop | 30 s (a hub plan may dwell 10 s) | ~0.5 s + boot | ~30 s |
| `node-sdr` | capture loop, every pass, and every dump line | 10 s | ~0.5 s + boot | ~10 s |
| `node-ble` (S3, C5) | stats task, once a second | 10 s | ~0.5 s + boot | ~10 s |
| `node-lr` | stats loop in `main`, once a second | 10 s | ~0.5 s + boot | ~10 s |
| `hub-p4` | main loop, every pass | 30 s | 0.5 s (5 s within 10 s of boot) + boot | ~30 s |
| `rid-beacon` | (no watchdog) | | ~0.5 s + boot | |

The RTC watchdog runs from the chip's internal slow oscillator, so "10 s" may come out a few
percent either way. "Boot" is the board's normal start-up (a second or two for the nodes; longer for
the P4, which tests its PSRAM first).

### The `faulttest` build

To trigger the faults on purpose, build with the feature `faulttest`. It is in no `default` list, so
a normal build cannot be made to panic or hang from the console; its `hello` line says
`"faulttest":true` and `"bench":["faulttest"]` (P4: `FAULT TEST BUILD`, and `BENCH BUILD` on the
screen). It is a bench feature: `export ROTORSCOPE_BENCH=1` in the shell first, or the build
stops (see "Bench builds" below). Type a whole line and Enter:

* `fault panic`: the board panics (backtrace on USB) and must come back on its own.
* `fault hang`: the board stops feeding the watchdog (spins without yielding) and must come back
  after the timeout above.

Nodes and the gateway read it on their USB-Serial-JTAG port (the same port their JSON comes out
of); node-sdr through its normal console; the P4 hub on the USB-C (CH340) console, where `f`
starts the line (Esc cancels).

| Board | Build (from the firmware's folder, as usual, plus the feature; `ROTORSCOPE_BENCH=1` exported) |
|---|---|
| gateway | `cargo build --release --features faulttest` |
| node-wifi C5 | in `c5/`: `cargo build --release --features esp32c5,faulttest` (add `band24`, `espnow` as for `fw24` / `fw24-now`) |
| node-wifi C5, ESP-NOW | in `c5/`: `cargo build --release --features esp32c5,band24,espnow,faulttest --target-dir ../target-espnow` |
| node-wifi S3 | in `s3/`: `cargo build --release --features esp32s3,faulttest` |
| node-sdr | in `c5/`: `cargo build --release --features faulttest` |
| node-sdr, ESP-NOW node | in `c5/`: `cargo build --release --features espnow,faulttest --target-dir ../target-espnow` |
| node-ble S3 / C5 | `cargo build --release --features faulttest` / `cargo +stable build --release --no-default-features --features esp32c5,faulttest --target riscv32imac-unknown-none-elf` |
| node-lr | `cargo build --release --features faulttest` |
| hub-p4 | `cargo build --release --features faulttest` (flash as in firmware/hub-p4/BRINGUP.md) |

(`ROTORSCOPE_BENCH=1` exported for all of them. Since the 2026-11-18 cut-over no build needs a key: an ESP-NOW node's key is in its `rskey` partition, see "Link keys: bench checklist".)

### The checklist (one board at a time)

Flash the `faulttest` build with the board's usual tool (`tools/dev-board.sh <elf> 90 <board>` for
the S3 and C5 boards; the P4 as in its BRINGUP.md). Note: opening the serial port resets the board,
so the first `hello` after flashing says `"usb"` or `"power_on"`, not a fault. On the P4 this is true
every time (ROM prints `POWERON`), even with DTR/RTS held low before the port is opened. Also on the
P4: it booted by itself right after flashing, both times on 2026-11-18, so it may need no reset (it
may still need one on some runs).

1. **First boot.** The `hello` line has `"faulttest":true` and a `reset` field. P4: the boot log
   also has `hub-p4: watchdog on: resets the chip if the main loop stops for N s`, with N close
   to 30 (between 15 and 60 is accepted; anything else prints `watchdog LEFT OFF` and is a FAIL to
   report with the numbers it shows).
2. **Normal running, 5 minutes.** No restart (`up_us` / uptime keeps growing, no second `hello`).
   This is the important one on the P4: run it with the SD card in, an airspace pack on the card
   (so the pack's load and check happen during the run) and the screen on. A restart here means
   the timeout is too short for something legitimate: record the last lines before it.
3. **Panic.** Type `fault panic` (for example
   `tools/dev-board.sh --log 40 c5b --send $'fault panic\r'`). Expect the `PANIC` banner with
   `faulttest: panic requested on the console`, a backtrace, then a fresh `hello` with
   `"reset":"software"` within the time in the table.
4. **Hang.** Type `fault hang` (`--send $'fault hang\r'`, reading for at least the timeout + 20 s).
   Expect the `faulttest ... hang` line, silence for about the timeout, then a fresh `hello` with
   `"reset":"watchdog"`. Time it: it should match the table within ~20 %. Known limit of this check
   on the C5 nodes: the USB port drops and returns about 0.4 s after the reset and the node's
   `hello` is printed before the port is back, so that line cannot be seen over USB. The restart
   TIME is the observable for nodes. (The gateway's `hello` prints later and is seen; the P4 uses a
   UART bridge and is seen.) Follow-up idea: repeat the reset reason in a later status line.
5. **Hub view (nodes behind a hub).** During 3 and 4 the hub shows the node going quiet, then its
   Hello again; nothing else on the hub changes.
6. **P4 only: evidence.** After step 4, the evidence file's boot event for the new boot ends with
   `last reset: watchdog` (console `r` replays it, or read the card on the laptop).
7. **P4 only: panic at start-up.** Not testable from the console; it is covered by the 5 s wait in
   `fault.rs`. If you ever see a P4 boot-looping, the panic message repeats every ~5 s plus boot.
8. Finish by flashing the normal build again (no `faulttest`, in a shell without
   `ROTORSCOPE_BENCH`) and checking its `hello` has `"faulttest":false` and `"bench":[]`.

Under a Wi-Fi frame flood, `node-wifi` must not reset: the drain task now yields to the hopper
every 2 ms. If a busy bench (many access points, a `rid-beacon` at 10 Hz) ever shows
`"reset":"watchdog"` on a node-wifi without `fault hang`, that is a FAIL worth reporting.

## Bench builds (2026-11-18, compile-checked only)

Some cargo features and build switches exist only for the bench: they fake data (a made-up drone
whose frames are written to the **signed** evidence log, a made-up GPS fix), loop the board's own
pins back, or open debug doors on the console (panic on request, inject samples, un-redact serial
numbers, drop to the ROM loader). Nothing in an image used to tell such a build from a field build.
Now:

* **The build refuses** unless the environment has `ROTORSCOPE_BENCH=1`. Each firmware's `build.rs`
  checks its own list (`crates/link/src/benchfence.rs`) and stops with a message naming what is on.
  For a bench session: `export ROTORSCOPE_BENCH=1` in that shell only. `tools/bench/run.py` sets it
  for its own runs.
* **The image says so.** Every hello line has `"bench":[...]` (`[]` for a normal build), and a bench
  image prints `{"t":"warn","what":"bench_build","tag":"bench-build:<list>",...}` after it. The P4
  hub lists them in its `hub-p4: hello` line and in the evidence log's boot event
  (`...; bench: fakedrone,faulttest`, or `bench: none`), and shows **BENCH BUILD** on the screen
  for as long as it runs.
* **The flashing tools check.** `tools/dev-board.sh` and `tools/flash-node.sh` find the
  `bench-build:` tag in an ELF and refuse to flash it without `ROTORSCOPE_BENCH=1` (and warn with it).
  The hub-p4 ELF carries no such tag (its hello line and evidence boot event carry the list instead)
  and no tool reads P4 ELFs, so a P4 image is not checked at flash time.

| Firmware | Bench features | Bench build switches (environment at build time) |
|---|---|---|
| gateway | `faulttest` | |
| node-wifi | `faulttest`, `forgetest` (send frames a node may not originate: "Origin checks" below), `floodtest` (synthetic frames into the node's own receive path: "Frame flood" below) | `ROTORSCOPE_BENCH_CAN`, `ROTORSCOPE_BENCH_FLIP` |
| node-ble | `faulttest` | `ROTORSCOPE_BENCH_CAN`, `ROTORSCOPE_BENCH_FLIP` |
| node-lr | `faulttest` | |
| node-sdr | `faulttest`, `bringup` (the bring-up console: BRINGUP.md "Bring-up builds") | `NODE_SDR_BOOT`, `NODE_SDR_ALLOW_UNVERIFIED_PHY` |
| hub (S3/C5 web head) | `demo` (fake drones, a made-up position, the build machine's clock), `open-hotspot` (no hotspot password) | |
| hub-p4 | `selftest`, `fakenode`, `fakedrone`, `benchlog`, `gpsloop`, `gpsloop-m10`, `webheadloop`, `embedpack`, `modesloop`, `faulttest`, `testpoint` (console `p`: own position to a fixed test point) | (`AIRSPACE_PAK` only with `embedpack`) |

Not fenced, on purpose: `rid-beacon` (it is the test transmitter; nothing fields it), hub-p4
`show-link-ids` (an operator privacy choice, not a bench aid), `modes` (a field feature), node-wifi
`ROTORSCOPE_ESPNOW_RATE` (a radio tuning). The S3 web head (`firmware/hub`) has `"bench"` in its
`boot` line (it has no `hello` line) and in `/api/state`, where the phone page shows a striped
**BENCH BUILD, NOT FOR USE** banner.

**Link keys.** Before the 2026-11-18 cut-over an ESP-NOW build needed the kit key
(`tools/link.key`) or `ROTORSCOPE_LINK_KEY_FILE=none`. Now no build reads a key: each board's key or
open record is written to its flash by `tools/link-key`, and `tools/flash-node.sh` wants `--open`
only to call a node with an open record good ("Link keys: bench checklist"). The bench tests in
`tools/bench/run.py` build only wired images.

### To check on hardware (not yet run)

Everything in this section, and the hub-p4 screen and evidence changes below, is compile-checked
only (2026-11-18, branch `sec/lane-a`). On the bench:

1. **Normal builds**: each board's hello has `"bench":[]` and no `bench_build` warning; the P4's
   hello says `evidence key: public_test (...)` (or `development_software`) and `bench: []`.
2. **A bench build** (e.g. node-wifi with `faulttest`): `"bench":["faulttest"]` plus the warning
   line; `tools/dev-board.sh <elf>` without `ROTORSCOPE_BENCH=1` refuses it before touching the
   board, with it flashes and warns.
3. **P4 screen, test key**: a strip at the bottom edge reads `TEST KEY: log not evidence` on every
   page (with a dev key: `DEV KEY: not evidence-grade`); a bench build adds `BENCH BUILD`. Check it
   is readable in both themes and does not hide anything needed (it covers the bottom 11 px).
4. **Open link** (since the cut-over the hub's record decides, not the gateway build):
   `link-key open p4 --hub`; the P4 shows `OPEN LINK` after its next boot, and it goes away after
   `link-key hub p4` and a reboot.
5. **Evidence not written**: (a) boot the P4 with no card: the strip shows
   `NO EVIDENCE (no SD): N waiting` with N growing; insert a card: the line goes away and REC shows.
   (b) On a spare card, create a *file* `ROTORSCP/EVIDENCE/EV000005.EVL` (any content) and a
   *folder* `ROTORSCP/EVIDENCE/EV000006.EVL`: the next boot writes `EV000007.EVL` (the folder's
   name is skipped) and links it to `EV000005.EVL`. Then add a *file* `EV999999.EVL`: the next
   mount uses the lowest free number (console: `using the lowest free number 1`) and evidence
   keeps recording.
   (c) Make `ROTORSCP/EVIDENCE` a *file* instead of a folder: the CSV still records, the status
   bar shows a boxed `NO EVID` instead of `REC`, the health page says `CSV only - NO EVIDENCE`,
   the strip says `NO EVIDENCE (file error): N waiting`, and the console prints a retry every
   15 s with the same `ev_errors` growing (no `TooManyOpenDirs` after many retries). Pull the
   card, delete the file on a laptop, put the card back: evidence records again (into the same
   new file chain, the queued records first).
6. **Console `p`** on a normal P4 build answers `p (test position) is in bench builds only`; a
   `testpoint` build sets the test position and logs the Setting event as before.
7. **node-sdr**: a normal build answers `freq 5756`, `reveal on`, `bootloader` and an `@F` line
   with `"e":"bring-up command: needs a bringup build (BRINGUP.md)"`; `stats`, `phy`, `plan` still
   work. A `bringup` build runs BRINGUP.md as before, and `tools/droneid-hil` passes on it.
   `@F fffffff8 AAAA` must be counted as a feed error (`hil` counters) and change nothing.
8. **Over-the-air text**: with `rid-beacon` or a fake open network whose SSID holds a control
   character (if the beacon can send one), the P4's `THREAT` console line and evidence text show
   `?` in its place.

## Origin checks (2026-11-18, gateway case run on hardware)

Item 1 below (keyed gateway, node-wifi `forgetest`) was run on 2026-11-18; see "Bench results, 2026-11-18". Items 2 to 7 were not run.

*Since the cut-over (protocol 14, the same day) the gateway holds no key and does not read frames:
the P4 hub verifies each node packet and does the origin check itself. Items 1 and 2 below, and
item 5's "old gateway", describe the gateway before it; for the current link the expected results
are in "Link keys: bench checklist" (the `forge` frame commands raise the hub's own `forged_link`
detail 5 and its `refused=`, and the gateway's counters do not move). Item 7's gateway table no
longer exists.*

Who may send which frame type (docs/ESPNOW.md "Who may send what") is checked by the gateway, the P4
hub, the P4's web-head feed, the S3 web head and the wired bus. Host tests cover the rules (`cargo test -p link -p
wirebus`); the firmware changes are compile-checked only. On the bench, a node built with the bench
feature **`forgetest`** sends frames it must never originate, and both hops must refuse them.

### The `forgetest` build (node-wifi only)

`export ROTORSCOPE_BENCH=1` first (bench fence). Not together with `faulttest` (both read the USB
console). From `firmware/node-wifi/c5/`:

| Link under test | Build |
|---|---|
| ESP-NOW through the gateway | `cargo build --release --features esp32c5,band24,espnow,forgetest --target-dir ../target-espnow` |
| Wired bus | `cargo build --release --features esp32c5,band24,wirebus,forgetest --target-dir ../target-bus` |
| Point-to-point wire (the P4 in legacy mode) | `cargo build --release --features esp32c5,band24,forgetest` |

Flash it alone with `tools/dev-board.sh <elf> 90 <board>` (it refuses a bench ELF without
`ROTORSCOPE_BENCH=1`). Its `hello` has `"bench":["forgetest"]` and a `bench_build` warning, then
`{"t":"forge","note":"FORGE TEST BUILD: ..."}`. Type a whole line and Enter (for example
`tools/dev-board.sh --log 20 c5a --send $'forge hubfix\r'`); each answers `{"t":"forge","sent":"..."}`:

| Command | Sends | Must never |
|---|---|---|
| `forge hubfix` | a `HubFix` at 0 N 0 E, UTC in 2001 | move the web head's map or clock |
| `forge csbee` | an ADS-B line for a made-up aircraft | show an aircraft `FA4E00` |
| `forge flags` | an FAA flag report for `FORGED` | show a flag |
| `forge via` | `Via` naming 02:00:00:00:00:99, then a `Stats` | create node 02:00:00:00:00:99 on the hub |
| `forge hello` | `Hello` role Gateway, `gw key FAKE` | change the hub's `"gw"` field or `OPEN LINK` state |
| `forge threat` | `forged_link` report naming 02:00:00:00:00:99 | appear as the gateway's report |
| `forge join` | a `BusJoin` | reach the bus master or the tracker |

### To check on hardware (item 1 run 2026-11-18; the rest not yet run)

1. **Gateway (keyed kit; c5a forgetest node, c5c gateway, P4 hub).** After each `forge` command:
   the gateway's `"t":"gw"` line's `refused` goes up by one (for `forge via` the `Via`; the `Stats`
   after it is relayed under c5a's own address), and within a second a `{"t":"threat","kind":"forged_link",
   "src":"<c5a>","why":"origin",...}` line. On the P4: `hub-p4: THREAT Forged node traffic`: one console line in total,
   naming `node 0` (the gateway reports it; IDs are not shown by default); later forgeries only raise
   that threat's count (detail 5), no new line; its `hub-p4: link` line's `refused=` does **not** move (the gateway already
   dropped the frames). Nothing in the "Must never" column happens on the P4 screen or the web head.
2. **Gateway, open kit.** The same with open records on the hub and the node (before the cut-over:
   images built without a key) (`OPEN LINK` on the P4): the same refusals; the rules do not depend
   on the key.
3. **P4, wired bus (forgetest node on the bus).** After each command the `{"t":"hub"}` line's
   `"bus":{...,"refused":N}` goes up, and within a second `hub-p4: refused 1 frame(s) from <node
   MAC> ...` plus a `THREAT Forged node traffic` line. `forge via`: the `Stats` arrives under the
   node's own MAC; no node 02:00:00:00:00:99 appears in the node list.
4. **P4, point-to-point wire (legacy mode).** `forge hubfix`, `csbee`, `flags`, `join`: the
   `hub-p4: link` line's `refused=` goes up and the hub raises its own `forged_link` with an
   all-zero source (the link's own stream). `forge hello` and `forge threat` are **accepted** here
   on purpose: a frame with no `Via` on a point-to-point link is the gateway's own. `forge via`: the
   `Via` names the node's `Stats`, so node 02:00:00:00:00:99 appears: that is what a one-node wire
   can do, and why the bus is the default.
5. **Old gateway (defence in depth).** Flash the gateway built from a commit before this change
   (for example `git show 0d0bfe3:firmware/gateway/src/main.rs`; build it in a scratch checkout),
   keep the P4 on this change. `forge hubfix|csbee|flags|hello|threat|join` now reach the P4 after a
   `Via`: its `refused=` goes up for each, a hub `forged_link` names c5a, and nothing in the "Must
   never" column happens. `forge via`: one refusal (the `Via` after the gateway's `Via`); the
   `Stats` is c5a's, and the gateway's next `Hello` still sets `"gw"` (no lingering).
6. **Web head.** The `hub-p4: webhead` line's `refused=` moves with the `hub-p4: link` line's
   `refused=` (so not in 1 and 3, where the gateway or the bus already dropped the frames), and
   the web head shows none of the forged frames in any of 1-5. Then wire the forgetest node (the
   point-to-point build) straight to the S3 web head's link 1 (`webhead` build): `forge hubfix`,
   `csbee`, `flags` and `join` raise the S3 `hub` line's `relay_refused` and change nothing on the
   page; on link 0 (the P4's) the first three are taken, by design.
7. **Node table.** With 16 relayed nodes known to the P4 (the bus and a gateway together, or
   `fakenode` plus real nodes), power one node off; after 60 s a new node takes its slot
   (`evicted=` on the `hub-p4: link` line goes up); before that the new node's frames count as
   `table_full=`. A keyed gateway with 16 nodes: a 17th node's packets count as `table_full` on the
   `"t":"gw"` line and arrive on the P4 as `forged_link` detail 6.
8. Finish by flashing the normal node build (no `forgetest`) and checking `"bench":[]`.

## Frame flood (node-wifi, 2026-11-18, compile-checked only)

Branch `sec/p2-a`. Nothing here has run on hardware yet: the firmware is compile-checked (C5
release builds for the wired and ESP-NOW nodes, `cargo check` for the other feature sets and the
S3), the decisions are host-tested (`cargo test -p wifiwatch -p openlink`).

**What a flood used to do.** Anyone in radio range can forge cheap frames that each became one hub
frame: a beacon named `ESP_RIDS` / `pwned` / `GhostNet` (an attack-tool `Threat`), a drone maker's
address block plus a drone network name (`WifiDrone`), a DJI DroneID element (`DjiWifi`), wfb-ng
frames with more than 16 link ids (up to 16 `OpenLink` frames every 250 ms), a Remote ID beacon from
a new random address (`Detection`). Per-address limits do not help against fresh random addresses.
All of it shared one 8-frame hub queue that dropped the newest frame when full, so a real drone's
Remote ID was lost whenever it met a full queue (most of the time over ESP-NOW, which drains only
while the radio is on the gateway's channel). The USB stream was written with interrupts masked,
an attacker-sized Remote ID line at a time.

**What the node does now** (`wifiwatch::shed`, `report.rs`, `usb.rs`, `detect.rs`, `rx.rs`):

* **Reserved room.** Of the 8 hub-queue slots, Remote ID from a *known* source may use all 8, the
  node's own `Hello` / `Status` / `Stats` 6, Remote ID from a *new* source 5, every alarm kind 4.
  A source is known once heard twice within 2.5 s; a random-address flood is heard once per address
  and never becomes known, and the source table (32 slots, salted hash) never gives up a slot a known
  source still uses. A known source is limited to 20 frames a second (bursts of 10).
* **Budgets.** Token buckets: `Threat`, `WifiDrone`, `DjiWifi` 8 a second (burst 8), `OpenLink`
  16 (burst 16), new-source Remote ID 8 (burst 8). A frame over its budget is not printed on USB
  either. `open_links` takes at most as many due streams as there is room for, longest-waiting
  first, so a full table goes out in turns.
* **It says so.** The USB stats line has `shed` (frames turned away per kind), `rid_src` (known
  sources), `usb_drop`, `alt`, `clamp`; `hub_drop` includes everything shed; the hub's `Stats`
  frame's `dropped` is now receive-queue drops plus hub frames not sent. **No link wire format
  changed**: the per-kind split is only on USB, because `Stats` has no field for it.
* **Spoofed address.** DJI DroneID and Wi-Fi drone reports are limited per (address, content), at
  most 2 contents per address a second, so frames forged from a drone's own address with a false
  position no longer win every second. An address alternating between contents (A, B, A) is flagged:
  `"alt":true` on its `dji` / `wifidrone` lines for a minute, `alt` counts in the stats line.
* **USB.** An async writer; lines are dropped and counted (`usb_drop`) when the host does not keep
  up, never written with interrupts off. The Remote ID line is formatted straight into the writer's
  one 6 KB buffer.
* **Receive length.** Every received frame is clamped to what one driver receive buffer (1.6 KB)
  holds after its header before anything reads it (`clamp` counts frames whose `sig_len` claimed
  more).

RAM (C5 wired build, static `.data` + `.bss`): +3 976 bytes (84 KB to 87.8 KB). USB pipe 2 048,
hub admission (`Shed`) ~900, DJI and Wi-Fi drone limiters 2 x ~600 (were 2 x ~260); the drain task's
6 KB JSON buffer became the USB writer's Remote ID buffer (no change).

**Known limits.** A drone that first appears *during* a random-address Remote ID flood is reported
through the new-source budget (8 a second, shared with the flood) until its slot in the source table
frees: seconds at 200 forged frames a second, longer at higher rates (host test
`a_drone_that_starts_during_a_flood_becomes_known_eventually`). Within one alarm kind, real and
forged reports share the kind's budget (a `flood dji` sheds the real DJI drone's reports too; Remote
ID is never affected). A forger sending many *different* contents from a drone's address still takes
most of that address's places (the address is flagged). The 32-slot receive pool in front of all
this drops whatever the drain task cannot parse in time (`drop`), real frames included, as before.

### The `floodtest` build

`export ROTORSCOPE_BENCH=1` first (bench fence). Not with `faulttest` or `forgetest` (all read the
USB console). It **transmits nothing**: it pushes synthetic frames into the node's own receive path
(`rx::receive`, where the radio callback puts real ones), on the channel the hopper is tuned to. From
`firmware/node-wifi/c5/`:

| Node | Build |
|---|---|
| Wired (point-to-point), 5 GHz plan | `cargo build --release --features esp32c5,floodtest` |
| Wired, 2.4 GHz plan | `cargo build --release --features esp32c5,band24,floodtest` |
| ESP-NOW through the gateway | `cargo build --release --features esp32c5,band24,espnow,floodtest --target-dir ../target-espnow` |
| Wired bus | `cargo build --release --features esp32c5,band24,wirebus,floodtest --target-dir ../target-bus` |

The `hello` has `"bench":["floodtest"]` and the `bench_build` warning, then
`{"t":"flood","note":"FLOOD TEST BUILD: ...","packs_ok":true,"drone_ok":true}` (both must be true).
Type a whole line and Enter (`tools/dev-board.sh --log 20 c5a --send $'flood rid 500\r'`):

| Command | Injects |
|---|---|
| `flood rid <n>` | `n` Remote ID beacons a second, each from a new random address with a new id (`FLOODFAKE` + ten digits, so a hub sees a flood of never-confirmed drones) |
| `flood tool <n>` | beacons named `ESP_RIDS`, `pwned`, `GhostNet` in turn, random addresses |
| `flood drone <n>` | beacons from DJI's first address block with random low bytes, named `MavicAir-FLOOD` |
| `flood dji <n>` | beacons with a DJI DroneID element (serial `FLOODDJI00000001`), random addresses |
| `flood wfb <n>` | wfb-ng data frames, link ids `0x7E57C0`.. (64 in turn) |
| `flood spoof <n>` | DJI DroneID elements from the real drone's address, one fixed false position |
| `flood crowd <n>` | `n` fake drones kept alive: each one Remote ID beacon a second from its own fixed address (`02:c2:0d:00:..`) with its own id (`FLOODFAKE9` + nine digits). Tests the hub's population rule (32 drone IDs active at once, docs/SPOOFING.md "The slow rules"); not in the `flood` status line |
| `flood real on` / `off` | the "real" drone at `02:52:45:41:4c:01`: one Remote ID beacon a second (id `FLOODREAL0000000001`) and one DJI DroneID element a second (serial `FLOODREAL0000001`, sequence moving on) |
| `flood off` / `flood` | everything off / print the state |

Rates are per second, up to 5 000 per kind; kinds mix. Each command answers, and every second while
anything runs the node prints `{"t":"flood","rate":{...},"sent":{...},"real":{"on":true,"sent":N,
"seen":N,"admitted":N,"queued":N,"dji_sent":N}}`: frames injected per kind (totals), and for the
real drone's Remote ID: injected, reached the drain task (not lost in the receive pool), admitted,
queued for the hub.

### To check on hardware (not yet run)

One `floodtest` node, wired to the P4 (or behind the gateway for step 5). Record the `stats`,
`flood` and hub lines. "Rises by about X a second" means between two `stats` lines a second apart.

1. **Real drone alone, 60 s.** `flood real on`. Every second `real.sent`, `real.seen`,
   `real.admitted` and `real.queued` rise by 1 and stay equal; `rid_src` is 1 from the second
   second on; `dji` rises by 1 a second; every `shed` count stays 0; `alt` stays [0,0]. On the hub
   the node's detections rise by 1 a second and a track `FLOODREAL0000000001` is shown.
2. **One flood at a time, 60 s each, real drone on, 500 a second.** In every case `real.queued`
   equals `real.sent` (none lost) and `shed.rid_known` and `shed.health` stay 0:
   * `flood tool 500`: `shed.threat` rises by about 490 a second; the hub gets at most 8 threats a
     second (attack tool, beacon flood).
   * `flood drone 500`: `wdrone` rises by about 8 a second, `shed.wdrone` by about 490; `threat`
     and `shed.threat` stay flat. (An earlier version of this list expected clone alarms here. The
     clone check (`wifiwatch::drones::Clones`) wants one copy address heard again and again; every
     forged beacon here has a new random address, heard once, so none becomes a copy. Bench
     2026-11-18 agreed: `threat` unchanged over the 60 s.)
   * `flood dji 500`: `dji` rises by about 8 a second, `shed.dji` by about 490 (the real DJI
     element is mostly shed here: it shares the DJI budget).
   * `flood wfb 500`: `open` rises by about 500 a second, `open_ev` rises (64 ids, 16-entry table),
     the hub gets at most 16 open-link frames a second, `shed.open` stays near 0.
   * `flood rid 500`: `rid` rises by about 500 a second, `shed.rid_new` by about 490, `rid_src`
     stays 1; the hub's detections for the node rise by about 9 a second (8 forged, 1 real) and the
     `FLOODREAL0000000001` track keeps updating every second.
   * `flood spoof 100`: `dji` rises by about 2 a second (the forger's content and the real one),
     `alt[0]` rises, the `dji` lines from `02:52:45:41:4c:01` say `"alt":true`, and the hub keeps
     getting the real element (serial `FLOODREAL0000001`, moving sequence) every second.
     (Before this change: the forger's every second, the real one almost never.)
3. **Everything at once, 120 s.** `flood real on`, then `flood rid 300`, `flood tool 300`,
   `flood drone 300`, `flood dji 300`, `flood wfb 300`. Expect: no restart (`up_us` keeps growing,
   no new `hello`, never `"reset":"watchdog"`); every channel's `per_ch` visit count (third number)
   keeps rising each second and `scout` keeps rising (the hopper keeps hopping); `real.queued`
   equals `real.sent`; `shed.rid_known` and `shed.health` 0; the hub's `Stats` for the node keep
   arriving every second and its dropped count grows by about the `shed` total. If `drop` (the
   receive pool) rises and `real.seen` falls behind `real.sent`, the drain task cannot parse that
   many frames a second: lower all rates together until `drop` stays flat and record that total
   (the node's parse capacity, a separate limit from this work).
4. **Step 3 at 1 000 a second per kind**, 60 s: the same, except `drop` may rise (record it); the
   watchdog must still not fire and `per_ch` visits must keep rising.
5. **ESP-NOW (2.4 GHz node behind a keyed gateway, P4 hub).** Step 3 again: `real.queued` equals
   `real.sent`, and the hub's detections from this node include one a second from
   `FLOODREAL0000000001` throughout. A 5 GHz plan (40 ms a second on channel 6) should give the
   same result; try it if time allows.
6. **USB not read.** Start step 3, close the serial monitor for 30 s, reopen: no restart, `usb_drop`
   has risen, and the hub kept getting the node's frames meanwhile (its frame count kept rising).
7. Finish by flashing the normal build (no `floodtest`) and checking `"bench":[]`.

## Alarm integrity on the nodes (WP-11a, 2026-11-18, compile-checked only)

Branch `sec/p2-a`. Host-tested (`cargo test -p wifiwatch -p mavwatch -p ctrlwatch -p wifidrone
-p tracker`); the firmware is compile-checked only (C5 `fw` and `fw24-now`, node-lr `fw`, S3
`cargo check`, C5 `floodtest` `cargo check`). What changed is in docs/DRONE-ATTACKS.md and
docs/WIFI-ATTACKS.md (marked WP-11a): Retry-bit deauths with new sequence numbers count; a drone
restart is believed only from several beacons on the new clock, and two clocks on one address are
a `DroneNetClone` (detail bits 3 and 5); 4 drone networks with displacement rules; MAVLink frames
of any length; one MAVLink frame is a suspicion (`detail` bit 24); contested links need 5 s of
history; small tables keep entries holding evidence.

New stats: node-wifi `stats` gains `"mavp":[held back, evidence given up, turned away, streams
turned away]` and `"dnet":[learned, displaced, made to wait]`; node-lr prints
`{"t":"ctrl_pressure",...}` when its counters change (at most every 5 s), and its
`mavlink_attack` line has `"suspect"`.

RAM, static `.data` + `.bss` from the ELF: node-wifi C5 ESP-NOW 88 676 -> 89 844 bytes (+1 168),
C5 wired 5 GHz 87 852 -> 89 020 (+1 168); node-lr (C3) 127 168 -> 127 848 (+680). Detector state:
`wifiwatch::Watch` 6 104 -> 6 616 bytes (`DroneLinks` 440 -> 816, `Floods` 1 176 -> 1 312),
`mavwatch::Watch` 8 216 -> 8 784, `ctrlwatch::contested::Watch<8>` 5 456 -> 5 616.

### To check on hardware with `floodtest` (not yet run)

A `floodtest` node-wifi (build as in "The `floodtest` build"; its first `flood` line must also say
`"hijack_ok":true`), wired to the P4. Nothing is transmitted. Record the `stats`, `flood`, `threat`
lines and the hub's threats.

1. **Retry-bit flood.** `flood retry 60` for 10 s: `Deauth` threats at `02:52:45:54:52:59`
   (target `02:50:48:4f:4e:45`) about once a second, `kill` rising by about 60 a second. Before
   WP-11a: no threat. `flood off`.
2. **Genuine retransmissions.** `flood dup 64` for 10 s (8 fresh frames a second, each retried
   seven times): no `Deauth` threat while `kill` rises by about 64 a second (counting every frame
   would be 64 a second, over the 30-a-second bar). `flood dup 400` (50 fresh a second): a
   `Deauth` about once a second. `flood off`.
3. **Forged restarts between deauths.** `flood hijack 1` for 15 s: the test drone `TELLO-7E5701`
   (`60:60:1f:7e:57:01`) is learned (`dnet` first number rises), then a `drone_deauth` threat for
   it within ~3 s and about once a second after, and `drone_net_clone` with `source` = `target` =
   `60:60:1f:7e:57:01` and detail bits 3 and 5 (`detail & 0x3F` = 40). Before WP-11a: no
   `drone_deauth` (each forged beacon emptied the count). `flood off`; both stop within ~10 s.
4. **A real drone, if one is available (your own Tello, in a shielded box):** power-cycle it while
   its phone app runs: no drone alarm (the restart is believed after about a second of beacons).
5. Finish with the normal build and `"bench":[]`.

The node-lr changes (contested-link history, the MAVLink suspicion and budgets, the tables) have no
bench injector; they need a real SiK pair or ExpressLRS link and are host-tested only.

## node-sdr under hostile signals (2026-11-18, compile-checked only)

Anyone in radio range can make the DroneID node work for them. Two ways were found and closed in
software; neither has run on a chip yet.

* **Blind while decoding.** Any 450-800 µs energy run on the dwell channel passes the trigger's
  length gate, the ring stops, and a DroneID-shaped burst takes ~150 ms to decode. One pulse every
  ~150 ms kept the node blind almost all the time. Now (`crates/rawcap/src/budget.rs`): at most 3
  full decodes per dwell and 4 a second, and a credit that earns 40 % of wall time (at most 450 ms)
  pays for every decode, so the decoder holds the loop at most ~40 % of the time plus 0.75 s. A
  window the budget refuses is dropped **before the ring is stopped**. Cheap gates come first
  (`droneid::rx::Config::cfo_window` and `turbo_gate`, set for the tuned channel): a span whose
  coarse frequency is more than 1 MHz from +500 kHz is refused before resampling, and the turbo
  decoder runs only at reference SNR ≥ −5 dB within 300 kHz. The `stats` line has
  `"dec":[decodes, full, skipped, busy_ms]`; a dwell where the budget refused windows and most full
  decodes were not valid bursts counts towards `jam_pulsed` (`detail` = the decoder's busy
  percent), which is how the hub hears of it (no new link field). A burst's `path` reads `gated`
  when the turbo gate skipped it.
* **The O4 follower could be steered.** One forged packet made a session that the node visited
  from the next publication on; four forged key IDs kept the node on attacker-chosen channels, a
  fifth evicted the real drone's session, and one packet with the real (cleartext) key ID wiped its
  hypotheses. Now (`crates/droneid/src/follow.rs`): a session is only visited after 3 packets that
  agree in channel and time; key IDs not yet followed are candidates in a table of 128 that a flood
  cannot cycle within their 30 s lifetime (the decode budget passes at most 87 new IDs in that
  time); a session 2 s without a packet goes before the others; visits arrive 50 ms early, and
  where hypotheses disagree an inconclusive visit sends the next one to the second-best channel; a
  packet that contradicts a followed session is ignored and counted; visits are paid from a credit
  that earns half of the plan's time (at most 1 s). `stats` `"follow"` is now `[followed, visits,
  hits, misses, tentative, contradictions, evicted, declined]`; `follow` lines say `confirmed`,
  `restart` and (at most once a second) `contradicted`.

**What an attacker can still do**: flood DroneID-shaped bursts so the budget is spent on them
first-come (real bursts are then decoded only in proportion; the node keeps capturing and probing
~60 % of the time and reports `jam_pulsed` when the flood is not valid bursts); replay real,
CRC-valid bursts (decoded and reported as sightings, never flagged as pressure); create followed
sessions by following the published schedule and spend the follower's half on them; keep a real
drone's session unconfirmed by restarting it with forged packets carrying its key ID while it is
still tentative. With all four followed slots taken by forged sessions before the drone is
confirmed, the drone is not followed (still heard by the plan). Followed sessions cost the plan
hearing time in proportion to their share, and a drone in step with a forged session is heard by
the plan under a third as often. On the air each forged packet also costs a ~165 ms decode, which
slows confirmation (host model: 78 % within 60 s at the budget's maximum). A DroneID flood is at
most ~2.8 new IDs a second per node, far below the hub's fast ID-flood line (20 a second).

### The scenarios (bench `bringup` build, nothing transmits)

`radio/hilsim.rs`, driven by `tools/droneid-hil`. Build and flash a bench image:

```sh
export ROTORSCOPE_BENCH=1
(cd firmware/node-sdr/c5 && cargo fw --features bringup)
```

1. **No regression on the recordings** (needs `tools/fetch-droneid-samples.sh` run once). The
   replay is outside the budget but goes through the new gates:
   ```sh
   tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT" --rms 200,150' 900 c5b
   tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT" --pre "trig 3" --rms 100,80' 900 c5b
   ```
   Expect what BRINGUP-RESULTS.md "HIL" measured: rms 200 and 150, 13/13 detected and CRC-valid
   per pass; with `trig 3`, rms 100 13/13 and rms 80 about 9/13; decode times 144-158 ms. No burst
   may report `"path":"gated"` or `{"err":"OffFrequency"}` when it decoded before. Every `hil`
   line: `"intact":true`; record `stack_free` from the `link` line afterwards (2026-11-18, c5b:
   5148 B after the recordings; 4.7 KiB after `hil train`; ~5.5 KiB on the live path). Expect at
   least 4.5 KiB (node-sdr README "Stack").
2. **Pulse train of white noise**, a 600 µs pulse covering the whole capture (no recording needed):
   ```sh
   tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT" --train 200,150 --pulse noise --rms 200' 300 c5b
   tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT" --train 400,20 --pulse noise --rms 200' 300 c5b
   ```
   Each prints two `hil_train` lines, `"budget":true` and `false`. Record `decode_ms` [min, max]:
   this is what one noise pulse costs the decoder now (measured 2026-11-18: 100-101 ms per noise
   pulse; the coarse-frequency gate does not make noise cheap). Expect `full` 0 and `valid` 0. If `busy_pct`
   without the budget is under ~40 %, the gates alone keep the node free and the budgeted line
   matches it (`skipped` 0, `flagged` 0). Where the unbudgeted line is above 40 % (at 20 ms gaps it
   should be), the budgeted one must stay at or below ~42 % with `skipped` > 0 and `flagged` close
   to `dwells`.
3. **Pulse train of a real burst** (the worst case: DroneID-shaped and expensive; a replayed
   recording, so its CRC is valid):
   ```sh
   tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT" --train 200,150 --pulse burst --rms 200' 300 c5b
   ```
   Without the budget: `busy_pct` about 57 (a window costs ~173 ms with decimation and DC removal,
   longer than the 150 ms gap, so every other pulse arrives during a decode: `missed` ~100,
   `decoded` ~100), `skipped` 0. With it: `busy_pct` at most ~42 (measured 41-45), `skipped` > 0,
   `decoded` about 72-80 of 200, `decode_ms` ~173 (it includes decimation and DC removal, ~20 ms
   more than step 1's decode-only time). `flagged` 0 both ways: valid bursts are not
   jamming (the hub would see them as sightings; HIL sightings are never sent).
4. **Forged sessions on the live follower**, three cases, 3 minutes each, each at least twice:
   ```sh
   for c in start flood late; do
     tools/dev-board.sh --run "cargo run --release -p droneid-hil -- --port \"\$BOARD_PORT\" --forge 180,$c" 400 c5b
   done
   ```
   A virtual drone (key ID `5ea10001`) and, depending on the case, four forged schedule-perfect
   sessions, a new forged key ID every 0.7 s, and (once the drone is followed) a packet with its
   key ID every 1.3 s at a wrong time. Each is "heard" whenever the node is tuned to its channel;
   nothing is decoded and nothing is transmitted. The follower is cleared at the start. The
   `forge` line fields:
   - `real` = [followed, hits, contradictions]
   - `followed_s`: when the drone was first followed (-1 = never)
   - `blocked`: three consistent packets but no free slot
   - `heard`: per stream
   - `unsettled` / `interrupted`: silent visits that could not count as misses
   - `share_permille`: time the follower held the receiver
   - `contra` = [wrong-time packets sent, contradictions counted]
   - `evicted`: candidates pushed out of the table before they expired

   The tool prints an `expect` line per run; the odds come from
   `crates/rawcap/tests/follow_forge.rs` (docs/DRONEID-O4.md).
   - `flood` (no forged sessions): `real[0]` true at the end, `followed_s` at most ~60 (model:
     95 % within 60 s), `evicted` 0, `real[2]` = `contra[1]` > 0. The other wrong-time packets
     fell within half a period of the drone's last packet or inside its window and were taken as
     duplicates or as its own; none restarts it.
   - `late` (all forgers once the drone is followed): `real[0]` true at the end (model 99.5 %),
     `active` at most 4, `share_permille` at most ~550, `declined` > 0, `evicted` 0.
   - `start` (everything from the start, the 2026-11-18 scenario): the drone is followed only if
     the plan confirms it before the fourth forged session. With these fixed phases that happens
     in about half the runs (model 45 %); otherwise `blocked` true and `forged_followed` 4. A run
     where the drone is not followed is the documented limit, not a failure. Always `evicted` 0,
     `active` at most 4, `share_permille` at most ~550.

   In the `stats` lines, `follow` is [followed, visits, hits, misses, tentative (candidates, up
   to 128), contradictions, evicted, declined], and `dec`'s `skipped` should stay 0.

   Measured 2026-11-18 on c5b with the fixed follower: `flood` followed at 19 s and 43 s and kept
   to the end (two runs, `evicted` 0); `late` kept to the end with three forged sessions followed
   (`share_permille` 380); `start` blocked with four forged sessions followed (the limit above).
   Recordings regression afterwards 13/13 at rms 200 and 150.
5. Flash a normal build afterwards and check `"bench":[]`.

### Restarting the ring before the decode: deferred, to try on the bench

Not done. By reading alone it looks safe in memory terms but would buy nothing on its own, so it
was left out rather than guessed:

* **Memory.** The decoder reads the 16,384 decimated samples from the capture bank
  (0x40830000), which the ring's DMA never writes (ring and descriptors end below 0x4082e0c0), so
  no copy to PSRAM is needed. But `Radio::restart` also re-arms the dump engine
  (`engine::free_run`), which hands SRAM block 1 to the engine for a few register writes and could
  drop or land writes in the bank while it owns it: a ring-only restart (`ring::start` without
  `engine::free_run`, which keeps running across `ring::stop`) is required.
* **Interrupt timing.** With the ring running, the DMA interrupt fires every 3584 bytes (~90 µs)
  for the whole ~150 ms decode and runs on the main stack on top of the decoder's deepest frame
  (~6.9 KiB left). The `burst` line (up to 2 KiB, ~6 ms over USB inside esp-println's critical
  section) would hold the interrupt off for several laps, the 2026-11-12 failure (a late interrupt
  clobbers a length write-back): it would have to go out with `out::paced`.
* **No gain without probing during the decode.** The ring holds 1.43 ms. After a 150 ms decode
  `probe` finds itself ~100 laps behind and skips to the newest data, exactly as when the ring is
  restarted afterwards: the node would still see nothing that arrived during the decode. The gain
  needs the decoder to call back into `Radio::probe` at least every ~1 ms (inside the resampler,
  the CP search and each turbo half-iteration), and a second capture buffer for a burst that rises
  meanwhile.

To try, in this order, each with `wb_lost`, `lapped`, `skipped`, `dma_err` and `stack_free` read
before and after: (1) ring-only restart after `remove_dc`, `burst` line paced; HIL step 1 must
still give `"intact":true` (compare the capture CRC) and identical decodes, and `wb_lost` must stay
0 over a ten-minute soak with a 10 Hz beacon; (2) a `poll` hook in `decode_span` stages; measure
the longest gap between hook calls (it must stay under 1 ms) and the decode-time cost.

## GPS time and trust scenarios (WP-12, 2026-11-18, compile-checked only)

No real GPS receiver has been on the P4 hub yet; its GPS path has only run
against the on-chip emulation (`firmware/hub-p4/src/gpsloop.rs`). These
scripted scenarios make the emulated receiver misbehave so the hub's reaction
(docs/GNSS-INTEGRITY.md "Trust and holding", "Time", "Home") can be watched.
**Not yet run.** Nothing transmits; leave the UART3-IN header, GPIO29 and
GPIO30 unconnected, as for any `gpsloop` run. The scenario is chosen at
build time:

```sh
cd firmware/hub-p4
GPSLOOP_SCENARIO=step1s ROTORSCOPE_BENCH=1 cargo build --release --features gpsloop
#   scenarios: honest (default), step1s, step500, drift, spoofboot, early
#   gpsloop-m10 for the u-blox emulation (no satellite table: spoofboot does nothing there)
```

The emulated UTC starts one day after the firmware's build date. Loop builds
keep their GPS memory in `ROTORSCP/GPSLOOP.TXT`, never `GPSMEM.TXT`, and do
not remember the time (the emulation restarts its clock at every boot). The
file is first written when home is learned (15 minutes of steady fixes with
integrity `ok`), so every boot before that says `GPS memory: none on the card
(no ROTORSCP/GPSLOOP.TXT yet: written once home is learned)`. That is
expected. (The first bench pass never saw the file because the rig's late PPS
edge, fixed since, kept integrity at `suspect_spoofing`, and home is only
learned while integrity is `ok`.)

**No GPS attached** (a normal build with nothing on the UART3-IN header):
`gps.state` is `absent`, and `trusted`, `untrusted_pos` and `untrusted_time`
all read `false`. "Untrusted" means a receiver gave a position or time that
the hub refused. With no receiver there is nothing to refuse, so there is no
position, `utc_ms:null`, and evidence records carry time source `none`, not
`untrusted`.
Every run starts with `hub-p4: gpsloop scenario <name> (GPSLOOP_SCENARIO;
emulated UTC starts <s> s)`. Watch the console and the `{"t":"hub"}` line's
`gps` object: `trusted`, `untrusted_pos`, `untrusted_time`, `time_held`,
`time_step_ms`, `time_floor`, `home`, `home_pending`.

| Scenario | What the emulated receiver does | Expected |
|---|---|---|
| `honest` | as before | `trusted:true` from the first fix on; no `gps integrity` line, no `gnss_*` evidence event for an hour; `time_held:false` throughout; after about 15 min `GPS home learned`. `time_step_ms` is the larger of the PPS and message channels, so it follows the message arrival: measured -134 to +17 ms over 18 min (tolerance 150 ms), mostly within +-15 |
| `step1s` | from second 120 every epoch's UTC is 1 s early | `gpsloop scenario step1s begins now`, then within 2 s `gps integrity ok -> suspect_spoofing: possible GPS spoofing: GPS time jumped -1000 ms against the hub's clock; holding last trusted position and time`. `trusted:false`, `time_held:true` for as long as it runs (it never ends); evidence context goes `free_running` after 2.5 s; the aero `TimeSync` keeps the hub's held clock, not the stepped one |
| `step500` | at second 120 the epochs and the PPS edge move 500 ms later | as `step1s` with about `-500 ms` |
| `drift` | from second 120 each epoch 2 ms later than the last (2000 ppm) | with PPS (both emulations drive it) the alarm comes about 3-5 s after `begins now`, `time_step_ms` around -5 to -10; then held as above |
| `spoofboot` | until second 600 every satellite at 46 dB-Hz | within 5 s of the first fix `gps integrity ok -> spoofing: GPS spoofing: ... satellites all near 46 dB-Hz ...`; from the start `untrusted_pos:true`, `untrusted_time:true`, `utc_ms:null`, `own:null`; health page `GPS no trusted position/time`; evidence records time source `untrusted`; no `TimeSync` sent (aero `syncs` does not grow); the SD log opens after 90 s numbered `LOGnnnnn.CSV`. After `spoofer off now` (second 600): back to `ok` 30 s later, `trusted:true` 10 s after that, then an evidence `UTC set from GPS` clock event |
| `early` | honest; the hub is told at boot it last trusted a time one day later | `gpsloop early: the hub now remembers trusting UTC ...`; at the first fix `gps integrity ok -> suspect_spoofing: possible GPS spoofing: GPS time 86397 s before the build date or the last time the hub trusted` (about a day); `untrusted_time:true`, `time_floor:true`. Console `T`: `GPS time accepted by the operator (console)`, then `ok` after 30 s and `trusted:true` 10 s later |

Home (any loop build, about 16 minutes): `GPS home learned: 39.9526000
-75.1652000 (15 min of steady fixes); remembered on the card` and a
`GPSLOOP.TXT` with a `home` line. Reboot: `GPS home loaded from the card`.
A placed home far away (console `L`, `home 39.9 -75.2`) makes the next fixes
alarm `off_home`; after 15 minutes `GPS: the hub seems to stand at ... NOT
adopted`, the Settings row reads `New home / Accept here`; SEL (or console
`H`) logs `GPS home confirmed` only if home is not placed (a placed home
answers that the site file wins: `home clear` first).

Link and node side (host-tested only): with a keyed kit an honest run keeps
`"late":0` on every node's `link_auth` line and `"sync_refused":0` on an aero
node's `stats` line.

## Bench results, 2026-11-18

The first hardware pass of the crash-recovery, bench-fence and origin-check work.

**Boards.** ESP32-P4 hub (chip rev v1.3), gateway on an ESP32-C5, two multi-image ESP32-C5 nodes.
Firmware built from branch `sec/wp07-audio` at c11d91a (which contains `sec/phase1`). The S3 web
head was not attached and its firmware does not link (static RAM over by about 18 KB), so nothing in
`firmware/hub` has run on hardware.

**P4 crash recovery.** Watchdog armed on every boot, timeout read back as 30.0 s, never left off.
Normal running for 320 s with SD card, airspace pack, gateway and two nodes: no restart; worst SD
step 381 ms.
* `fault panic`: backtrace, reset 0.5 s later, next hello 1.9 s after the panic, reason `software`
  (ROM SW_SYS_RESET).
* `fault hang`: reset 30.0 s later, reason `watchdog` (ROM CHIP_LP_WDT_RESET, cause 0x10).
* The evidence boot events carried the reset reason, the bench list and `audio clips off at boot`.

**Gateway.** 330 s of relaying without a restart. Panic: reset 0.5 s, hello 1.3 s, `software`.
Hang: reset about 9.5 s, `watchdog`.

**node-wifi (C5).** 400 s hopping with no restart and no unexpected watchdog reset. Panic: reset
0.5 s, hello 0.7 s, `software`. Hang: reset at 30.2 s and 30.1 s.
**node-sdr (C5).** 480 s without a restart. Panic: reset 0.4 s, hello 0.7 s. Hang: reset at 10.0 s
and 10.2 s. The reset reason after a watchdog reset could not be read on the nodes (see the known
limit in "Crash recovery", checklist step 4).

**Origin filtering** (keyed kit, node-wifi `forgetest`). Each of the seven forbidden frame kinds was
refused at the gateway within about 1 s (`refused` 1 to 7, `"why":"origin"`). One forged-link threat
with detail 5 reached the P4 and its count rose to 7; the P4's own `refused` stayed 0. The node's
normal packets kept flowing with none lost.

**Fail-closed tools** (the key checks named here were replaced at the cut-over by the boot-line
check of "Link keys: bench checklist"). `flash-node.sh` refused a keyless build, refused
`ROTORSCOPE_LINK_KEY_FILE=none` without `--open`, and refused a bench image without
`ROTORSCOPE_BENCH=1`. `dev-board.sh` refused a bench gateway image. The cargo build refuses bench
features and bench switches by itself.

**Normal builds restored** on all four boards. Runs of 345 s and a further 95 s: no restarts; hub
and gateway `refused`, `corrupt` and `bad_tag` all 0; both nodes assigned roles. The hub's evidence
file (10 records, 5 checkpoints) was replayed over the console. `evidence-verify` refused it by
default because it is signed with the published test key; with `--allow-test-key` and the test key
supplied it reported 10 records, 0 failures, 0 gaps, VERIFIED, which shows low-S signing works on
the chip.

**Not checked.** Anything on the P4's screen (nobody could see it); the S3 web head; audio clips
switched on; a real sound through the microphone; the wired bus; an old gateway against the new hub;
node-table eviction; node-ble and node-lr crash recovery.

## Bench results, 2026-11-18 (Phase 2 fixes)

Two defects from the Phase 2 bench pass, fixed on branch `sec/wp07-audio` and run again on the
same boards: P4 hub, gateway `c5c`, multi-image node `c5a` (`c5b` was in use for other work and
was not touched). Logs: `a-*.log` in the session scratchpad (`p2fix/`).

### "Node blinded" after a flood (hub deaf-node check)

**What failed.** About 10 s after a 1 000 frames/s injected flood on `c5a` stopped, the P4 raised
`THREAT Node blinded (node 2, +0 dB, detail 3)` and held it for the remaining 170 s while the
node heard its usual ~80 frames/s. The check (`tracker::threats::DeafWatch`) learned a node's
normal rate with a ~30 s memory per Stats report, so the flood became "normal"; the quiet after it
read as under a tenth of that; and learning stopped while a node read deaf, so it never recovered.
This check ran only on the S3 web head before the P4 integration, so it had not met a flood.

**What it does now.** Learning runs on node uptime: a plain average for the first minute, then up
with a 10 min time constant (one report counts as at most twice the normal) and down with a 3 min
one. It learns from frames kept (seen minus the Stats `dropped` total, so a flood the node sheds
is not learned) and judges deafness on frames seen. While a node reads deaf the normal keeps sinking
(floored at 20 frames/s), so a lasting lower level becomes the new normal: the alarm ends and the
hub says `node N now hears X/s, was Y/s; accepted as its new normal (node blinded alarm ends)`
(console and a `Setting` evidence event). A node that hears under 2 frames/s stays raised. Once a
minute the P4 prints `deaf-check node N: X/s now, normal Y/s`. Host tests: `tests_deaf.rs`.

**Bench** (`a-p4-flood.log`, `a-c5a-flood.log`, `a-c5c-flood.log`; P4 normal build with the fix,
`c5a` node-wifi `floodtest` behind the keyed gateway). `flood real on` at 45 s; at 180 s `rid`,
`tool`, `drone`, `dji`, `wfb` at 300 a second each; at 300 s all at 1 000; at 400 s all off;
watched to 840 s.

| Hub time | Node 2 now | Normal (hub) |
|---|---|---|
| 120 s (settled) | 83/s | 103/s |
| 242 s (300 a second each) | 1 265/s | 104/s |
| 361 s (1 000 a second each) | 4 042/s | 105/s |
| 421 s (20 s after) | 85/s | 100/s |
| 600 s | 75/s | 84/s |
| 780 s | 105/s | 84/s |

No `Node blinded` at any time (no `blinded` threat line at all). The node-shed alarm raised at
185.0 s (5 s after the flood began: 1 072 frames a second discarded) and its last report was at
404.5 s, so it ended about 15 s after the flood stopped, as before. `FLOODREAL0000000001` stayed
active and confirmed all run. No restart on any board. Node 1 (`c5b`, ~15/s) is under the 20/s
floor and is not judged.

### The GPS loop rig's late PPS edge

**What failed.** In the bench-only `gpsloop` builds the emulated PPS pad is raised from the hub's
main loop, which pauses for SD writes (18-35 ms flushes, ~130 ms airspace pack reads). The edge
interrupt timestamped the physically late edge, more than the 5 ms PPS tolerance, so every scenario
reported `GPS time jumped -33 ms` about once a minute: `honest` could not pass, the step and drift
alarms could not be timed, home was never learned and so no `GPSLOOP.TXT` was ever written. The
real path (a receiver's on-time edge, timestamped in the interrupt) was not affected.

**Fix.** The emulator hands the interrupt the instant the edge was scheduled for
(`gps::pps_drive_at`, `gpsloop` feature only). Tolerances unchanged; the normal build's interrupt
is unchanged (its image has no trace of the hand-over).

**Bench** (`a-p4-gps-<scenario>.log`; the gateway and `c5a` stayed attached, `c5a` still on its `floodtest` image injecting only the synthetic real drone). In every
run the published UTC (`utc_ms - up_ms`) never moved after the first trusted epoch, except as noted
for `drift`; the aero `TimeSync` is sent from that same clock.

| Scenario | Result |
|---|---|
| `honest`, 18 min | No integrity finding, `trusted:true` and `time_held:false` from the first fix (12.7 s) to the end; `UTC set from GPS (PPS-locked)` at 12.5 s; `GPS home learned` at 905 s. A reboot of the same image: `GPS home loaded from the card` (so `GPSLOOP.TXT` was written). PASS |
| `step1s` | `begins now` 121.98 s; held (`trusted:false`, `time_held:true`) at 122.28 s, the console line `GPS time jumped -1000 ms` at 122.98 s (first stepped epoch). Held to the end; published UTC unchanged |
| `step500` | `begins now` 121.88 s; the first late epoch is about 1.5 s later; held at 124.48 s, `GPS time jumped -500 ms` at 124.88 s: within two epochs. Published UTC unchanged |
| `drift` | `begins now` 121.88 s; held at 125.48 s, `GPS time jumped -7 ms` at 125.88 s (4.0 s after). Before the alarm the published UTC followed the walk by 4 ms (inside the 5 ms PPS tolerance, by design), never after |
| `early` | At the first fix (5.7 s) `GPS time 86394 s before the build date or the last time the hub trusted`; `untrusted_time:true`, `time_floor:true`, no UTC. Console `T` at 90 s: accepted; `ok` at 122.6 s, `trusted:true` at 132.1 s |
| `spoofboot` | `ok -> spoofing ... 14 satellites all near 46 dB-Hz` at 6.7 s; `untrusted_pos`/`untrusted_time` true, no UTC, no `TimeSync` (`syncs` 0) until trusted; SD log `LOG00119.CSV` opened at 91 s; evidence records marked `untrusted`. `spoofer off now` 601.8 s: `ok` 29.3 s later, `trusted:true` at 640.8 s and the evidence event `UTC set from GPS (PPS-locked)` at 640.6 s; `syncs` grows from then on. Because `honest` had learned a home, the remembered home was held as the own position from the card mount (91 s) while spoofed |

**`GPS memory: none on the card`** on every loop boot was expected before a home was learned (see
"GPS time and trust scenarios"); the line now names the file it looked for.

**Seen, not fixed.**
* The message channel has little margin at boot: while the airspace pack loads (main-loop pauses
  of 130-376 ms, `max_tick ... (pack_read)`), a GPS message read late by more than 150 ms makes that
  epoch untrusted. In `step500` (30.8 s) and `spoofboot` (91.5 s) the hub held its clock for about
  10 s with no console or evidence finding, then trusted again. This is the real receiver path too
  (messages are read in the main loop), not the rig.
* Between integrity going back to `ok` and `trusted:true` (about 10 s, `early` and `spoofboot`)
  `own` is `null` even when a remembered home exists, although the home was held as `own` while
  spoofing was suspected just before.

**Boards left on normal keyed builds.** P4: normal build with both fixes (`bench: []`). `c5a`:
node-wifi, node-ble and node-sdr normal images, link key 42eeb2c6, `"bench":[]`,
`"faulttest":false`. `c5c`: gateway as before, link key 42eeb2c6, `"bench":[]`. A 200 s run
together (`a-p4-final.log`): no restart, `refused` and `corrupt` 0, the deaf check holding node 1
(`c5a`) at about 82/s now and 91/s normal.

### Slow fake-drone floods (hub-wide drone alarm, slow rules)

Branch `sec/wp07-audio`, P4 at `7b4d279` (normal build, `bench: []`), `c5a` a `floodtest` node-wifi
built with `ROTORSCOPE_BENCH_CAN=wifi24` (so the hub gives it Wi-Fi 2.4; without it the hub gave
`c5a` BLE and the injector never ran), `c5c` gateway. Logs `c-*.log` in the session scratchpad
(`p2fix/`). Times are from the command.

* Quiet 3 minutes, `flood real on`: no threat at all.
* `flood rid 3` for 60 s: `id_flood` (`drones`) at 12 s, first said as "32 drone IDs active at once
  (limit 32)" (30 one-frame fakes in 10 s plus the test drone's Remote ID and DroneID tracks), then
  "124 new drone IDs in the last minute"; ended 50 s after the flood stopped. The test drone's two
  tracks stayed confirmed under their own ids for the whole run.
* `flood crowd 40` for 120 s: no drone flood alarm. The node let about 28 of the 40 through (21
  known sources in its 32-slot table, the rest over the 8 a second new-source budget), so the hub
  held 30 tracks, under 32. A `Fake network flood` (node 1, the `FLOOD` beacons) was raised.
* `flood rid 200` for 60 s: the drone alarm 2 s after the start and the node-shed alarm 5 s after
  ("166 frames a second discarded, 4 new drone IDs a second never confirmed"); the node-shed alarm
  ended 15 s after the flood stopped, the drone alarm (minute rule, 419 then falling) 61 s after.
* The console printed one `THREAT Flood of new drone IDs` line for the whole run: an entry that
  ends and is raised again before it is removed (10 min) is not announced again. Not changed here.
* The GPS hold note could not be seen: no GPS module was attached.

