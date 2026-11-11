//! The whole jobs: identify the chip, back up its flash, program an image.
//! Generic over [`Link`], so the tests run them against [`crate::sim`].

use crate::ccfg::{self, CCFG_LEN, Ccfg};
use crate::crc::crc32;
use crate::proto::{Bsl, Error, Link, MAX_READ_WORDS, Result};

pub const ICEPICK_DEVICE_ID: u32 = 0x5000_1318;
pub const FCFG1_USER_ID: u32 = 0x5000_1294;
pub const FLASH_SIZE_REG: u32 = 0x4003_002C;
pub const IEEE_PRIMARY: u32 = 0x5000_12F0;
/// CC13x2/CC26x2 wafer IDs (ICEPICK). Their flash sectors are 8 KB.
pub const AGAMA_WAFERS: [u16; 3] = [0xBB41, 0xBB77, 0xBB7A];
pub const SECTOR: u32 = 0x2000;
/// The CC2652P (and R/R7 and CC1352P): 352 KB.
pub const CC2652P_FLASH: u32 = 0x58000;
/// Bytes per SEND_DATA. 248 (not the 252 maximum) is what cc2538-bsl has
/// used for years against these ROMs, and it keeps every packet a multiple
/// of 8.
pub const CHUNK: usize = 248;

#[derive(Debug, Clone)]
pub struct ChipInfo {
    pub chip_id: u32,
    pub icepick: u32,
    pub user_id: u32,
    pub sectors: u32,
    pub ieee: [u8; 8],
    pub ccfg: Ccfg,
}

impl ChipInfo {
    pub fn wafer_id(&self) -> u16 {
        (self.icepick >> 12) as u16
    }
    pub fn pg(&self) -> u8 {
        (self.icepick >> 28) as u8
    }
    pub fn flash_size(&self) -> u32 {
        self.sectors * SECTOR
    }
    pub fn is_agama(&self) -> bool {
        AGAMA_WAFERS.contains(&self.wafer_id())
    }
    pub fn summary(&self) -> String {
        let ieee: Vec<String> = self.ieee.iter().map(|b| format!("{b:02X}")).collect();
        format!(
            "chip ID 0x{:08X}; ICEPICK 0x{:08X} (wafer 0x{:04X}{}, PG field {}); \
             FCFG1 USER_ID 0x{:08X}; flash {} KB ({} x 8 KB sectors); primary IEEE address {}",
            self.chip_id,
            self.icepick,
            self.wafer_id(),
            if self.is_agama() {
                " = CC13x2/CC26x2"
            } else {
                ", NOT a CC13x2/CC26x2"
            },
            self.pg(),
            self.user_id,
            self.flash_size() / 1024,
            self.sectors,
            ieee.join(":"),
        )
    }
}

/// Reset into the bootloader and sync.
pub fn connect<L: Link>(bsl: &mut Bsl<L>) -> Result<()> {
    bsl.enter_bootloader_sonoff()?;
    bsl.sync()?;
    bsl.ping()
}

/// Read-only identification.
pub fn identify<L: Link>(bsl: &mut Bsl<L>) -> Result<ChipInfo> {
    let chip_id = bsl.chip_id()?;
    let icepick = bsl.read_u32(ICEPICK_DEVICE_ID)?;
    let user_id = bsl.read_u32(FCFG1_USER_ID)?;
    let sectors = bsl.read_u32(FLASH_SIZE_REG)? & 0xFF;
    let lo = bsl.read_u32(IEEE_PRIMARY)?;
    let hi = bsl.read_u32(IEEE_PRIMARY + 4)?;
    // Stored most significant byte at the highest address.
    let mut ieee = [0u8; 8];
    ieee[..4].copy_from_slice(&hi.to_be_bytes());
    ieee[4..].copy_from_slice(&lo.to_be_bytes());
    let info0 = ChipInfo {
        chip_id,
        icepick,
        user_id,
        sectors,
        ieee,
        ccfg: Ccfg::from_block(&[0; CCFG_LEN]).unwrap(),
    };
    if !info0.is_agama() || sectors == 0 {
        return Err(Error::Refused(format!(
            "not a CC13x2/CC26x2 this tool knows: {}",
            info0.summary()
        )));
    }
    let block = read_range(
        bsl,
        info0.flash_size() - CCFG_LEN as u32,
        CCFG_LEN as u32,
        &mut |_, _| {},
    )?;
    Ok(ChipInfo {
        ccfg: Ccfg::from_block(&block).unwrap(),
        ..info0
    })
}

/// Read `len` bytes of memory (a multiple of 4) with MEMORY_READ.
pub fn read_range<L: Link>(
    bsl: &mut Bsl<L>,
    addr: u32,
    len: u32,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<Vec<u8>> {
    assert!(len.is_multiple_of(4) && addr.is_multiple_of(4));
    let mut out = Vec::with_capacity(len as usize);
    let mut at = addr;
    while at < addr + len {
        let words = ((addr + len - at) / 4).min(u32::from(MAX_READ_WORDS)) as u8;
        out.extend(bsl.read_words(at, words)?);
        at += 4 * u32::from(words);
        progress(at - addr, len);
    }
    Ok(out)
}

/// Read flash `0..len` and check it against the chip's own CRC32 of the
/// same range. Returns the bytes and the CRC they share.
pub fn backup<L: Link>(
    bsl: &mut Bsl<L>,
    len: u32,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<(Vec<u8>, u32)> {
    let data = read_range(bsl, 0, len, progress)?;
    let ours = crc32(&data);
    let chip = bsl.crc32(0, len)?;
    if ours != chip {
        return Err(Error::Verify(format!(
            "the bytes read have CRC32 0x{ours:08X}, the chip says 0x{chip:08X}"
        )));
    }
    Ok((data, chip))
}

/// Runs of 4-aligned, non-blank data to program: `(start, end)`. Erased
/// flash is already 0xFF, so all-0xFF chunks are skipped.
pub fn runs(image: &[u8]) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = Vec::new();
    for (i, c) in image.chunks(CHUNK).enumerate() {
        if c.iter().all(|&b| b == 0xFF) {
            continue;
        }
        let (s, e) = (i * CHUNK, i * CHUNK + c.len());
        match v.last_mut() {
            Some(last) if last.1 == s => last.1 = e,
            _ => v.push((s, e)),
        }
    }
    v
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Erase {
    Bank,
    /// BANK_ERASE was refused; fell back to erasing each sector.
    Sectors,
}

#[derive(Debug, Clone)]
pub struct Programmed {
    pub erase: Erase,
    pub bytes_sent: usize,
    pub packets: usize,
    pub crc: u32,
}

/// Refuse anything that is not a full image for this chip with a CCFG
/// that keeps the Sonoff's bootloader entry working.
pub fn preflight(image: &[u8], flash_size: u32) -> std::result::Result<ccfg::Report, String> {
    if image.len() != flash_size as usize {
        return Err(format!(
            "image is 0x{:X} bytes; this chip's flash is 0x{flash_size:X}",
            image.len()
        ));
    }
    let c = Ccfg::from_image(image).ok_or("image too small for a CCFG")?;
    let r = ccfg::check(&c, flash_size / SECTOR);
    if !r.safe() {
        return Err(format!(
            "this image's CCFG would lock the bootloader out:\n  - {}",
            r.fatal.join("\n  - ")
        ));
    }
    Ok(r)
}

/// After an erase, check the chip can take `image` bit for bit. Main
/// flash must be blank. The CCFG is not: the ROM's BANK_ERASE writes
/// back BL_CONFIG.BOOTLOADER_ENABLE, CCFG_TI_OPTIONS and both
/// CCFG_TAP_DAP words (0xC5 bytes) so an erased chip keeps its
/// bootloader and debug access. Programming can only clear bits, so each
/// CCFG byte the image wants must be a subset of what the erase left.
pub fn erased_ok<L: Link>(bsl: &mut Bsl<L>, image: &[u8]) -> Result<()> {
    let main = (image.len() - CCFG_LEN) as u32;
    let blank = crc32(&vec![0xFF; main as usize]);
    let after = bsl.crc32(0, main)?;
    if after != blank {
        return Err(Error::Verify(format!(
            "flash below the CCFG not blank after erase (CRC32 0x{after:08X}, blank is 0x{blank:08X})"
        )));
    }
    let left = read_range(bsl, main, CCFG_LEN as u32, &mut |_, _| {})?;
    let want = &image[main as usize..];
    if let Some(i) = (0..CCFG_LEN).find(|&i| want[i] & left[i] != want[i]) {
        return Err(Error::Verify(format!(
            "after the erase the CCFG byte at 0x{:X} is 0x{:02X}; the image needs 0x{:02X}, \
             which programming cannot reach (it only clears bits)",
            main as usize + i,
            left[i],
            want[i]
        )));
    }
    Ok(())
}

/// Erase, program and verify. The caller has already connected and
/// identified the chip, and run [`preflight`].
pub fn program<L: Link>(
    bsl: &mut Bsl<L>,
    image: &[u8],
    progress: &mut dyn FnMut(&str, usize, usize),
) -> Result<Programmed> {
    let size = image.len() as u32;
    // Erase. An erased chip has IMAGE_VALID_CONF = 0xFFFFFFFF, and the ROM
    // always stays in the bootloader on such a chip, so from here on a
    // failure is recoverable by entering the bootloader again.
    progress("erase", 0, 1);
    let erase = match bsl.bank_erase() {
        Ok(()) => Erase::Bank,
        Err(Error::Status { .. }) => {
            for s in 0..size / SECTOR {
                bsl.sector_erase(s * SECTOR)?;
                progress("erase", s as usize + 1, (size / SECTOR) as usize);
            }
            Erase::Sectors
        }
        Err(e) => return Err(e),
    };
    erased_ok(bsl, image)?;

    let runs = runs(image);
    let total: usize = runs.iter().map(|(s, e)| e - s).sum();
    let (mut sent, mut packets) = (0usize, 0usize);
    for (s, e) in runs {
        bsl.download(s as u32, (e - s) as u32)?;
        for c in image[s..e].chunks(CHUNK) {
            bsl.send_data(c)?;
            sent += c.len();
            packets += 1;
            progress("write", sent, total);
        }
    }

    let want = crc32(image);
    let got = bsl.crc32(0, size)?;
    if got != want {
        return Err(Error::Verify(format!(
            "flash CRC32 0x{got:08X} after writing, image is 0x{want:08X}"
        )));
    }
    Ok(Programmed {
        erase,
        bytes_sent: sent,
        packets,
        crc: got,
    })
}

/// [`program`], re-entering the bootloader and starting again on failure,
/// up to `attempts` times in all. Safe because a failed run leaves the
/// chip either without a CCFG (the CCFG is the last bytes of the last
/// run, so until then IMAGE_VALID_CONF reads erased and the ROM stays in
/// the bootloader) or with the image's CCFG, which [`preflight`] checked
/// keeps the DIO15 backdoor working.
pub fn program_with_retry<L: Link>(
    bsl: &mut Bsl<L>,
    image: &[u8],
    attempts: usize,
    progress: &mut dyn FnMut(&str, usize, usize),
    on_retry: &mut dyn FnMut(usize, &Error),
) -> Result<Programmed> {
    let mut n = 1;
    loop {
        match program(bsl, image, progress) {
            Ok(p) => return Ok(p),
            Err(e) if n < attempts => {
                on_retry(n, &e);
                n += 1;
                // Re-entering resets the chip, which also resyncs.
                connect(bsl)?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Load a `.hex` (Intel HEX) or raw binary image, padded with 0xFF to
/// `size`.
pub fn load_image(path: &std::path::Path, size: u32) -> std::result::Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let is_hex = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("hex"))
        || bytes.first() == Some(&b':');
    if is_hex {
        let text = std::str::from_utf8(&bytes).map_err(|_| "HEX file is not text".to_string())?;
        let chunks = crate::hex::parse(text).map_err(|e| format!("{}: {e}", path.display()))?;
        crate::hex::to_image(&chunks, size as usize)
    } else {
        if bytes.len() > size as usize {
            return Err(format!(
                "binary is 0x{:X} bytes, more than the 0x{size:X}-byte flash",
                bytes.len()
            ));
        }
        let mut img = bytes;
        img.resize(size as usize, 0xFF);
        Ok(img)
    }
}
