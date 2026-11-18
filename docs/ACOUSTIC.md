# Hearing drones: the acoustic detector

PLAN.md Phase 15. Status 2026-11-17: the detector library, the file tool and the hub tracker's
sound-contact table are built and tested on a laptop. Nothing has run on the hub yet (the
microphone driver is not written), and nothing has been tested with our own microphone or a
real drone.

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
| Al-Emadi "unknown" set, every clip end to end | 2 h 59 min | 6.1% |

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
| Al-Emadi drone clips, end to end (22 min, indoor, Bebop and Mambo) | heard 60% of the time, median pitch 465 Hz |
| Al-Emadi drone clips one at a time (1 s each; frame-level ladder check only) | 77% rotor-like |
| Bat Ear 5-inch FPV (11 short files) | heard in 2; most files are a few seconds long, shorter than warm-up plus decision |
| VolAnti rotor rig (3 files of 8–16 s) | heard in 1; the other two end before a decision is possible |

## Known limits

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

## Recordings and their terms

| Set | Terms | Use here |
|---|---|---|
| batear-io/batear-datasets | MIT | tuning and tests |
| agamrossen/VolAnti `test/audio` | repository is CERN-OHL-W / Apache-2.0 / CC BY-SA; per-file terms not stated | tests |
| saraalemadi/DroneAudioDataset | non-commercial academic; excludes security use. Owner says we may use it (2026-11-17) | measuring the detector only |

Nothing fetched is committed, and nothing in the repository is derived from a recording: the
detector has no trained parts.
