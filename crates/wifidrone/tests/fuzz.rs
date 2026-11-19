//! Fuzz-style tests (crates/fuzzkit) for the Wi-Fi drone maker table:
//! network names (SSIDs) and addresses come straight off the air.

#[test]
fn fuzz_maker_table() {
    let seeds: [&[u8]; 4] = [b"\x60\x60\x1f\x00\x00\x01DJI-MAVIC-3", b"\x00\x00\x00\x00\x00\x00TELLO-ABCDEF", b"\x90\x3a\xe6\x00\x00\x00Anafi-123", b"\x00\x00\x00\x00\x00\x00"];
    fuzzkit::run_with("wifidrone::classify", &fuzzkit::Config::new().max_len(40), &seeds, 20_000, |bytes| {
        let mut mac = [0u8; 6];
        let n = bytes.len().min(6);
        mac[..n].copy_from_slice(&bytes[..n]);
        let ssid = &bytes[n..];
        if let Some(i) = wifidrone::classify(&mac, ssid) {
            assert!(usize::from(i) < wifidrone::MAKERS.len());
        }
        if let Some((i, _)) = wifidrone::name_maker(ssid) {
            assert!(usize::from(i) < wifidrone::MAKERS.len());
        }
    });
}
