//! Library tests. Everything runs in fresh temporary directories with test
//! masters; no board is touched (boards are fakes or [`DryRun`]).

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A temporary directory removed (with its contents) on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "link-key-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// Deterministic "random" bytes: 0x11, 0x12, ... (never all zero).
fn counter_rng() -> impl FnMut(&mut [u8]) -> std::io::Result<()> {
    let mut n = 0x10u8;
    move |b: &mut [u8]| {
        for x in b {
            n = n.wrapping_add(1);
            *x = n;
        }
        Ok(())
    }
}

/// The hex of the master the counter rng makes first.
fn counter_master_hex() -> String {
    (0x11u8..0x31).map(|b| format!("{b:02x}")).collect()
}

const NODE_CSV: &str = "# Name, Type, SubType, Offset, Size\n\
nvs,      data, nvs,       0x9000,   0x4000\n\
rsjob,    data, undefined, 0x610000, 0x1000\n\
rskey,    data, 0x40,      0x611000, 0x1000\n";
const HUB_CSV: &str = "nvs, data, nvs, 0x9000, 0x6000\n\
rskit, data, 0x41, 0x3f0000, 8K\n\
rsboot, data, 0x42, 0x3f2000, 0x2000\n";

struct Kit {
    dir: TempDir,
    node_csv: PathBuf,
    hub_csv: PathBuf,
}

fn kit() -> Kit {
    let dir = TempDir::new();
    kit_new(&dir.0, false, &mut counter_rng()).unwrap();
    let node_csv = dir.0.join("node.csv");
    let hub_csv = dir.0.join("hub.csv");
    std::fs::write(&node_csv, NODE_CSV).unwrap();
    std::fs::write(&hub_csv, HUB_CSV).unwrap();
    Kit { dir, node_csv, hub_csv }
}

/// What a fake board saw for one write.
struct Seen {
    board: String,
    offset: u32,
    path: PathBuf,
    bytes: Vec<u8>,
    file_mode: u32,
    dir_mode: u32,
    /// A hard link to the same inode, kept to check it was overwritten.
    link: PathBuf,
}

struct Fake {
    mac: Mac,
    fail: bool,
    seen: Vec<Seen>,
    links: TempDir,
}

impl Fake {
    fn new(mac: Mac) -> Self {
        Fake { mac, fail: false, seen: Vec::new(), links: TempDir::new() }
    }
}

impl Board for Fake {
    fn read_mac(&mut self, _board: &str) -> Result<Mac, String> {
        Ok(self.mac)
    }

    fn write_bin(&mut self, board: &str, offset: u32, file: &Path) -> Result<(), String> {
        let link = self.links.0.join(format!("{}", self.seen.len()));
        std::fs::hard_link(file, &link).unwrap();
        self.seen.push(Seen {
            board: board.into(),
            offset,
            path: file.into(),
            bytes: std::fs::read(file).unwrap(),
            file_mode: mode(file),
            dir_mode: mode(file.parent().unwrap()),
            link,
        });
        if self.fail { Err("board went away".into()) } else { Ok(()) }
    }
}

fn ctx<'a>(k: &Kit, board: &'a mut dyn Board, rng: Rng<'a>, dry_run: bool) -> Ctx<'a> {
    Ctx { kit_dir: k.dir.0.clone(), tmp_parent: k.dir.0.clone(), board, dry_run, rng }
}

/// The key image is gone and its inode was overwritten with zeros.
fn assert_wiped(s: &Seen) {
    assert!(!s.path.exists(), "{} still exists", s.path.display());
    assert!(!s.path.parent().unwrap().exists(), "private dir still exists");
    let after = std::fs::read(&s.link).unwrap();
    assert_eq!(after.len(), s.bytes.len());
    assert!(after.iter().all(|&b| b == 0), "key image not overwritten");
}

/// No secret in `s`: not the master, and no long hex run at all (kit ids
/// and key checks are 8 digits; keys and images would be 64+).
fn assert_no_secret(s: &str) {
    assert!(!s.contains(&counter_master_hex()), "{s}");
    let mut run = 0;
    for c in s.chars() {
        run = if c.is_ascii_hexdigit() { run + 1 } else { 0 };
        assert!(run < 16, "long hex run in: {s}");
    }
}

const MAC1: Mac = [0x24, 0x58, 0x7c, 0x01, 0x02, 0x03];
const MAC2: Mac = [0x24, 0x58, 0x7c, 0x01, 0x02, 0x04];

// ---------------------------------------------------------------- kit file --

#[test]
fn kit_new_writes_private_files_and_refuses_overwrite() {
    let k = kit();
    let key = k.dir.0.join(KIT_FILE);
    assert_eq!(mode(&key), 0o600);
    let text = std::fs::read_to_string(&key).unwrap();
    assert!(text.contains(&counter_master_hex()));
    let rf = load_roster(&k.dir.0, None).unwrap();
    assert!(rf.roster.is_empty());
    assert_eq!(rf.kit_id, load_master(&k.dir.0).unwrap().kit_id());

    // A second `kit new` refuses and leaves the file alone.
    let mut other = |b: &mut [u8]| {
        b.fill(0x77);
        Ok(())
    };
    let e = kit_new(&k.dir.0, false, &mut other).unwrap_err();
    assert!(e.contains("already exists") && e.contains("--force"), "{e}");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), text);
    // Also when only the roster is there.
    std::fs::remove_file(&key).unwrap();
    assert!(kit_new(&k.dir.0, false, &mut other).is_err());
    assert!(!key.exists());

    // --force replaces both, still 0600.
    let out = kit_new(&k.dir.0, true, &mut other).unwrap();
    assert_eq!(mode(&key), 0o600);
    assert!(std::fs::read_to_string(&key).unwrap().contains(&"77".repeat(32)));
    assert_no_secret(&out);
    assert!(!out.contains(&"77".repeat(32)));
}

#[test]
fn kit_new_skips_an_all_zero_master() {
    let d = TempDir::new();
    let mut calls = 0;
    let mut rng = |b: &mut [u8]| {
        calls += 1;
        b.fill(if calls == 1 { 0 } else { 5 });
        Ok(())
    };
    kit_new(&d.0, false, &mut rng).unwrap();
    assert!(load_master(&d.0).is_ok());
}

#[test]
fn roster_file_round_trip() {
    let k = kit();
    let mut b = Fake::new(MAC1);
    let mut rng = counter_rng();
    let mut c = ctx(&k, &mut b, &mut rng, false);
    let o = NodeOpts { label: Some("roof mast".into()), ..Default::default() };
    node(&mut c, "c5a", &k.node_csv, &o).unwrap();
    let rf = load_roster(&k.dir.0, None).unwrap();
    let text = rf.to_text();
    assert!(text.contains("0 1 24:58:7c:01:02:03 "), "{text}");
    assert!(text.contains(" live roof mast"), "{text}");
    assert_eq!(RosterFile::parse(&text).unwrap(), rf);
    assert!(RosterFile::parse("3 1 24:58:7c:01:02:03 00000000 live x").is_err());
    assert!(RosterFile::parse("kit 01020304\n3 1 24:58:7c:01:02:03 00000000 maybe x").is_err());
    assert!(RosterFile::parse("kit 01020304\n3 1 24:58:7c:01:02:03 0000 live").is_err());
    assert!(RosterFile::parse("kit 01020304\n3 1 a 00000000 live\n3 2 a 00000000 live").is_err());
}

#[test]
fn partition_lookup() {
    assert_eq!(find_partition(NODE_CSV, "rskey"), Some((0x611000, 0x1000)));
    assert_eq!(find_partition(HUB_CSV, "rskit"), Some((0x3f0000, 0x2000)));
    assert_eq!(find_partition(NODE_CSV, "rskit"), None);
    // The real tables (package 13g added rskey to both).
    let tools = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    assert_eq!(partition_offset(&tools.join("partitions-node.csv"), "rskey", 0x1000), Ok(0x611000));
    assert_eq!(partition_offset(&tools.join("partitions-single.csv"), "rskey", 0x1000), Ok(0x3f0000));
    // A table without one is refused.
    let d = TempDir::new();
    let t = d.0.join("none.csv");
    std::fs::write(&t, "nvs, data, nvs, 0x9000, 0x6000\n").unwrap();
    let e = partition_offset(&t, "rskey", 0x1000).unwrap_err();
    assert!(e.contains("no `rskey`"), "{e}");
    // Wrong size is refused.
    let d = TempDir::new();
    let t = d.0.join("t.csv");
    std::fs::write(&t, "rskey, data, 0x40, 0x611000, 0x2000\n").unwrap();
    assert!(partition_offset(&t, "rskey", 0x1000).unwrap_err().contains("expected 0x1000"));
}

// -------------------------------------------------------------- provision --

#[test]
fn node_writes_a_keyed_record_through_a_private_file() {
    let k = kit();
    let mut b = Fake::new(MAC1);
    let mut rng = counter_rng();
    let out = node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &NodeOpts::default()).unwrap();
    assert_no_secret(&out);
    assert!(out.contains("slot 0 gen 1"), "{out}");
    assert_eq!(b.seen.len(), 1);
    let s = &b.seen[0];
    assert_eq!((s.board.as_str(), s.offset), ("c5a", 0x611000));
    assert_eq!((s.file_mode, s.dir_mode), (0o600, 0o700));
    assert!(s.path.starts_with(&k.dir.0));
    assert_eq!(s.bytes.len(), 0x1000);
    let kit_id = load_master(&k.dir.0).unwrap().kit_id();
    match record::read(&s.bytes, &MAC1) {
        record::State::Keyed(r) => {
            assert_eq!((r.ident.slot, r.ident.generation, r.kit_id), (0, 1, kit_id));
            let e = load_roster(&k.dir.0, None).unwrap().roster.get(0).copied().unwrap();
            assert_eq!(e.key_check, r.key_check);
            assert!(out.contains(&r.key_check.to_string()));
        }
        st => panic!("{st:?}"),
    }
    assert_eq!(record::read(&s.bytes, &MAC2).name(), "wrong_board");
    assert_wiped(s);
}

#[test]
fn node_slots_and_generations() {
    let k = kit();
    let mut rng = counter_rng();
    let mut b1 = Fake::new(MAC1);
    node(&mut ctx(&k, &mut b1, &mut rng, false), "c5a", &k.node_csv, &NodeOpts::default()).unwrap();
    let mut b2 = Fake::new(MAC2);
    node(&mut ctx(&k, &mut b2, &mut rng, false), "c5b", &k.node_csv, &NodeOpts::default()).unwrap();
    // The same board again without --slot is refused.
    let e = node(&mut ctx(&k, &mut b2, &mut rng, false), "c5b", &k.node_csv, &NodeOpts::default()).unwrap_err();
    assert!(e.contains("already in slot 1"), "{e}");
    // A replacement board for slot 0: generation 2.
    let mac3 = [0x24, 0x58, 0x7c, 9, 9, 9];
    let mut b3 = Fake::new(mac3);
    let o = NodeOpts { slot: Some(0), ..Default::default() };
    let out = node(&mut ctx(&k, &mut b3, &mut rng, false), "c5c", &k.node_csv, &o).unwrap();
    assert!(out.contains("slot 0 gen 2"), "{out}");
    let rf = load_roster(&k.dir.0, None).unwrap();
    assert_eq!(rf.roster.get(0).unwrap().mac, mac3);
    assert_eq!(rf.roster.get(1).unwrap().mac, MAC2);
    // The old board's record is not the new one's.
    assert_ne!(b1.seen[0].bytes, b3.seen[0].bytes);
}

#[test]
fn node_mac_mismatch_and_failed_write() {
    let k = kit();
    let mut rng = counter_rng();
    let mut b = Fake::new(MAC1);
    let o = NodeOpts { mac: Some(MAC2), ..Default::default() };
    let e = node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &o).unwrap_err();
    assert!(e.contains("reports MAC"), "{e}");
    assert!(b.seen.is_empty());
    assert!(load_roster(&k.dir.0, None).unwrap().roster.is_empty());

    // A failed write still wipes and removes the key image. The roster
    // keeps the generation (it may be on the board).
    b.fail = true;
    let e = node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &NodeOpts::default()).unwrap_err();
    assert!(e.contains("board went away"));
    assert_wiped(&b.seen[0]);
    assert_eq!(load_roster(&k.dir.0, None).unwrap().roster.get(0).unwrap().generation, 1);
}

#[test]
fn node_dry_run_prints_commands_and_keeps_the_roster() {
    let k = kit();
    let mut rng = counter_rng();
    let mut d = DryRun { script: "tools/dev-board.sh".into(), log: Vec::new() };
    // Without --mac a dry run cannot go on.
    let e = node(&mut ctx(&k, &mut d, &mut rng, true), "c5a", &k.node_csv, &NodeOpts::default()).unwrap_err();
    assert!(e.contains("--mac"), "{e}");
    assert!(d.log[0].contains("espflash board-info"), "{:?}", d.log);
    d.log.clear();
    let o = NodeOpts { mac: Some(MAC1), ..Default::default() };
    let out = node(&mut ctx(&k, &mut d, &mut rng, true), "c5a", &k.node_csv, &o).unwrap();
    assert_eq!(d.log.len(), 1);
    let line = &d.log[0];
    assert!(line.starts_with("[dry-run] tools/dev-board.sh '--run' 'espflash write-bin"), "{line}");
    assert!(line.contains("0x611000") && line.contains("rskey.bin") && line.contains("'c5a'"), "{line}");
    assert!(line.contains(&k.dir.0.display().to_string()), "key file path shown: {line}");
    assert_no_secret(line);
    assert_no_secret(&out);
    assert!(out.contains("roster not changed"));
    assert!(load_roster(&k.dir.0, None).unwrap().roster.is_empty());
    // The private directory is gone again.
    let left: Vec<_> = std::fs::read_dir(&k.dir.0)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("link-key-"))
        .collect();
    assert!(left.is_empty());
    // dev-board.sh never gets asked to erase anything.
    assert!(!line.contains("erase"));
}

#[test]
fn node_bench_corrupt_reads_damaged() {
    let k = kit();
    let mut rng = counter_rng();
    let mut b = Fake::new(MAC1);
    let o = NodeOpts { bench_corrupt: true, ..Default::default() };
    node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &o).unwrap();
    assert_eq!(record::read(&b.seen[0].bytes, &MAC1).name(), "damaged");
}

#[test]
fn hub_writes_master_roster_and_boot_seed() {
    let k = kit();
    let mut rng = counter_rng();
    let mut nb = Fake::new(MAC1);
    node(&mut ctx(&k, &mut nb, &mut rng, false), "c5a", &k.node_csv, &NodeOpts::default()).unwrap();
    let mut b = Fake::new([0; 6]);
    let out = hub(&mut ctx(&k, &mut b, &mut rng, false), "p4", &k.hub_csv, false).unwrap();
    assert_no_secret(&out);
    assert!(out.contains("captured hub is a captured kit"));
    assert_eq!(b.seen.len(), 2);
    let (kit_img, boot) = (&b.seen[0], &b.seen[1]);
    assert_eq!((kit_img.offset, boot.offset), (0x3f0000, 0x3f2000));
    let kit_id = load_master(&k.dir.0).unwrap().kit_id();
    match hub::read_master(&kit_img.bytes) {
        hub::MasterState::Good(hub::Master::Keyed { kit_id: id, master: Some(m) }) => {
            assert_eq!(id, kit_id);
            assert_eq!(m.kit_id(), kit_id);
        }
        s => panic!("{s:?}"),
    }
    let hub::RosterState::Good(r) = hub::read_roster(&kit_img.bytes[0x1000..], kit_id) else { panic!() };
    assert_eq!(r, load_roster(&k.dir.0, None).unwrap().roster);
    assert!(matches!(bootlog::read(&boot.bytes), bootlog::State::Good { .. }));
    assert_wiped(kit_img);
    assert_wiped(boot);

    // --keep-boot leaves rsboot alone.
    let mut b2 = Fake::new([0; 6]);
    hub(&mut ctx(&k, &mut b2, &mut rng, false), "p4", &k.hub_csv, true).unwrap();
    assert_eq!(b2.seen.len(), 1);
}

#[test]
fn hub_roster_never_sends_the_master() {
    let k = kit();
    let mut rng = counter_rng();
    let mut nb = Fake::new(MAC1);
    node(&mut ctx(&k, &mut nb, &mut rng, false), "c5a", &k.node_csv, &NodeOpts::default()).unwrap();
    let master: Vec<u8> = (0x11u8..0x31).collect();
    let mut b = Fake::new([0; 6]);
    let out = hub_roster(&mut ctx(&k, &mut b, &mut rng, false), "p4", &k.hub_csv).unwrap();
    assert!(out.contains("the master was not sent"));
    assert_eq!(b.seen.len(), 1);
    let s = &b.seen[0];
    assert_eq!(s.offset, 0x3f1000);
    assert_eq!(s.bytes.len(), 0x1000);
    assert!(!s.bytes.windows(32).any(|w| w == master.as_slice()));
    let kit_id = load_roster(&k.dir.0, None).unwrap().kit_id;
    assert!(matches!(hub::read_roster(&s.bytes, kit_id), hub::RosterState::Good(r) if r.len() == 1));
    // hub-roster works without kit.key (it never needs the master).
    std::fs::remove_file(k.dir.0.join(KIT_FILE)).unwrap();
    hub_roster(&mut ctx(&k, &mut b, &mut rng, false), "p4", &k.hub_csv).unwrap();

    // node --hub pushes the roster right after the node.
    let k = kit();
    let mut b = Fake::new(MAC1);
    let o = NodeOpts { hub: Some("p4".into()), hub_table: Some(k.hub_csv.clone()), ..Default::default() };
    node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &o).unwrap();
    assert_eq!(b.seen.iter().map(|s| (s.board.as_str(), s.offset)).collect::<Vec<_>>(), [("c5a", 0x611000), ("p4", 0x3f1000)]);
}

#[test]
fn open_records() {
    let k = kit();
    let mut rng = counter_rng();
    let mut b = Fake::new(MAC1);
    open(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, false, false).unwrap();
    assert_eq!(record::read(&b.seen[0].bytes, &MAC1).name(), "open");
    let mut h = Fake::new([0; 6]);
    open(&mut ctx(&k, &mut h, &mut rng, false), "p4", &k.hub_csv, true, false).unwrap();
    assert_eq!(hub::read_master(&h.seen[0].bytes).name(), "open");
    assert_eq!(h.seen.len(), 2);
    // Opening needs no kit at all.
    let empty = TempDir::new();
    let mut c = Ctx { kit_dir: empty.0.clone(), tmp_parent: empty.0.clone(), board: &mut b, dry_run: false, rng: &mut rng };
    open(&mut c, "c5a", &k.node_csv, false, false).unwrap();
}

#[test]
fn revoke_and_show() {
    let k = kit();
    let mut rng = counter_rng();
    let mut b = Fake::new(MAC1);
    let o = NodeOpts { label: Some("north".into()), ..Default::default() };
    node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &o).unwrap();
    revoke(&k.dir.0, 0).unwrap();
    assert!(revoke(&k.dir.0, 5).unwrap_err().contains("empty"));
    let s = show(&k.dir.0).unwrap();
    assert_no_secret(&s);
    let rf = load_roster(&k.dir.0, None).unwrap();
    assert!(s.contains(&format!("kit {}", rf.kit_id)));
    assert!(s.contains("24:58:7c:01:02:03") && s.contains("revoked") && s.contains("north"), "{s}");
    assert!(s.contains(&rf.roster.get(0).unwrap().key_check.to_string()));
    assert!(s.contains("next free slot: 1"));
    // Re-provision slot 0: generation 2, live again.
    let o = NodeOpts { slot: Some(0), ..Default::default() };
    node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &o).unwrap();
    let e = *load_roster(&k.dir.0, None).unwrap().roster.get(0).unwrap();
    assert_eq!((e.generation, e.revoked), (2, false));
}

#[test]
fn roster_of_another_kit_is_refused() {
    let k = kit();
    let mut other = |b: &mut [u8]| {
        b.fill(0x42);
        Ok(())
    };
    let d = TempDir::new();
    kit_new(&d.0, false, &mut other).unwrap();
    std::fs::copy(d.0.join(ROSTER_FILE), k.dir.0.join(ROSTER_FILE)).unwrap();
    let mut rng = counter_rng();
    let mut b = Fake::new(MAC1);
    let e = node(&mut ctx(&k, &mut b, &mut rng, false), "c5a", &k.node_csv, &NodeOpts::default()).unwrap_err();
    assert!(e.contains("belongs to kit"), "{e}");
    assert!(show(&k.dir.0).unwrap().contains("WARNING"));
}

// ------------------------------------------------------------ board glue --

#[test]
fn board_info_mac_is_parsed() {
    let out = "[2026-11-18T10:00:00Z INFO ] Serial port: '/dev/ttyACM0'\n\
               Chip type:         esp32c5 (revision v1.0)\n\
               Crystal frequency: 48 MHz\n\
               Flash size:        8MB\n\
               Features:          WiFi 6, BT 5\n\
               MAC address:       3c:dc:75:aa:bb:cc\n";
    assert_eq!(parse_board_info_mac(out), Some([0x3c, 0xdc, 0x75, 0xaa, 0xbb, 0xcc]));
    assert_eq!(parse_board_info_mac("Chip type: esp32\n"), None);
}

#[test]
fn write_command_quotes_the_path() {
    let a = write_bin_args("c5a", 0x611000, Path::new("/tmp/it's here/rskey.bin"));
    assert_eq!(a[0], "--run");
    assert_eq!(a[1], r#"espflash write-bin --port "$BOARD_PORT" --non-interactive 0x611000 '/tmp/it'\''s here/rskey.bin'"#);
    assert_eq!(&a[2..], ["120", "c5a"]);
}

#[test]
fn secret_file_lifecycle() {
    let d = TempDir::new();
    let sd = SecretDir::new(&d.0).unwrap();
    assert_eq!(mode(sd.path()), 0o700);
    let f = sd.write("x.bin", &[7u8; 100]).unwrap();
    assert_eq!(mode(f.path()), 0o600);
    // Refuses to reuse a name (create_new): nothing pre-placed is written through.
    assert!(sd.write("x.bin", &[1]).is_err());
    let link = d.0.join("link");
    std::fs::hard_link(f.path(), &link).unwrap();
    let (p, dir) = (f.path().to_path_buf(), sd.path().to_path_buf());
    drop(f);
    drop(sd);
    assert!(!p.exists() && !dir.exists());
    assert_eq!(std::fs::read(&link).unwrap(), vec![0u8; 100]);
}
