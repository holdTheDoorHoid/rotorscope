//! Fuzz-style tests (crates/fuzzkit) for cc26xx-flash: Intel HEX images
//! (downloaded firmware), the CCFG block check that guards against a
//! bricking image, and the ROM bootloader's replies as the serial line
//! delivers them (a scripted link answers with the fuzzed bytes).

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use cc26xx_flash::proto::{Bsl, Link};
use cc26xx_flash::{ccfg, hex, ops};

fn rec(ty: u8, offset: u16, data: &[u8]) -> String {
    let mut b = vec![data.len() as u8, (offset >> 8) as u8, offset as u8, ty];
    b.extend_from_slice(data);
    let sum = b.iter().fold(0u8, |s, &x| s.wrapping_add(x));
    b.push(sum.wrapping_neg());
    let mut s = String::from(":");
    for x in b {
        s.push_str(&format!("{x:02X}"));
    }
    s
}

#[test]
fn fuzz_intel_hex() {
    let text = [rec(4, 0, &[0, 0]), rec(0, 0x0000, &[1, 2, 3, 4, 5, 6, 7, 8]), rec(2, 0, &[0x10, 0]), rec(0, 0x0010, &[9; 16]), rec(5, 0, &[0, 0, 0, 0]), rec(1, 0, &[])].join("\n");
    // The CCFG image shipped for the tests, too.
    let real = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sniffle-v1.11.0-cc2652p1-ccfg.hex")).unwrap();
    assert!(hex::parse(std::str::from_utf8(&real).unwrap()).is_ok());
    let dict: [&[u8]; 4] = [b":", b"\r\n", b"FF", b"00"];
    let cfg = fuzzkit::Config::new().max_len(real.len().max(600) + 64).dict(&dict);
    fuzzkit::run_with("cc26xx-flash::hex::parse", &cfg, &[text.as_bytes(), &real], 6_000, |b| {
        if let Ok(chunks) = hex::parse(&fuzzkit::text(b)) {
            if let Ok(img) = hex::to_image(&chunks, 0x2_0000) {
                assert_eq!(img.len(), 0x2_0000);
                let _ = ops::runs(&img);
                if let Some(c) = ccfg::Ccfg::from_image(&img) {
                    let _ = ccfg::describe(&c);
                }
            }
        }
    });
}

#[test]
fn fuzz_ccfg_block() {
    let mut good = vec![0xFF; 88];
    good[ccfg::OFF_BL_CONFIG..ccfg::OFF_BL_CONFIG + 4].copy_from_slice(&0xC5FE_0FC5u32.to_le_bytes());
    good[ccfg::OFF_IMAGE_VALID..ccfg::OFF_IMAGE_VALID + 4].copy_from_slice(&0u32.to_le_bytes());
    assert!(ccfg::check(&ccfg::Ccfg::from_block(&good).unwrap(), 44).safe());
    fuzzkit::run("cc26xx-flash::ccfg", &[&good], 8_000, |b| {
        if let Some(c) = ccfg::Ccfg::from_block(b) {
            let r = ccfg::check(&c, 44);
            let _ = (r.safe(), ccfg::describe(&c), c.sector_protected(u32::MAX), c.backdoor_pin());
        }
    });
}

/// A link that answers every read from a fixed script, then times out.
struct Script(VecDeque<u8>);

impl Link for Script {
    fn write_all(&mut self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn read_byte(&mut self, _: Duration) -> io::Result<Option<u8>> {
        Ok(self.0.pop_front())
    }
    fn set_dtr(&mut self, _: bool) -> io::Result<()> {
        Ok(())
    }
    fn set_rts(&mut self, _: bool) -> io::Result<()> {
        Ok(())
    }
    fn discard_input(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn delay(&mut self, _: Duration) {}
}

#[test]
fn fuzz_bootloader_replies() {
    // ACK (00 CC), then a 4-byte response packet (size, checksum, data), ACK.
    let reply = |data: &[u8]| {
        let mut v = vec![0x00, 0xCC, (data.len() + 2) as u8, data.iter().fold(0u8, |s, &x| s.wrapping_add(x))];
        v.extend_from_slice(data);
        v
    };
    let mut s = vec![0x00, 0xCC];
    s.extend(reply(&[0x40]));
    s.extend(reply(&[0x3B, 0x20, 0x40, 0x2B]));
    s.extend(reply(&[1, 2, 3, 4, 5, 6, 7, 8]));
    fuzzkit::run_with("cc26xx-flash::proto::Bsl", &fuzzkit::Config::new().max_len(200), &[&s], 4_000, |b| {
        let mut bsl = Bsl::new(Script(b.iter().copied().collect()));
        let _ = bsl.get_status();
        let _ = bsl.chip_id();
        if let Ok(v) = bsl.read_words(0x5000_1000, 2) {
            assert!(v.len() <= 2 * 4 + 255);
        }
        let _ = bsl.crc32(0, 0x1000);
        let mut bsl = Bsl::new(Script(b.iter().copied().collect()));
        let _ = ops::identify(&mut bsl);
    });
}
