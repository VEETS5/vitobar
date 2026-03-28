#[path = "../render.rs"]   mod render;
#[path = "../config.rs"]   mod config;
#[path = "../icons.rs"]    mod icons;
mod categories;
mod widgets;

use config::Config;
use render::Renderer;
use categories::Category;
use widgets::{Widget, apply_widget_action, build_widgets};

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
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;

const WIN_W: u32 = 800;
const WIN_H: u32 = 560;
const SIDEBAR_W: f32 = 160.0;
const CAT_ROW_H: f32 = 40.0;
const WIDGET_ROW_H: f32 = 52.0;
const CONTENT_PAD: f32 = 16.0;

struct SettingsApp {
    registry_state:  RegistryState,
    output_state:    OutputState,
    compositor:      CompositorState,
    layer_shell:     LayerShell,
    shm:             Shm,
    seat_state:      SeatState,
    surface:         Option<LayerSurface>,
    pool:            Option<SlotPool>,
    conn:            Connection,
    qh:              QueueHandle<Self>,
    scale:           u32,
    width:           u32,
    height:          u32,
    configured:      bool,
    running:         bool,
    pointer:         Option<wl_pointer::WlPointer>,
    keyboard:        Option<wl_keyboard::WlKeyboard>,
    pointer_pos:     (f64, f64),
    drag_widget:     Option<usize>,   // index of widget being dragged (slider)

    config:          Config,
    active_category: Category,
    widgets:         Vec<Widget>,
}

impl SettingsApp {
    fn switch_category(&mut self, cat: Category) {
        self.active_category = cat;
        self.widgets = build_widgets(cat);
    }

    fn draw(&mut self) {
        let scale  = self.scale;
        let pw     = self.width  * scale;
        let ph     = self.height * scale;
        let sf     = scale as f32;

        let pool    = match self.pool.as_mut()    { Some(p) => p, None => return };
        let surface = match self.surface.as_ref() { Some(s) => s, None => return };

        let stride = pw as i32 * 4;
        let (buffer, canvas) = pool
            .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        let mut r = Renderer::new(pw, ph);
        r.clear(&self.config.colors.base00);

        let fsz = self.config.font_size.unwrap_or(11.0) * sf;

        // ── Sidebar ────────────────────────────────────────────────────────
        let sidebar_w = SIDEBAR_W * sf;
        r.draw_rect(0.0, 0.0, sidebar_w, ph as f32, &self.config.colors.base01);

        for (i, cat) in Category::ALL.iter().enumerate() {
            let ry = i as f32 * CAT_ROW_H * sf;
            let rh = CAT_ROW_H * sf;

            if *cat == self.active_category {
                r.draw_rect(0.0, ry, sidebar_w, rh, &self.config.colors.base02);
                // Left accent border
                r.draw_rect(0.0, ry, 3.0 * sf, rh, &self.config.colors.base0d);
            }

            let text_y = ry + rh * 0.65;
            let text_col = if *cat == self.active_category {
                self.config.colors.base05.clone()
            } else {
                self.config.colors.base04.clone()
            };
            r.draw_text(cat.label(), 10.0 * sf, text_y, fsz, &text_col);
        }

        // ── Content area ───────────────────────────────────────────────────
        let cx = sidebar_w + CONTENT_PAD * sf;
        let cw = pw as f32 - cx - CONTENT_PAD * sf;

        // Category title
        r.draw_text(
            self.active_category.label(),
            cx, 28.0 * sf,
            fsz * 1.3,
            &self.config.colors.base05,
        );

        // Divider
        let div_y = 38.0 * sf;
        r.draw_rect(cx, div_y, cw, 1.0 * sf, &self.config.colors.base02);

        let mut wy = div_y + 12.0 * sf;

        for widget in &self.widgets {
            let wh = WIDGET_ROW_H * sf;

            match widget {
                Widget::Slider { label, value, .. } => {
                    r.draw_text(label, cx, wy + fsz * 1.1, fsz, &self.config.colors.base04);

                    let track_x = cx + 120.0 * sf;
                    let track_w = cw - 120.0 * sf - 60.0 * sf;
                    let track_y = wy + wh * 0.5 - 3.0 * sf;
                    let track_h = 6.0 * sf;

                    // Track background
                    r.draw_rect(track_x, track_y, track_w, track_h, &self.config.colors.base02);
                    // Fill
                    r.draw_rect(track_x, track_y, track_w * value, track_h,
                        &self.config.colors.base0d);
                    // Value label
                    let pct_str = format!("{:.0}%", value * 100.0);
                    let pct_x = track_x + track_w + 8.0 * sf;
                    r.draw_text(&pct_str, pct_x, wy + wh * 0.65, fsz, &self.config.colors.base05);
                }

                Widget::Toggle { label, value, .. } => {
                    r.draw_text(label, cx, wy + fsz * 1.1, fsz, &self.config.colors.base04);

                    let toggle_x = cx + 120.0 * sf;
                    let toggle_y = wy + wh * 0.5 - 9.0 * sf;
                    let toggle_w = 36.0 * sf;
                    let toggle_h = 18.0 * sf;

                    let track_col = if *value {
                        self.config.colors.base0d.clone()
                    } else {
                        self.config.colors.base02.clone()
                    };
                    r.draw_rect(toggle_x, toggle_y, toggle_w, toggle_h, &track_col);
                    r.draw_rect_outline(toggle_x, toggle_y, toggle_w, toggle_h,
                        &self.config.colors.base03, 1.0 * sf);

                    let thumb_x = if *value {
                        toggle_x + toggle_w - toggle_h
                    } else {
                        toggle_x
                    };
                    let thumb_col = if *value {
                        self.config.colors.base00.clone()
                    } else {
                        self.config.colors.base04.clone()
                    };
                    r.draw_rect(thumb_x, toggle_y, toggle_h, toggle_h, &thumb_col);

                    let state_str = if *value { "On" } else { "Off" };
                    r.draw_text(state_str, toggle_x + toggle_w + 8.0 * sf,
                        wy + wh * 0.65, fsz, &self.config.colors.base05);
                }

                Widget::InfoRow { label, value } => {
                    r.draw_text(label, cx, wy + fsz * 1.1, fsz, &self.config.colors.base04);
                    let avail = cw - 120.0 * sf;
                    let clipped = r.clip_text(value, avail, fsz);
                    r.draw_text(&clipped, cx + 120.0 * sf, wy + fsz * 1.1, fsz,
                        &self.config.colors.base05);
                }

                Widget::Button { label, .. } => {
                    let btn_w = r.measure_text(label, fsz) + 24.0 * sf;
                    let btn_h = 28.0 * sf;
                    let btn_y = wy + (wh - btn_h) / 2.0;

                    r.draw_rect(cx, btn_y, btn_w, btn_h, &self.config.colors.base01);
                    r.draw_rect_outline(cx, btn_y, btn_w, btn_h,
                        &self.config.colors.base0d, 1.5 * sf);
                    r.draw_text(label, cx + 12.0 * sf, btn_y + btn_h * 0.72, fsz,
                        &self.config.colors.base0d);
                }
            }

            wy += wh;
        }

        // ── Flush ──────────────────────────────────────────────────────────
        let bgra = r.as_bgra();
        let len  = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        surface.wl_surface().set_buffer_scale(scale as i32);
        surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
        buffer.attach_to(surface.wl_surface()).expect("buffer attach");
        surface.commit();
        self.conn.flush().ok();
    }

    fn handle_click(&mut self, lx: f32, ly: f32) {
        let sidebar_w = SIDEBAR_W;

        if lx < sidebar_w {
            // Sidebar click — switch category
            let idx = (ly / CAT_ROW_H) as usize;
            if let Some(&cat) = Category::ALL.get(idx) {
                self.switch_category(cat);
                self.draw();
            }
            return;
        }

        // Content area click
        let div_y    = 38.0 + 12.0;
        let content_y = ly - div_y;
        if content_y < 0.0 { return; }
        let widget_idx = (content_y / WIDGET_ROW_H) as usize;
        if widget_idx >= self.widgets.len() { return; }

        let cx      = SIDEBAR_W + CONTENT_PAD;
        let track_x = cx + 120.0;
        let cw      = self.width as f32 - cx - CONTENT_PAD;
        let track_w = cw - 120.0 - 60.0;

        match &self.widgets[widget_idx] {
            Widget::Slider { .. } => {
                if lx >= track_x && lx <= track_x + track_w {
                    let val = ((lx - track_x) / track_w).clamp(0.0, 1.0);
                    apply_widget_action(&mut self.widgets[widget_idx], val);
                    self.draw();
                }
                self.drag_widget = Some(widget_idx);
            }
            Widget::Toggle { .. } => {
                let toggle_x = cx + 120.0;
                let toggle_w = 36.0;
                if lx >= toggle_x && lx <= toggle_x + toggle_w {
                    let current = matches!(&self.widgets[widget_idx], Widget::Toggle { value: true, .. });
                    apply_widget_action(&mut self.widgets[widget_idx], if current { 0.0 } else { 1.0 });
                    self.draw();
                }
            }
            Widget::Button { .. } => {
                apply_widget_action(&mut self.widgets[widget_idx], 1.0);
            }
            Widget::InfoRow { .. } => {}
        }
    }
}

// ── SCTK trait implementations ───────────────────────────────────────────────

impl CompositorHandler for SettingsApp {
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

impl OutputHandler for SettingsApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for SettingsApp {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.running = false;
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>,
                 _: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
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

impl ShmHandler for SettingsApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl SeatHandler for SettingsApp {
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

impl KeyboardHandler for SettingsApp {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32) {}
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
            Keysym::Tab => {
                let idx = Category::ALL.iter().position(|&c| c == self.active_category)
                    .unwrap_or(0);
                let next = (idx + 1) % Category::ALL.len();
                self.switch_category(Category::ALL[next]);
                self.draw();
            }
            _ => {}
        }
    }
}

impl PointerHandler for SettingsApp {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>,
                     _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        use PointerEventKind::*;
        for event in events {
            match &event.kind {
                Motion { .. } => {
                    self.pointer_pos = event.position;
                    // Drag slider
                    if let Some(widget_idx) = self.drag_widget {
                        let lx = event.position.0 as f32;
                        let cx = SIDEBAR_W + CONTENT_PAD;
                        let track_x = cx + 120.0;
                        let cw = self.width as f32 - cx - CONTENT_PAD;
                        let track_w = cw - 120.0 - 60.0;
                        if track_w > 0.0 {
                            let val = ((lx - track_x) / track_w).clamp(0.0, 1.0);
                            apply_widget_action(&mut self.widgets[widget_idx], val);
                            self.draw();
                        }
                    }
                }
                Press { button, .. } if *button == BTN_LEFT => {
                    let lx = event.position.0 as f32;
                    let ly = event.position.1 as f32;
                    self.handle_click(lx, ly);
                }
                Release { button, .. } if *button == BTN_LEFT => {
                    self.drag_widget = None;
                }
                _ => {}
            }
        }
    }
}

impl ProvidesRegistryState for SettingsApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(SettingsApp);
delegate_output!(SettingsApp);
delegate_layer!(SettingsApp);
delegate_shm!(SettingsApp);
delegate_seat!(SettingsApp);
delegate_keyboard!(SettingsApp);
delegate_pointer!(SettingsApp);
delegate_registry!(SettingsApp);

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
    let initial_category = Category::Display;
    let initial_widgets  = build_widgets(initial_category);

    let conn = Connection::connect_to_env().expect("wayland connect");
    let (globals, queue) = registry_queue_init::<SettingsApp>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor   = CompositorState::bind(&globals, &qh).expect("compositor");
    let layer_shell  = LayerShell::bind(&globals, &qh).expect("layer shell");
    let shm          = Shm::bind(&globals, &qh).expect("shm");
    let seat_state   = SeatState::new(&globals, &qh);
    let output_state = OutputState::new(&globals, &qh);

    let wl_surface = compositor.create_surface(&qh);
    let surface = layer_shell.create_layer_surface(
        &qh, wl_surface, Layer::Overlay, Some("vitobar-settings"), None,
    );
    surface.set_size(WIN_W, WIN_H);
    surface.set_anchor(Anchor::empty());   // centered
    surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();

    let mut app = SettingsApp {
        registry_state:  RegistryState::new(&globals),
        output_state,
        compositor,
        layer_shell,
        shm,
        seat_state,
        surface:     Some(surface),
        pool:        None,
        conn:        conn.clone(),
        qh:          qh.clone(),
        scale:       1,
        width:       WIN_W,
        height:      WIN_H,
        configured:  false,
        running:     true,
        pointer:     None,
        keyboard:    None,
        pointer_pos: (0.0, 0.0),
        drag_widget: None,
        config,
        active_category: initial_category,
        widgets:         initial_widgets,
    };

    let mut event_loop: EventLoop<SettingsApp> = EventLoop::try_new().expect("event loop");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    while app.running {
        event_loop.dispatch(None, &mut app).expect("dispatch");
    }
}
