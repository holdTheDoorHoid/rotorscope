# Hearing drones: the acoustic detector

PLAN.md Phase 15. Status 2026-11-18:

| Piece | State |
|---|---|
| Detector library, file tool | Built; tested on recordings (below) |
| Hub tracker: sound contacts, alert, radio and aircraft cross-check | Built; laptop tests |
| Hub firmware: microphone, detector, alert, clips to SD, evidence entries | **Running on the P4**; proven with a made-up rotor sound mixed in inside the chip |
| A real sound through the real microphone | **Loudspeaker test passed 2026-11-18** (three drone recordings alerted, three non-drone ones did not; below). No real drone outdoors yet |
| Clips off by default, clip only on alert, caps, screen marker (WP-07) | Built 2026-11-18, **compile-checked only**; laptop tests; bench checks below not yet run |
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
| Low hum heard (fan, engine or aircraft) | not explained by radio or a tracked aircraft, and heard mostly below 140 Hz (a site setting) | no, unless the site says `low hum alerts on` |

### Low hums (2026-11-18)

A contact heard mostly below **140 Hz** is kept, shown and logged like any other, but does not
alert and gets no clip. Both halves are site settings (docs/SITE.md "Low hums"): `low hum
alerts on|off` and `low hum below <Hz>` (80 to 400). Off and 140 are the recommended setting
and what a hub with no such line uses (`tracker::sound::LowHum::RECOMMENDED`). Why 140:

- The first day indoors (hub on a bench, a fan running, about four hours of console logs) gave
  eight false "possible drone" alerts. All eight sat at 102-120 Hz, quiet (-57 to -72 dBFS, at
  the room's own level), drifting slowly, for up to a minute. Ten more stretches were heard for
  under five seconds and never alerted.
- Every drone recording we have is higher: Mavic Pro 158-199 Hz, Mini 4 Pro 190-232 Hz, Bebop
  300-500 Hz, Mambo about 600 Hz, 5-inch FPV 650-750 Hz.
- Replayed through the same contact rules: the eight alerts become none; on the Al-Emadi
  "unknown" set (three hours of everyday noise) 37 alerts become 25; drone alerts are unchanged
  (Bebop 16, Mambo 14, Bat Ear field files 13).

On the board (2026-11-18, loudspeaker into the built-in microphone): a made-up 110 Hz rotor
hum, heard for 23 s at 107-113 Hz, became a "Low hum" contact with no alert; the Mavic Pro
field recording straight after raised the alert at 189 Hz. `low hum below 200` and back to 140
typed at the console took effect and were logged.

"Mostly" is counted over the contact's once-a-second reports, so one second at the wrong octave
does not make a hum alert, and a hum whose pitch rises into the drone range and stays there
does. A drone on the radio still makes the contact "Drone audible (close)".

The price: a heavy-lift drone with propellers of about 20 inches or more (crop sprayers, large
cinema and cargo hexacopters) can sit below 140 Hz and would be logged as a low hum, not
alerted, unless it is also on the radio; a site that expects those lowers the line or turns
low-hum alerts on. What the eight false alerts actually were is not
settled: the laptop's own fan was ruled out (it tops out at 85 Hz and the hub does not hear
it), and no sound was kept at the time, clips being off.

Contacts appear as `"t":"sound"` JSON lines and as `S<n>` rows in the SD log. The aircraft
check needs the hub's own position and uses distance only; matching the pitch curve to the
aircraft's track and looking up its type are PLAN 15f and 15g.

Both checks count only what has passed probation (docs/SPOOFING.md "Floods"): a drone track
once **confirmed**, an aircraft once **established**. One forged ADS-B frame placing an
"aircraft" overhead must not explain a drone's sound away (and so suppress its alert and its
clip), and one forged Remote ID frame does not make a sound "with radio". (FPV, DJI-link and
Wi-Fi-drone detections have no probation and count as before.)

**Without trusted time** (no GPS receiver, or GPS time not yet trusted): nothing here needs the
wall clock. Contacts, the alert's five seconds, the clip's rules and the six-an-hour limit all
run on the hub's uptime (the hourly window restarts with the hub). Clip files are numbered, not
dated (`Cnnnnnnn.WAV`, continuing from the highest on the card); their FAT date is 2026-01-01
until there is trusted time. The `sound` and `sound_clip` evidence entries carry the record
header's time source like every other record (UTC 0 and the uptime, or `untrusted`), and the
clip is tied to its entry by number and SHA-256, not by time.

## On the hub (P4)

- **Microphone.** The board's own PDM microphone (clock GPIO24, data GPIO25), 16 kHz, through
  a driver of our own (`firmware/hub-p4/src/mic.rs`): esp-hal has no audio input for this chip.
- **Detector.** `ear.rs` runs it on every 32 ms of sound: about 4.4 ms of work, 14% of one
  core. The hub's loop still runs about 450 times a second.
- **Alert.** A contact that has lasted five seconds and is not a tracked aircraft prints
  `hub-p4: SOUND ...` and writes a `sound` entry to the evidence log. This works the same
  whether audio clips are on or off.
- **Audio clips are off unless the owner turns them on** for a site (owner decision
  2026-11-18): `audio clips on` in `ROTORSCP/SITE.TXT`, or typed at the console (`L`, then the
  line). `audio clips off` turns them off again. Every change is in the evidence log (below).
- **Tape (clips on only).** While clips are on, the last two minutes of sound go round in PSRAM
  so a clip can include the seconds before the alert. With clips off nothing is written to it;
  switching clips off wipes it, and it is wiped at every start-up.
- **Clip (clips on only).** A clip starts **at the moment a contact alerts**, and only for the
  kinds that alert ("Drone audible (close)", "Possible drone heard, no radio"), never for
  "Aircraft heard" and never for a contact shorter than the five seconds that raise the alert.
  It reaches back to five seconds before the contact began (the onset is kept), at most twelve
  seconds before the alert. It ends two seconds after the sound stops, or as soon as the contact
  turns out to be a tracked aircraft, and is sixty seconds long at most. One clip at a time; a
  second contact alerting meanwhile gets none. The rules are `tracker::sound::clip`, tested on a
  laptop. The file is `ROTORSCP/AUDIO/Cnnnnnnn.WAV`; the evidence log gets a `sound_clip` entry
  with the clip's SHA-256. The clip is written a little slower than real time so the hub keeps
  working.
- **Limits.** Six clips in any hour; 256 MiB of audio on the card in all; and a clip is never
  started unless the whole largest clip (1.9 MB) fits while leaving 1 GiB free for the evidence
  and CSV logs. A clip that would cross a limit while it is written is ended there. Free space
  comes from the card's FAT32 free count; a card without one (FAT16, or a count marked
  unknown) saves no clips, so format the card as FAT32 on a computer. Every refusal is counted:
  the `hub-p4: ear ...` status line (`not_saved N (no_card .. hour .. audio_cap .. space ..
  space_unknown .. cut ..)`) and the screen.
- **On the screen.** While clips are on, a line at the bottom of every page reads
  `AUDIO CLIPS ON: sound may be recorded` (`...; N not saved` once any were refused). While a
  clip is being recorded or written it reads `RECORDING SOUND to SD card`. With clips off there
  is no line.
- **Evidence log.** The boot event says `audio clips off at boot`. Turning clips on or off from
  the site file is a `setting` event `site (file): audio clips on|off`; from the console it is
  the `setting` event `site (console): audio clips on|off` written when the line is typed.
- **Pennsylvania** requires everyone's consent to record a conversation. Where clips are on,
  post a notice; the screen line is not a substitute for one.

### What sound the hub keeps

| State | Sound in memory | Sound on the card |
|---|---|---|
| Clips off (default) | The microphone's DMA ring in internal RAM: 8 buffers of 32 ms, stereo (256 ms), overwritten as it goes round. The detector's analysis window in PSRAM: the last 128 ms (2048 samples, and a windowed copy of them for the transform). One 32 ms buffer on the stack while it is processed. Nothing else: the tape is not written. | None |
| Clips on, no clip | The above, plus the tape: the last 131 s (2^21 samples, 4 MB) in PSRAM | None |
| Clips on, clip being made | The above | The clip being written |
| Clips switched off | The tape is overwritten with zeros at once; any clip being written is ended where it is (`sound_clip` text says `cut short: audio clips switched off`) | Clips already saved stay |

Everything in RAM is lost at power-off. PSRAM can keep its contents through a software reset
(crash recovery, watchdog), which is why the tape is wiped at every start-up.

### Proof on the board (2026-11-18, before WP-07)

This was run before clips were made off by default and tied to the alert; the clip rules it
shows (clip from the first heard second) are gone. Built with `--features earfake`, the firmware mixes a made-up quadcopter sound (four rotors
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
seconds of the tape as hex for checking on a laptop (nothing while clips are off: there is no
tape); it must never be in a deployed unit. Both features are bench-only and fenced: the build
refuses them without `ROTORSCOPE_BENCH=1`, and the hello line, the boot event and the screen's
`BENCH BUILD` mark name them (docs/BENCH.md "Bench builds").

### Bench checks (WP-07, only the default-off part of step 1 run)

Everything WP-07 changed is compile-checked and laptop-tested only, except the default-off part of
step 1 (run 2026-11-18). On the bench, one board, with a spare FAT32 card:

1. **Default off.** (Default-off part RUN 2026-11-18: the site line ends `audio clips off`, the
   `hub-p4: ear` status line says `clips off tape 0 s not_saved 0`, no clip files were created, and
   the boot event text was `audio clips off at boot`. The console `L` / `audio clips on` / `off`
   part below was NOT run.) Normal build, card without an `audio` line in `SITE.TXT`. Boot: the evidence
   boot event ends `audio clips off at boot`; no audio line on the screen; the `hub-p4: ear`
   status line says `clips off tape 0 s`. Console `L`, `audio clips on`: within a second
   `hub-p4: audio clips ON (from the console)`, the screen shows `AUDIO CLIPS ON: sound may be
   recorded`, the status line's `tape` count starts rising; one `setting` event
   `site (console): audio clips on`, not two. `L`, `audio clips off`: the line goes, `tape 0 s`.
2. **From the file.** Add `audio clips on` to `SITE.TXT` on a laptop and boot: one `setting`
   event `site (file): audio clips on` after the card mounts; the screen line appears.
3. **No tape while off** (`ROTORSCOPE_BENCH=1`, `--features eardump`): with clips off, console
   `w` prints `EARPCM none: audio clips are off`. Turn clips on, wait 10 s, `w` prints sound;
   turn them off and `w` again prints `none`.
4. **Clip only on alert** (`--features earfake`, clips on in `SITE.TXT`): the made-up rotor
   sound starts 20 s after boot. No clip file is created before the `SOUND` line (about 25 s);
   the screen shows `RECORDING SOUND to SD card` from then until the clip is written; the clip
   starts about 11 s before the `SOUND` line (check its length against the console times) and
   the read-back says `MATCH`. With clips off, the same run makes the `SOUND` line and no
   clip.
5. **Aircraft.** With an ADS-B aircraft tracked within 3 km (or `modesloop`) and the fake sound,
   no `SOUND` line and no clip.
6. **Hourly cap.** Needs seven alerts in one boot (the count is kept in RAM, so a reboot
   starts it again; the 256 MiB total on the card is what survives reboots). `earfake` gives
   only two per boot (20-45 s and 70-80 s), so this needs a local, uncommitted change to repeat
   the fake sound every 40 s. The seventh contact prints `audio clip not saved: hourly clip
   limit reached`; the status line shows `not_saved 1 (... hour 1 ...)` and the screen
   `AUDIO CLIPS ON: may record; 1 not saved`. The rule itself is tested on a laptop
   (`tracker::sound::clip` tests).
7. **Space.** A small card (2 GB, FAT32): every clip is refused with `card space kept for the
   evidence log`. A FAT16 card: `card free space unknown`. A card whose `ROTORSCP/AUDIO` holds
   over 254 MiB of files: `audio limit on the card reached`. Evidence keeps recording in each case.
8. **Switching off mid-clip.** During a fake-sound clip type `L`, `audio clips off`: the clip is
   closed at once, its `sound_clip` entry ends `(cut short: audio clips switched off)`, the WAV
   plays to its end on a laptop, the screen line goes.
9. **Watchdog and loop.** During a clip, `max_gap` on the link line stays under 100 ms and the
   watchdog never fires; switching clips off (tape wipe) shows one gap of about 0.1 s.
10. **Microphone DMA** (src/mic.rs, "Ring"). The driver now reads buffers with volatile loads
    and copies them out first; check that `bufs` still rises by 31.25 a second and `dma_err`
    stays 0, and the earfake detection is unchanged. Then, separately and only if wanted: set
    `in_check_owner` (`IN_CONF1` bit 12) in `Mic::start`, stall the loop for 0.5 s (a bench-only
    busy-wait), and see whether the DMA stops with a descriptor error. If it does, the owner
    check needs a restart path (re-link the ring and restart `IN_LINK`) before it can be used.
11. **A real sound through the microphone** (needs the owner's OK before any audio is played):
    a loudspeaker playing a drone recording 1-2 m from the board, clips on. Expect `Heard`,
    a `SOUND` line after 5 s, one clip; then a recording of speech: no `SOUND` line and no
    clip. Delete the clips afterwards.

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
