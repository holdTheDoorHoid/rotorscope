# What it can and cannot do today

**Level: 🟢 Beginner** · An honest scoreboard: what works, how far each part has been proven,
what Rotorscope will miss, and what is coming next. As of 2026-11-18.

A detector that overstates itself is dangerous: you might relax when you should not. So this
page says plainly what has been shown to work and how. It uses six fixed phrases:

| Phrase | Meaning |
|---|---|
| **Heard for real** | Received from a real device someone else made (a real aircraft, a real drone). |
| **Proven on the air** | Real boards received it over the air from the project's own test transmitter. |
| **Proven on recordings** | Decodes public recordings of the real signal; not yet heard live. |
| **Simulated only** | Tested with made-up data on a computer or on the chip itself. |
| **Built, not yet tried** | The code exists and compiles; nobody has run it against the real thing. |
| **Planned** | Not built. |

The detailed, piece-by-piece table is on the [status page](../project/status.md).

## What works, and how far it is proven

### Hearing drones and aircraft

| What | How far | In plain words |
|---|---|---|
| Remote ID over 2.4 GHz Wi-Fi, 5 GHz Wi-Fi and Bluetooth (all seven flavours) | **Proven on the air** | On 2026-11-15 every flavour went from the test transmitter, through a listener node and the gateway, into the hub's own drone list ([bench tests](../BENCH.md)). |
| Remote ID on a laptop (built-in Bluetooth, or a Sonoff USB dongle) | **Proven on the air** | `rid-view` heard the test transmitter and a laptop's test advert. |
| DJI DroneID, older drones (OcuSync 2, 3, 3+) | **Proven on recordings** | Every burst in two public recordings decoded, fed through the listener chip's own capture path. Never a live DJI drone. |
| DJI DroneID, O4 drones (encrypted) | **Proven on recordings** | O4 records found in public recordings of two real O4 drones, on a computer. Following a drone's session is **Simulated only**. Never a live O4 drone. |
| DJI Wi-Fi DroneID (older Wi-Fi DJI models) | **Simulated only** | Decoder tested on hand-built frames. |
| Open digital FPV video links | **Proven on the air** | All four systems recognised from the test transmitter's fake frames. Never real FPV hardware. |
| Analog FPV video | **Simulated only** | The scanner ran on the air for 200 seconds (over three minutes) with no video transmitter present and raised nothing false. It has never heard a real one. |
| Toy and hobby drone Wi-Fi networks | **Simulated only** | A table of 13 makers, tested on made-up data. |
| Control links (ExpressLRS and others) | **Simulated only** | The recognisers are tested on made-up signals. Hearing real links needs an LR2021 radio board, whose firmware is **Built, not yet tried**: the project does not have the board yet. |
| Crewed aircraft by ADS-B (1090 MHz) | **Heard for real** | Real aircraft overhead, through an ADSBee plugged into a laptop. The ADSBee is not yet wired to the hub. |
| The project's own ADS-B decoder | **Heard for real** | Agreed with the ADSBee's own decoding on 98 to 100 % of checks over 15 minutes of live traffic, on the laptop. |
| UAT, FLARM and other light-aircraft signals | **Simulated only** | The decoders are tested on made-up frames; hearing them needs the LR2021 aero node or a 978 MHz receiver, which are **Built, not yet tried** or not built. |

### Making sense of what it hears

| What | How far | In plain words |
|---|---|---|
| Merging reports from every radio into one drone | **Proven on the air** | One track for the test drone across all seven Remote ID flavours. |
| The hub's GPS (its own position and time) | **Simulated only** | Tested by the chip feeding its own pins; no real GPS module wired to the hub yet. |
| Spotting faked drones (23 consistency checks) | **Simulated only** | [Spoofed drones](../SPOOFING.md). |
| Spotting Wi-Fi and Bluetooth attacks, and jamming | **Simulated only** | [Attacks and interference](../detect/threats.md). |
| Spotting jamming or faking of the hub's own GPS | **Simulated only** | [GPS jamming and spoofing at the hub](../GNSS-INTEGRITY.md). |
| FAA rule flags ("possible violation") | **Simulated only** | Runs on the hub, but only against made-up drone positions ([FAA rule flags](../use/faa-flags.md)). |
| Warning of a drone near a crewed aircraft | **Simulated only** | [Drone near a crewed aircraft](../AIRCRAFT-CONFLICT.md). |

### Getting the results out

- **Hub touch screen and memory-card log** work on the bench: the hub wrote and read back
  every row of its log in the 2026-11-15 run.
- **Signed link between boards** runs on the bench (**Proven on the air**): on 2026-11-18 the
  gateway refused every kind of frame a test board was not allowed to send. Dropping packets
  with a bad signature or replayed ones has been tested only on a computer.
- **Evidence log** works on the bench. The hub signs it with a published test key unless a
  development key is built in, and the screen says which. Either way it is not yet
  evidence-grade; [the evidence log page](../EVIDENCE.md) says exactly what it does and does
  not prove.
- **Phone page, hotspot and TAK from the kit: not available today.** The phone page ran on the
  earlier ESP32-S3 hub with made-up drones. In the current kit a separate **web head** board is
  meant to serve it; only the hub's side of that wire has been tested (looped back on the chip).
  As of 2026-11-18 the S3 firmware the web head runs does not link: it needs 18,088 bytes more
  static memory than the chip has, and cannot be used until it is slimmed down
  ([hub README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/hub/README.md)).
  So the kit has no phone page, hotspot or TAK output today; for a TAK map, use the laptop
  viewer.
- **Laptop viewer** `rid-view` works, including TAK output and a demo that needs no hardware
  ([Step 1: The demo](../get-going/step-1-demo.md)).
- **Wiring nodes to the hub on one shared cable** (instead of radio): **Simulated only**, on a
  laptop.

## What it will not do, or cannot see

- **It has not yet heard a real drone.** The project owns no drone. Everything about drones
  above was proven with its own test transmitter, public recordings or made-up data.
- **It cannot see a drone that does not transmit** something it understands. A drone with
  Remote ID switched off or not fitted, and no other radio Rotorscope hears, is invisible.
- **It does not locate a silent or anonymous drone.** Position comes only from what a drone
  says about itself (Remote ID, DroneID). For video and control links it can say "something is
  here", not where.
- **Encrypted signals stay locked.** O4 DroneID is detect-only unless the owner installs a key
  of their own; encrypted control links are reported as "encrypted". Nothing tries to break
  encryption.
- **No range figure exists yet.** The plan requires measuring real range and publishing it;
  that has not been done. Do not assume any particular distance.
- **It never says "illegal".** FAA flags say "possible violation". The hub cannot see waivers,
  authorisations or what kind of flight it is.
- **It does not take drones down, jam or interfere.** It only listens.
- **Some signals are out of reach.** Walksnail cannot be told apart yet, HDZero is recognised
  only in simulation, and DJI's video and
  controller links are recognised only as "a DJI link", never read. Some bands (1.2 GHz video,
  900 MHz without the LR2021) no ESP32 chip can tune to, and analog video channels from
  5,820 MHz up need an add-on module.
- **A detection is a claim, not proof of who is flying.** Remote ID can be faked; the
  spoofing checks help but are untested against real attackers.
- **No kit is on sale.** There is no carrier board or case yet; today it is a set of
  development boards on a bench.

> [!WARNING]
> Do not rely on Rotorscope for safety decisions yet. It has not been tested against real
> drones, and nobody knows its real-world range.

## What is coming

The next big steps, as the [status page](../project/status.md) lists them, are wiring the
listener nodes to the hub over one shared cable, getting the web head working with the new hub,
connecting a real GPS and ADSBee to it, and trying the LR2021 control-link node on real hardware.
Beyond that the plan covers recognising more digital video links, a carrier board and case for a kit, a browser-based flasher for
kit buyers, and a hardware-held key for the evidence log. The first real-drone and range tests
are the gaps that matter most. Order and detail: [Roadmap](../project/roadmap.md).

**Next:** [Law, safety and ethics](law-and-ethics.md)
