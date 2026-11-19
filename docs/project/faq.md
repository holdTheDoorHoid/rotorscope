# FAQ

**Level: 🟢 Beginner** · Straight answers to the questions newcomers, buyers, security
researchers and sceptics ask most, each with a link to the page that says more.

Facts here are as of 2026-11-18. Words such as **Proven on the air** are the project's six proof
phrases, explained on the [Status](status.md#how-to-read-the-proof-column) page.

## The basics

### What is Rotorscope?

A detector for drones, built from several cheap ESP32 boards with firmware written in Rust. It
listens for the radio signals drones give off, chiefly **Remote ID**, the "digital licence plate"
most drones must broadcast, and shows what it hears on a touch screen, in logs, and in tools such
as ATAK. It is open source and still a bench prototype.
More: [What is Rotorscope?](../start/what-is-rotorscope.md)

### Does it jam or take down drones?

No, never. Rotorscope only listens. It has no code for jamming, spoofing or interfering with any
drone, and "taking drones down or interfering with them in any way" is on the plan's list of
things the project will not do. More: [Law, safety and ethics](../start/law-and-ethics.md)

### Does it transmit anything at all?

Only to talk to itself and to you. In today's bench kit the listener nodes send their reports to a
gateway board over ESP-NOW (a short-range Wi-Fi link); the wired bus, built but not yet wired, is
meant to replace that so nodes never transmit. The phone page needs a small Wi-Fi hotspot, served
by a separate web-head board whose firmware does not link as of 2026-11-18. The
separate test transmitter, `rid-beacon`, is a bench tool and not part of a detector.
More: [The link between boards](../how/link.md)

### Will it see every drone?

No. It sees only drones that broadcast something it can read, within range of the bands it
listens to. A drone that sends no radio signal is invisible to it, and so is one using a link no
Rotorscope board can tune to. Each board listens on one band at a time and hops between
channels, so some messages are missed. DJI's newest O4 drones can be detected but not read
without a key. More: [What it can and cannot do today](../start/capabilities.md)

### How far can it hear?

Nobody knows yet. **No range has been measured**: every test so far has had the boards about a
metre apart on a desk. The plan requires measuring range with stock and external antennas and
publishing the numbers; that has not happened yet. More: [Status](status.md#known-gaps)

### Has it detected a real drone yet?

No. As of 2026-11-18 Rotorscope has **never heard a real drone**. Remote ID is
**Proven on the air** from the project's own test transmitter, DJI DroneID is
**Proven on recordings** of real DJI drones, and the only thing **Heard for real** is crewed
aircraft through an ADSBee receiver. More: [Status](status.md)

### Does it need the internet?

No. The hub works offline: the airspace map for the FAA flags is a file on the SD card, and the
phone page is meant to come from the kit's own hotspot (not available on the P4 kit yet). A network
is needed only on a laptop, to build the airspace file and refresh temporary flight restrictions,
or to upload evidence records to a server, and both are the owner's choice.
More: [FAA rule flags](../use/faa-flags.md)

### Why does it show aeroplanes?

So you know when a crewed aircraft, such as a medevac helicopter, is nearby. An optional ADSBee
receiver hears the ADS-B signals aircraft send. Aircraft go in their own table and never set off
the new-drone alarm, and the hub is built to warn when a drone is close to one (that warning is
**Simulated only**).
More: [Crewed aircraft](../use/aircraft.md), [Drone near a crewed aircraft](../AIRCRAFT-CONFLICT.md)

### Can it track the pilot?

Only as far as the drone tells it. Standard Remote ID includes the operator's position (often the
take-off point), and DJI's plaintext DroneID includes the pilot's position; Rotorscope shows what
was broadcast. It has no other way of finding a pilot.
More: [Reading a detection](../use/reading-detections.md)

### What about FPV drones with no Remote ID?

Partly covered, and none of it tried on real equipment yet. Rotorscope recognises open-source
digital FPV links (**Proven on the air** with fake frames), looks for analog video transmitters
and DJI's radio links (**Simulated only** and **Proven on recordings**), and has firmware for
control links such as ExpressLRS waiting for its radio hardware (**Built, not yet tried**).
More: [FPV video links](../detect/fpv.md), [Control links](../CONTROL-LINKS.md)

## Law and where it works

### Is it legal to use?

Remote ID is public by design: a broadcast meant for anyone to receive, and listening to it is
what Rotorscope is for. Some features are a different matter. Reading control-link identifiers or
content, decoding FLARM, and decrypting DJI O4 are separate opt-ins, because monitoring links can
raise wiretap and similar questions. This is not legal advice; read
[Law, safety and ethics](../start/law-and-ethics.md) and check your own local law.

### Does it work outside the US?

The Remote ID decoder follows the US and European standards (ASTM F3411 and ASD-STAN) and also
reads China's GB 46750 format. The defaults are set for the US, though: the Wi-Fi country is
"US", the aircraft and control-link plans use US bands, and the **FAA flags are US-only**, built
from FAA data. Other countries' rules and bands differ.
More: [Remote ID](../detect/remote-id.md), [FAA rule flags](../use/faa-flags.md)

### What is the test transmitter, and is it legal?

`rid-beacon` is firmware that makes a spare board broadcast Remote ID for a plainly fake drone
(`TESTDRONE0000000001`, Self-ID "BENCH TEST-NOT AIRCRAFT"), so the receivers have something known
to hear. It uses the lowest power the libraries allow (2 dBm Wi-Fi, -15 dBm Bluetooth) and is meant
for a desk, metres not kilometres. The project's rule is low power, a fake identity, indoors, and
never imitating a real aircraft; whether that is allowed where you are is for you to check.
More: [Law, safety and ethics](../start/law-and-ethics.md),
[rid-beacon README](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/rid-beacon/README.md)

## Getting one

### Can I buy one?

Not yet. Selling kits at conventions is the aim, but there is no kit hardware, case or build
guide yet. You can build the bench version from the parts in this documentation.
More: [Roadmap](roadmap.md), [Parts and tools](../get-going/kit-parts.md)

### What does it cost to build?

There is no current costing. The plan set a parts budget of **$100–200** per unit, and its rough
parts list for the original S3-based Remote ID unit came to about **$117–143**. Those are rough
estimates made before the hub moved to the ESP32-P4 board, and they do not include an ADSBee. More: [PLAN.md, "Rough parts list"](https://github.com/holdTheDoorHoid/rotorscope/blob/main/PLAN.md)

### Can I try it without buying anything?

Yes. The laptop viewer has a demo mode with made-up drones; it needs only Rust on a computer.
With one ESP32-C5 board you can listen for Remote ID over the air.
More: [Step 1: The demo, no hardware](../get-going/step-1-demo.md),
[Step 2: One board](../get-going/step-2-one-board.md)

### Can I use it with ATAK?

Yes, through TAK Cursor-on-Target messages. The laptop viewer sends them today (`rid-view
--cot`). The kit's own TAK output comes from the web head board, whose firmware does not link
(it is over its memory budget) as of 2026-11-18, so on the P4 kit use the laptop for now.
More: [Phone page and TAK](../use/phone-and-tak.md)

### Can I use it with WarDragon or DragonSync?

The laptop viewer can publish drones and signal alerts in the ZeroMQ JSON that DragonSync reads
(`rid-view --dragonsync`). It was tested with DragonSync's own parsing code, but not yet against
a running DragonSync or a WarDragon kit (**Simulated only**).
More: [The laptop viewer, rid-view](../use/rid-view.md)

### What licence is it under, and can I sell kits?

GPL-3.0-or-later. Anyone may use, change and sell Rotorscope, including as kits, but whoever ships
a changed version must publish their changes under the same licence.
More: [Credits and licences](credits.md)

## Security and evidence

### What does it record, and who can read it?

The hub writes an everyday CSV log and a separate evidence log to its SD card, and streams JSON
over USB. These contain what was broadcast, which can include the pilot's position: personal
data. Anyone holding the SD card or plugged into the USB port can read them. Uploading to a
server is off unless the owner turns it on.
More: [SD card and logs](../use/sd-card.md), [The evidence log](../EVIDENCE.md)

### Can someone fake a drone to fool it?

Yes. Remote ID, DroneID and ADS-B carry no authentication, so anyone can broadcast a forged one,
and Rotorscope cannot prove who sent a signal. It can notice when a broadcast disagrees with
physics, with itself or with what else it hears, keeps new drones "unconfirmed" until heard
again, raises an alarm on floods of fake drones, and marks a drone **DISPUTED** when two
transmitters claim one identity. These checks are **Simulated only** so far.
More: [Spoofed drones](../SPOOFING.md)

### Can someone inject fake reports between the boards?

Once a kit is sealed, every ESP-NOW packet is signed with that node's own key, so forged or
replayed packets are dropped and reported, and the hub refuses frames a node is not allowed to
send. Kits ship open, and stay open until the owner seals them. Someone who takes a node can
speak as that node until its slot is revoked; someone who takes the hub has the whole kit. The
link documents spell out exactly what.
More: [ESP-NOW in depth](../ESPNOW.md)

### Can it decode DJI?

It decodes the plaintext DroneID bursts of DJI's OcuSync 2 and 3 drones (serial, drone and pilot
position) on public recordings, also on the C5 chip itself (**Proven on recordings**). It also
reads DJI's older Wi-Fi DroneID beacons (**Simulated only**).
It has never heard a real DJI drone, and it never transmits DroneID.
More: [DJI DroneID](../detect/dji-droneid.md)

### What about encrypted DJI O4?

O4 records are encrypted. Without a key Rotorscope can still detect an O4 drone and follow its
session ID: it has found valid O4 records in public recordings of real O4 drones
(**Proven on recordings**), while following a session is **Simulated only**. It can decrypt them only with a key the owner places on the SD card, which never goes
in the repository. **No public key exists**, so this has only been tested with a made-up key.
More: [O4 DroneID in depth](../DRONEID-O4.md)

### Is the evidence log admissible in court?

That is a court's decision, and the project says to have a lawyer review the method first. Today
the log proves only that records have not changed since they were signed by whoever holds the
signing key. It does not prove which unit made the log, that the log is complete, or that the
times are true. The hub signs with the **published test key** unless a **development key** (which
can be read out of the unit) is built in, and the screen and every checkpoint say which; there is
no secure boot. The verifier counts a log signed with the published test key as a failure.
More: [The evidence log](../EVIDENCE.md)

### How do I report a security problem?

Privately, through the repository's Security tab, not in a public issue.
More: [Security policy](security.md)

## Why it is built this way

### Why ESP32s and not an SDR?

Cost and simplicity: the boards cost a few dollars each and the plan's budget is $100–200 a
unit. The ESP32-C5 can also read raw samples from its own radio, which is how Rotorscope looks for
DJI DroneID and analog video without a separate SDR. The trade-off is reach: no ESP32 can tune to
900 MHz, and the C5's raw capture stops at about 5810 MHz, so those need other radios.
More: [Boards and chips](../hardware/boards.md)

### Why so many chips?

Each chip has one radio and can listen to one band at a time. One open-source project measured a
single chip sharing Wi-Fi and Bluetooth catching 4 Wi-Fi Remote ID frames in the time it caught 234
Bluetooth ones, so Rotorscope gives each job its own chip. The hub is separate because a listening
chip cannot also run a hotspot without going deaf.
More: [The big picture](../how/architecture.md)

### Why Rust?

It was the owner's choice at the start: Rust wherever the ecosystem allows, with any exception
written down in the plan. One practical gain: the same Rust library decodes Remote ID on the chips and in tests
on a laptop, where it is checked against the official C decoder.
More: [A tour of the repository](../dev/repo-tour.md)

### How do you know the decoders are right?

The Remote ID decoder is compared field for field with the official opendroneid-core-c decoder
on large numbers of random messages. The DroneID decoder decodes all 13 bursts in two public
recordings. Rotorscope's own ADS-B decoder agreed with the ADSBee on 98-100 % of checks over 15
minutes of live traffic.
More: [Bench tests](../BENCH.md), [Crate reference](../dev/crates.md)

### How can I help without hardware?

Run the demo, read the code and documents and report what is wrong or unclear, review the
security-sensitive parts, or contribute recordings of drone signals the project lacks (Walksnail,
HDZero, OpenIPC, Skydio). Many of the crates are tested entirely on a laptop.
More: [Contributing](../dev/contributing.md)

### Who makes it?

It is a small open-source project published under the
[holdTheDoorHoid](https://github.com/holdTheDoorHoid/rotorscope) GitHub account, and contributions
are welcome ([Contributing](../dev/contributing.md)). It builds on many other people's open work,
listed in [Credits and licences](credits.md).

Next: [Glossary](glossary.md)
