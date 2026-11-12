//! Palette resolution.
//!
//! The terminal core stores either an override (from OSC) or a named/indexed
//! color. Defaults live here so a config file can theme the 16 ANSI colors,
//! the foreground, the background, and the cursor without teaching the parser
//! about them. 256-color and truecolor values are computed, not configured.

use alacritty_terminal::term::color::{Colors, COUNT};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb as VteRgb};

/// 8-bit sRGB color used by the renderer and the config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn from_vte(rgb: VteRgb) -> Self {
        Self::new(rgb.r, rgb.g, rgb.b)
    }

    pub fn to_vte(self) -> VteRgb {
        VteRgb {
            r: self.r,
            g: self.g,
            b: self.b,
        }
    }

    /// Parse `#RGB`, `#RRGGBB`, or `#RRGGBBAA` (alpha is ignored).
    pub fn parse(text: &str) -> Result<Self, String> {
        let hex = text.trim().trim_start_matches('#');
        let value = u32::from_str_radix(hex, 16)
            .map_err(|_| format!("invalid color {text:?}, expected #RRGGBB"))?;
        match hex.len() {
            3 => {
                let r = ((value >> 8) & 0xf) as u8;
                let g = ((value >> 4) & 0xf) as u8;
                let b = (value & 0xf) as u8;
                Ok(Self::new(r * 17, g * 17, b * 17))
            }
            6 => Ok(Self::new(
                (value >> 16) as u8,
                (value >> 8) as u8,
                value as u8,
            )),
            8 => Ok(Self::new(
                (value >> 24) as u8,
                (value >> 16) as u8,
                (value >> 8) as u8,
            )),
            _ => Err(format!("invalid color {text:?}, expected #RRGGBB")),
        }
    }

    pub fn dim(self) -> Self {
        Self::new(scale(self.r), scale(self.g), scale(self.b))
    }
}

fn scale(channel: u8) -> u8 {
    (f32::from(channel) * 0.66) as u8
}

/// Colors the user can set, plus the derived 256-color cube.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    fn default() -> Self {
        // Catppuccin Mocha. Readable on a dark window and easy to tell apart
        // in a screenshot. Overridden wholesale by the config file.
        Self {
            background: Rgb::new(0x1e, 0x1e, 0x2e),
            foreground: Rgb::new(0xcd, 0xd6, 0xf4),
            cursor: Rgb::new(0xf5, 0xe0, 0xdc),
            selection: Rgb::new(0x45, 0x47, 0x5a),
            ansi: [
                Rgb::new(0x45, 0x47, 0x5a),
                Rgb::new(0xf3, 0x8b, 0xa8),
                Rgb::new(0xa6, 0xe3, 0xa1),
                Rgb::new(0xf9, 0xe2, 0xaf),
                Rgb::new(0x89, 0xb4, 0xfa),
                Rgb::new(0xf5, 0xc2, 0xe7),
                Rgb::new(0x94, 0xe2, 0xd5),
                Rgb::new(0xba, 0xc2, 0xde),
                Rgb::new(0x58, 0x5b, 0x70),
                Rgb::new(0xf3, 0x8b, 0xa8),
                Rgb::new(0xa6, 0xe3, 0xa1),
                Rgb::new(0xf9, 0xe2, 0xaf),
                Rgb::new(0x89, 0xb4, 0xfa),
                Rgb::new(0xf5, 0xc2, 0xe7),
                Rgb::new(0x94, 0xe2, 0xd5),
                Rgb::new(0xa6, 0xad, 0xc8),
            ],
        }
    }
}

impl Palette {
    pub fn named(&self, name: NamedColor) -> Rgb {
        match name {
            NamedColor::Black => self.ansi[0],
            NamedColor::Red => self.ansi[1],
            NamedColor::Green => self.ansi[2],
            NamedColor::Yellow => self.ansi[3],
            NamedColor::Blue => self.ansi[4],
            NamedColor::Magenta => self.ansi[5],
            NamedColor::Cyan => self.ansi[6],
            NamedColor::White => self.ansi[7],
            NamedColor::BrightBlack => self.ansi[8],
            NamedColor::BrightRed => self.ansi[9],
            NamedColor::BrightGreen => self.ansi[10],
            NamedColor::BrightYellow => self.ansi[11],
            NamedColor::BrightBlue => self.ansi[12],
            NamedColor::BrightMagenta => self.ansi[13],
            NamedColor::BrightCyan => self.ansi[14],
            NamedColor::BrightWhite => self.ansi[15],
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimBlack => self.ansi[0].dim(),
            NamedColor::DimRed => self.ansi[1].dim(),
            NamedColor::DimGreen => self.ansi[2].dim(),
            NamedColor::DimYellow => self.ansi[3].dim(),
            NamedColor::DimBlue => self.ansi[4].dim(),
            NamedColor::DimMagenta => self.ansi[5].dim(),
            NamedColor::DimCyan => self.ansi[6].dim(),
            NamedColor::DimWhite => self.ansi[7].dim(),
            NamedColor::DimForeground => self.foreground.dim(),
        }
    }

    pub fn indexed(&self, index: u8) -> Rgb {
        if index < 16 {
            return self.ansi[index as usize];
        }
        if index < 232 {
            let cube = index - 16;
            let r = cube / 36;
            let g = (cube / 6) % 6;
            let b = cube % 6;
            return Rgb::new(cube_level(r), cube_level(g), cube_level(b));
        }
        let gray = 8 + 10 * (index - 232);
        Rgb::new(gray, gray, gray)
    }

    /// Color index used by OSC dynamic color replies (0-255 plus the named slots).
    pub fn dynamic(&self, colors: &Colors, index: usize) -> Rgb {
        if index < COUNT {
            if let Some(rgb) = colors[index] {
                return Rgb::from_vte(rgb);
            }
        }
        if index < 256 {
            return self.indexed(index as u8);
        }
        match index {
            256 => self.foreground,
            257 => self.background,
            258 => self.cursor,
            _ => self.foreground,
        }
    }
}

fn cube_level(level: u8) -> u8 {
    if level == 0 {
        0
    } else {
        55 + 40 * level
    }
}

/// Resolve a cell color, preferring an OSC override over the configured palette.
pub fn resolve(palette: &Palette, colors: &Colors, color: Color) -> Rgb {
    match color {
        Color::Spec(rgb) => Rgb::from_vte(rgb),
        Color::Indexed(index) => {
            lookup(colors, index as usize).unwrap_or_else(|| palette.indexed(index))
        }
        Color::Named(name) => lookup(colors, name as usize).unwrap_or_else(|| palette.named(name)),
    }
}

fn lookup(colors: &Colors, index: usize) -> Option<Rgb> {
    if index >= alacritty_terminal::term::color::COUNT {
        return None;
    }
    colors[index].map(Rgb::from_vte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex() {
        assert_eq!(Rgb::parse("#1e1e2e").unwrap(), Rgb::new(0x1e, 0x1e, 0x2e));
        assert_eq!(Rgb::parse("#abc").unwrap(), Rgb::new(0xaa, 0xbb, 0xcc));
        assert!(Rgb::parse("nope").is_err());
    }

    #[test]
    fn color_cube_and_gray() {
        let palette = Palette::default();
        // Index 196 is cube (5, 0, 0) → pure red.
        assert_eq!(palette.indexed(196), Rgb::new(255, 0, 0));
        // Index 232 is the first gray, 8.
        assert_eq!(palette.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(palette.indexed(1), palette.ansi[1]);
    }
}
