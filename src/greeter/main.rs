#[path = "../render.rs"]   mod render;
#[path = "../config.rs"]   mod config;

use config::Config;
use render::Renderer;

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

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

// ── greetd IPC ──────────────────────────────────────────────────────────────

fn greetd_send(stream: &mut UnixStream, msg: &serde_json::Value) -> std::io::Result<()> {
    let json = serde_json::to_string(msg).unwrap();
    stream.write_all(&(json.len() as u32).to_le_bytes())?;
    stream.write_all(json.as_bytes())?;
    stream.flush()
}

fn greetd_recv(stream: &mut UnixStream) -> std::io::Result<serde_json::Value> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn greetd_login(username: &str, password: &str, cmd: &[&str]) -> Result<(), String> {
    let sock = std::env::var("GREETD_SOCK")
        .map_err(|_| "Not running under greetd".to_string())?;
    let mut stream = UnixStream::connect(&sock)
        .map_err(|e| format!("Cannot connect to greetd: {}", e))?;

    // Create session
    greetd_send(&mut stream, &serde_json::json!({
        "type": "create_session",
        "username": username
    })).map_err(|e| e.to_string())?;

    let resp = greetd_recv(&mut stream).map_err(|e| e.to_string())?;

    match resp["type"].as_str().unwrap_or("") {
        "success" => {
            // No auth needed — go straight to start_session
        }
        "auth_message" => {
            greetd_send(&mut stream, &serde_json::json!({
                "type": "post_auth_message_response",
                "response": password
            })).map_err(|e| e.to_string())?;

            let resp2 = greetd_recv(&mut stream).map_err(|e| e.to_string())?;
            match resp2["type"].as_str().unwrap_or("") {
                "success" => {}
                "error" => {
                    let _ = greetd_send(&mut stream, &serde_json::json!({
                        "type": "cancel_session"
                    }));
                    return Err(resp2["description"].as_str()
                        .unwrap_or("Authentication failed").into());
                }
                other => {
                    let _ = greetd_send(&mut stream, &serde_json::json!({
                        "type": "cancel_session"
                    }));
                    return Err(format!("Unexpected response: {}", other));
                }
            }
        }
        "error" => {
            let _ = greetd_send(&mut stream, &serde_json::json!({
                "type": "cancel_session"
            }));
            return Err(resp["description"].as_str()
                .unwrap_or("Session creation failed").into());
        }
        other => {
            let _ = greetd_send(&mut stream, &serde_json::json!({
                "type": "cancel_session"
            }));
            return Err(format!("Unexpected response: {}", other));
        }
    }

    // Start session
    greetd_send(&mut stream, &serde_json::json!({
        "type": "start_session",
        "cmd": cmd
    })).map_err(|e| e.to_string())?;

    let resp3 = greetd_recv(&mut stream).map_err(|e| e.to_string())?;
    match resp3["type"].as_str().unwrap_or("") {
        "success" => Ok(()),
        "error" => Err(resp3["description"].as_str()
            .unwrap_or("Failed to start session").into()),
        other => Err(format!("Unexpected response: {}", other)),
    }
}

// ── UI Constants ────────────────────────────────────────────────────────────

const CARD_W: f32 = 400.0;
const CARD_H: f32 = 300.0;
const FIELD_W: f32 = 340.0;
const FIELD_H: f32 = 36.0;
const BTN_H: f32 = 36.0;
const GAP: f32 = 12.0;
const PAD: f32 = 30.0;

// ── App ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum ActiveField { Username, Password }

/// One fullscreen layer-shell surface per connected monitor.
struct OutputSurface {
    output:     wl_output::WlOutput,
    layer:      LayerSurface,
    pool:       Option<SlotPool>,
    width:      u32,
    height:     u32,
    scale:      u32,
    configured: bool,
    // Hit regions in surface-logical coords, recomputed each draw.
    username_rect: [f32; 4],
    password_rect: [f32; 4],
    button_rect:   [f32; 4],
}

struct GreeterApp {
    registry_state: RegistryState,
    output_state:   OutputState,
    compositor:     CompositorState,
    layer_shell:    LayerShell,
    shm:            Shm,
    seat_state:     SeatState,
    conn:           Connection,
    qh:             QueueHandle<Self>,
    running:        bool,
    pointer:        Option<wl_pointer::WlPointer>,
    keyboard:       Option<wl_keyboard::WlKeyboard>,

    outputs: Vec<OutputSurface>,

    config:       Config,
    username:     String,
    password:     String,
    active_field: ActiveField,
    error_msg:    String,
    hostname:     String,
}

impl GreeterApp {
    fn attempt_login(&mut self) {
        if self.username.is_empty() {
            self.error_msg = "Enter a username".into();
            self.active_field = ActiveField::Username;
            return;
        }
        if self.password.is_empty() {
            self.error_msg = "Enter a password".into();
            return;
        }

        self.error_msg.clear();

        match greetd_login(&self.username, &self.password, &["niri", "--session"]) {
            Ok(()) => self.running = false,
            Err(e) => {
                self.error_msg = e;
                self.password.clear();
            }
        }
    }

    fn output_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.outputs.iter().position(|o| o.layer.wl_surface() == surface)
    }

    fn draw_all(&mut self) {
        for i in 0..self.outputs.len() {
            if self.outputs[i].configured {
                self.draw_output(i);
            }
        }
    }

    fn draw_output(&mut self, idx: usize) {
        let scale  = self.outputs[idx].scale;
        let width  = self.outputs[idx].width;
        let height = self.outputs[idx].height;
        let sw = width * scale;
        let sh = height * scale;
        let sf = scale as f32;

        // Shared UI state copied out so drawing doesn't tangle with the mutable
        // borrow of self.outputs[idx] below.
        let c = self.config.colors.clone();
        let fsz = self.config.font_size.unwrap_or(11.0) * sf;
        let icon_fsz = fsz * 1.2;
        let username = self.username.clone();
        let password = self.password.clone();
        let hostname = self.hostname.clone();
        let error_msg = self.error_msg.clone();
        let active_field = self.active_field;

        let mut r = Renderer::new(sw, sh);

        // ── Background ───────────────────────────────────────────────────
        r.clear(&c.base00);

        // ── Card ─────────────────────────────────────────────────────────
        let cx = (width as f32 - CARD_W) / 2.0 * sf;
        let cy = (height as f32 - CARD_H) / 2.0 * sf;
        let cw = CARD_W * sf;
        let ch = CARD_H * sf;
        let pad = PAD * sf;
        let gap = GAP * sf;
        let fw = FIELD_W * sf;
        let fh = FIELD_H * sf;
        let bh = BTN_H * sf;

        r.draw_rect(cx, cy, cw, ch, &c.base01);
        r.draw_rect_outline(cx, cy, cw, ch, &c.base0d, 2.0 * sf);

        let field_x = cx + (cw - fw) / 2.0;

        // ── Hostname ─────────────────────────────────────────────────────
        let host_w = r.measure_text(&hostname, fsz * 1.4);
        r.draw_text(&hostname, cx + (cw - host_w) / 2.0,
            cy + pad + fsz * 1.2, fsz * 1.4, &c.base0d);

        // ── Time ─────────────────────────────────────────────────────────
        let time_str = chrono::Local::now().format("%A, %B %d  %H:%M").to_string();
        let time_w = r.measure_text(&time_str, fsz);
        r.draw_text(&time_str, cx + (cw - time_w) / 2.0,
            cy + pad + fsz * 1.2 + fsz * 1.6, fsz, &c.base04);

        // ── Username field ───────────────────────────────────────────────
        let uf_y = cy + pad + fsz * 1.2 + fsz * 1.6 + gap * 2.0;
        let uf_border = if active_field == ActiveField::Username { &c.base0d } else { &c.base03 };
        r.draw_rect(field_x, uf_y, fw, fh, &c.base00);
        r.draw_rect_outline(field_x, uf_y, fw, fh, uf_border, 1.5 * sf);

        let icon_pad = 8.0 * sf;
        let text_y = uf_y + fh * 0.68;
        r.draw_text("\u{f007}", field_x + icon_pad, text_y, icon_fsz, &c.base04);
        let icon_w = r.measure_text("\u{f007}", icon_fsz) + icon_pad * 1.5;

        let (user_str, user_col) = if username.is_empty() && active_field != ActiveField::Username {
            ("Username".to_string(), c.base03.clone())
        } else {
            let mut s = username.clone();
            if active_field == ActiveField::Username { s.push('\u{258f}'); }
            if s.is_empty() { s.push('\u{258f}'); }
            (s, c.base05.clone())
        };
        r.draw_text(&user_str, field_x + icon_w, text_y, fsz, &user_col);

        let u_rect = [field_x / sf, uf_y / sf, FIELD_W, FIELD_H];

        // ── Password field ───────────────────────────────────────────────
        let pf_y = uf_y + fh + gap;
        let pf_border = if active_field == ActiveField::Password { &c.base0d } else { &c.base03 };
        r.draw_rect(field_x, pf_y, fw, fh, &c.base00);
        r.draw_rect_outline(field_x, pf_y, fw, fh, pf_border, 1.5 * sf);

        let pw_text_y = pf_y + fh * 0.68;
        r.draw_text("\u{f023}", field_x + icon_pad, pw_text_y, icon_fsz, &c.base04);

        let (pw_str, pw_col) = if password.is_empty() && active_field != ActiveField::Password {
            ("Password".to_string(), c.base03.clone())
        } else if password.is_empty() {
            ("\u{258f}".to_string(), c.base05.clone())
        } else {
            let mut masked = "\u{2022}".repeat(password.len());
            if active_field == ActiveField::Password { masked.push('\u{258f}'); }
            (masked, c.base05.clone())
        };
        r.draw_text(&pw_str, field_x + icon_w, pw_text_y, fsz, &pw_col);

        let p_rect = [field_x / sf, pf_y / sf, FIELD_W, FIELD_H];

        // ── Login button ─────────────────────────────────────────────────
        let btn_y = pf_y + fh + gap * 2.0;
        r.draw_rect(field_x, btn_y, fw, bh, &c.base0d);
        let btn_text = "Log in";
        let btn_tw = r.measure_text(btn_text, fsz);
        r.draw_text(btn_text, field_x + (fw - btn_tw) / 2.0,
            btn_y + bh * 0.68, fsz, &c.base00);

        let b_rect = [field_x / sf, btn_y / sf, FIELD_W, BTN_H];

        // ── Error message ────────────────────────────────────────────────
        if !error_msg.is_empty() {
            let err_y = btn_y + bh + gap;
            let err_w = r.measure_text(&error_msg, fsz);
            r.draw_text(&error_msg, cx + (cw - err_w) / 2.0,
                err_y + fsz, fsz, &c.base08);
        }

        // ── Flush ────────────────────────────────────────────────────────
        let bgra = r.into_bgra();

        let out = &mut self.outputs[idx];
        let pool = match out.pool.as_mut() { Some(p) => p, None => return };
        let stride = sw as i32 * 4;
        let (buffer, canvas) = match pool
            .create_buffer(sw as i32, sh as i32, stride, wl_shm::Format::Argb8888)
        {
            Ok(bc) => bc,
            Err(_) => return,
        };
        let len = canvas.len().min(bgra.len());
        canvas[..len].copy_from_slice(&bgra[..len]);

        let surface = out.layer.wl_surface();
        surface.set_buffer_scale(scale as i32);
        surface.damage_buffer(0, 0, sw as i32, sh as i32);
        if buffer.attach_to(surface).is_err() { return; }
        surface.commit();

        out.username_rect = u_rect;
        out.password_rect = p_rect;
        out.button_rect   = b_rect;

        self.conn.flush().ok();
    }
}

// ── SCTK trait implementations ──────────────────────────────────────────────

impl CompositorHandler for GreeterApp {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>,
                            surface: &wl_surface::WlSurface, factor: i32) {
        let new_scale = factor.max(1) as u32;
        if let Some(idx) = self.output_index(surface) {
            if self.outputs[idx].scale != new_scale {
                self.outputs[idx].scale = new_scale;
                self.outputs[idx].pool = None; // realloc on next draw
                if self.outputs[idx].configured {
                    self.alloc_pool(idx);
                    self.draw_output(idx);
                }
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

impl GreeterApp {
    fn alloc_pool(&mut self, idx: usize) {
        let out = &mut self.outputs[idx];
        let s = out.scale as usize;
        let size = (out.width as usize * out.height as usize * 4 * s * s * 2).max(4096);
        out.pool = Some(SlotPool::new(size, &self.shm).expect("pool"));
    }
}

impl OutputHandler for GreeterApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let info = self.output_state.info(&output);
        let scale = info.as_ref().map(|i| i.scale_factor.max(1) as u32).unwrap_or(1);
        let (w, h) = info.as_ref()
            .and_then(|i| i.logical_size)
            .map(|(w, h)| (w.max(1) as u32, h.max(1) as u32))
            .unwrap_or((1920, 1080));

        let wl = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh, wl, Layer::Overlay, Some("vitogreeter"), Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(0, 0);
        layer.commit();

        self.outputs.push(OutputSurface {
            output,
            layer,
            pool: None,
            width: w,
            height: h,
            scale,
            configured: false,
            username_rect: [0.0; 4],
            password_rect: [0.0; 4],
            button_rect:   [0.0; 4],
        });
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.outputs.retain(|o| o.output != output);
    }
}

impl LayerShellHandler for GreeterApp {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.running = false;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>,
                 layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let Some(idx) = self.output_index(layer.wl_surface()) else { return; };
        if configure.new_size.0 > 0 { self.outputs[idx].width  = configure.new_size.0; }
        if configure.new_size.1 > 0 { self.outputs[idx].height = configure.new_size.1; }
        self.outputs[idx].configured = true;
        self.alloc_pool(idx);
        self.draw_output(idx);
    }
}

impl ShmHandler for GreeterApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl SeatHandler for GreeterApp {
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

impl KeyboardHandler for GreeterApp {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
             _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>,
                 _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        match event.keysym {
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                self.active_field = match self.active_field {
                    ActiveField::Username => ActiveField::Password,
                    ActiveField::Password => ActiveField::Username,
                };
                self.draw_all();
            }
            Keysym::Return | Keysym::KP_Enter => {
                match self.active_field {
                    ActiveField::Username => {
                        self.active_field = ActiveField::Password;
                        self.draw_all();
                    }
                    ActiveField::Password => {
                        self.attempt_login();
                        self.draw_all();
                    }
                }
            }
            Keysym::BackSpace => {
                match self.active_field {
                    ActiveField::Username => { self.username.pop(); }
                    ActiveField::Password => { self.password.pop(); }
                }
                self.draw_all();
            }
            Keysym::Escape => {
                self.error_msg.clear();
                self.password.clear();
                self.draw_all();
            }
            _ => {
                if let Some(s) = event.utf8 {
                    let chars: Vec<char> = s.chars().filter(|c| !c.is_control()).collect();
                    if !chars.is_empty() {
                        match self.active_field {
                            ActiveField::Username => self.username.extend(chars),
                            ActiveField::Password => self.password.extend(chars),
                        }
                        self.draw_all();
                    }
                }
            }
        }
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>,
                   _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>,
                        _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}
    fn update_repeat_info(&mut self, _: &Connection, _: &QueueHandle<Self>,
                          _: &wl_keyboard::WlKeyboard, _: RepeatInfo) {}
}

impl PointerHandler for GreeterApp {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>,
                     _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            if let PointerEventKind::Press { button, .. } = &event.kind {
                if *button != BTN_LEFT { continue; }
                let Some(idx) = self.output_index(&event.surface) else { continue; };
                let (lx, ly) = (event.position.0 as f32, event.position.1 as f32);
                let hit = |r: &[f32; 4]| {
                    lx >= r[0] && lx < r[0] + r[2] && ly >= r[1] && ly < r[1] + r[3]
                };

                if hit(&self.outputs[idx].username_rect) {
                    self.active_field = ActiveField::Username;
                    self.draw_all();
                } else if hit(&self.outputs[idx].password_rect) {
                    self.active_field = ActiveField::Password;
                    self.draw_all();
                } else if hit(&self.outputs[idx].button_rect) {
                    self.attempt_login();
                    self.draw_all();
                }
            }
        }
    }
}

impl ProvidesRegistryState for GreeterApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(GreeterApp);
delegate_output!(GreeterApp);
delegate_layer!(GreeterApp);
delegate_shm!(GreeterApp);
delegate_seat!(GreeterApp);
delegate_keyboard!(GreeterApp);
delegate_pointer!(GreeterApp);
delegate_registry!(GreeterApp);

// ── Main ────────────────────────────────────────────────────────────────────

fn find_font() -> String {
    let out = std::process::Command::new("fc-match")
        .args(["JetBrainsMono Nerd Font Mono", "--format=%{file}"])
        .output()
        .expect("fc-match failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "nixos".to_string())
        .trim()
        .to_string()
}

fn main() {
    env_logger::init();

    let font_path = find_font();
    render::load_font(&font_path);

    let config = Config::load();
    let hostname = read_hostname();

    let conn = Connection::connect_to_env().expect("wayland connect");
    let (globals, queue) = registry_queue_init::<GreeterApp>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor   = CompositorState::bind(&globals, &qh).expect("compositor");
    let layer_shell  = LayerShell::bind(&globals, &qh).expect("layer shell");
    let shm          = Shm::bind(&globals, &qh).expect("shm");
    let seat_state   = SeatState::new(&globals, &qh);
    let output_state = OutputState::new(&globals, &qh);

    let mut app = GreeterApp {
        registry_state: RegistryState::new(&globals),
        output_state,
        compositor,
        layer_shell,
        shm,
        seat_state,
        conn:   conn.clone(),
        qh:     qh.clone(),
        running:    true,
        pointer:    None,
        keyboard:   None,
        outputs:    Vec::new(),
        config,
        username:     String::new(),
        password:     String::new(),
        active_field: ActiveField::Username,
        error_msg:    String::new(),
        hostname,
    };

    let mut event_loop: EventLoop<GreeterApp> = EventLoop::try_new().expect("event loop");

    // Redraw every 60s to keep the clock updated
    use calloop::timer::{Timer, TimeoutAction};
    let timer = Timer::from_duration(Duration::from_secs(60));
    event_loop.handle().insert_source(timer, |_, _, app| {
        app.draw_all();
        TimeoutAction::ToDuration(Duration::from_secs(60))
    }).expect("timer source");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    while app.running {
        if event_loop.dispatch(None, &mut app).is_err() {
            break;
        }
    }
}
