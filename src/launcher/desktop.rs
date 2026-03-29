use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub name:   String,
    pub exec:   String,
    pub icon:   Option<String>,
    pub app_id: String,   // desktop file stem, used for icon lookup
}

/// Load all .desktop applications from standard XDG search paths.
/// Returns entries sorted by name, with duplicates by name removed (first wins).
pub fn load_all() -> Vec<DesktopEntry> {
    let mut entries: Vec<DesktopEntry> = Vec::new();

    let search_dirs: Vec<PathBuf> = {
        let home = std::env::var("HOME").unwrap_or_default();

        // XDG_DATA_HOME (default: ~/.local/share)
        let data_home = std::env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{}/.local/share", home));

        // XDG_DATA_DIRS (nix sets this to include nix profile and system paths)
        let data_dirs_str = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());

        let mut dirs: Vec<PathBuf> = Vec::new();
        dirs.push(PathBuf::from(&data_home).join("applications"));
        for entry in data_dirs_str.split(':') {
            let p = PathBuf::from(entry).join("applications");
            if !dirs.contains(&p) {
                dirs.push(p);
            }
        }
        dirs
    };

    for dir in &search_dirs {
        let Ok(read) = std::fs::read_dir(dir) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(de) = parse_desktop_file(&path) {
                // deduplicate by name — first directory wins
                if !entries.iter().any(|e| e.name == de.name) {
                    entries.push(de);
                }
            }
        }
    }

    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    entries
}

fn parse_desktop_file(path: &Path) -> Option<DesktopEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name:       Option<String> = None;
    let mut exec:       Option<String> = None;
    let mut icon:       Option<String> = None;
    let mut no_display  = false;
    let mut entry_type: Option<String> = None;
    let mut in_section  = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_section = true;
            continue;
        }
        if line.starts_with('[') {
            in_section = false;
            continue;
        }
        if !in_section { continue; }

        if let Some(v) = line.strip_prefix("Name=") {
            if name.is_none() { name = Some(v.to_string()); }
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Icon=") {
            icon = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Type=") {
            entry_type = Some(v.to_string());
        } else if line == "NoDisplay=true" || line == "Hidden=true" {
            no_display = true;
        }
    }

    if no_display { return None; }
    if entry_type.as_deref() != Some("Application") { return None; }

    let app_id = path.file_stem()?.to_str()?.to_string();

    Some(DesktopEntry {
        name: name?,
        exec: exec?,
        icon,
        app_id,
    })
}

/// Strip Exec field codes (%f %F %u %U %i %c %k) and split into command + args.
pub fn clean_exec(exec: &str) -> (String, Vec<String>) {
    let cleaned = exec
        .replace("%f", "").replace("%F", "")
        .replace("%u", "").replace("%U", "")
        .replace("%i", "").replace("%c", "")
        .replace("%k", "");
    let mut parts = cleaned.split_whitespace();
    let cmd  = parts.next().unwrap_or("").to_string();
    let args = parts.map(|s| s.to_string()).collect();
    (cmd, args)
}
