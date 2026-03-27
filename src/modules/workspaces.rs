/// Workspace state from Niri IPC
#[derive(Debug, Clone)]
pub struct Workspace {
    pub id:        u64,
    pub idx:       u8,   // 1-9
    pub is_active: bool,
    pub has_windows: bool,
}

/// Fetches current workspace list from Niri via IPC socket.
/// Returns sorted list by index.
pub fn get_workspaces() -> Vec<Workspace> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = match std::env::var("NIRI_SOCKET") {
        Ok(p) => p,
        Err(_) => {
            log::warn!("NIRI_SOCKET not set");
            return dummy_workspaces();
        }
    };

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("could not connect to niri socket: {}", e);
            return dummy_workspaces();
        }
    };

    // Send request
    let request = r#"{"Workspaces":null}"#;
    if stream.write_all(format!("{}\n", request).as_bytes()).is_err() {
        return dummy_workspaces();
    }

    // Read response
    let reader = BufReader::new(&stream);
    let line = reader.lines().next();
    match line {
        Some(Ok(json)) => parse_workspaces(&json),
        _ => dummy_workspaces(),
    }
}

fn parse_workspaces(json: &str) -> Vec<Workspace> {
    // Minimal JSON parsing — we'll use serde properly once types stabilize
    // For now return dummy data so the bar compiles and renders
    log::debug!("niri workspace response: {}", json);
    dummy_workspaces()
}

fn dummy_workspaces() -> Vec<Workspace> {
    (1u8..=9).map(|i| Workspace {
        id:          i as u64,
        idx:         i,
        is_active:   i == 1,
        has_windows: i <= 3,
    }).collect()
}
