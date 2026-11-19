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
//!
//! Legacy (until the per-node keys replace it; the current firmware build
//! still reads tools/link.key):
//! link-key new [--force] [FILE]   link-key id [FILE]
//! ```
//!
//! Keys are never printed. Kit id and key checks (8 hex digits each) name
//! them without revealing them; boards print the same at boot.

use link_key::*;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn tools_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

// ------------------------------------------------------------- legacy key --

fn default_path() -> PathBuf {
    tools_dir().join("link.key")
}

fn id_of(key: &[u8; 32]) -> String {
    airlink::auth::Key::new(key).id().iter().map(|b| format!("{b:02x}")).collect()
}

fn read_key(path: &PathBuf) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(format!("{}: not a link key (expected 64 hex digits)", path.display()));
    }
    let mut key = [0u8; 32];
    for (i, pair) in digits.chunks(2).enumerate() {
        key[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    Ok(key)
}

fn new_key(path: &PathBuf, force: bool) -> Result<String, String> {
    if path.exists() && !force {
        return Err(format!(
            "{} already exists (ID {}). Keep it: every board of the kit uses it. \
             To replace it on purpose: new --force, then reflash the gateway and every ESP-NOW node.",
            path.display(),
            read_key(path).map(|k| id_of(&k)).unwrap_or_else(|_| "unreadable".into())
        ));
    }
    let mut key = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut key))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(hex.as_bytes()).and_then(|()| f.write_all(b"\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(id_of(&key))
}

fn legacy(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("new") => {
            let force = args.iter().any(|a| a == "--force");
            let path = args.iter().skip(1).find(|a| !a.starts_with("--")).map(PathBuf::from).unwrap_or_else(default_path);
            new_key(&path, force).map(|id| {
                format!(
                    "wrote {} (key ID {id}). Never commit it (tools/link.key is git-ignored); back it up somewhere private.\n\
                     Now rebuild and flash the gateway and every ESP-NOW node; each prints \"key {id}\" at boot.",
                    path.display()
                )
            })
        }
        Some("id") => {
            let path = args.get(1).map(PathBuf::from).unwrap_or_else(default_path);
            read_key(&path).map(|k| id_of(&k))
        }
        _ => Err(USAGE.into()),
    }
}

// --------------------------------------------------------------- kit keys --

const USAGE: &str = "usage: link-key kit new [--force] | hub <hub-board> [--keep-boot] | \
node <board> [--slot N] [--label TEXT] [--mac MAC] [--hub <hub-board>] | hub-roster <hub-board> | \
revoke --slot N | open <board> [--hub] [--keep-boot] | show\n\
options: --kit-dir DIR  --table CSV  --hub-table CSV  --dry-run\n\
legacy: link-key new [--force] [FILE] | link-key id [FILE]";

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
    if matches!(argv.first().map(String::as_str), Some("new" | "id")) {
        return legacy(argv);
    }
    let a = parse(argv)?;
    let kit_dir = a
        .kit_dir
        .clone()
        .or_else(|| std::env::var_os("ROTORSCOPE_KIT_DIR").map(PathBuf::from))
        .unwrap_or_else(tools_dir);
    let pos: Vec<&str> = a.pos.iter().map(String::as_str).collect();
    let node_table = || a.table.clone().unwrap_or_else(|| tools_dir().join("partitions-node.csv"));
    let hub_table = || a.table.clone().unwrap_or_else(|| tools_dir().join("partitions-hub.csv"));
    let script = tools_dir().join("dev-board.sh");
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
            ExitCode::FAILURE
        }
    }
}
