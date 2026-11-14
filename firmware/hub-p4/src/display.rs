//! The hub screen on the 800x480 panel.
//!
//! The shared `screen` crate draws a 320x240 UI (the S3 hub's panel). Here it
//! is drawn at 2x (640x480) on the left, and the right 160 px hold three touch
//! buttons standing in for the S3 hub's Up / Select / Down keys. A native
//! 800x480 layout can come later; this reuses every page as it is.

use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::{Rgb565, raw::RawU16},
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};

use crate::rgb::{H_RES, V_RES};

pub const SCALE: usize = 2;
pub const UI_W: usize = screen::WIDTH as usize * SCALE;
/// Width of the touch-button column on the right.
pub const KEYS_W: usize = H_RES - UI_W;
const KEY_H: usize = V_RES / 3;

/// Copy one finished `screen::Band` into the framebuffer at 2x.
pub fn blit_band(fb: &mut [u16], band: &screen::Band<'_>) {
    let w = screen::WIDTH as usize;
    for (r, row) in band.pixels().chunks(w).enumerate() {
        let y = (band.top() as usize + r) * SCALE;
        for dy in 0..SCALE {
            let line = &mut fb[(y + dy) * H_RES..(y + dy) * H_RES + UI_W];
            for (x, c) in row.iter().enumerate() {
                let v = RawU16::from(*c).into_inner();
                line[x * SCALE..x * SCALE + SCALE].fill(v);
            }
        }
    }
}

/// The three touch keys, top to bottom.
pub const KEYS: [(screen::Button, &str); 3] =
    [(screen::Button::Up, "UP"), (screen::Button::Select, "SELECT"), (screen::Button::Down, "DOWN")];

/// Which key a touch at (x, y) hits, if any. Panel coordinates.
pub fn key_at(x: u16, y: u16) -> Option<screen::Button> {
    let (x, y) = (usize::from(x), usize::from(y));
    (UI_W..H_RES).contains(&x).then(|| KEYS[(y / KEY_H).min(2)].0)
}

/// A plain framebuffer target for the key column.
struct Full<'a>(&'a mut [u16]);

impl OriginDimensions for Full<'_> {
    fn size(&self) -> Size {
        Size::new(H_RES as u32, V_RES as u32)
    }
}

impl DrawTarget for Full<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;
    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && (p.x as usize) < H_RES && (p.y as usize) < V_RES {
                self.0[p.y as usize * H_RES + p.x as usize] = RawU16::from(c).into_inner();
            }
        }
        Ok(())
    }
}

/// Draw the key column; `lit` highlights the key just pressed.
pub fn draw_keys(fb: &mut [u16], lit: Option<screen::Button>) {
    let mut t = Full(fb);
    let text = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    for (i, (b, label)) in KEYS.iter().enumerate() {
        let fill = if lit == Some(*b) { Rgb565::new(0, 40, 20) } else { Rgb565::new(4, 8, 6) };
        let style = PrimitiveStyleBuilder::new().fill_color(fill).stroke_color(Rgb565::new(12, 24, 12)).stroke_width(2).build();
        let r = Rectangle::new(Point::new((UI_W + 6) as i32, (i * KEY_H + 6) as i32), Size::new((KEYS_W - 12) as u32, (KEY_H - 12) as u32));
        let _ = r.into_styled(style).draw(&mut t);
        let _ = Text::with_alignment(label, r.center() + Point::new(0, 7), text, Alignment::Center).draw(&mut t);
    }
}
