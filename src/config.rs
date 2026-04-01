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

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub colors: Colors,
    pub bar_height: Option<u32>,
    pub taskbar_height: Option<u32>,
    pub font_size: Option<f32>,
    pub font_path: Option<String>,
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
        // 1. Try stylix palette.json — colors only; other settings from config.toml if present
        if let Some(colors) = load_stylix_colors() {
            log::info!("loaded colors from stylix palette.json");
            let toml = Self::load_from_toml();
            return Self {
                colors,
                bar_height:     toml.as_ref().and_then(|c| c.bar_height).or(Some(22)),
                taskbar_height: toml.as_ref().and_then(|c| c.taskbar_height).or(Some(22)),
                font_size:      toml.as_ref().and_then(|c| c.font_size).or(Some(11.0)),
                font_path:      toml.as_ref().and_then(|c| c.font_path.clone()),
            };
        }

        // 2. Try ~/.config/vitobar/config.toml
        if let Some(cfg) = Self::load_from_toml() {
            return cfg;
        }

        // 3. Hardcoded Catppuccin Mocha defaults
        log::warn!("no config found at {:?}, using defaults", config_path());
        Self::default()
    }

    fn load_from_toml() -> Option<Self> {
        let path = config_path();
        if path.exists() {
            let content = fs::read_to_string(&path).ok()?;
            toml::from_str(&content).ok()
        } else {
            None
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
            bar_height:      Some(22),
            taskbar_height:  Some(22),
            font_size:       Some(11.0),
            font_path:       None,
        }
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".config/vitobar/config.toml")
}

/// Save bar-specific settings to config.toml (preserves existing values).
pub fn save_bar_settings(bar_height: Option<u32>, taskbar_height: Option<u32>, font_size: Option<f32>) {
    let path = config_path();
    let mut val: toml::Value = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or(toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };

    if let Some(table) = val.as_table_mut() {
        if let Some(v) = bar_height {
            table.insert("bar_height".into(), toml::Value::Integer(v as i64));
        }
        if let Some(v) = taskbar_height {
            table.insert("taskbar_height".into(), toml::Value::Integer(v as i64));
        }
        if let Some(v) = font_size {
            table.insert("font_size".into(), toml::Value::Float(v as f64));
        }
    }

    if let Ok(s) = toml::to_string_pretty(&val) {
        fs::write(path, s).ok();
    }
}

/// Parse a hex color string like "1e1e2e" into (r, g, b, a)
pub fn hex_to_rgba(hex: &str) -> (u8, u8, u8, u8) {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    (r, g, b, 255)
}
