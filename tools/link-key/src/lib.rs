//! Per-node link keys on the computer (docs/LINK-KEYS-DESIGN.md 4.5): the
//! kit file, the roster file, partition images, and writing them to boards.
//!
//! Files (both git-ignored, in `tools/` unless `--kit-dir` says otherwise):
//! - `kit.key`: the kit master, 0600. Secret. Never printed.
//! - `kit.roster`: slot, generation, MAC, key check, state, label. No secrets.
//!
//! Every board access goes through a [`Board`]: [`DevBoard`] runs
//! `tools/dev-board.sh --run '<espflash ...>'` (which holds the board's lab
//! lock), [`DryRun`] only prints what it would run. A partition image that
//! holds a key goes to a 0600 file in a private 0700 directory
//! ([`SecretDir`]); `espflash write-bin` reads it from there, and the file is
//! overwritten with zeros and removed as soon as the write returns (also on
//! errors). Only its path is ever shown.

use kitkeys::roster::{Entry, Roster};
use kitkeys::{KeyCheck, KitId, Mac, MasterKey, SLOTS, SoftKeySource, bootlog, hub, record};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const KIT_FILE: &str = "kit.key";
pub const ROSTER_FILE: &str = "kit.roster";
pub const ROSTER_HEADER: &str = "# rotorscope kit roster v1 (no secrets; the master is in kit.key)";

/// Source of OS randomness (`/dev/urandom` in the binary; fixed in tests).
pub type Rng<'a> = &'a mut dyn FnMut(&mut [u8]) -> std::io::Result<()>;

pub fn os_random(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

// ------------------------------------------------------------- formatting --

pub fn fmt_mac(m: &Mac) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

pub fn parse_mac(s: &str) -> Result<Mac, String> {
    let parts: Vec<&str> = s.split([':', '-']).collect();
    let mut m = [0u8; 6];
    if parts.len() != 6 {
        return Err(format!("{s}: not a MAC address (aa:bb:cc:dd:ee:ff)"));
    }
    for (o, p) in m.iter_mut().zip(parts) {
        *o = u8::from_str_radix(p, 16).map_err(|_| format!("{s}: not a MAC address"))?;
    }
    Ok(m)
}

fn parse_hex4(s: &str) -> Option<[u8; 4]> {
    if s.len() != 8 {
        return None;
    }
    let mut o = [0u8; 4];
    for (i, b) in o.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(o)
}

// -------------------------------------------------------------- kit files --

fn write_private(path: &Path, bytes: &[u8], create_new: bool) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).mode(0o600);
    if create_new {
        opts.create_new(true);
    } else {
        opts.create(true).truncate(true);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Replace `path` atomically with a 0600 file holding `bytes`.
fn replace_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let tmp = path.with_file_name(name);
    let _ = std::fs::remove_file(&tmp);
    write_private(&tmp, bytes, true)?;
    std::fs::rename(&tmp, path)
}

/// `link-key kit new`: a new master in `dir/kit.key` (0600) and an empty
/// roster. Refuses to replace either without `force`.
pub fn kit_new(dir: &Path, force: bool, rng: Rng) -> Result<String, String> {
    let key = dir.join(KIT_FILE);
    let roster = dir.join(ROSTER_FILE);
    if !force {
        for p in [&key, &roster] {
            if p.exists() {
                return Err(format!(
                    "{} already exists. Keep it: every keyed board of the kit depends on it. \
                     To start a NEW kit on purpose (every node and the hub must then be provisioned again): kit new --force",
                    p.display()
                ));
            }
        }
    }
    let master = loop {
        let mut b = [0u8; 32];
        rng(&mut b).map_err(|e| format!("random source: {e}"))?;
        if let Some(m) = MasterKey::from_bytes(b) {
            break m;
        }
    };
    let text = master.to_kit_file();
    let r = if force { replace_private(&key, text.as_bytes()) } else { write_private(&key, text.as_bytes(), true) };
    r.map_err(|e| format!("{}: {e}", key.display()))?;
    drop(text);
    let kit_id = master.kit_id();
    save_roster(&roster, &RosterFile::new(kit_id))?;
    Ok(format!(
        "kit {kit_id}: wrote {} (secret, 0600) and an empty {}.\n\
         Never commit or share kit.key; back it up somewhere private.\n\
         Next: link-key hub <hub-board>, then link-key node <board> for each node.",
        key.display(),
        roster.display()
    ))
}

/// Read the kit master. Warns (on stderr) if the file is readable by others.
pub fn load_master(dir: &Path) -> Result<MasterKey, String> {
    let path = dir.join(KIT_FILE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(&path)
            && md.permissions().mode() & 0o077 != 0
        {
            eprintln!("link-key: WARNING: {} is readable by other users; chmod 600 it", path.display());
        }
    }
    let mut text = std::fs::read(&path).map_err(|e| format!("{}: {e} (make a kit with: link-key kit new)", path.display()))?;
    let r = MasterKey::from_kit_file(&text).map_err(|e| format!("{}: not a kit file ({e:?})", path.display()));
    text.fill(0);
    std::hint::black_box(&text);
    r
}

/// The roster file: the kit it belongs to, the roster, a label per slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterFile {
    pub kit_id: KitId,
    pub roster: Roster,
    pub labels: [String; SLOTS],
}

impl RosterFile {
    pub fn new(kit_id: KitId) -> Self {
        RosterFile { kit_id, roster: Roster::new(), labels: Default::default() }
    }

    pub fn to_text(&self) -> String {
        let mut s = format!("{ROSTER_HEADER}\nkit {}\n# slot gen mac key_check state label\n", self.kit_id);
        for e in self.roster.entries() {
            let _ = writeln!(
                s,
                "{} {} {} {} {} {}",
                e.slot,
                e.generation,
                fmt_mac(&e.mac),
                e.key_check,
                if e.revoked { "revoked" } else { "live" },
                self.labels[usize::from(e.slot)]
            );
        }
        s
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut kit_id = None;
        let mut out: Option<RosterFile> = None;
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = || format!("line {}: {line:?}", n + 1);
            if let Some(k) = line.strip_prefix("kit ") {
                if kit_id.is_some() {
                    return Err(bad());
                }
                let k = KitId(parse_hex4(k.trim()).ok_or_else(bad)?);
                kit_id = Some(k);
                out = Some(RosterFile::new(k));
                continue;
            }
            let rf = out.as_mut().ok_or_else(|| format!("line {}: entry before the `kit` line", n + 1))?;
            let mut f = line.splitn(6, ' ');
            let mut next = || f.next().ok_or_else(bad);
            let slot: u8 = next()?.parse().map_err(|_| bad())?;
            let generation: u16 = next()?.parse().map_err(|_| bad())?;
            let mac = parse_mac(next()?).map_err(|_| bad())?;
            let key_check = KeyCheck(parse_hex4(next()?).ok_or_else(bad)?);
            let revoked = match next()? {
                "live" => false,
                "revoked" => true,
                _ => return Err(bad()),
            };
            let label = f.next().unwrap_or("").to_string();
            rf.roster
                .insert(Entry { slot, generation, mac, key_check, revoked })
                .map_err(|e| format!("line {}: {e}", n + 1))?;
            rf.labels[usize::from(slot)] = label;
        }
        out.ok_or_else(|| "no `kit` line".into())
    }
}

pub fn load_roster(dir: &Path, kit_id: Option<KitId>) -> Result<RosterFile, String> {
    let path = dir.join(ROSTER_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rf = RosterFile::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(k) = kit_id
        && rf.kit_id != k
    {
        return Err(format!("{} belongs to kit {}, but kit.key is kit {k}", path.display(), rf.kit_id));
    }
    Ok(rf)
}

pub fn save_roster(path: &Path, rf: &RosterFile) -> Result<(), String> {
    replace_private(path, rf.to_text().as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

// ------------------------------------------------------- partition tables --

/// Offset and size of partition `name` in an ESP-IDF partition CSV.
pub fn find_partition(csv: &str, name: &str) -> Option<(u32, u32)> {
    fn num(s: &str) -> Option<u32> {
        let s = s.trim();
        if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            return u32::from_str_radix(h, 16).ok();
        }
        if let Some(k) = s.strip_suffix(['K', 'k']) {
            return k.parse::<u32>().ok()?.checked_mul(1024);
        }
        if let Some(m) = s.strip_suffix(['M', 'm']) {
            return m.parse::<u32>().ok()?.checked_mul(1024 * 1024);
        }
        s.parse().ok()
    }
    csv.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).find_map(|l| {
        let f: Vec<&str> = l.split(',').map(str::trim).collect();
        (f.len() >= 5 && f[0] == name).then(|| Some((num(f[3])?, num(f[4])?))).flatten()
    })
}

/// The offset of `name` in `table`, which must have exactly `size` bytes.
pub fn partition_offset(table: &Path, name: &str, size: usize) -> Result<u32, String> {
    let csv = std::fs::read_to_string(table).map_err(|e| format!("{}: {e}", table.display()))?;
    let (off, sz) = find_partition(&csv, name)
        .ok_or_else(|| format!("{}: no `{name}` partition (with offset and size) in this table", table.display()))?;
    if sz as usize != size {
        return Err(format!("{}: `{name}` is {sz:#x} bytes, expected {size:#x}", table.display()));
    }
    Ok(off)
}

// -------------------------------------------------------- secret temp files --

/// A private directory (0700, random name) for key images. Removed on drop.
pub struct SecretDir {
    path: PathBuf,
}

impl SecretDir {
    pub fn new(parent: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let mut r = [0u8; 6];
        os_random(&mut r)?;
        let name: String = r.iter().map(|b| format!("{b:02x}")).collect();
        let path = parent.join(format!("link-key-{}-{name}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(SecretDir { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write `bytes` to a new 0600 file `name` here.
    pub fn write(&self, name: &str, bytes: &[u8]) -> std::io::Result<SecretFile> {
        let path = self.path.join(name);
        write_private(&path, bytes, true)?;
        Ok(SecretFile { path, len: bytes.len() })
    }
}

impl Drop for SecretDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// A file holding a key image: overwritten with zeros (same inode, synced)
/// and removed when dropped.
pub struct SecretFile {
    path: PathBuf,
    len: usize,
}

impl SecretFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&self.path) {
            let _ = f.write_all(&vec![0u8; self.len]).and_then(|()| f.sync_all());
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

// ----------------------------------------------------------------- boards --

/// Everything that touches a board.
pub trait Board {
    /// The board's station MAC (`espflash board-info`).
    fn read_mac(&mut self, board: &str) -> Result<Mac, String>;
    /// `espflash write-bin <offset> <file>`.
    fn write_bin(&mut self, board: &str, offset: u32, file: &Path) -> Result<(), String>;
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `dev-board.sh` arguments that read the board's MAC under its lock.
pub fn board_info_args(board: &str) -> Vec<String> {
    vec![
        "--run".into(),
        r#"espflash board-info --port "$BOARD_PORT" --non-interactive"#.into(),
        "60".into(),
        board.into(),
    ]
}

/// `dev-board.sh` arguments that write `file` at `offset` under the lock.
pub fn write_bin_args(board: &str, offset: u32, file: &Path) -> Vec<String> {
    vec![
        "--run".into(),
        format!(
            r#"espflash write-bin --port "$BOARD_PORT" --non-interactive {offset:#x} {}"#,
            shell_quote(&file.to_string_lossy())
        ),
        "120".into(),
        board.into(),
    ]
}

/// The station MAC in `espflash board-info` output.
pub fn parse_board_info_mac(out: &str) -> Option<Mac> {
    out.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if !k.trim().eq_ignore_ascii_case("MAC address") {
            return None;
        }
        parse_mac(v.trim()).ok()
    })
}

/// The real thing: `tools/dev-board.sh --run ...`. Not exercised by tests.
pub struct DevBoard {
    pub script: PathBuf,
}

impl Board for DevBoard {
    fn read_mac(&mut self, board: &str) -> Result<Mac, String> {
        let out = std::process::Command::new(&self.script)
            .args(board_info_args(board))
            .output()
            .map_err(|e| format!("{}: {e}", self.script.display()))?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            return Err(format!("reading {board}'s MAC failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        parse_board_info_mac(&text).ok_or_else(|| format!("no `MAC address:` line from espflash board-info on {board}"))
    }

    fn write_bin(&mut self, board: &str, offset: u32, file: &Path) -> Result<(), String> {
        let st = std::process::Command::new(&self.script)
            .args(write_bin_args(board, offset, file))
            .status()
            .map_err(|e| format!("{}: {e}", self.script.display()))?;
        if st.success() { Ok(()) } else { Err(format!("writing {board} at {offset:#x} failed ({st})")) }
    }
}

/// Prints the commands a [`DevBoard`] would run; touches nothing.
pub struct DryRun {
    pub script: PathBuf,
    pub log: Vec<String>,
}

impl DryRun {
    fn line(&mut self, args: Vec<String>) {
        let mut s = format!("[dry-run] {}", self.script.display());
        for a in args {
            s.push(' ');
            s.push_str(&shell_quote(&a));
        }
        self.log.push(s);
    }
}

impl Board for DryRun {
    fn read_mac(&mut self, board: &str) -> Result<Mac, String> {
        self.line(board_info_args(board));
        Err("--dry-run cannot read a MAC: pass --mac aa:bb:cc:dd:ee:ff".into())
    }

    fn write_bin(&mut self, board: &str, offset: u32, file: &Path) -> Result<(), String> {
        self.line(write_bin_args(board, offset, file));
        Ok(())
    }
}

// --------------------------------------------------------------- commands --

/// What the board-writing commands need.
pub struct Ctx<'a> {
    /// Holds `kit.key` and `kit.roster`.
    pub kit_dir: PathBuf,
    /// Parent of the private temporary directory.
    pub tmp_parent: PathBuf,
    pub board: &'a mut dyn Board,
    /// Do not change the roster file (the board is a [`DryRun`]).
    pub dry_run: bool,
    pub rng: Rng<'a>,
}

fn written(ctx: &Ctx) -> &'static str {
    if ctx.dry_run { "NOT written (dry run)" } else { "written" }
}

fn write_image(ctx: &mut Ctx, board: &str, offset: u32, name: &str, bytes: &[u8]) -> Result<String, String> {
    let dir = SecretDir::new(&ctx.tmp_parent).map_err(|e| format!("private temporary directory: {e}"))?;
    let file = dir.write(name, bytes).map_err(|e| format!("temporary file: {e}"))?;
    let r = ctx.board.write_bin(board, offset, file.path());
    let shown = file.path().display().to_string();
    drop(file); // overwritten and removed here, before anything else
    drop(dir);
    r.map(|()| shown)
}

/// Options of `link-key node`.
#[derive(Default)]
pub struct NodeOpts {
    pub slot: Option<u8>,
    /// Required with `--dry-run`; otherwise checked against the board.
    pub mac: Option<Mac>,
    pub label: Option<String>,
    /// Push the roster to this hub afterwards.
    pub hub: Option<String>,
    pub hub_table: Option<PathBuf>,
    /// Bench only: write the record with both CRCs broken (`damaged`).
    pub bench_corrupt: bool,
}

/// `link-key node <board>`: provision one node.
pub fn node(ctx: &mut Ctx, board: &str, table: &Path, o: &NodeOpts) -> Result<String, String> {
    let master = load_master(&ctx.kit_dir)?;
    let kit_id = master.kit_id();
    let mut rf = load_roster(&ctx.kit_dir, Some(kit_id))?;
    let offset = partition_offset(table, "rskey", record::PARTITION_LEN)?;
    let mac = match (o.mac, ctx.dry_run) {
        (Some(m), true) => m,
        (given, false) => {
            let m = ctx.board.read_mac(board)?;
            if let Some(g) = given
                && g != m
            {
                return Err(format!("{board} reports MAC {}, not {}", fmt_mac(&m), fmt_mac(&g)));
            }
            m
        }
        (None, true) => return Err(ctx.board.read_mac(board).unwrap_err()),
    };
    let mut src = SoftKeySource::new(master);
    let ident = match rf.roster.provision(&mut src, o.slot, mac) {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(never) => match never {},
    };
    if let Some(l) = &o.label {
        rf.labels[usize::from(ident.slot)] = l.replace(['\n', '\r'], " ");
    }
    let rec = match record::NodeRecord::provision(&mut src, ident) {
        Ok(r) => r,
        Err(never) => match never {},
    };
    drop(src);
    let check = rec.key_check;
    let mut bytes = record::encode(&rec);
    drop(rec);
    if o.bench_corrupt {
        bytes.0[60] ^= 0xFF;
    }
    let image = record::partition_image(&bytes);
    drop(bytes);
    let mut out = String::new();
    // The roster is saved before the board is written: a generation that
    // may exist on a board is never handed out again.
    let roster_path = ctx.kit_dir.join(ROSTER_FILE);
    if ctx.dry_run {
        let _ = writeln!(out, "[dry-run] roster not changed (would record slot {} gen {})", ident.slot, ident.generation);
    } else {
        save_roster(&roster_path, &rf)?;
    }
    let shown = write_image(ctx, board, offset, "rskey.bin", &image.0)?;
    drop(image);
    let _ = writeln!(
        out,
        "{board}: slot {} gen {} mac {} kit {kit_id} check {check}: {}{} (key image was {shown}, now removed)",
        ident.slot,
        ident.generation,
        fmt_mac(&mac),
        written(ctx),
        if o.bench_corrupt { " BENCH: written DAMAGED" } else { "" }
    );
    let _ = writeln!(out, "the board should print auth \"kit:{kit_id} slot:{} gen:{} check:{check}\"", ident.slot, ident.generation);
    match &o.hub {
        Some(h) => {
            let t = o.hub_table.as_deref().ok_or("--hub needs the hub's partition table")?;
            out.push_str(&push_roster(ctx, h, t, &rf)?);
        }
        None => {
            let _ = writeln!(out, "now push the roster to the hub: link-key hub-roster <hub-board>");
        }
    }
    Ok(out)
}

fn push_roster(ctx: &mut Ctx, board: &str, table: &Path, rf: &RosterFile) -> Result<String, String> {
    let offset = partition_offset(table, "rskit", hub::PARTITION_LEN)?;
    let sector = hub::roster_sector(&hub::encode_roster(rf.kit_id, &rf.roster));
    write_image(ctx, board, offset + hub::ROSTER_SECTOR as u32, "rskit-roster.bin", &sector)?;
    Ok(format!(
        "{board}: roster of kit {} ({} entries) {}; the master was not sent\n",
        rf.kit_id,
        rf.roster.len(),
        written(ctx)
    ))
}

/// `link-key hub-roster <hub-board>`: the roster only, never the master.
pub fn hub_roster(ctx: &mut Ctx, board: &str, table: &Path) -> Result<String, String> {
    // The kit id comes from the roster file: no need to read the master.
    let rf = load_roster(&ctx.kit_dir, None)?;
    push_roster(ctx, board, table, &rf)
}

fn seed_boot(ctx: &mut Ctx, board: &str, table: &Path) -> Result<String, String> {
    let offset = partition_offset(table, "rsboot", bootlog::PARTITION_LEN)?;
    let mut r = [0u8; 4];
    (ctx.rng)(&mut r).map_err(|e| format!("random source: {e}"))?;
    let seed = bootlog::seed_from_random(r);
    write_image(ctx, board, offset, "rsboot.bin", &bootlog::seed_image(seed))?;
    Ok(format!("{board}: boot counter seed {}\n", written(ctx)))
}

/// `link-key hub <hub-board>`: master and roster to `rskit`, boot counter
/// seeded (unless `keep_boot`).
pub fn hub(ctx: &mut Ctx, board: &str, table: &Path, keep_boot: bool) -> Result<String, String> {
    let master = load_master(&ctx.kit_dir)?;
    let kit_id = master.kit_id();
    let rf = load_roster(&ctx.kit_dir, Some(kit_id))?;
    let offset = partition_offset(table, "rskit", hub::PARTITION_LEN)?;
    if !keep_boot {
        partition_offset(table, "rsboot", bootlog::PARTITION_LEN)?;
    }
    let rec = hub::encode_master_keyed(Some(&master), KitId::default());
    drop(master);
    let image = hub::kit_image(&rec, &hub::encode_roster(kit_id, &rf.roster));
    drop(rec);
    let shown = write_image(ctx, board, offset, "rskit.bin", &image.0)?;
    drop(image);
    let mut out = format!(
        "{board}: kit {kit_id} master and roster ({} entries) {} (key image was {shown}, now removed)\n",
        rf.roster.len(),
        written(ctx)
    );
    if !keep_boot {
        out.push_str(&seed_boot(ctx, board, table)?);
    }
    out.push_str("Until a secure element is fitted, a captured hub is a captured kit.\n");
    Ok(out)
}

/// `link-key open <board> [--hub]`: an open record (unauthenticated link).
pub fn open(ctx: &mut Ctx, board: &str, table: &Path, is_hub: bool, keep_boot: bool) -> Result<String, String> {
    if is_hub {
        let offset = partition_offset(table, "rskit", hub::PARTITION_LEN)?;
        let image = hub::kit_image(&hub::encode_master_open(), &hub::encode_roster(KitId::default(), &Roster::new()));
        write_image(ctx, board, offset, "rskit.bin", &image.0)?;
        let mut out = format!("{board}: hub record OPEN {}: the ESP-NOW link is NOT authenticated\n", written(ctx));
        if !keep_boot {
            out.push_str(&seed_boot(ctx, board, table)?);
        }
        Ok(out)
    } else {
        let offset = partition_offset(table, "rskey", record::PARTITION_LEN)?;
        let image = record::partition_image(&record::encode_open());
        write_image(ctx, board, offset, "rskey.bin", &image.0)?;
        Ok(format!(
            "{board}: node record OPEN {}: its ESP-NOW link is NOT authenticated.\n\
             If this board is in the roster, revoke its slot (link-key revoke --slot N).\n",
            written(ctx)
        ))
    }
}

/// `link-key revoke --slot N`.
pub fn revoke(kit_dir: &Path, slot: u8) -> Result<String, String> {
    let mut rf = load_roster(kit_dir, None)?;
    rf.roster.revoke(slot).map_err(|e| e.to_string())?;
    save_roster(&kit_dir.join(ROSTER_FILE), &rf)?;
    Ok(format!("slot {slot} revoked. Now: link-key hub-roster <hub-board>\n"))
}

/// `link-key show`: kit id and roster with key checks. Nothing secret.
pub fn show(kit_dir: &Path) -> Result<String, String> {
    let rf = load_roster(kit_dir, None)?;
    let mut s = String::new();
    // The kit id from kit.key, if it is there, must agree with the roster.
    let key_path = kit_dir.join(KIT_FILE);
    match load_master(kit_dir) {
        Ok(m) if m.kit_id() != rf.kit_id => {
            let _ = writeln!(s, "WARNING: kit.key is kit {}, the roster is kit {}", m.kit_id(), rf.kit_id);
        }
        Ok(_) => {}
        Err(_) => {
            let _ = writeln!(s, "(no readable {}; showing the roster only)", key_path.display());
        }
    }
    let _ = writeln!(s, "kit {}", rf.kit_id);
    let _ = writeln!(s, "slot gen   mac               check    state    label");
    for e in rf.roster.entries() {
        let _ = writeln!(
            s,
            "{:<4} {:<5} {} {} {:<8} {}",
            e.slot,
            e.generation,
            fmt_mac(&e.mac),
            e.key_check,
            if e.revoked { "revoked" } else { "live" },
            rf.labels[usize::from(e.slot)]
        );
    }
    match rf.roster.next_free_slot() {
        Some(n) => {
            let _ = writeln!(s, "next free slot: {n}");
        }
        None => {
            let _ = writeln!(s, "no free slot");
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests;
