# DJI O4 DroneID: what Rotorscope receives

PLAN.md Phase 10. Status 2026-11-16: built and tested on synthetic signals only. No O4 drone or
O4 recording has been through it yet. *Update 2026-11-18: O4 recordings have since been through
the receiver: RFUAV recordings of a DJI Mini 4 Pro and an Avata 2 gave CRC-valid O4 records
(INFP-10/11) on 2026-11-16, via `tools/rfuav-check` (docs/DJI-LINKS.md, "Validation against RFUAV
recordings"). No real O4 drone yet.*

DJI's newest video link (O4, also called "DroneID v3" in the spec below: Mini 4 Pro, Mini 5 Pro,
Air 3, Avata 2, Neo, Flip, Mavic 4) still sends DroneID bursts, but the contents are encrypted.
Rotorscope receives them, checks them exactly as the published specification says, and shows
each drone as a session: `DJI O4 <key id>`.

## Sources (facts only)

| Source | Licence | Used for |
|---|---|---|
| EdwardBlair/dji-droneid-spec, [`RF_PHY.md` at 55bb886](https://github.com/EdwardBlair/dji-droneid-spec/blob/55bb88676dbc75913f074d7816ed5f7a3ab92454/RF_PHY.md) | none stated | waveform, coding, channel schedule, the SHA-256 test vectors |
| EdwardBlair/dji-droneid-spec, `SPEC.md` / `README.md` (HEAD `be58aa0`) | none stated | logical-object envelope, CRYP/INFP core layouts, key ID meaning |
| [luyii-code-1/dji-ocusync-droneid-research](https://github.com/luyii-code-1/dji-ocusync-droneid-research) `FACTS.md` | GPL-3.0 | model behaviour, other frequency reports (King-Of-Knights, Bender 2022), O3 encryption report |

The spec repository has no licence, so none of its text or code (`droneid.py`) is copied; our
code is written from the facts. luyii's repository is GPL-3.0 like ours, but nothing is copied
from it either.

## The facts we rely on

**Waveform.** 8 OFDM symbols at 15.36 MS/s, 1024-point FFT, 15 kHz carriers, cyclic prefixes
80, 72, 72, 72, 72, 72, 72, 80 samples (8784 samples, 572 µs). 600 carriers, -300..-1 and
+1..+300. Symbol 2 is the root-600 Zadoff-Chu reference, symbol 4 the root-147 one; symbols 0,
1, 3, 5, 6, 7 carry QPSK data. This is the same "8-symbol" layout Mavic Pro / Mavic 2 used, so
`crates/droneid` already handled it.

**Coding.** 173 payload bytes + CRC-24A = 1408 bits, LTE turbo code (K = 1408, QPP 43/88),
rate matching to 7200 bits (45 rows, 28 dummies, start 90, RV 0), LTE Gold scrambler with
`c_init = 0x12345678`. Again identical to the older bursts.

**Packet.** The 173 bytes are a slot holding one logical object: `[length][core][7 zero
bytes][CRC-16, little-endian]` (CRC-16 initial value 0x3692, reflected polynomial 0x8408, over
the length byte and payload), then zeros to the end of the slot. The core is one of:

| Record | Selector | Marker | Core length | Cleartext fields |
|---|---|---|---|---|
| CRYP | 0x13 | `CRYP` | 163 bytes | key ID (bytes 5-8), IV (121-128), length 32 (129-130) |
| INFP-10 | 0x10 | `INFP` | 128 bytes | key ID (5-8), IV (9-16), length 109 (17-18) |
| INFP-11 | 0x11 | `INFP` | 160 bytes | key ID (5-8), IV (9-16), length 141 (17-18) |

Everything after those headers is ciphertext. The key ID is the first four bytes of a hash of
the per-flight session key: it is the same in every CRYP and INFP record of one session, so it
works as a session identifier. It is not unique (four bytes; collisions are possible) and it
proves nothing about who sent the packet.

**Behaviour (model-dependent).** On a DJI Mini 5 Pro, CRYP records start after the drone gets a
GNSS fix and INFP records after takeoff. Older drones (Mini 2, Mavic 2) send 13 bursts per
frequency 640 ms apart before moving on. Encrypted DroneID is also reported from an Inspire 3
(O3), not yet independently confirmed.

**Channel schedule.** Seven centres: 2414.5, 2429.5, 2444.5, 5756.5, 5776.5, 5796.5,
5816.5 MHz. One publication every 640 ms (nominal); an 8-bit counter `c` picks the channel:
`r = (c + 255) mod 256`, `i = (r + 6) mod 7`, channel = `[2429.5, 2444.5, 5756.5, 5776.5,
5796.5, 5816.5, 2414.5][i]`. The full cycle is 163.84 s. A different report (King-Of-Knights,
via luyii) lists eight frequencies including 2459.5 MHz and calls the hopping random per drone,
so we treat the schedule as useful but unproven.

## What we checked

**The published test vectors.** The spec gives SHA-256 digests of seven stages for the input
bytes `00 01 .. ac`. `crates/droneid/src/vectors.rs` builds the transmit chain stage by stage:

| Stage | Matches |
|---|---|
| input (173 B), + CRC-24A (`b6 a0 38`) | yes |
| turbo output (4236 bits) | yes |
| rate-matched (7200 bits; first 32 `0011...`) | yes |
| scrambled (7200 bits; first 32 `1110 0001 ...`) | yes |
| frequency-domain symbols (8 x 1024 complex f32) | **no** |
| time-domain packet (8784 complex f32) | **no** |

The five bit-level stages match exactly, so our coding chain (and the receiver's inverse of it)
is the published one bit for bit, and no fix to the receiver was needed. The two floating-point
stages do not match although every value follows the spec's formulas (checked to 1e-9). A digest
changes if a single float is rounded differently, and the spec publishes no code for these
stages; we tried the plausible conventions (listed in the test's comment) without finding the
one used. This does not show the waveform is wrong; it shows we cannot prove it bit for bit.
Our zero-padded references and carrier order agree with the spec's formulas exactly, and with
the convention that decodes real OcuSync 2 recordings.

**Decoding.** The spec's test packet and synthetic CRYP / INFP-10 / INFP-11 packets go through
the same capture model the C5 uses (20 MS/s, and the 4-bit Path B quantiser) and decode with
CRC-24A at 30, 20, 12 and 6 dB SNR and offsets from -310 kHz to +1.25 MHz; at 2 dB only the
turbo decoder gets them.

## What Rotorscope does with an O4 packet

1. **Validation** (`droneid::payload::O4Checks`). After CRC-24A: the logical object's end lies
   inside the slot, its CRC-16 matches, selector and marker agree, the length is exactly core +
   7, the record's own length field is right, the padding and the rest of the slot are zero. Only
   a record that passes all of them counts (`Encrypted::valid`); anything else is reported as a
   plain DroneID detection. The older 0x55 "V1" envelope is recognised in logs but never counts.
2. **Cleartext only.** Record type, key ID, IV presence. The ciphertext is length-checked and
   otherwise untouched.
3. **Link (protocol 8).** The node sends `DroneIdRecord::O4 { kind, key_id, object }`: the
   type, the key ID and the whole logical object as received (at most 173 bytes; a link frame is
   at most 320). The raw bytes are kept on the hub's evidence log as part of the frame.
4. **Tracks.** CRYP and INFP records with the same key ID are one track, labelled
   `DJI O4 <key id>` on the screen and in `rid-view` (DragonSDR's equivalent is
   `drone-alert-<hex>`). JSON has `dji.o4 { cryp, infp10, infp11, state, model_dependent }`;
   CSV puts the key ID in `uas_id` with `id_type` `O4Session`. The state is "GPS locked (CRYP
   seen)" or "probably flying (INFP seen)", always marked model-dependent. With
   `hide_droneid_ids` the key ID is hidden everywhere; the state still shows.
5. **The follower** (`droneid::follow`, wired into node-sdr's DroneID job). See below.
6. **Raw bytes and plaintext parsers.** `droneid::payload::Encrypted::{core, ciphertext,
   logical_object}` hand out the raw bytes, and `droneid::o4plain` checks and reads CRYP / INFP-10
   / INFP-11 *plaintext* (the spec's field tables and semantic checks; INFP-10 altitude is metres
   and height decimetres, unlike OcuSync 2). `crates/djicrypt` (see "Decryption") sits between
   the two, with an owner-supplied key.

## The channel follower

A C5 has one tuner. Parked on one frequency it hears an O4 drone on about one publication in
seven (one every 4-5 s). Scanning the 13-hop DroneID plan with 700 ms dwells (seven of the hops
are schedule channels) is worse: about one publication every 8.3 s (7.2 a minute, host model
`crates/rawcap/tests/follow_forge.rs`). The follower uses the schedule:

- The first valid O4 packet of a session starts 256 counter hypotheses and keeps the ~37 that
  put it on the channel it was heard on.
- Because the counter steps by one, those hypotheses almost always agree on the next channel,
  so the node can be on the right frequency for the very next publication (once the session is
  confirmed: see "The follower under forged packets"). They disagree only at
  the counter wrap; a later packet, or a missed window the node sat through completely, settles
  it.
- Before each predicted publication (window ±25 ms, growing with distance) the DroneID job
  retunes there, 50 ms early to allow for the DC calibration (~23 ms on 5 GHz, ~5 ms on 2.4 GHz) and
  the capture loop's own lateness (30 ms until 2026-11-18: on the bench most silent windows began
  before the receiver had settled); between windows it keeps scanning
  its plan, so other drones and other frequencies (2459.5, 2399.5 MHz) are still covered. A plan
  dwell cut short for a window ends 50 ms before the window counted from *before* its own retune
  (fixed 2026-11-18: counted from after it, the visit's retune ended ~8 ms inside the window, so
  no visit was ever "settled", silent windows never counted as misses, and at the counter wrap,
  where only a miss prunes the wrong hypotheses, the session was lost: one followed drone in ten
  within three minutes in the host model; the 2026-11-18 bench run had 90 silent visits and 2
  counted misses).
- A visit that hears nothing counts as a miss only if the receiver was settled for the whole
  window and no decode of something else held it (on a busy 2.4 GHz channel most visits have one).
  A visit that proves nothing is not a miss, but where the hypotheses disagree (the counter wrap)
  the next visit tries the second-best channel, so the wrap is still resolved by hearing the drone
  (bench 2026-11-18 before that: 21 of 26 silent visits interrupted, the drone lost at its wrap).
- Up to four sessions are followed at once; if two windows overlap the earlier one wins, except
  that a session 2 s without a packet goes first (so the share cannot starve it out).
- **Fallback:** four silent windows in a row, or 6 s without a packet, drop a followed session
  and the node is back to plain scanning. Plaintext (OcuSync 2) bursts never feed the follower,
  since those drones use a different hopping pattern.

### The follower under forged packets (2026-11-18)

The key ID is in clear and the CRCs are public, so anyone with a transmitter can send valid-looking
O4 bursts with any key ID, on any channel, at any time. Before the first change one such packet made
the node visit the predicted channel from the next publication on, four forged key IDs kept it on
attacker-chosen channels, a fifth evicted the real drone's session, and one packet with the real
drone's key ID on another channel restarted (wiped) its hypotheses. Now:

- **Confirmation.** A key ID that is not followed is a *candidate*: a 16-byte entry (key ID, time
  and channel of its last packet, channel and gap of the one before) in a table of 128. Its
  hypotheses are rebuilt from those two packets when needed. It is never visited. It is followed
  once it holds three packets that agree in channel and in time (each inside the window the earlier
  ones predict, ±25 ms growing to ±75 ms) and a followed slot is free. Until then only the plan's
  scanning feeds it: alone, a drone is followed within 60 s in 95 % of runs of the host model (90 %
  by 53 s). A candidate is restarted by a packet that fits no hypothesis (the old fallback) and
  dropped after 30 s without one.
- **A flood of new key IDs displaces nothing that matters.** With the table full, a new key ID takes
  the entry whose last packet is oldest. Every new key ID costs the node a full decode of a valid
  burst, and the decode budget lets at most 87 of those through in 30 s (at the fastest decode
  measured, 144 ms; 72 at 173 ms), fewer than the 128 entries: a flood can only push out
  candidates that would have expired anyway, and has nothing to steer (rate, timing and key IDs all
  make no difference). `crates/rawcap/tests/follow_forge.rs` checks the arithmetic against the real
  budget. The first version (2026-11-18 morning) had four slots instead: a new forged key ID every
  0.7 s replaced the drone's first packet within ~3 s, long before the plan heard it again, and on
  the bench the drone was never followed. Static memory: 128 x 16 bytes of candidates plus four
  ~70-byte sessions, about 2.3 KiB in all (inside node-sdr's boxed `Radio`, no heap of its own).
- **No eviction, no restart.** A packet that contradicts a followed session (a channel no hypothesis
  predicts, or outside the predicted window) is ignored and counted; the session keeps its
  hypotheses and timing. A confirmed candidate never displaces a followed session.
- **Time share.** Each visit is paid from a credit that earns half of the time spent on the plan
  and holds at most 1 s; a visit it cannot pay for is declined (the plan dwell is not cut short
  for it either). So the follower takes at most about half the receiver, however many sessions.
  A followed session 2 s without a packet goes before the others, so a drone followed before
  forged sessions arrive keeps its visits (host model: 99.5 % still followed after four forged
  sessions, the flood and wrong-time packets arrive; 96.4 % without that rule).

**What the forger still gets.** Numbers from the host model (1000 runs of 3 minutes each, the bench
scenario's streams, no decode cost unless stated):

| Case | Drone followed within 60 s | Followed at the end |
|---|---|---|
| Drone alone | 95 % | 100 % |
| New forged key ID every 0.7 s | 95 % | 100 % |
| Forgers arrive once the drone is followed | (95 %) | 99.5 % |
| Three forged sessions + the flood | 77 % | 84 % |
| Four forged sessions + the flood from the start | 71 % | 76 % |
| The same with the bench's phases (a forged session in step with the drone) | 45 % | 45 % |

- **All four followed slots taken first.** Schedule-perfect forged sessions are followed like a
  drone. If four of them are confirmed before the drone, the drone is not followed (in the bench's
  fixed phases, 50 % of runs; with random phases 11 %): it stays a candidate and is followed as
  soon as a slot frees up, which a forger who keeps transmitting never allows. Any rule that let a
  confirmed candidate take a followed slot could be used by a forger to take the drone's, so there
  is none. What the operator loses is the gain of following (a position every 0.64 s); the drone is
  still heard by the plan, see the next point.
- **The plan hears less, and not evenly.** Scanning is *not* untouched, as this page used to say.
  The follower's visits take their share of the receiver from the plan, so the plan hears a drone
  that is not followed in proportion: 7.2 a minute with the follower idle, 6.3 with one followed
  drone, 4.1-4.2 with three or four forged sessions followed (the follower then holds 41-45 % of the
  receiver). Worse, the visits come at the forged sessions' publication moments: a drone that
  publishes at the same moments as a followed forged session is heard by the plan only 1.9-2.3
  times a minute, under a third of the idle rate, because the receiver is away on the forged session's
  channel each time the drone transmits. A forger who can hear the drone can choose that phase.
  With the plan's rate down, three plan hits take longer, which is all that lowers the "within
  60 s" figures above; nothing the forger sends displaces the drone's candidate.
- **Decoding costs time too.** On the air every forged packet the node hears is a full decode
  (144-173 ms) during which it hears nothing. With the decode cost modelled: a new forged key ID
  every 0.7 s, drone followed within 60 s in 83 % of runs (90 % by 71 s); one every 360 ms (the
  most the decode budget passes, steady), 78 % (90 % by 82 s); three forged sessions with the flood,
  54-60 %. A forger who keeps the decode budget spent (a valid burst every few tens of ms) starves
  the drone's own decodes: 0.7 % followed. And once the drone is followed, forged packets in its
  windows and an exhausted budget can still make it miss its own packets: with all the forgers
  arriving after it is followed and every packet decoded, it is still followed after 4 minutes in
  55 % of runs. These are the decode budget's documented trade-off (first come), not the
  follower's rules.
- **Its key ID.** One who sends packets with a real drone's key ID while it is a candidate keeps
  restarting it, so it is never followed (it is still detected by the plan); consistent packets
  with a real key ID can narrow a followed session's hypotheses to a wrong one near the counter
  wrap, after which its own misses drop it.
- **What the hub sees.** Each forged key ID the node decodes is a DroneID sighting like any other:
  forged sessions show as drones, new key IDs as new unconfirmed tracks. The hub's flood alarm
  (`crates/tracker/src/probation.rs`, `IdFlood`) needs 20 new identities a second; one DroneID node
  decodes at most ~2.8 valid bursts a second, so this flood does **not** raise it, and nothing else
  flags it (valid bursts never count as jamming pressure).

Host tests: `crates/droneid/src/follow.rs`; the node model and the table above:
`crates/rawcap/tests/follow_forge.rs` (`cargo test --release -p rawcap --test follow_forge --
--ignored --nocapture`); bench scenario: BENCH.md "node-sdr under hostile signals" step 4
(`hil forge <secs> start|flood|late`).

Gain, if the schedule holds: once a session is confirmed, nearly every publication instead of
one in seven, i.e. a position update every 0.64 s instead of every ~4.5 s. The node's stats line
reports `"follow":[followed, visits, hits, misses, tentative, contradictions, refused, declined]`
so the gain can be measured on a real drone.

## What we will not do

- **Transmit DroneID.** Never, not even on the bench. The test packets exist only as sample
  arrays in host tests.
- **Embed or guess keys.** The repository contains no private or public key, and no
  session-key prediction. Decryption (below) only ever uses a key the owner installs.

## Decryption (owner decision 2026-11-16)

The owner authorised O4 decryption. `crates/djicrypt` does it on the P4 hub, following SPEC.md's
receiver steps (facts only; our own code):

1. A **CRYP** record carries a fresh 16-byte session key, locked to the receiver's SM2 public key.
   With the matching private key `d`, the hub computes `d x C1`, derives the mask with SM3, unlocks
   the session key, and accepts it only if SM3 check value `C3` matches and the key hashes to the
   record's key ID. It also decrypts CRYP's 32 auxiliary bytes (serial and aircraft position).
2. Session keys are kept by key ID (8 at once, forgotten after 30 minutes unused).
3. Every **INFP** record of that session is then decrypted (AES-128-CTR, counter `iv || 0 x 8`)
   and checked against the plaintext rules in `droneid::o4plain`: INFP-10 gives the serial, model,
   drone, pilot and home positions, altitude, height, speeds and the motors-on / in-air flags;
   INFP-11 the flight-purpose text.
4. The tracker folds the session's track into the drone's serial-number track (the same serial
   Remote ID carries), so one drone is one track. JSON shows `"decrypted": true`.

**The key.** There is no public key that decrypts real aircraft: DJI's AeroScope private key has
never been published, and the services that decode O4 today are closed. The owner installs a
lawfully obtained key as `ROTORSCP/O4KEY.TXT` on the hub's SD card: 64 hex digits (spaces, a `0x`
prefix and `#` comment lines are fine). At mount the hub prints only a fingerprint of the
matching public key; the status line shows `"o4":{"key":"loaded",...}` (or `none` / `bad`).
Without the file, nothing changes: O4 stays detect-only with its session ID.

**Key hygiene.** The key types have no debug or print form and are wiped from memory on drop; the
file buffer is wiped after reading. The key is never printed, logged, sent between boards or
written to the evidence log. `.gitignore` excludes `O4KEY.TXT` and `*o4key*` files.

**Evidence.** The raw frame (ciphertext included) is logged as before. Each decrypted record adds
an `o4_decrypted` event naming the session key ID and record type, with the decrypted serial and
positions as text, marked as derived from the frame before it. Anyone with the same key can
re-derive it from the raw record.

**Privacy.** INFP-10 also carries the pilot's DJI account name. The hub never reads it into the
tracker, the screen, any output or the evidence text. "Hide DroneID IDs" hides decrypted serials
like plaintext ones.

**Tested** with a key made up in the tests (`crates/djicrypt/src/tests.rs`): the test plays the
aircraft (SM2 wrap and AES-CTR as the spec gives the sender side), the hub code undoes it, and the
records pass `droneid`'s own structural checks first. Wrong keys, a changed C1, C2, C3 or key ID,
wrong lengths and colliding key IDs are all refused, and no rejected CRYP leaves a key behind.
**Not tested on a real aircraft** (no key exists for us to try).

## Needs a real O4 drone

- that the waveform decodes at all (the float stages are unproven, see above);
- that the record layout and the zero padding hold on air;
- whether the seven-channel schedule is what the drone does (and on which models), and the
  follower's hit rate;
- 5816.5 MHz on the C5: LO 5816 MHz is above the 5800 MHz the raw-capture path has been measured
  to reach;
- the CRYP/INFP state rule on models other than the Mini 5 Pro.
