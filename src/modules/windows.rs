/// A window tracked in the taskbar
#[derive(Debug, Clone)]
pub struct WindowEntry {
    pub id:             u64,
    pub title:          String,
    pub app_id:         String,   // e.g. "firefox", "foot", "discord"
    pub workspace_idx:  u8,
    pub is_focused:     bool,
}

pub fn get_windows() -> Vec<WindowEntry> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = match std::env::var("NIRI_SOCKET") {
        Ok(p) => p,
        Err(_) => return dummy_windows(),
    };

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return dummy_windows(),
    };

    let request = r#"{"Windows":null}"#;
    if stream.write_all(format!("{}\n", request).as_bytes()).is_err() {
        return dummy_windows();
    }

    let reader = BufReader::new(&stream);
    match reader.lines().next() {
        Some(Ok(json)) => {
            log::debug!("niri windows response: {}", json);
            dummy_windows() // swap for real parser once IPC types are wired
        }
        _ => dummy_windows(),
    }
}

fn dummy_windows() -> Vec<WindowEntry> {
    vec![
        WindowEntry { id: 1, title: "Mozilla Firefox".into(), app_id: "firefox".into(), workspace_idx: 2, is_focused: false },
        WindowEntry { id: 2, title: "foot".into(),            app_id: "foot".into(),    workspace_idx: 1, is_focused: true  },
        WindowEntry { id: 3, title: "nvim".into(),            app_id: "nvim".into(),    workspace_idx: 3, is_focused: false },
        WindowEntry { id: 4, title: "Discord".into(),         app_id: "discord".into(), workspace_idx: 1, is_focused: false },
    ]
}
