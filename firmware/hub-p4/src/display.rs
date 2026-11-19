//! The hub screen on the 800x480 panel: the shared `screen` crate's native
//! wide layout (`screen::wide`), drawn band by band into a PSRAM buffer and
//! copied into the framebuffer, with the touch keys as a strip of three
//! context keys along the bottom whose labels say what they do on the
//! screen showing.

use crate::rgb::{H_RES, V_RES};

const _: () = assert!(H_RES == screen::wide::W as usize && V_RES == screen::wide::H as usize);

/// Rows per band, and the band's size in pixels (800 x 48, 75 KB in PSRAM).
pub const BAND_ROWS: usize = screen::wide::BAND_ROWS;
pub const BAND_PX: usize = H_RES * BAND_ROWS;

/// Copy one finished band (`rows` whole rows starting at panel row `top`)
/// into the framebuffer.
pub fn blit_band(fb: &mut [u16], top: i32, px: &[u16]) {
    let start = top as usize * H_RES;
    fb[start..start + px.len()].copy_from_slice(px);
}

/// Which key a touch at (x, y) hits, if any (panel coordinates): the bottom
/// strip, in thirds edge to edge, a little taller than the drawn keys.
/// Touches on the page above are ignored.
pub fn key_at(x: u16, y: u16) -> Option<screen::Button> {
    screen::wide::key_at(i32::from(x), i32::from(y))
}
