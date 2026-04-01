mod config;
mod render;
mod modules;
mod icons;

use config::Config;
use modules::{
    clock::get_time_string,
    sysinfo::{SysMonitor, SysStats},
    bluetooth::BluetoothStatus,
};
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
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
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
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

#[derive(Debug, Clone)]
enum BarAction {
    FocusWorkspace { id: u64 },
    FocusWindow    { id: u64 },
    Spawn          { cmd: String },
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
    }
}

use calloop::{EventLoop, channel::{channel as calloop_channel, Event as ChannelEvent}, timer::{Timer, TimeoutAction}};
use calloop_wayland_source::WaylandSource;
use std::time::Duration;

const BAR_HEIGHT:     u32 = 22;
const TASKBAR_HEIGHT: u32 = 22;

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

    niri_state:      EventStreamState,
    windows_ordered: Vec<niri_ipc::Window>,

    seat_state:      SeatState,
    pointer:         Option<wl_pointer::WlPointer>,
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
            let size = width as usize * BAR_HEIGHT as usize * 4 * 4;
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
    status_block!( 68.0, "\u{f185}", &format!(" {:>3}%",  stats.brightness_pct), &config.colors.base0a, "vitosettings");
    status_block!( 68.0, "\u{f0e7}", &format!(" {:>3.0}%", stats.cpu_pct),       &config.colors.base09, "vitosettings");

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

    // ── Background apps tray (running processes without niri windows) ───
    rx -= gap;
    for app_id in &stats.background_apps {
        let icon_block_w = 22.0 * sf;
        if rx - icon_block_w < x + 40.0 * sf { break; }
        rx -= icon_block_w;

        r.draw_rect(rx, pad, icon_block_w, bh, &config.colors.base01);
        r.draw_rect_outline(rx, pad, icon_block_w, bh, &config.colors.base02, 1.5 * sf);

        let icon_phys = (14.0 * sf) as u32;
        let icon_x = (rx + (icon_block_w - 14.0 * sf) / 2.0) as u32;
        let icon_y = (pad + (bh - 14.0 * sf) / 2.0) as u32;
        if let Some(rgba) = icons::load(app_id, icon_phys) {
            r.draw_icon(icon_x, icon_y, icon_phys, &rgba);
        } else {
            let glyph = app_icon(app_id);
            let gw = r.measure_text(glyph, fsz);
            r.draw_text(glyph, rx + (icon_block_w - gw) / 2.0, pad + bh * 0.82, fsz, &config.colors.base05);
        }

        rx -= gap;
    }

    out.top_hits = hits;

    // ── Flush ───────────────────────────────────────────────────────────
    let bgra = r.as_bgra();
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
            let size = width as usize * TASKBAR_HEIGHT as usize * 4 * 4;
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

    let bgra = r.as_bgra();
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
    fn redraw_all_tops(&mut self) {
        let VitoBar {
            ref mut outputs, ref config, ref niri_state, ref stats,
            ref windows_ordered, ref conn, ref shm, ..
        } = *self;
        for out in outputs.iter_mut() {
            if out.top_configured {
                draw_top_on(out, config, niri_state, stats, windows_ordered, conn, shm);
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

    fn redraw_output_top(&mut self, idx: usize) {
        let VitoBar {
            ref mut outputs, ref config, ref niri_state, ref stats,
            ref windows_ordered, ref conn, ref shm, ..
        } = *self;
        if let Some(out) = outputs.get_mut(idx) {
            if out.top_configured {
                draw_top_on(out, config, niri_state, stats, windows_ordered, conn, shm);
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
}

// ── Trait implementations required by SCTK ──────────────────────────────────

impl CompositorHandler for VitoBar {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>,
                            surface: &wl_surface::WlSurface, factor: i32) {
        for out in &mut self.outputs {
            let is_mine = out.top_surface.as_ref().map(|s| s.wl_surface() == surface).unwrap_or(false)
                || out.bot_surface.as_ref().map(|s| s.wl_surface() == surface).unwrap_or(false);
            if is_mine {
                out.scale = factor.max(1) as u32;
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
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.running = false;
    }

    fn configure(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>,
                 surface: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
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
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>,
                         _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() { p.release(); }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for VitoBar {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>,
                     _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        use PointerEventKind::*;
        for event in events {
            for out in &mut self.outputs {
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
                    _ => {}
                }
                break;
            }
        }
    }
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
delegate_pointer!(VitoBar);
delegate_registry!(VitoBar);

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
        niri_state,
        windows_ordered,
        seat_state,
        pointer:     None,
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

    // ── Sysinfo + clock: redraw tops every 5 s ─────────────────────────
    event_loop.handle()
        .insert_source(
            Timer::from_duration(Duration::from_secs(5)),
            |_, _, app: &mut VitoBar| {
                let window_app_ids: Vec<String> = app.windows_ordered.iter()
                    .filter_map(|w| w.app_id.clone())
                    .collect();
                app.stats = app.monitor.refresh();
                app.stats.background_apps = app.monitor.detect_background_apps(&window_app_ids);
                app.redraw_all_tops();
                TimeoutAction::ToDuration(Duration::from_secs(5))
            },
        )
        .expect("sysinfo timer");

    while app.running {
        event_loop.dispatch(None, &mut app).expect("dispatch failed");
    }
}
