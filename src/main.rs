mod config;
mod render;
mod modules;
mod icons;

use config::Config;
use modules::{clock::get_time_string, sysinfo::{SysMonitor, SysStats}};
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
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::wlr_layer::{
        Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
        LayerSurfaceConfigure,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use calloop::{EventLoop, channel::{channel as calloop_channel, Event as ChannelEvent}, timer::{Timer, TimeoutAction}};
use calloop_wayland_source::WaylandSource;
use std::time::Duration;

const BAR_HEIGHT:     u32 = 22;
const TASKBAR_HEIGHT: u32 = 22;


struct VitoBar {
    registry_state: RegistryState,
    output_state:   OutputState,
    compositor:     CompositorState,
    layer_shell:    LayerShell,
    shm:            Shm,

    // Top bar
    top_surface:    Option<LayerSurface>,
    top_pool:       Option<SlotPool>,

    // Bottom taskbar
    bot_surface:    Option<LayerSurface>,
    bot_pool:       Option<SlotPool>,

    conn:           Connection,
    qh:             QueueHandle<Self>,
    width:          u32,
    scale:          u32,   // output scale factor (1 = normal, 2 = HiDPI)
    config:         Config,
    monitor:        SysMonitor,
    stats:          SysStats,   // cached — refreshed by timer, not per-draw
    running:        bool,
    top_configured: bool,
    bot_configured: bool,

    // Live niri state — updated via EventStream, zero IPC calls per draw
    niri_state:      EventStreamState,
    // Windows in niri's visual order — kept as a Vec so we never lose the sequence.
    // WorkspacesChanged gives the full ordered list; incremental events patch it in-place.
    windows_ordered: Vec<niri_ipc::Window>,
}

/// Sort windows into L→R display order: by workspace index, then by column, then by tile within
/// column. Uses `layout.pos_in_scrolling_layout` (column, tile), which niri 25.11+ provides.
/// Windows without layout info (e.g. floating) sort to the end of their workspace.
fn sort_windows_by_position(windows: &mut Vec<niri_ipc::Window>, ws_map: &HashMap<u64, u8>) {
    windows.sort_by_key(|w| {
        let ws_idx = w.workspace_id.and_then(|id| ws_map.get(&id)).copied().unwrap_or(u8::MAX);
        let (col, tile) = w.layout.pos_in_scrolling_layout.unwrap_or((usize::MAX, usize::MAX));
        (ws_idx, col, tile)
    });
}

impl VitoBar {
    fn draw_top(&mut self, qh: &QueueHandle<Self>) {
        let width  = self.width;
        let height = BAR_HEIGHT;
        let scale  = self.scale;
        // Physical pixel dimensions — rendered at full resolution, compositor scales down.
        let pw = width  * scale;
        let ph = height * scale;
        let sf = scale as f32;

        let pool = match self.top_pool.as_mut() {
            Some(p) => p,
            None    => return,
        };
        let surface = match self.top_surface.as_ref() {
            Some(s) => s,
            None    => return,
        };

        let stride = pw as i32 * 4;
        let (buffer, canvas) = pool
            .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
            .expect("failed to create buffer");

        let mut r = Renderer::new(pw, ph);
        r.clear(&self.config.colors.base00);

        // All logical coordinates × sf → physical pixels.
        let fsz    = self.config.font_size.unwrap_or(11.0) * sf;
        let pad    = 2.0 * sf;
        let bh     = 18.0 * sf; // box height inside bar
        let text_y = pad + bh * 0.75; // baseline

        // ── Workspaces (from cached EventStream state, no IPC per frame) ───
        let mut workspaces: Vec<&niri_ipc::Workspace> =
            self.niri_state.workspaces.workspaces.values().collect();
        workspaces.sort_by_key(|w| w.idx);

        let bsz = 18.0 * sf;
        let mut x = 4.0 * sf;
        for ws in &workspaces {
            let has_windows = ws.active_window_id.is_some();
            let fill = if ws.is_active {
                &self.config.colors.base0d
            } else if has_windows {
                &self.config.colors.base02
            } else {
                &self.config.colors.base01
            };
            r.draw_rect(x, pad, bsz, bsz, fill);
            r.draw_rect_outline(x, pad, bsz, bsz, &self.config.colors.base02.clone(), 1.5 * sf);

            let text_color = if ws.is_active {
                &self.config.colors.base00
            } else if has_windows {
                &self.config.colors.base05
            } else {
                &self.config.colors.base03
            };
            let num = ws.idx.to_string();
            let tw  = r.measure_text(&num, fsz);
            r.draw_text(&num, x + (bsz - tw) / 2.0, text_y, fsz, text_color);

            x += 20.0 * sf;
        }

        // ── Center: launcher (NixOS ) + settings () ─────────────────────
        let cx = pw as f32 / 2.0;
        // Launcher block
        let launch_label = "\u{f303}";  // nf-linux-nixos
        let lw = 34.0 * sf;
        let lx = cx - 36.0 * sf;
        r.draw_rect(lx, pad, lw, bh, &self.config.colors.base01);
        r.draw_rect_outline(lx, pad, lw, bh, &self.config.colors.base02.clone(), 1.5 * sf);
        let ltw = r.measure_text(launch_label, fsz);
        r.draw_text(launch_label, lx + (lw - ltw) / 2.0, text_y, fsz, &self.config.colors.base0d);
        // Settings block
        let cfg_label = "\u{f013}";     // nf-fa-cog ⚙
        let sw = 20.0 * sf;
        let sx = cx + 2.0 * sf;
        r.draw_rect(sx, pad, sw, bh, &self.config.colors.base01);
        r.draw_rect_outline(sx, pad, sw, bh, &self.config.colors.base02.clone(), 1.5 * sf);
        let stw = r.measure_text(cfg_label, fsz);
        r.draw_text(cfg_label, sx + (sw - stw) / 2.0, text_y, fsz, &self.config.colors.base03);

        // ── Status blocks (right-to-left) ────────────────────────────────────
        let stats = &self.stats;
        let time  = get_time_string();
        let gap   = 4.0 * sf;
        let mut rx = pw as f32 - 4.0 * sf;

        macro_rules! status_block {
            ($w:expr, $text:expr, $color:expr) => {{
                let bw = $w * sf;
                rx -= bw;
                r.draw_rect(rx, pad, bw, bh, &self.config.colors.base01);
                r.draw_rect_outline(rx, pad, bw, bh, &self.config.colors.base02.clone(), 1.5 * sf);
                r.draw_text($text, rx + 4.0 * sf, text_y, fsz, $color);
                rx -= gap;
            }};
        }

        // Right-to-left: clock → vol → bat → brightness → cpu → ram
        // Icons: fa-clock-o  fa-volume-up  fa-battery-full  fa-sun-o  fa-bolt  fa-database
        status_block!(148.0, &format!("\u{f017} {}", time),                       &self.config.colors.base07);
        status_block!( 62.0, &format!("\u{f028} {:>3}%",  stats.volume_pct),      &self.config.colors.base0c);
        if let Some(bat) = stats.battery_pct {
            status_block!(62.0, &format!("\u{f240} {:>3.0}%", bat),               &self.config.colors.base0b);
        }
        status_block!( 62.0, &format!("\u{f185} {:>3}%",  stats.brightness_pct),  &self.config.colors.base0a);
        status_block!( 62.0, &format!("\u{f0e7} {:>3.0}%", stats.cpu_pct),        &self.config.colors.base09);
        status_block!( 70.0, &format!("\u{f1c0} {:.1}G",  stats.ram_gb),          &self.config.colors.base0d);
        let _ = rx;

        // ── Flush ────────────────────────────────────────────────────────────
        let bgra = r.as_bgra();
        let len = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        surface.wl_surface().set_buffer_scale(scale as i32);
        surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
        buffer.attach_to(surface.wl_surface()).expect("buffer attach");
        surface.commit();
        self.conn.flush().ok();
    }

    fn draw_bot(&mut self, qh: &QueueHandle<Self>) {
        let width  = self.width;
        let height = TASKBAR_HEIGHT;

        let pool = match self.bot_pool.as_mut() {
            Some(p) => p,
            None    => return,
        };
        let surface = match self.bot_surface.as_ref() {
            Some(s) => s,
            None    => return,
        };

        let scale = self.scale;
        let pw    = width  * scale;
        let ph    = height * scale;
        let sf    = scale as f32;

        let stride = pw as i32 * 4;
        let (buffer, canvas) = pool
            .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
            .expect("failed to create buffer");

        let mut r = Renderer::new(pw, ph);
        r.clear(&self.config.colors.base00);

        let fsz       = self.config.font_size.unwrap_or(11.0) * sf;
        let pad       = 2.0 * sf;
        let bh        = 18.0 * sf;
        let text_y    = pad + bh * 0.75;
        let badge_fsz = (fsz - sf).max(8.0 * sf);

        // ── Windows in niri's visual order, grouped by workspace ────────────────
        let ws_map: HashMap<u64, u8> = self.niri_state.workspaces.workspaces.values()
            .map(|w| (w.id, w.idx)).collect();

        // windows_ordered is already normalized to L→R visual order per workspace
        // (sorted + reversed at storage time in the WindowsChanged handler).
        // A stable sort by workspace idx here groups workspaces correctly without
        // disturbing the within-workspace order.
        let mut windows: Vec<&niri_ipc::Window> = self.windows_ordered.iter().collect();
        windows.sort_by_key(|w| {
            w.workspace_id.and_then(|id| ws_map.get(&id)).copied().unwrap_or(255)
        });

        let mut tx = 4.0 * sf;
        for win in &windows {
            let block_w = 160.0 * sf;
            if tx + block_w > pw as f32 - 4.0 * sf { break; }

            let (fill, text_col) = if win.is_focused {
                (&self.config.colors.base02, &self.config.colors.base07)
            } else {
                (&self.config.colors.base01, &self.config.colors.base05)
            };
            let outline_col = if win.is_focused {
                &self.config.colors.base0d
            } else {
                &self.config.colors.base02
            };
            r.draw_rect(tx, pad, block_w, bh, fill);
            r.draw_rect_outline(tx, pad, block_w, bh, &outline_col.clone(), 1.5 * sf);

            // Workspace badge
            let bdg_x = tx + 3.0 * sf;
            let bdg_y = 4.0 * sf;
            let bdg_w = 13.0 * sf;
            let bdg_h = 12.0 * sf;
            r.draw_rect(bdg_x, bdg_y, bdg_w, bdg_h, &self.config.colors.base00);
            r.draw_rect_outline(bdg_x, bdg_y, bdg_w, bdg_h, &self.config.colors.base03.clone(), 1.0 * sf);
            let ws_idx = win.workspace_id.and_then(|id| ws_map.get(&id)).copied().unwrap_or(0);
            let ws_str = ws_idx.to_string();
            let ws_tw  = r.measure_text(&ws_str, badge_fsz);
            r.draw_text(&ws_str, bdg_x + (bdg_w - ws_tw) / 2.0,
                        bdg_y + bdg_h * 0.75, badge_fsz, &self.config.colors.base04);

            // ── App icon ────────────────────────────────────────────────────
            let app_id       = win.app_id.as_deref().unwrap_or("unknown");
            let icon_phys    = (14.0 * sf) as u32;
            let icon_x_phys  = (tx + 19.0 * sf) as u32;
            let icon_y_phys  = (pad + (bh - 14.0 * sf) / 2.0) as u32;
            let icon_logical = 14.0 * sf;  // fixed advance width for either render path

            if let Some(rgba) = icons::load(app_id, icon_phys) {
                r.draw_icon(icon_x_phys, icon_y_phys, icon_phys, &rgba);
            } else {
                // Nerd Font fallback
                let glyph = app_icon(app_id);
                let glyph_col = if win.is_focused {
                    &self.config.colors.base0d
                } else {
                    &self.config.colors.base03
                };
                r.draw_text(glyph, tx + 19.0 * sf, text_y, fsz, &glyph_col.clone());
            }

            // ── Label: app_id - title (clipped, no "…") ─────────────────────
            let title  = win.title.as_deref().unwrap_or("");
            let label  = if title.is_empty() || title.eq_ignore_ascii_case(app_id) {
                app_id.to_string()
            } else {
                format!("{} - {}", app_id, title)
            };
            let label_x = tx + 19.0 * sf + icon_logical + 3.0 * sf;
            let avail_w = (block_w - (label_x - tx) - 4.0 * sf).max(0.0);
            let clipped = r.clip_text(&label, avail_w, fsz);
            r.draw_text(&clipped, label_x, text_y, fsz, &text_col.clone());

            tx += block_w + 4.0 * sf;
        }

        let bgra = r.as_bgra();
        let len = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        surface.wl_surface().set_buffer_scale(scale as i32);
        surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
        buffer.attach_to(surface.wl_surface()).expect("buffer attach");
        surface.commit();
        self.conn.flush().ok();
    }
}

// ── Trait implementations required by SCTK ──────────────────────────────────

impl CompositorHandler for VitoBar {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        self.scale = factor.max(1) as u32;
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for VitoBar {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for VitoBar {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.running = false;
    }

fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
    if configure.new_size.0 > 0 {
        self.width = configure.new_size.0;
    }

    let is_top = self.top_surface.as_ref().map(|s| s.wl_surface() == surface.wl_surface()).unwrap_or(false);
    let is_bot = self.bot_surface.as_ref().map(|s| s.wl_surface() == surface.wl_surface()).unwrap_or(false);

    if is_top && !self.top_configured {
        self.top_configured = true;
        if self.top_pool.is_none() {
            // Allocate 4× base size to cover up to 2× HiDPI scale (scale² = 4).
            let size = self.width as usize * BAR_HEIGHT as usize * 4 * 4;
            self.top_pool = Some(SlotPool::new(size, &self.shm).expect("top pool"));
        }
        self.draw_top(qh);
    }

    if is_bot && !self.bot_configured {
        self.bot_configured = true;
        if self.bot_pool.is_none() {
            let size = self.width as usize * TASKBAR_HEIGHT as usize * 4 * 4;
            self.bot_pool = Some(SlotPool::new(size, &self.shm).expect("bot pool"));
        }
        self.draw_bot(qh);
    }
}
}

impl ShmHandler for VitoBar {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for VitoBar {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState];
}

delegate_compositor!(VitoBar);
delegate_output!(VitoBar);
delegate_layer!(VitoBar);
delegate_shm!(VitoBar);
delegate_registry!(VitoBar);

fn app_icon(app_id: &str) -> &'static str {
    let s = app_id.to_ascii_lowercase();
    let s = s.as_str();
    if s.contains("firefox") || s.contains("librewolf") { "\u{f269}" }        // nf-fa-firefox
    else if s.contains("chromium") || s.contains("chrome") || s.contains("brave") { "\u{f268}" } // nf-fa-chrome
    else if s.contains("foot") || s.contains("alacritty") || s.contains("kitty") ||
            s.contains("wezterm") || s.contains("urxvt") || s.contains("xterm") { "\u{f120}" }   // nf-fa-terminal
    else if s.contains("nvim") || s.contains("neovim") { "\u{e62b}" }         // nf-dev-vim
    else if s.contains("vim") { "\u{e62b}" }
    else if s.contains("emacs") { "\u{e632}" }                                 // nf-dev-gnu_emacs
    else if s.contains("code") || s.contains("vscode") { "\u{e70c}" }         // nf-dev-visualstudio
    else if s.contains("discord") { "\u{f392}" }                              // nf-fab-discord
    else if s.contains("telegram") || s.contains("tdesktop") { "\u{f2c6}" }  // nf-fa-telegram
    else if s.contains("slack") { "\u{f198}" }                                // nf-fa-slack
    else if s.contains("spotify") { "\u{f1bc}" }                              // nf-fa-spotify
    else if s.contains("mpv") || s.contains("vlc") { "\u{f03d}" }             // nf-fa-film
    else if s.contains("gimp") || s.contains("inkscape") { "\u{f1fc}" }       // nf-fa-paint-brush
    else if s.contains("thunar") || s.contains("nautilus") ||
            s.contains("dolphin") || s.contains("nemo") { "\u{f07b}" }        // nf-fa-folder
    else if s.contains("obsidian") { "\u{f1d8}" }                             // nf-fa-diamond
    else if s.contains("steam") { "\u{f1b6}" }                                // nf-fa-steam
    else { "\u{f2d0}" }                                                        // nf-fa-window-maximize
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

    // ── Top bar surface ──
    let top_wl_surface = compositor.create_surface(&qh);
    let top_surface    = layer_shell.create_layer_surface(
        &qh, top_wl_surface, Layer::Top, Some("vitobar-top"), None,
    );
    top_surface.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    top_surface.set_size(0, BAR_HEIGHT);
    top_surface.set_exclusive_zone(BAR_HEIGHT as i32);
    top_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    top_surface.commit();

    // ── Bottom taskbar surface ──
    let bot_wl_surface = compositor.create_surface(&qh);
    let bot_surface    = layer_shell.create_layer_surface(
        &qh, bot_wl_surface, Layer::Top, Some("vitobar-bot"), None,
    );
    bot_surface.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    bot_surface.set_size(0, TASKBAR_HEIGHT);
    bot_surface.set_exclusive_zone(TASKBAR_HEIGHT as i32);
    bot_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    bot_surface.commit();

    let output_state = OutputState::new(&globals, &qh);

    // ── Pre-populate niri state so first draw has real data ─────────────────
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

    let mut app = VitoBar {
        registry_state: RegistryState::new(&globals),
        output_state,
        compositor,
        layer_shell,
        shm,
        top_surface: Some(top_surface),
        top_pool:    None,
        bot_surface: Some(bot_surface),
        bot_pool:    None,
        conn:        conn.clone(),
        qh:          qh.clone(),
        width:       1920,
        scale:       1,
        config,
        monitor,
        stats:       initial_stats,
        running:     true,
        top_configured: false,
        bot_configured: false,
        niri_state,
        windows_ordered,
    };

    // ── Event loop ───────────────────────────────────────────────────────────
    let mut event_loop: EventLoop<VitoBar> = EventLoop::try_new().expect("event loop");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    // ── Niri EventStream — background thread pushes events, zero polling ─────
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
                                // Unknown/future niri event type — skip it, keep reading.
                                log::debug!("niri: skipping unknown event: {e}");
                            }
                            Err(e) => {
                                // Real socket error — reconnect.
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

            // Keep windows_ordered in sync in L→R visual order.
            // WindowsChanged gives the authoritative list; WindowLayoutsChanged fires when columns
            // are moved left/right and carries updated pos_in_scrolling_layout for each window.
            match &event {
                NiriEvent::WindowsChanged { windows } => {
                    // ws_map must be built before niri_state.apply() mutates workspaces
                    let ws_map: HashMap<u64, u8> = app.niri_state.workspaces.workspaces.values()
                        .map(|w| (w.id, w.idx)).collect();
                    let mut ordered = windows.clone();
                    sort_windows_by_position(&mut ordered, &ws_map);
                    app.windows_ordered = ordered;
                }
                NiriEvent::WindowLayoutsChanged { changes } => {
                    // Update each window's layout, then re-sort to reflect the new column order.
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
                    // Ensure at most one window is focused at a time
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
                NiriEvent::WorkspaceActiveWindowChanged { .. }
            );
            let redraw_bot = matches!(event,
                NiriEvent::WindowsChanged { .. }           |
                NiriEvent::WindowLayoutsChanged { .. }     |
                NiriEvent::WindowOpenedOrChanged { .. }    |
                NiriEvent::WindowClosed { .. }             |
                NiriEvent::WindowFocusChanged { .. }
            );
            app.niri_state.apply(event);
            let qh = app.qh.clone();
            if redraw_top { app.draw_top(&qh); }
            if redraw_bot { app.draw_bot(&qh); }
        })
        .expect("niri channel");

    // ── Sysinfo + clock: redraw top every 5 s ───────────────────────────────
    // Clock shows HH:MM so 5 s granularity is imperceptible.
    // CPU/battery/brightness/volume change slowly; 5 s is plenty.
    event_loop.handle()
        .insert_source(
            Timer::from_duration(Duration::from_secs(5)),
            |_, _, app: &mut VitoBar| {
                app.stats = app.monitor.refresh();
                let qh = app.qh.clone();
                app.draw_top(&qh);
                TimeoutAction::ToDuration(Duration::from_secs(5))
            },
        )
        .expect("sysinfo timer");

    while app.running {
        event_loop.dispatch(None, &mut app).expect("dispatch failed");
    }
}
