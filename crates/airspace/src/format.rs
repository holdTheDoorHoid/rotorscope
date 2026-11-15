//! Layout constants and the small enums stored in a pack.

/// First four bytes of every pack.
pub const MAGIC: [u8; 4] = *b"RSAP";
/// Layout version this crate reads and writes.
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 64;
pub const SOURCE_LEN: usize = 48;
pub const SOURCE_NAME_LEN: usize = 36;
pub const SECTION_LEN: usize = 12;
/// Fixed part of one area record (then the name, padded to 4, the ring
/// lengths as u16 padded to 4, then the points as i32 lat/lon pairs).
pub const AREA_HEAD_LEN: usize = 44;
pub const AIRPORT_LEN: usize = 20;
pub const GRID_HEAD_LEN: usize = 24;

/// Facility-map cell with no facility map (outside controlled airspace
/// covered by a grid).
pub const UASFM_NONE: u8 = 255;
/// Terrain post with no data (treated as unknown).
pub const TERRAIN_NONE: i16 = i16::MIN;

/// Section kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Section {
    Areas = 1,
    /// u8 per cell: ceiling in units of 10 ft, [`UASFM_NONE`] for none.
    Uasfm = 2,
    /// i16 per post: metres above the geoid (orthometric).
    Terrain = 3,
    /// i16 per post: geoid height in centimetres (ellipsoid minus geoid).
    Geoid = 4,
    /// 1 bit per cell, row-major, LSB first: census urban area.
    Urban = 5,
    Airports = 6,
}

impl Section {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            1 => Self::Areas,
            2 => Self::Uasfm,
            3 => Self::Terrain,
            4 => Self::Geoid,
            5 => Self::Urban,
            6 => Self::Airports,
            _ => return None,
        })
    }
}

/// What an area is. The rule engine decides what each means for a flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AreaKind {
    ClassB = 1,
    ClassC = 2,
    ClassD = 3,
    /// Class E surface area (E2), controlled from the ground up.
    ClassE2 = 4,
    /// Class E extension to a surface area (E3/E4), also from the ground.
    ClassE4 = 5,
    Prohibited = 10,
    Restricted = 11,
    /// National-security UAS flight restriction (14 CFR 99.7, DoD/DOE/...
    /// sites): no drones below the ceiling (usually 400 ft AGL).
    NationalSecurity = 12,
    /// Part-time national-security UAS restriction (active by schedule).
    NationalSecurityPartTime = 13,
    /// 3 NM around a large stadium: restricted only during events (FDC
    /// NOTAM 4/3621), which the pack cannot know.
    Stadium = 14,
    /// Temporary flight restriction, with start and end times.
    Tfr = 15,
    /// National defense airspace (e.g. the DC SFRA/FRZ).
    NationalDefense = 16,
    /// FAA-Recognized Identification Area: drones without Remote ID may
    /// fly here (14 CFR 89.115(b)), so no "no Remote ID" flag inside.
    Fria = 17,
}

impl AreaKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::ClassB,
            2 => Self::ClassC,
            3 => Self::ClassD,
            4 => Self::ClassE2,
            5 => Self::ClassE4,
            10 => Self::Prohibited,
            11 => Self::Restricted,
            12 => Self::NationalSecurity,
            13 => Self::NationalSecurityPartTime,
            14 => Self::Stadium,
            15 => Self::Tfr,
            16 => Self::NationalDefense,
            17 => Self::Fria,
            _ => return None,
        })
    }

    /// Short name for flags and the screen.
    pub fn name(self) -> &'static str {
        match self {
            Self::ClassB => "Class B",
            Self::ClassC => "Class C",
            Self::ClassD => "Class D",
            Self::ClassE2 => "Class E surface",
            Self::ClassE4 => "Class E ext",
            Self::Prohibited => "Prohibited",
            Self::Restricted => "Restricted",
            Self::NationalSecurity => "Natl security UAS",
            Self::NationalSecurityPartTime => "Natl security UAS (part time)",
            Self::Stadium => "Stadium 3NM",
            Self::Tfr => "TFR",
            Self::NationalDefense => "Natl defense",
            Self::Fria => "FRIA",
        }
    }

    /// Controlled airspace from the surface (needs an authorization such
    /// as LAANC for a Part 107 flight, 107.41; 44809(a)(5) for recreation).
    pub fn is_controlled(self) -> bool {
        matches!(
            self,
            Self::ClassB | Self::ClassC | Self::ClassD | Self::ClassE2 | Self::ClassE4
        )
    }
}

/// What a floor or ceiling value is measured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AltRef {
    /// Feet above mean sea level.
    Msl = 0,
    /// Feet above the ground ("SFC" with a value; 0 = the surface).
    Agl = 1,
    /// No limit.
    Unlimited = 2,
}

impl AltRef {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Msl,
            1 => Self::Agl,
            _ => Self::Unlimited,
        }
    }
}

/// Source ids the pack tool assigns, one per dataset, so a flag can name
/// where its evidence came from. Ids 250+ are not pack data.
pub mod src {
    pub const CLASS_AIRSPACE: u8 = 1;
    pub const UAS_FACILITY_MAP: u8 = 2;
    pub const SPECIAL_USE: u8 = 3;
    pub const NATIONAL_SECURITY: u8 = 4;
    pub const STADIUMS: u8 = 5;
    pub const TFR: u8 = 6;
    pub const TERRAIN: u8 = 7;
    pub const GEOID: u8 = 8;
    pub const URBAN: u8 = 9;
    pub const AIRPORTS: u8 = 10;
    pub const FRIA: u8 = 11;
    pub const NATIONAL_DEFENSE: u8 = 12;
    pub const PROHIBITED: u8 = 13;
    pub const NATIONAL_SECURITY_PART_TIME: u8 = 14;
    /// The drone's own broadcast (Remote ID / DroneID).
    pub const BROADCAST: u8 = 250;
    /// Computed on the hub (sun position).
    pub const COMPUTED: u8 = 251;
    /// ADS-B report.
    pub const ADSB: u8 = 252;
}
