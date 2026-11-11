//! rid-ble-beacon: a BENCH-ONLY Bluetooth 4 Remote ID test transmitter for
//! a Linux laptop, so `rid-view --ble`, `node-ble` and phone apps can be
//! tested without a drone or a second board.
//!
//! It registers a BlueZ LE advertisement (over D-Bus; no root) whose 0xFFFA
//! service data is a Remote ID message built by `rid`'s encoder, and
//! rotates the five message types as a real BT4 broadcaster does, with the
//! counter going up on every change. Identity, flight and conduct are
//! exactly `firmware/rid-beacon`'s: its `rid_tx.rs` and `flight.rs` are
//! compiled in here unchanged, so the fake serial `TESTDRONE0000000001` and
//! the 50 m test circle cannot drift apart. Advert layout: see `payload.rs`.

#[path = "../../../firmware/rid-beacon/src/flight.rs"]
#[allow(dead_code)]
mod flight;
#[path = "../../../firmware/rid-beacon/src/rid_tx.rs"]
#[allow(dead_code)]
mod rid_tx;

mod payload;

#[cfg(target_os = "linux")]
mod bluez;

use std::process::ExitCode;

const USAGE: &str = "\
rid-ble-beacon: BENCH-ONLY Bluetooth 4 Remote ID test transmitter (BlueZ)

USAGE:
    rid-ble-beacon ADAPTER [--interval-ms N] [--secs N]

ADAPTER is a BlueZ adapter name, e.g. hci0. Sends legacy (BT4) adverts with
the obviously fake identity TESTDRONE0000000001, flying a 50 m circle
around RID_TEST_LAT / RID_TEST_LON (environment; default Philadelphia City
Hall). Ctrl-C stops it and withdraws the advert.

OPTIONS:
    --interval-ms N   Change the message every N ms (200-2000, default
                      500). Each of the five types repeats every 5*N ms.
    --secs N          Stop after N seconds.
    -h, --help        This text.
";

pub struct Options {
    pub adapter: String,
    pub interval_ms: u64,
    pub secs: Option<u64>,
}

fn parse_args() -> Result<Options, String> {
    let mut adapter = None;
    let mut interval_ms = 500;
    let mut secs = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut num = |name: &str| -> Result<u64, String> {
            args.next()
                .ok_or(format!("{name} needs a value"))?
                .parse()
                .map_err(|_| format!("{name}: not a number"))
        };
        match a.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--interval-ms" => interval_ms = num("--interval-ms")?,
            "--secs" => secs = Some(num("--secs")?),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ if adapter.is_none() => adapter = Some(a),
            _ => return Err("only one adapter".into()),
        }
    }
    if !(200..=2000).contains(&interval_ms) {
        return Err("--interval-ms must be 200-2000".into());
    }
    Ok(Options {
        adapter: adapter.ok_or("which adapter? e.g. rid-ble-beacon hci0")?,
        interval_ms,
        secs,
    })
}

fn banner() {
    eprintln!(
        "\
************************************************************************
*  rid-ble-beacon TRANSMITS a Remote ID TEST signal over Bluetooth.    *
*  Bench use only: indoors, metres not kilometres, only while testing. *
*  Identity is deliberately fake (TESTDRONE0000000001 / TEST-OPERATOR, *
*  Self-ID \"BENCH TEST-NOT AIRCRAFT\"). Never use it to imitate a real  *
*  aircraft. Ctrl-C stops it.                                          *
************************************************************************"
    );
}

fn main() -> ExitCode {
    let o = match parse_args() {
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("rid-ble-beacon: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        Ok(o) => o,
    };
    banner();
    match run(&o) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rid-ble-beacon: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn run(o: &Options) -> Result<(), String> {
    bluez::run(o)
}

#[cfg(not(target_os = "linux"))]
fn run(_o: &Options) -> Result<(), String> {
    Err("needs Linux and BlueZ".into())
}
