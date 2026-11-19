# Step 1: The demo, no hardware

**Level: 🟢 Beginner** · From nothing to made-up drones moving on your screen, with no
electronics at all. You will install one programming tool, download Rotorscope and run its
laptop viewer.

The laptop viewer is called **rid-view**. Normally it shows what Rotorscope's radio boards
hear. With its demo switch, `--demo`, it invents some drones, aircraft and radio signals
itself, so you can see what Rotorscope shows without owning any hardware.

> [!NOTE]
> Everything in this step is **simulated only**. The demo does not listen to the radio. It
> builds its drones' messages with the same code a real transmitter would use and feeds them
> through the same tracking code the hub runs, so what you see is what the hub would show if
> those drones were real.

## What you need

| Thing | Notes |
|---|---|
| A computer running **Linux** | This is what the developer uses, and the only system this has been tried on. |
| An internet connection | To download the code and the pieces it is built from. |
| Free disk space | The download is small. Building it makes a `target` folder that is far larger: on the developer's laptop, a debug build of rid-view from scratch left a 427 MB `target` folder. The release build used below and `cargo test` were not measured, so keep a few gigabytes free to be safe. |
| Time | The first build compiles everything from scratch. On the developer's laptop a debug build of rid-view took 1 min 38 s; the release build was not timed and yours may be slower. Later runs start in seconds. |

**What about macOS and Windows?** Nobody has tried them.

- **Windows:** rid-view will not build as it is. Parts of it use features only Unix-like
  systems have (it reads serial ports the Unix way).
- **macOS:** it may build, because macOS is Unix-like, but this has not been tried. The
  laptop-Bluetooth listener (`--ble`) and the Sonoff dongle reader (`--sniffle`) are built
  only on Linux; on any other system those two options report that they are not available.

If you only have Windows or macOS and something fails, that is expected for now. Please say
so on the project's issue tracker rather than assume you did something wrong.

## 1. Open a terminal

A **terminal** is a window where you type commands instead of clicking. On most Linux
desktops you can open one by pressing Ctrl+Alt+T, or by searching your applications for
"Terminal".

Each grey box below holds one command. Copy it, paste it into the terminal (Ctrl+Shift+V in
most Linux terminals) and press Enter. Wait for one command to finish, and the prompt to come
back, before you paste the next.

## 2. Install Rust

Rotorscope is written in **Rust**, a programming language. You install it with a tool called
**rustup**. This is the official command from [rustup.rs](https://rustup.rs):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

It asks you to choose an installation option. Press Enter to accept the default.

When it finishes, **close the terminal and open a new one**, so it can find the new tools.
Then check that Rust is there:

```bash
rustc --version
```

You should see one line starting with `rustc` and a version number, for example:

```text
rustc 1.xx.x (...)
```

with real numbers in place of the `x`s. Rotorscope needs a recent version of Rust, so use the
one rustup installs rather than an older one from your system's software manager.

> [!TIP]
> If a later step stops with an error mentioning `linker` or `cc not found`, your computer
> has no C compiler, which Rust needs to finish a program. Install your Linux system's
> standard build tools (on Debian and Ubuntu the package is called `build-essential`), then
> run the command again.

## 3. Get the code

You need **git**, the tool that downloads and tracks source code. Most Linux systems have
it already; if the command below says `git: command not found`, install `git` with your
system's software manager first.

```bash
git clone https://github.com/holdTheDoorHoid/rotorscope
```

This makes a folder called `rotorscope` with all the code in it. Move into that folder:

```bash
cd rotorscope
```

Every command from here on is run from inside this folder.

## 4. Run the demo

```bash
cargo run --release -p rid-view -- --demo
```

`cargo` is Rust's build tool, installed with rustup. This command builds rid-view and then
starts it with the demo switched on. (Everything after the lone `--` is passed to rid-view
itself.)

The first time, you will see a long list of `Compiling ...` lines while cargo builds
Rotorscope and everything it depends on. This is the slow part. Then the screen clears and a
live table appears, redrawn several times a second.

## Reading the table

The screen has a few parts, from top to bottom.

**The top line** shows the time (in UTC, marked `Z`), how many drones are active and how many
have been seen in total, and your own position. In the plain demo you have not given a
position, so it says:

```text
own position unknown (no --own-pos): no range or bearing
```

and the distance and direction columns show `-`. The first thing to try below fixes that.

**The node lines** each describe one source of data. A **node** is one of Rotorscope's
listening boards. In the demo there are five pretend ones (`demo Wi-Fi`, `demo BLE`,
`demo Wi-Fi 5 GHz`, `demo DroneID` and `demo FPV scanner`) and one pretend aircraft receiver
(`adsbee demo`). Each line counts what that source has delivered so far.

**The drone table** has one row per drone:

```text
  UAS ID                 TYPE    HEARD        RSSI    AGE     DIST      BRG    ALT    SPD  OPERATOR         SELF-ID
* DEMO-ALPHA-0001        multi   WB+B4         -71   0.0s        -        -   120m   31.5  39.9522,-75.1649 rid-view demo: survey
```

That row is an example from the plain demo (no position given, so DIST and BRG are `-` and
OPERATOR shows the pilot's latitude and longitude); your numbers will move as the drones fly.

| Column | What it means |
|---|---|
| (first character) | `*` a drone announced in the last 10 seconds (also highlighted in yellow). `?` **unconfirmed**: not yet heard again a second after it was first heard, so not announced, and possibly forged. Blank: an ordinary confirmed drone. |
| **UAS ID** | The drone's identity, usually its serial number. If no identity has arrived yet, its radio address in brackets. Some DJI drones heard only on DJI's own signal show as `DJI ...` (for example `DJI O4 ...`), and a DJI signal that could not be decoded as `DroneID?` with its frequency in MHz. |
| **TYPE** | What kind of aircraft it says it is: `multi` (multirotor), `plane`, `vtol`, `glider` and so on, or `?`. |
| **HEARD** | How it was heard. `WB` Wi-Fi beacon, `WN` Wi-Fi NAN (see the [glossary](../project/glossary.md)), `B4` Bluetooth 4, `B5` Bluetooth 5, `LR` Bluetooth Long Range, `DJ` DJI DroneID, `O4` DJI's newer encrypted DroneID, `DW` DJI's older Wi-Fi DroneID. `WB+B4` means both. |
| **RSSI** | Signal strength of the latest message, in dBm. Closer to zero is stronger: -40 is strong, -90 is faint. |
| **AGE** | How long since it was last heard. |
| **DIST** | Distance from you to the drone, in metres (or km past 10 km). Needs your position. |
| **BRG** | Bearing: the direction from you to the drone, in degrees from north and as a compass point. Needs your position. |
| **ALT** | The drone's reported altitude, in metres. |
| **SPD** | Its reported ground speed, in metres per second. |
| **OPERATOR** | Where the pilot is: distance and bearing from you, or the pilot's latitude and longitude if you have not given your position, or `-` if the drone did not say. |
| **SELF-ID** | A short free-text description the pilot can set. |

A row turns dim when the drone has not been heard for 60 seconds, and disappears after 10
minutes (an unconfirmed one, which is also shown dim, after 30 seconds). Drones you are hearing now are listed first, nearest first.

**Below the drones**, the demo also fills in tables that appear only when there is something
in them:

- **OPEN FPV LINKS**: a pretend video link from a hobby system called wfb-ng. Its
  identifier is hidden unless you ask for it, because it could identify someone's equipment.
- **RADIO SIGNALS**: pretend raw radio energy, such as analog video, with frequency and
  strength but no position.
- **WI-FI DRONES**: a pretend toy drone recognised by its Wi-Fi network name.
- **AIRCRAFT (ADS-B)**: three pretend crewed aircraft (a helicopter, an airliner and a light
  aircraft) with plainly made-up call signs such as `DEMOHELI`. Crewed aircraft are shown
  for awareness and never raise an alarm.

**At the bottom**, a short log of recent events, such as `new drone: ...`. The terminal also
beeps when a new drone is announced, standing in for the hub's buzzer.

Every identity in the demo is made up: drone serial numbers start with `DEMO`, and the
others (the DJI key, the toy drone's network name, the aircraft) are equally fake. For what each piece of a drone's
report means in practice, see [Reading a detection](../use/reading-detections.md).

## Stopping it

Press **Ctrl+C** (hold Ctrl and press C). rid-view stops and the prompt comes back. The demo
runs until you stop it.

## Things to try next, still without hardware

**Give it your position**, so distances and directions appear. The demo's drones circle the
position you give. This example uses Philadelphia City Hall, which is also the demo's default
centre:

```bash
cargo run --release -p rid-view -- --demo --own-pos 39.9526,-75.1652
```

The numbers are latitude then longitude, with a comma and no space. Use your own if you like
(you can read them off any online map).

**Print one table and stop.** `--once` simulates 10 seconds of the demo instantly, prints the
table once and exits. Handy for a quick look or to save the output:

```bash
cargo run --release -p rid-view -- --demo --once
```

**See the raw data the hub sends.** `--json` prints one line of machine-readable text per
drone, aircraft and signal instead of the table, built by the same code that writes the
hub's JSON lines for other programs:

```bash
cargo run --release -p rid-view -- --demo --once --json
```

**Open the phone page in your browser.** The hub serves a web page for phones. rid-view can
serve the same page from your laptop:

```bash
cargo run --release -p rid-view -- --demo --http 127.0.0.1:8080
```

It prints `rid-view: phone page at http://127.0.0.1:8080/`. Open that address in your web
browser while rid-view is running. The page can also set your position. Keep the address as
`127.0.0.1`, which means "this computer only": the page shows where drones are, so it should
not be open to your whole network. Press Ctrl+C in the terminal to stop.

**See every option.**

```bash
cargo run --release -p rid-view -- --help
```

Some options need hardware (a board's serial port, `--ble`, `--sniffle`, `--adsbee`), and
some send data to other programs (`--cot` for TAK map apps, `--dragonsync` for WarDragon).
rid-view can also **replay a recording**: give it a file of a board's output instead of a
port, and it plays the run back as it happened. The repository does not include a recording
made for this, so that becomes useful once you have a board. The
[rid-view page](../use/rid-view.md) explains every option.

**Run the project's tests.** Rotorscope has a large set of automatic tests that check its
decoding and tracking against made-up and recorded data. Running them is a good way to check
your setup works, and it needs no hardware:

```bash
cargo test
```

This builds much more than rid-view (and some C code from the official Remote ID library, used
to cross-check Rotorscope's own decoder), so it takes considerably longer than the demo. Some
tests deliberately push millions of made-up samples through the code. At the end every group
of tests should report:

```text
test result: ok.
```

followed by how many passed.

## If something goes wrong

- `cargo: command not found`: close the terminal and open a new one after installing Rust.
- `could not find Cargo.toml`: you are not inside the `rotorscope` folder. Run `cd rotorscope` first.
- The table looks scrambled: make the terminal window wider. The table is about 130
  characters across.
- Anything else: see [Troubleshooting](troubleshooting.md).

**Next:** [Step 2: One board, a real receiver](step-2-one-board.md)
