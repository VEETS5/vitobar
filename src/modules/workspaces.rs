use niri_ipc::{socket::Socket, Request, Response};

/// Workspace state from Niri IPC
#[derive(Debug, Clone)]
pub struct Workspace {
    pub id:          u64,
    pub idx:         u8,   // 1-based display index
    pub is_active:   bool,
    pub has_windows: bool,
}

/// Fetches current workspace list from Niri via IPC socket.
/// Returns sorted list by index.
pub fn get_workspaces() -> Vec<Workspace> {
    let socket = match Socket::connect() {
        Ok(s)  => s,
        Err(e) => { log::warn!("niri socket: {e}"); return dummy_workspaces(); }
    };

    match socket.send(Request::Workspaces) {
        Ok((Ok(Response::Workspaces(ws)), _)) => {
            let mut out: Vec<Workspace> = ws.into_iter().map(|w| Workspace {
                id:          w.id,
                idx:         w.idx,
                is_active:   w.is_active,
                // niri always sets active_window_id when a workspace has windows
                has_windows: w.active_window_id.is_some(),
            }).collect();
            out.sort_by_key(|w| w.idx);
            out
        }
        Ok((Err(e), _)) => { log::warn!("niri: {e}");     dummy_workspaces() }
        Err(e)          => { log::warn!("niri IPC: {e}"); dummy_workspaces() }
        _               =>                                 dummy_workspaces(),
    }
}

fn dummy_workspaces() -> Vec<Workspace> {
    (1u8..=9).map(|i| Workspace {
        id:          i as u64,
        idx:         i,
        is_active:   i == 1,
        has_windows: i <= 3,
    }).collect()
}
