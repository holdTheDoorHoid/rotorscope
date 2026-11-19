//! The real binary, run in temporary directories with --kit-dir and
//! --dry-run only: no board, and never the repository's tools/ directory.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("link-key-cli-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
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

/// Run link-key with its private temporary directory under `tmp`.
fn run(tmp: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_link-key"))
        .args(args)
        .env("TMPDIR", tmp)
        // The runtime directory is preferred when set: keep the test in its own.
        .env("XDG_RUNTIME_DIR", tmp)
        .env_remove("ROTORSCOPE_KIT_DIR")
        .env_remove("ROTORSCOPE_BENCH")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)
}

/// The master's 64 hex digits as written in kit.key.
fn master_hex(dir: &Path) -> String {
    let t = std::fs::read_to_string(dir.join("kit.key")).unwrap();
    let h = t.lines().nth(1).unwrap().trim().to_string();
    assert_eq!(h.len(), 64);
    h
}

fn assert_no_secret(s: &str, master: &str) {
    assert!(!s.contains(master), "master printed: {s}");
    assert!(!s.to_ascii_uppercase().contains(&master.to_ascii_uppercase()));
    let mut run = 0;
    for c in s.chars() {
        run = if c.is_ascii_hexdigit() { run + 1 } else { 0 };
        assert!(run < 16, "long hex run in output: {s}");
    }
}

struct Setup {
    kit: TempDir,
    tmp: TempDir,
    node_csv: String,
    hub_csv: String,
    master: String,
}

fn setup() -> Setup {
    let kit = TempDir::new();
    let tmp = TempDir::new();
    let d = kit.0.to_str().unwrap();
    let o = run(&tmp.0, &["kit", "new", "--kit-dir", d]);
    assert!(o.status.success(), "{}", text(&o));
    let node_csv = kit.0.join("node.csv");
    std::fs::write(&node_csv, "rskey, data, undefined, 0x611000, 0x1000\n").unwrap();
    let hub_csv = kit.0.join("hub.csv");
    std::fs::write(&hub_csv, "rskit, data, undefined, 0x3f0000, 0x2000\nrsboot, data, undefined, 0x3f2000, 0x2000\n").unwrap();
    let master = master_hex(&kit.0);
    assert_no_secret(&text(&o), &master);
    Setup { master, kit, tmp, node_csv: node_csv.to_str().unwrap().into(), hub_csv: hub_csv.to_str().unwrap().into() }
}

fn tmp_is_empty(tmp: &Path) -> bool {
    std::fs::read_dir(tmp).unwrap().next().is_none()
}

#[test]
fn kit_new_is_private_and_not_overwritten() {
    let s = setup();
    let d = s.kit.0.to_str().unwrap();
    assert_eq!(std::fs::metadata(s.kit.0.join("kit.key")).unwrap().permissions().mode() & 0o777, 0o600);
    let o = run(&s.tmp.0, &["kit", "new", "--kit-dir", d]);
    assert!(!o.status.success());
    assert!(text(&o).contains("already exists"), "{}", text(&o));
    assert_eq!(master_hex(&s.kit.0), s.master);
    let o = run(&s.tmp.0, &["kit", "new", "--force", "--kit-dir", d]);
    assert!(o.status.success());
    assert_ne!(master_hex(&s.kit.0), s.master);
}

#[test]
fn show_prints_no_secret() {
    let s = setup();
    let o = run(&s.tmp.0, &["show", "--kit-dir", s.kit.0.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("kit ") && t.contains("next free slot: 0"), "{t}");
    assert_no_secret(&t, &s.master);
}

#[test]
fn dry_runs_print_commands_not_keys() {
    let s = setup();
    let d = s.kit.0.to_str().unwrap();
    let cases: [&[&str]; 5] = [
        &["node", "c5a", "--dry-run", "--mac", "24:58:7c:01:02:03", "--kit-dir", d, "--table", &s.node_csv],
        &["node", "c5a", "--dry-run", "--mac", "24:58:7c:01:02:03", "--hub", "p4", "--hub-table", &s.hub_csv, "--kit-dir", d, "--table", &s.node_csv],
        &["hub", "p4", "--dry-run", "--kit-dir", d, "--table", &s.hub_csv],
        &["hub-roster", "p4", "--dry-run", "--kit-dir", d, "--table", &s.hub_csv],
        &["open", "c5a", "--dry-run", "--kit-dir", d, "--table", &s.node_csv],
    ];
    for args in cases {
        let o = run(&s.tmp.0, args);
        let t = text(&o);
        assert!(o.status.success(), "{args:?}: {t}");
        assert!(t.contains("[dry-run]") && t.contains("espflash write-bin"), "{t}");
        assert!(t.contains("NOT written (dry run)") && !t.contains(" written;"), "{t}");
        // The key file's path is shown, inside the private temporary directory.
        assert!(t.contains(s.tmp.0.to_str().unwrap()), "{t}");
        assert_no_secret(&t, &s.master);
        // ... and it is gone afterwards.
        assert!(tmp_is_empty(&s.tmp.0), "{args:?} left files in the temporary directory");
    }
    // Dry runs changed nothing.
    let o = run(&s.tmp.0, &["show", "--kit-dir", d]);
    assert!(text(&o).contains("next free slot: 0"));
}

#[test]
fn dry_run_node_needs_a_mac() {
    let s = setup();
    let o = run(&s.tmp.0, &["node", "c5a", "--dry-run", "--kit-dir", s.kit.0.to_str().unwrap(), "--table", &s.node_csv]);
    assert!(!o.status.success());
    let t = text(&o);
    assert!(t.contains("--mac") && t.contains("espflash board-info"), "{t}");
}

#[test]
fn bench_corrupt_needs_the_bench_switch() {
    let s = setup();
    let o = run(
        &s.tmp.0,
        &["node", "c5a", "--dry-run", "--bench-corrupt", "--mac", "24:58:7c:01:02:03", "--kit-dir", s.kit.0.to_str().unwrap(), "--table", &s.node_csv],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("ROTORSCOPE_BENCH=1"));
}

#[test]
fn revoke_needs_a_slot_and_an_entry() {
    let s = setup();
    let d = s.kit.0.to_str().unwrap();
    assert!(!run(&s.tmp.0, &["revoke", "--kit-dir", d]).status.success());
    let o = run(&s.tmp.0, &["revoke", "--slot", "2", "--kit-dir", d]);
    assert!(!o.status.success());
    assert!(text(&o).contains("slot 2 is empty"));
}

#[test]
fn usage_on_nonsense() {
    let tmp = TempDir::new();
    let o = run(&tmp.0, &["frobnicate"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("usage: link-key"));
}

/// The kit-key commands of the retired version 2 are gone.
#[test]
fn legacy_key_commands_are_gone() {
    let tmp = TempDir::new();
    for args in [&["new"][..], &["id"], &["new", "--force"]] {
        let o = run(&tmp.0, args);
        assert!(!o.status.success(), "{args:?}");
        assert!(text(&o).contains("usage: link-key"), "{args:?}");
    }
    assert!(tmp_is_empty(&tmp.0));
}

/// Ctrl-C during a board write (section 13, 13a leftover): the key image is
/// wiped and removed before the program ends, and it says so. The fake
/// board script signals link-key itself, as a terminal's Ctrl-C would (the
/// terminal also signals the board command, which then fails).
#[test]
fn interrupted_board_write_leaves_no_key_file() {
    let s = setup();
    let fake = s.kit.0.join("fake-dev-board.sh");
    let seen = s.kit.0.join("seen");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n\
             case \"$2\" in *write-bin*) f=${{2##* }}; f=${{f#\\'}}; f=${{f%\\'}}; [ -s \"$f\" ] && echo present >{seen};;\n\
             *) exit 1;; esac\n\
             kill -INT $PPID\n\
             sleep 1\n\
             exit 1\n",
            seen = seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        &["open", "c5a", "--kit-dir", s.kit.0.to_str().unwrap(), "--table", &s.node_csv][..],
        &["hub", "p4", "--kit-dir", s.kit.0.to_str().unwrap(), "--table", &s.hub_csv],
    ] {
        let _ = std::fs::remove_file(&seen);
        let o = Command::new(env!("CARGO_BIN_EXE_link-key"))
            .args(args)
            .env("TMPDIR", &s.tmp.0)
            .env("ROTORSCOPE_DEV_BOARD", &fake)
            .env_remove("ROTORSCOPE_KIT_DIR")
            .output()
            .unwrap();
        let t = text(&o);
        assert_eq!(o.status.code(), Some(130), "{args:?}: {t}");
        assert!(t.contains("interrupted") && t.contains("wiped and removed"), "{t}");
        assert!(seen.exists(), "the board command saw the key image: {t}");
        assert!(tmp_is_empty(&s.tmp.0), "{args:?} left a file behind");
        assert_no_secret(&t, &s.master);
    }
}
