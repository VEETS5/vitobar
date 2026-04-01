// App icon loader — finds the PNG icon for an app via XDG desktop files,
// decodes it, and scales to the requested physical pixel size.
// Results are cached per (app_id, size) so the filesystem is only hit once.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static CACHE: OnceLock<Mutex<HashMap<String, Option<Vec<u8>>>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Option<Vec<u8>>>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns `size × size` RGBA bytes for the app's icon, or None if not found.
/// Result is cached — safe to call every frame.
pub fn load(app_id: &str, size: u32) -> Option<Vec<u8>> {
    let key = format!("{}@{}", app_id, size);
    {
        let map = cache().lock().unwrap();
        if let Some(v) = map.get(&key) {
            return v.clone();
        }
    }
    let result = lookup(app_id, size);
    cache().lock().unwrap().insert(key, result.clone());
    result
}

// ── Path helpers ─────────────────────────────────────────────────────────────

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for p in &["/run/current-system/sw/share", "/usr/local/share", "/usr/share"] {
        let pb = PathBuf::from(p);
        if !dirs.contains(&pb) {
            dirs.push(pb);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let local = PathBuf::from(home).join(".local/share");
        if !dirs.contains(&local) {
            dirs.insert(0, local);
        }
    }
    dirs
}

// ── Desktop file → icon name ──────────────────────────────────────────────────

fn desktop_icon_name(app_id: &str) -> Option<String> {
    let lower = app_id.to_ascii_lowercase();
    let candidates: &[&str] = &[app_id, &lower];
    for dir in data_dirs() {
        let apps = dir.join("applications");
        for &name in candidates {
            let path = apps.join(format!("{}.desktop", name));
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    if let Some(v) = line.strip_prefix("Icon=") {
                        return Some(v.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

// ── Icon file search ──────────────────────────────────────────────────────────

fn find_png_path(icon_name: &str) -> Option<PathBuf> {
    // Absolute path already
    if icon_name.starts_with('/') {
        let p = PathBuf::from(icon_name);
        if p.extension().and_then(|e| e.to_str()) == Some("png") && p.exists() {
            return Some(p);
        }
    }

    let dirs = data_dirs();
    // Larger sizes give better quality when downscaling
    let sizes: &[u32] = &[48, 64, 32, 128, 256, 22, 24, 16];
    let themes = ["hicolor", "Papirus", "Papirus-Dark", "Adwaita", "gnome", "breeze"];

    for dir in &dirs {
        let icons = dir.join("icons");
        for theme in &themes {
            for &sz in sizes {
                let p = icons
                    .join(theme)
                    .join(format!("{}x{}", sz, sz))
                    .join("apps")
                    .join(format!("{}.png", icon_name));
                if p.exists() {
                    return Some(p);
                }
            }
        }
        // Pixmaps fallback
        let p = dir.join("pixmaps").join(format!("{}.png", icon_name));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

// ── PNG decode + scale ────────────────────────────────────────────────────────

fn lookup(app_id: &str, size: u32) -> Option<Vec<u8>> {
    let icon_name = desktop_icon_name(app_id).unwrap_or_else(|| app_id.to_string());
    let path = find_png_path(&icon_name)?;
    decode_and_scale(&path, size)
}

fn decode_and_scale(path: &std::path::Path, target: u32) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let dec = png::Decoder::new(std::io::Cursor::new(&bytes));
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;

    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }

    let (sw, sh) = (info.width, info.height);
    // Normalise to RGBA
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => {
            buf[..(sw * sh * 4) as usize].to_vec()
        }
        png::ColorType::Rgb => buf[..(sw * sh * 3) as usize]
            .chunks(3)
            .flat_map(|c| [c[0], c[1], c[2], 255u8])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf[..(sw * sh * 2) as usize]
            .chunks(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => buf[..(sw * sh) as usize]
            .iter()
            .flat_map(|&v| [v, v, v, 255u8])
            .collect(),
        _ => return None,
    };

    // Bilinear interpolation scale to target × target
    let t = target as usize;
    let (sw_us, sh_us) = (sw as usize, sh as usize);
    let mut out = vec![0u8; t * t * 4];
    let rx = sw as f32 / target as f32;
    let ry = sh as f32 / target as f32;
    for dy in 0..t {
        for dx in 0..t {
            let src_x = (dx as f32 + 0.5) * rx - 0.5;
            let src_y = (dy as f32 + 0.5) * ry - 0.5;
            let x0 = (src_x.floor() as isize).max(0) as usize;
            let y0 = (src_y.floor() as isize).max(0) as usize;
            let x1 = (x0 + 1).min(sw_us - 1);
            let y1 = (y0 + 1).min(sh_us - 1);
            let fx = src_x.fract().clamp(0.0, 1.0);
            let fy = src_y.fract().clamp(0.0, 1.0);

            let i00 = (y0 * sw_us + x0) * 4;
            let i10 = (y0 * sw_us + x1) * 4;
            let i01 = (y1 * sw_us + x0) * 4;
            let i11 = (y1 * sw_us + x1) * 4;
            let di  = (dy * t + dx) * 4;

            for c in 0..4 {
                let v00 = rgba[i00 + c] as f32;
                let v10 = rgba[i10 + c] as f32;
                let v01 = rgba[i01 + c] as f32;
                let v11 = rgba[i11 + c] as f32;
                let top    = v00 + (v10 - v00) * fx;
                let bottom = v01 + (v11 - v01) * fx;
                let val    = top + (bottom - top) * fy;
                out[di + c] = val.round() as u8;
            }
        }
    }
    Some(out)
}
