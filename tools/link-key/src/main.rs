//! Kit keys for the ESP-NOW link (docs/LINK-KEYS-DESIGN.md 4.5).
//!
//! ```text
//! link-key kit new [--force]                 new master in tools/kit.key + empty roster; prints the kit id
//! link-key hub <hub-board> [--keep-boot]     master + roster to the hub's rskit; seeds rsboot
//! link-key node <board> [--slot N] [--label TEXT] [--mac MAC] [--hub <hub-board>]
//!                                            provision a node (next free slot, or slot N at gen+1)
//! link-key hub-roster <hub-board>            roster only to the hub (never the master)
//! link-key revoke --slot N                   mark a slot revoked (then hub-roster)
//! link-key open <board> [--hub] [--keep-boot] write an open (unauthenticated) record
//! link-key show                              kit id and roster with key checks
//!
//! Common options:
//!   --kit-dir DIR   where kit.key and kit.roster live (default tools/)
//!   --table CSV     the board's partition table (default tools/partitions-node.csv,
//!                   or tools/partitions-hub.csv for hub commands)
//!   --hub-table CSV the hub's table for `node --hub`
//!   --dry-run       print the dev-board.sh / espflash commands; touch no board
//!                   and do not change the roster (node needs --mac then)
//!   --bench-corrupt (node, bench only) write a record whose CRCs are both bad
//! ```
//!
//! Keys are never printed. Kit id and key checks (8 hex digits each) name
//! them without revealing them; boards print the same at boot.
//!
//! `link-key hub` (and `open --hub`) first reads the hub's boot counter and,
//! when it still reads good, seeds the new one just above it (the nodes then
//! follow the re-provisioned hub at once); `--keep-boot` leaves it alone.
//! The dry run prints the read command too.
//!
//! Boards are reached through `tools/dev-board.sh` (`$ROTORSCOPE_DEV_BOARD`
//! overrides the script, for tests). An interrupted board write wipes the
//! key image first and exits with status 130.

use link_key::*;
use std::path::PathBuf;
use std::process::ExitCode;

fn tools_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

// --------------------------------------------------------------- kit keys --

const USAGE: &str = "usage: link-key kit new [--force] | hub <hub-board> [--keep-boot] | \
node <board> [--slot N] [--label TEXT] [--mac MAC] [--hub <hub-board>] | hub-roster <hub-board> | \
revoke --slot N | open <board> [--hub] [--keep-boot] | show\n\
options: --kit-dir DIR  --table CSV  --hub-table CSV  --dry-run";

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    kit_dir: Option<PathBuf>,
    table: Option<PathBuf>,
    hub_table: Option<PathBuf>,
    slot: Option<u8>,
    mac: Option<String>,
    label: Option<String>,
    hub: Option<String>,
    hub_flag: bool,
    force: bool,
    dry_run: bool,
    keep_boot: bool,
    bench_corrupt: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match x.as_str() {
            "--kit-dir" => a.kit_dir = Some(val(x)?.into()),
            "--table" => a.table = Some(val(x)?.into()),
            "--hub-table" => a.hub_table = Some(val(x)?.into()),
            "--slot" => a.slot = Some(val(x)?.parse().map_err(|_| "--slot needs a number 0..15")?),
            "--mac" => a.mac = Some(val(x)?),
            "--label" => a.label = Some(val(x)?),
            "--force" => a.force = true,
            "--dry-run" => a.dry_run = true,
            "--keep-boot" => a.keep_boot = true,
            "--bench-corrupt" => a.bench_corrupt = true,
            "--hub" => {
                // `open <board> --hub` is a flag; `node <board> --hub <hub-board>` takes a value.
                if a.pos.first().map(String::as_str) == Some("node") {
                    a.hub = Some(val(x)?);
                } else {
                    a.hub_flag = true;
                }
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n{USAGE}")),
            s => a.pos.push(s.into()),
        }
    }
    Ok(a)
}

fn run(argv: &[String]) -> Result<String, String> {
    let a = parse(argv)?;
    let kit_dir = a
        .kit_dir
        .clone()
        .or_else(|| std::env::var_os("ROTORSCOPE_KIT_DIR").map(PathBuf::from))
        .unwrap_or_else(tools_dir);
    let pos: Vec<&str> = a.pos.iter().map(String::as_str).collect();
    let node_table = || a.table.clone().unwrap_or_else(|| tools_dir().join("partitions-node.csv"));
    let hub_table = || a.table.clone().unwrap_or_else(|| tools_dir().join("partitions-hub.csv"));
    let script = std::env::var_os("ROTORSCOPE_DEV_BOARD").map(PathBuf::from).unwrap_or_else(|| tools_dir().join("dev-board.sh"));
    let mut dry = DryRun { script: script.clone(), log: Vec::new() };
    let mut real = DevBoard { script };
    let mut rng = os_random;
    let mut ctx = Ctx {
        kit_dir: kit_dir.clone(),
        tmp_parent: std::env::temp_dir(),
        board: if a.dry_run { &mut dry } else { &mut real },
        dry_run: a.dry_run,
        rng: &mut rng,
    };
    let r = match pos.as_slice() {
        ["kit", "new"] => kit_new(&kit_dir, a.force, ctx.rng),
        ["show"] => show(&kit_dir),
        ["revoke"] => {
            let slot = a.slot.ok_or("revoke needs --slot N")?;
            if a.dry_run {
                Ok(format!("[dry-run] would revoke slot {slot}\n"))
            } else {
                revoke(&kit_dir, slot)
            }
        }
        ["node", board] => {
            let mac = a.mac.as_deref().map(parse_mac).transpose()?;
            let o = NodeOpts {
                slot: a.slot,
                mac,
                label: a.label.clone(),
                hub: a.hub.clone(),
                hub_table: a.hub_table.clone().or_else(|| Some(tools_dir().join("partitions-hub.csv"))),
                bench_corrupt: a.bench_corrupt,
            };
            if o.bench_corrupt && std::env::var("ROTORSCOPE_BENCH").as_deref() != Ok("1") {
                return Err("--bench-corrupt is a bench check: set ROTORSCOPE_BENCH=1".into());
            }
            node(&mut ctx, board, &node_table(), &o)
        }
        ["hub", board] => hub(&mut ctx, board, &hub_table(), a.keep_boot),
        ["hub-roster", board] => hub_roster(&mut ctx, board, &hub_table()),
        ["open", board] => {
            let t = if a.hub_flag { hub_table() } else { node_table() };
            open(&mut ctx, board, &t, a.hub_flag, a.keep_boot)
        }
        _ => Err(USAGE.into()),
    };
    // Dry-run commands first, then the result (or the error after them).
    let mut out = String::new();
    for l in &dry.log {
        out.push_str(l);
        out.push('\n');
    }
    match r {
        Ok(s) => Ok(out + &s),
        Err(e) => {
            print!("{out}");
            Err(e)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(s) => {
            print!("{}", if s.ends_with('\n') { s } else { s + "\n" });
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("link-key: {e}");
            if interrupt::any() { ExitCode::from(130) } else { ExitCode::FAILURE }
        }
    }
}
