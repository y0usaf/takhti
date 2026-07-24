//! Software text rasterization for compositor-drawn UI (dialogs, overlays).
//!
//! No pango/cairo: `fontdue` rasterizes system TTFs into a premultiplied-RGBA
//! `Canvas`, which becomes a `MemoryRenderBuffer` render element.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle};
use fontdue::{Font, FontSettings};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Physical, Size, Transform};

pub struct Fonts {
    /// [sans, mono]; indexed by `Face`.
    faces: [Font; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Sans = 0,
    Mono = 1,
}

/// One run of uniformly-styled text within a line.
pub struct Span<'a> {
    pub text: &'a str,
    pub face: Face,
    pub size: f32,
    pub color: [f32; 4],
    /// Background "key chip" drawn behind the span (e.g. ` Enter `).
    pub chip: Option<[f32; 4]>,
}

impl<'a> Span<'a> {
    pub fn sans(text: &'a str, size: f32, color: [f32; 4]) -> Self {
        Self {
            text,
            face: Face::Sans,
            size,
            color,
            chip: None,
        }
    }

    pub fn key(text: &'a str, size: f32, color: [f32; 4], chip: [f32; 4]) -> Self {
        Self {
            text,
            face: Face::Mono,
            size,
            color,
            chip: Some(chip),
        }
    }
}

impl Fonts {
    pub fn load() -> Result<Self> {
        let sans = load_font(
            "TOMOE_FONT",
            "sans",
            &[
                "DejaVuSans.ttf",
                "NotoSans-Regular.ttf",
                "LiberationSans-Regular.ttf",
            ],
        )
        .context("no sans font found")?;
        let mono = load_font(
            "TOMOE_FONT_MONO",
            "monospace",
            &[
                "DejaVuSansMono.ttf",
                "NotoSansMono-Regular.ttf",
                "LiberationMono-Regular.ttf",
            ],
        )
        .unwrap_or_else(|_| sans.clone());
        Ok(Self {
            faces: [sans, mono],
        })
    }

    fn layout(&self, spans: &[Span]) -> Layout<usize> {
        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings::default());
        let fonts: Vec<&Font> = self.faces.iter().collect();
        for (i, span) in spans.iter().enumerate() {
            layout.append(
                &fonts,
                &TextStyle::with_user_data(span.text, span.size, span.face as usize, i),
            );
        }
        layout
    }

    /// Measure a single line of spans: (width, height) in pixels.
    pub fn measure(&self, spans: &[Span]) -> (i32, i32) {
        let layout = self.layout(spans);
        let width = layout
            .glyphs()
            .iter()
            .map(|g| g.x + g.width as f32)
            .fold(0.0f32, f32::max);
        (width.ceil() as i32, layout.height().ceil() as i32)
    }
}

/// Premultiplied-RGBA software canvas.
pub struct Canvas {
    pub width: i32,
    pub height: i32,
    data: Vec<u8>,
}

impl Canvas {
    pub fn new(width: i32, height: i32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Self {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
        }
    }

    pub fn fill(&mut self, color: [f32; 4]) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [f32; 4]) {
        let px = premul_bytes(color);
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.width), (y + h).min(self.height));
        for row in y0..y1 {
            for col in x0..x1 {
                let i = ((row * self.width + col) * 4) as usize;
                self.data[i..i + 4].copy_from_slice(&px);
            }
        }
    }

    pub fn border(&mut self, width: i32, color: [f32; 4]) {
        self.fill_rect(0, 0, self.width, width, color);
        self.fill_rect(0, self.height - width, self.width, width, color);
        self.fill_rect(0, 0, width, self.height, color);
        self.fill_rect(self.width - width, 0, width, self.height, color);
    }

    /// Draw one line of spans with its top-left corner at (x, y).
    pub fn draw_spans(&mut self, fonts: &Fonts, x: i32, y: i32, spans: &[Span]) {
        let layout = fonts.layout(spans);
        let line_height = layout.height().ceil() as i32;

        // Key chips first: one rect per span, spanning its glyph extents.
        for (i, span) in spans.iter().enumerate() {
            let Some(chip) = span.chip else { continue };
            let mut min_x = f32::MAX;
            let mut max_x = f32::MIN;
            for glyph in layout.glyphs().iter().filter(|g| g.user_data == i) {
                min_x = min_x.min(glyph.x);
                max_x = max_x.max(glyph.x + glyph.width as f32);
            }
            if min_x > max_x {
                continue;
            }
            let pad = (span.size * 0.25) as i32;
            self.fill_rect(
                x + min_x as i32 - pad,
                y,
                (max_x - min_x) as i32 + 2 * pad,
                line_height,
                chip,
            );
        }

        for glyph in layout.glyphs() {
            if glyph.width == 0 {
                continue;
            }
            let span = &spans[glyph.user_data];
            let font = &fonts.faces[span.face as usize];
            let (_, coverage) = font.rasterize_config(glyph.key);
            self.blend_glyph(
                x + glyph.x as i32,
                y + glyph.y as i32,
                glyph.width as i32,
                &coverage,
                span.color,
            );
        }
    }

    fn blend_glyph(&mut self, x: i32, y: i32, w: i32, coverage: &[u8], color: [f32; 4]) {
        for (i, cov) in coverage.iter().enumerate() {
            if *cov == 0 {
                continue;
            }
            let col = x + (i as i32 % w);
            let row = y + (i as i32 / w);
            if col < 0 || row < 0 || col >= self.width || row >= self.height {
                continue;
            }
            let a = *cov as f32 / 255.0;
            let idx = ((row * self.width + col) * 4) as usize;
            // Premultiplied src-over: dst = src*cov + dst*(1 - src.a*cov)
            let src = [color[0] * a, color[1] * a, color[2] * a, color[3] * a];
            for (c, s) in src.iter().enumerate() {
                let dst = self.data[idx + c] as f32 / 255.0;
                let out = s * color[3] + dst * (1.0 - color[3] * a);
                self.data[idx + c] = (out.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }

    /// Returns the buffer with its logical size (MemoryRenderBuffer doesn't
    /// expose one).
    pub fn into_buffer(self) -> (MemoryRenderBuffer, Size<i32, Physical>) {
        let buffer = MemoryRenderBuffer::from_slice(
            &self.data,
            Fourcc::Abgr8888,
            (self.width, self.height),
            1,
            Transform::Normal,
            None,
        );
        (buffer, Size::from((self.width, self.height)))
    }
}

fn premul_bytes(color: [f32; 4]) -> [u8; 4] {
    let a = color[3];
    [
        (color[0] * a * 255.0) as u8,
        (color[1] * a * 255.0) as u8,
        (color[2] * a * 255.0) as u8,
        (a * 255.0) as u8,
    ]
}

fn load_font(env: &str, fc_pattern: &str, file_names: &[&str]) -> Result<Font> {
    let path = font_path(env, fc_pattern, file_names)
        .ok_or_else(|| anyhow!("no font file for {fc_pattern:?}"))?;
    let data = std::fs::read(&path).with_context(|| format!("error reading {path:?}"))?;
    Font::from_bytes(data, FontSettings::default())
        .map_err(|err| anyhow!("error parsing font {path:?}: {err}"))
}

fn font_path(env: &str, fc_pattern: &str, file_names: &[&str]) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(env).map(PathBuf::from) {
        if path.exists() {
            return Some(path);
        }
    }

    // fontconfig knows the user's actual fonts; fall back to well-known dirs.
    if let Ok(out) = Command::new("fc-match")
        .args(["-f", "%{file}", fc_pattern])
        .output()
    {
        if out.status.success() {
            let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if path.exists() {
                return Some(path);
            }
        }
    }

    let dirs = [
        "/run/current-system/sw/share/X11/fonts",
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts/TTF",
        "/usr/share/fonts/noto",
    ];
    for dir in dirs {
        for name in file_names {
            let path = PathBuf::from(dir).join(name);
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}
