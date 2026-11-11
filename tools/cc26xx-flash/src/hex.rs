//! Intel HEX, parsed here rather than with a library so every rule is in
//! view: each record's checksum is checked, an end-of-file record is
//! required, nothing may follow it, and no byte may be given twice.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexError {
    /// 1-based line number.
    pub line: usize,
    pub why: String,
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.why)
    }
}

/// One data record: absolute address and bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub addr: u32,
    pub data: Vec<u8>,
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Parse Intel HEX text into data chunks at absolute addresses.
pub fn parse(text: &str) -> Result<Vec<Chunk>, HexError> {
    let mut out = Vec::new();
    // Upper address bits from record type 02 (segment << 4) or 04 (<< 16).
    let mut base: u32 = 0;
    let mut eof = false;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let err = |why: &str| HexError {
            line,
            why: why.into(),
        };
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        if eof {
            return Err(err("data after the end-of-file record"));
        }
        let body = l
            .strip_prefix(':')
            .ok_or_else(|| err("record does not start with ':'"))?;
        let b = hex_bytes(body).ok_or_else(|| err("not an even number of hex digits"))?;
        if b.len() < 5 {
            return Err(err("record too short"));
        }
        let count = usize::from(b[0]);
        if b.len() != count + 5 {
            return Err(err("byte count does not match the record length"));
        }
        if b.iter().fold(0u8, |s, &x| s.wrapping_add(x)) != 0 {
            return Err(err("checksum mismatch"));
        }
        let offset = u32::from(u16::from_be_bytes([b[1], b[2]]));
        let data = &b[4..4 + count];
        match b[3] {
            0x00 => {
                let addr = base
                    .checked_add(offset)
                    .ok_or_else(|| err("address overflows 32 bits"))?;
                out.push(Chunk {
                    addr,
                    data: data.to_vec(),
                });
            }
            0x01 => {
                if count != 0 {
                    return Err(err("end-of-file record carries data"));
                }
                eof = true;
            }
            0x02 if count == 2 => base = u32::from(u16::from_be_bytes([data[0], data[1]])) << 4,
            0x04 if count == 2 => base = u32::from(u16::from_be_bytes([data[0], data[1]])) << 16,
            // Start addresses (CS:IP or EIP). The ROM boots from the vector
            // table, so these carry nothing we need.
            0x03 | 0x05 if count == 4 => {}
            t @ (0x02..=0x05) => {
                return Err(err(&format!("record type {t:02X} has the wrong length")));
            }
            t => return Err(err(&format!("unknown record type {t:02X}"))),
        }
    }
    if !eof {
        return Err(HexError {
            line: text.lines().count(),
            why: "no end-of-file record (file truncated?)".into(),
        });
    }
    Ok(out)
}

/// Lay chunks into a `size`-byte image starting at address 0, filling gaps
/// with 0xFF (erased flash). Refuses data outside the image and any byte
/// given twice.
pub fn to_image(chunks: &[Chunk], size: usize) -> Result<Vec<u8>, String> {
    let mut img = vec![0xFFu8; size];
    let mut set = vec![false; size];
    for c in chunks {
        let start = c.addr as usize;
        let end = start + c.data.len();
        if end > size {
            return Err(format!(
                "data at 0x{:X}..0x{:X} is outside the 0x{size:X}-byte flash",
                c.addr, end
            ));
        }
        for (i, &b) in c.data.iter().enumerate() {
            if set[start + i] {
                return Err(format!("address 0x{:X} is given twice", start + i));
            }
            set[start + i] = true;
            img[start + i] = b;
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a record with a correct checksum.
    pub fn rec(ty: u8, offset: u16, data: &[u8]) -> String {
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
    fn simple_file() {
        let t = format!("{}\r\n{}\n", rec(0, 0x0010, &[1, 2, 3]), rec(1, 0, &[]));
        let c = parse(&t).unwrap();
        assert_eq!(
            c,
            vec![Chunk {
                addr: 0x10,
                data: vec![1, 2, 3]
            }]
        );
        let img = to_image(&c, 0x20).unwrap();
        assert_eq!(&img[0x10..0x13], &[1, 2, 3]);
        assert_eq!(img[0], 0xFF);
        assert_eq!(img[0x13], 0xFF);
    }

    #[test]
    fn extended_addresses() {
        let t = [
            rec(4, 0, &[0x00, 0x01]),
            rec(0, 0x0004, &[0xAA]),
            rec(2, 0, &[0x10, 0x00]),
            rec(0, 0x0002, &[0xBB]),
            rec(5, 0, &[0, 0, 0, 0]),
            rec(3, 0, &[0, 0, 0xD2, 0xC5]),
            rec(1, 0, &[]),
        ]
        .join("\n");
        let c = parse(&t).unwrap();
        assert_eq!(c[0].addr, 0x1_0004);
        assert_eq!(c[1].addr, 0x1_0002);
    }

    #[test]
    fn known_record_checksum() {
        // The last record of the Sniffle image, verbatim.
        assert!(parse(":087FF800FFFFFFFFFFFFFFFF89\n:00000001FF\n").is_ok());
        assert_eq!(rec(0, 0x7FF8, &[0xFF; 8]), ":087FF800FFFFFFFFFFFFFFFF89");
    }

    #[test]
    fn bad_checksum() {
        let e = parse(":087FF800FFFFFFFFFFFFFFFF88\n:00000001FF\n").unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.why.contains("checksum"));
    }

    #[test]
    fn truncated_or_malformed() {
        assert!(
            parse(&rec(0, 0, &[1]))
                .unwrap_err()
                .why
                .contains("end-of-file")
        );
        assert!(parse(":0100000001\n").is_err(), "short of the data byte");
        assert!(parse("0100000001FE\n:00000001FF").is_err(), "no colon");
        assert!(parse(":01000000ZZ00\n:00000001FF").is_err(), "not hex");
        assert!(parse(":00000001FF\n:00000001FF").is_err(), "after EOF");
        assert!(
            parse(&format!("{}\n:00000001FF", rec(7, 0, &[]))).is_err(),
            "unknown type"
        );
        assert!(
            parse(&format!("{}\n:00000001FF", rec(4, 0, &[1]))).is_err(),
            "bad 04 length"
        );
    }

    #[test]
    fn image_bounds_and_overlap() {
        let c = vec![Chunk {
            addr: 0x1E,
            data: vec![0; 4],
        }];
        assert!(to_image(&c, 0x20).unwrap_err().contains("outside"));
        let c = vec![
            Chunk {
                addr: 0,
                data: vec![0; 4],
            },
            Chunk {
                addr: 3,
                data: vec![1],
            },
        ];
        assert!(to_image(&c, 0x20).unwrap_err().contains("twice"));
    }
}
