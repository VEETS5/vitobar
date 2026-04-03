mod config;
mod render;
mod modules;
mod icons;

use config::Config;
use modules::{
    clock::get_time_string,
    sysinfo::{SysMonitor, SysStats},
    bluetooth::BluetoothStatus,
    tray::{self, TrayState, TrayDirty, TrayItem},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use render::Renderer;

use niri_ipc::{
    Event as NiriEvent, Request as NiriRequest, Response as NiriResponse,
    socket::Socket as NiriSocket,
    state::{EventStreamState, EventStreamStatePart},
};
use std::collections::HashMap;

use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RepeatInfo},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::wlr_layer::{
        Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
        LayerSurfaceConfigure,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

#[derive(Debug, Clone)]
enum BarAction {
    FocusWorkspace { id: u64 },
    FocusWindow    { id: u64 },
    Spawn          { cmd: String },
    // Right-click context menu actions
    CloseWindow    { id: u64 },
    MaximizeColumn,
    FullscreenWindow { id: u64 },
    ToggleFloating { id: u64 },
    MoveToWorkspace { window_id: u64, ws_idx: u8 },
    // System tray actions
    TrayActivate    { service: String, id: String, sni_path: String },
    TrayShowMenu    { tray_idx: usize },
    TrayMenuItem     { tray_idx: usize, menu_id: i32 },
}

#[derive(Debug, Clone)]
struct HitRegion {
    x: f32, y: f32, w: f32, h: f32,
    action: BarAction,
}

fn fire_action(action: BarAction) {
    match action {
        BarAction::FocusWorkspace { id } => {
            std::thread::spawn(move || {
                if let Ok(mut s) = NiriSocket::connect() {
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::FocusWorkspace {
                            reference: niri_ipc::WorkspaceReferenceArg::Id(id),
                        },
                    ));
                }
            });
        }
        BarAction::FocusWindow { id } => {
            std::thread::spawn(move || {
                if let Ok(mut s) = NiriSocket::connect() {
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::FocusWindow { id },
                    ));
                }
            });
        }
        BarAction::Spawn { cmd } => {
            std::process::Command::new(&cmd).spawn().ok();
        }
        BarAction::CloseWindow { id } => {
            std::thread::spawn(move || {
                if let Ok(mut s) = NiriSocket::connect() {
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::CloseWindow { id: Some(id) },
                    ));
                }
            });
        }
        BarAction::MaximizeColumn => {
            std::thread::spawn(move || {
                if let Ok(mut s) = NiriSocket::connect() {
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::MaximizeColumn {},
                    ));
                }
            });
        }
        BarAction::FullscreenWindow { id } => {
            std::thread::spawn(move || {
                if let Ok(mut s) = NiriSocket::connect() {
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::FullscreenWindow { id: Some(id) },
                    ));
                }
            });
        }
        BarAction::ToggleFloating { id } => {
            std::thread::spawn(move || {
                if let Ok(mut s) = NiriSocket::connect() {
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::ToggleWindowFloating { id: Some(id) },
                    ));
                }
            });
        }
        BarAction::MoveToWorkspace { window_id, ws_idx } => {
            std::thread::spawn(move || {
                if let Ok(mut s) = NiriSocket::connect() {
                    // Focus the window first so niri targets it
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::FocusWindow { id: window_id },
                    ));
                }
                if let Ok(mut s) = NiriSocket::connect() {
                    let _ = s.send(NiriRequest::Action(
                        niri_ipc::Action::MoveWindowToWorkspace {
                            window_id: Some(window_id),
                            reference: niri_ipc::WorkspaceReferenceArg::Index(ws_idx),
                            focus: true,
                        },
                    ));
                }
            });
        }
        BarAction::TrayActivate { service, id, sni_path } => {
            std::thread::spawn(move || {
                tray::activate_item_by_service(&service, &id, &sni_path);
            });
        }
        // TrayShowMenu and TrayMenuItem handled inline in VitoBar methods
        BarAction::TrayShowMenu { .. }
        | BarAction::TrayMenuItem { .. } => {}
    }
}

use calloop::{EventLoop, channel::{channel as calloop_channel, Event as ChannelEvent}, timer::{Timer, TimeoutAction}};
use calloop_wayland_source::WaylandSource;
use std::time::Duration;

const BAR_HEIGHT:     u32 = 22;
const TASKBAR_HEIGHT: u32 = 22;
const POPUP_WIDTH:    u32 = 160;
const POPUP_ITEM_H:   u32 = 22;
const POPUP_ITEMS:    u32 = 9; // Close, Maximize, Fullscreen, Float, WS 1-5
const POPUP_HEIGHT:   u32 = POPUP_ITEM_H * POPUP_ITEMS;

#[derive(Clone)]
enum PopupKind {
    WindowMenu { window_id: u64 },
    TrayMenu   { item: TrayItem, menu_items: Vec<tray::MenuItem> },
}

struct PopupState {
    kind:         PopupKind,
    surface:      LayerSurface,
    pool:         Option<SlotPool>,
    configured:   bool,
    hits:         Vec<HitRegion>,
    pointer_pos:  (f64, f64),
    num_items:    u32,
    // Fullscreen overlay dimensions (from configure)
    width:        u32,
    height:       u32,
    // Menu position in logical coords
    menu_x:       f32,
    anchor_top:   bool,   // true = below top bar, false = above bottom bar
    scale:        u32,
    last_hovered_idx: Option<usize>,  // skip redraw if hover unchanged
}

// ── Per-output state: each monitor gets its own top + bottom surfaces ────────

struct PerOutput {
    output: wl_output::WlOutput,
    output_name:    Option<String>,

    top_surface:    Option<LayerSurface>,
    top_pool:       Option<SlotPool>,
    bot_surface:    Option<LayerSurface>,
    bot_pool:       Option<SlotPool>,

    width:          u32,
    scale:          u32,
    top_configured: bool,
    bot_configured: bool,

    top_hits:        Vec<HitRegion>,
    top_tray_hits:   Vec<(usize, HitRegion)>,  // (tray_idx, region) for right-click
    bot_hits:        Vec<HitRegion>,
    top_pointer_pos: (f64, f64),
    bot_pointer_pos: (f64, f64),
}

struct VitoBar {
    registry_state: RegistryState,
    output_state:   OutputState,
    compositor:     CompositorState,
    layer_shell:    LayerShell,
    shm:            Shm,

    outputs:        Vec<PerOutput>,

    conn:           Connection,
    qh:             QueueHandle<Self>,
    config:         Config,
    monitor:        SysMonitor,
    stats:          SysStats,

    running:        bool,
    bars_hidden:    bool,
    toggle_signal:  Arc<AtomicBool>,

    niri_state:      EventStreamState,
    windows_ordered: Vec<niri_ipc::Window>,

    seat_state:      SeatState,
    pointer:         Option<wl_pointer::WlPointer>,
    keyboard:        Option<wl_keyboard::WlKeyboard>,
    popup:           Option<PopupState>,
    tray_state:      TrayState,
    tray_dirty:      TrayDirty,
    tray_cached:     Vec<TrayItem>,
    tray_menu_tx:    calloop::channel::Sender<(usize, Vec<tray::MenuItem>)>,
    pending_tray_popup: Option<(TrayItem, usize, f32)>, // (item, output_idx, menu_x)
    last_time_string:  String,
}

fn sort_windows_by_position(windows: &mut Vec<niri_ipc::Window>, ws_map: &HashMap<u64, u8>) {
    windows.sort_by_key(|w| {
        let ws_idx = w.workspace_id.and_then(|id| ws_map.get(&id)).copied().unwrap_or(u8::MAX);
        let (col, tile) = w.layout.pos_in_scrolling_layout.unwrap_or((usize::MAX, usize::MAX));
        (ws_idx, col, tile)
    });
}

// ── Free drawing functions (takes disjoint borrows to satisfy borrow checker) ─

fn draw_top_on(
    out: &mut PerOutput,
    config: &Config,
    niri_state: &EventStreamState,
    stats: &SysStats,
    _windows_ordered: &[niri_ipc::Window],
    conn: &Connection,
    shm: &Shm,
    tray_items: &[TrayItem],
) {
    let width  = out.width;
    let height = BAR_HEIGHT;
    let scale  = out.scale;
    let pw = width  * scale;
    let ph = height * scale;
    let sf = scale as f32;

    let pool = match out.top_pool.as_mut() {
        Some(p) => p,
        None    => {
            let size = pw as usize * ph as usize * 4 * 2;
            out.top_pool = Some(SlotPool::new(size, shm).expect("top pool"));
            out.top_pool.as_mut().unwrap()
        }
    };
    let surface = match out.top_surface.as_ref() {
        Some(s) => s,
        None    => return,
    };

    let stride = pw as i32 * 4;
    let (buffer, canvas) = pool
        .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
        .expect("failed to create buffer");

    let mut r = Renderer::new(pw, ph);
    r.clear(&config.colors.base00);

    let mut hits: Vec<HitRegion> = Vec::new();

    let fsz    = config.font_size.unwrap_or(11.0) * sf;
    let pad    = 2.0 * sf;
    let bh     = 18.0 * sf;
    let text_y = pad + bh * 0.75;

    // ── Workspaces (filtered to this output) ────────────────────────────
    let output_name = out.output_name.as_deref();
    let mut workspaces: Vec<&niri_ipc::Workspace> =
        niri_state.workspaces.workspaces.values()
            .filter(|ws| match (output_name, ws.output.as_deref()) {
                (Some(mine), Some(theirs)) => mine == theirs,
                _ => true, // show all if output unknown
            })
            .collect();
    workspaces.sort_by_key(|w| w.idx);

    let bsz = 18.0 * sf;
    let mut x = 4.0 * sf;
    for ws in &workspaces {
        let has_windows = ws.active_window_id.is_some();
        let fill = if ws.is_active {
            &config.colors.base0d
        } else if has_windows {
            &config.colors.base02
        } else {
            &config.colors.base01
        };
        r.draw_rect(x, pad, bsz, bsz, fill);
        r.draw_rect_outline(x, pad, bsz, bsz, &config.colors.base02, 1.5 * sf);
        hits.push(HitRegion {
            x: x / sf, y: 2.0, w: bsz / sf, h: 18.0,
            action: BarAction::FocusWorkspace { id: ws.id },
        });

        let text_color = if ws.is_active {
            &config.colors.base00
        } else if has_windows {
            &config.colors.base05
        } else {
            &config.colors.base03
        };
        let num = ws.idx.to_string();
        let tw  = r.measure_text(&num, fsz);
        r.draw_text(&num, x + (bsz - tw) / 2.0, text_y, fsz, text_color);

        x += 20.0 * sf;
    }

    // ── Center: launcher (NixOS) + settings ─────────────────────────────
    let cx = pw as f32 / 2.0;
    let cx_log = width as f32 / 2.0;
    let icon_fsz = fsz * 1.5;

    let launch_label = "\u{f313}";
    let lw = 36.0 * sf;
    let lx = cx - 38.0 * sf;
    r.draw_rect(lx, pad, lw, bh, &config.colors.base01);
    r.draw_rect_outline(lx, pad, lw, bh, &config.colors.base02, 1.5 * sf);
    let ltw = r.measure_text(launch_label, icon_fsz);
    r.draw_text(launch_label, lx + (lw - ltw) / 2.0, pad + bh * 0.82, icon_fsz, &config.colors.base0d);
    hits.push(HitRegion {
        x: cx_log - 38.0, y: 2.0, w: 36.0, h: 18.0,
        action: BarAction::Spawn { cmd: "vitolauncher".into() },
    });

    let cfg_label = "\u{f013}";
    let sw = 22.0 * sf;
    let sx = cx + 2.0 * sf;
    r.draw_rect(sx, pad, sw, bh, &config.colors.base01);
    r.draw_rect_outline(sx, pad, sw, bh, &config.colors.base02, 1.5 * sf);
    let stw = r.measure_text(cfg_label, icon_fsz);
    r.draw_text(cfg_label, sx + (sw - stw) / 2.0, pad + bh * 0.82, icon_fsz, &config.colors.base0e);
    hits.push(HitRegion {
        x: cx_log + 2.0, y: 2.0, w: 22.0, h: 18.0,
        action: BarAction::Spawn { cmd: "vitosettings".into() },
    });

    // ── Status blocks (right-to-left) ───────────────────────────────────
    let time  = get_time_string();
    let gap   = 4.0 * sf;
    let mut rx = pw as f32 - 4.0 * sf;

    macro_rules! status_block {
        ($w:expr, $icon:expr, $text:expr, $color:expr, $cmd:expr) => {{
            let bw = $w * sf;
            rx -= bw;
            r.draw_rect(rx, pad, bw, bh, &config.colors.base01);
            r.draw_rect_outline(rx, pad, bw, bh, &config.colors.base02, 1.5 * sf);
            let iw = r.measure_text($icon, icon_fsz);
            r.draw_text($icon, rx + 4.0 * sf, pad + bh * 0.82, icon_fsz, $color);
            r.draw_text($text, rx + 4.0 * sf + iw + 2.0 * sf, text_y, fsz, $color);
            hits.push(HitRegion {
                x: rx / sf, y: 2.0, w: bw / sf, h: 18.0,
                action: BarAction::Spawn { cmd: $cmd.into() },
            });
            rx -= gap;
        }};
    }

    status_block!(152.0, "\u{f017}", &format!(" {}", time),                      &config.colors.base07, "vitosettings");
    status_block!( 68.0, "\u{f028}", &format!(" {:>3}%",  stats.volume_pct),     &config.colors.base0c, "vitosettings");
    if let Some(bat) = stats.battery_pct {
        status_block!(68.0, "\u{f240}", &format!(" {:>3.0}%", bat),              &config.colors.base0b, "vitosettings");
    }
    if let Some(brt) = stats.brightness_pct {
        status_block!(68.0, "\u{f0eb}", &format!(" {:>3}%", brt),                &config.colors.base0a, "vitosettings");
    }
    status_block!( 68.0, "\u{f0e7}", &format!(" {:>3.0}%", stats.cpu_pct),       &config.colors.base09, "vitosettings");
    if let Some(ct) = stats.cpu_temp {
        status_block!(68.0, "\u{f2c9}", &format!(" {:>2.0}°C", ct),              &config.colors.base09, "vitosettings");
    }
    if let Some(gt) = stats.gpu_temp {
        status_block!(68.0, "\u{f26c}", &format!(" {:>2.0}°C", gt),              &config.colors.base08, "vitosettings");
    }

    let (bt_icon, bt_text, bt_col) = match &stats.bluetooth.status {
        BluetoothStatus::Off        => ("\u{f294}", " off".to_string(), config.colors.base03.clone()),
        BluetoothStatus::OnNoDevice => ("\u{f294}", " on".to_string(),  config.colors.base0c.clone()),
        BluetoothStatus::Connected { device_name } => {
            let name = if device_name.len() > 6 { &device_name[..6] } else { device_name.as_str() };
            ("\u{f294}", format!(" {}", name), config.colors.base0b.clone())
        }
    };
    status_block!(68.0, bt_icon, &bt_text, &bt_col, "blueman-manager");

    status_block!( 74.0, "\u{f1c0}", &format!(" {:.1}G",  stats.ram_gb),         &config.colors.base0d, "vitosettings");

    // ── System tray (StatusNotifierItem icons) ────────────────────────
    rx -= gap;
    for (ti, tray_item) in tray_items.iter().enumerate() {
        let icon_block_w = 22.0 * sf;
        if rx - icon_block_w < x + 40.0 * sf { break; }
        rx -= icon_block_w;

        r.draw_rect(rx, pad, icon_block_w, bh, &config.colors.base01);
        r.draw_rect_outline(rx, pad, icon_block_w, bh, &config.colors.base02, 1.5 * sf);

        let icon_phys = (14.0 * sf) as u32;
        let icon_x = (rx + (icon_block_w - 14.0 * sf) / 2.0) as u32;
        let icon_y = (pad + (bh - 14.0 * sf) / 2.0) as u32;

        let mut drawn = false;
        // Prefer pixmap from D-Bus if available
        if let Some(ref rgba) = tray_item.icon_rgba {
            if tray_item.icon_size > 0 {
                // Scale the SNI pixmap to our target size
                let scaled = scale_icon(rgba, tray_item.icon_size, icon_phys);
                r.draw_icon(icon_x, icon_y, icon_phys, &scaled);
                drawn = true;
            }
        }
        // Fall back to icon_name → XDG icon lookup
        if !drawn && !tray_item.icon_name.is_empty() {
            if let Some(rgba) = icons::load(&tray_item.icon_name, icon_phys) {
                r.draw_icon(icon_x, icon_y, icon_phys, &rgba);
                drawn = true;
            }
        }
        // Fall back to app_id → XDG icon lookup
        if !drawn {
            if let Some(rgba) = icons::load(&tray_item.id, icon_phys) {
                r.draw_icon(icon_x, icon_y, icon_phys, &rgba);
                drawn = true;
            }
        }
        // Last resort: font glyph
        if !drawn {
            let glyph = app_icon(&tray_item.id);
            let gw = r.measure_text(glyph, fsz);
            r.draw_text(glyph, rx + (icon_block_w - gw) / 2.0, pad + bh * 0.82, fsz, &config.colors.base05);
        }

        // Hit region: left-click activates, right-click shows menu
        hits.push(HitRegion {
            x: rx / sf, y: 2.0, w: icon_block_w / sf, h: 18.0,
            action: BarAction::TrayActivate {
                service: tray_item.service.clone(),
                id: tray_item.id.clone(),
                sni_path: tray_item.sni_path.clone(),
            },
        });

        rx -= gap;
    }

    // ── Visual borders ────────────────────────────────────────────────
    r.draw_rect_outline(0.0, 0.0, pw as f32, ph as f32, &config.colors.base02, sf);
    r.draw_rect(0.0, ph as f32 - sf, pw as f32, sf, &config.colors.base0d);

    out.top_hits = hits;

    // ── Flush ───────────────────────────────────────────────────────────
    let bgra = r.into_bgra();
    let len = canvas.len().min(bgra.len());
    canvas[..len].copy_from_slice(&bgra[..len]);

    surface.wl_surface().set_buffer_scale(scale as i32);
    surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
    buffer.attach_to(surface.wl_surface()).expect("buffer attach");
    surface.commit();
    conn.flush().ok();
}

fn draw_bot_on(
    out: &mut PerOutput,
    config: &Config,
    niri_state: &EventStreamState,
    windows_ordered: &[niri_ipc::Window],
    conn: &Connection,
    shm: &Shm,
) {
    let width  = out.width;
    let height = TASKBAR_HEIGHT;
    let scale  = out.scale;
    let pw = width  * scale;
    let ph = height * scale;
    let sf = scale as f32;

    let pool = match out.bot_pool.as_mut() {
        Some(p) => p,
        None    => {
            let size = pw as usize * ph as usize * 4 * 2;
            out.bot_pool = Some(SlotPool::new(size, shm).expect("bot pool"));
            out.bot_pool.as_mut().unwrap()
        }
    };
    let surface = match out.bot_surface.as_ref() {
        Some(s) => s,
        None    => return,
    };

    let stride = pw as i32 * 4;
    let (buffer, canvas) = pool
        .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
        .expect("failed to create buffer");

    let mut r = Renderer::new(pw, ph);
    r.clear(&config.colors.base00);

    // ── Visual borders ──────────────────────────────────────────────────
    r.draw_rect_outline(0.0, 0.0, pw as f32, ph as f32, &config.colors.base02, sf);
    r.draw_rect(0.0, 0.0, pw as f32, sf, &config.colors.base0d);

    let fsz       = config.font_size.unwrap_or(11.0) * sf;
    let pad       = 2.0 * sf;
    let bh        = 18.0 * sf;
    let text_y    = pad + bh * 0.75;
    let badge_fsz = (fsz - sf).max(8.0 * sf);

    let output_name = out.output_name.as_deref();
    let ws_map: HashMap<u64, u8> = niri_state.workspaces.workspaces.values()
        .map(|w| (w.id, w.idx)).collect();

    // Workspace IDs belonging to this output
    let output_ws_ids: std::collections::HashSet<u64> = niri_state.workspaces.workspaces.values()
        .filter(|ws| match (output_name, ws.output.as_deref()) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        })
        .map(|ws| ws.id)
        .collect();

    let mut windows: Vec<&niri_ipc::Window> = windows_ordered.iter()
        .filter(|w| w.workspace_id.map(|id| output_ws_ids.contains(&id)).unwrap_or(true))
        .collect();
    windows.sort_by_key(|w| {
        w.workspace_id.and_then(|id| ws_map.get(&id)).copied().unwrap_or(255)
    });

    let mut hits: Vec<HitRegion> = Vec::new();
    let mut tx = 4.0 * sf;
    for win in &windows {
        let block_w = 160.0 * sf;
        if tx + block_w > pw as f32 - 4.0 * sf { break; }

        let (fill, text_col) = if win.is_focused {
            (&config.colors.base02, &config.colors.base07)
        } else {
            (&config.colors.base01, &config.colors.base05)
        };
        let outline_col = if win.is_focused {
            &config.colors.base0d
        } else {
            &config.colors.base02
        };
        r.draw_rect(tx, pad, block_w, bh, fill);
        r.draw_rect_outline(tx, pad, block_w, bh, outline_col, 1.5 * sf);

        // Workspace badge
        let bdg_x = tx + 3.0 * sf;
        let bdg_y = 4.0 * sf;
        let bdg_w = 13.0 * sf;
        let bdg_h = 12.0 * sf;
        r.draw_rect(bdg_x, bdg_y, bdg_w, bdg_h, &config.colors.base00);
        r.draw_rect_outline(bdg_x, bdg_y, bdg_w, bdg_h, &config.colors.base03, 1.0 * sf);
        let ws_idx = win.workspace_id.and_then(|id| ws_map.get(&id)).copied().unwrap_or(0);
        let ws_str = ws_idx.to_string();
        let ws_tw  = r.measure_text(&ws_str, badge_fsz);
        r.draw_text(&ws_str, bdg_x + (bdg_w - ws_tw) / 2.0,
                    bdg_y + bdg_h * 0.75, badge_fsz, &config.colors.base04);

        // App icon
        let app_id       = win.app_id.as_deref().unwrap_or("unknown");
        let icon_phys    = (14.0 * sf) as u32;
        let icon_x_phys  = (tx + 19.0 * sf) as u32;
        let icon_y_phys  = (pad + (bh - 14.0 * sf) / 2.0) as u32;
        let icon_logical = 14.0 * sf;

        if let Some(rgba) = icons::load(app_id, icon_phys) {
            r.draw_icon(icon_x_phys, icon_y_phys, icon_phys, &rgba);
        } else {
            let glyph = app_icon(app_id);
            let glyph_col = if win.is_focused {
                &config.colors.base0d
            } else {
                &config.colors.base03
            };
            r.draw_text(glyph, tx + 19.0 * sf, text_y, fsz, glyph_col);
        }

        // Label
        let title  = win.title.as_deref().unwrap_or("");
        let label  = if title.is_empty() || title.eq_ignore_ascii_case(app_id) {
            app_id.to_string()
        } else {
            format!("{} - {}", app_id, title)
        };
        let label_x = tx + 19.0 * sf + icon_logical + 3.0 * sf;
        let avail_w = (block_w - (label_x - tx) - 4.0 * sf).max(0.0);
        let clipped = r.clip_text(&label, avail_w, fsz);
        r.draw_text(&clipped, label_x, text_y, fsz, text_col);

        hits.push(HitRegion {
            x: tx / sf, y: 2.0, w: 160.0, h: 18.0,
            action: BarAction::FocusWindow { id: win.id },
        });
        tx += block_w + 4.0 * sf;
    }

    out.bot_hits = hits;

    let bgra = r.into_bgra();
    let len = canvas.len().min(bgra.len());
    canvas[..len].copy_from_slice(&bgra[..len]);

    surface.wl_surface().set_buffer_scale(scale as i32);
    surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
    buffer.attach_to(surface.wl_surface()).expect("buffer attach");
    surface.commit();
    conn.flush().ok();
}

// ── Helper methods on VitoBar that destructure self for disjoint borrows ─────

impl VitoBar {
    fn refresh_tray_cache(&mut self) {
        if self.tray_dirty.swap(false, Ordering::Acquire) {
            self.tray_cached = self.tray_state.lock()
                .unwrap_or_else(|e| e.into_inner()).clone();
        }
    }

    fn redraw_all_tops(&mut self) {
        self.refresh_tray_cache();
        let VitoBar {
            ref mut outputs, ref config, ref niri_state, ref stats,
            ref windows_ordered, ref conn, ref shm, ref tray_cached, ..
        } = *self;
        for out in outputs.iter_mut() {
            if out.top_configured {
                draw_top_on(out, config, niri_state, stats, windows_ordered, conn, shm, tray_cached);
            }
        }
    }

    fn redraw_all_bots(&mut self) {
        let VitoBar {
            ref mut outputs, ref config, ref niri_state,
            ref windows_ordered, ref conn, ref shm, ..
        } = *self;
        for out in outputs.iter_mut() {
            if out.bot_configured {
                draw_bot_on(out, config, niri_state, windows_ordered, conn, shm);
            }
        }
    }

    fn toggle_bars(&mut self) {
        self.bars_hidden = !self.bars_hidden;
        if self.bars_hidden {
            for out in &mut self.outputs {
                if let Some(top) = &out.top_surface {
                    top.set_exclusive_zone(0);
                    top.set_size(0, 1);
                    top.wl_surface().attach(None, 0, 0);
                    top.wl_surface().commit();
                }
                if let Some(bot) = &out.bot_surface {
                    bot.set_exclusive_zone(0);
                    bot.set_size(0, 1);
                    bot.wl_surface().attach(None, 0, 0);
                    bot.wl_surface().commit();
                }
            }
        } else {
            for out in &mut self.outputs {
                if let Some(top) = &out.top_surface {
                    top.set_size(0, BAR_HEIGHT);
                    top.set_exclusive_zone(BAR_HEIGHT as i32);
                    top.commit();
                }
                if let Some(bot) = &out.bot_surface {
                    bot.set_size(0, TASKBAR_HEIGHT);
                    bot.set_exclusive_zone(TASKBAR_HEIGHT as i32);
                    bot.commit();
                }
                out.top_configured = false;
                out.bot_configured = false;
            }
        }
    }

    fn redraw_output_top(&mut self, idx: usize) {
        self.refresh_tray_cache();
        let VitoBar {
            ref mut outputs, ref config, ref niri_state, ref stats,
            ref windows_ordered, ref conn, ref shm, ref tray_cached, ..
        } = *self;
        if let Some(out) = outputs.get_mut(idx) {
            if out.top_configured {
                draw_top_on(out, config, niri_state, stats, windows_ordered, conn, shm, tray_cached);
            }
        }
    }

    fn redraw_output_bot(&mut self, idx: usize) {
        let VitoBar {
            ref mut outputs, ref config, ref niri_state,
            ref windows_ordered, ref conn, ref shm, ..
        } = *self;
        if let Some(out) = outputs.get_mut(idx) {
            if out.bot_configured {
                draw_bot_on(out, config, niri_state, windows_ordered, conn, shm);
            }
        }
    }

    fn show_popup(&mut self, window_id: u64, output_idx: usize, click_x: f32) {
        self.create_popup(PopupKind::WindowMenu { window_id }, output_idx, POPUP_ITEMS, click_x, false);
    }

    fn create_popup(&mut self, kind: PopupKind, output_idx: usize, num_items: u32, menu_x: f32, anchor_top: bool) {
        self.dismiss_popup();

        let (output, scale) = match self.outputs.get(output_idx) {
            Some(o) => (&o.output, o.scale),
            None => return,
        };

        // Fullscreen overlay so click-outside and ESC work
        let popup_wl = self.compositor.create_surface(&self.qh);
        let popup = self.layer_shell.create_layer_surface(
            &self.qh, popup_wl, Layer::Overlay,
            Some("vitobar-popup"), Some(output),
        );
        popup.set_size(0, 0);
        popup.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        popup.set_exclusive_zone(-1);
        popup.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        popup.commit();

        self.popup = Some(PopupState {
            kind,
            surface: popup,
            pool: None,
            configured: false,
            hits: Vec::new(),
            pointer_pos: (0.0, 0.0),
            num_items,
            width: 0,
            height: 0,
            menu_x,
            anchor_top,
            scale,
            last_hovered_idx: None,
        });
    }

    fn dismiss_popup(&mut self) {
        if let Some(popup) = self.popup.take() {
            // Clone the wl_surface handle before dropping the LayerSurface,
            // so we can destroy it AFTER the role object is cleaned up.
            // Destroying wl_surface before its role is a Wayland protocol error.
            let wl_surf = popup.surface.wl_surface().clone();
            drop(popup); // drops LayerSurface first (sends role destroy)
            wl_surf.destroy();
        }
    }

    fn show_tray_popup(&mut self, item: TrayItem, output_idx: usize, click_x: f32) {
        // Fetch menu in background thread, then send results via channel
        let tx = self.tray_menu_tx.clone();
        let tray_idx = self.tray_cached.iter().position(|t| t.service == item.service && t.id == item.id).unwrap_or(0);
        let item_clone = item.clone();

        std::thread::spawn(move || {
            let menu_items = tray::fetch_menu_items(&item_clone);
            let _ = tx.send((tray_idx, menu_items));
        });

        self.dismiss_popup();
        self.pending_tray_popup = Some((item, output_idx, click_x));
    }

    fn draw_popup(&mut self) {
        let VitoBar {
            ref mut popup, ref config, ref conn, ref shm, ..
        } = *self;

        let popup = match popup.as_mut() {
            Some(p) if p.configured && p.width > 0 && p.height > 0 => p,
            _ => return,
        };

        let scale = popup.scale.max(1);
        let num_items = popup.num_items;
        let sf = scale as f32;

        // Fullscreen overlay dimensions (for the Wayland buffer)
        let ow = popup.width * scale;
        let oh = popup.height * scale;

        // Menu dimensions and position in physical coords
        let menu_pw = (POPUP_WIDTH as f32 * sf) as u32;
        let menu_ph = (POPUP_ITEM_H * num_items) as f32 * sf;
        let menu_ph_u = menu_ph.ceil() as u32;
        let menu_x_phys = (popup.menu_x * sf).min(ow as f32 - menu_pw as f32).max(0.0);
        let menu_y_phys = if popup.anchor_top {
            BAR_HEIGHT as f32 * sf
        } else {
            oh as f32 - TASKBAR_HEIGHT as f32 * sf - menu_ph
        }.max(0.0);

        // Logical coords for hit regions
        let menu_x_log = menu_x_phys / sf;
        let menu_y_log = menu_y_phys / sf;

        // Pointer position relative to the menu origin (for hover detection)
        let pointer_in_menu = (
            popup.pointer_pos.0 - menu_x_log as f64,
            popup.pointer_pos.1 - menu_y_log as f64,
        );

        let kind = popup.kind.clone();

        let pool = match popup.pool.as_mut() {
            Some(p) => p,
            None => {
                let size = ow as usize * oh as usize * 4;
                match SlotPool::new(size, shm) {
                    Ok(p) => { popup.pool = Some(p); popup.pool.as_mut().unwrap() }
                    Err(e) => { log::error!("popup pool alloc failed: {e}"); return; }
                }
            }
        };

        let stride = ow as i32 * 4;
        let (buffer, canvas) = match pool
            .create_buffer(ow as i32, oh as i32, stride, wl_shm::Format::Argb8888)
        {
            Ok(bc) => bc,
            Err(e) => { log::error!("popup buffer alloc failed: {e}"); return; }
        };

        // Clear canvas to transparent (zero)
        canvas.fill(0);

        // Render only the menu-sized region (not fullscreen!)
        let mut r = Renderer::new(menu_pw, menu_ph_u);

        // Menu background + border (drawn at origin of small renderer)
        r.draw_rect(0.0, 0.0, menu_pw as f32, menu_ph, &config.colors.base00);
        r.draw_rect_outline(0.0, 0.0, menu_pw as f32, menu_ph, &config.colors.base0d, 1.5 * sf);

        let fsz = config.font_size.unwrap_or(11.0) * sf;
        let mut hits: Vec<HitRegion> = Vec::new();

        match kind {
            PopupKind::WindowMenu { window_id } => {
                let items: Vec<(&str, &str, BarAction)> = vec![
                    ("\u{f00d}", " Close",      BarAction::CloseWindow { id: window_id }),
                    ("\u{f2d0}", " Maximize",   BarAction::MaximizeColumn),
                    ("\u{f065}", " Fullscreen", BarAction::FullscreenWindow { id: window_id }),
                    ("\u{f24d}", " Float/Tile", BarAction::ToggleFloating { id: window_id }),
                    ("\u{f6d7}", " \u{2192} WS 1", BarAction::MoveToWorkspace { window_id, ws_idx: 1 }),
                    ("\u{f6d7}", " \u{2192} WS 2", BarAction::MoveToWorkspace { window_id, ws_idx: 2 }),
                    ("\u{f6d7}", " \u{2192} WS 3", BarAction::MoveToWorkspace { window_id, ws_idx: 3 }),
                    ("\u{f6d7}", " \u{2192} WS 4", BarAction::MoveToWorkspace { window_id, ws_idx: 4 }),
                    ("\u{f6d7}", " \u{2192} WS 5", BarAction::MoveToWorkspace { window_id, ws_idx: 5 }),
                ];
                draw_popup_items(&mut r, pointer_in_menu, &items, config, fsz, sf, 0.0, 0.0, POPUP_WIDTH, &mut hits, num_items);
            }
            PopupKind::TrayMenu { item: _, ref menu_items } => {
                let items: Vec<(String, String, BarAction)> = menu_items.iter()
                    .filter(|m| !m.is_separator && !m.label.is_empty())
                    .map(|m| {
                        (
                            "\u{f0da}".to_string(),
                            format!(" {}", m.label),
                            BarAction::TrayMenuItem { tray_idx: 0, menu_id: m.id },
                        )
                    })
                    .collect();
                let refs: Vec<(&str, &str, BarAction)> = items.iter()
                    .map(|(i, l, a)| (i.as_str(), l.as_str(), a.clone()))
                    .collect();
                draw_popup_items(&mut r, pointer_in_menu, &refs, config, fsz, sf, 0.0, 0.0, POPUP_WIDTH, &mut hits, num_items);
            }
        }

        // Offset hit regions from menu-local to overlay-global logical coords
        for hit in &mut hits {
            hit.x += menu_x_log;
            hit.y += menu_y_log;
        }
        popup.hits = hits;

        // Blit the small menu renderer into the fullscreen canvas at the right position
        let bgra = r.into_bgra();
        let menu_stride = menu_pw as usize * 4;
        let canvas_stride = ow as usize * 4;
        let start_x = menu_x_phys as usize;
        let start_y = menu_y_phys as usize;
        for row in 0..menu_ph_u as usize {
            let dst_y = start_y + row;
            if dst_y >= oh as usize { break; }
            let src_off = row * menu_stride;
            let dst_off = dst_y * canvas_stride + start_x * 4;
            let copy_len = menu_stride.min(canvas.len().saturating_sub(dst_off));
            if src_off + copy_len <= bgra.len() {
                canvas[dst_off..dst_off + copy_len].copy_from_slice(&bgra[src_off..src_off + copy_len]);
            }
        }

        popup.surface.wl_surface().set_buffer_scale(scale as i32);
        popup.surface.wl_surface().damage_buffer(0, 0, ow as i32, oh as i32);
        buffer.attach_to(popup.surface.wl_surface()).expect("popup attach");
        popup.surface.commit();
        conn.flush().ok();
    }
}

fn draw_popup_items(
    r: &mut Renderer,
    pointer_pos: (f64, f64),
    items: &[(&str, &str, BarAction)],
    config: &Config,
    fsz: f32,
    sf: f32,
    menu_x: f32,
    menu_y: f32,
    menu_w: u32,
    hits: &mut Vec<HitRegion>,
    num_items: u32,
) {
    let item_h = POPUP_ITEM_H as f32 * sf;
    let mw = menu_w as f32 * sf;
    let pad_x  = 6.0 * sf;
    for (i, (icon, label, action)) in items.iter().enumerate() {
        let y = menu_y + i as f32 * item_h;

        let (px, py) = pointer_pos;
        let hovered = px as f32 >= menu_x && (px as f32) < menu_x + mw
            && py as f32 >= y && (py as f32) < y + item_h;
        let (bg, fg) = if hovered {
            (&config.colors.base02, &config.colors.base07)
        } else {
            (&config.colors.base00, &config.colors.base05)
        };
        r.draw_rect(menu_x + 1.5 * sf, y, mw - 3.0 * sf, item_h, bg);

        let icon_col = if i == 0 { &config.colors.base08 } else { &config.colors.base0d };
        let text_y = y + item_h * 0.75;
        let iw = r.measure_text(icon, fsz);
        r.draw_text(icon, menu_x + pad_x, text_y, fsz, icon_col);
        r.draw_text(label, menu_x + pad_x + iw, text_y, fsz, fg);

        if i < num_items as usize - 1 {
            r.draw_rect(menu_x + 4.0 * sf, y + item_h - 1.0, mw - 8.0 * sf, 1.0, &config.colors.base01);
        }

        // Hit regions in logical coords (fullscreen overlay space)
        hits.push(HitRegion {
            x: menu_x / sf, y: y / sf, w: menu_w as f32, h: POPUP_ITEM_H as f32,
            action: action.clone(),
        });
    }
}

// ── Trait implementations required by SCTK ──────────────────────────────────

impl CompositorHandler for VitoBar {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>,
                            surface: &wl_surface::WlSurface, factor: i32) {
        if let Some(ref mut popup) = self.popup {
            if popup.surface.wl_surface() == surface {
                let new_scale = factor.max(1) as u32;
                if new_scale != popup.scale {
                    popup.scale = new_scale;
                    popup.pool = None; // force re-allocation for new scale
                }
                return;
            }
        }
        for out in &mut self.outputs {
            let is_mine = out.top_surface.as_ref().map(|s| s.wl_surface() == surface).unwrap_or(false)
                || out.bot_surface.as_ref().map(|s| s.wl_surface() == surface).unwrap_or(false);
            if is_mine {
                let new_scale = factor.max(1) as u32;
                if new_scale != out.scale {
                    out.scale = new_scale;
                    out.top_pool = None;
                    out.bot_pool = None;
                }
                break;
            }
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for VitoBar {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }

    fn new_output(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let output_name = self.output_state.info(&output)
            .and_then(|info| info.name.clone());
        log::info!("new output detected: {:?}, creating bar surfaces", output_name);

        // Top bar surface bound to this output
        let top_wl = self.compositor.create_surface(qh);
        let top = self.layer_shell.create_layer_surface(
            qh, top_wl, Layer::Top, Some("vitobar-top"), Some(&output),
        );
        top.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        top.set_size(0, BAR_HEIGHT);
        top.set_exclusive_zone(BAR_HEIGHT as i32);
        top.set_keyboard_interactivity(KeyboardInteractivity::None);
        top.commit();

        // Bottom taskbar surface bound to this output
        let bot_wl = self.compositor.create_surface(qh);
        let bot = self.layer_shell.create_layer_surface(
            qh, bot_wl, Layer::Top, Some("vitobar-bot"), Some(&output),
        );
        bot.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        bot.set_size(0, TASKBAR_HEIGHT);
        bot.set_exclusive_zone(TASKBAR_HEIGHT as i32);
        bot.set_keyboard_interactivity(KeyboardInteractivity::None);
        bot.commit();

        self.outputs.push(PerOutput {
            output,
            output_name,
            top_surface: Some(top),
            top_pool:    None,
            bot_surface: Some(bot),
            bot_pool:    None,
            width:       1920,
            scale:       1,
            top_configured: false,
            bot_configured: false,
            top_hits:        Vec::new(),
            top_tray_hits:   Vec::new(),
            bot_hits:        Vec::new(),
            top_pointer_pos: (0.0, 0.0),
            bot_pointer_pos: (0.0, 0.0),
        });
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.outputs.retain(|o| o.output != output);
    }
}

impl LayerShellHandler for VitoBar {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &LayerSurface) {
        // Check if the popup was closed
        if self.popup.as_ref().map(|p| p.surface.wl_surface() == surface.wl_surface()).unwrap_or(false) {
            self.popup = None;
            return;
        }
        self.running = false;
    }

    fn configure(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>,
                 surface: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
        // Check if this is the popup surface
        if let Some(ref mut popup) = self.popup {
            if popup.surface.wl_surface() == surface.wl_surface() {
                if configure.new_size.0 > 0 { popup.width  = configure.new_size.0; }
                if configure.new_size.1 > 0 { popup.height = configure.new_size.1; }
                popup.configured = true;
                self.draw_popup();
                return;
            }
        }

        // Find which output owns this surface
        let idx_and_kind = self.outputs.iter().enumerate().find_map(|(i, out)| {
            if out.top_surface.as_ref().map(|s| s.wl_surface() == surface.wl_surface()).unwrap_or(false) {
                Some((i, true))
            } else if out.bot_surface.as_ref().map(|s| s.wl_surface() == surface.wl_surface()).unwrap_or(false) {
                Some((i, false))
            } else {
                None
            }
        });

        let Some((idx, is_top)) = idx_and_kind else { return; };

        if configure.new_size.0 > 0 {
            self.outputs[idx].width = configure.new_size.0;
        }

        if is_top && !self.outputs[idx].top_configured {
            self.outputs[idx].top_configured = true;
            self.redraw_output_top(idx);
        }

        if !is_top && !self.outputs[idx].bot_configured {
            self.outputs[idx].bot_configured = true;
            self.redraw_output_bot(idx);
        }
    }
}

impl ShmHandler for VitoBar {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl SeatHandler for VitoBar {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>,
                      seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            let ptr = self.seat_state.get_pointer(qh, &seat)
                .expect("failed to create pointer");
            self.pointer = Some(ptr);
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = Some(self.seat_state.get_keyboard(qh, &seat, None)
                .expect("keyboard"));
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>,
                         _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() { p.release(); }
        }
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() { k.release(); }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for VitoBar {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>,
                     _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        use PointerEventKind::*;

        let mut popup_action: Option<BarAction> = None;
        let mut show_popup: Option<(u64, usize, f32)> = None; // (window_id, output_idx, click_x)
        let mut show_tray_menu: Option<(TrayItem, usize, f32)> = None; // (tray_item, output_idx, click_x)
        let mut dismiss_popup = false;
        let mut need_popup_redraw = false;

        for event in events {
            // ── Check if event is on the popup surface ──
            let is_popup = self.popup.as_ref()
                .map(|p| p.surface.wl_surface() == &event.surface)
                .unwrap_or(false);

            if is_popup {
                match &event.kind {
                    Motion { .. } => {
                        if let Some(ref mut popup) = self.popup {
                            popup.pointer_pos = event.position;
                            // Compute which menu item is hovered; skip redraw if unchanged
                            let (px, py) = (event.position.0 as f32, event.position.1 as f32);
                            let menu_w = POPUP_WIDTH as f32;
                            let menu_h = (POPUP_ITEM_H * popup.num_items) as f32;
                            let mx = popup.menu_x;
                            let my = if popup.anchor_top {
                                BAR_HEIGHT as f32
                            } else {
                                popup.height as f32 - TASKBAR_HEIGHT as f32 - menu_h
                            };
                            let new_idx = if px >= mx && px < mx + menu_w && py >= my && py < my + menu_h {
                                Some(((py - my) / POPUP_ITEM_H as f32) as usize)
                            } else {
                                None
                            };
                            if new_idx != popup.last_hovered_idx {
                                popup.last_hovered_idx = new_idx;
                                need_popup_redraw = true;
                            }
                        }
                    }
                    Press { button, .. } if *button == smithay_client_toolkit::seat::pointer::BTN_LEFT => {
                        if let Some(ref popup) = self.popup {
                            let (lx, ly) = (popup.pointer_pos.0 as f32, popup.pointer_pos.1 as f32);
                            // Check if click is inside the menu content area
                            let menu_w = POPUP_WIDTH as f32;
                            let menu_h = (POPUP_ITEM_H * popup.num_items) as f32;
                            let mx = popup.menu_x;
                            let my = if popup.anchor_top {
                                BAR_HEIGHT as f32
                            } else {
                                popup.height as f32 - TASKBAR_HEIGHT as f32 - menu_h
                            };
                            if lx >= mx && lx < mx + menu_w && ly >= my && ly < my + menu_h {
                                let hits = popup.hits.clone();
                                if let Some(hit) = hits.iter().find(|h| {
                                    lx >= h.x && lx < h.x + h.w && ly >= h.y && ly < h.y + h.h
                                }) {
                                    popup_action = Some(hit.action.clone());
                                }
                            }
                            // Any click (inside or outside menu) dismisses
                        }
                        dismiss_popup = true;
                    }
                    Press { .. } => {
                        // Right-click or other button → dismiss
                        dismiss_popup = true;
                    }
                    _ => {}
                }
                continue;
            }

            // ── Normal bar surfaces ──
            for (out_idx, out) in self.outputs.iter_mut().enumerate() {
                let is_top = out.top_surface.as_ref()
                    .map(|s| s.wl_surface() == &event.surface).unwrap_or(false);
                let is_bot = out.bot_surface.as_ref()
                    .map(|s| s.wl_surface() == &event.surface).unwrap_or(false);

                if !is_top && !is_bot { continue; }

                match &event.kind {
                    Motion { .. } => {
                        if is_top { out.top_pointer_pos = event.position; }
                        if is_bot { out.bot_pointer_pos = event.position; }
                    }
                    Press { button, .. } if *button == smithay_client_toolkit::seat::pointer::BTN_LEFT => {
                        let (pos, hits) = if is_top {
                            (out.top_pointer_pos, out.top_hits.clone())
                        } else {
                            (out.bot_pointer_pos, out.bot_hits.clone())
                        };
                        let (lx, ly) = (pos.0 as f32, pos.1 as f32);
                        if let Some(hit) = hits.iter().find(|h| {
                            lx >= h.x && lx < h.x + h.w && ly >= h.y && ly < h.y + h.h
                        }) {
                            fire_action(hit.action.clone());
                        }
                    }
                    // Right-click on bottom bar → show window context menu
                    Press { button, .. } if *button == smithay_client_toolkit::seat::pointer::BTN_RIGHT && is_bot => {
                        let pos = out.bot_pointer_pos;
                        let hits = out.bot_hits.clone();
                        let (lx, ly) = (pos.0 as f32, pos.1 as f32);
                        if let Some(hit) = hits.iter().find(|h| {
                            lx >= h.x && lx < h.x + h.w && ly >= h.y && ly < h.y + h.h
                        }) {
                            if let BarAction::FocusWindow { id } = hit.action {
                                show_popup = Some((id, out_idx, hit.x));
                            }
                        }
                    }
                    // Right-click on top bar tray icon → show tray menu
                    Press { button, .. } if *button == smithay_client_toolkit::seat::pointer::BTN_RIGHT && is_top => {
                        let pos = out.top_pointer_pos;
                        let hits = out.top_hits.clone();
                        let (lx, ly) = (pos.0 as f32, pos.1 as f32);
                        if let Some(hit) = hits.iter().find(|h| {
                            lx >= h.x && lx < h.x + h.w && ly >= h.y && ly < h.y + h.h
                        }) {
                            if let BarAction::TrayActivate { ref service, ref id, .. } = hit.action {
                                if let Some(item) = self.tray_cached.iter().find(|t| t.service == *service && t.id == *id) {
                                    let item = item.clone();
                                    show_tray_menu = Some((item, out_idx, hit.x));
                                }
                            }
                        }
                    }
                    _ => {}
                }
                break;
            }
        }

        // Apply deferred popup actions
        if let Some(action) = popup_action {
            match action {
                BarAction::TrayMenuItem { menu_id, .. } => {
                    // Fire the DBusMenu event before dismissing
                    if let Some(ref popup) = self.popup {
                        if let PopupKind::TrayMenu { ref item, .. } = popup.kind {
                            let item = item.clone();
                            std::thread::spawn(move || {
                                tray::activate_menu_item(&item, menu_id);
                            });
                        }
                    }
                }
                other => fire_action(other),
            }
        }
        if dismiss_popup {
            self.dismiss_popup();
        }
        if let Some((window_id, output_idx, click_x)) = show_popup {
            self.show_popup(window_id, output_idx, click_x);
        }
        let had_tray_menu = show_tray_menu.is_some();
        if let Some((tray_item, output_idx, click_x)) = show_tray_menu {
            self.show_tray_popup(tray_item, output_idx, click_x);
        }
        if need_popup_redraw && !dismiss_popup && show_popup.is_none() && !had_tray_menu {
            self.draw_popup();
        }
    }
}

impl KeyboardHandler for VitoBar {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>,
                 _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        if event.keysym == Keysym::Escape {
            self.dismiss_popup();
        }
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>,
                   _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>,
                        _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}
    fn update_repeat_info(&mut self, _: &Connection, _: &QueueHandle<Self>,
                          _: &wl_keyboard::WlKeyboard, _: RepeatInfo) {}
}

impl ProvidesRegistryState for VitoBar {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(VitoBar);
delegate_output!(VitoBar);
delegate_layer!(VitoBar);
delegate_shm!(VitoBar);
delegate_seat!(VitoBar);
delegate_keyboard!(VitoBar);
delegate_pointer!(VitoBar);
delegate_registry!(VitoBar);

/// Bilinear-interpolated scale of an RGBA icon from src_size to dst_size.
fn scale_icon(rgba: &[u8], src_size: u32, dst_size: u32) -> Vec<u8> {
    if src_size == dst_size { return rgba.to_vec(); }
    bilinear_scale(rgba, src_size, dst_size)
}

fn bilinear_scale(rgba: &[u8], src_size: u32, dst_size: u32) -> Vec<u8> {
    let t = dst_size as usize;
    let s = src_size as usize;
    let mut out = vec![0u8; t * t * 4];
    let ratio = s as f32 / t as f32;
    for dy in 0..t {
        for dx in 0..t {
            let src_x = (dx as f32 + 0.5) * ratio - 0.5;
            let src_y = (dy as f32 + 0.5) * ratio - 0.5;
            let x0 = (src_x.floor() as isize).max(0) as usize;
            let y0 = (src_y.floor() as isize).max(0) as usize;
            let x1 = (x0 + 1).min(s - 1);
            let y1 = (y0 + 1).min(s - 1);
            let fx = src_x - x0 as f32;
            let fy = src_y - y0 as f32;
            let fx = fx.clamp(0.0, 1.0);
            let fy = fy.clamp(0.0, 1.0);

            let i00 = (y0 * s + x0) * 4;
            let i10 = (y0 * s + x1) * 4;
            let i01 = (y1 * s + x0) * 4;
            let i11 = (y1 * s + x1) * 4;
            let di  = (dy * t + dx) * 4;

            if i11 + 3 >= rgba.len() { continue; }
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
    out
}

fn app_icon(app_id: &str) -> &'static str {
    let s = app_id.to_ascii_lowercase();
    let s = s.as_str();
    if s.contains("firefox") || s.contains("librewolf") { "\u{f269}" }
    else if s.contains("chromium") || s.contains("chrome") || s.contains("brave") { "\u{f268}" }
    else if s.contains("foot") || s.contains("alacritty") || s.contains("kitty") ||
            s.contains("wezterm") || s.contains("urxvt") || s.contains("xterm") { "\u{f120}" }
    else if s.contains("nvim") || s.contains("neovim") { "\u{e62b}" }
    else if s.contains("vim") { "\u{e62b}" }
    else if s.contains("emacs") { "\u{e632}" }
    else if s.contains("code") || s.contains("vscode") { "\u{e70c}" }
    else if s.contains("discord") || s.contains("vesktop") { "\u{f392}" }
    else if s.contains("telegram") || s.contains("tdesktop") { "\u{f2c6}" }
    else if s.contains("slack") { "\u{f198}" }
    else if s.contains("spotify") { "\u{f1bc}" }
    else if s.contains("mpv") || s.contains("vlc") { "\u{f03d}" }
    else if s.contains("thunar") || s.contains("nautilus") ||
            s.contains("dolphin") || s.contains("nemo") { "\u{f07b}" }
    else if s.contains("obsidian") { "\u{f1d8}" }
    else if s.contains("steam") { "\u{f1b6}" }
    else if s.contains("ubisoft") || s.contains("uplay") || s.contains("uconnect") { "\u{f11b}" }
    else { "\u{f2d0}" }
}

fn find_font() -> String {
    let out = std::process::Command::new("fc-match")
        .args(["JetBrainsMono Nerd Font Mono", "--format=%{file}"])
        .output()
        .expect("fc-match failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn main() {
    env_logger::init();
    log::info!("vitobar starting...");
    let font_path = find_font();
    render::load_font(&font_path);
    log::info!("font loaded from: {}", font_path);

    let config  = Config::load();
    let mut monitor = SysMonitor::new();
    let initial_stats = monitor.refresh();

    let conn = Connection::connect_to_env().expect("failed to connect to Wayland");
    let (globals, queue) = registry_queue_init::<VitoBar>(&conn).expect("failed to init registry");
    let qh = queue.handle();

    let compositor  = CompositorState::bind(&globals, &qh).expect("compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell not available");
    let shm         = Shm::bind(&globals, &qh).expect("shm not available");
    let seat_state  = SeatState::new(&globals, &qh);
    let output_state = OutputState::new(&globals, &qh);

    // ── Pre-populate niri state so first draw has real data ──────────────
    let mut niri_state = EventStreamState::default();
    if let Ok(mut s) = NiriSocket::connect() {
        if let Ok(Ok(NiriResponse::Workspaces(ws))) = s.send(NiriRequest::Workspaces) {
            niri_state.apply(NiriEvent::WorkspacesChanged { workspaces: ws });
        }
    }
    let mut windows_ordered: Vec<niri_ipc::Window> = Vec::new();
    if let Ok(mut s) = NiriSocket::connect() {
        if let Ok(Ok(NiriResponse::Windows(wins))) = s.send(NiriRequest::Windows) {
            niri_state.apply(NiriEvent::WindowsChanged { windows: wins.clone() });
            let ws_map: HashMap<u64, u8> = niri_state.workspaces.workspaces.values()
                .map(|w| (w.id, w.idx)).collect();
            let mut ordered = wins;
            sort_windows_by_position(&mut ordered, &ws_map);
            windows_ordered = ordered;
        }
    }

    // Surfaces are created dynamically in new_output() — no manual surface
    // creation here. The OutputHandler will be called for each connected monitor.

    // ── System tray ───────────────────────────────────────────────────
    let (tray_state, tray_dirty) = tray::spawn_tray_watcher();
    let (tray_menu_tx, tray_menu_rx) = calloop_channel::<(usize, Vec<tray::MenuItem>)>();

    let mut app = VitoBar {
        registry_state: RegistryState::new(&globals),
        output_state,
        compositor,
        layer_shell,
        shm,
        outputs:     Vec::new(),
        conn:        conn.clone(),
        qh:          qh.clone(),
        config,
        monitor,
        stats:       initial_stats,
        running:     true,
        bars_hidden: false,
        toggle_signal: Arc::new(AtomicBool::new(false)),
        niri_state,
        windows_ordered,
        seat_state,
        pointer:     None,
        keyboard:    None,
        popup:       None,
        tray_state,
        tray_dirty,
        tray_cached: Vec::new(),
        tray_menu_tx,
        pending_tray_popup: None,
        last_time_string: String::new(),
    };

    // ── Event loop ──────────────────────────────────────────────────────
    let mut event_loop: EventLoop<VitoBar> = EventLoop::try_new().expect("event loop");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    // ── Niri EventStream — background thread ────────────────────────────
    let (niri_tx, niri_rx) = calloop_channel::<NiriEvent>();
    std::thread::spawn(move || {
        loop {
            let result = NiriSocket::connect().and_then(|mut s| {
                let _ = s.send(NiriRequest::EventStream)?;
                Ok(s.read_events())
            });
            match result {
                Ok(mut read_event) => {
                    log::info!("niri: event stream connected");
                    loop {
                        match read_event() {
                            Ok(ev) => { if niri_tx.send(ev).is_err() { return; } }
                            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                                log::debug!("niri: skipping unknown event: {e}");
                            }
                            Err(e) => {
                                log::warn!("niri stream: {e}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => log::warn!("niri connect: {e}"),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    event_loop.handle()
        .insert_source(niri_rx, |ev, _, app: &mut VitoBar| {
            let ChannelEvent::Msg(event) = ev else { return; };

            match &event {
                NiriEvent::WindowsChanged { windows } => {
                    let ws_map: HashMap<u64, u8> = app.niri_state.workspaces.workspaces.values()
                        .map(|w| (w.id, w.idx)).collect();
                    let mut ordered = windows.clone();
                    sort_windows_by_position(&mut ordered, &ws_map);
                    app.windows_ordered = ordered;
                }
                NiriEvent::WindowLayoutsChanged { changes } => {
                    for (id, layout) in changes {
                        if let Some(w) = app.windows_ordered.iter_mut().find(|w| w.id == *id) {
                            w.layout = layout.clone();
                        }
                    }
                    let ws_map: HashMap<u64, u8> = app.niri_state.workspaces.workspaces.values()
                        .map(|w| (w.id, w.idx)).collect();
                    sort_windows_by_position(&mut app.windows_ordered, &ws_map);
                }
                NiriEvent::WindowOpenedOrChanged { window } => {
                    if let Some(pos) = app.windows_ordered.iter().position(|w| w.id == window.id) {
                        app.windows_ordered[pos] = window.clone();
                    } else {
                        app.windows_ordered.push(window.clone());
                        let ws_map: HashMap<u64, u8> = app.niri_state.workspaces.workspaces.values()
                            .map(|w| (w.id, w.idx)).collect();
                        sort_windows_by_position(&mut app.windows_ordered, &ws_map);
                    }
                    if window.is_focused {
                        let focused_id = window.id;
                        for w in &mut app.windows_ordered {
                            if w.id != focused_id { w.is_focused = false; }
                        }
                    }
                }
                NiriEvent::WindowClosed { id } => {
                    app.windows_ordered.retain(|w| w.id != *id);
                }
                NiriEvent::WindowFocusChanged { id } => {
                    let focused = *id;
                    for w in &mut app.windows_ordered {
                        w.is_focused = Some(w.id) == focused;
                    }
                }
                _ => {}
            }

            let redraw_top = matches!(event,
                NiriEvent::WorkspacesChanged { .. }        |
                NiriEvent::WorkspaceActivated { .. }       |
                NiriEvent::WorkspaceActiveWindowChanged { .. } |
                NiriEvent::WindowsChanged { .. }           |
                NiriEvent::WindowOpenedOrChanged { .. }    |
                NiriEvent::WindowClosed { .. }             |
                NiriEvent::WindowFocusChanged { .. }
            );
            let redraw_bot = matches!(event,
                NiriEvent::WindowsChanged { .. }           |
                NiriEvent::WindowLayoutsChanged { .. }     |
                NiriEvent::WindowOpenedOrChanged { .. }    |
                NiriEvent::WindowClosed { .. }             |
                NiriEvent::WindowFocusChanged { .. }
            );
            app.niri_state.apply(event);
            if redraw_top { app.redraw_all_tops(); }
            if redraw_bot { app.redraw_all_bots(); }
        })
        .expect("niri channel");

    // ── Tray menu results channel ─────────────────────────────────────
    event_loop.handle()
        .insert_source(tray_menu_rx, |ev, _, app: &mut VitoBar| {
            let ChannelEvent::Msg((_tray_idx, menu_items)) = ev else { return; };
            if let Some((item, output_idx, menu_x)) = app.pending_tray_popup.take() {
                let visible: Vec<_> = menu_items.iter()
                    .filter(|m| !m.is_separator && !m.label.is_empty())
                    .cloned()
                    .collect();
                let count = visible.len().max(1) as u32;
                let kind = PopupKind::TrayMenu { item, menu_items: visible };
                app.create_popup(kind, output_idx, count, menu_x, true);
            }
        })
        .expect("tray menu channel");

    // ── Clock redraw: every 5s, only redraws if time string changed ──
    event_loop.handle()
        .insert_source(
            Timer::from_duration(Duration::from_secs(5)),
            |_, _, app: &mut VitoBar| {
                let now = get_time_string();
                if now != app.last_time_string {
                    app.last_time_string = now;
                    app.redraw_all_tops();
                }
                TimeoutAction::ToDuration(Duration::from_secs(5))
            },
        )
        .expect("clock timer");

    // ── Sysinfo refresh: every 5s (CPU, memory, volume, etc.) ──────────
    event_loop.handle()
        .insert_source(
            Timer::from_duration(Duration::from_secs(5)),
            |_, _, app: &mut VitoBar| {
                app.stats = app.monitor.refresh();
                TimeoutAction::ToDuration(Duration::from_secs(5))
            },
        )
        .expect("sysinfo timer");

    // ── Background apps detection: every 30s (expensive process scan) ──
    event_loop.handle()
        .insert_source(
            Timer::from_duration(Duration::from_secs(30)),
            |_, _, app: &mut VitoBar| {
                let window_app_ids: Vec<String> = app.windows_ordered.iter()
                    .filter_map(|w| w.app_id.clone())
                    .collect();
                app.stats.background_apps = app.monitor.detect_background_apps(&window_app_ids);
                TimeoutAction::ToDuration(Duration::from_secs(30))
            },
        )
        .expect("background apps timer");

    // ── SIGUSR1 toggle: hide/show bars (for OLED burn-in prevention) ──
    let toggle_flag = app.toggle_signal.clone();
    signal_hook::flag::register(signal_hook::consts::SIGUSR1, toggle_flag)
        .expect("SIGUSR1 handler");

    event_loop.handle()
        .insert_source(
            Timer::from_duration(Duration::from_millis(100)),
            |_, _, app: &mut VitoBar| {
                if app.toggle_signal.swap(false, Ordering::Relaxed) {
                    app.toggle_bars();
                }
                TimeoutAction::ToDuration(Duration::from_millis(100))
            },
        )
        .expect("toggle timer");

    while app.running {
        event_loop.dispatch(None, &mut app).expect("dispatch failed");
    }
}
