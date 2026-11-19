# How drones give themselves away

**Level: 🟢 Beginner** · The radio signals a drone sends, which of them are meant to be
public, what a listener can learn from each, and which ones Rotorscope hears today.

## A drone is a flying radio

A drone is a small aircraft with several radios on board. The pilot's controller talks to it,
it sends live video back down, and most modern drones also announce themselves on purpose.
Every one of those radios is a clue. Rotorscope is a set of receivers that listen for those
clues; it never sends anything back.

The project plan sorts the clues into three kinds
([PLAN.md](https://github.com/holdTheDoorHoid/rotorscope/blob/main/PLAN.md), "What
cooperative and semi-cooperative mean here"):

- **Cooperative.** The drone deliberately announces itself. Remote ID is the main example,
  and ADS-B for crewed aircraft.
- **Semi-cooperative.** The drone is not trying to be identified, but its radios give it
  away: DJI's own identification bursts, the Wi-Fi network of a toy drone, a video
  transmitter.
- **Silent.** A drone with every radio off sends nothing to hear. No radio receiver,
  Rotorscope included, can find it.

## The four kinds of signal

### 1. Remote ID: the digital licence plate

**Remote ID** is a short message the drone broadcasts about once a second over ordinary
Wi-Fi and Bluetooth. In the US the FAA requires it on most drones. It carries the drone's
serial number, its position, height and speed, and the position of the pilot or of the
take-off point. It is **public by design**: it exists so that people on the ground can find
out who is flying. A listener learns identity and position of both drone and pilot.

Remote ID comes in several radio flavours (Wi-Fi beacon and Wi-Fi NAN on 2.4 GHz and 5 GHz,
Bluetooth 4, Bluetooth 5 and Bluetooth 5 Long Range), which is why Rotorscope needs several listeners to
catch them all. Deep dive: [Remote ID](../detect/remote-id.md).

### 2. The maker's own ID: DJI DroneID

DJI, the largest drone maker, has its own identification inside its radio link, called
**DroneID**. It was made for DJI's own receivers (sold as AeroScope), but older versions send
it in plain form, so any receiver that understands the format can read the serial number and
the positions of the drone, the pilot and the take-off point. Newer DJI drones (the "O4"
generation) **encrypt** it: a listener without the key learns only that a DJI O4 drone is
there, plus a temporary session number. Some older Wi-Fi DJI models send a similar record in
a Wi-Fi beacon. Deep dive: [DJI DroneID](../detect/dji-droneid.md).

### 3. The video link coming down

Most drones send live camera video to the pilot. Hobby racing drones often use **analog
video** on 5.8 GHz, a simple signal anyone can tune to; others use digital systems. A video
signal is not meant as an announcement. Rotorscope can tell that **a video transmitter is
on the air and on which channel**, and for some open digital systems which system it is. It
does not show the video and cannot say where the drone is from video alone. Deep dive:
[FPV video links](../detect/fpv.md).

### 4. The control link going up

The pilot's controller sends stick commands to the drone, and often gets battery and
position readings back. These links are private communications. A listener can tell that a
control link is active and which system it is; reading what is inside raises legal questions,
so Rotorscope reports presence only unless the owner opts in
([Law, safety and ethics](law-and-ethics.md)). Deep dive: [Control links](../CONTROL-LINKS.md).

## What each signal tells you, and where Rotorscope stands

The last column uses the project's proof words: **Heard for real** (from a real device
someone else made), **Proven on the air** (from the project's own test transmitter),
**Proven on recordings** (public recordings of the real signal), **Simulated only** (made-up
data on a computer or chip), **Built, not yet tried**, and **Planned**.

| Signal | Who sends it | What you learn | Rotorscope today |
|---|---|---|---|
| Remote ID over Wi-Fi and Bluetooth | Most drones sold for US use | Identity, drone position and height, pilot or take-off position | **Proven on the air** |
| DJI DroneID, older OcuSync drones | DJI drones on OcuSync 2, 3 and 3+ | Serial, drone, pilot and home positions | **Proven on recordings** |
| DJI DroneID, O4 drones (encrypted) | Newer DJI drones | "A DJI O4 drone is here", with a session number | **Proven on recordings** (finding the records); following a session **Simulated only** |
| DJI Wi-Fi DroneID | Older Wi-Fi DJI models | Serial, drone and pilot positions | **Simulated only** |
| Toy and hobby drone Wi-Fi networks | Tello, Parrot and similar | The maker, from the network's name and address | **Simulated only** |
| Analog FPV video | Racing and hobby drones | A video transmitter is on, and its channel | **Simulated only** |
| Open digital FPV video (wfb-ng, OpenHD, RubyFPV, DroneBridge) | Home-built drones | Which system; link identifiers only if opted in | **Proven on the air** (the test transmitter's fake frames) |
| DJI video and controller links | DJI drones and FPV air units | "DJI video" or "DJI controller" is on the air | **Proven on recordings** (on a computer, not yet on the chip) |
| HDZero, Walksnail and other digital video | Many drones | Which family of system | **Planned** as named systems; an HDZero check exists, **Simulated only** |
| Control links (ExpressLRS and others) | Hobby and FPV drones, model aircraft | A link is active, which system; more only if opted in | **Simulated only** (the recognisers); the LR2021 radio board that would hear them is **Built, not yet tried** |
| ADS-B on 1090 MHz | Crewed aircraft | Identity, position, height | **Heard for real** (through an ADSBee on a laptop; not yet wired to the hub) |
| UAT and FLARM | Small crewed aircraft, gliders | Identity, position | **Simulated only** (the decoders; the radio board they need is not in hand) |

As of 2026-11-18 Rotorscope has **not yet heard a real drone**. Its Remote ID receivers
have been proven against the project's own test transmitter, and its DJI decoder against
public recordings. The full, signal-by-signal list with frequencies is the
[signal catalogue](../SIGNALS.md).

## Who sends nothing

Some drones will never show up:

- a drone with Remote ID switched off or never fitted, and no other radio Rotorscope hears;
- a drone that is radio-silent, with nothing switched on that transmits;
- a drone whose radios Rotorscope cannot recognise or tune to (for example Walksnail cannot be
  told apart yet, HDZero only in simulation; some links use bands no ESP32 chip reaches).

Rotorscope cannot locate a drone that does not tell it where it is. For video and control
links it can say "something is here", not where.

## Frequency bands, by analogy

Think of radio bands as **different stations on a dial**. A receiver tuned to one station
hears nothing on the others, which is one reason Rotorscope uses several receivers.

- **2.4 GHz.** The crowded high street: home Wi-Fi, Bluetooth, microwave ovens, and most drone
  Remote ID, DJI links and many control links all live here. Rotorscope's busiest band.
- **5 GHz.** The second Wi-Fi band, with 5.8 GHz at its top end. Some Remote ID (channel 149 is
  the usual one), DJI's links, and most FPV video sit here.
- **900 MHz.** A lower band (902 to 928 MHz in the US) used by long-range control links,
  telemetry radios and, in the US, FLARM. Also full of smart utility meters. No ESP32 can tune
  there, so it needs the separate LR2021 radio.
- **1090 MHz.** The crewed-aircraft band, where airliners and many other crewed aircraft
  broadcast ADS-B. Rotorscope hears it through an add-on receiver, the ADSBee 1090.

## Why crewed aircraft are shown too

A drone is most dangerous near a crewed aircraft: a medical or police helicopter coming in
low over a park is the case the project names. So Rotorscope also listens for **ADS-B**, the
crewed-aircraft equivalent of Remote ID, and shows aircraft in their own table, separate from
drones, never as an alarm on their own. The hub then compares every drone with every
aircraft and warns when they are close; that warning is **Simulated only** so far
([Drone near a crewed aircraft](../AIRCRAFT-CONFLICT.md)). More:
[Crewed-aircraft signals](../detect/aircraft-signals.md).

**Next:** [What it can and cannot do today](capabilities.md)
