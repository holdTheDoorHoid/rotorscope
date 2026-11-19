# Contributing

**Level: 🟡 Intermediate** · How to report a problem, propose a change, and which house rules a
change is expected to follow, with or without hardware.

Rotorscope is a small project with one owner. Help of every size is welcome: a clear bug report
or a corrected sentence counts as much as code. This page collects the rules the codebase and its
history already follow, so a change fits in the first time.

## Reporting a bug

Open an issue on [the GitHub repository](https://github.com/holdTheDoorHoid/rotorscope/issues). A
report the owner can act on says:

1. **Which board and firmware**: the board (for example a XIAO ESP32-C5 running `node-wifi`), how
   it was built (features, `cargo fw24-now` and so on) and the commit (`git rev-parse --short HEAD`).
   For a laptop tool, the tool and its options.
2. **What you did**, as commands someone else can run again.
3. **What happened**, and separately **what you expected**.
4. **The evidence**: the console output (the nodes print one JSON line per event, so a log saved
   with `tools/dev-board.sh --log 60 <board> | tee board.log` is ideal), a recording, or a short
   script that reproduces it.

Before you attach anything, take out what identifies other people: real serial numbers, board
addresses, a pilot's position (Remote ID carries it; it is personal data) and your own home
location. `droneid-file --redact` and `rid-view --hide-droneid-ids` exist for this.

## Security problems go privately

Do not open a public issue for a security problem. Use the repository's **Security** tab, **Report
a vulnerability**
(<https://github.com/holdTheDoorHoid/rotorscope/security/advisories/new>). What counts, what to
include and what to expect are in the [security policy](../project/security.md) and
[`SECURITY.md`](https://github.com/holdTheDoorHoid/rotorscope/blob/main/SECURITY.md). Some things
are limits of the signals, not bugs: anyone can broadcast a forged Remote ID, for example.

## Proposing a change

1. Fork the repository and make a branch for one change.
2. Make the change, with its tests and documentation (the rules below).
3. Run `cargo test`, and `tools/docs-build.sh check` if you touched the documentation. CI runs
   these and more on every pull request; [What CI checks](build-test-flash.md#what-ci-checks) lists
   each check and how to run it yourself.
4. Open a pull request against `main`. Say what you changed, why, and how far you proved it: host
   tests, compile-checked only, or a bench run (which boards, which date).

There are no tagged releases yet; `main` is the only supported code.

## House rules

These are visible across the code and the commit history. A change that follows them is quick
to review.

### Tests come with the change

- **Logic goes in a crate, with host tests.** Decoders, detectors and rules live in `crates/`,
  `no_std` and free of hardware, and are tested with `cargo test` on a laptop. Firmware stays a
  thin layer that calls them. If you find yourself writing logic in `firmware/`, ask whether it
  belongs in a crate. (`crates/rawcap` exists only to make the C5 raw-capture logic host-testable.)
- **A fix lands with a test that reproduces the problem.** The history shows the pattern: the
  commit that fixed `rid-view` replays names the test that failed before (`stdin_table`), and the
  drone-flood fix lists each case with "(failed before)". Write the failing test first.
- **Say how far it got.** Commit messages and documents use "Host tests: ...",
  "Compile-checked only", or a dated bench result. Do not round up; the six proof phrases in
  [Writing these docs](docs-style.md#say-what-is-proven-every-time) apply to code claims too.
- **Bench-only code is fenced.** A feature that fakes data, loops pins back or opens debug commands
  goes on its firmware's bench list in `build.rs` (`crates/link/src/benchfence.rs`), so a build
  with it needs `ROTORSCOPE_BENCH=1` and announces itself.
- If you change the screen, `cargo test` rewrites `crates/screen/snapshots/*.png`: look at them and
  commit them. If you change a shared crate's dependencies, commit the affected firmware
  `Cargo.lock` files too ([Hard-won lessons](gotchas.md#crate-versions-and-pins)).

### Rotorscope only listens

The project's conduct, from `README.md` and `PLAN.md`: receive-only, no jamming, no spoofing. The
one transmitter, `firmware/rid-beacon` (and its laptop twin `rid-ble-beacon`), exists purely to test
the receivers on the bench, uses an obviously fake identity and the lowest power, and must not be
used to impersonate an aircraft. It must **never** send DJI DroneID of any kind. Changes that add
transmission at, or interference with, anything will not be accepted. If a node's radio could
transmit (the C5 raw-capture node checks `txq_off` at boot), keep it provably off.

### Licences: what may be ported

Rotorscope is **GPL-3.0-or-later**. From `PLAN.md` (decided 2026-11-09) and `RESEARCH.md`:

- Third-party code must be **GPL-3.0-compatible**: Apache-2.0, MIT, BSD and GPL-3.0 are fine;
  **GPL-2.0-only is not**, and code with no licence may not be copied at all. (`RESEARCH.md` names
  ArduRemoteID, GPL-2.0, and RID_Scanner, unlicensed, as off limits.)
- Keep the original copyright notices when you port code (`PLAN.md`), and credit the project the
  way `README.md` ("Acknowledgements") and [Credits and licences](../project/credits.md) do.
- Where a project's licence does not fit, use it for **facts only**: `crates/droneid` takes layouts,
  constants and published examples from DroneID research and contains no code from it.
- `third_party/opendroneid-core-c` keeps its Apache-2.0 licence and is for host tests only.
- The licences of crates.io dependencies are checked by CI against the list in `deny.toml`
  ([Adding a licence](build-test-flash.md#adding-a-licence)).

### Never commit secrets, recordings or bench logs

Keys (`tools/kit.key` and `tools/kit.roster`, an old `tools/link.key`, `*.evkey`, FLARM and O4 key files), Wi-Fi credentials, downloaded recordings
(`testdata/`, `rfuav-results/`), `bench-logs/` and `tools/boards.local` are all in `.gitignore`.
Keep it that way. The repository is public: a key once pushed cannot be taken back, and the DroneID
recordings are AGPL-3.0 content the project only downloads. Test fixtures use obviously made-up
identities (`TESTDRONE0000000001`) and public landmarks for positions. More in
[A tour of the repository](repo-tour.md#what-is-deliberately-not-in-the-repository).

### Documentation changes with the code

The capital-letter deep dives (`docs/SPOOFING.md`, `docs/ESPNOW.md` and the rest) and the firmware
READMEs are edited **in the same change** as the code they describe; their names stay fixed because
other pages and commit messages link to them. Pages under `docs/` follow
[Writing these docs](docs-style.md): a level line, the proof phrases, British spelling.

### Commit messages in plain words

Look at `git log` for the style. A subject line names the area, then says what changed as a plain
sentence:

```text
tracker: deaf-node check learns by time, slowly, and never freezes
hub-p4: name the GPS memory file when the card has none
Follower: a busy channel no longer loses a followed drone at its counter wrap
```

The body explains the problem and why this fixes it, gives the numbers that were measured, and ends
with how far it was proven ("Host tests: ...", "Compile-checked only", "Verified on the P4 ...").

## Helping without hardware

Most of Rotorscope can be worked on with only a laptop:

- **Host tests and crates.** Every decoder and detector runs under `cargo test`. Tests for edge
  cases, hostile input and real-world formats are always useful.
- **Decoders against public recordings.** `tools/fetch-droneid-samples.sh` fetches the two DJI
  DroneID recordings; `tools/rfuav-check` runs the FPV and DroneID detectors over RFUAV recordings
  (Apache-2.0, downloaded, never committed). Results that disagree with the detectors are findings.
- **Documentation.** Fixes, clearer explanations and missing facts, following the
  [style page](docs-style.md).
- **The phone page.** `web/index.html` is one file. `rid-view --demo --http 127.0.0.1:8080` serves
  it from the laptop with simulated drones, so you can work on it with no hub
  ([The laptop viewer](../use/rid-view.md)).

## Helping with real drones or FPV gear

As of 2026-11-18, **Rotorscope has not yet heard a real drone.** Remote ID is proven on the air only
from the project's own test transmitter and a laptop; DJI DroneID is proven on recordings only.
What the project lacks, from the [Status](../project/status.md) page ("Known gaps") and
[Bench tests](../BENCH.md) ("Not testable with our hardware"):

| Missing | What would help |
|---|---|
| Remote ID from a real drone or a certified broadcast module | A drone or module broadcasting as it normally does, heard by a node or `rid-view` |
| Real DJI OcuSync / O4 DroneID | A DJI drone flown as usual near a `node-sdr` node |
| DJI legacy Wi-Fi DroneID | A Wi-Fi DJI (Spark, Mavic Air, Mini SE) |
| Analog FPV video, HDZero, Walksnail | A video transmitter in its owner's normal, lawful use ([Bench tests](../BENCH.md) notes analog video needs an amateur radio licence) |
| wfb-ng / OpenIPC with its default coding | OpenIPC hardware |
| ExpressLRS and other control links | An LR2021 board for the aero node (`firmware/node-lr`, never yet run on hardware), near a control link in normal use |
| China GB 46750 Remote ID | A drone sending it |
| Wi-Fi drones (Tello, Parrot, ...) | A real one; the project never imitates a maker's network |
| Real GPS modules and an ADSBee wired to the P4 | Hardware tests of code so far tested only by the chip feeding its own pins |

The project only ever **listens**, and asks the same of you. Fly only where and as you lawfully
would anyway, and let a Rotorscope board listen nearby; never transmit anything for the project's
sake, and never point anything at someone else's aircraft. [Law, safety and ethics](../start/law-and-ethics.md)
has more.

If you capture something, describe it in an issue first: what, with which hardware, where it can be
downloaded, and under what licence. The project keeps recordings out of the repository and fetches
them by checksum (as `tools/fetch-droneid-samples.sh` does), so a recording needs a public home and
a clear licence. Remove other people's identities and positions before sharing.

**Next:** [Writing these docs](docs-style.md)

## Where this lives in the code

- `SECURITY.md`: private reporting
- `PLAN.md` ("Risks to watch", item 4) and `RESEARCH.md`: the licence rule
- `README.md` ("Conduct", "Acknowledgements"): conduct and attribution
- `crates/link/src/benchfence.rs`: the bench fence
- `.gitignore`: what is never committed
- `docs/dev/docs-style.md`: the documentation rules
