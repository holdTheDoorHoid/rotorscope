# Hearing drones: the acoustic detector

PLAN.md Phase 15. Status 2026-11-18:

| Piece | State |
|---|---|
| Detector library, file tool | Built; tested on recordings (below) |
| Hub tracker: sound contacts, alert, radio and aircraft cross-check | Built; laptop tests |
| Hub firmware: microphone, detector, alert, clips to SD, evidence entries | **Running on the P4**; proven with a made-up rotor sound mixed in inside the chip |
| A real sound through the real microphone | **Loudspeaker test passed 2026-11-18** (three drone recordings alerted, three non-drone ones did not; below). No real drone outdoors yet |
| Outdoor microphone, windscreen, range | Not started |
| Bearing pod, fly-past measurement, aircraft type table | Not started (PLAN 15e, 15f, 15g) |

## What it does

`crates/acoustic` takes 16 kHz sound from one microphone and says **heard** when a rotor-like
sound has been present for most of three seconds. It reports the pitch, a rough size class
(large or small propellers), and a confidence. How it decides is in the crate's own
documentation (`crates/acoustic/src/lib.rs`).

It needs no training data and no model file. The numbers that tune it are in `acoustic::Config`.

## What the hub does with it

`tracker::sound` groups the detector's once-a-second reports into **sound contacts** and labels
each one:

| Label | When | Alerts |
|---|---|---|
| Drone audible (close) | a drone, FPV link, DJI link or Wi-Fi drone was on the radio in the last 15 s | yes |
| Aircraft heard | no drone on the radio, but a tracked aircraft is airborne within 3 km | no |
| Possible drone heard, no radio | neither | yes, after 5 s |

Contacts appear as `"t":"sound"` JSON lines and as `S<n>` rows in the SD log. The aircraft
check needs the hub's own position and uses distance only; matching the pitch curve to the
aircraft's track and looking up its type are PLAN 15f and 15g.

## On the hub (P4)

- **Microphone.** The board's own PDM microphone (clock GPIO24, data GPIO25), 16 kHz, through
  a driver of our own (`firmware/hub-p4/src/mic.rs`): esp-hal has no audio input for this chip.
- **Detector.** `ear.rs` runs it on every 32 ms of sound: about 4.4 ms of work, 14% of one
  core. The hub's loop still runs about 450 times a second.
- **Tape.** The last two minutes of sound go round in PSRAM. Nothing reads them unless a drone
  is heard, and they are gone at power-off.
- **Alert.** A contact that has lasted five seconds and is not a tracked aircraft prints
  `hub-p4: SOUND ...` and writes a `sound` entry to the evidence log.
- **Clip.** While a drone is heard the hub saves `ROTORSCP/AUDIO/Cnnnnnnn.WAV`: from five
  seconds before the detection to two seconds after it ends, sixty seconds at most, one clip
  per contact. The evidence log gets a `sound_clip` entry with the clip's SHA-256. The clip is
  written a little slower than real time so the hub keeps working.
- **Turning clips off.** `audio clips off` in `ROTORSCP/SITE.TXT` or typed at the console
  (`L`). Detection and alerts continue; no sound is stored.

### Proof on the board (2026-11-18)

Built with `--features earfake`, the firmware mixes a made-up quadcopter sound (four rotors
near 210 Hz) into the microphone signal inside the chip, 20–45 s and 70–80 s after start-up,
and reads every saved clip back from the card.

| Check | Result |
|---|---|
| Microphone capture | 31.25 buffers a second, 0 DMA errors over several minutes |
| Sound lost | one buffer while the SD card mounts at boot; none after, including while a clip is written |
| Detection | Candidate, then Heard, at 206–215 Hz ("Large propellers"), confidence 100 |
| Alert | `SOUND Possible drone heard, no radio` 5 s after the contact began, once per contact |
| Clips | two saved (31.2 s and 9.0 s); read back from the card: length, header and SHA-256 match; 0 samples lost |
| Rest of the hub meanwhile | gateway link 0 corrupt, 0 overflow, 2 nodes throughout; loop 220–255 passes a second while a clip is written |

The bench read-back blocks the loop for several seconds and loses sound while it runs; it is
not in the normal firmware. `--features eardump` adds console `w`, which prints the last six
seconds of the tape as hex for checking on a laptop; it must never be in a deployed unit.

## Trying it on recordings

```
tools/fetch-acoustic-samples.sh                 # about 1.8 GB into testdata/acoustic (never committed)
cargo run --release -p acoustic-file -- --timeline FILE.wav
cargo run --release -p acoustic-file -- --tones FILE.wav      # what steady tones are in it
cargo run --release -p acoustic-file -- --join FOLDER         # short clips played end to end
cargo run --release -p acoustic-file -- --clips FOLDER        # short clips, frame-level only
```

## Results on recordings (2026-11-17)

These are other people's recordings, made with other microphones. They show the method works
and where it stops. They are not our range.

### Distance: Bat Ear field set (AudioMoth recorder, drone GPS distance per file)

One-minute files; the first five seconds of each are warm-up. "Heard" is the share of the file
in the heard state. The drone moves during a file, so a file is listed by its closest and
median distance.

| Drone distance (closest in file) | Files | Files with "heard" | Typical share heard |
|---|---|---|---|
| 4–26 m, flying | 12 | 12 | 6–51% |
| 42–70 m | 5 | 1 (3% of the file) | 0% |
| 91–136 m | 6 | 0 | 0% |
| 353–776 m | 19 | 0 | 0% |

- Two more files list the drone at 6 m and 12 m and were not heard. The sound level in both
  drops to background within seconds: the drone was on the ground with its motors off.
- Pitch found: Mavic Pro 167–189 Hz, Mini 4 Pro 188–232 Hz. Both are in the camera-drone range.
- So on this recorder the working range for these two small, quiet drones is **about 40 m**.
  Published work reaches 50–100 m with trained classifiers; see "What would extend the range".

### False alarms

| Recording | Length | Share "heard" |
|---|---|---|
| Rural background (Bat Ear, 12 files) | 12 min | 0% |
| Fireworks (Bat Ear, 11 files) | 11 min | 0% |
| Wind only (VolAnti, 2 files) | 1 min | 0% |
| Same session as the drone flights, labelled "no drone in photos" (10 files) | 10 min | 3 files, 4–12% each, at 149–209 Hz |
| Al-Emadi "unknown" set, every clip end to end | 2 h 59 min | 5.9% |

- The three hits in the "no drone" files are at the drone's own pitch, minutes before or after
  logged flights. They are probably the drone flying without taking photos (the distance log
  comes from photo timestamps). Unconfirmed.
- The Al-Emadi "unknown" set is built to be hard: it contains the ESC-50 sound library. By
  sound type, the share of time falsely heard:

| Fooled it | Share | Did not fool it (0%) |
|---|---|---|
| Siren | 38% | Rain, sea, thunder, crackling fire |
| Vacuum cleaner | 24% | Speech sounds: laughing, coughing, sneezing, snoring |
| Washing machine | 22% | Birds, crickets, crow, sheep, pig |
| Car horn | 21% | Footsteps, door knock, clapping, typing |
| Helicopter | 20% | Train, fireworks, glass breaking |
| Chainsaw | 18% | (27 of the 50 sound types in all) |
| Church bells, insects | 17% each | |
| Alarm clock | 16% | |
| Aeroplane | 11% | |
| Engine | 10% | |

  Wind, dog, cat, cow, hen, frog, crying baby, hand saw and a few others: 1–7%.
- This is the list every published acoustic detector reports. It is why the hub treats sound
  alone as a weaker claim than a radio detection, and why aircraft are cross-checked against
  ADS-B (PLAN 15g).

### Drone recordings without distances

| Recording | Result |
|---|---|
| Al-Emadi drone clips, end to end (22 min, indoor, Bebop and Mambo) | heard 58% of the time, median pitch 476 Hz |
| Al-Emadi drone clips one at a time (1 s each; frame-level ladder check only) | 77% rotor-like |
| Bat Ear 5-inch FPV (11 short files) | heard in 3; most files are a few seconds long, shorter than warm-up plus decision |
| VolAnti rotor rig (3 files of 8–16 s) | heard in 1; the other two end before a decision is possible |

## Known limits

- **Not yet heard a real drone on the hub.** It hears recordings of drones played through a
  loudspeaker (below); a drone outdoors, at measured distances, is the next test.
- **Five-second warm-up** after power-up, and **about three seconds** to decide. A drone that
  passes in under three seconds is not reported.
- **A hum that never changes pitch** is ignored after ten seconds (mains hum, a fan). A machine
  whose pitch drifts (an engine under load) is not.
- **One microphone gives no direction and no distance.** PLAN 15e and 15f address these.
- Wind noise is untested on our hardware.

## What would extend the range

1. A second, slower pass over two seconds of sound for faint steady tones. The quarter-second
   averaging added on 2026-11-17 took the Bat Ear result from "heard at 25 m in half the files"
   to "heard in all of them".
2. A better microphone and a windscreen. Most published range differences come from here.
3. A small trained classifier as a second opinion. Needs our own outdoor recordings.

## Through the real microphone: loudspeaker test (2026-11-18)

Recordings played from a small USB loudspeaker on the bench into the P4 board's built-in
microphone. Firmware on the board: branch `sec/wp07-audio` (this detector, clips off). The room
read about -70 dBFS when silent.

| Played | Level at the microphone | Hub |
|---|---|---|
| 1 kHz test tone | -43 dBFS | pitch 1000 Hz; alert after 5 s; turned to "steady hum" after 10 s, as designed |
| Mavic Pro, field recording (4-22 m), as recorded | -67 median, -58 peak | **alert**, 190 Hz, large propellers (file scores 188 Hz on the computer); heard in 22 of 37 s reported |
| Mini 4 Pro, field recording (23-27 m), as recorded | -68 median | missed: barely above the room |
| Mini 4 Pro, same file 9x louder | -55 median | **alert**, 210 Hz, large propellers; heard 27 of 38 s |
| 5-inch FPV, as recorded | -69 median | missed: barely above the room |
| 5-inch FPV, same file 10x louder | -63 median, -44 peak | **alert**, 720 Hz, small propellers |
| VolAnti two propellers (8 s) | -52 | heard, 543 Hz; too short for the 5 s alert |
| Rural ambience, as recorded and 28x louder | -70 / about -53 | no alert (a few one-second candidates, none held) |
| Fireworks | -56 median, -45 peak | no alert (candidates, none held) |
| Wind | -70 | no alert |

Capture stayed clean throughout: 0 DMA errors, 1 overrun (the known one at start-up),
detector 4.4-4.6 ms per frame.

What this does and does not show: the microphone, the capture and the detector work end to end
on real sound, at the pitches the same files give on the computer. It is not a range
measurement: a small loudspeaker has little output below about 200 Hz and the playback level
was arbitrary. One Mavic file raised a second contact at 343 Hz late in the clip, not yet
explained. Clips were not exercised: they are off by default and the card was waiting for a
clock.

## Recordings and their terms

| Set | Terms | Use here |
|---|---|---|
| batear-io/batear-datasets | MIT | tuning and tests |
| agamrossen/VolAnti `test/audio` | repository is CERN-OHL-W / Apache-2.0 / CC BY-SA; per-file terms not stated | tests |
| saraalemadi/DroneAudioDataset | non-commercial academic; excludes security use. Owner says we may use it (2026-11-17) | measuring the detector only |

Nothing fetched is committed, and nothing in the repository is derived from a recording: the
detector has no trained parts.
