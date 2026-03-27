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

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        if path.exists() {
            let content = fs::read_to_string(&path)
                .expect("failed to read vitobar config");
            toml::from_str(&content).expect("failed to parse vitobar config")
        } else {
            log::warn!("no config found at {:?}, using defaults", path);
            Self::default()
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

/// Parse a hex color string like "1e1e2e" into (r, g, b, a)
pub fn hex_to_rgba(hex: &str) -> (u8, u8, u8, u8) {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    (r, g, b, 255)
}
