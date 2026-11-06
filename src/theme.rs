//! Colors and font, read from `DankMaterialShell`'s generated Material 3 palette.
//!
//! DMS (via matugen) writes `~/.cache/DankMaterialShell/dms-colors.json` whenever the
//! wallpaper or theme changes. We map its Material roles onto the keypad and fall back to a
//! built-in palette when the file is missing or unreadable.

// `0xRRGGBB` colors read best without separators.
#![allow(clippy::unreadable_literal)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

use gpui::{Rgba, rgb};
use serde::Deserialize;

/// Colors for one kind of key.
#[derive(Clone, Copy)]
pub struct KeyColors {
    pub bg: Rgba,
    pub hover: Rgba,
    pub pressed: Rgba,
    pub fg: Rgba,
}

impl KeyColors {
    /// Material 3 state layers: content color over the container at 8% (hover) / 12% (press).
    fn new(bg: u32, fg: u32) -> Self {
        Self {
            bg: rgb(bg),
            hover: rgb(mix(bg, fg, 0.08)),
            pressed: rgb(mix(bg, fg, 0.12)),
            fg: rgb(fg),
        }
    }
}

#[derive(Clone)]
pub struct Theme {
    pub background: Rgba,
    pub display: Rgba,
    pub history: Rgba,
    pub digit: KeyColors,
    pub function: KeyColors,
    pub operator: KeyColors,
    pub operator_active: KeyColors,
    /// History drawer background, with hover/press layers for its entries.
    pub drawer: KeyColors,
    /// Text buttons such as the history toggle.
    pub accent: Rgba,
    pub font_family: Option<String>,
}

impl Theme {
    /// The original iOS-like palette, used when no DMS palette is found.
    pub fn fallback() -> Self {
        Self {
            background: rgb(0x1c1c1c),
            display: rgb(0xffffff),
            history: rgb(0x8e8e93),
            digit: KeyColors::new(0x333333, 0xffffff),
            function: KeyColors::new(0xa5a5a5, 0x1c1c1c),
            operator: KeyColors::new(0xff9f0a, 0xffffff),
            operator_active: KeyColors::new(0xfff4e0, 0xff9f0a),
            drawer: KeyColors::new(0x141414, 0xffffff),
            accent: rgb(0xff9f0a),
            font_family: None,
        }
    }

    /// Loads the DMS palette, or the fallback if there is none.
    pub fn load() -> Self {
        Self::load_dms().unwrap_or_else(Self::fallback)
    }

    /// The DMS palette, if there is one and it reads.
    pub fn load_dms() -> Option<Self> {
        dms_dir().and_then(|dir| Self::from_dms(&dir))
    }

    fn from_dms(dir: &Path) -> Option<Self> {
        let file: DmsColors =
            serde_json::from_slice(&std::fs::read(dir.join(COLORS)).ok()?).ok()?;
        let roles = file.colors.get(&file.mode)?;
        let c = |name: &str| roles.get(name).and_then(|hex| parse_hex(hex));

        let surface = c("surface")?;
        let on_surface = c("on_surface")?;
        let primary = c("primary")?;
        let on_primary = c("on_primary")?;
        let primary_container = c("primary_container").unwrap_or(primary);
        let on_surface_variant = c("on_surface_variant").unwrap_or(on_surface);
        let container_high = c("surface_container_high").unwrap_or(surface);
        let container_highest = c("surface_container_highest").unwrap_or(container_high);
        // Some palettes (e.g. Catppuccin presets) reuse the same tone for both; keep the
        // function row distinct from the digits.
        let secondary_container = c("secondary_container")
            .filter(|&color| color != container_high)
            .unwrap_or(container_highest);
        let on_secondary_container = c("on_secondary_container").unwrap_or(on_surface);
        let container_low = c("surface_container_low").unwrap_or(surface);

        let font_family = std::fs::read(dir.join(TYPOGRAPHY))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<DmsTypography>(&bytes).ok())
            .and_then(|t| t.font_family);

        Some(Self {
            background: rgb(surface),
            display: rgb(on_surface),
            history: rgb(on_surface_variant),
            digit: KeyColors::new(container_high, on_surface),
            function: KeyColors::new(secondary_container, on_secondary_container),
            operator: KeyColors::new(primary, on_primary),
            operator_active: KeyColors::new(primary_container, primary),
            drawer: KeyColors::new(container_low, on_surface),
            accent: rgb(primary),
            font_family,
        })
    }
}

/// Watches the DMS palette files for changes by modification time.
pub struct ThemeWatcher {
    paths: Vec<PathBuf>,
    stamps: Vec<Option<SystemTime>>,
}

impl ThemeWatcher {
    pub fn new() -> Self {
        let paths = dms_dir()
            .map(|dir| vec![dir.join(COLORS), dir.join(TYPOGRAPHY)])
            .unwrap_or_default();
        let stamps = paths.iter().map(|p| modified(p)).collect();
        Self { paths, stamps }
    }

    /// True if any watched file changed since the last call.
    pub fn changed(&mut self) -> bool {
        let stamps: Vec<_> = self.paths.iter().map(|p| modified(p)).collect();
        let changed = stamps != self.stamps;
        self.stamps = stamps;
        changed
    }
}

const COLORS: &str = "dms-colors.json";
const TYPOGRAPHY: &str = "dms-typography.json";

#[derive(Deserialize)]
struct DmsColors {
    mode: String,
    colors: HashMap<String, HashMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DmsTypography {
    font_family: Option<String>,
}

fn dms_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(cache.join("DankMaterialShell"))
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn parse_hex(hex: &str) -> Option<u32> {
    let hex = hex.strip_prefix('#')?;
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok())?
}

/// Linear blend of two `0xRRGGBB` colors; `amount = 0` is `from`, `amount = 1` is `to`.
fn mix(from: u32, to: u32, amount: f32) -> u32 {
    let (from, to) = (from.to_be_bytes(), to.to_be_bytes());
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a blend of two bytes is a byte"
    )]
    let blend = |i: usize| {
        let (x, y) = (f32::from(from[i]), f32::from(to[i]));
        (y - x).mul_add(amount, x).round() as u8
    };
    u32::from_be_bytes([0, blend(1), blend(2), blend(3)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("#cba6f7"), Some(0xcba6f7));
        assert_eq!(parse_hex("cba6f7"), None);
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
    }

    #[test]
    fn mixing() {
        assert_eq!(mix(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xffffff, 1.0), 0xffffff);
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
    }

    #[test]
    fn reads_dms_palette() {
        let dir = std::env::temp_dir().join(format!("dms-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(COLORS),
            r##"{"mode": "light", "colors": {
                "dark":  {"surface": "#000000", "on_surface": "#ffffff", "primary": "#ff0000", "on_primary": "#000000"},
                "light": {"surface": "#eeeeee", "on_surface": "#111111", "primary": "#0000ff", "on_primary": "#ffffff"}
            }}"##,
        )
        .unwrap();
        std::fs::write(dir.join(TYPOGRAPHY), r#"{"fontFamily": "Inter"}"#).unwrap();

        let theme = Theme::from_dms(&dir).unwrap();
        assert_eq!(theme.background, rgb(0xeeeeee));
        assert_eq!(theme.operator.bg, rgb(0x0000ff));
        assert_eq!(theme.font_family.as_deref(), Some("Inter"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
