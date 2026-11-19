# Threat model: what a kit resists, and what it does not

**Level: 🟢 Beginner, then 🟡 Intermediate** · Who might try to fool, blind, break into or
misuse a Rotorscope kit, how far each of them gets today, and what you should therefore not
rely on. As of 2026-11-19.

> [!IMPORTANT]
> Rotorscope is pre-release and has not yet heard a real drone. Treat it as a tripwire and a
> notebook: something that tells you where to look and keeps a record of what it heard. Do not
> treat it as proof of who flew what, and do not let a quiet screen tell you the sky is empty.

A **threat model** is a plain list of who might attack a thing, what they want, and how far
they can get. This page collects, in one place, the limits that the deep-dive pages state one
subject at a time. Where it says how far something has been proven it uses the six phrases from
[What it can and cannot do today](start/capabilities.md).

## What a kit is trusted to do

| What you rely on | In plain words |
|---|---|
| A true picture | What the screen shows was really heard, and is labelled for what it is |
| A complete picture | A drone that announces itself is not hidden from you |
| Staying awake | The boards keep listening, and say so when they cannot |
| Honest records | The logs say what was heard and when, and a change to them shows |
| Other people's privacy | Records of where a pilot stood reach only the people you choose |
| Its own keys | The secrets that bind the boards together and sign the records stay secret |
| Its own software | The firmware on the boards is the published code, built without tampering |

## Who might attack it

| Who | What they have | What they want |
|---|---|---|
| **A prankster or protester nearby** | A hobby radio board and code from the internet | Fake drones on your screen, or so many that you stop looking |
| **A careful radio attacker** | A software radio, time and skill | A believable fake, a real drone hidden, a wrong clock or position on the hub |
| **Someone who picks up a board** | A USB cable and a minute alone with a node, the gateway or the hub | The kit's keys, or their own firmware on your board |
| **Whoever holds the kit and its memory card** | Everything, including you, the owner | Records that say something other than what happened |
| **Someone on the kit's Wi-Fi hotspot** | A phone or laptop in range | Settings changed, or the page taken down |
| **Someone upstream of the software** | A poisoned library, download or contribution | Their code inside every kit |

A pilot who switches off or never fits [Remote ID](project/glossary.md) is not attacking the
kit, but the effect is the same as hiding: see [Hiding a real drone](#hiding-a-real-drone).

## The short version

| An attacker wants to... | Today |
|---|---|
| Put a fake drone on your screen | **Not prevented.** Drone broadcasts carry no proof of who sent them. Clumsy fakes are flagged; a careful one is not |
| Hide a drone that is broadcasting | Hard to do quietly: floods and jamming raise alarms. A drone that does not broadcast is not seen at all |
| Stop the kit listening | **Jamming works.** Crashes and hangs restart the board by themselves |
| Forge traffic between your boards | **Refused**, once you have sealed the kit. A kit as shipped is open and says so |
| Read your keys or change your firmware | **Not prevented** for anyone who can plug into a board. There is no secure boot or flash encryption yet |
| Change a record after it was written | **Shows**, if you check the log against the unit's key |
| Write false records as the owner | **Not prevented.** The signing key can be read from the unit |
| Read where pilots were standing | **Not prevented** for anyone who holds the memory card or is in range of the boards' link |
| Slip hostile code into the firmware | Made harder by locked dependencies and automatic checks; the build computer is still trusted |

The sections below say what stands behind each line.

## Fake drones and aircraft

Remote ID, DJI DroneID, ADS-B, FLARM and the control links Rotorscope listens to carry no
authentication: anyone can transmit a forged one, and tools to do so are public and cheap
([Spoofed drones](SPOOFING.md), "What attackers have"). Rotorscope shows what was claimed on the
air. It cannot show who sent it.

What the kit does about it (**Simulated only**: tested with made-up data on a computer and on
the chips, never against a real attacker):

- 23 consistency checks compare a broadcast with physics, with the radio layer and with other
  sources, and flag what does not fit.
- Every detection is labelled as a claim ("Claim: unverified broadcast"), and two transmitters
  claiming one identity show as "DISPUTED" instead of one silently replacing the other.
- A new identity starts unconfirmed, and a flood of new identities raises an alarm instead of
  pushing established drones off the list.

What it does not do: a careful attacker can beat every one of these checks. A fake that moves
along a smooth path, keeps a plausible speed and height, and uses a well-formed serial number
looks like a drone. [Spoofed drones](SPOOFING.md) lists the cases under "What still gets
through".

So: never act on a detection alone. Confirm with your own eyes before anyone confronts a
pilot or calls the authorities ([Law, safety and ethics](start/law-and-ethics.md)).

## Hiding a real drone

- **A drone that does not transmit something Rotorscope understands is invisible to it.** That
  includes a drone with Remote ID removed or switched off, and anything flown on a link the kit
  has no radio for ([What it can and cannot do today](start/capabilities.md)).
- **[Jamming](project/glossary.md) a receiver stops it receiving.** The nodes look for the signs
  of a jammer and report them ([Jamming](JAMMING.md)), **Simulated only**.
- **Floods.** Up to 1,000 fake frames a second, injected on the node chip itself, did not
  restart the node or lose the test drone ([Bench tests](BENCH.md); **Simulated only**: nothing
  was sent on the air). A flood shows as an alarm. It can still slow down how quickly a real
  drone is confirmed.
- **Encrypted DJI drones** (O4) are reported as present, without position, unless you have
  supplied a key. A forger can reduce how often such a drone is heard
  ([O4 DroneID in depth](DRONEID-O4.md)).

A quiet screen means "nothing announced itself that this kit could hear". It does not mean
"nothing is flying".

## Stopping the kit listening

- **Crashes and hangs.** A board that crashes restarts by itself in about half a second, and a
  board whose main loop stops is reset by a watchdog after 10 or 30 seconds. Tried on the bench
  on the hub, the gateway and two of the node firmwares on 2026-11-18; the Bluetooth and
  long-range node firmwares are **Built, not yet tried**.
- **Malformed radio input.** Every part that reads bytes from the air or from a file has tests
  that feed it damaged and random input ([Build, test and flash](dev/build-test-flash.md), "What
  CI checks"). None is known to crash. That is evidence, not a guarantee.
- **Power and cables.** Unplugging a board stops it. The hub's "Node blinded" alarm covers a node
  that can no longer hear what it normally hears, whatever the cause
  ([Alarms and warnings](use/alarms.md)).

## The link between boards

The nodes report to the hub by radio, through a gateway
([The link between boards](how/link.md)). **Proven on the air** on 2026-11-19 with the project's
own test boards playing the attacker ([Bench tests](BENCH.md)).

Once you have **sealed** the kit:

- Every packet, both ways, carries a tag made with that node's own key. Forged, replayed and
  late packets are refused, and the hub raises "Forged node traffic".
- A node that is taken can speak only as itself. You revoke it and the rest of the kit carries
  on.
- The gateway holds no key, so taking it gives nothing to forge with. It can still drop or
  delay traffic. The hub counts what each node says it sent against what arrived and raises
  "Node reports going missing" when at least 50 frames and more than 20 % are lost in a minute
  or in five, or when a node's reports stop arriving. Lighter loss than that is not flagged.

What it does not do:

- **A kit as shipped is open.** Until you seal it the link is unauthenticated and the hub's
  screen says `OPEN LINK` ([ESP-NOW in depth](ESPNOW.md), "Kits ship open; sealing yours").
- **A captured hub is a captured kit.** The hub holds the kit's master key in its flash memory.
- **Nothing is encrypted.** Anyone in range can read the nodes' reports and the hub's commands.
  They are, for the most part, what anyone nearby could hear from the drone itself.
- **The wired bus is not authenticated.** Whoever can touch its wires can report as any node on
  it. The wired bus is **Simulated only**.
- **Jamming the link works**, and a forger can crowd one node out by flooding its address. Both
  show as nodes going quiet.

## Someone who can plug into a board

This is the weakest side of a kit today, and the project says so wherever it comes up.

- Reading a board's flash memory takes the board and a USB cable for about a minute. That
  gives up the board's link key (for the hub: the kit master) and the hub's evidence signing
  key.
- **There is no secure boot and no flash encryption.** Anyone who can plug in can load their
  own firmware, which can keep the board's key and lie about everything else. Sealing a unit
  with your own secure-boot key is **Planned**; nothing has been tried on hardware.
- Keeping the signing key in a separate **secure element** chip, where software cannot read it,
  is **Planned**.
- **The memory card is ordinary and unencrypted.** Whoever holds it can read every log, and the
  decryption key files if you put them there ([SD card and logs](use/sd-card.md)).

So: a kit left unattended in a place an opponent can reach should be treated as theirs until
you have reflashed every board and made new kit keys.

## The records and the evidence log

The hub writes a plain log and a signed, chained **evidence log**
([The evidence log](EVIDENCE.md)). Signing and checking ran on the bench hub; the checker
refuses a log signed with the published test key unless told to accept it.

- **A change shows.** Altering, removing or reordering records after they were signed makes the
  check fail, provided you give the checker the unit's key from outside the log.
- **A missing boot in the middle shows**, and so does an older boot's log cut short.
- **It does not show which unit wrote a log.** The key can be read from the unit and from the
  computer that built its firmware, so anyone with either can write a log that checks out. That
  includes the owner: the log cannot answer "the owner made it up".
- **It does not show that a log is complete.** The newest records, the newest files or a wiped
  card leave no trace.
- **It does not show that the times are true.** They come from the hub's own clock and GPS.
  Outside time-stamps are built and off by default; they show a log existed by the time it was
  uploaded, not when its records were written.

[The evidence log](EVIDENCE.md) lists what is still missing before a log could be called
court-grade. Until then, describe it as a tamper-evident log, not as evidence that speaks for
itself.

## The hub's own position and time

The hub stamps records with its GPS position and time, and sends the time to the nodes. A GPS
receiver can be jammed or fed false signals. The hub checks for both and stops publishing a
position or time it does not trust ([GPS jamming and spoofing at the hub](GNSS-INTEGRITY.md)).
**Simulated only**: no real GPS module has been wired to the hub, and no real spoofer has been
tried.

Known limits: a spoofer that is present from the very first power-up, or one that moves the
position or time slowly enough, is not caught. Typing the hub's position into the site file at a
fixed site removes most of the first case.

## The hotspot and the phone page

The kit has **no working phone page, hotspot or TAK output today**: the firmware for the board
that serves them does not fit on its chip ([What it can and cannot do
today](start/capabilities.md)). Its protections (a password, limits on what the page may change,
read-only behaviour on someone else's network) are **Built, not yet tried**. When it works, the
hotspot password will be the only thing between a nearby phone and the page.

## Other people's privacy

A kit records where drones were and, from Remote ID, where their pilots stood. That is
information about people.

- The hub uploads nothing by itself. Records leave it on the memory card, or when you run the
  upload tool on a laptop.
- The memory card and the link between boards are not encrypted (above). Guard the card as you
  would the records themselves.
- Control-link identifiers and content are off until you turn them on in the site file, and
  every change is written to the evidence log
  ([Law, safety and ethics](start/law-and-ethics.md)).
- Recording sound is not part of the published firmware. Work on it is on a branch that has not
  been merged; there, saving sound clips is off unless the owner turns it on.

The kit cannot decide for you who may see its records or how long you keep them.

## The software itself

- **Locked dependencies.** Every outside library is pinned to an exact version and checksum, and
  a check fails if a library appears that the owner has not approved by name
  ([Agents, dependencies and review](dev/agents.md)).
- **Automatic checks on every change.** Tests, the dependency check, a licence and
  known-vulnerability check, and a scan for hidden characters run on every push
  ([Build, test and flash](dev/build-test-flash.md), "What CI checks").
- **Images hold no keys**, so a firmware file can be shared or published without giving anything
  away.
- **Builds can be repeated.** A recipe produces byte-identical images from the same source
  ([Releases: build, check, sign](dev/release.md) says which images that has been checked for).

What it does not do:

- **There is no published or signed release yet.** If you did not build the firmware yourself
  from source you have checked out, nothing tells you what is in it.
- **The computer that builds the firmware is trusted**, and so are the Rust compiler and the
  library sources it downloads.
- **The radio chips run Espressif's closed radio libraries.** Rotorscope checks that they are
  the published ones, not what is inside them
  ([Security policy](project/security.md), "Known limits").

## What this means for you

1. **Seal your kit** before you depend on it, and keep the kit key somewhere private.
2. **Keep hold of the boards and the card.** Most of what a kit resists, it resists over the
   air. Very little of it survives someone plugging in.
3. **Check with your own eyes.** A detection is a reason to look, not a finding.
4. **Do not read silence as safety.**
5. **Describe the records honestly** if you pass them on: who held the kit, who held the card,
   and that the log is tamper-evident, not proof of who wrote it.
6. **Report a weakness privately** through the [Security policy](project/security.md).

## Where this lives in the code

- Spoofing checks and flood handling: `crates/tracker/src/` ([Spoofed drones](SPOOFING.md)).
- The link's keys and checks: `crates/kitkeys`, `crates/airlink`, `crates/hublink`
  ([Link keys: the design as built](LINK-KEYS-DESIGN.md), "Limits that remain").
- Crash recovery: each firmware's panic handler and watchdog
  ([Firmware reference](dev/firmware.md)).
- The evidence log and its checker: `crates/evidence`, `tools/evidence-verify`
  ([The evidence log](EVIDENCE.md), "What is NOT protected yet").
- GPS checks: `crates/gnss` ([GPS jamming and spoofing at the hub](GNSS-INTEGRITY.md), "Known
  limits").
- Dependency and CI checks: `tools/check-deps.py`, `.github/workflows/ci.yml`, `deny.toml`.
- Known limits in the reporting policy:
  [SECURITY.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/SECURITY.md).
