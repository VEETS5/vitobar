use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::config::hex_to_rgba;

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
}
