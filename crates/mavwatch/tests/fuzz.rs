//! Fuzz-style tests (crates/fuzzkit) for the MAVLink scanner: frame parsing
//! at every offset, the reassembling stream watcher and analyser, and the
//! text checks applied to names and URIs a vehicle sends.

use mavwatch::frame::encode::{v1, v2};
use mavwatch::{Parse, StreamKey, Watch, frames, msg, parse_at, text};

fn seeds() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut b = [0u8; 300];
    let n = v2(&mut b, 1, 1, 1, msg::HEARTBEAT, &[0, 0, 0, 0, 2, 3, 0x81, 4, 3], None);
    out.push(b[..n].to_vec());
    let n = v1(&mut b, 2, 1, 1, msg::HEARTBEAT, &[0, 0, 0, 0, 2, 3, 0x81, 4, 3]);
    out.push(b[..n].to_vec());
    let mut cam = [0u8; 237];
    use msg::camera_information as c;
    cam[c::VENDOR..c::VENDOR + 18].copy_from_slice(b"../../../../../tmp");
    cam[c::URI..c::URI + 30].copy_from_slice(b"http://192.168.2.64:8000/c.xml");
    let n = v2(&mut b, 9, 1, 100, msg::CAMERA_INFORMATION, &cam, None);
    out.push(b[..n].to_vec());
    let joined: Vec<u8> = out.concat();
    out.push(joined);
    out
}

#[test]
fn fuzz_frames_and_watch() {
    let seeds = seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let cfg = fuzzkit::Config::new().max_len(900);
    fuzzkit::run_with("mavwatch::Watch::feed", &cfg, &refs, 3_000, |bytes| {
        for i in 0..bytes.len().min(64) {
            if let Parse::Frame(f) = parse_at(&bytes[i..]) {
                assert!(f.len() <= bytes.len() - i);
                let _ = f.text(0, 300);
                let _ = f.raw(250, 40);
                let _ = (f.u32(400), f.f32(1000), f.odd_length());
            }
        }
        let mut total = 0;
        for f in frames(bytes) {
            total += f.len();
        }
        assert!(total <= bytes.len());
        let mut w = Watch::default();
        let key = StreamKey { system: 2, id: Some(25), dir: 0 };
        let cut = bytes.len() / 2;
        for (k, part) in [&bytes[..cut], &bytes[cut..]].into_iter().enumerate() {
            w.feed(key, part, 1_000 + k as u64 * 50, &mut |_, f| assert!(f.len() <= 280));
        }
        let _ = w.pressure();
    });
}

#[test]
fn fuzz_text_checks() {
    let seeds: [&[u8]; 4] = [b"../../etc/passwd", b"rtsp://192.168.0.10:8554/live", b"<a href='x'>\x1b[2Jcam</a>", b"mftp://[;0]/camera.xml"];
    fuzzkit::run("mavwatch::text", &seeds, 8_000, |s| {
        let _ = text::traversal(s);
        let _ = text::control_chars(s);
        let _ = text::metadata_uri(s);
        let _ = text::video_uri(s);
        let _ = text::file_name(s);
        let _ = text::link_markup(s);
        assert!(text::text_control(s) <= s.len());
        let e = text::excerpt::<40>(s);
        assert!(e.len() <= 40);
    });
}
