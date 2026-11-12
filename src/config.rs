//! TOML config for font and colors.
//!
//! A missing file is not an error: the window still opens with [`Config::default`].
//! A file that exists but does not parse is an error, so a typo is visible.

use std::collections::BTreeMap;
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
    pub cwd: Option<PathBuf>,
    pub default_profile: Option<String>,
    pub profiles: BTreeMap<String, Profile>,
}

/// Fully resolved settings retained by one terminal session.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub font_family: String,
    pub font_size: f32,
    pub scrollback: usize,
    pub shell: Option<String>,
    pub cwd: Option<PathBuf>,
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
            cwd: None,
            default_profile: None,
            profiles: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Resolve a named profile, or the global settings when `name` is absent.
    /// Selection of `default_profile` is the caller's responsibility.
    pub fn profile(&self, name: Option<&str>) -> Result<Profile, String> {
        match name {
            Some(name) => self
                .profiles
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unknown profile {name:?}")),
            None => Ok(Profile {
                font_family: self.font_family.clone(),
                font_size: self.font_size,
                scrollback: self.scrollback,
                shell: self.shell.clone(),
                cwd: self.cwd.clone(),
                palette: self.palette.clone(),
            }),
        }
    }

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
        PathBuf::from(base).join("aiterm")
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join("Library/Application Support/aiterm")
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
    cwd: Option<String>,
    default_profile: Option<String>,
    profiles: BTreeMap<String, toml::Value>,
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
            cwd: None,
            default_profile: None,
            profiles: BTreeMap::new(),
            colors: RawColors::default(),
        }
    }
}

impl RawConfig {
    fn into_config(self) -> Result<Config, String> {
        validate_font_family(&self.font_family)?;
        let mut config = Config {
            font_family: self.font_family,
            font_size: font_size(self.font_size)?,
            scrollback: self.scrollback.min(200_000),
            shell: self.shell.and_then(normalize_shell),
            cwd: self.cwd.map(parse_cwd).transpose()?,
            palette: self.colors.into_palette(&Palette::default())?,
            default_profile: self.default_profile,
            profiles: BTreeMap::new(),
        };
        let base = config.profile(None)?;
        for (name, value) in self.profiles {
            if name.trim().is_empty() {
                return Err("profile name is empty".to_string());
            }
            let raw: RawProfile = value
                .try_into()
                .map_err(|err| format!("profiles.{name}: {err}"))?;
            let profile = raw
                .resolve(&base)
                .map_err(|err| format!("profiles.{name}: {err}"))?;
            config.profiles.insert(name, profile);
        }
        if let Some(name) = &config.default_profile {
            if name.trim().is_empty() {
                return Err("default_profile is empty".to_string());
            }
            if !config.profiles.contains_key(name) {
                return Err(format!(
                    "default_profile refers to unknown profile {name:?}"
                ));
            }
        }
        Ok(config)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawProfile {
    font_family: Option<String>,
    font_size: Option<f32>,
    scrollback: Option<usize>,
    shell: Option<String>,
    cwd: Option<String>,
    colors: RawColors,
}

impl RawProfile {
    fn resolve(self, base: &Profile) -> Result<Profile, String> {
        let mut profile = base.clone();
        if let Some(family) = self.font_family {
            validate_font_family(&family)?;
            profile.font_family = family;
        }
        if let Some(size) = self.font_size {
            profile.font_size = font_size(size)?;
        }
        if let Some(scrollback) = self.scrollback {
            profile.scrollback = scrollback.min(200_000);
        }
        if let Some(shell) = self.shell {
            profile.shell = normalize_shell(shell);
        }
        if let Some(cwd) = self.cwd {
            profile.cwd = Some(parse_cwd(cwd)?);
        }
        profile.palette = self.colors.into_palette(&base.palette)?;
        Ok(profile)
    }
}

fn validate_font_family(family: &str) -> Result<(), String> {
    if family.trim().is_empty() {
        Err("font_family is empty".to_string())
    } else {
        Ok(())
    }
}

fn font_size(size: f32) -> Result<f32, String> {
    if !size.is_finite() {
        return Err("font_size must be finite".to_string());
    }
    Ok(size.clamp(6.0, 72.0))
}

fn normalize_shell(shell: String) -> Option<String> {
    let shell = shell.trim().to_string();
    if shell.is_empty() {
        None
    } else {
        Some(shell)
    }
}

fn parse_cwd(cwd: String) -> Result<PathBuf, String> {
    if cwd.trim().is_empty() {
        Err("cwd is empty".to_string())
    } else {
        Ok(PathBuf::from(cwd))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawColors {
    background: Option<String>,
    foreground: Option<String>,
    cursor: Option<String>,
    selection: Option<String>,
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
    bright_black: Option<String>,
    bright_red: Option<String>,
    bright_green: Option<String>,
    bright_yellow: Option<String>,
    bright_blue: Option<String>,
    bright_magenta: Option<String>,
    bright_cyan: Option<String>,
    bright_white: Option<String>,
}

impl RawColors {
    fn into_palette(self, base: &Palette) -> Result<Palette, String> {
        fn apply(field: &str, text: Option<String>, fallback: Rgb) -> Result<Rgb, String> {
            match text {
                Some(text) => Rgb::parse(&text).map_err(|err| format!("colors.{field}: {err}")),
                None => Ok(fallback),
            }
        }
        Ok(Palette {
            background: apply("background", self.background, base.background)?,
            foreground: apply("foreground", self.foreground, base.foreground)?,
            cursor: apply("cursor", self.cursor, base.cursor)?,
            selection: apply("selection", self.selection, base.selection)?,
            ansi: [
                apply("black", self.black, base.ansi[0])?,
                apply("red", self.red, base.ansi[1])?,
                apply("green", self.green, base.ansi[2])?,
                apply("yellow", self.yellow, base.ansi[3])?,
                apply("blue", self.blue, base.ansi[4])?,
                apply("magenta", self.magenta, base.ansi[5])?,
                apply("cyan", self.cyan, base.ansi[6])?,
                apply("white", self.white, base.ansi[7])?,
                apply("bright_black", self.bright_black, base.ansi[8])?,
                apply("bright_red", self.bright_red, base.ansi[9])?,
                apply("bright_green", self.bright_green, base.ansi[10])?,
                apply("bright_yellow", self.bright_yellow, base.ansi[11])?,
                apply("bright_blue", self.bright_blue, base.ansi[12])?,
                apply("bright_magenta", self.bright_magenta, base.ansi[13])?,
                apply("bright_cyan", self.bright_cyan, base.ansi[14])?,
                apply("bright_white", self.bright_white, base.ansi[15])?,
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_example_loads_with_inherited_profile_settings() {
        let config = Config::parse(include_str!("../config.example.toml")).unwrap();
        let profile = config.profile(Some("large")).unwrap();
        assert_eq!(profile.font_size, 18.0);
        assert_eq!(profile.font_family, config.font_family);
        assert_eq!(profile.palette.foreground, config.palette.foreground);
    }

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

    #[test]
    fn profiles_inherit_globals_and_override_only_supplied_fields() {
        let config = Config::parse(
            r##"
            font_family = "Global Mono"
            font_size = 18
            scrollback = 1234
            shell = "global-shell"
            cwd = "global directory"
            [colors]
            background = "#112233"
            red = "#223344"
            [profiles.work]
            shell = "work-shell"
            font_size = 24
            cwd = "work directory"
            [profiles.work.colors]
            foreground = "#445566"
            [profiles.inherited]
        "##,
        )
        .unwrap();
        let base = config.profile(None).unwrap();
        assert_eq!(config.profile(Some("inherited")).unwrap(), base);
        let work = config.profile(Some("work")).unwrap();
        assert_eq!(work.shell.as_deref(), Some("work-shell"));
        assert_eq!(work.cwd, Some(PathBuf::from("work directory")));
        assert_eq!(work.font_family, "Global Mono");
        assert_eq!(work.font_size, 24.0);
        assert_eq!(work.scrollback, 1234);
        assert_eq!(work.palette.background, Rgb::new(0x11, 0x22, 0x33));
        assert_eq!(work.palette.ansi[1], Rgb::new(0x22, 0x33, 0x44));
        assert_eq!(work.palette.foreground, Rgb::new(0x44, 0x55, 0x66));
        assert_eq!(base.shell.as_deref(), Some("global-shell"));
        assert_eq!(base.palette.foreground, Palette::default().foreground);
    }

    #[test]
    fn default_profile_is_validated_without_changing_global_resolution() {
        let config = Config::parse(
            r#"
            default_profile = "large"
            [profiles.large]
            font_size = 30
        "#,
        )
        .unwrap();
        assert_eq!(config.default_profile.as_deref(), Some("large"));
        assert_eq!(config.profile(None).unwrap().font_size, 15.0);
        assert_eq!(config.profile(Some("large")).unwrap().font_size, 30.0);
        assert!(config
            .profile(Some("missing"))
            .unwrap_err()
            .contains("missing"));
        assert!(Config::parse("default_profile = 'missing'")
            .unwrap_err()
            .contains("default_profile"));
        assert!(Config::parse("default_profile = ''").is_err());
    }

    #[test]
    fn profile_errors_include_profile_name_and_reject_unknown_fields() {
        for text in [
            "[profiles.bad]\nnope = 1",
            "[profiles.bad]\nfont_family = ''",
            "[profiles.bad.colors]\nbackground = 'invalid'",
            "[profiles.bad.colors]\nnope = '#112233'",
            "[profiles.bad]\nextends = 'other'",
            "[profiles.bad]\nfont_size = 'large'",
            "profiles.bad = 42",
        ] {
            let error = Config::parse(text).unwrap_err();
            assert!(error.contains("profiles.bad"), "{text}: {error}");
        }
        assert!(Config::parse("[profiles.'']")
            .unwrap_err()
            .contains("name is empty"));
        assert!(Config::parse("[profiles.'   ']").is_err());
    }

    #[test]
    fn sizes_are_finite_and_clamped_for_globals_and_profiles() {
        for value in ["nan", "+nan", "-nan", "inf", "+inf", "-inf"] {
            assert!(Config::parse(&format!("font_size = {value}"))
                .unwrap_err()
                .contains("finite"));
            let error = Config::parse(&format!("[profiles.bad]\nfont_size = {value}")).unwrap_err();
            assert!(
                error.contains("profiles.bad") && error.contains("finite"),
                "{error}"
            );
        }
        let config = Config::parse("[profiles.small]\nfont_size = 1\n[profiles.large]\nfont_size = 999\nscrollback = 9999999").unwrap();
        assert_eq!(config.profile(Some("small")).unwrap().font_size, 6.0);
        assert_eq!(config.profile(Some("large")).unwrap().font_size, 72.0);
        assert_eq!(config.profile(Some("large")).unwrap().scrollback, 200_000);
    }

    #[test]
    fn cwd_is_literal_and_empty_values_are_rejected() {
        assert!(Config::parse("cwd = '   '")
            .unwrap_err()
            .contains("cwd is empty"));
        let error = Config::parse("[profiles.bad]\ncwd = ''").unwrap_err();
        assert!(error.contains("profiles.bad") && error.contains("cwd is empty"));
        let config = Config::parse(
            r#"
            cwd = 'relative folder'
            shell = "global-shell"
            [profiles.windows]
            cwd = 'C:\Users\A User\Project'
            shell = " "
        "#,
        )
        .unwrap();
        assert_eq!(config.cwd, Some(PathBuf::from("relative folder")));
        let windows = config.profile(Some("windows")).unwrap();
        assert_eq!(windows.cwd, Some(PathBuf::from(r"C:\Users\A User\Project")));
        assert_eq!(windows.shell, None);
    }
}
