use niri_ipc::{socket::Socket, Request, Response};
use std::collections::HashMap;

/// A window tracked in the taskbar
#[derive(Debug, Clone)]
pub struct WindowEntry {
    pub id:            u64,
    pub title:         String,
    pub app_id:        String,   // e.g. "firefox", "foot", "discord"
    pub workspace_idx: u8,
    pub is_focused:    bool,
}

pub fn get_windows() -> Vec<WindowEntry> {
    // Build workspace id → idx map so we can show the right workspace number.
    let ws_map: HashMap<u64, u8> = Socket::connect()
        .and_then(|s| s.send(Request::Workspaces))
        .ok()
        .and_then(|(reply, _)| reply.ok())
        .and_then(|resp| match resp {
            Response::Workspaces(ws) => Some(ws.into_iter().map(|w| (w.id, w.idx)).collect()),
            _                        => None,
        })
        .unwrap_or_default();

    let socket = match Socket::connect() {
        Ok(s)  => s,
        Err(e) => { log::warn!("niri socket: {e}"); return dummy_windows(); }
    };

    match socket.send(Request::Windows) {
        Ok((Ok(Response::Windows(wins)), _)) => {
            let mut out: Vec<WindowEntry> = wins.into_iter().map(|w| {
                let workspace_idx = w.workspace_id
                    .and_then(|id| ws_map.get(&id))
                    .copied()
                    .unwrap_or(0);
                WindowEntry {
                    id:            w.id,
                    title:         w.title.unwrap_or_default(),
                    app_id:        w.app_id.unwrap_or_else(|| "unknown".into()),
                    workspace_idx,
                    is_focused:    w.is_focused,
                }
            }).collect();
            // Focused window first, then sorted by workspace idx
            out.sort_by(|a, b| {
                b.is_focused.cmp(&a.is_focused)
                    .then(a.workspace_idx.cmp(&b.workspace_idx))
            });
            out
        }
        Ok((Err(e), _)) => { log::warn!("niri: {e}");     dummy_windows() }
        Err(e)          => { log::warn!("niri IPC: {e}"); dummy_windows() }
        _               =>                                 dummy_windows(),
    }
}

fn dummy_windows() -> Vec<WindowEntry> {
    vec![
        WindowEntry { id: 1, title: "Mozilla Firefox".into(), app_id: "firefox".into(), workspace_idx: 2, is_focused: false },
        WindowEntry { id: 2, title: "foot".into(),            app_id: "foot".into(),    workspace_idx: 1, is_focused: true  },
        WindowEntry { id: 3, title: "nvim".into(),            app_id: "nvim".into(),    workspace_idx: 3, is_focused: false },
    ]
}
