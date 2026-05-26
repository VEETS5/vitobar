use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Colors {
    pub base00: String, // background
    pub base01: String, // surface
    pub base02: String, // border inactive
    pub base03: String, // muted text
    pub base04: String, // subtle text
    pub base05: String, // foreground
    pub base06: String, // light fg
    pub base07: String, // bright fg
    pub base08: String, // red
    pub base09: String, // orange
    pub base0a: String, // yellow
    pub base0b: String, // green
    pub base0c: String, // cyan
    pub base0d: String, // blue (accent)
    pub base0e: String, // purple
    pub base0f: String, // brown
}

impl Colors {
    /// The 16 base colors in order, as hex strings (for palette previews).
    pub fn swatches(&self) -> Vec<String> {
        vec![
            self.base00.clone(), self.base01.clone(), self.base02.clone(), self.base03.clone(),
            self.base04.clone(), self.base05.clone(), self.base06.clone(), self.base07.clone(),
            self.base08.clone(), self.base09.clone(), self.base0a.clone(), self.base0b.clone(),
            self.base0c.clone(), self.base0d.clone(), self.base0e.clone(), self.base0f.clone(),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub colors:         Colors,
    pub bar_height:     Option<u32>,
    pub taskbar_height: Option<u32>,
    pub font_size:      Option<f32>,
    pub font_path:      Option<String>,
    pub selected_theme: Option<String>,
    pub bar_opacity:    Option<f32>,
}

impl Config {
    pub fn bar_h(&self)     -> u32 { self.bar_height.unwrap_or(21) }
    pub fn taskbar_h(&self) -> u32 { self.taskbar_height.unwrap_or(21) }
    pub fn opacity(&self)   -> f32 { self.bar_opacity.unwrap_or(1.0).clamp(0.0, 1.0) }
}

/// Everything that may live in config.toml. All fields optional so a partial
/// file (e.g. just `bar_height`, no `[colors]`) still parses.
#[derive(Debug, Default, Deserialize)]
struct TomlSettings {
    colors:         Option<Colors>,
    bar_height:     Option<u32>,
    taskbar_height: Option<u32>,
    font_size:      Option<f32>,
    font_path:      Option<String>,
    selected_theme: Option<String>,
    bar_opacity:    Option<f32>,
}

/// A named base16 color scheme selectable in the Appearance tab.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name:   String,
    pub colors: Colors,
}

#[derive(Deserialize)]
struct ThemeFile {
    name: String,
    #[serde(flatten)]
    colors: Colors,
}

fn parse_theme(s: &str) -> Option<Theme> {
    let tf: ThemeFile = serde_json::from_str(s).ok()?;
    Some(Theme { name: tf.name, colors: tf.colors })
}

fn bundled_themes() -> Vec<Theme> {
    const FILES: &[&str] = &[
        include_str!("settings/themes/catppuccin-mocha.json"),
        include_str!("settings/themes/catppuccin-latte.json"),
        include_str!("settings/themes/nord.json"),
        include_str!("settings/themes/gruvbox-dark.json"),
        include_str!("settings/themes/gruvbox-light.json"),
        include_str!("settings/themes/tokyo-night.json"),
        include_str!("settings/themes/dracula.json"),
        include_str!("settings/themes/rose-pine.json"),
        include_str!("settings/themes/solarized-dark.json"),
        include_str!("settings/themes/solarized-light.json"),
    ];
    FILES.iter().filter_map(|s| parse_theme(s)).collect()
}

fn user_themes() -> Vec<Theme> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let dir  = PathBuf::from(home).join(".config/vitobar/themes");
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(theme) = fs::read_to_string(&path).ok().and_then(|s| parse_theme(&s)) {
                    out.push(theme);
                }
            }
        }
    }
    out
}

/// All selectable themes: bundled base16 schemes plus any user-provided ones
/// in ~/.config/vitobar/themes/*.json.
pub fn available_themes() -> Vec<Theme> {
    let mut themes = bundled_themes();
    themes.extend(user_themes());
    themes
}

fn load_stylix_colors() -> Option<Colors> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let path = PathBuf::from(home).join(".config/stylix/palette.json");
    let content = fs::read_to_string(&path).ok()?;
    let map: serde_json::Value = serde_json::from_str(&content).ok()?;
    let obj = map.as_object()?;

    // palette.json uses "base0A"–"base0F" (uppercase hex digits A-F), struct fields lowercase.
    let get = |key: &str| -> Option<String> {
        let upper_key = if key.len() == 6 {
            format!("{}{}", &key[..5], key[5..].to_uppercase())
        } else {
            key.to_string()
        };
        obj.get(key)
            .or_else(|| obj.get(&upper_key))
            .and_then(|v| v.as_str())
            .map(|s| s.trim_start_matches('#').to_string())
    };

    Some(Colors {
        base00: get("base00")?,
        base01: get("base01")?,
        base02: get("base02")?,
        base03: get("base03")?,
        base04: get("base04")?,
        base05: get("base05")?,
        base06: get("base06")?,
        base07: get("base07")?,
        base08: get("base08")?,
        base09: get("base09")?,
        base0a: get("base0a")?,
        base0b: get("base0b")?,
        base0c: get("base0c")?,
        base0d: get("base0d")?,
        base0e: get("base0e")?,
        base0f: get("base0f")?,
    })
}

impl Config {
    pub fn load() -> Self {
        let ts = Self::load_toml_settings();

        // 1. Explicit in-app theme pick overrides everything (incl. Stylix).
        if let Some(name) = ts.selected_theme.as_deref() {
            if let Some(theme) = available_themes().into_iter().find(|t| t.name == name) {
                log::info!("using selected theme '{}'", name);
                return Self::from_settings(theme.colors, &ts);
            }
            log::warn!("selected_theme '{}' not found, falling back", name);
        }

        // 2. Stylix palette.json (colors only; other settings from config.toml).
        if let Some(colors) = load_stylix_colors() {
            log::info!("loaded colors from stylix palette.json");
            return Self::from_settings(colors, &ts);
        }

        // 3. Colors written directly in config.toml.
        if let Some(colors) = ts.colors.clone() {
            return Self::from_settings(colors, &ts);
        }

        // 4. Hardcoded Catppuccin Mocha defaults (still honoring numeric settings).
        log::warn!("no colors found at {:?}, using defaults", config_path());
        Self::from_settings(Self::default().colors, &ts)
    }

    fn load_toml_settings() -> TomlSettings {
        let path = config_path();
        if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| toml::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            TomlSettings::default()
        }
    }

    fn from_settings(colors: Colors, ts: &TomlSettings) -> Self {
        Self {
            colors,
            bar_height:     ts.bar_height.or(Some(21)),
            taskbar_height: ts.taskbar_height.or(Some(21)),
            font_size:      ts.font_size.or(Some(11.0)),
            font_path:      ts.font_path.clone(),
            selected_theme: ts.selected_theme.clone(),
            bar_opacity:    ts.bar_opacity.or(Some(1.0)),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            colors: Colors {
                base00: "1e1e2e".into(),
                base01: "313244".into(),
                base02: "45475a".into(),
                base03: "6c7086".into(),
                base04: "a6adc8".into(),
                base05: "cdd6f4".into(),
                base06: "f5e0dc".into(),
                base07: "b4befe".into(),
                base08: "f38ba8".into(),
                base09: "fab387".into(),
                base0a: "f9e2af".into(),
                base0b: "a6e3a1".into(),
                base0c: "94e2d5".into(),
                base0d: "89b4fa".into(),
                base0e: "cba6f7".into(),
                base0f: "f2cdcd".into(),
            },
            bar_height:      Some(21),
            taskbar_height:  Some(21),
            font_size:       Some(11.0),
            font_path:       None,
            selected_theme:  None,
            bar_opacity:     Some(1.0),
        }
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".config/vitobar/config.toml")
}

/// Merge a single key/value into config.toml, preserving all other keys.
pub fn save_setting(key: &str, value: toml::Value) {
    let path = config_path();
    let mut val: toml::Value = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_else(|| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };

    if let Some(table) = val.as_table_mut() {
        table.insert(key.to_string(), value);
    }

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(s) = toml::to_string_pretty(&val) {
        let _ = fs::write(path, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_all_parse() {
        let themes = bundled_themes();
        assert_eq!(themes.len(), 10, "all bundled scheme files should parse");
        for t in &themes {
            assert!(!t.name.is_empty());
            assert_eq!(t.colors.base00.len(), 6, "{} base00 must be a 6-digit hex", t.name);
            assert_eq!(t.colors.swatches().len(), 16);
        }
    }
}

/// Parse a hex color string like "1e1e2e" into (r, g, b, a)
pub fn hex_to_rgba(hex: &str) -> (u8, u8, u8, u8) {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return (0, 0, 0, 255);
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    (r, g, b, 255)
}
