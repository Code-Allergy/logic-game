//! Drawing a [`GateKind::Screen`]: its palettes, the built-in text font
//! and the rasterizer that turns a copy of its video memory into RGBA
//! texels. Pure functions, tested without rendering; the visuals keep one
//! texture per screen and call [`rasterize`] only when the memory changed.
//!
//! Value 0 is transparent in every palette, so the dark screen set into
//! the tile shows through (the same in the day and night themes).

use super::kind::{GateKind, ScreenMode, ScreenPalette, TEXT_CELL};

/// The lit colour of a mono screen and of text: the theme's amber "on"
/// (`Palette::on`, the same in both themes).
pub const PHOSPHOR: [u8; 4] = [0xFF, 0xB6, 0x27, 0xFF];

const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// The 16 colours of [`ScreenPalette::Colors16`], IRGB: bit 0 blue, bit 1
/// green, bit 2 red, bit 3 bright. Index 0 (black) is drawn transparent.
pub const COLORS16: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x1D, 0x3F, 0xBB],
    [0x1E, 0x9E, 0x3A],
    [0x14, 0x9C, 0xA6],
    [0xB8, 0x2A, 0x2A],
    [0x9A, 0x37, 0xB0],
    [0xB8, 0x74, 0x1E],
    [0xB4, 0xBA, 0xC4],
    [0x5A, 0x63, 0x73],
    [0x4F, 0x86, 0xFF],
    [0x5C, 0xE0, 0x6E],
    [0x5C, 0xE6, 0xF0],
    [0xFF, 0x5C, 0x5C],
    [0xF0, 0x6E, 0xFF],
    [0xFF, 0xE0, 0x4A],
    [0xFF, 0xFF, 0xFF],
];

/// RGBA of `value` in `palette` (masked to the palette's bits).
pub fn colour(palette: ScreenPalette, value: u64) -> [u8; 4] {
    let v = (value & ((1 << palette.bits()) - 1)) as usize;
    if v == 0 {
        return CLEAR;
    }
    match palette {
        ScreenPalette::Mono => PHOSPHOR,
        ScreenPalette::Gray => {
            let g = (v * 0x55) as u8;
            [g, g, g, 0xFF]
        }
        ScreenPalette::Colors16 => {
            let [r, g, b] = COLORS16[v];
            [r, g, b, 0xFF]
        }
        ScreenPalette::Rgb332 => {
            let scale = |x: usize, max: usize| (x * 255 / max) as u8;
            [
                scale(v >> 5 & 7, 7),
                scale(v >> 2 & 7, 7),
                scale(v & 3, 3),
                0xFF,
            ]
        }
    }
}

/// The built-in font: printable ASCII (0x20 to 0x7E), 5 columns per
/// character, bit 0 the top row (bit 7 a descender row).
const FONT: [[u8; 5]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00], // ' '
    [0x00, 0x00, 0x5F, 0x00, 0x00], // !
    [0x00, 0x07, 0x00, 0x07, 0x00], // "
    [0x14, 0x7F, 0x14, 0x7F, 0x14], // #
    [0x24, 0x2A, 0x7F, 0x2A, 0x12], // $
    [0x23, 0x13, 0x08, 0x64, 0x62], // %
    [0x36, 0x49, 0x56, 0x20, 0x50], // &
    [0x00, 0x05, 0x03, 0x00, 0x00], // '
    [0x00, 0x1C, 0x22, 0x41, 0x00], // (
    [0x00, 0x41, 0x22, 0x1C, 0x00], // )
    [0x2A, 0x1C, 0x7F, 0x1C, 0x2A], // *
    [0x08, 0x08, 0x3E, 0x08, 0x08], // +
    [0x00, 0x50, 0x30, 0x00, 0x00], // ,
    [0x08, 0x08, 0x08, 0x08, 0x08], // -
    [0x00, 0x60, 0x60, 0x00, 0x00], // .
    [0x20, 0x10, 0x08, 0x04, 0x02], // /
    [0x3E, 0x51, 0x49, 0x45, 0x3E], // 0
    [0x00, 0x42, 0x7F, 0x40, 0x00], // 1
    [0x42, 0x61, 0x51, 0x49, 0x46], // 2
    [0x21, 0x41, 0x45, 0x4B, 0x31], // 3
    [0x18, 0x14, 0x12, 0x7F, 0x10], // 4
    [0x27, 0x45, 0x45, 0x45, 0x39], // 5
    [0x3C, 0x4A, 0x49, 0x49, 0x30], // 6
    [0x01, 0x71, 0x09, 0x05, 0x03], // 7
    [0x36, 0x49, 0x49, 0x49, 0x36], // 8
    [0x06, 0x49, 0x49, 0x29, 0x1E], // 9
    [0x00, 0x36, 0x36, 0x00, 0x00], // :
    [0x00, 0x56, 0x36, 0x00, 0x00], // ;
    [0x08, 0x14, 0x22, 0x41, 0x00], // <
    [0x14, 0x14, 0x14, 0x14, 0x14], // =
    [0x00, 0x41, 0x22, 0x14, 0x08], // >
    [0x02, 0x01, 0x51, 0x09, 0x06], // ?
    [0x32, 0x49, 0x79, 0x41, 0x3E], // @
    [0x7E, 0x11, 0x11, 0x11, 0x7E], // A
    [0x7F, 0x49, 0x49, 0x49, 0x36], // B
    [0x3E, 0x41, 0x41, 0x41, 0x22], // C
    [0x7F, 0x41, 0x41, 0x22, 0x1C], // D
    [0x7F, 0x49, 0x49, 0x49, 0x41], // E
    [0x7F, 0x09, 0x09, 0x09, 0x01], // F
    [0x3E, 0x41, 0x49, 0x49, 0x7A], // G
    [0x7F, 0x08, 0x08, 0x08, 0x7F], // H
    [0x00, 0x41, 0x7F, 0x41, 0x00], // I
    [0x20, 0x40, 0x41, 0x3F, 0x01], // J
    [0x7F, 0x08, 0x14, 0x22, 0x41], // K
    [0x7F, 0x40, 0x40, 0x40, 0x40], // L
    [0x7F, 0x02, 0x0C, 0x02, 0x7F], // M
    [0x7F, 0x04, 0x08, 0x10, 0x7F], // N
    [0x3E, 0x41, 0x41, 0x41, 0x3E], // O
    [0x7F, 0x09, 0x09, 0x09, 0x06], // P
    [0x3E, 0x41, 0x51, 0x21, 0x5E], // Q
    [0x7F, 0x09, 0x19, 0x29, 0x46], // R
    [0x46, 0x49, 0x49, 0x49, 0x31], // S
    [0x01, 0x01, 0x7F, 0x01, 0x01], // T
    [0x3F, 0x40, 0x40, 0x40, 0x3F], // U
    [0x1F, 0x20, 0x40, 0x20, 0x1F], // V
    [0x3F, 0x40, 0x38, 0x40, 0x3F], // W
    [0x63, 0x14, 0x08, 0x14, 0x63], // X
    [0x07, 0x08, 0x70, 0x08, 0x07], // Y
    [0x61, 0x51, 0x49, 0x45, 0x43], // Z
    [0x00, 0x7F, 0x41, 0x41, 0x00], // [
    [0x02, 0x04, 0x08, 0x10, 0x20], // backslash
    [0x00, 0x41, 0x41, 0x7F, 0x00], // ]
    [0x04, 0x02, 0x01, 0x02, 0x04], // ^
    [0x40, 0x40, 0x40, 0x40, 0x40], // _
    [0x00, 0x01, 0x02, 0x04, 0x00], // `
    [0x20, 0x54, 0x54, 0x54, 0x78], // a
    [0x7F, 0x48, 0x44, 0x44, 0x38], // b
    [0x38, 0x44, 0x44, 0x44, 0x20], // c
    [0x38, 0x44, 0x44, 0x48, 0x7F], // d
    [0x38, 0x54, 0x54, 0x54, 0x18], // e
    [0x08, 0x7E, 0x09, 0x01, 0x02], // f
    [0x18, 0xA4, 0xA4, 0xA4, 0x7C], // g
    [0x7F, 0x08, 0x04, 0x04, 0x78], // h
    [0x00, 0x44, 0x7D, 0x40, 0x00], // i
    [0x40, 0x80, 0x84, 0x7D, 0x00], // j
    [0x7F, 0x10, 0x28, 0x44, 0x00], // k
    [0x00, 0x41, 0x7F, 0x40, 0x00], // l
    [0x7C, 0x04, 0x18, 0x04, 0x78], // m
    [0x7C, 0x08, 0x04, 0x04, 0x78], // n
    [0x38, 0x44, 0x44, 0x44, 0x38], // o
    [0xFC, 0x24, 0x24, 0x24, 0x18], // p
    [0x18, 0x24, 0x24, 0x24, 0xFC], // q
    [0x7C, 0x08, 0x04, 0x04, 0x08], // r
    [0x48, 0x54, 0x54, 0x54, 0x20], // s
    [0x04, 0x3F, 0x44, 0x40, 0x20], // t
    [0x3C, 0x40, 0x40, 0x20, 0x7C], // u
    [0x1C, 0x20, 0x40, 0x20, 0x1C], // v
    [0x3C, 0x40, 0x30, 0x40, 0x3C], // w
    [0x44, 0x28, 0x10, 0x28, 0x44], // x
    [0x1C, 0xA0, 0xA0, 0xA0, 0x7C], // y
    [0x44, 0x64, 0x54, 0x4C, 0x44], // z
    [0x00, 0x08, 0x36, 0x41, 0x00], // {
    [0x00, 0x00, 0x7F, 0x00, 0x00], // |
    [0x00, 0x41, 0x36, 0x08, 0x00], // }
    [0x08, 0x04, 0x08, 0x10, 0x08], // ~
];

/// Whether pixel `(x, y)` (0..8 each, `y` down) of character `code` is
/// lit. Bit 7 of the code inverts the cell; codes outside printable ASCII
/// (0 included, so empty memory is a blank screen) are blank, 0x7F a
/// solid block.
pub fn glyph_pixel(code: u8, x: u32, y: u32) -> bool {
    let inverse = code & 0x80 != 0;
    let c = code & 0x7F;
    let lit = match c {
        0x20..=0x7E if (1..=5).contains(&x) && y < 8 => {
            FONT[usize::from(c - 0x20)][(x - 1) as usize] >> y & 1 == 1
        }
        0x7F => true,
        _ => false,
    };
    lit != inverse
}

/// The texture size of a screen: [`GateKind::screen_pixels`].
pub fn texture_size(kind: &GateKind) -> Option<(u32, u32)> {
    kind.screen_pixels()
}

/// Write the RGBA8 texels of `kind` showing memory `mem` into `out`
/// (resized to fit), row 0 at the top. Words past the end of `mem` read 0.
pub fn rasterize(kind: &GateKind, mem: &[u64], out: &mut Vec<u8>) {
    let GateKind::Screen {
        mode,
        width,
        palette,
        ..
    } = kind
    else {
        return;
    };
    let Some((pw, ph)) = kind.screen_pixels() else {
        return;
    };
    out.clear();
    out.reserve((pw * ph * 4) as usize);
    let word = |i: usize| mem.get(i).copied().unwrap_or(0);
    let cols = usize::from(*width);
    if *mode == ScreenMode::Text {
        let cell = u32::from(TEXT_CELL);
        for y in 0..ph {
            for x in 0..pw {
                let code = word((y / cell) as usize * cols + (x / cell) as usize) as u8;
                out.extend(if glyph_pixel(code, x % cell, y % cell) {
                    PHOSPHOR
                } else {
                    CLEAR
                });
            }
        }
    } else {
        let lut: Vec<[u8; 4]> = (0..1u64 << palette.bits())
            .map(|v| colour(*palette, v))
            .collect();
        let mask = (1u64 << palette.bits()) - 1;
        for i in 0..(pw * ph) as usize {
            out.extend(lut[(word(i) & mask) as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(mode: ScreenMode, width: u16, height: u16, palette: ScreenPalette) -> GateKind {
        GateKind::Screen {
            mode,
            width,
            height,
            palette,
            scale: 2,
        }
    }

    /// The font as text, for eyeballing: `cargo test font_sheet --
    /// --nocapture`.
    fn sheet(text: &str) -> String {
        let mut s = String::new();
        for y in 0..8 {
            for c in text.bytes() {
                for x in 0..8 {
                    s.push(if glyph_pixel(c, x, y) { '#' } else { '.' });
                }
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn font_sheet() {
        let all: String = (0x20u8..0x7F).map(char::from).collect();
        for chunk in all.as_bytes().chunks(16) {
            println!("{}", sheet(std::str::from_utf8(chunk).unwrap()));
        }
        // "H": two posts and a bar, in columns 1 and 5 with a gap of one
        // column to the next cell.
        let h = sheet("H");
        assert_eq!(h.lines().next(), Some(".#...#.."));
        assert_eq!(h.lines().nth(3), Some(".#####.."));
        assert_eq!(h.lines().nth(7), Some("........"));
        // Every printable character but the space has ink; nothing
        // touches column 0 or 6..8.
        for c in 0x21u8..0x7F {
            assert!((0..8).any(|y| (0..8).any(|x| glyph_pixel(c, x, y))), "{c}");
            for y in 0..8 {
                assert!(!glyph_pixel(c, 0, y) && !glyph_pixel(c, 6, y) && !glyph_pixel(c, 7, y));
            }
        }
        // Blank below the space, inverted with bit 7.
        assert!((0..8).all(|y| (0..8).all(|x| !glyph_pixel(0, x, y))));
        assert!((0..8).all(|y| (0..8).all(|x| glyph_pixel(0x80, x, y))));
        assert_eq!(glyph_pixel(b'H' | 0x80, 1, 0), !glyph_pixel(b'H', 1, 0));
    }

    #[test]
    fn palettes_keep_zero_dark_and_decode_bits() {
        for p in ScreenPalette::ALL {
            assert_eq!(colour(p, 0), CLEAR, "{p:?}");
            assert_eq!(colour(p, 1 << p.bits()), CLEAR, "{p:?} masks");
        }
        assert_eq!(colour(ScreenPalette::Mono, 1), PHOSPHOR);
        assert_eq!(colour(ScreenPalette::Gray, 3), [255, 255, 255, 255]);
        assert_eq!(colour(ScreenPalette::Colors16, 15), [255, 255, 255, 255]);
        assert_eq!(colour(ScreenPalette::Rgb332, 0xE0), [255, 0, 0, 255]);
        assert_eq!(colour(ScreenPalette::Rgb332, 0x1C), [0, 255, 0, 255]);
        assert_eq!(colour(ScreenPalette::Rgb332, 0x03), [0, 0, 255, 255]);
        // IRGB: red is bit 2, bright red bit 3 too.
        let red = colour(ScreenPalette::Colors16, 4);
        assert!(red[0] > red[1] && red[0] > red[2]);
        let bright = colour(ScreenPalette::Colors16, 12);
        assert!(bright[0] >= red[0]);
    }

    #[test]
    fn rasterize_places_pixels_row_major_from_the_top() {
        let kind = screen(ScreenMode::Framebuffer, 8, 8, ScreenPalette::Colors16);
        let mut mem = vec![0u64; 64];
        mem[8 + 3] = 12; // (3, 1)
        let mut out = Vec::new();
        rasterize(&kind, &mem, &mut out);
        assert_eq!(out.len(), 64 * 4);
        let at = |x: usize, y: usize| &out[(y * 8 + x) * 4..][..4];
        assert_eq!(at(3, 1), colour(ScreenPalette::Colors16, 12));
        assert_eq!(at(0, 0), CLEAR);
        // A short memory reads 0 past its end.
        rasterize(&kind, &mem[..4], &mut out);
        assert_eq!(out.len(), 64 * 4);
    }

    #[test]
    fn text_draws_eight_by_eight_cells() {
        let kind = screen(ScreenMode::Text, 4, 4, ScreenPalette::Mono);
        assert_eq!(texture_size(&kind), Some((32, 32)));
        let mut mem = vec![0u64; 16];
        mem[4 + 1] = u64::from(b'H'); // cell (1, 1)
        let mut out = Vec::new();
        rasterize(&kind, &mem, &mut out);
        let lit = |x: u32, y: u32| out[((y * 32 + x) * 4 + 3) as usize] != 0;
        // The H's left post at pixel column 8 + 1, rows 8..15.
        assert!(lit(9, 8) && lit(9, 14));
        assert!(!lit(10, 8), "the H is open at the top");
        assert!(!lit(1, 1), "cell (0, 0) is blank");
    }
}
