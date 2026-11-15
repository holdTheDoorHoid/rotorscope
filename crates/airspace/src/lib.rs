//! Offline airspace and terrain for the Rotorscope hub (PLAN.md Phase 12).
//!
//! A host tool (`tools/airspace-pack`) downloads public FAA and USGS data,
//! cuts a region and writes one compact **pack** file; the hub copies it
//! from the SD card into RAM and answers questions about a position with no
//! parsing and no allocation:
//!
//! - which airspace areas contain it ([`Pack::areas_at`]): Class B/C/D and
//!   Class E surface areas, prohibited and restricted areas, national
//!   security UAS flight restrictions, stadium zones and TFRs, each with
//!   its floor and ceiling ([`Area::contains_alt`]) and validity time;
//! - the UAS Facility Map ceiling there ([`Pack::uasfm_ceiling_ft`]);
//! - the ground height, bilinear between terrain posts
//!   ([`Pack::ground_msl_m`], [`Pack::ground_hae_m`] adds the geoid so it
//!   compares with Remote ID's WGS-84 altitude);
//! - whether it is in a census urban area ([`Pack::urban`], the stand-in
//!   for "congested area" in 14 CFR 91.119);
//! - the nearest airport ([`Pack::nearest_airport`]);
//! - whether terrain blocks the line between two points
//!   ([`Pack::line_of_sight`]).
//!
//! [`sun`] computes the sun's elevation from UTC and position (no data).
//!
//! # Pack format (version 1, little-endian)
//!
//! ```text
//! header   64 bytes   magic "RSAP", version u16, header_len u16, created u32
//!                     (Unix s), centre lat/lon i32 (1e-7 deg), radius_m u32,
//!                     n_sources u16, n_sections u16, total_len u32,
//!                     crc32 u32 (IEEE, of bytes header_len..total_len),
//!                     reserved
//! sources  48 bytes each: id u8, 3 pad, date u32 (data's own date, Unix s),
//!                     fetched u32 (Unix s), name 36 bytes (NUL padded)
//! sections 12 bytes each: kind u16, reserved u16, offset u32, len u32
//! data     the sections, each 4-byte aligned
//! ```
//!
//! Section kinds ([`format::Section`]): areas, the facility-map grid (u8
//! ceiling / 10 ft, 255 none), terrain (i16 m above the geoid, NAVD88 for
//! 3DEP), the geoid grid (i16 cm), the urban bitmap and airports. Grids
//! share one 24-byte header ([`Grid`]). Every record names the source it
//! came from, so a flag can say "Class B, FAA ADDS 2026-10-30".
//!
//! Unknown section kinds are skipped, so a newer tool can add sections
//! without breaking older hubs; a new *version* means an incompatible
//! layout and is refused.
#![no_std]

#[cfg(any(feature = "alloc", test))]
extern crate alloc;

pub mod format;
mod geom;
mod pack;
pub mod sun;
#[cfg(any(feature = "alloc", test))]
pub mod write;

pub use format::{AltRef, AreaKind, Section};
pub use geom::{FT_PER_M, M_PER_FT, distance_m, point_in_rings};
pub use pack::{Airport, Area, AreaRef, Grid, Los, Pack, PackError, Source, crc32};

#[cfg(test)]
mod tests;
