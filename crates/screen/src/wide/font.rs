//! Anti-aliased bitmap fonts: 4-bit coverage per pixel, rasterised on the
//! laptop from open-licence TTFs by `tools/fontgen` (the tables are in
//! `glyphs.rs` and `glyphs.bin`, both generated and committed). Drawing
//! blends each glyph pixel into whatever is already in the canvas, so text
//! is smooth on any flat background, inverse boxes included.

/// Which characters a font holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// Printable ASCII (32..=126), then `°` and `·`.
    Ascii,
    /// `0-9`, `.`, `-` and space only (the big numerals).
    Digits,
}

/// One glyph's bitmap and placement.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    pub w: u8,
    pub h: u8,
    /// Left edge from the pen position.
    pub x: i8,
    /// Top edge from the baseline (negative: above it).
    pub y: i8,
    /// Advance, 1/16 px.
    pub adv: u16,
    /// Byte offset of the bitmap in the blob: `w * h` nibbles, high nibble
    /// first, row-major.
    pub off: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Font {
    /// Pixel size it was rasterised at.
    pub px: u8,
    /// Baseline below the line's top.
    pub ascent: u8,
    /// Line height (ascent + descent).
    pub height: u8,
    /// Height of a capital H above the baseline.
    pub cap: u8,
    pub charset: Charset,
    pub glyphs: &'static [Glyph],
}

/// The text roles a theme's lettering is drawn in.
#[derive(Debug, Clone, Copy)]
pub struct FontSet {
    /// Small caps labels (13-14 px).
    pub label: Font,
    pub small: Font,
    pub small_b: Font,
    pub body: Font,
    pub body_b: Font,
    pub title: Font,
    pub head: Font,
    /// Data: tabular, monospaced where the family has it.
    pub data: Font,
    pub data_l: Font,
    pub id: Font,
    /// Big numerals, digits only.
    pub num_m: Font,
    pub num_l: Font,
    pub num_xl: Font,
}

impl Font {
    /// The glyph for `c`; `?` stands in for anything missing (Ascii), a
    /// space for anything missing in the digits-only fonts.
    pub fn glyph(&self, c: char) -> &Glyph {
        let i = match self.charset {
            Charset::Ascii => match c {
                ' '..='~' => c as usize - 32,
                '\u{b0}' => 95,
                '\u{b7}' => 96,
                _ => '?' as usize - 32,
            },
            Charset::Digits => match c {
                '0'..='9' => c as usize - '0' as usize,
                '.' => 10,
                '-' => 11,
                _ => 12,
            },
        };
        &self.glyphs[i]
    }

    /// Width of `s` in pixels (rounded advance sum), with `track` extra
    /// pixels between letters.
    pub fn width(&self, s: &str, track: i32) -> i32 {
        let mut adv = 0u32;
        let mut n = 0;
        for c in s.chars() {
            adv += u32::from(self.glyph(c).adv);
            n += 1;
        }
        ((adv + 8) / 16) as i32 + track * (n - 1).max(0)
    }
}
