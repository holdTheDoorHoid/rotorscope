//! Which Wi-Fi networks belong to drones and drone controllers.
//!
//! Many consumer drones (DJI Spark and Tello, Parrot, toy "WIFI-UFO" quads)
//! fly over plain Wi-Fi: the drone or its controller runs an access point
//! the phone app joins. A Wi-Fi node sees those beacons; this crate says
//! whose they are, from two clues:
//!
//! - the **address prefix** (OUI, the first 24 bits of the MAC, or 28 bits
//!   for an IEEE MA-M block) registered to the maker, and
//! - the **network name** (SSID) the maker ships as default.
//!
//! [`classify`] returns an index into [`MAKERS`]. That index travels on the
//! wire (`link::WifiDroneSighting::maker`), so **entries are only ever
//! appended; never reorder or remove one** (retire it by emptying its lists).
//!
//! # False positives
//! Most makers' prefixes are also on products that are not drones (DJI's
//! are on Osmo cameras and microphones, Parrot's on car kits and
//! headphones), and default names can be changed. So:
//!
//! - a [`Prefix`] with `alone: false` only counts together with one of that
//!   maker's network names;
//! - a [`Pattern`] with `alone: false` only counts together with one of that
//!   maker's prefixes;
//! - only a prefix registered to a company that makes nothing but drones,
//!   or a name too specific to be anything else (`TELLO-` plus six hex
//!   digits), counts on its own.
//!
//! A locally administered (randomised) address never matches a prefix.
//!
//! # Sources
//! Every prefix was checked against the IEEE registry as mirrored by
//! maclookup.app (2026-11-14); the page is named next to each entry.
//! Network names are cited where a manual or project documents them; the
//! ones marked *community* come from user reports and forums, not from the
//! maker, and are kept to names unlikely to be anything else.
#![no_std]

/// A registered address block: the first `bits` bits of `value` (24 for an
/// MA-L "OUI", 28 for an MA-M block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    pub value: [u8; 4],
    pub bits: u8,
    /// Enough on its own: the registrant makes only drones.
    pub alone: bool,
}

impl Prefix {
    const fn oui(a: u8, b: u8, c: u8) -> Self {
        Self {
            value: [a, b, c, 0],
            bits: 24,
            alone: false,
        }
    }
    const fn oui_alone(a: u8, b: u8, c: u8) -> Self {
        Self {
            value: [a, b, c, 0],
            bits: 24,
            alone: true,
        }
    }
    /// An MA-M block: 24 bits plus the high nibble `d`.
    const fn mam(a: u8, b: u8, c: u8, d: u8, alone: bool) -> Self {
        Self {
            value: [a, b, c, d << 4],
            bits: 28,
            alone,
        }
    }

    pub fn matches(&self, mac: &[u8; 6]) -> bool {
        if mac[0] & 0x02 != 0 {
            return false; // locally administered: not from any registry
        }
        if mac[..3] != self.value[..3] {
            return false;
        }
        self.bits <= 24 || mac[3] & 0xF0 == self.value[3]
    }
}

/// What must follow a [`Pattern`]'s prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// Anything, including nothing.
    Any,
    /// Exactly this many hex digits, then the end of the name.
    Hex(u8),
    /// Exactly this many decimal digits, then the end of the name.
    Digits(u8),
}

/// A default network name: `prefix` (ASCII, case-insensitive) then `tail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    pub prefix: &'static str,
    pub tail: Tail,
    /// Enough on its own, without one of the maker's address prefixes.
    pub alone: bool,
}

const fn any(prefix: &'static str) -> Pattern {
    Pattern {
        prefix,
        tail: Tail::Any,
        alone: false,
    }
}
const fn any_alone(prefix: &'static str) -> Pattern {
    Pattern {
        prefix,
        tail: Tail::Any,
        alone: true,
    }
}
const fn hex_alone(prefix: &'static str, n: u8) -> Pattern {
    Pattern {
        prefix,
        tail: Tail::Hex(n),
        alone: true,
    }
}
const fn hex(prefix: &'static str, n: u8) -> Pattern {
    Pattern {
        prefix,
        tail: Tail::Hex(n),
        alone: false,
    }
}
const fn digits_alone(prefix: &'static str, n: u8) -> Pattern {
    Pattern {
        prefix,
        tail: Tail::Digits(n),
        alone: true,
    }
}

impl Pattern {
    pub fn matches(&self, ssid: &[u8]) -> bool {
        let p = self.prefix.as_bytes();
        let Some(head) = ssid.get(..p.len()) else {
            return false;
        };
        if !head.eq_ignore_ascii_case(p) {
            return false;
        }
        let rest = &ssid[p.len()..];
        match self.tail {
            Tail::Any => true,
            Tail::Hex(n) => rest.len() == usize::from(n) && rest.iter().all(u8::is_ascii_hexdigit),
            Tail::Digits(n) => rest.len() == usize::from(n) && rest.iter().all(u8::is_ascii_digit),
        }
    }
}

/// One maker (or family of look-alike toys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Maker {
    /// Short display name, at most 16 characters.
    pub name: &'static str,
    pub prefixes: &'static [Prefix],
    pub ssids: &'static [Pattern],
}

impl Maker {
    /// Whether `mac` and `ssid` together say "this maker's drone".
    pub fn matches(&self, mac: &[u8; 6], ssid: &[u8]) -> bool {
        let by_prefix = self.prefixes.iter().find(|p| p.matches(mac));
        let by_name = self.ssids.iter().find(|p| p.matches(ssid));
        match (by_prefix, by_name) {
            (Some(_), Some(_)) => true,
            (Some(p), None) => p.alone,
            (None, Some(n)) => n.alone,
            (None, None) => false,
        }
    }
}

/// DJI's MA-L blocks, all registered as "SZ DJI TECHNOLOGY CO.,LTD"
/// (maclookup.app/vendors/sz-dji-technology-co-ltd), plus 34:91:F0 "DJI
/// Baiwang Technology Co Ltd" (maclookup.app/macaddress/3491f0). Shared with
/// Osmo cameras, DJI Mic and RoboMaster, so never enough alone.
const DJI_PREFIXES: &[Prefix] = &[
    Prefix::oui(0x04, 0xA8, 0x5A),
    Prefix::oui(0x0C, 0x9A, 0xE6),
    Prefix::oui(0x34, 0xD2, 0x62),
    Prefix::oui(0x48, 0x1C, 0xB9),
    Prefix::oui(0x4C, 0x43, 0xF6),
    Prefix::oui(0x58, 0xB8, 0x58),
    Prefix::oui(0x60, 0x60, 0x1F),
    Prefix::oui(0x88, 0x29, 0x85),
    Prefix::oui(0x8C, 0x58, 0x23),
    Prefix::oui(0xE4, 0x7A, 0x2C),
    Prefix::oui(0x34, 0x91, 0xF0),
];

/// The maker table. **Append only**: the index is sent by the nodes.
pub const MAKERS: &[Maker] = &[
    // 0
    Maker {
        name: "DJI",
        prefixes: DJI_PREFIXES,
        ssids: &[
            // Spark aircraft "Spark-45beb8" and its controller "Spark-RC-..."
            // (sparkpilots.com thread 13952; dronekenner.nl Wi-Fi FAQ). Not
            // alone: "Spark" is also an ISP's router brand.
            hex("Spark-RC-", 6),
            hex("Spark-", 6),
            // Mavic Air / Pro Wi-Fi mode, Phantom 3 Standard / 4, Mini SE:
            // community names, so only with a DJI prefix.
            any("MavicAir"),
            any("Mavic-"),
            any("MavicPro"),
            any("Phantom3"),
            any("Phantom4"),
            any("PHANTOM_"),
            any("Mini SE"),
            any("MiniSE"),
        ],
    },
    // 1
    Maker {
        name: "DJI Tello",
        // No Ryze prefix in the IEEE registry (it uses its Wi-Fi module's).
        prefixes: &[],
        ssids: &[
            // Tello SDK 3.0 User Guide: "TELLO-XXXXXX"; RoboMaster TT with
            // its open-source controller: "RMTT-XXXXXX" (droneblocks.io).
            hex_alone("TELLO-", 6),
            hex_alone("RMTT-", 6),
        ],
    },
    // 2
    Maker {
        name: "Parrot",
        // "PARROT SA" (maclookup.app/vendors/parrot-sa). Also on car kits
        // and Zik headphones, so never alone.
        prefixes: &[
            Prefix::oui(0x00, 0x12, 0x1C),
            Prefix::oui(0x00, 0x26, 0x7E),
            Prefix::oui(0x90, 0x03, 0xB7),
            Prefix::oui(0x90, 0x3A, 0xE6),
            Prefix::oui(0xA0, 0x14, 0x3D),
        ],
        ssids: &[
            // Bebop 2: "Bebop2_xxxxxx" / "Bebop2-xxxxxx" (techsolutions
            // support article; devel.hds.utc.fr flair wiki).
            any("Bebop"),
            // Anafi, Disco, AR.Drone 2, SkyController: community names.
            any("Anafi"),
            any("Disco-"),
            any("ardrone"),
            any("SkyController"),
        ],
    },
    // 3
    Maker {
        name: "Autel",
        prefixes: &[
            // MA-M "Autel lntelligent Technology Corp.,Ltd" (sic,
            // maclookup.app/macaddress/18d7936): Autel also makes car
            // diagnostic tools and chargers, so not alone.
            Prefix::mam(0x18, 0xD7, 0x93, 0x6, false),
            // MA-M "Autel Robotics USA LLC"
            // (maclookup.app/vendors/autel-robotics-usa-llc): drones only.
            Prefix::mam(0xEC, 0x5B, 0xCD, 0xE, true),
        ],
        // Community names.
        ssids: &[any("Autel"), any("EVO")],
    },
    // 4
    Maker {
        name: "Skydio",
        // "Skydio Inc." (maclookup.app/vendors/skydio-inc): drones and
        // their docks only.
        prefixes: &[Prefix::oui_alone(0x38, 0x1D, 0x14)],
        // Skydio prints each unit's network name on a sticker
        // (support.skydio.com); the brand word is the only safe part.
        ssids: &[any_alone("Skydio")],
    },
    // 5
    Maker {
        name: "Yuneec",
        // MA-M "Yuneec International (China) Co., Ltd"
        // (maclookup.app/vendors/yuneec-international-china-co-ltd). Yuneec
        // also makes handheld gimbal cameras, so not alone.
        prefixes: &[Prefix::mam(0xE0, 0xB6, 0xF5, 0x8, false)],
        // Community names (Breeze, Typhoon H, ST10/ST16 controllers, Mantis).
        ssids: &[
            any("Breeze"),
            any("Typhoon"),
            any("ST10"),
            any("ST16"),
            any("Mantis"),
        ],
    },
    // 6
    Maker {
        name: "Holy Stone",
        // No prefix of its own in the registry. Community name.
        prefixes: &[],
        ssids: &[
            any_alone("HolyStone"),
            any_alone("Holy Stone"),
            any_alone("HS_FPV"),
        ],
    },
    // 7
    Maker {
        name: "Potensic",
        prefixes: &[],
        // Community name; the brand word is not used for anything else.
        ssids: &[any_alone("Potensic")],
    },
    // 8
    Maker {
        name: "Hubsan",
        prefixes: &[],
        // Community name.
        ssids: &[any_alone("Hubsan")],
    },
    // 9
    Maker {
        name: "Snaptain",
        prefixes: &[],
        // Community name.
        ssids: &[any_alone("Snaptain")],
    },
    // 10
    Maker {
        name: "Syma",
        prefixes: &[],
        // Syma X5SW / X8W: "FPV WIFI_1234" / "FPV WIFI ****" (wiki.yobi.be
        // Syma_X5SW; manualslib Syma X8W manual p.6).
        ssids: &[
            digits_alone("FPV WIFI_", 4),
            digits_alone("FPV WIFI ", 4),
            any_alone("SYMA"),
        ],
    },
    // 11
    Maker {
        name: "Toy drone",
        prefixes: &[],
        // Generic Wi-Fi camera modules in unbranded quads (JJRC, Eachine,
        // SJRC, ...). Community names, each specific to these toys.
        ssids: &[
            any_alone("WIFI-UFO"),
            any_alone("WIFI_UFO"),
            any_alone("WiFi-720P"),
            any_alone("WiFi-1080P"),
            any_alone("WIFI_FPV"),
            any_alone("FPV_WIFI"),
            any_alone("JJRC"),
            any_alone("Eachine"),
            any_alone("SJRC"),
        ],
    },
    // 12
    Maker {
        name: "SkyViper",
        prefixes: &[],
        // ArduPilot SkyViper docs: the app requires the "SKYVIPERGPS_" prefix.
        ssids: &[any_alone("SKYVIPERGPS_"), any_alone("SkyViper")],
    },
];

/// The maker whose drone or controller this network probably is, as an
/// index into [`MAKERS`]; `None` when nothing (or only a weak clue) matches.
/// `ssid` is the raw network name (up to 32 bytes, not necessarily UTF-8).
pub fn classify(mac: &[u8; 6], ssid: &[u8]) -> Option<u8> {
    MAKERS
        .iter()
        .position(|m| m.matches(mac, ssid))
        .map(|i| i as u8)
}

/// The first maker one of whose default network names `ssid` matches,
/// whatever the address, with the pattern that matched. Used to notice a
/// drone's name on an address that is not the drone's (a copy of its
/// network): [`classify`] alone would just say "not a drone".
pub fn name_maker(ssid: &[u8]) -> Option<(u8, &'static Pattern)> {
    MAKERS.iter().enumerate().find_map(|(i, m)| {
        m.ssids
            .iter()
            .find(|p| p.matches(ssid))
            .map(|p| (i as u8, p))
    })
}

/// Whether `mac` is in one of maker `index`'s registered address blocks
/// (`alone` or not). False for an unknown index or a maker with no prefix.
pub fn has_prefix(index: u8, mac: &[u8; 6]) -> bool {
    MAKERS
        .get(usize::from(index))
        .is_some_and(|m| m.prefixes.iter().any(|p| p.matches(mac)))
}

/// Whether maker `index` has any registered address block, i.e. whether
/// [`has_prefix`] being false says anything.
pub fn has_prefixes(index: u8) -> bool {
    MAKERS
        .get(usize::from(index))
        .is_some_and(|m| !m.prefixes.is_empty())
}

/// The maker's display name, `None` for an index this build does not know
/// (a newer node).
pub fn maker_name(index: u8) -> Option<&'static str> {
    MAKERS.get(usize::from(index)).map(|m| m.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(mac: [u8; 6], ssid: &str) -> Option<&'static str> {
        classify(&mac, ssid.as_bytes()).and_then(maker_name)
    }

    const DJI_MAC: [u8; 6] = [0x60, 0x60, 0x1F, 0x12, 0x34, 0x56];
    const PARROT_MAC: [u8; 6] = [0x90, 0x03, 0xB7, 0x01, 0x02, 0x03];
    const OTHER_MAC: [u8; 6] = [0x00, 0x1A, 0x2B, 0x3C, 0x4D, 0x5E];

    #[test]
    fn table_is_sane() {
        assert!(MAKERS.len() < 255);
        for m in MAKERS {
            assert!(!m.name.is_empty() && m.name.len() <= 16, "{}", m.name);
            assert!(
                !m.ssids.is_empty() || m.prefixes.iter().any(|p| p.alone),
                "{}",
                m.name
            );
            for p in m.prefixes {
                assert!(p.bits == 24 || p.bits == 28);
                assert_eq!(p.value[0] & 0x03, 0, "{}: not a unicast global OUI", m.name);
                if p.bits == 24 {
                    assert_eq!(p.value[3], 0);
                }
            }
            for s in m.ssids {
                assert!(!s.prefix.is_empty() && s.prefix.len() <= 32 && s.prefix.is_ascii());
            }
        }
    }

    /// The wire index of the first entries must never move.
    #[test]
    fn indices_are_stable() {
        let expected = [
            "DJI",
            "DJI Tello",
            "Parrot",
            "Autel",
            "Skydio",
            "Yuneec",
            "Holy Stone",
            "Potensic",
            "Hubsan",
            "Snaptain",
            "Syma",
            "Toy drone",
            "SkyViper",
        ];
        for (i, n) in expected.iter().enumerate() {
            assert_eq!(maker_name(i as u8), Some(*n));
        }
        assert_eq!(maker_name(250), None);
    }

    #[test]
    fn drones_are_found() {
        assert_eq!(name(DJI_MAC, "Spark-45beb8"), Some("DJI"));
        assert_eq!(name(DJI_MAC, "SPARK-RC-45BEB8"), Some("DJI"));
        assert_eq!(name(DJI_MAC, "MavicAir-0a1b2c"), Some("DJI"));
        assert_eq!(name(OTHER_MAC, "TELLO-5A1B2C"), Some("DJI Tello"));
        assert_eq!(name(DJI_MAC, "TELLO-5A1B2C"), Some("DJI Tello"));
        assert_eq!(name(OTHER_MAC, "RMTT-0A1B2C"), Some("DJI Tello"));
        assert_eq!(name(PARROT_MAC, "Bebop2_123456"), Some("Parrot"));
        assert_eq!(name(PARROT_MAC, "Anafi-1234567"), Some("Parrot"));
        assert_eq!(
            name([0x38, 0x1D, 0x14, 1, 2, 3], "anything"),
            Some("Skydio")
        );
        assert_eq!(name([0xEC, 0x5B, 0xCD, 0xE7, 2, 3], "x"), Some("Autel"));
        assert_eq!(
            name([0x18, 0xD7, 0x93, 0x61, 2, 3], "Autel EVO II"),
            Some("Autel")
        );
        assert_eq!(
            name([0xE0, 0xB6, 0xF5, 0x81, 2, 3], "Breeze_1234"),
            Some("Yuneec")
        );
        assert_eq!(name(OTHER_MAC, "WIFI-UFO-1a2b3c"), Some("Toy drone"));
        assert_eq!(name(OTHER_MAC, "FPV WIFI_1234"), Some("Syma"));
        assert_eq!(name(OTHER_MAC, "Hubsan_Zino"), Some("Hubsan"));
        assert_eq!(name(OTHER_MAC, "SKYVIPERGPS_1234"), Some("SkyViper"));
    }

    #[test]
    fn shared_prefixes_need_a_name() {
        // A DJI Osmo, a DJI Mic, a Parrot car kit or headphones.
        assert_eq!(name(DJI_MAC, "OSMO_POCKET_123"), None);
        assert_eq!(name(DJI_MAC, "DIRECT-ab-DJI Mic"), None);
        assert_eq!(name(PARROT_MAC, "Parrot ZIK 3"), None);
        assert_eq!(name(PARROT_MAC, ""), None);
        // Autel's car-tool block and Yuneec's camera block.
        assert_eq!(name([0x18, 0xD7, 0x93, 0x61, 2, 3], "MaxiSys"), None);
        assert_eq!(name([0xE0, 0xB6, 0xF5, 0x81, 2, 3], "CGO"), None);
        // The 28-bit blocks: same 24 bits, other nibble, other registrant.
        assert_eq!(name([0xEC, 0x5B, 0xCD, 0x17, 2, 3], "x"), None);
        assert_eq!(name([0xE0, 0xB6, 0xF5, 0x11, 2, 3], "Breeze_1"), None);
    }

    #[test]
    fn weak_names_need_a_prefix() {
        // A home router called after the NZ ISP, or a pet.
        assert_eq!(name(OTHER_MAC, "Spark-45beb8"), None);
        assert_eq!(name(OTHER_MAC, "Bebop2_123456"), None);
        assert_eq!(name(OTHER_MAC, "Typhoon"), None);
        assert_eq!(name(OTHER_MAC, "EVO-home"), None);
        // Name too short or wrong tail for an alone pattern.
        assert_eq!(name(OTHER_MAC, "TELLO-5A1B2"), None);
        assert_eq!(name(OTHER_MAC, "TELLO-5A1B2CZ"), None);
        assert_eq!(name(OTHER_MAC, "TELLO"), None);
    }

    #[test]
    fn phones_and_routers_are_not_drones() {
        let cases: &[([u8; 6], &str)] = &[
            ([0xF0, 0x18, 0x98, 1, 2, 3], "iPhone"),
            ([0xAC, 0x37, 0x43, 1, 2, 3], "AndroidAP_1234"),
            ([0xA0, 0x40, 0xA0, 1, 2, 3], "NETGEAR42"),
            ([0x00, 0x14, 0x6C, 1, 2, 3], "xfinitywifi"),
            ([0x50, 0xC7, 0xBF, 1, 2, 3], "TP-Link_5G_1A2B"),
            ([0x24, 0x0A, 0xC4, 1, 2, 3], "ESP_1A2B3C"),
            ([0x02, 0x00, 0x00, 1, 2, 3], "DIRECT-xy-HP OfficeJet"),
            ([0x3C, 0x37, 0x86, 1, 2, 3], "Starbucks WiFi"),
            ([0x00, 0x11, 0x22, 1, 2, 3], ""),
        ];
        for (mac, ssid) in cases {
            assert_eq!(name(*mac, ssid), None, "{ssid}");
        }
    }

    #[test]
    fn randomised_addresses_never_match_a_prefix() {
        // Skydio's prefix with the locally administered bit set.
        assert_eq!(name([0x3A, 0x1D, 0x14, 1, 2, 3], "x"), None);
    }

    #[test]
    fn names_without_addresses() {
        // A DJI Spark name from a laptop: not a drone by `classify`, but
        // the name is DJI's, and the address is not.
        let (m, p) = name_maker(b"Spark-45beb8").unwrap();
        assert_eq!(maker_name(m), Some("DJI"));
        assert_eq!(p.tail, Tail::Hex(6));
        assert!(has_prefixes(m));
        assert!(!has_prefix(m, &OTHER_MAC));
        assert!(has_prefix(m, &DJI_MAC));
        assert_eq!(name_maker(b"TELLO-5A1B2C").map(|x| x.0), Some(1));
        assert!(!has_prefixes(1));
        assert_eq!(name_maker(b"HomeNet"), None);
        assert!(!has_prefix(250, &DJI_MAC));
    }

    #[test]
    fn non_utf8_names_are_fine() {
        assert_eq!(classify(&OTHER_MAC, &[0xFF; 32]), None);
        assert_eq!(
            classify(&OTHER_MAC, b"TELLO-\xff\xff\xff\xff\xff\xff"),
            None
        );
    }
}
