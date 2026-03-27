mod config;
mod render;
mod modules;

use config::Config;
use modules::{
    clock::get_time_string,
    sysinfo::SysMonitor,
    windows::get_windows,
    workspaces::get_workspaces,
};
use render::Renderer;

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

    width:          u32,
    scale:          u32,   // output scale factor (1 = normal, 2 = HiDPI)
    config:         Config,
    monitor:        SysMonitor,
    running:        bool,
    top_configured: bool,
    bot_configured: bool,
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

        // ── Workspaces ───────────────────────────────────────────────────────
        let workspaces = get_workspaces();
        let bsz = 18.0 * sf; // workspace box size
        let mut x = 4.0 * sf;
        for ws in &workspaces {
            let fill = if ws.is_active {
                &self.config.colors.base0d
            } else if ws.has_windows {
                &self.config.colors.base02
            } else {
                &self.config.colors.base01
            };
            r.draw_rect(x, pad, bsz, bsz, fill);
            r.draw_rect_outline(x, pad, bsz, bsz, &self.config.colors.base02.clone(), 1.5 * sf);

            let text_color = if ws.is_active {
                &self.config.colors.base00
            } else if ws.has_windows {
                &self.config.colors.base05
            } else {
                &self.config.colors.base03
            };
            let num = ws.idx.to_string();
            let tw  = r.measure_text(&num, fsz);
            r.draw_text(&num, x + (bsz - tw) / 2.0, text_y, fsz, text_color);

            x += 20.0 * sf;
        }

        // ── Center: launcher + settings ──────────────────────────────────────
        let cx = pw as f32 / 2.0;
        r.draw_rect(cx - 36.0 * sf, pad, 34.0 * sf, bh, &self.config.colors.base01);
        r.draw_rect_outline(cx - 36.0 * sf, pad, 34.0 * sf, bh, &self.config.colors.base02.clone(), 1.5 * sf);
        r.draw_rect(cx + 2.0 * sf, pad, 20.0 * sf, bh, &self.config.colors.base01);
        r.draw_rect_outline(cx + 2.0 * sf, pad, 20.0 * sf, bh, &self.config.colors.base02.clone(), 1.5 * sf);
        r.draw_text("/|\\^", cx - 32.0 * sf, text_y, fsz, &self.config.colors.base05);
        r.draw_text("*",    cx +  6.0 * sf,  text_y, fsz, &self.config.colors.base05);

        // ── Status blocks (right-to-left) ────────────────────────────────────
        let stats = self.monitor.refresh();
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

        status_block!(150.0, &time,                                      &self.config.colors.base07);
        status_block!( 54.0, &format!("V:{:>3}%", stats.volume_pct),    &self.config.colors.base0c);
        if let Some(bat) = stats.battery_pct {
            status_block!(56.0, &format!("B:{:>4.0}%", bat),            &self.config.colors.base0b);
        }
        status_block!( 56.0, &format!("L:{:>3}%", stats.brightness_pct), &self.config.colors.base0a);
        status_block!( 62.0, &format!("C:{:>4.0}%", stats.cpu_pct),     &self.config.colors.base09);
        let _ = rx;

        // ── Flush ────────────────────────────────────────────────────────────
        let bgra = r.as_bgra();
        let len = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        surface.wl_surface().set_buffer_scale(scale as i32);
        surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
        buffer.attach_to(surface.wl_surface()).expect("buffer attach");
        surface.commit();
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

        let windows   = get_windows();
        let mut tx    = 4.0 * sf;
        for win in &windows {
            let block_w = 130.0 * sf;
            if tx + block_w > pw as f32 - 4.0 * sf { break; }

            let (fill, outline) = if win.is_focused {
                (&self.config.colors.base02, &self.config.colors.base0d)
            } else {
                (&self.config.colors.base01, &self.config.colors.base02)
            };
            r.draw_rect(tx, pad, block_w, bh, fill);
            r.draw_rect_outline(tx, pad, block_w, bh, &outline.clone(), 1.5 * sf);

            // Workspace badge
            let bdg_x = tx + 3.0 * sf;
            let bdg_y = 4.0 * sf;
            let bdg_w = 13.0 * sf;
            let bdg_h = 12.0 * sf;
            r.draw_rect(bdg_x, bdg_y, bdg_w, bdg_h, &self.config.colors.base00);
            r.draw_rect_outline(bdg_x, bdg_y, bdg_w, bdg_h, &self.config.colors.base03.clone(), 1.0 * sf);
            let ws_str = win.workspace_idx.to_string();
            let ws_tw  = r.measure_text(&ws_str, badge_fsz);
            r.draw_text(&ws_str, bdg_x + (bdg_w - ws_tw) / 2.0,
                        bdg_y + bdg_h * 0.75, badge_fsz, &self.config.colors.base04);

            // Icon placeholder
            r.draw_rect(tx + 19.0 * sf, 5.0 * sf, 12.0 * sf, 12.0 * sf, &self.config.colors.base0d);

            // App name — truncated to fit remaining block width
            let app = r.truncate_text(&win.app_id, block_w - 39.0 * sf, fsz);
            r.draw_text(&app, tx + 35.0 * sf, text_y, fsz, &self.config.colors.base05);

            tx += block_w + 4.0 * sf;
        }

        let bgra = r.as_bgra();
        let len = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        surface.wl_surface().set_buffer_scale(scale as i32);
        surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
        buffer.attach_to(surface.wl_surface()).expect("buffer attach");
        surface.commit();
    }
}

// ── Trait implementations required by SCTK ──────────────────────────────────

impl CompositorHandler for VitoBar {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        self.scale = factor.max(1) as u32;
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.draw_top(qh);
        self.draw_bot(qh);
    }
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
    let monitor = SysMonitor::new();

    let conn = Connection::connect_to_env().expect("failed to connect to Wayland");
    let (globals, mut queue) = registry_queue_init::<VitoBar>(&conn).expect("failed to init registry");
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
        width:       1920,
        scale:       1,
        config,
        monitor,
        running:     true,
        top_configured: false,
        bot_configured: false,
    };

    // ── Event loop ──
    while app.running {
        queue.blocking_dispatch(&mut app).expect("dispatch failed");
    }
}
