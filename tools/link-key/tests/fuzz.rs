//! Fuzz-style tests (crates/fuzzkit) for link-key's text readers: the kit
//! roster file (no secrets in it), the partition-table CSV, MAC addresses
//! and the board-info output it reads a MAC from.

use link_key::{RosterFile, find_partition, fmt_mac, parse_board_info_mac, parse_mac};

const ROSTER: &str = "# rotorscope kit roster v1 (no secrets; the master is in kit.key)\n\
kit 0a1b2c3d\n\
# slot gen mac key_check state label\n\
0 1 02:00:00:00:00:01 096f7f75 live roof west\n\
3 2 02:00:00:00:00:02 ee4200f1 revoked\n\
15 7 24:58:7c:aa:bb:cc 01020304 live\n";

#[test]
fn fuzz_roster_file() {
    let dict: [&[u8]; 6] = [b"kit ", b" live", b" revoked", b"\n", b":", b"#"];
    let cfg = fuzzkit::Config::new().max_len(600).dict(&dict);
    assert!(RosterFile::parse(ROSTER).is_ok());
    fuzzkit::run_with("link-key::RosterFile::parse", &cfg, &[ROSTER.as_bytes()], 6_000, |b| {
        if let Ok(rf) = RosterFile::parse(&fuzzkit::text(b)) {
            // What parses writes back to text that parses to the same roster.
            let text = rf.to_text();
            let back = RosterFile::parse(&text).expect("a written roster parses");
            assert_eq!(back.kit_id, rf.kit_id);
            assert_eq!(back.roster, rf.roster);
            assert_eq!(back.to_text(), text);
        }
    });
}

#[test]
fn fuzz_partition_csv_and_macs() {
    let csv = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../partitions-node.csv")).unwrap();
    let seeds: [&[u8]; 3] = [&csv, b"MAC address: 24:58:7c:aa:bb:cc\nChip type: ESP32-C5", b"02:00:00:00:00:0a"];
    fuzzkit::run_with("link-key::find_partition", &fuzzkit::Config::new().max_len(csv.len() + 64), &seeds, 6_000, |b| {
        let t = fuzzkit::text(b);
        for name in ["rskey", "rsjob", "nvs", ""] {
            let _ = find_partition(&t, name);
        }
        if let Some(m) = parse_board_info_mac(&t) {
            assert_eq!(parse_mac(&fmt_mac(&m)), Ok(m));
        }
        if let Ok(m) = parse_mac(&t) {
            assert_eq!(parse_mac(&fmt_mac(&m)), Ok(m));
        }
    });
}
