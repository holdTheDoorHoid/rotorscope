//! The customer configuration area (CCFG): the last 88 bytes of flash on a
//! CC13x2/CC26x2. The ROM reads it at every boot. Two of its words decide
//! whether the ROM serial bootloader can ever be reached again, which makes
//! a bad CCFG the one realistic way to brick this chip over UART. This
//! module refuses any image that would lock us out.
//!
//! Word offsets from the start of the 88-byte block (TI SWCU185, CCFG
//! registers; the same layout `cc2538-bsl` uses for its bootloader-disable
//! address `size - 88 + 0x30`):
//!
//! | off  | register          |
//! |------|-------------------|
//! | 0x30 | `BL_CONFIG`       |
//! | 0x34 | `ERASE_CONF`      |
//! | 0x44 | `IMAGE_VALID_CONF`|
//! | 0x48 | `CCFG_PROT_31_0` .. 0x54 `CCFG_PROT_127_96` |

pub const CCFG_LEN: usize = 88;
pub const OFF_BL_CONFIG: usize = 0x30;
pub const OFF_ERASE_CONF: usize = 0x34;
pub const OFF_IMAGE_VALID: usize = 0x44;
pub const OFF_PROT: usize = 0x48;

/// `BL_CONFIG` byte value meaning "enabled" (both the bootloader and its
/// backdoor use 0xC5; anything else is disabled).
pub const ENABLED: u8 = 0xC5;
/// The Sonoff dongle's bootloader-backdoor pin: DIO15, pulled low by the
/// CP2102N's DTR/RTS gate.
pub const SONOFF_BSL_PIN: u8 = 15;

/// Decoded fields that matter for staying recoverable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ccfg {
    pub bl_config: u32,
    pub erase_conf: u32,
    pub image_valid: u32,
    pub prot: [u32; 4],
}

fn word(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

impl Ccfg {
    /// From the 88-byte CCFG block (as stored, little-endian words).
    pub fn from_block(b: &[u8]) -> Option<Ccfg> {
        if b.len() != CCFG_LEN {
            return None;
        }
        Some(Ccfg {
            bl_config: word(b, OFF_BL_CONFIG),
            erase_conf: word(b, OFF_ERASE_CONF),
            image_valid: word(b, OFF_IMAGE_VALID),
            prot: [0, 1, 2, 3].map(|i| word(b, OFF_PROT + 4 * i)),
        })
    }

    /// From a whole-flash image: its last 88 bytes.
    pub fn from_image(img: &[u8]) -> Option<Ccfg> {
        Self::from_block(img.get(img.len().checked_sub(CCFG_LEN)?..)?)
    }

    pub fn bootloader_enable(&self) -> u8 {
        (self.bl_config >> 24) as u8
    }
    /// BL_LEVEL: true = the backdoor triggers on a high pin.
    pub fn backdoor_active_high(&self) -> bool {
        self.bl_config & (1 << 16) != 0
    }
    pub fn backdoor_pin(&self) -> u8 {
        (self.bl_config >> 8) as u8
    }
    pub fn backdoor_enable(&self) -> u8 {
        self.bl_config as u8
    }
    /// ERASE_CONF.CHIP_ERASE_DIS_N (bit 8): 1 = chip erase allowed.
    pub fn chip_erase_allowed(&self) -> bool {
        self.erase_conf & (1 << 8) != 0
    }
    /// ERASE_CONF.BANK_ERASE_DIS_N (bit 0): 1 = bank erase allowed.
    pub fn bank_erase_allowed(&self) -> bool {
        self.erase_conf & 1 != 0
    }
    /// CCFG_PROT: a 0 bit write- and erase-protects that sector.
    pub fn sector_protected(&self, sector: u32) -> bool {
        sector < 128 && self.prot[(sector / 32) as usize] & (1 << (sector % 32)) == 0
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Reasons this image must not be written. Empty = safe.
    pub fatal: Vec<String>,
    /// Worth saying, not a reason to stop.
    pub notes: Vec<String>,
}

impl Report {
    pub fn safe(&self) -> bool {
        self.fatal.is_empty()
    }
}

/// Check that a device running with this CCFG stays reachable through the
/// Sonoff's ROM bootloader entry (DIO15 pulled low), and can be erased and
/// rewritten by it. `sectors` is how many flash sectors the chip has.
pub fn check(c: &Ccfg, sectors: u32) -> Report {
    let mut r = Report::default();
    if c.bootloader_enable() != ENABLED {
        r.fatal.push(format!(
            "BL_CONFIG.BOOTLOADER_ENABLE is 0x{:02X}, not 0xC5: the ROM bootloader would be disabled",
            c.bootloader_enable()
        ));
    }
    if c.backdoor_enable() != ENABLED {
        r.fatal.push(format!(
            "BL_CONFIG.BL_ENABLE is 0x{:02X}, not 0xC5: the bootloader backdoor pin would be ignored",
            c.backdoor_enable()
        ));
    }
    if c.backdoor_pin() != SONOFF_BSL_PIN {
        r.fatal.push(format!(
            "BL_CONFIG.BL_PIN_NUMBER is DIO{}, not DIO{SONOFF_BSL_PIN} (the Sonoff's BSL pin)",
            c.backdoor_pin()
        ));
    }
    if c.backdoor_active_high() {
        r.fatal.push(
            "BL_CONFIG.BL_LEVEL is active-high; the Sonoff pulls DIO15 low to enter the bootloader"
                .into(),
        );
    }
    if !c.chip_erase_allowed() {
        r.fatal
            .push("ERASE_CONF.CHIP_ERASE_DIS_N is 0: chip erase would be disabled".into());
    }
    if !c.bank_erase_allowed() {
        r.fatal.push(
            "ERASE_CONF.BANK_ERASE_DIS_N is 0: the bootloader's bank erase would be refused".into(),
        );
    }
    let protected: Vec<u32> = (0..sectors).filter(|&s| c.sector_protected(s)).collect();
    if !protected.is_empty() {
        r.fatal.push(format!(
            "CCFG_PROT write-protects sector(s) {protected:?}; they could never be rewritten over UART"
        ));
    }
    if c.image_valid != 0 {
        r.notes.push(format!(
            "IMAGE_VALID_CONF is 0x{:08X}, not 0: the ROM will not start this image and will stay in the bootloader",
            c.image_valid
        ));
    }
    r
}

/// One line per field, for `info` and `flash --dry-run`.
pub fn describe(c: &Ccfg) -> String {
    format!(
        "BL_CONFIG 0x{:08X} (bootloader {}, backdoor {}: DIO{} active-{}), \
         ERASE_CONF 0x{:08X} (bank erase {}, chip erase {}), IMAGE_VALID_CONF 0x{:08X}",
        c.bl_config,
        if c.bootloader_enable() == ENABLED {
            "on"
        } else {
            "OFF"
        },
        if c.backdoor_enable() == ENABLED {
            "on"
        } else {
            "OFF"
        },
        c.backdoor_pin(),
        if c.backdoor_active_high() {
            "high"
        } else {
            "low"
        },
        c.erase_conf,
        if c.bank_erase_allowed() {
            "allowed"
        } else {
            "DISABLED"
        },
        if c.chip_erase_allowed() {
            "allowed"
        } else {
            "DISABLED"
        },
        c.image_valid,
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A good CCFG block: the Sniffle image's BL_CONFIG, everything else
    /// erased-style (all ones) except IMAGE_VALID_CONF = 0.
    pub fn good_block() -> Vec<u8> {
        let mut b = vec![0xFF; CCFG_LEN];
        b[OFF_BL_CONFIG..OFF_BL_CONFIG + 4].copy_from_slice(&0xC5FE_0FC5u32.to_le_bytes());
        b[OFF_IMAGE_VALID..OFF_IMAGE_VALID + 4].copy_from_slice(&0u32.to_le_bytes());
        b
    }

    fn with_word(off: usize, v: u32) -> Ccfg {
        let mut b = good_block();
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        Ccfg::from_block(&b).unwrap()
    }

    #[test]
    fn good_is_safe() {
        let c = Ccfg::from_block(&good_block()).unwrap();
        let r = check(&c, 44);
        assert!(r.safe(), "{r:?}");
        assert!(r.notes.is_empty());
        assert_eq!(c.backdoor_pin(), 15);
        assert!(!c.backdoor_active_high());
    }

    #[test]
    fn bootloader_disabled() {
        let r = check(&with_word(OFF_BL_CONFIG, 0x00FE_0FC5), 44);
        assert!(!r.safe());
        assert!(r.fatal[0].contains("BOOTLOADER_ENABLE"));
    }

    #[test]
    fn backdoor_disabled_wrong_pin_wrong_level() {
        assert!(!check(&with_word(OFF_BL_CONFIG, 0xC5FE_0F00), 44).safe());
        assert!(
            !check(&with_word(OFF_BL_CONFIG, 0xC5FE_0BC5), 44).safe(),
            "DIO11"
        );
        assert!(
            !check(&with_word(OFF_BL_CONFIG, 0xC5FF_0FC5), 44).safe(),
            "active high"
        );
        // The all-ones value an erased CCFG has also counts as unsafe to write.
        assert!(!check(&with_word(OFF_BL_CONFIG, 0xFFFF_FFFF), 44).safe());
    }

    #[test]
    fn erase_disabled() {
        assert!(
            !check(&with_word(OFF_ERASE_CONF, 0xFFFF_FEFF), 44).safe(),
            "chip erase"
        );
        assert!(
            !check(&with_word(OFF_ERASE_CONF, 0xFFFF_FFFE), 44).safe(),
            "bank erase"
        );
    }

    #[test]
    fn write_protection() {
        let r = check(&with_word(OFF_PROT + 4, 0xFFFF_F7FF), 44);
        assert!(!r.safe());
        assert!(r.fatal[0].contains("43"), "{r:?}");
        // A protect bit beyond the chip's sectors does not matter.
        assert!(check(&with_word(OFF_PROT + 4, 0xFFFF_EFFF), 44).safe());
    }

    #[test]
    fn invalid_image_is_a_note() {
        let r = check(&with_word(OFF_IMAGE_VALID, 0xFFFF_FFFF), 44);
        assert!(r.safe());
        assert_eq!(r.notes.len(), 1);
    }
}
