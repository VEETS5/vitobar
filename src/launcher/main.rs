#[path = "../render.rs"]   mod render;
#[path = "../config.rs"]   mod config;
#[path = "../icons.rs"]    mod icons;
mod desktop;

use config::Config;
use render::Renderer;
use desktop::{DesktopEntry, clean_exec, load_all};

use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RepeatInfo},
        pointer::{BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
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
use calloop::{EventLoop};
use calloop_wayland_source::WaylandSource;

const WIN_W: u32 = 600;
const WIN_H: u32 = 400;
const ROW_H: f32 = 28.0;
const SEARCH_H: f32 = 36.0;
const PADDING: f32 = 8.0;

struct LauncherApp {
    registry_state: RegistryState,
    output_state:   OutputState,
    compositor:     CompositorState,
    layer_shell:    LayerShell,
    shm:            Shm,
    seat_state:     SeatState,
    surface:        Option<LayerSurface>,
    pool:           Option<SlotPool>,
    conn:           Connection,
    qh:             QueueHandle<Self>,
    scale:          u32,
    width:          u32,
    height:         u32,
    configured:     bool,
    running:        bool,
    pointer:        Option<wl_pointer::WlPointer>,
    keyboard:       Option<wl_keyboard::WlKeyboard>,
    pointer_pos:    (f64, f64),

    config:         Config,
    all_entries:    Vec<DesktopEntry>,
    query:          String,
    filtered:       Vec<usize>,   // indices into all_entries
    selected:       usize,
    scroll_offset:  usize,
}

impl LauncherApp {
    fn update_filter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self.all_entries.iter().enumerate()
            .filter(|(_, e)| {
                e.name.to_lowercase().contains(&q) ||
                e.app_id.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    fn visible_rows(&self) -> usize {
        let area_h = WIN_H as f32 - SEARCH_H - PADDING * 2.0;
        (area_h / ROW_H) as usize
    }

    fn clamp_scroll(&mut self) {
        let vis = self.visible_rows();
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + vis && vis > 0 {
            self.scroll_offset = self.selected + 1 - vis;
        }
    }

    fn launch_selected(&mut self) {
        if let Some(&idx) = self.filtered.get(self.selected) {
            let entry = &self.all_entries[idx];
            let (cmd, args) = clean_exec(&entry.exec);
            std::process::Command::new(&cmd).args(&args).spawn().ok();
        }
        self.running = false;
    }

    fn draw(&mut self) {
        let scale  = self.scale;
        let pw     = self.width  * scale;
        let ph     = self.height * scale;
        let sf     = scale as f32;

        let pool = match self.pool.as_mut() { Some(p) => p, None => return };
        let surface = match self.surface.as_ref() { Some(s) => s, None => return };

        let stride = pw as i32 * 4;
        let (buffer, canvas) = pool
            .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        let mut r = Renderer::new(pw, ph);
        r.clear(&self.config.colors.base00);

        // Window border
        r.draw_rect_outline(0.0, 0.0, pw as f32, ph as f32,
            &self.config.colors.base0d, 2.0 * sf);

        let fsz    = self.config.font_size.unwrap_or(11.0) * sf;
        let pad    = PADDING * sf;
        let sh     = SEARCH_H * sf;
        let rh     = ROW_H * sf;
        let w      = self.width as f32 * sf;

        // ── Search box ───────────────────────────────────────────────────────
        r.draw_rect(pad, pad, w - pad * 2.0, sh - pad, &self.config.colors.base01);
        let outline_col = if self.query.is_empty() {
            self.config.colors.base02.clone()
        } else {
            self.config.colors.base0d.clone()
        };
        r.draw_rect_outline(pad, pad, w - pad * 2.0, sh - pad, &outline_col, 1.5 * sf);

        let search_icon = "\u{f002}"; // nf-fa-search
        let icon_x = pad + 4.0 * sf;
        let text_baseline = pad + (sh - pad) * 0.72;
        r.draw_text(search_icon, icon_x, text_baseline, fsz, &self.config.colors.base03);

        let query_display = if self.query.is_empty() {
            "Search applications…".to_string()
        } else {
            self.query.clone()
        };
        let query_col = if self.query.is_empty() {
            self.config.colors.base03.clone()
        } else {
            self.config.colors.base05.clone()
        };
        let icon_w = r.measure_text(search_icon, fsz) + 6.0 * sf;
        r.draw_text(&query_display, icon_x + icon_w, text_baseline, fsz, &query_col);

        // ── App list ─────────────────────────────────────────────────────────
        let list_top = sh + pad;
        let vis = {
            let area_h = WIN_H as f32 - SEARCH_H - PADDING * 2.0;
            (area_h / ROW_H) as usize
        };

        for (view_idx, &entry_idx) in self.filtered.iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(vis)
        {
            let display_idx = view_idx - self.scroll_offset;
            let ry = list_top + display_idx as f32 * rh;
            let entry = &self.all_entries[entry_idx];
            let is_selected = view_idx == self.selected;

            if is_selected {
                r.draw_rect(pad, ry, w - pad * 2.0, rh - 2.0 * sf, &self.config.colors.base02);
                r.draw_rect_outline(pad, ry, w - pad * 2.0, rh - 2.0 * sf,
                    &self.config.colors.base0d, 1.5 * sf);
            }

            // Icon
            let icon_size = (16.0 * sf) as u32;
            let icon_x_phys  = (pad + 4.0 * sf) as u32;
            let icon_y_phys  = (ry + (rh - 16.0 * sf) / 2.0) as u32;
            let text_x;
            if let Some(rgba) = icons::load(&entry.app_id, icon_size) {
                r.draw_icon(icon_x_phys, icon_y_phys, icon_size, &rgba);
                text_x = pad + 4.0 * sf + 16.0 * sf + 6.0 * sf;
            } else {
                text_x = pad + 8.0 * sf;
            }

            let text_col = if is_selected {
                self.config.colors.base07.clone()
            } else {
                self.config.colors.base05.clone()
            };
            let text_y = ry + rh * 0.70;
            let avail = w - text_x - pad;
            let clipped = r.clip_text(&entry.name, avail, fsz);
            r.draw_text(&clipped, text_x, text_y, fsz, &text_col);
        }

        // ── Empty state ───────────────────────────────────────────────────────
        if self.filtered.is_empty() {
            let msg = if self.query.is_empty() { "No applications found" } else { "No results" };
            let mw = r.measure_text(msg, fsz);
            let mx = (w - mw) / 2.0;
            let my = list_top + 40.0 * sf;
            r.draw_text(msg, mx, my, fsz, &self.config.colors.base03);
        }

        // ── Flush ─────────────────────────────────────────────────────────────
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

// ── SCTK trait implementations ───────────────────────────────────────────────

impl CompositorHandler for LauncherApp {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>,
                            _: &wl_surface::WlSurface, factor: i32) {
        self.scale = factor.max(1) as u32;
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>,
                         _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>,
             _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>,
                     _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>,
                     _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for LauncherApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for LauncherApp {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.running = false;
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>,
                 _surface: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        if configure.new_size.0 > 0 { self.width  = configure.new_size.0; }
        if configure.new_size.1 > 0 { self.height = configure.new_size.1; }

        if !self.configured {
            self.configured = true;
            let size = self.width as usize * self.height as usize * 4 * 4;
            self.pool = Some(SlotPool::new(size, &self.shm).expect("pool"));
        }
        self.draw();
        let _ = qh;
    }
}

impl ShmHandler for LauncherApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl SeatHandler for LauncherApp {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>,
                      seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = Some(self.seat_state.get_keyboard(qh, &seat, None)
                .expect("keyboard"));
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = Some(self.seat_state.get_pointer(qh, &seat)
                .expect("pointer"));
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>,
                         _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() { k.release(); }
        }
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() { p.release(); }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for LauncherApp {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32) {
        self.running = false;
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
                   _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
                        _: u32, _: Modifiers, _: u32) {}
    fn update_repeat_info(&mut self, _: &Connection, _: &QueueHandle<Self>,
                          _: &wl_keyboard::WlKeyboard, _: RepeatInfo) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>,
                 _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        match event.keysym {
            Keysym::Escape => {
                self.running = false;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.launch_selected();
            }
            Keysym::Up | Keysym::KP_Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.clamp_scroll();
                    self.draw();
                }
            }
            Keysym::Down | Keysym::KP_Down => {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                    self.clamp_scroll();
                    self.draw();
                }
            }
            Keysym::BackSpace => {
                self.query.pop();
                self.update_filter();
                self.draw();
            }
            _ => {
                if let Some(s) = event.utf8 {
                    let ch: Vec<char> = s.chars().filter(|c| !c.is_control()).collect();
                    if !ch.is_empty() {
                        self.query.extend(ch);
                        self.update_filter();
                        self.draw();
                    }
                }
            }
        }
    }
}

impl PointerHandler for LauncherApp {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>,
                     _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        use PointerEventKind::*;
        for event in events {
            match &event.kind {
                Motion { .. } => {
                    self.pointer_pos = event.position;
                }
                Leave { .. } => {
                    self.running = false;
                }
                Press { button, .. } if *button == BTN_LEFT => {
                    let (lx, ly) = (event.position.0 as f32, event.position.1 as f32);
                    let search_h = SEARCH_H + PADDING;
                    let row_h = ROW_H;
                    if ly >= search_h {
                        let row = ((ly - search_h) / row_h) as usize + self.scroll_offset;
                        if row < self.filtered.len() {
                            self.selected = row;
                            self.launch_selected();
                        }
                    }
                    let _ = lx;
                }
                _ => {}
            }
        }
    }
}

impl ProvidesRegistryState for LauncherApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(LauncherApp);
delegate_output!(LauncherApp);
delegate_layer!(LauncherApp);
delegate_shm!(LauncherApp);
delegate_seat!(LauncherApp);
delegate_keyboard!(LauncherApp);
delegate_pointer!(LauncherApp);
delegate_registry!(LauncherApp);

fn find_font() -> String {
    let out = std::process::Command::new("fc-match")
        .args(["JetBrainsMono Nerd Font Mono", "--format=%{file}"])
        .output()
        .expect("fc-match failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn main() {
    env_logger::init();

    let font_path = find_font();
    render::load_font(&font_path);

    let config = Config::load();
    let all_entries = load_all();
    let filtered: Vec<usize> = (0..all_entries.len()).collect();

    let conn = Connection::connect_to_env().expect("wayland connect");
    let (globals, queue) = registry_queue_init::<LauncherApp>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor  = CompositorState::bind(&globals, &qh).expect("compositor");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell");
    let shm         = Shm::bind(&globals, &qh).expect("shm");
    let seat_state  = SeatState::new(&globals, &qh);
    let output_state = OutputState::new(&globals, &qh);

    let wl_surface = compositor.create_surface(&qh);
    let surface = layer_shell.create_layer_surface(
        &qh, wl_surface, Layer::Overlay, Some("vitolauncher"), None,
    );
    surface.set_size(WIN_W, WIN_H);
    surface.set_anchor(Anchor::empty());   // centered
    surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();

    let mut app = LauncherApp {
        registry_state: RegistryState::new(&globals),
        output_state,
        compositor,
        layer_shell,
        shm,
        seat_state,
        surface: Some(surface),
        pool:    None,
        conn:    conn.clone(),
        qh:      qh.clone(),
        scale:   1,
        width:   WIN_W,
        height:  WIN_H,
        configured: false,
        running:    true,
        pointer:    None,
        keyboard:   None,
        pointer_pos: (0.0, 0.0),
        config,
        all_entries,
        query:        String::new(),
        filtered,
        selected:     0,
        scroll_offset: 0,
    };

    let mut event_loop: EventLoop<LauncherApp> = EventLoop::try_new().expect("event loop");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    while app.running {
        event_loop.dispatch(None, &mut app).expect("dispatch");
    }
}
