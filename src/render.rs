use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::config::hex_to_rgba;
use fontdue::{Font, FontSettings};
use std::sync::OnceLock;

static FONT: OnceLock<Font> = OnceLock::new();

pub fn load_font(path: &str) {
    let bytes = std::fs::read(path).expect("failed to read font");
    let font  = Font::from_bytes(bytes.as_slice(), FontSettings::default()).expect("failed to parse font");
    FONT.set(font).ok();
}

pub fn get_font() -> &'static Font {
    FONT.get().expect("font not loaded")
}

pub struct Renderer {
    pub pixmap: Pixmap,
}

impl Renderer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pixmap: Pixmap::new(width, height).expect("failed to create pixmap"),
        }
    }

    pub fn clear(&mut self, hex: &str) {
        let (r, g, b, a) = hex_to_rgba(hex);
        self.pixmap.fill(Color::from_rgba8(r, g, b, a));
    }

    pub fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill_hex: &str) {
        let (r, g, b, a) = hex_to_rgba(fill_hex);
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(r, g, b, a));
        paint.anti_alias = false;

        let rect = Rect::from_xywh(x, y, w, h).unwrap();
        let path = PathBuilder::from_rect(rect);
        self.pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    pub fn draw_rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, stroke_hex: &str, stroke_width: f32) {
        let (r, g, b, a) = hex_to_rgba(stroke_hex);
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(r, g, b, a));
        paint.anti_alias = false;

        let rect = Rect::from_xywh(x, y, w, h).unwrap();
        let path = PathBuilder::from_rect(rect);
        let stroke = Stroke {
            width: stroke_width,
            ..Default::default()
        };
        self.pixmap.stroke_path(
            &path,
            &paint,
            &stroke,
            Transform::identity(),
            None,
        );
    }

    /// Returns raw BGRA bytes for Wayland shm buffer
    pub fn as_bgra(&self) -> Vec<u8> {
        let data = self.pixmap.data();
        let mut bgra = Vec::with_capacity(data.len());
        for chunk in data.chunks(4) {
            // tiny-skia is RGBA, Wayland xrgb8888 wants BGRA
            bgra.push(chunk[2]); // B
            bgra.push(chunk[1]); // G
            bgra.push(chunk[0]); // R
            bgra.push(chunk[3]); // A
        }
        bgra
    }

    /// `y` is the **baseline** in screen-space (Y increases downward).
    /// Each glyph bitmap row 0 is the topmost row, placed at
    ///   screen_y = baseline - ymin - height
    pub fn draw_text(&mut self, text: &str, x: f32, y: f32, size: f32, color_hex: &str) {
        let font = get_font();
        let (r, g, b, _) = hex_to_rgba(color_hex);
        let mut cx = x;

        for ch in text.chars() {
            let (metrics, bitmap) = font.rasterize(ch, size);

            // Top-left of the bitmap in screen coordinates.
            let top_y  = y as i32 - metrics.ymin as i32 - metrics.height as i32;
            let left_x = cx as i32 + metrics.xmin;

            for gy in 0..metrics.height {
                for gx in 0..metrics.width {
                    let alpha = bitmap[gy * metrics.width + gx];
                    if alpha == 0 { continue; }

                    let px_i = left_x + gx as i32;
                    let py_i = top_y  + gy as i32;
                    if px_i < 0 || py_i < 0 { continue; }
                    let px = px_i as u32;
                    let py = py_i as u32;
                    if px >= self.pixmap.width() || py >= self.pixmap.height() { continue; }

                    let a   = alpha as f32 / 255.0;
                    let idx = (py * self.pixmap.width() + px) as usize * 4;
                    let data = self.pixmap.data_mut();
                    data[idx]     = (r as f32 * a + data[idx]     as f32 * (1.0 - a)) as u8;
                    data[idx + 1] = (g as f32 * a + data[idx + 1] as f32 * (1.0 - a)) as u8;
                    data[idx + 2] = (b as f32 * a + data[idx + 2] as f32 * (1.0 - a)) as u8;
                    data[idx + 3] = 255;
                }
            }
            cx += metrics.advance_width;
        }
    }

    /// Returns the total advance width of `text` at `size` px.
    pub fn measure_text(&self, text: &str, size: f32) -> f32 {
        let font = get_font();
        text.chars().map(|ch| font.rasterize(ch, size).0.advance_width).sum()
    }

    /// Truncates `text` so it fits within `max_width` px, appending "..." if needed.
    pub fn truncate_text(&self, text: &str, max_width: f32, size: f32) -> String {
        if self.measure_text(text, size) <= max_width {
            return text.to_string();
        }
        let font    = get_font();
        let dot_w   = font.rasterize('.', size).0.advance_width;
        let dots_w  = dot_w * 3.0;
        let mut out = String::new();
        let mut used = 0.0f32;
        for ch in text.chars() {
            let cw = font.rasterize(ch, size).0.advance_width;
            if used + cw + dots_w > max_width { break; }
            out.push(ch);
            used += cw;
        }
        out.push_str("...");
        out
    }
}
