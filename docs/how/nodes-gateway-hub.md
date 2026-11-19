# Nodes, gateway and hub

**Level: 🟡 Intermediate** · Every kind of board in a Rotorscope kit: what it listens to, which
chip it runs on, where its firmware lives, and how far each has been proven.

## In plain words

A Rotorscope kit has three kinds of board:

- **Listener nodes** hear drones. Each one listens to one kind of signal at a time and reports
  what it finds.
- **The gateway** is a messenger. It collects reports sent by radio from the nodes and passes
  them down a wire to the hub. It hears no drones itself.
- **The hub** is the brain: it merges, checks, shows and records everything, and tells each node
  what to listen to.

One more board, `rid-beacon`, is not part of a kit at all: it is a fake test drone for the
bench. The proof words below (**proven on the air**, **simulated only** and so on) are explained
in [Writing these docs](../dev/docs-style.md#say-what-is-proven-every-time); other terms are in
the [glossary](../project/glossary.md).

## The listener nodes

On the bench every node is a Seeed **XIAO ESP32-C5**. The C5 has 2.4 and 5 GHz Wi-Fi, Bluetooth
5 and, as the project found, a way to read its radio's raw samples, so one board type can do
every listening job (PLAN.md, "Node chip"). The Wi-Fi and Bluetooth firmware also still build
for the older ESP32-S3, which has no 5 GHz and no raw capture.

### node-wifi: Wi-Fi

`firmware/node-wifi`. Listens in promiscuous mode (it reads every frame on the channel) and
hops channels to a plan the hub sends.

| What it looks for | Proof |
|---|---|
| Remote ID in Wi-Fi beacons and NAN frames, 2.4 GHz | **Proven on the air** (S3 and C5, from `rid-beacon`) |
| Remote ID in Wi-Fi beacons and NAN frames, 5 GHz | **Proven on the air** (C5, channel 149, from a C5 `rid-beacon`) |
| Open digital FPV links (wfb-ng/OpenIPC, OpenHD, RubyFPV, DroneBridge) | **Proven on the air** from the test beacon's fake frames on channels 6, 149, 161 and 165; not yet heard from real FPV hardware |
| DJI's older Wi-Fi DroneID element, drone-maker Wi-Fi networks | **Simulated only**; not yet seen from a real drone |
| Wi-Fi attacks: deauthentication and beacon floods, evil twins and others ([WIFI-ATTACKS.md](../WIFI-ATTACKS.md)) | **Simulated only** |
| MAVLink drone telemetry sent over open Wi-Fi, only when the owner turns the control-link opt-in on ([DRONE-ATTACKS.md](../DRONE-ATTACKS.md)) | **Simulated only** |

Whatever its plan, a Wi-Fi node leaves it for 105 ms every 3 s to "scout" a channel its plan
skips, so nothing can hide on an unwatched channel (`crates/coverage/src/plans.rs`).

### node-ble: Bluetooth

`firmware/node-ble`. Scans for Bluetooth Remote ID on both Bluetooth radio modes at once:
Bluetooth 4 "legacy" adverts, Bluetooth 5 "extended" adverts and Bluetooth 5 **Long Range**
(the coded mode that reaches further). It never transmits on its own; over ESP-NOW it pauses the
scan 200 ms each second to send.

**Proven on the air**: all three kinds decoded on a C5 from a C5 `rid-beacon` (2026-11-12),
and Bluetooth 4 from a laptop test advert on an S3. It also watches for Bluetooth pairing
pop-up spam, an attack on nearby phones (**simulated only**).

### node-sdr: raw capture

`firmware/node-sdr`. A C5 that reads its own radio's raw samples ("I/Q", the radio wave as
numbers) instead of Wi-Fi frames. It needs a C5 with PSRAM (extra memory) for the DroneID
decoder; the XIAO ESP32-C5 has it. The hub gives it one of two jobs:

| Job | What it does | Proof |
|---|---|---|
| DJI DroneID | Walks the DroneID frequencies at 2.4 and 5.8 GHz and decodes each DroneID burst (serial number, drone and pilot position) | Decoder **proven on recordings**, including public recordings played through the C5's own capture path; the job runs on air on hub assignment, but **no real DJI drone has been heard** |
| FPV and RF scan | Sweeps the analog FPV video channels at 2.4 and 5.8 GHz, recognises analog video by its line rate, fingerprints digital video links and watches for jamming | **Simulated only** on synthetic signals; the sweep runs on air, but no real video transmitter has been heard |

While it captures, the radio's transmit queues are stopped and checked; it only transmits in
short windows between captures when built for ESP-NOW.

### node-lr: the LR2021 aero and control-link node

`firmware/node-lr`. A Semtech **LR2021** radio on an ESP32-C3, for signals no ESP32 radio can
reach: aircraft on 1090 MHz and 978 MHz, FLARM and OGN glider trackers on 902-928 MHz, and drone
control links such as ExpressLRS on 900 MHz and 2.4 GHz ([AERO-NODE.md](../AERO-NODE.md),
[CONTROL-LINKS.md](../CONTROL-LINKS.md)). It has no code that can make the LR2021 transmit.

**Built, not yet tried**: no LR2021 board was in hand as of 2026-11-18; the logic is tested on
a computer only.

## Several firmware images on one node

A C5 node can hold three firmware images side by side and become a Wi-Fi, Bluetooth or
raw-capture node on the hub's command. Its 8 MB of flash is laid out in
`tools/partitions-node.csv`:

| Slot | Usually holds |
|---|---|
| `ota_0` | node-wifi |
| `ota_1` | node-ble |
| `ota_2` | node-sdr, if it was built (else left empty) |
| `rsjob` | one 4 KB sector: the job the node switched images for |

How a switch works (`crates/nodeimage`, `firmware/node-common`):

1. At start-up the running image reads the start of every slot and recognises each image by the
   project name inside it, not by which slot it is in.
2. It tells the hub every role those images can do (the `can` list in its Status report).
3. When the hub assigns a role the running image cannot do, the image writes the job to
   `rsjob`, marks the other slot to boot, and restarts.
4. The new image reads the job and starts with it, reporting the plan number the hub sent.

A switch takes a reboot of a few seconds, which is why the hub changes a node's role at most
once a minute. The node's address is the same in every image, so the hub keeps recognising it.
A board flashed with a single image still works; it only offers its own roles and refuses to
switch.

`tools/flash-node.sh <board>` builds node-wifi and node-ble and flashes every slot. The images
carry no key: an ESP-NOW node keeps its own key record in a separate part of its flash, which
reflashing leaves alone and `link-key` writes ([The link between boards](link.md)). After an
ESP-NOW flash the script calls the flash good only if the node reports a key (or an open record,
with `--open`), and it refuses bench-only images unless `ROTORSCOPE_BENCH=1` is set.

Image switching is **proven on the air**: on 2026-11-15 the hub moved a node between Bluetooth
and Wi-Fi 5 seven times in five and a half minutes, each switch on a hub command
([ESPNOW.md](../ESPNOW.md), "Node roles and image switching").

## The gateway

`firmware/gateway`, one XIAO ESP32-C5 wired to the hub. It sits on 2.4 GHz channel 6, puts the
hub's beacon on the air twice a second so nodes can find it, receives the nodes' ESP-NOW packets
and passes each one to the hub unchanged. It puts the hub's commands on the air for the nodes. It
listens to no drones itself. Since 2026-11-18 it **holds no key**: it cannot check a packet's
signature, or make one, so a stolen gateway gives nothing away. The hub checks every packet.

What it **refuses** to pass on, with checks that need no key: packets from addresses that are not
in the kit's roster (the hub sends it the list), more than its share of packets from one address
(a flood), packets from boards still on the old firmware, and packets of the wrong shape. It
reports each to the hub, which raises a "forged node traffic" alarm. Everything else is the hub's
job: a bad signature, an old or replayed packet, and any frame a node has no business sending,
such as the kit's own GPS position, the hub's aircraft lines, FAA flags, or a marker naming
another node.

Relaying was **proven on the air** before the 2026-11-18 cut-over, when the gateway still held
the kit's one key and checked packets itself: on the bench it relayed two nodes to the P4 with 0
corrupt frames (firmware/hub-p4/BRINGUP.md, steps 11 and 13), and the forbidden-frame refusals
were tested on hardware with a deliberately forging test build on 2026-11-18. The keyless gateway
is **simulated only** (tested on a computer and built for the board). Details: [The link between boards](link.md) and
[ESPNOW.md](../ESPNOW.md).

## The hub: hub-p4

`firmware/hub-p4`, an ESP32-P4 on the Elecrow CrowPanel Advanced 5" touch screen. It has 32 MB
of PSRAM (extra memory) inside the chip package, and the hub will not start without it: the
track table lives there. What it runs:

- the **tracker** (64 drones, 32 aircraft) and its spoofing, dispute and flood checks;
- the **coverage coordinator**, deciding each node's job every 2 s;
- the **touch screen**: radar, list, signals, health, settings and FAA flags pages;
- the **SD card**: a CSV log and the signed, hash-chained evidence log;
- **USB JSON** lines on its USB-C console for a laptop;
- its own **GPS** (u-blox MAX-M10S or Quectel LC29H, found automatically, with PPS) and the
  GPS jamming and spoofing checks;
- an **ADSBee 1090** input for crewed aircraft, and its own ADS-B decoder (off by default);
- decoding of the aero node's aircraft frames and the control-link table;
- **FAA rule flags** from an offline airspace pack on the SD card;
- a one-wire **feed to the web head**.

Proof: drone tracks through the gateway are **proven on the air**. The GPS and the ADSBee are
**simulated only** on the P4: the chip plays the module on its own pins
([PARITY.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub-p4/PARITY.md)).
The P4 board has no buzzer, so a new drone shows as a banner on the screen with no sound.

### The S3 hub and the web head

`firmware/hub` is the first hub, on an ESP32-S3. It reads up to three wired nodes, runs its own
Wi-Fi hotspot with the phone page, TAK Cursor-on-Target output, a small 320x240 screen and a
buzzer. Built with the `webhead` feature, the same firmware becomes the P4's **web head**: it
takes everything the P4 hears down one wire and serves the phone page on a hotspot on channel
11, away from Remote ID's channel 6
([WEBHEAD.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub-p4/WEBHEAD.md)).

> [!WARNING]
> As of 2026-11-18 the S3 firmware **does not link**: its fixed memory is about 18 KB over the
> chip's RAM budget (firmware/hub/README.md; [bench results](../BENCH.md)). So the phone page
> and TAK output are **built, not yet tried** with the P4, and nothing in `firmware/hub` has run
> on hardware since that change.

## rid-beacon: the test transmitter

`firmware/rid-beacon` sends a fake test drone (`TESTDRONE0000000001`, Self-ID
`BENCH TEST-NOT AIRCRAFT`) on every Remote ID transport, on an S3 (2.4 GHz) or a C5 (5 GHz too),
at the lowest power the chip allows. It can also send fake open-FPV-link frames. It is a **bench
tool only**: it is the known signal every receiver is tested against, and it must never imitate
a real aircraft.

## Watchdogs and crashes

Every board reboots itself after a crash (a "panic"), and every board except `rid-beacon` has a
**watchdog**, a timer in the chip that restarts it if its main work stops ([bench tests](../BENCH.md),
"Crash recovery"):

| Board | Watchdog fed by | Timeout | Tried on hardware (2026-11-18) |
|---|---|---|---|
| hub-p4 | the main loop, every pass | 30 s | yes: panic back in about 2 s, hang reset at 30.0 s |
| gateway | the relay loop, while the beacon loop is alive | 10 s | yes |
| node-wifi | the channel hopper, every hop | 30 s | yes (C5) |
| node-sdr | the capture loop | 10 s | yes |
| node-ble | the stats task, once a second | 10 s | not yet |
| node-lr | the stats loop, once a second | 10 s | not yet |

After a restart the board's first status line says why it last reset (power on, software,
watchdog, USB, brownout); the P4 also writes it into the evidence log. When a node comes back,
the coverage coordinator gives it its job again.

## Where this lives in the code

- `firmware/node-wifi`, `firmware/node-ble`, `firmware/node-sdr`, `firmware/node-lr`: the nodes.
- `firmware/node-common/src/lib.rs` and `crates/nodeimage/src/lib.rs`: image switching.
- `tools/partitions-node.csv`, `tools/flash-node.sh`: the multi-image flash layout and tool.
- `firmware/gateway/src/main.rs`: the gateway.
- `firmware/hub-p4/src/main.rs`, `app.rs`, `fault.rs`: the hub, its watchdog and panic reboot.
- `firmware/hub`: the S3 hub and web head.
- `firmware/rid-beacon`: the test transmitter.

**Next:** [The link between boards](link.md)
