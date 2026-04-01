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
use calloop::{EventLoop, channel as calloop_channel};
use calloop_wayland_source::WaylandSource;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

// Popup content dimensions
const POPUP_W: f32 = 600.0;
const POPUP_H: f32 = 420.0;
const ROW_H:    f32 = 30.0;
const SEARCH_H: f32 = 40.0;
const PADDING:  f32 = 10.0;

#[derive(Clone, Copy, Debug)]
enum RepeatAction { Up, Down }

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
    filtered:       Vec<usize>,
    selected:       usize,
    scroll_offset:  usize,

    // Key repeat
    repeat_tx:      calloop_channel::Sender<RepeatAction>,
    repeat_cancel:  Arc<AtomicBool>,
    repeat_info:    (u64, u64),  // (initial_delay_ms, interval_ms)

    // Content bounds in logical coords (updated each draw)
    content_x: f32,
    content_y: f32,

    // Mouse hover tracking
    hovered_row: Option<usize>,
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
        let area_h = POPUP_H - SEARCH_H - PADDING * 2.0;
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

    fn start_repeat(&mut self, action: RepeatAction) {
        // Cancel any previous repeat
        self.repeat_cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.repeat_cancel = cancel.clone();
        let tx = self.repeat_tx.clone();
        let (delay, interval) = self.repeat_info;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay));
            loop {
                if cancel.load(Ordering::SeqCst) { break; }
                if tx.send(action).is_err() { break; }
                std::thread::sleep(Duration::from_millis(interval));
            }
        });
    }

    fn stop_repeat(&mut self) {
        self.repeat_cancel.store(true, Ordering::SeqCst);
    }

    fn draw(&mut self) {
        let scale  = self.scale;
        let sw     = self.width  * scale;  // surface physical width
        let sh_px  = self.height * scale;  // surface physical height
        let sf     = scale as f32;

        // Center the popup on screen
        let cx = (self.width as f32 - POPUP_W) / 2.0;
        let cy = (self.height as f32 - POPUP_H) / 2.0;
        self.content_x = cx;
        self.content_y = cy;

        let pool = match self.pool.as_mut() { Some(p) => p, None => return };
        let surface = match self.surface.as_ref() { Some(s) => s, None => return };

        let stride = sw as i32 * 4;
        let (buffer, canvas) = pool
            .create_buffer(sw as i32, sh_px as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        let mut r = Renderer::new(sw, sh_px);
        // Transparent fullscreen background (no fill = stays at 0x00000000)
        // We intentionally skip r.clear() so background is transparent

        let fsz    = self.config.font_size.unwrap_or(11.0) * sf;
        let icon_fsz = fsz * 1.3;
        let pad    = PADDING * sf;
        let sh     = SEARCH_H * sf;
        let rh     = ROW_H * sf;
        let pw     = POPUP_W * sf;  // popup physical width
        let ph     = POPUP_H * sf;  // popup physical height
        let px     = cx * sf;       // popup origin in physical coords
        let py     = cy * sf;

        // ── Popup background ─────────────────────────────────────────────────
        r.draw_rect(px, py, pw, ph, &self.config.colors.base00);
        // Outer accent border
        r.draw_rect_outline(px, py, pw, ph, &self.config.colors.base0d, 2.0 * sf);
        // Inner shadow border (90s style)
        r.draw_rect_outline(px + 3.0 * sf, py + 3.0 * sf,
            pw - 6.0 * sf, ph - 6.0 * sf,
            &self.config.colors.base03, 1.0 * sf);

        // ── Search box ───────────────────────────────────────────────────────
        let sb_x = px + pad;
        let sb_y = py + pad;
        let sb_w = pw - pad * 2.0;
        let sb_h = sh - pad;

        r.draw_rect(sb_x, sb_y, sb_w, sb_h, &self.config.colors.base01);
        let outline_col = if self.query.is_empty() {
            self.config.colors.base02.clone()
        } else {
            self.config.colors.base0d.clone()
        };
        r.draw_rect_outline(sb_x, sb_y, sb_w, sb_h, &outline_col, 1.5 * sf);

        let search_icon = "\u{f002}";
        let icon_x = sb_x + 6.0 * sf;
        let text_baseline = sb_y + sb_h * 0.72;
        r.draw_text(search_icon, icon_x, text_baseline, icon_fsz,
            &self.config.colors.base03);

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
        let icon_w = r.measure_text(search_icon, icon_fsz) + 8.0 * sf;
        r.draw_text(&query_display, icon_x + icon_w, text_baseline, fsz, &query_col);

        // ── App list ─────────────────────────────────────────────────────────
        let list_top = py + sh + pad;
        let vis = {
            let area_h = POPUP_H - SEARCH_H - PADDING * 2.0;
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
            let is_hovered = self.hovered_row == Some(view_idx);

            if is_selected {
                r.draw_rect(px + pad, ry, pw - pad * 2.0, rh - 2.0 * sf,
                    &self.config.colors.base02);
                r.draw_rect_outline(px + pad, ry, pw - pad * 2.0, rh - 2.0 * sf,
                    &self.config.colors.base0d, 1.5 * sf);
            } else if is_hovered {
                r.draw_rect(px + pad, ry, pw - pad * 2.0, rh - 2.0 * sf,
                    &self.config.colors.base01);
                r.draw_rect_outline(px + pad, ry, pw - pad * 2.0, rh - 2.0 * sf,
                    &self.config.colors.base03, 1.0 * sf);
            }

            // Icon
            let icon_size = (18.0 * sf) as u32;
            let icon_x_phys  = (px + pad + 4.0 * sf) as u32;
            let icon_y_phys  = (ry + (rh - 18.0 * sf) / 2.0) as u32;
            let text_x;
            if let Some(rgba) = icons::load(&entry.app_id, icon_size) {
                r.draw_icon(icon_x_phys, icon_y_phys, icon_size, &rgba);
                text_x = px + pad + 4.0 * sf + 18.0 * sf + 8.0 * sf;
            } else {
                text_x = px + pad + 8.0 * sf;
            }

            let text_col = if is_selected {
                self.config.colors.base07.clone()
            } else {
                self.config.colors.base05.clone()
            };
            let text_y = ry + rh * 0.70;
            let avail = pw - (text_x - px) - pad;
            let clipped = r.clip_text(&entry.name, avail, fsz);
            r.draw_text(&clipped, text_x, text_y, fsz, &text_col);
        }

        // ── Empty state ───────────────────────────────────────────────────────
        if self.filtered.is_empty() {
            let msg = if self.query.is_empty() { "No applications found" } else { "No results" };
            let mw  = r.measure_text(msg, fsz);
            let mx  = px + (pw - mw) / 2.0;
            let my  = list_top + 40.0 * sf;
            r.draw_text(msg, mx, my, fsz, &self.config.colors.base03);
        }

        // ── Scroll indicator ─────────────────────────────────────────────────
        if self.filtered.len() > vis && vis > 0 {
            let total = self.filtered.len();
            let bar_h = (POPUP_H - SEARCH_H - PADDING * 2.0) * sf;
            let thumb_h = (vis as f32 / total as f32 * bar_h).max(20.0 * sf);
            let thumb_y = list_top + (self.scroll_offset as f32 / total as f32 * bar_h);
            let bar_x = px + pw - 6.0 * sf;
            r.draw_rect(bar_x, list_top, 4.0 * sf, bar_h, &self.config.colors.base01);
            r.draw_rect(bar_x, thumb_y, 4.0 * sf, thumb_h, &self.config.colors.base0d);
        }

        // ── Flush ─────────────────────────────────────────────────────────────
        let bgra = r.as_bgra();
        let len = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        surface.wl_surface().set_buffer_scale(scale as i32);
        surface.wl_surface().damage_buffer(0, 0, sw as i32, sh_px as i32);
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

    fn configure(&mut self, _: &Connection, _qh: &QueueHandle<Self>,
                 _surface: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        if configure.new_size.0 > 0 { self.width  = configure.new_size.0; }
        if configure.new_size.1 > 0 { self.height = configure.new_size.1; }

        if !self.configured {
            self.configured = true;
            let size = self.width as usize * self.height as usize * 4 * 4;
            self.pool = Some(SlotPool::new(size, &self.shm).expect("pool"));
        }
        self.draw();
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
        self.stop_repeat();
        self.running = false;
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>,
                   _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        match event.keysym {
            Keysym::Up | Keysym::KP_Up | Keysym::Down | Keysym::KP_Down => {
                self.stop_repeat();
            }
            _ => {}
        }
    }

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>,
                        _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}

    fn update_repeat_info(&mut self, _: &Connection, _: &QueueHandle<Self>,
                          _: &wl_keyboard::WlKeyboard, info: RepeatInfo) {
        if let RepeatInfo::Repeat { delay, rate } = info {
            // delay is u32 ms, rate is NonZero<u32> keys/sec
            let delay_ms    = delay as u64;
            let interval_ms = 1000u64 / u64::from(rate.get()).max(1);
            self.repeat_info = (delay_ms, interval_ms);
        }
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>,
                 _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        match event.keysym {
            Keysym::Escape => {
                self.stop_repeat();
                self.running = false;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.stop_repeat();
                self.launch_selected();
            }
            Keysym::Up | Keysym::KP_Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.clamp_scroll();
                    self.draw();
                }
                self.start_repeat(RepeatAction::Up);
            }
            Keysym::Down | Keysym::KP_Down => {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                    self.clamp_scroll();
                    self.draw();
                }
                self.start_repeat(RepeatAction::Down);
            }
            Keysym::BackSpace => {
                self.stop_repeat();
                self.query.pop();
                self.update_filter();
                self.draw();
            }
            _ => {
                self.stop_repeat();
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

                    // Update hover highlight
                    let (lx, ly) = (event.position.0 as f32, event.position.1 as f32);
                    let rx = lx - self.content_x;
                    let ry = ly - self.content_y;
                    let search_area_h = SEARCH_H + PADDING;

                    let new_hover = if rx >= 0.0 && rx < POPUP_W
                        && ry >= search_area_h && ry < POPUP_H
                    {
                        let row = ((ry - search_area_h) / ROW_H) as usize + self.scroll_offset;
                        if row < self.filtered.len() { Some(row) } else { None }
                    } else {
                        None
                    };

                    if new_hover != self.hovered_row {
                        self.hovered_row = new_hover;
                        self.draw();
                    }
                }
                Press { button, .. } if *button == BTN_LEFT => {
                    let (lx, ly) = (event.position.0 as f32, event.position.1 as f32);

                    // Click outside popup content area → close
                    let in_popup = lx >= self.content_x && lx < self.content_x + POPUP_W
                                && ly >= self.content_y && ly < self.content_y + POPUP_H;
                    if !in_popup {
                        self.running = false;
                        continue;
                    }

                    // Click inside popup — relative coords
                    let ry = ly - self.content_y;
                    let search_area_h = SEARCH_H + PADDING;

                    if ry >= search_area_h {
                        let row = ((ry - search_area_h) / ROW_H) as usize + self.scroll_offset;
                        if row < self.filtered.len() {
                            self.selected = row;
                            self.launch_selected();
                        }
                    }
                }
                Axis { horizontal: _, vertical, source: _, time: _ } => {
                    // Mouse wheel scroll
                    let delta = if vertical.discrete != 0 {
                        vertical.discrete
                    } else if vertical.absolute.abs() > 0.5 {
                        vertical.absolute.signum() as i32
                    } else {
                        0
                    };

                    let vis = self.visible_rows();
                    if delta > 0 && self.scroll_offset + vis < self.filtered.len() {
                        // Scroll down
                        self.scroll_offset += 1;
                        if self.selected < self.scroll_offset {
                            self.selected = self.scroll_offset;
                        }
                        self.draw();
                    } else if delta < 0 && self.scroll_offset > 0 {
                        // Scroll up
                        self.scroll_offset -= 1;
                        if vis > 0 && self.selected >= self.scroll_offset + vis {
                            self.selected = self.scroll_offset + vis - 1;
                        }
                        self.draw();
                    }
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
    // Full-screen overlay so click-outside detection works
    surface.set_size(0, 0);
    surface.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    surface.set_exclusive_zone(-1);
    surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();

    // Key repeat channel
    let (repeat_tx, repeat_rx) = calloop_channel::channel::<RepeatAction>();
    let repeat_cancel = Arc::new(AtomicBool::new(false));

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
        width:   1920,  // fallback, will be overridden by configure
        height:  1080,
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
        repeat_tx,
        repeat_cancel,
        repeat_info:  (300, 50),  // defaults: 300ms initial, 50ms interval
        content_x:    0.0,
        content_y:    0.0,
        hovered_row:  None,
    };

    let mut event_loop: EventLoop<LauncherApp> = EventLoop::try_new().expect("event loop");

    // Register key repeat channel source
    event_loop.handle().insert_source(repeat_rx, |event, _, app| {
        if let calloop_channel::Event::Msg(action) = event {
            match action {
                RepeatAction::Up => {
                    if app.selected > 0 {
                        app.selected -= 1;
                        app.clamp_scroll();
                        app.draw();
                    }
                }
                RepeatAction::Down => {
                    if app.selected + 1 < app.filtered.len() {
                        app.selected += 1;
                        app.clamp_scroll();
                        app.draw();
                    }
                }
            }
        }
    }).expect("insert repeat source");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    while app.running {
        event_loop.dispatch(None, &mut app).expect("dispatch");
    }
}
