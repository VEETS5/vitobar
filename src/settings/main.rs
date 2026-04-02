#[path = "../render.rs"]   mod render;
#[path = "../config.rs"]   mod config;
#[path = "../icons.rs"]    mod icons;
mod categories;
mod widgets;

use config::Config;
use render::Renderer;
use categories::Category;
use widgets::{Widget, WidgetResult, apply_widget_action, build_widgets};

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

const WIN_W: u32 = 820;
const WIN_H: u32 = 620;
const TITLE_H:    f32 = 32.0;
const SIDEBAR_W:  f32 = 180.0;
const CAT_ROW_H:  f32 = 40.0;
const WIDGET_ROW_H:   f32 = 48.0;
const SECTION_ROW_H:  f32 = 30.0;
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
    drag_widget:     Option<usize>,

    config:          Config,
    active_category: Category,
    widgets:         Vec<Widget>,
    scroll_offset:   f32,
    hovered_widget:  Option<usize>,
    max_scroll:      f32,
    draw_pending:    bool,
}

impl SettingsApp {
    fn switch_category(&mut self, cat: Category) {
        self.active_category = cat;
        self.widgets = build_widgets(cat, &self.config);
        self.scroll_offset = 0.0;
        self.hovered_widget = None;
    }

    fn handle_config_result(&mut self, result: WidgetResult) {
        if let WidgetResult::ConfigUpdate { key, value } = result {
            match key {
                "bar_height" => self.config.bar_height = Some(value.round() as u32),
                "taskbar_height" => self.config.taskbar_height = Some(value.round() as u32),
                "font_size" => self.config.font_size = Some((value * 10.0).round() / 10.0),
                _ => {}
            }
            config::save_bar_settings(
                self.config.bar_height,
                self.config.taskbar_height,
                self.config.font_size,
            );
        }
    }

    fn content_height(&self) -> f32 {
        let mut h: f32 = 0.0;
        for widget in &self.widgets {
            h += match widget {
                Widget::SectionHeader { .. } => SECTION_ROW_H,
                Widget::InfoRow { .. }       => WIDGET_ROW_H * 0.7,
                _                            => WIDGET_ROW_H,
            };
        }
        h
    }

    fn widget_at_y(&self, content_ly: f32) -> Option<usize> {
        let mut wy: f32 = 0.0;
        for (i, widget) in self.widgets.iter().enumerate() {
            let widget_h = match widget {
                Widget::SectionHeader { .. } => SECTION_ROW_H,
                Widget::InfoRow { .. }       => WIDGET_ROW_H * 0.7,
                _                            => WIDGET_ROW_H,
            };
            if content_ly >= wy && content_ly < wy + widget_h {
                return Some(i);
            }
            wy += widget_h;
        }
        None
    }

    fn draw(&mut self) {
        let scale  = self.scale;
        let pw     = self.width  * scale;
        let ph     = self.height * scale;
        let sf     = scale as f32;

        // Pre-compute before borrowing pool
        let total_content_h_logical = self.content_height();

        let pool    = match self.pool.as_mut()    { Some(p) => p, None => return };
        let surface = match self.surface.as_ref() { Some(s) => s, None => return };

        let stride = pw as i32 * 4;
        let (buffer, canvas) = pool
            .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        let mut r = Renderer::new(pw, ph);
        r.clear(&self.config.colors.base00);

        let fsz = self.config.font_size.unwrap_or(11.0) * sf;
        let icon_fsz = fsz * 1.3;

        // ── Outer border + accent ─────────────────────────────────────────
        r.draw_rect_outline(0.0, 0.0, pw as f32, ph as f32,
            &self.config.colors.base02, 1.5 * sf);
        r.draw_rect(0.0, 0.0, pw as f32, 1.0 * sf, &self.config.colors.base0d);

        // ── Title bar ───────────────────────────────────────────────────────
        let th = TITLE_H * sf;
        r.draw_rect(0.0, 0.0, pw as f32, th, &self.config.colors.base01);
        r.draw_rect(0.0, th - 1.0 * sf, pw as f32, 1.0 * sf, &self.config.colors.base0d);

        let title_str = "\u{f313}  Settings";
        r.draw_text(title_str, 12.0 * sf, th * 0.70, fsz * 1.1, &self.config.colors.base0d);

        // X close button
        let close_w = 28.0 * sf;
        let close_h = th - 6.0 * sf;
        let close_x = pw as f32 - close_w - 4.0 * sf;
        let close_y = 3.0 * sf;
        r.draw_rect(close_x, close_y, close_w, close_h, &self.config.colors.base02);
        r.draw_rect_outline(close_x, close_y, close_w, close_h,
            &self.config.colors.base08, 1.0 * sf);
        let x_label = "\u{f00d}";
        let xtw = r.measure_text(x_label, icon_fsz);
        r.draw_text(x_label,
            close_x + (close_w - xtw) / 2.0,
            close_y + close_h * 0.72,
            icon_fsz, &self.config.colors.base08);

        // ── Sidebar ────────────────────────────────────────────────────────
        let sidebar_w = SIDEBAR_W * sf;
        let sidebar_y = th;
        let sidebar_h = ph as f32 - sidebar_y;
        r.draw_rect(0.0, sidebar_y, sidebar_w, sidebar_h, &self.config.colors.base01);

        for (i, cat) in Category::ALL.iter().enumerate() {
            let ry = sidebar_y + i as f32 * CAT_ROW_H * sf;
            let rh = CAT_ROW_H * sf;

            if *cat == self.active_category {
                r.draw_rect(0.0, ry, sidebar_w, rh, &self.config.colors.base02);
                // Left accent strip
                r.draw_rect(0.0, ry, 3.0 * sf, rh, &self.config.colors.base0d);
            }

            // Bottom separator
            r.draw_rect(8.0 * sf, ry + rh - 1.0, sidebar_w - 16.0 * sf, 1.0,
                &self.config.colors.base02);

            let text_col = if *cat == self.active_category {
                self.config.colors.base07.clone()
            } else {
                self.config.colors.base04.clone()
            };
            r.draw_text(cat.label(), 14.0 * sf, ry + rh * 0.62, fsz, &text_col);
        }

        // Sidebar right border
        r.draw_rect(sidebar_w, sidebar_y, 1.0 * sf, sidebar_h,
            &self.config.colors.base02);

        // ── Content area ───────────────────────────────────────────────────
        let cx  = sidebar_w + CONTENT_PAD * sf;
        let cw  = pw as f32 - cx - CONTENT_PAD * sf;
        let cty = th;

        // Content area border
        let content_border_x = sidebar_w + 6.0 * sf;
        let content_border_y = cty + 4.0 * sf;
        let content_border_w = pw as f32 - content_border_x - 6.0 * sf;
        let content_border_h = ph as f32 - content_border_y - 6.0 * sf;
        r.draw_rect_outline(content_border_x, content_border_y,
            content_border_w, content_border_h,
            &self.config.colors.base02, 1.0 * sf);

        // Category title
        r.draw_text(
            self.active_category.label(),
            cx, cty + 24.0 * sf,
            fsz * 1.2,
            &self.config.colors.base05,
        );

        // Divider below title
        let div_y = cty + 32.0 * sf;
        r.draw_rect(cx, div_y, cw, 1.0 * sf, &self.config.colors.base0d);

        let content_top = div_y + 8.0 * sf;
        let content_area_h = ph as f32 - content_top - 4.0 * sf;
        let total_content_h = total_content_h_logical * sf;
        self.max_scroll = (total_content_h - content_area_h).max(0.0) / sf;

        let mut wy = content_top - self.scroll_offset * sf;

        for (wi, widget) in self.widgets.iter().enumerate() {
            let is_hovered = self.hovered_widget == Some(wi);

            match widget {
                Widget::SectionHeader { label } => {
                    let sh = SECTION_ROW_H * sf;
                    if wy + sh > content_top && wy < ph as f32 {
                        r.draw_text(label, cx, wy + sh * 0.72, fsz * 0.9,
                            &self.config.colors.base0d);
                        r.draw_rect(cx, wy + sh - 1.0 * sf, cw, 1.0 * sf,
                            &self.config.colors.base02);
                    }
                    wy += sh;
                }

                Widget::Slider { label, value, .. } | Widget::ConfigSlider { label, value, .. } => {
                    let wh = WIDGET_ROW_H * sf;
                    if wy + wh > content_top && wy < ph as f32 {
                        if is_hovered {
                            r.draw_rect(cx - 4.0 * sf, wy, cw + 8.0 * sf, wh,
                                &self.config.colors.base01);
                        }

                        r.draw_text(label, cx, wy + fsz * 1.3, fsz,
                            &self.config.colors.base05);

                        let track_x = cx + 140.0 * sf;
                        let track_w = cw - 140.0 * sf - 60.0 * sf;
                        let track_y = wy + wh * 0.5 - 3.0 * sf;
                        let track_h = 6.0 * sf;

                        // Track background
                        r.draw_rect(track_x, track_y, track_w, track_h,
                            &self.config.colors.base02);
                        // Track fill
                        r.draw_rect(track_x, track_y, track_w * value, track_h,
                            &self.config.colors.base0d);
                        // Thumb
                        let thumb_x = track_x + track_w * value - 5.0 * sf;
                        r.draw_rect(thumb_x, track_y - 4.0 * sf, 10.0 * sf, track_h + 8.0 * sf,
                            &self.config.colors.base0d);
                        r.draw_rect_outline(thumb_x, track_y - 4.0 * sf,
                            10.0 * sf, track_h + 8.0 * sf,
                            &self.config.colors.base07, 1.0 * sf);

                        // Value display
                        let display_val = match widget {
                            Widget::ConfigSlider { min, max, .. } => {
                                let actual = min + (max - min) * value;
                                format!("{:.0}", actual)
                            }
                            _ => format!("{:.0}%", value * 100.0),
                        };
                        let pct_x = track_x + track_w + 10.0 * sf;
                        r.draw_text(&display_val, pct_x, wy + wh * 0.62, fsz,
                            &self.config.colors.base05);
                    }
                    wy += wh;
                }

                Widget::Toggle { label, value, .. } => {
                    let wh = WIDGET_ROW_H * sf;
                    if wy + wh > content_top && wy < ph as f32 {
                        if is_hovered {
                            r.draw_rect(cx - 4.0 * sf, wy, cw + 8.0 * sf, wh,
                                &self.config.colors.base01);
                        }

                        r.draw_text(label, cx, wy + fsz * 1.3, fsz,
                            &self.config.colors.base05);

                        let toggle_x = cx + 140.0 * sf;
                        let toggle_y = wy + wh * 0.5 - 10.0 * sf;
                        let toggle_w = 42.0 * sf;
                        let toggle_h = 22.0 * sf;

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
                            self.config.colors.base07.clone()
                        } else {
                            self.config.colors.base04.clone()
                        };
                        r.draw_rect(thumb_x + 2.0 * sf, toggle_y + 2.0 * sf,
                            toggle_h - 4.0 * sf, toggle_h - 4.0 * sf, &thumb_col);

                        let state_str = if *value { "On" } else { "Off" };
                        let state_col = if *value {
                            &self.config.colors.base0b
                        } else {
                            &self.config.colors.base04
                        };
                        r.draw_text(state_str, toggle_x + toggle_w + 8.0 * sf,
                            wy + wh * 0.62, fsz, state_col);
                    }
                    wy += wh;
                }

                Widget::InfoRow { label, value } => {
                    let wh = WIDGET_ROW_H * 0.7 * sf;
                    if wy + wh > content_top && wy < ph as f32 {
                        r.draw_text(label, cx, wy + fsz * 1.1, fsz,
                            &self.config.colors.base03);
                        let avail = cw - 110.0 * sf;
                        let clipped = r.clip_text(value, avail, fsz);
                        r.draw_text(&clipped, cx + 110.0 * sf, wy + fsz * 1.1, fsz,
                            &self.config.colors.base05);
                    }
                    wy += wh;
                }

                Widget::Button { label, .. } => {
                    let wh = WIDGET_ROW_H * sf;
                    if wy + wh > content_top && wy < ph as f32 {
                        let btn_w = (r.measure_text(label, fsz) + 24.0 * sf).min(cw - 8.0 * sf);
                        let btn_h = 30.0 * sf;
                        let btn_y = wy + (wh - btn_h) / 2.0;

                        let (btn_bg, btn_border) = if is_hovered {
                            (&self.config.colors.base02, &self.config.colors.base0d)
                        } else {
                            (&self.config.colors.base01, &self.config.colors.base03)
                        };
                        r.draw_rect(cx, btn_y, btn_w, btn_h, btn_bg);
                        r.draw_rect_outline(cx, btn_y, btn_w, btn_h, btn_border, 1.0 * sf);
                        let text_col = if is_hovered {
                            &self.config.colors.base07
                        } else {
                            &self.config.colors.base05
                        };
                        r.draw_text(label, cx + 12.0 * sf, btn_y + btn_h * 0.70, fsz, text_col);
                    }
                    wy += wh;
                }
            }
        }

        // ── Scrollbar ───────────────────────────────────────────────────────
        if total_content_h > content_area_h {
            let sb_x = pw as f32 - 6.0 * sf;
            let sb_h = content_area_h;
            let thumb_h = (content_area_h / total_content_h * sb_h).max(20.0 * sf);
            let thumb_y = content_top + (self.scroll_offset * sf / total_content_h * sb_h);
            r.draw_rect(sb_x, content_top, 4.0 * sf, sb_h, &self.config.colors.base01);
            r.draw_rect(sb_x, thumb_y, 4.0 * sf, thumb_h, &self.config.colors.base0d);
        }

        // ── Flush ──────────────────────────────────────────────────────────
        let bgra = r.into_bgra();
        let len  = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        surface.wl_surface().set_buffer_scale(scale as i32);
        surface.wl_surface().damage_buffer(0, 0, pw as i32, ph as i32);
        buffer.attach_to(surface.wl_surface()).expect("buffer attach");
        surface.commit();
        self.conn.flush().ok();
    }

    fn handle_click(&mut self, lx: f32, ly: f32) {
        // X close button
        let close_x = self.width as f32 - 32.0;
        if lx >= close_x && ly < TITLE_H {
            self.running = false;
            return;
        }

        // Sidebar click
        if lx < SIDEBAR_W {
            if ly < TITLE_H { return; }
            let ly_sidebar = ly - TITLE_H;
            let idx = (ly_sidebar / CAT_ROW_H) as usize;
            if let Some(&cat) = Category::ALL.get(idx) {
                self.switch_category(cat);
                self.draw();
            }
            return;
        }

        // Content area click
        let content_top = TITLE_H + 32.0 + 8.0;
        let content_ly = ly - content_top + self.scroll_offset;
        if content_ly < 0.0 { return; }

        if let Some(widget_idx) = self.widget_at_y(content_ly) {
            let cx      = SIDEBAR_W + CONTENT_PAD;
            let track_x = cx + 140.0;
            let cw      = self.width as f32 - cx - CONTENT_PAD;
            let track_w = cw - 140.0 - 60.0;

            match &self.widgets[widget_idx] {
                Widget::Slider { .. } | Widget::ConfigSlider { .. } => {
                    if lx >= track_x && lx <= track_x + track_w {
                        let val = ((lx - track_x) / track_w).clamp(0.0, 1.0);
                        let result = apply_widget_action(&mut self.widgets[widget_idx], val);
                        self.handle_config_result(result);
                        self.draw();
                    }
                    self.drag_widget = Some(widget_idx);
                }
                Widget::Toggle { .. } => {
                    let current = matches!(&self.widgets[widget_idx],
                        Widget::Toggle { value: true, .. });
                    let result = apply_widget_action(&mut self.widgets[widget_idx],
                        if current { 0.0 } else { 1.0 });
                    self.handle_config_result(result);
                    self.draw();
                }
                Widget::Button { .. } => {
                    let result = apply_widget_action(&mut self.widgets[widget_idx], 1.0);
                    self.handle_config_result(result);
                }
                Widget::InfoRow { .. } | Widget::SectionHeader { .. } => {}
            }
        }
    }
}

// ── SCTK trait implementations ───────────────────────────────────────────────

impl CompositorHandler for SettingsApp {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>,
                            _: &wl_surface::WlSurface, factor: i32) {
        let new_scale = (factor.max(1) as u32).max(2);
        if new_scale != self.scale {
            self.scale = new_scale;
            // Re-allocate pool for new scale
            let size = self.width as usize * self.height as usize
                       * 4 * (new_scale as usize * new_scale as usize) * 2;
            self.pool = Some(SlotPool::new(size, &self.shm).expect("pool"));
            if self.configured {
                self.draw();
            }
        }
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

    fn configure(&mut self, _: &Connection, _qh: &QueueHandle<Self>,
                 _: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        if configure.new_size.0 > 0 { self.width  = configure.new_size.0; }
        if configure.new_size.1 > 0 { self.height = configure.new_size.1; }

        if !self.configured {
            self.configured = true;
            let s = self.scale as usize;
            let size = self.width as usize * self.height as usize * 4 * s * s * 2;
            self.pool = Some(SlotPool::new(size, &self.shm).expect("pool"));
        }
        self.draw();
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

                    // Slider drag - update value but defer redraw
                    if let Some(widget_idx) = self.drag_widget {
                        let lx = event.position.0 as f32;
                        let cx = SIDEBAR_W + CONTENT_PAD;
                        let track_x = cx + 140.0;
                        let cw = self.width as f32 - cx - CONTENT_PAD;
                        let track_w = cw - 140.0 - 60.0;
                        if track_w > 0.0 {
                            let val = ((lx - track_x) / track_w).clamp(0.0, 1.0);
                            let result = apply_widget_action(&mut self.widgets[widget_idx], val);
                            self.handle_config_result(result);
                            self.draw_pending = true;
                        }
                        continue;
                    }

                    // Hover tracking in content area
                    let lx = event.position.0 as f32;
                    let ly = event.position.1 as f32;
                    let content_top = TITLE_H + 32.0 + 8.0;
                    let new_hover = if lx > SIDEBAR_W && ly > content_top {
                        let content_ly = ly - content_top + self.scroll_offset;
                        self.widget_at_y(content_ly)
                    } else {
                        None
                    };
                    if new_hover != self.hovered_widget {
                        self.hovered_widget = new_hover;
                        self.draw();
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
                Axis { horizontal: _, vertical, source: _, time: _ } => {
                    let delta = if vertical.discrete != 0 {
                        vertical.discrete as f32 * 20.0
                    } else {
                        vertical.absolute as f32
                    };
                    self.scroll_offset = (self.scroll_offset + delta).clamp(0.0, self.max_scroll);
                    self.draw();
                }
                _ => {}
            }
        }
        // Flush deferred draws (coalesces multiple motion events into one redraw)
        if self.draw_pending {
            self.draw_pending = false;
            self.draw();
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
    let initial_category = Category::Appearance;
    let initial_widgets  = build_widgets(initial_category, &config);

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
        &qh, wl_surface, Layer::Overlay, Some("vitosettings"), None,
    );
    surface.set_size(WIN_W, WIN_H);
    surface.set_anchor(Anchor::empty());
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
        scale:       2,
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
        scroll_offset:   0.0,
        hovered_widget:  None,
        max_scroll:      0.0,
        draw_pending:    false,
    };

    let mut event_loop: EventLoop<SettingsApp> = EventLoop::try_new().expect("event loop");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    while app.running {
        event_loop.dispatch(None, &mut app).expect("dispatch");
    }
}
