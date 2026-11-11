//! Monospace font loading.
//!
//! Faces are located without scanning every system font: fontconfig on Unix,
//! and a filename search of the standard font directories elsewhere. Only the
//! regular face is required. Bold and italic are used when a distinct file
//! exists; otherwise the renderer fakes bold with a one-pixel overstrike and
//! draws italic as regular.

use std::path::{Path, PathBuf};
use std::process::Command;

use fontdue::Font;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaceKind {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

pub struct FontSet {
    faces: [Option<Font>; 4],
    pub px: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub ascent: f32,
}

pub struct Raster {
    pub metrics: fontdue::Metrics,
    pub bitmap: Vec<u8>,
    pub fake_bold: bool,
}

impl FontSet {
    pub fn load(family: &str, px: f32) -> Result<Self, String> {
        let px = px.clamp(6.0, 128.0);
        let regular = load_kind(family, FaceKind::Regular).ok_or_else(|| {
            format!(
                "could not find a monospace font for {family:?}. \
                 Install JetBrains Mono, DejaVu Sans Mono, Consolas, or set font_family."
            )
        })?;
        let bold = load_kind(family, FaceKind::Bold).filter(|path| path != &regular);
        let italic = load_kind(family, FaceKind::Italic).filter(|path| path != &regular);
        let bold_italic = load_kind(family, FaceKind::BoldItalic).filter(|path| path != &regular);

        let regular_font = read_font(&regular)?;
        let faces = [
            Some(regular_font),
            bold.as_ref().and_then(|path| read_font(path).ok()),
            italic.as_ref().and_then(|path| read_font(path).ok()),
            bold_italic.as_ref().and_then(|path| read_font(path).ok()),
        ];
        let metrics = metrics_for(faces[0].as_ref().expect("regular face"), px);
        Ok(Self {
            faces,
            px: metrics.px,
            cell_width: metrics.cell_width,
            cell_height: metrics.cell_height,
            ascent: metrics.ascent,
        })
    }

    pub fn rasterize(&self, ch: char, style: u8) -> Raster {
        let style = style.min(3) as usize;
        let (face, fake_bold) = match &self.faces[style] {
            Some(face) => (face, false),
            None => {
                let fallback = self.faces[0].as_ref().expect("regular face");
                let fake = style == 1 || style == 3;
                (fallback, fake)
            }
        };
        let (metrics, bitmap) = face.rasterize(ch, self.px);
        Raster {
            metrics,
            bitmap,
            fake_bold,
        }
    }
}

struct BuiltMetrics {
    px: f32,
    cell_width: f32,
    cell_height: f32,
    ascent: f32,
}

fn metrics_for(font: &Font, px: f32) -> BuiltMetrics {
    let line = font.horizontal_line_metrics(px);
    let (ascent, descent) = match line {
        Some(line) => (line.ascent.ceil().max(1.0), (-line.descent).ceil().max(0.0)),
        None => (px.ceil(), (px * 0.2).ceil()),
    };
    let advance = font
        .metrics('M', px)
        .advance_width
        .max(font.metrics('0', px).advance_width);
    let cell_width = advance.ceil().max(1.0);
    // One pixel of leading so underscores and descenders do not collide.
    let cell_height = (ascent + descent + 1.0).max(1.0);
    BuiltMetrics {
        px,
        cell_width,
        cell_height,
        ascent,
    }
}

fn read_font(path: &Path) -> Result<Font, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("reading {}: {err}", path.display()))?;
    Font::from_bytes(bytes, fontdue::FontSettings::default())
        .map_err(|err| format!("parsing {}: {err}", path.display()))
}

fn load_kind(family: &str, kind: FaceKind) -> Option<PathBuf> {
    if let Some(path) = fontconfig(family, kind) {
        return Some(path);
    }
    // fontconfig's monospace fallback, then a directory walk for Windows/macOS
    // and for Linux systems without fontconfig.
    if let Some(path) = fontconfig("monospace", kind) {
        return Some(path);
    }
    scan_dirs(family, kind).or_else(|| scan_dirs("monospace", kind))
}

fn fontconfig(family: &str, kind: FaceKind) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let pattern = format!("{family}:style={}", kind.style_name());
        let output = Command::new("fc-match")
            .args(["-f", "%{file}\n%{index}\n%{family}\n%{style}\n", &pattern])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let file = lines.next()?.trim();
        let index: u32 = lines.next()?.trim().parse().ok()?;
        let family_got = lines.next()?.trim();
        let style = lines.next().unwrap_or("").trim();
        // fontdue reads face 0 of a file. Collections (index != 0) are skipped.
        if file.is_empty() || index != 0 || !kind.style_matches(style) {
            return None;
        }
        if family != "monospace" && !family_matches(family_got, family) {
            return None;
        }
        let path = PathBuf::from(file);
        if path.is_file() {
            Some(path)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (family, kind);
        None
    }
}

fn family_matches(got: &str, want: &str) -> bool {
    let want_n = normalize(want);
    if want_n.is_empty() {
        return false;
    }
    got.split(',').any(|part| {
        let part_n = normalize(part);
        part_n == want_n || part_n.contains(&want_n)
    })
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

impl FaceKind {
    fn style_name(self) -> &'static str {
        match self {
            Self::Regular => "Regular",
            Self::Bold => "Bold",
            Self::Italic => "Italic",
            Self::BoldItalic => "Bold Italic",
        }
    }

    fn style_matches(self, style: &str) -> bool {
        let style = style.to_lowercase();
        let italic = style.contains("italic") || style.contains("oblique");
        // "bolditalic" and "semibold" both contain "bold"; that is what we want
        // for filenames like JetBrainsMono-BoldItalic.ttf.
        let bold = style.contains("bold");
        match self {
            Self::Regular => !italic && !bold,
            Self::Bold => bold && !italic,
            Self::Italic => italic && !bold,
            Self::BoldItalic => italic && bold,
        }
    }

    fn filename_matches(self, file_name: &str) -> bool {
        self.style_matches(file_name)
    }
}

fn scan_dirs(family: &str, kind: FaceKind) -> Option<PathBuf> {
    let needles = family_needles(family);
    if needles.is_empty() {
        return None;
    }
    for dir in font_dirs() {
        if let Some(path) = walk(&dir, &needles, kind, 0) {
            return Some(path);
        }
    }
    None
}

fn family_needles(family: &str) -> Vec<String> {
    let mut needles = Vec::new();
    let normalized = normalize(family);
    if !normalized.is_empty() && normalized != "monospace" {
        needles.push(normalized);
    }
    match normalize(family).as_str() {
        "consolas" => needles.push("consola".to_string()),
        "cascadiamono" | "cascadiacode" => needles.push("cascadia".to_string()),
        "couriernew" => {
            needles.push("cour".to_string());
            needles.push("courier".to_string());
        }
        "monospace" | "" => {
            needles.push("jetbrainsmono".to_string());
            needles.push("dejavusansmono".to_string());
            needles.push("liberationmono".to_string());
            needles.push("cascadiamono".to_string());
            needles.push("consola".to_string());
        }
        _ => {}
    }
    needles
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(windir) = std::env::var("WINDIR") {
            dirs.push(PathBuf::from(windir).join("Fonts"));
        }
        dirs.push(PathBuf::from(r"C:\Windows\Fonts"));
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/System/Library/Fonts/Supplemental"));
        dirs.push(PathBuf::from("/Library/Fonts"));
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(home).join(".local/share/fonts"));
        }
    }
    dirs
}

fn walk(dir: &Path, needles: &[String], kind: FaceKind, depth: u8) -> Option<PathBuf> {
    if depth > 4 || !dir.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk(&path, needles, kind, depth + 1) {
                return Some(found);
            }
            continue;
        }
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let lower = name.to_lowercase();
        if !lower.ends_with(".ttf") && !lower.ends_with(".otf") {
            continue;
        }
        let normalized = normalize(name);
        if !needles.iter().any(|needle| normalized.contains(needle)) {
            continue;
        }
        if kind.filename_matches(name) {
            return Some(path);
        }
    }
    None
}
