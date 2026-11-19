# Law, safety and ethics

**Level: 🟢 Beginner** · What Rotorscope does and does not do with the signals it hears, why
some features are off until you turn them on, and what you are responsible for.

> [!IMPORTANT]
> This page is not legal advice. It describes how Rotorscope is built and what the project's
> own documents record. Laws on radio reception, drones and privacy differ between countries,
> states and cities. You are responsible for knowing and following the rules where you use it.
> If it matters, ask a lawyer.

## Rotorscope only listens

Rotorscope is a receiver. It does not jam, block, take over, or send anything to a drone, a
pilot's controller or any aircraft. The only transmissions in a working kit are its own: the
signed link between its boards and the Wi-Fi hotspot your phone connects to. The project
plan makes this a firm requirement ("Receive-only. No jamming, no spoofing.",
[PLAN.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/PLAN.md) requirement S3),
and it lists taking drones down or interfering with them as something the project will not
do.

Please keep it that way. Never use Rotorscope, or anything built from it, to interfere with an
aircraft or its radio links.

## Remote ID is public by design

**Remote ID** is a broadcast the drone sends so that people on the ground can see who is
flying and where. Listening to it is the purpose it was made for. It does, however, include
the **pilot's position** (or the take-off point), and the project plan treats that as personal
data. Treat what Rotorscope shows you with the same care you would give any record of where a
person was standing.

## The test transmitter, `rid-beacon`

To test its receivers without a drone, the project wrote one small transmitter,
**`rid-beacon`**. Its rules are written into its own README
([firmware/rid-beacon/README.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/firmware/rid-beacon/README.md))
and the project README's "Conduct" section:

- **A plainly fake identity.** The drone calls itself `TESTDRONE0000000001`, the operator
  `TEST-OPERATOR`, and its description reads `BENCH TEST-NOT AIRCRAFT`.
- **The lowest power its radio software allows:** 2 dBm on Wi-Fi and about −15 dBm on Bluetooth.
  "Metres, not kilometres."
- **Never imitate a real aircraft**, serial number, registration or manufacturer. It never
  sends DJI DroneID of any kind.
- **Indoors, on the bench, only while testing**, with one console key press (`x`) that silences
  every transmission at once.

A fake drone on someone else's screen could cause real alarm, or worse, hide a real one. That
is why these rules are not optional.

## Why some features are off until you turn them on

Some signals are not public broadcasts, so Rotorscope is more careful with them.

### Control links (the pilot's stick signals)

The project's control-link document records that a 2020 advisory from the US Department of
Justice, FAA, FCC and Department of Homeland Security warns that monitoring drone control links
can fall under the Wiretap Act and the Pen/Trap statute ([Control links](../CONTROL-LINKS.md),
"The legal line").
So, on every hub:

| Setting | Off (the default) | On (opt-in) |
|---|---|---|
| Identifiers (`control ids`) | Which system, where in the band, how strong, how sure | Also the link's identifier |
| Content (`control content`) | No content and no raw packets kept | Stick positions, telemetry, positions, raw packets for evidence |

The content setting reaches further than control radios. With it on, the Wi-Fi nodes also read
drone telemetry (MAVLink) that someone sends over a Wi-Fi network with no password
([Attacks on drones](../DRONE-ATTACKS.md), "Hostile MAVLink telemetry"). That means looking inside
another person's Wi-Fi traffic. With it off, they never do. Networks with a password are never
looked into either way.

The two are separate choices, made with lines in the hub's site file
([Site settings](../SITE.md)), and every change is written to the evidence log. The project
owner has turned content decoding on for their own kit on their stated permission; that
decision does not transfer to you. Encryption is never broken.

### Open FPV video links

Rotorscope recognises open digital video systems (wfb-ng, OpenHD, RubyFPV, DroneBridge) by
default, but their **link and vehicle identifiers are hidden** unless the hub is built with
that option. The one exception, by the owner's decision, is the evidence log, which always
keeps the raw bytes so that a record is complete.

### DJI DroneID

Older DJI drones send DroneID in plain form for third-party receivers, so Rotorscope shows it
by default, like Remote ID. The laptop viewer can hide the identifiers
(`rid-view --hide-droneid-ids`); as of 2026-11-18 the hub has no such setting. The plan calls
showing them by default "our call, not legal advice". Newer O4 drones encrypt it: without a key, Rotorscope only reports
that an O4 drone is present.

## Keys you must supply yourself

Two features need secret keys that are **never in the repository**, never printed, never
logged and never sent between boards. Each is off without its key file.

- **DJI O4 decryption.** No public key exists. The owner may install a lawfully obtained key
  on the hub's memory card ([O4 DroneID in depth](../DRONEID-O4.md), "Decryption"). The plan
  records a caution for anyone selling kits: decrypting "can lose the 'readily accessible' safe
  harbour and may raise anti-circumvention questions", so only use a lawfully obtained key.
  Even with a key, the hub never reads the pilot's DJI account name.
- **FLARM legacy decoding** (gliders and light aircraft). In May 2026 FLARM published its radio
  protocol with a free licence for non-commercial, receive-only use; commercial use needs a paid
  licence. The project owner reports having permission to use the FLARM legacy keys; that
  permission covers using them, not publishing them
  ([Beyond 1090 MHz: UAT and FLARM](../AIRCRAFT-SIGNALS.md)).

## FAA flags say "possible", never "illegal"

The hub can flag flights that **may** break an FAA rule: too high, in controlled or
restricted airspace, too far from the pilot, at night. It cannot see waivers, airspace
authorisations, public-safety operations, or whether a flight is flown under Part 107 or
as recreation.
So every flag reads "possible violation", names its evidence and data source, and never says
"illegal". A flag is a reason to look more closely, not a finding
([FAA rule flags](../use/faa-flags.md)).

## What the hub records

- **The memory-card log** keeps every detection with the time and the hub's own position, so
  it holds drone and pilot positions ([SD card and logs](../use/sd-card.md)).
- **The evidence log** keeps the raw bytes of every detection, signed so that later changes
  show ([The evidence log](../EVIDENCE.md)). The hub uploads nothing by itself; it reaches a
  server only if the owner runs the upload tool on a laptop.

Decide in advance who may see these files, how long you keep them, and how you will delete
them. A memory card is small and can go missing.

## Do not act on a detection alone

A detection shows that a broadcast was received. It does not prove who was flying. Remote ID
and DroneID can be faked; Rotorscope's [spoofing checks](../SPOOFING.md) help but have not been
tested against real attackers, and Rotorscope itself has not yet heard a real drone. Before
anyone confronts a pilot, calls the authorities or makes a safety decision, confirm with your
own eyes and other information. Never approach or interfere with an aircraft. What an attacker
can make a kit show, hide or record is set out in the [threat model](../THREAT-MODEL.md).

## Rules differ by country

Rotorscope's FAA flags follow US rules, and its examples use US frequencies. Other countries
have their own Remote ID rules (Rotorscope also decodes China's GB 46750 format), their own
radio bands and their own privacy and interception laws. The repository's documents describe
what the project chose to do; they do not tell you what is lawful where you are.

**Next:** [Choose your path](../get-going/README.md)
