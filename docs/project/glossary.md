# Glossary

**Level: 🟢 Beginner** · The acronyms and special words these pages use, explained in a sentence
or two the way Rotorscope uses it, with a link to the page that says more.

Jump to a letter:
[A](#a) · [B](#b) · [C](#c) · [D](#d) · [E](#e) · [F](#f) · [G](#g) · [H](#h) · [I](#i) ·
[J](#j) · [K](#k) · [L](#l) · [M](#m) · [N](#n) · [O](#o) · [P](#p) · [R](#r) · [S](#s) · [T](#t) · [U](#u) ·
[V](#v) · [W](#w) · [X](#x) · [Z](#z)

## A

**ADS-B** (Automatic Dependent Surveillance–Broadcast). The signal crewed aircraft send on
1090 MHz (and, in the US, on 978 MHz as UAT) saying who they are, where they are and how fast they
are going. Small drones almost never carry it. See [Crewed-aircraft signals](../detect/aircraft-signals.md).

**ADS-L.** An open European standard for light aircraft and drones to announce their position on
the 868 MHz band. Almost unused in the US, and Rotorscope does not decode it. See
[Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md).

**ADSBee.** A small ADS-B receiver (the ADSBee 1090) made by Pants for Birds. Rotorscope reads its
output to show aircraft beside drones. See [ADSBee 1090](../ADSBEE.md).

**Aero node.** A Rotorscope node built from an ESP32-C3 and an LR2021 radio that listens for
aircraft signals the ADSBee cannot hear (UAT, FLARM, OGN). Built, not yet tried: no LR2021 board
has been in hand. See [The aero node](../AERO-NODE.md).

**AeroScope.** DJI's own receiver, the one DroneID was built for. Rotorscope
reads the unencrypted kinds of DroneID with open code. See
[DJI DroneID](../detect/dji-droneid.md).

**AGL** (above ground level). Height measured from the ground below, rather than from sea level.
The FAA's 400 ft limit for most drone flights is AGL. See [FAA rule flags](../use/faa-flags.md).

**Airspace pack.** A file on the hub's SD card (`ROTORSCP/AIRSPACE.PAK`) holding airspace shapes,
restrictions and ground heights for one region, built from public FAA and USGS (US Geological
Survey) data by
`tools/airspace-pack`. It lets the FAA flags work offline. See [FAA rule flags](../use/faa-flags.md).

**Analog video.** The older kind of FPV video, sent as a plain TV-style signal, mostly on
5.8 GHz. Rotorscope's raw-capture node looks for the steady rate at which its picture lines repeat. See [FPV video links](../detect/fpv.md).

**ASD-STAN.** The European standards body whose Remote ID standard carries the same messages as
the US one. See [Remote ID](../detect/remote-id.md).

**ASTM F3411.** The US standard that defines Remote ID messages and how they are sent over Wi-Fi
and Bluetooth. Rotorscope's decoder follows it. See [Remote ID](../detect/remote-id.md).

**ATAK.** The Android Team Awareness Kit, a map app used by emergency services and others. It can
show Rotorscope's drones as Cursor-on-Target messages. See [Phone page and TAK](../use/phone-and-tak.md).

## B

**Band.** A range of radio frequencies, such as 2.4 GHz or 5 GHz. Each Rotorscope listener works
in one band at a time. See [How drones give themselves away](../start/drone-signals-101.md).

**Basic ID.** The Remote ID message that carries the drone's identity, usually its serial number.
See [Remote ID](../detect/remote-id.md).

**Beacon.** A Wi-Fi frame that a device broadcasts again and again to announce itself. Remote ID
can ride inside beacons. See [Remote ID](../detect/remote-id.md).

**BEAST.** A binary format for raw 1090 MHz aircraft messages. The ADSBee can send it so that
Rotorscope's own Mode S decoder does the decoding. See [ADSBee 1090](../ADSBEE.md).

**Bench.** The desk where boards are tested, usually about a metre apart. A "bench build" has test
features switched on and is refused unless `ROTORSCOPE_BENCH=1` is set. See [Bench tests](../BENCH.md).

**BLE** (Bluetooth Low Energy). The low-power form of Bluetooth that phones and Remote ID
broadcasters use. See [Remote ID](../detect/remote-id.md).

**Bootloader.** The small program that starts a chip and loads its main firmware. Rotorscope's C5
nodes use it to choose between several firmware images stored side by side. See
[Firmware reference](../dev/firmware.md).

**BT4 / BT5.** Bluetooth 4 sends short "legacy" adverts. Bluetooth 5 adds larger extended adverts
and Long Range. Remote ID can use all three, and Rotorscope's Bluetooth node hears all three. See
[Remote ID](../detect/remote-id.md).

## C

**Cargo.** Rust's build tool. `cargo test` runs the tests, `cargo build` compiles. See
[Build, test and flash](../dev/build-test-flash.md).

**CC2652P.** The Texas Instruments radio chip inside the Sonoff dongle that Rotorscope can turn
into a Bluetooth 5 receiver. See [A Sonoff dongle](../SONOFF.md).

**Channel.** One slot within a band, such as Wi-Fi channel 6 or 149. A receiver tunes to one
channel at a time. See [Who listens where](../how/coverage.md).

**Checkpoint.** A point in the evidence log where the hub signs the latest fingerprint: once a
minute, and when the operator asks (for example before removing the card). A signature covers
every record before it. See [The evidence log](../EVIDENCE.md).

**COBS** (Consistent Overhead Byte Stuffing). A way of packing messages so that a zero byte only
ever marks the end of one. A reader that joins mid-stream finds the next message at once. See
[Wire protocol reference](../dev/protocol.md).

**Coded PHY.** The Bluetooth 5 "Long Range" mode (PHY: the way bits are sent over the air), which
sends each bit as 2 or 8 symbols with error correction, trading speed for reach. See
[Remote ID](../detect/remote-id.md).

**Confirmed.** A drone track heard again at least a second after it was first heard. Only
confirmed drones raise the new-drone alert and go to TAK. See [The tracker](../how/tracker.md).

**Control link.** The radio link between a pilot's controller and the drone, such as ExpressLRS.
See [Control links](../CONTROL-LINKS.md).

**Controlled airspace.** Airspace around airports (Class B, C, D and surface E) where drones need
authorisation. Rotorscope flags a possible flight inside it, knowing it may be authorised. See
[FAA rule flags](../use/faa-flags.md).

**CoT** (Cursor on Target). The message format TAK apps use to put things on a map. Rotorscope
sends drones and operators as CoT. See [Phone page and TAK](../use/phone-and-tak.md).

**Coverage.** Which node listens to which band and channels. The hub's coverage coordinator
decides and tells each node its job. See [Who listens where](../how/coverage.md).

**Crate.** A Rust library or program. Rotorscope's shared code lives in `crates/`. See
[Crate reference](../dev/crates.md).

**CSBee.** The ADSBee's text output: one decoded aircraft per line, each line ending in a
checksum. See
[ADSBee 1090](../ADSBEE.md).

**CSV** (comma-separated values). A plain table file that spreadsheets open. The hub's everyday SD
card log is CSV. See [SD card and logs](../use/sd-card.md).

## D

**dBm.** A measure of radio power: 0 dBm is one milliwatt, and every 10 dB less is ten times
weaker. Received signals are strongly negative, such as -40 dBm. See
[Reading a detection](../use/reading-detections.md).

**DC offset.** A false constant level that a radio's receiver adds to its samples, showing up as a
spike at the centre frequency. The raw-capture node has the radio measure it again after each
retune. See [Hard-won lessons](../dev/gotchas.md#esp32-c5).

**Deauth** (deauthentication). A Wi-Fi frame that tells a device to disconnect. Floods of them are
a common attack, which Rotorscope's Wi-Fi nodes watch for. See
[Wi-Fi and Bluetooth attacks](../WIFI-ATTACKS.md).

**Differential test.** Feeding the same input to two programs and checking they agree. The Remote
ID decoder is tested this way against the official opendroneid-core-c decoder. See
[Crate reference](../dev/crates.md).

**DISPUTED.** The label a drone gets when two transmitters claim the same identity along separate
paths. Rotorscope says DISPUTED, never "fake", because it cannot tell which one is real. See
[Spoofed drones](../SPOOFING.md).

**DJI.** The drone maker whose own signals (DroneID and its OcuSync radio link) Rotorscope reads.
See [DJI DroneID](../detect/dji-droneid.md).

**DragonSync.** Open-source software that collects drone detections and passes them on to TAK map
apps and other software; it runs on WarDragon kits. Rotorscope's laptop viewer can feed it. See
[The laptop viewer](../use/rid-view.md).

**DroneBridge.** An open-source system for sending drone video and telemetry over ordinary Wi-Fi
cards. Rotorscope recognises its frames. See [FPV video links](../detect/fpv.md).

**DroneID.** DJI's own broadcast of a drone's serial number and position (and its pilot's). Older
drones send it in Wi-Fi beacons; OcuSync drones send it as a short radio burst. See
[DJI DroneID](../detect/dji-droneid.md).

## E

**eFuse.** Bits inside a chip that can be set once and never cleared, used to hold keys and switch
on secure boot. Burning them is permanent, so it is never done on bench boards. See
[The evidence log](../EVIDENCE.md#the-hardware-key-design-nothing-burned).

**ELRS** (ExpressLRS). The most common open-source control link for FPV drones, on 900 MHz and
2.4 GHz. See [Control links](../CONTROL-LINKS.md).

**ESP-NOW.** Espressif's short-message radio protocol. Rotorscope nodes that are not wired send
their reports to the gateway with it, each node signing with its own key once the kit is sealed
(a kit ships open). See [ESP-NOW in depth](../ESPNOW.md).

**ESP32.** A family of cheap microcontrollers (small computers on one chip) with built-in radios, made by Espressif. Rotorscope
is built from several of them. See [Boards and chips](../hardware/boards.md).

**ESP32-C3.** A small ESP32. It is the controller in the LR2021 node (built, not yet tried). See
[Boards and chips](../hardware/boards.md).

**ESP32-C5.** An ESP32 whose Wi-Fi works on both 2.4 and 5 GHz and which can sample its own radio.
Every Wi-Fi, Bluetooth and raw-capture node, and the gateway, can be a C5. See [Boards and chips](../hardware/boards.md).

**ESP32-C6.** A Wi-Fi companion chip on the hub's P4 board. Nothing in Rotorscope uses it yet:
driving it from Rust would be weeks of work, so a separate web head does the networking. See [Boards and chips](../hardware/boards.md).

**ESP32-P4.** A fast ESP32 with no radio of its own. It drives the hub's 5-inch touch screen. See
[Boards and chips](../hardware/boards.md).

**ESP32-S3.** The ESP32 the project started on. It still runs the 2.4 GHz and Bluetooth node
firmware and the web head, and needs Espressif's own Rust compiler. See
[Boards and chips](../hardware/boards.md).

**espflash.** The tool that writes firmware onto an ESP32 over USB. See
[Build, test and flash](../dev/build-test-flash.md).

**espup.** The installer for Espressif's Rust toolchain, needed for S3 builds. See
[Set up your machine](../dev/setup.md).

**Evidence log.** A separate log on the hub's SD card in which every record carries the
fingerprint of the one before and the hub signs the chain every minute, so later changes show. See
[The evidence log](../EVIDENCE.md).

**Evil twin.** A fake Wi-Fi network with the same name as a real one, to lure devices onto it.
Rotorscope's Wi-Fi nodes watch for a known name coming from a second transmitter with different
security, channel or maker. See
[Wi-Fi and Bluetooth attacks](../WIFI-ATTACKS.md).

**Extended advertising.** Bluetooth 5 adverts that can carry much more data than legacy ones,
including a whole pack of Remote ID messages. See [Remote ID](../detect/remote-id.md).

## F

**FAA** (Federal Aviation Administration). The US government's aviation regulator, which makes
the drone rules. See [FAA rule flags](../use/faa-flags.md).

**FANET.** An open protocol paragliders and hang gliders use to share positions over LoRa. See
[Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md).

**FAT.** The file system on the SD card (FAT32), readable by any computer. See
[SD card and logs](../use/sd-card.md).

**FCS** (frame check sequence). The checksum at the end of every Wi-Fi frame. A frame whose FCS
does not match was damaged on the way, and the radio drops it. Rotorscope's Wi-Fi nodes receive
each frame with 4 extra bytes in the FCS position (not a valid checksum) and cut them off before
reading. See [Remote ID](../detect/remote-id.md).

**Firmware.** The program that runs on a board. Each folder under `firmware/` is one. See
[Firmware reference](../dev/firmware.md).

**FLARM.** A collision-warning radio fitted to gliders, light aircraft and some helicopters, on
868 MHz in Europe and 902-928 MHz in the US. Its content is obfuscated; Rotorscope decodes it only
with a key file the owner supplies. See [Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md).

**Flashing.** Writing firmware onto a board, usually over USB. See
[Build and flash](../get-going/kit-flash.md).

**FPV** (first-person view). Flying a drone by watching live video from its camera, usually in
goggles. FPV drones often carry no Remote ID. See [FPV video links](../detect/fpv.md).

**Frame.** One message. On the link between boards, a frame is one report (a detection, a status)
in postcard and COBS form; on Wi-Fi, a frame is one packet on the air. See
[Wire protocol reference](../dev/protocol.md).

## G

**Gateway.** A C5 board wired to the hub that relays the reports of every wireless node, so the
hub sees one wired link that speaks for many nodes. It holds no key: it passes each packet on
unchanged, and the hub checks it. See [Nodes, gateway and hub](../how/nodes-gateway-hub.md).

**GB 46750.** China's drone broadcast standard (GB 46750-2025). Rotorscope's decoder reads it. See
[Remote ID](../detect/remote-id.md).

**GNSS / GPS.** Satellite positioning. GPS is the US system; GNSS covers all of them. A GPS receiver
gives the hub its own position and accurate time; none has been wired to the P4 hub yet. See [Position and time](../how/position-and-time.md).

**GPIO** (general-purpose input/output). A pin on a chip that software can read or switch. See
[Pins and wiring reference](../hardware/pinouts.md).

**GPL** (GNU General Public License). Rotorscope's licence, version 3 or later: anyone may use,
change and sell it, but whoever ships a changed version must publish their changes under the same
licence. See
[Credits and licences](credits.md).

## H

**Hash chain.** A list of records in which each one includes the fingerprint (SHA-256) of the one
before, so changing or removing a record in the middle breaks every later link. See
[The evidence log](../EVIDENCE.md).

**HDZero.** A digital FPV video system with a fixed channel grid. Rotorscope labels a wide, flat,
steady signal on that grid as HDZero; simulated only, never tried on a real one. See [FPV video links](../detect/fpv.md).

**HIL** (hardware in the loop). Testing real hardware with recorded input. `tools/droneid-hil`
feeds DJI recordings through a C5's own capture path. See [Bench tests](../BENCH.md).

**Hotspot.** A small Wi-Fi network the kit runs itself, so a phone can join it and open the
phone page with no internet. On the P4 kit the web head is meant to provide it; it does not work
yet. See
[Phone page and TAK](../use/phone-and-tak.md).

**Hub.** The board that gathers every node's reports, merges them into tracks, and shows them on
its screen, SD log and outputs. Today it is an ESP32-P4 touch-screen board. See
[Nodes, gateway and hub](../how/nodes-gateway-hub.md).

## I

**I/Q.** The two parts (in-phase and quadrature) of a raw radio sample, which together describe
the signal fully. The raw-capture node works on I/Q samples. See [DJI DroneID](../detect/dji-droneid.md).

**I2C.** A slow two-wire bus for connecting chips, such as the hub's touch controller. See
[Pins and wiring reference](../hardware/pinouts.md).

**ICAO address.** The unique 24-bit number every ADS-B aircraft sends, assigned with its
registration (ICAO is the United Nations' civil aviation body). See
[Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md).

## J

**Jamming.** Drowning a radio band in noise so receivers cannot hear. Rotorscope never jams, but
its raw-capture node watches for jamming. See [Jamming](../JAMMING.md).

## K

**Karma.** An attack in which a fake access point answers to any network name a device asks for.
Rotorscope's Wi-Fi nodes watch for one transmitter using many names. See
[Wi-Fi and Bluetooth attacks](../WIFI-ATTACKS.md).

**Kit master.** The one secret key a kit's owner makes with `link-key kit new` (`tools/kit.key`,
never in git). Every node's link key is worked out from it; the hub holds it too, so a captured
hub is a captured kit. See [ESP-NOW in depth](../ESPNOW.md#securing-the-link).

## L

**Link key.** A node's own secret key, which signs every ESP-NOW packet between that node and
the hub. It is worked out from the kit master and kept in a small part of the node's flash
(`rskey`), never in a firmware image or in git. Until 2026-11-18 one link key served the whole
kit, in `tools/link.key`, compiled into every image; that is gone. See
[ESP-NOW in depth](../ESPNOW.md#securing-the-link).

**Long Range.** Bluetooth 5's longer-reaching mode (coded PHY). Remote ID can use it, and
Rotorscope's Bluetooth node hears it. See [Remote ID](../detect/remote-id.md).

**LoRa.** A long-range, low-data-rate way of sending radio signals. Some control links use it, and the planned mesh
between Rotorscope units would too. See [Control links](../CONTROL-LINKS.md).

**LR2021.** A Semtech radio chip that covers 150-960 MHz and 1.5-2.5 GHz. It is the planned radio
for Rotorscope's control-link and aero node. See [The aero node](../AERO-NODE.md).

## M

**MAVLink.** The message protocol many autopilots and ground stations use for telemetry and
commands. Rotorscope can watch for hostile MAVLink when content reading is switched on. See
[Attacks on drones](../DRONE-ATTACKS.md).

**mdBook.** The Rust tool that turns these Markdown pages into the documentation website. See
[Writing these docs](../dev/docs-style.md).

**Message Pack.** A Remote ID message that bundles several others (identity, location, operator)
into one broadcast. See [Remote ID](../detect/remote-id.md).

**mLRS.** An open-source long-range control and telemetry link. See [Control links](../CONTROL-LINKS.md).

**Mode S.** The radar reply system aircraft transponders use on 1090 MHz; ADS-B messages are one
kind of Mode S message. Rotorscope has its own Mode S decoder. See
[Crewed-aircraft signals](../detect/aircraft-signals.md).

**Multicast.** Sending one network packet to every device that wants it. The web head sends TAK
messages to ATAK's usual multicast address, with a hop limit of 1 so they stay on the local
network. See [Phone page and TAK](../use/phone-and-tak.md).

## N

**NAN** (Neighbor Awareness Networking, the Wi-Fi Alliance's name, also called Wi-Fi Aware). A Wi-Fi feature for devices to find
each other; Remote ID can be sent in NAN frames. See [Remote ID](../detect/remote-id.md).

**no_std.** Rust code that does not need an operating system, so it runs both on a bare chip and on
a laptop. Most of Rotorscope's crates are no_std. See [Crate reference](../dev/crates.md).

**Node.** A listener board with one job, such as 5 GHz Wi-Fi or Bluetooth. Nodes listen for drones
and report to the hub. See [Nodes, gateway and hub](../how/nodes-gateway-hub.md).

## O

**O2 / O3 / O4.** Generations of DJI's OcuSync link. O2 sends DroneID in plain form, O3 mostly
does (one report says the Inspire 3 encrypts), and O4 encrypts it. See [DJI DroneID](../detect/dji-droneid.md).

**OcuSync.** DJI's own radio link for control and video between drone and controller. See
[DJI radio links in depth](../DJI-LINKS.md).

**OGN** (Open Glider Network). Open trackers and ground stations, mostly for gliders, using a
FLARM-like radio signal. See [Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md).

**OpenHD.** An open-source digital FPV video system over Wi-Fi cards. Rotorscope recognises its
frames. See [FPV video links](../detect/fpv.md).

**OpenIPC.** Open-source camera firmware widely used for digital FPV, sending video with wfb-ng.
See [FPV video links](../detect/fpv.md).

**Operator.** The person flying the drone. Remote ID can carry an operator ID and the operator's
position, which Rotorscope shows as broadcast. See [Reading a detection](../use/reading-detections.md).

## P

**P-256.** A standard kind of public-key cryptography (an elliptic curve) that the evidence log's
signatures use, in the form called ECDSA. See
[Evidence log format](../EVIDENCE-FORMAT.md).

**Panic.** A Rust program stopping because something went badly wrong. Every Rotorscope board
prints the reason and reboots itself. See [Bench tests](../BENCH.md).

**Part 107.** The US rules for commercial drone flight (14 CFR Part 107), the source of several of
the FAA flags, such as the 400 ft limit and visual line of sight. See [FAA rule flags](../use/faa-flags.md).

**Postcard.** A compact Rust format for turning data into bytes. Messages between boards use it.
See [Wire protocol reference](../dev/protocol.md).

**PPS** (pulse per second). A wire from the GPS receiver that ticks exactly on each second, so the
hub's clock can be set to well under a millisecond. See [Position and time](../how/position-and-time.md).

**Probation.** The time a new drone track spends unconfirmed, until it is heard again a second
later. It keeps one forged frame from raising an alarm. See [The tracker](../how/tracker.md).

**Promiscuous mode.** A Wi-Fi receiver setting that passes on every frame it hears, not only those
addressed to it. Rotorscope's Wi-Fi nodes listen this way. See [Remote ID](../detect/remote-id.md).

**PSRAM.** Extra memory added to a microcontroller, inside its package or as a separate chip,
slower than its built-in memory. The hub's P4 board has 32 MB of it. See
[Boards and chips](../hardware/boards.md).

## R

**Remote ID.** The broadcast most drones must send, by law in the US, carrying their identity,
position, speed and the operator's position, over Wi-Fi or Bluetooth. It is Rotorscope's main
signal. See [Remote ID](../detect/remote-id.md).

**Replay.** Recording a genuine message and sending it again later. A sealed ESP-NOW link drops
replayed packets. See [ESP-NOW in depth](../ESPNOW.md).

**`rid-beacon`.** Rotorscope's bench test transmitter: firmware that broadcasts Remote ID for a
plainly fake drone at the lowest power. See [Bench tests](../BENCH.md).

**`rid-view`.** The laptop viewer: it reads nodes over USB, files or a Bluetooth adapter and shows
a table, and can send TAK and DragonSync output. See [The laptop viewer](../use/rid-view.md).

**RSSI** (received signal strength indicator). How strong a signal arrived, in dBm. A rough guide
to distance, easily fooled. See [Reading a detection](../use/reading-detections.md).

**RubyFPV.** An open-source digital FPV system over Wi-Fi cards. Rotorscope recognises its frames.
See [FPV video links](../detect/fpv.md).

**Rust.** The programming language Rotorscope's firmware and most of its tools are written in. See
[What is Rotorscope?](../start/what-is-rotorscope.md).

## S

**SD card.** The memory card in the hub that holds the logs, the site settings and the airspace
pack. See [SD card and logs](../use/sd-card.md).

**SDR** (software-defined radio). A receiver that hands raw samples to a computer, which does the
decoding in software. Rotorscope uses the C5's own radio this way instead of a separate SDR. See
[Boards and chips](../hardware/boards.md).

**Signature.** A value made with a secret key that anyone holding the matching public key can
check. The evidence log's checkpoints are signed. See [The evidence log](../EVIDENCE.md).

**SiK.** Open-source firmware for telemetry radios (RFD900 and others) linking a drone's autopilot
to a ground station. See [Control links](../CONTROL-LINKS.md).

**`SITE.TXT`.** The site settings file on the hub's SD card: where the nodes are, which alarms are
muted, and which opt-ins are on. See [Site settings](../SITE.md).

**Sniffle.** NCC Group's open-source Bluetooth 5 sniffer firmware, which Rotorscope loads onto a
Sonoff dongle. See [A Sonoff dongle](../SONOFF.md).

**Sonoff dongle.** The Sonoff Zigbee 3.0 USB Dongle Plus (model ZBDongle-P), a cheap USB stick that
can become a Bluetooth 5 Remote ID receiver for a laptop. See [A Sonoff dongle](../SONOFF.md).

**SPI.** A fast bus for connecting chips such as screens and radios, with a select wire per chip.
See [Pins and wiring reference](../hardware/pinouts.md).

**Spoofing.** Broadcasting a false signal: an invented drone, or a false GPS position. Rotorscope
checks drone broadcasts and its own GPS for signs of it. See [Spoofed drones](../SPOOFING.md).

**Stale.** A track not heard for 60 seconds. It stays listed for a while, marked stale, then is
removed. See [The tracker](../how/tracker.md).

**SX1262.** A common Semtech LoRa radio chip, used on the Meshtastic boards (an open-source LoRa
messaging system) planned for sharing detections between units. See [Roadmap](roadmap.md).

## T

**TAK.** The Team Awareness Kit family of map apps (ATAK on Android, WinTAK, iTAK). See
[Phone page and TAK](../use/phone-and-tak.md).

**Test key.** A signing key whose secret is published in the repository so the tests can run. The
hub signs its evidence log with it, and says so, unless a development key was built in. A log
signed with it proves nothing, and the verifier rejects it unless told it is a test. See
[The evidence log](../EVIDENCE.md).

**TFR** (temporary flight restriction). An area the FAA closes to flights for a while, such as
around an event. TFRs change daily, so they must be refreshed. See [FAA rule flags](../use/faa-flags.md).

**Toolchain.** The compiler and tools needed to build firmware for a given chip. See
[Set up your machine](../dev/setup.md).

**Track.** One drone (or aircraft) in the hub's table, merged from every radio that heard it. See
[The tracker](../how/tracker.md).

## U

**UART.** A simple serial connection over two wires (send and receive). The gateway, and any
wired node, talks to the hub over one. See [Wire protocol reference](../dev/protocol.md).

**UAS / UAV.** Unmanned aircraft system / unmanned aerial vehicle: the formal words for a drone (the
system includes its controller).

**UAT** (Universal Access Transceiver). The US ADS-B on 978 MHz, used by light aircraft below
18,000 ft. The ADSBee 1090 cannot hear it. See [Beyond 1090 MHz](../AIRCRAFT-SIGNALS.md).

## V

**VLOS** (visual line of sight). The rule that a drone pilot must be able to see the drone. The
FAA flags estimate it from the distance between drone and operator, and terrain. See
[FAA rule flags](../use/faa-flags.md).

**VTX** (video transmitter). The part of an FPV drone that sends its camera's video. See
[FPV video links](../detect/fpv.md).

## W

**Walksnail.** A digital FPV video system (Walksnail Avatar). Rotorscope cannot yet tell it apart
from DJI's FPV system. See [FPV video links](../detect/fpv.md).

**WarDragon.** A ready-made drone-detection kit that runs DragonSync. See
[The laptop viewer](../use/rid-view.md).

**Watchdog.** A hardware timer that restarts a chip if its software stops feeding it, so a hung
board recovers by itself. See [Bench tests](../BENCH.md).

**Web head.** An ESP32-S3 fed by the P4 hub over one wire, serving the hotspot, phone page and TAK
output that the P4 cannot (it has no radio). Built, not yet tried: its firmware does not link
today (it is over its memory budget). See [Phone page and TAK](../use/phone-and-tak.md).

**wfb-ng.** An open-source system that sends digital FPV video over ordinary Wi-Fi cards; OpenIPC
uses it. Rotorscope recognises its frames. See [FPV video links](../detect/fpv.md).

**Wired bus.** Many nodes sharing one hub serial line, the hub asking each in turn. It means no
node ever transmits on the air. Simulated only: nobody has wired it yet. See
[The wired bus in depth](../WIREBUS.md).

## X

**XIAO.** Seeed Studio's thumb-sized development boards. The XIAO ESP32-C5 is Rotorscope's
standard node. See [Boards and chips](../hardware/boards.md).

## Z

**ZeroMQ.** A messaging library for sending data between programs. DragonSync reads detections
over it. See [The laptop viewer](../use/rid-view.md).
