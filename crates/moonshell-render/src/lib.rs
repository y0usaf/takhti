//! CPU rendering into wl_shm ARGB8888 buffers: tiny-skia for geometry,
//! cosmic-text for shaping and glyph rasterization.
//!
//! Coordinate doctrine: everything here works in integer physical
//! (buffer) pixels. Logical→physical scaling happens in the caller;
//! fonts are sized in physical pixels directly so glyphs are never
//! resampled.
//!
//! Pixel format: wl_shm ARGB8888 is little-endian, i.e. bytes
//! [B, G, R, A], premultiplied alpha. tiny-skia's pixmaps are byte-order
//! [R, G, B, A] premultiplied, so colors are swizzled (R<->B) exactly
//! once, inside [`Rgba::to_skia`], and tiny-skia then writes correct
//! ARGB8888 memory without any post-pass.

mod assets;
pub mod draw;
pub mod element;
pub mod layout;
pub mod scene;

pub use draw::{draw, render_tree};
pub use element::Element;
pub use layout::LayoutNode;
pub use scene::{PixelRect, Scene, SceneDamage};

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{FillRule, Paint, PathBuilder, PixmapMut, Rect, Stroke, Transform};

/// Straight-alpha color as the caller thinks of it (CSS-style RGBA).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// R and B swapped: tiny-skia writes RGBA byte order, the buffer is
    /// interpreted as ARGB8888 little-endian (BGRA bytes).
    fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.b, self.g, self.r, self.a)
    }
}

/// Owns the font system and glyph caches — the dominant allocation in
/// the process (budgeted in PLAN.md). Create one and keep it alive.
pub struct Renderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Decoded icon/image pixmaps (see `assets`); part of the same
    /// cache budget as the glyph caches.
    pub(crate) assets: assets::AssetCache,
}

impl Renderer {
    /// Discovers system fonts via fontconfig configuration (parsed in
    /// pure Rust by fontdb).
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            assets: assets::AssetCache::default(),
        }
    }

    /// Src-over a cached pixmap (already premultiplied and in buffer
    /// byte order) onto the canvas at `(x, y)`. Pixmaps are pre-scaled
    /// to their target size, so no filtering happens here.
    pub(crate) fn blit(
        &mut self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        x: i32,
        y: i32,
        pm: &tiny_skia::Pixmap,
    ) {
        let Some(mut dst) = pixmap(canvas, width, height) else {
            return;
        };
        dst.draw_pixmap(
            x,
            y,
            pm.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }

    /// Fill the whole canvas with `color`.
    pub fn clear(&mut self, canvas: &mut [u8], width: u32, height: u32, color: Rgba) {
        let Some(mut pixmap) = pixmap(canvas, width, height) else {
            return;
        };
        pixmap.fill(color.to_skia());
    }

    /// Fill an axis-aligned rect, clipped to the canvas.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rect(
        &mut self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Rgba,
    ) {
        let Some(rect) = Rect::from_xywh(x, y, w, h) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        let Some(mut pixmap) = pixmap(canvas, width, height) else {
            return;
        };
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }

    /// Fill a rounded rect (radius clamped to half the short side).
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rounded_rect(
        &mut self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: Rgba,
    ) {
        let r = radius.min(w / 2.0).min(h / 2.0);
        if r <= 0.5 {
            self.fill_rect(canvas, width, height, x, y, w, h, color);
            return;
        }
        let Some(path) = rounded_rect_path(x, y, w, h, r) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        let Some(mut pixmap) = pixmap(canvas, width, height) else {
            return;
        };
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    /// Stroke a circular arc centered at `(cx, cy)`. Angles in degrees,
    /// 0° = 3 o'clock, positive = clockwise (screen coordinates).
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc(
        &mut self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        cx: f32,
        cy: f32,
        radius: f32,
        thickness: f32,
        start_deg: f32,
        sweep_deg: f32,
        color: Rgba,
    ) {
        if radius <= 0.0 || thickness <= 0.0 || sweep_deg == 0.0 {
            return;
        }
        let Some(path) = arc_path(
            cx,
            cy,
            radius,
            start_deg.to_radians(),
            sweep_deg.to_radians(),
        ) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        paint.anti_alias = true;
        let stroke = Stroke {
            width: thickness,
            ..Stroke::default()
        };
        let Some(mut pixmap) = pixmap(canvas, width, height) else {
            return;
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    /// Shape a single line and return its advance width in buffer
    /// pixels, without drawing. Used by the layout measure pass.
    pub fn measure_text(&mut self, text: &str, font_px: f32, line_px: f32) -> f32 {
        if self.font_system.db().faces().next().is_none() {
            return 0.0;
        }
        let buffer = self.shape(text, font_px, line_px);
        buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0_f32, f32::max)
    }

    fn shape(&mut self, text: &str, font_px: f32, line_px: f32) -> Buffer {
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_px, line_px));
        buffer.set_size(None, Some(line_px));
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        // set_text only marks dirty in cosmic-text 0.19; shaping is explicit.
        buffer.shape_until_scroll(&mut self.font_system, true);
        buffer
    }

    /// Shape and draw a single line of text with its top-left corner at
    /// `(x, y)` in buffer pixels. `font_px` is the physical font size.
    /// `max_w` clips glyphs beyond `x + max_w` (icon fallback text must
    /// stay inside its box instead of overpainting siblings); `None`
    /// draws unclipped. Returns the advance width in buffer pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn text_line(
        &mut self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        x: i32,
        y: i32,
        text: &str,
        font_px: f32,
        line_px: f32,
        color: Rgba,
        max_w: Option<f32>,
    ) -> f32 {
        // cosmic-text panics ("no default font found") when the font
        // database is empty; a fontless system gets a bar without text,
        // not a crash — library crates don't panic.
        if self.font_system.db().faces().next().is_none() {
            tracing::warn!("no fonts available; skipping text draw");
            return 0.0;
        }
        let mut buffer = self.shape(text, font_px, line_px);

        let advance = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0_f32, f32::max);

        let src = cosmic_text::Color::rgba(color.r, color.g, color.b, color.a);
        let clip_x1 = max_w.map(|w| x + w.round() as i32);
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            src,
            |gx, gy, gw, gh, c| {
                let (gx, gw) = match clip_x1 {
                    Some(x1) => {
                        let gx0 = x + gx;
                        let gx1 = (gx0 + gw as i32).min(x1);
                        if gx1 <= gx0 {
                            return;
                        }
                        (gx, (gx1 - gx0) as u32)
                    }
                    None => (gx, gw),
                };
                blend_rect(canvas, width, height, x + gx, y + gy, gw, gh, c);
            },
        );
        advance
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Rounded-rect outline as a tiny-skia path; corners are single cubic
/// beziers with the circle-approximation constant.
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    // 4/3 * (sqrt(2) - 1): cubic control distance approximating a
    // quarter circle.
    const K: f32 = 0.552_285;
    let k = r * K;
    let (x1, y1) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + k, y, x1, y + r - k, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - k, y1, x, y1 - r + k, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Circular arc as cubic bezier segments of at most 90° each.
fn arc_path(cx: f32, cy: f32, r: f32, start: f32, sweep: f32) -> Option<tiny_skia::Path> {
    let segments = (sweep.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0);
    let delta = sweep / segments;
    let k = 4.0 / 3.0 * (delta / 4.0).tan();
    let point = |a: f32| (cx + r * a.cos(), cy + r * a.sin());
    let mut pb = PathBuilder::new();
    let mut a0 = start;
    let (px0, py0) = point(a0);
    pb.move_to(px0, py0);
    for _ in 0..segments as u32 {
        let a1 = a0 + delta;
        let (x0, y0) = point(a0);
        let (x3, y3) = point(a1);
        // Tangent directions at the endpoints, scaled by k*r.
        let (t0x, t0y) = (-a0.sin(), a0.cos());
        let (t1x, t1y) = (-a1.sin(), a1.cos());
        pb.cubic_to(
            x0 + k * r * t0x,
            y0 + k * r * t0y,
            x3 - k * r * t1x,
            y3 - k * r * t1y,
            x3,
            y3,
        );
        a0 = a1;
    }
    pb.finish()
}

/// `None` (drawing silently skipped) if the byte length doesn't match
/// `width * height * 4` — a caller bug, but library crates don't panic.
fn pixmap<'a>(canvas: &'a mut [u8], width: u32, height: u32) -> Option<PixmapMut<'a>> {
    let pm = PixmapMut::from_bytes(canvas, width, height);
    debug_assert!(pm.is_some(), "canvas size mismatch");
    pm
}

/// Src-over blend of a solid straight-alpha color rect into the
/// ARGB8888 canvas. cosmic-text emits mostly 1x1 rects (per-pixel
/// coverage baked into the alpha channel).
#[allow(clippy::too_many_arguments)]
fn blend_rect(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    c: cosmic_text::Color,
) {
    let a = c.a() as u32;
    if a == 0 {
        return;
    }
    // Premultiplied source, in buffer byte order [B, G, R, A].
    let src = [
        (c.b() as u32 * a + 127) / 255,
        (c.g() as u32 * a + 127) / 255,
        (c.r() as u32 * a + 127) / 255,
        a,
    ];
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x + w as i32).clamp(0, width as i32) as u32;
    let y1 = (y + h as i32).clamp(0, height as i32) as u32;
    let inv = 255 - a;
    for py in y0..y1 {
        let row = (py * width) as usize * 4;
        for px in x0..x1 {
            let i = row + px as usize * 4;
            let dst = &mut canvas[i..i + 4];
            for ch in 0..4 {
                dst[ch] = (src[ch] + (dst[ch] as u32 * inv + 127) / 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The R<->B swizzle: an opaque red clear must produce ARGB8888
    /// little-endian bytes [B, G, R, A] = [0, 0, 255, 255].
    #[test]
    fn clear_writes_argb8888_le() {
        let mut renderer = Renderer::new();
        let mut buf = vec![0u8; 2 * 2 * 4];
        renderer.clear(&mut buf, 2, 2, Rgba::new(255, 0, 0, 255));
        assert_eq!(&buf[0..4], &[0, 0, 255, 255]);
    }

    /// Text rasterization touches pixels — skipped when the environment
    /// has no fonts (e.g. the nix sandbox), which is a config problem,
    /// not a renderer bug.
    #[test]
    fn text_line_draws_glyphs() {
        let mut renderer = Renderer::new();
        if renderer.font_system.db().faces().next().is_none() {
            return;
        }
        let (w, h) = (128u32, 32u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let advance = renderer.text_line(
            &mut buf,
            w,
            h,
            0,
            0,
            "moonshell",
            16.0,
            20.0,
            Rgba::new(255, 255, 255, 255),
            None,
        );
        assert!(advance > 0.0, "no advance — shaping produced nothing");
        assert!(buf.iter().any(|&b| b != 0), "no pixels touched");
    }
}
