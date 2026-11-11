//! TOML config for font and colors.
//!
//! A missing file is not an error: the window still opens with [`Config::default`].
//! A file that exists but does not parse is an error, so a typo is visible.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::color::{Palette, Rgb};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub font_family: String,
    pub font_size: f32,
    pub scrollback: usize,
    /// `None` means the platform default shell (`$SHELL` / `%COMSPEC%`).
    pub shell: Option<String>,
    pub palette: Palette,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: "JetBrains Mono".to_string(),
            font_size: 15.0,
            scrollback: 10_000,
            shell: None,
            palette: Palette::default(),
        }
    }
}

impl Config {
    /// Load `explicit`, otherwise `$AITERM_CONFIG`, otherwise the platform path.
    pub fn load(explicit: Option<PathBuf>) -> Result<Self, String> {
        let env_path = std::env::var_os("AITERM_CONFIG").map(PathBuf::from);
        let path = explicit
            .clone()
            .or(env_path.clone())
            .unwrap_or_else(default_config_path);
        if !path.exists() {
            if explicit.is_some() || env_path.is_some() {
                return Err(format!("config file not found: {}", path.display()));
            }
            eprintln!("aiterm: no config at {} — using defaults", path.display());
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .map_err(|err| format!("reading {}: {err}", path.display()))?;
        Self::parse(&text).map_err(|err| format!("{}: {err}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let raw: RawConfig = toml::from_str(text).map_err(|err| err.to_string())?;
        raw.into_config()
    }
}

/// Platform config path. The directory is not created until the user wants a file.
pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        return PathBuf::from(base).join("aiterm");
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        return PathBuf::from(home).join("Library/Application Support/aiterm");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.config")
        });
        PathBuf::from(base).join("aiterm")
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    font_family: String,
    font_size: f32,
    scrollback: usize,
    shell: Option<String>,
    colors: RawColors,
}

impl Default for RawConfig {
    fn default() -> Self {
        let config = Config::default();
        Self {
            font_family: config.font_family,
            font_size: config.font_size,
            scrollback: config.scrollback,
            shell: None,
            colors: RawColors::from_palette(&config.palette),
        }
    }
}

impl RawConfig {
    fn into_config(self) -> Result<Config, String> {
        if self.font_family.trim().is_empty() {
            return Err("font_family is empty".to_string());
        }
        let font_size = self.font_size.clamp(6.0, 72.0);
        let scrollback = self.scrollback.min(200_000);
        let shell = self.shell.and_then(|shell| {
            let shell = shell.trim().to_string();
            if shell.is_empty() {
                None
            } else {
                Some(shell)
            }
        });
        Ok(Config {
            font_family: self.font_family,
            font_size,
            scrollback,
            shell,
            palette: self.colors.into_palette()?,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawColors {
    background: String,
    foreground: String,
    cursor: String,
    selection: String,
    black: String,
    red: String,
    green: String,
    yellow: String,
    blue: String,
    magenta: String,
    cyan: String,
    white: String,
    bright_black: String,
    bright_red: String,
    bright_green: String,
    bright_yellow: String,
    bright_blue: String,
    bright_magenta: String,
    bright_cyan: String,
    bright_white: String,
}

impl RawColors {
    fn from_palette(palette: &Palette) -> Self {
        fn hex(rgb: Rgb) -> String {
            format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)
        }
        Self {
            background: hex(palette.background),
            foreground: hex(palette.foreground),
            cursor: hex(palette.cursor),
            selection: hex(palette.selection),
            black: hex(palette.ansi[0]),
            red: hex(palette.ansi[1]),
            green: hex(palette.ansi[2]),
            yellow: hex(palette.ansi[3]),
            blue: hex(palette.ansi[4]),
            magenta: hex(palette.ansi[5]),
            cyan: hex(palette.ansi[6]),
            white: hex(palette.ansi[7]),
            bright_black: hex(palette.ansi[8]),
            bright_red: hex(palette.ansi[9]),
            bright_green: hex(palette.ansi[10]),
            bright_yellow: hex(palette.ansi[11]),
            bright_blue: hex(palette.ansi[12]),
            bright_magenta: hex(palette.ansi[13]),
            bright_cyan: hex(palette.ansi[14]),
            bright_white: hex(palette.ansi[15]),
        }
    }

    fn into_palette(self) -> Result<Palette, String> {
        let ansi = [
            parse("black", &self.black)?,
            parse("red", &self.red)?,
            parse("green", &self.green)?,
            parse("yellow", &self.yellow)?,
            parse("blue", &self.blue)?,
            parse("magenta", &self.magenta)?,
            parse("cyan", &self.cyan)?,
            parse("white", &self.white)?,
            parse("bright_black", &self.bright_black)?,
            parse("bright_red", &self.bright_red)?,
            parse("bright_green", &self.bright_green)?,
            parse("bright_yellow", &self.bright_yellow)?,
            parse("bright_blue", &self.bright_blue)?,
            parse("bright_magenta", &self.bright_magenta)?,
            parse("bright_cyan", &self.bright_cyan)?,
            parse("bright_white", &self.bright_white)?,
        ];
        Ok(Palette {
            background: parse("background", &self.background)?,
            foreground: parse("foreground", &self.foreground)?,
            cursor: parse("cursor", &self.cursor)?,
            selection: parse("selection", &self.selection)?,
            ansi,
        })
    }
}

impl Default for RawColors {
    fn default() -> Self {
        Self::from_palette(&Palette::default())
    }
}

fn parse(field: &str, text: &str) -> Result<Rgb, String> {
    Rgb::parse(text).map_err(|err| format!("colors.{field}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_partial_config() {
        let empty = Config::parse("").unwrap();
        assert_eq!(empty.font_family, "JetBrains Mono");
        assert_eq!(empty.font_size, 15.0);

        let partial =
            Config::parse("font_size = 18\nfont_family = \"DejaVu Sans Mono\"\n").unwrap();
        assert_eq!(partial.font_size, 18.0);
        assert_eq!(partial.font_family, "DejaVu Sans Mono");
        assert_eq!(partial.palette.background, Palette::default().background);
    }

    #[test]
    fn color_override_and_clamp() {
        let config = Config::parse(
            r##"
            font_size = 1
            scrollback = 999999999
            shell = "  "
            [colors]
            background = "#112233"
            "##,
        )
        .unwrap();
        assert_eq!(config.font_size, 6.0);
        assert_eq!(config.scrollback, 200_000);
        assert_eq!(config.shell, None);
        assert_eq!(config.palette.background, Rgb::new(0x11, 0x22, 0x33));
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(Config::parse("nope = 1\n").is_err());
    }
}
