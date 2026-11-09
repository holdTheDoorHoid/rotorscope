//! Rendering a frame in horizontal bands, for a hub without PSRAM.
//!
//! A whole 320x240 RGB565 frame is 150 KB, which the hub's ESP32-S3 (no
//! PSRAM) cannot spare next to Wi-Fi. Instead the frame is drawn once per
//! band into a small buffer (320x24 = 15 KB), and each finished band is sent
//! to the panel. [`Band`] is a `DrawTarget` that reports the full panel size,
//! so [`crate::render`] lays out exactly as it would on a framebuffer, and
//! keeps only the pixels inside the current band.
//!
//! Why this does not flicker: the panel only ever receives finished pixels.
//! A band is cleared and redrawn in RAM, then written over the old one in a
//! single transfer, so the "clear" in `render` never reaches the glass. (A
//! band written while the panel scans past it can show a one-frame tear
//! line on fast motion; nothing on these screens moves fast.)
//!
//! The cost is CPU: every band re-runs the whole `render`, with most of the
//! drawing clipped away. [`Band`] makes the clipped work cheap by rejecting
//! whole rectangles (fills, glyphs) before touching their pixels.

use embedded_graphics::{Pixel, pixelcolor::Rgb565, prelude::*, primitives::Rectangle};

use crate::model::UiState;
use crate::render::{HEIGHT, WIDTH, render};
use crate::ui::Ui;

/// Rows per band that the hub uses: 320 x 24 x 2 bytes = 15 KB, 10 bands.
pub const BAND_ROWS: usize = 24;

/// A horizontal slice of the panel, `WIDTH` pixels wide.
pub struct Band<'a> {
    px: &'a mut [Rgb565],
    /// Panel row of the band's first row.
    top: i32,
    /// Rows the buffer holds.
    cap_rows: i32,
}

impl<'a> Band<'a> {
    /// A band over `px`, which holds `px.len() / WIDTH` rows.
    pub fn new(px: &'a mut [Rgb565]) -> Self {
        let cap_rows = (px.len() / WIDTH as usize) as i32;
        Self {
            px,
            top: 0,
            cap_rows,
        }
    }

    /// Move the band to start at panel row `top`.
    pub fn set_top(&mut self, top: i32) {
        self.top = top;
    }

    pub fn top(&self) -> i32 {
        self.top
    }

    /// Rows of the panel this band covers at its current position (the last
    /// band may be shorter than the buffer).
    pub fn rows(&self) -> i32 {
        self.cap_rows.min(HEIGHT - self.top).max(0)
    }

    /// The band's pixels, row-major, `rows() * WIDTH` of them.
    pub fn pixels(&self) -> &[Rgb565] {
        &self.px[..(self.rows() * WIDTH) as usize]
    }

    /// The panel area this band covers.
    fn area(&self) -> Rectangle {
        Rectangle::new(
            Point::new(0, self.top),
            Size::new(WIDTH as u32, self.rows() as u32),
        )
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: Rgb565) {
        let row = y - self.top;
        if (0..WIDTH).contains(&x) && (0..self.rows()).contains(&row) {
            self.px[(row * WIDTH + x) as usize] = c;
        }
    }
}

impl OriginDimensions for Band<'_> {
    /// The whole panel, so layouts are computed as for a full framebuffer.
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Band<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            self.put(p.x, p.y, c);
        }
        Ok(())
    }

    /// Glyphs and images arrive here. Outside the band: return without
    /// generating a single pixel. Inside: skip the rows above the band and
    /// stop after the last row in it.
    fn fill_contiguous<I: IntoIterator<Item = Rgb565>>(
        &mut self,
        area: &Rectangle,
        colors: I,
    ) -> Result<(), Self::Error> {
        let clip = area.intersection(&self.area());
        if clip.is_zero_sized() || area.is_zero_sized() {
            return Ok(());
        }
        let w = area.size.width as i32;
        let first = clip.top_left.y;
        let last = first + clip.size.height as i32; // exclusive
        let skip = ((first - area.top_left.y) * w) as usize;
        let mut it = colors.into_iter().skip(skip);
        for y in first..last {
            for dx in 0..w {
                let Some(c) = it.next() else { return Ok(()) };
                self.put(area.top_left.x + dx, y, c);
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let clip = area.intersection(&self.area());
        if clip.is_zero_sized() {
            return Ok(());
        }
        let x0 = clip.top_left.x.max(0);
        let x1 = (clip.top_left.x + clip.size.width as i32).min(WIDTH);
        if x0 >= x1 {
            return Ok(());
        }
        for y in clip.top_left.y..clip.top_left.y + clip.size.height as i32 {
            let row = ((y - self.top) * WIDTH) as usize;
            self.px[row + x0 as usize..row + x1 as usize].fill(color);
        }
        Ok(())
    }

    fn clear(&mut self, color: Rgb565) -> Result<(), Self::Error> {
        let n = (self.rows() * WIDTH) as usize;
        self.px[..n].fill(color);
        Ok(())
    }
}

/// Render one frame band by band: for each band, draw it into `band` and
/// hand it to `flush(top_row, rows, pixels)`, which sends it to the panel.
/// Stops at the first flush error.
pub fn render_banded<E>(
    ui: &Ui,
    state: &UiState,
    band: &mut Band<'_>,
    mut flush: impl FnMut(i32, i32, &[Rgb565]) -> Result<(), E>,
) -> Result<(), E> {
    let mut top = 0;
    while top < HEIGHT {
        band.set_top(top);
        if band.rows() == 0 {
            break; // a buffer smaller than one row: nothing can be drawn
        }
        // Infallible: Band never errors.
        let _ = render(ui, state, band);
        flush(top, band.rows(), band.pixels())?;
        top += band.rows();
    }
    Ok(())
}
