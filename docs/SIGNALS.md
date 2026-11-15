# Signals: what drones send, where, and what Rotorscope does about each

One table per family. Every frequency is in MHz. "Node" is the job a C5 does to hear it
(a C5 can do one job at a time). Sources are in [RESEARCH.md](../RESEARCH.md); this page
collects them and adds the state of our code. Written 2026-11-14.

**Status words:**

- **On air**: our receiver decoded it from a real radio signal.
- **Built**: the code exists and passes its tests, but has never heard the real thing.
- **Not built**: no code yet.
- **Can't**: no ESP32 can tune there. It needs other hardware.

C5 reach: raw capture is measured at 2200–2650 and 5180–5800 MHz. A capture is 40 MHz wide, so
signals up to about 5820 MHz can be reached by tuning to the 5800 edge. Normal Wi-Fi mode on
channel 165 (5825) is still to be checked.

## 1. Remote ID (the legally required broadcast)

| Signal | Where | How it's sent | Node | Status | How we test it |
|---|---|---|---|---|---|
| Wi-Fi NAN, 2.4 GHz | ch 6 (2437) only | Wi-Fi action frame to `51:6f:9a:01:00:00` | Wi-Fi 2.4 | **On air** (S3 and C5) | `rid-beacon` |
| Wi-Fi beacon, 2.4 GHz | any channel, mostly 6; also 1 (2412), 11 (2462) | Vendor element, OUI `FA:0B:BC` type `0x0D` | Wi-Fi 2.4 | **On air**, and through ESP-NOW to the P4 (2026-11-14) | `rid-beacon` |
| Wi-Fi beacon and NAN, 5 GHz | anchor ch 149 (5745); 153, 157, 161, 165; 36–48 (5180–5240) | Same as 2.4 | Wi-Fi 5 | **On air** at 149 | `rid-beacon` on a C5 |
| Bluetooth 4 legacy | advertising ch 37/38/39 (2402, 2426, 2480) | Service data UUID `0xFFFA`, app code `0x0D` | Bluetooth | **On air** | `rid-beacon`, laptop `rid-ble-beacon` |
| Bluetooth 5 extended (1M PHY) | primary 37–39, data on 0–36 (2404–2478) | Same, extended adverts, message packs | Bluetooth | **On air** | `rid-beacon` |
| Bluetooth 5 Long Range (coded PHY) | same channels | Same, coded PHY | Bluetooth | **On air** | `rid-beacon` |
| China GB 46750-2025 | inside the Wi-Fi element above | Different record layout | Wi-Fi 2.4 / 5 | **Built** (decoder) | needs a test-beacon mode, or a DJI drone on 2026 firmware |

## 2. DJI

| Signal | Where | What it is | Node | Status | How we test it |
|---|---|---|---|---|---|
| DJI legacy Wi-Fi DroneID (Spark, Mavic Air, Mini SE and other Wi-Fi DJIs) | 2.4 GHz Wi-Fi beacons, any channel | Vendor element, OUI `26:37:12`: serial, drone and pilot position | Wi-Fi 2.4 | **Built**: decoder in `rid::dji`; Wi-Fi nodes forward the element raw as `Frame::DjiWifi` (rate-limited); hub makes a drone track merged with Remote ID by serial, in every output (2026-11-14). Never heard from a real drone | Recorded captures; a real Wi-Fi DJI. We never transmit it |
| OcuSync DroneID burst (OcuSync 2, 3, 3+) | 2399.5, 2414.5, 2429.5, 2444.5, 2459.5 / 5756.5, 5776.5, 5796.5 | 9 MHz LTE-style burst, about 0.6 ms, every ~640 ms, only while the motors run. Plaintext: serial, drone, pilot and home position | DroneID (raw capture) | **Built, proven on the chip with recordings**: the public recordings, fed through the C5's own capture path over USB, detect and decode 78/78 at 7-9 dB SNR, ~151 ms per decode (2026-11-14). Node sends `Frame::DroneId` over ESP-NOW; hub shows drone, pilot and home, merged with Remote ID by serial. 0 false detections in 200 s on air. Never a real drone | `tools/droneid-hil` (recordings into the chip, no radio); a real DJI with props off, or a field visit |
| O4 / O4+ DroneID (Mini 4 Pro, Air 3, Avata 2, Neo, Flip, Mavic 4) | hops 2414.5, 2429.5, 2444.5, 5756.5, 5776.5, 5796.5, 5816.5 | Same burst, encrypted | DroneID | **Built**: detect only, with a session ID. Never decrypted, by design. Hub: presence-only track per session key ID | A real O4 drone |
| OcuSync / O4 video downlink | 2400–2483; 5725–5850; O4 also 5170–5250 | OFDM, 10/20/40 MHz (O4 up to 60) | Fingerprint (raw capture) | **Not built** (PLAN 11b) | Public RFUAV recordings, then a real drone |
| DJI controller uplink | same bands | narrow hops, 0.4–5 MHz wide, 0.5–5 ms bursts | Fingerprint | **Not built** | as above |
| DJI FPV, O3, O4 air units | 5660, 5695, 5735, 5770, 5805, 5839, 5878, 5914 (FCC); O4 also 5170–5250 | LTE-style OFDM video | Fingerprint; RX5808 above 5820 | **Not built** | needs an air unit |

## 3. Analog FPV video (5.8 GHz FM video)

| Band | Channels 1–8 | Status |
|---|---|---|
| A (Boscam A) | 5865, 5845, 5825, 5805, 5785, 5765, 5745, 5725 | **Built** (detection only, `crates/fpvscan`): channels up to 5810 MHz; never heard a real transmitter |
| B (Boscam B) | 5733, 5752, 5771, 5790, 5809, 5828, 5847, 5866 | **Built** (detection only, `crates/fpvscan`): channels up to 5810 MHz; never heard a real transmitter |
| E (Boscam E) | 5705, 5685, 5665, 5645, 5885, 5905, 5925, 5945 | **Built** (detection only, `crates/fpvscan`): channels up to 5810 MHz; never heard a real transmitter |
| F (Fatshark / IRC) | 5740, 5760, 5780, 5800, 5820, 5840, 5860, 5880 | **Built** (detection only, `crates/fpvscan`): channels up to 5810 MHz; never heard a real transmitter |
| R (Raceband) | 5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917 | **Built** (detection only, `crates/fpvscan`): channels up to 5810 MHz; never heard a real transmitter |
| L (Low band) | 5362, 5399, 5436, 5473, 5510, 5547, 5584, 5621 | **Built** (detection only, `crates/fpvscan`): channels up to 5810 MHz; never heard a real transmitter |

Hub side (2026-11-14): `Frame::Rf` sightings fill an RF-emitter table (kind and frequency, level,
duty, line rate) shown on the phone page, screen SIGNALS page, CSV and rid-view; analog video,
HDZero, LTE-style and narrow Wi-Fi alert, plain energy never does. The node scanner that sends them
is still **Not built**.

- **How:** a C5 in raw-capture mode steps across the channels and measures the energy on each. It then
  confirms real video by the 15.6/15.7 kHz line rate in the FM-demodulated signal, so Wi-Fi isn't
  mistaken for a video transmitter.
- **Reach:** a C5 reaches 31 of the 47 distinct channels (carriers 5181–5810 MHz; list in firmware/node-sdr/README.md). Anything above about 5820 MHz needs an RX5808
  module (about $6). That covers the top of bands A, B, E, F and R.
- **Also exist:** analog video at 1.2–1.3 GHz (**Can't**), and 2.4 GHz analog video (a C5 can reach
  it; not built).
- **Testing:** in the US, a 25 mW video transmitter needs an amateur radio licence. We can test
  with one if you have that licence, at an FPV meet-up, or from recordings.

## 4. Digital FPV

| System | Where | What we can get | Node | Status | How we test it |
|---|---|---|---|---|---|
| wfb-ng / OpenIPC | 5 GHz Wi-Fi, default ch 165 (5825), often 161 | System, link ID (opt-in) | Wi-Fi 5 / 2.4 | **Built**; fake frames heard on air ch 6/149/161/165 (2026-11-14); real LDPC/STBC video untested | `tools/bench/run.py --group openlink`; OpenIPC hardware for LDPC/STBC |
| OpenHD | 5745 (ch 149) default | System, air or ground side | Wi-Fi 5 | **Built**; fake frames heard on air ch 6/149/161/165 (2026-11-14) | `--group openlink`; real hardware |
| RubyFPV | 5 GHz Wi-Fi | System, vehicle ID (opt-in) | Wi-Fi 5 | **Built**; fake frames heard on air ch 6/149/161/165 (2026-11-14) | `--group openlink`; real hardware |
| DroneBridge | 2.4 / 5 GHz Wi-Fi | System | Wi-Fi | **Built**; fake frames heard on air ch 6/149/161/165 (2026-11-14) | `--group openlink`; real hardware |
| HDZero | R1–R8 (5658–5917, 37 MHz apart), E1 5705, F1 5740, F2 5760, F4 5800, low band 5362–5621 | "HDZero on channel X" by the grid and 27/17 MHz width | Fingerprint; RX5808 above 5820 | **Not built** | HDZero VTX ($40–70); it transmits without pairing |
| Walksnail Avatar | same 8-channel plan as DJI (inferred), 20/40 MHz | "LTE-style video link" | Fingerprint | **Not built** | needs a kit |

## 5. Other drone links

| System | Where | What we can get | Status |
|---|---|---|---|
| Autel SkyLink | 2407–2472, 5154–5246, 5728–5847 (+ 900 MHz) | LTE-style OFDM family (1.4/5/10/20 MHz) | **Not built** (fingerprint). 900 MHz **Can't** |
| Skydio 2/2+/X10 | 2.4 / 5 GHz Wi-Fi | Remote ID beacon (already decoded); Wi-Fi headers | RID **On air**-capable; narrow modes **Not built** |
| Skydio X10D | 1625–2500 | — | mostly **Can't** |
| Wi-Fi toy and hobby drones (Tello, Parrot, Holy Stone, ...) | 2.4 / 5 GHz Wi-Fi | Maker by network name and address prefix | **Built, hub side** (2026-11-14): maker table `crates/wifidrone` (13 makers; prefixes checked against the IEEE registry), `Frame::WifiDrone` table and outputs. **Node not yet** (it must call `wifidrone::classify`) |

## 6. Control links (the pilot's stick signals)

| Link | Where | What we can get | Status |
|---|---|---|---|
| ExpressLRS 2.4 GHz | 2400–2480, 80 channels; sync near 2440.4 | Presence, rate; ID opt-in | **Not built**, needs an LR1121 or SX1280 radio (not owned) |
| ExpressLRS 900 MHz | 903.5–926.9, 40 channels; sync near 915.5 | same | **Can't** on ESP32; LR1121 |
| TBS Crossfire / Tracer, ImmersionRC Ghost, FrSky, Spektrum, FlySky | 868/915 or 2.4 GHz | energy only | **Not built**; 2.4 GHz energy maybe on a raw-capture C5 |
| SiK / RFD900 telemetry | 902–928 | hopping pattern | **Can't** on ESP32 |

Legal line: presence and energy are the default. Link identifiers are opt-in. Content is never
decoded (2020 DOJ/FAA/FCC/DHS advisory).

## 7. Crewed aircraft

| Signal | Where | Status | How we test it |
|---|---|---|---|
| ADS-B | 1090 | **On air** on the laptop (2026-11-14: 77 lines, 0 bad, 6 aircraft); not yet wired to the P4 hub | real aircraft overhead |
| UAT | 978 | same | same (rare in the air near Philly) |
