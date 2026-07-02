//! Modal exit confirmation, drawn by the compositor (niri-style).
//!
//! While open, all keyboard input is intercepted in `input.rs`: Enter
//! confirms, any other key or click dismisses.

use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Physical, Size};

use super::text::{Fonts, Span};
use super::{BACKDROP, BG, FG, KEY_CHIP, RED};
use crate::render::{OutputRenderElements, TakhtiRenderer};

const PADDING: i32 = 32;
const BORDER: i32 = 8;
const FONT_SIZE: f32 = 20.0;

pub struct ExitConfirmDialog {
    open: bool,
    buffer: Option<(MemoryRenderBuffer, Size<i32, Physical>)>,
    backdrop: SolidColorBuffer,
}

impl ExitConfirmDialog {
    pub fn new() -> Self {
        Self {
            open: false,
            buffer: None,
            backdrop: SolidColorBuffer::default(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn show(&mut self) {
        self.open = true;
    }

    pub fn hide(&mut self) {
        self.open = false;
    }

    pub fn render_elements<R: TakhtiRenderer>(
        &mut self,
        fonts: &Fonts,
        renderer: &mut R,
        output_size: Size<i32, Physical>,
        elements: &mut Vec<OutputRenderElements<R>>,
    ) {
        if !self.open {
            return;
        }

        let (buffer, size) = self.buffer.get_or_insert_with(|| render(fonts));
        let size = *size;
        let loc = (
            ((output_size.w - size.w).max(0) / 2) as f64,
            ((output_size.h - size.h).max(0) / 2) as f64,
        );
        if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc,
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ) {
            elements.push(OutputRenderElements::Memory(element));
        }

        // SolidColorBuffer sizes are Logical-typed in smithay; ours are pixel
        // sizes rendered at scale 1.0, so the retype is a no-op.
        self.backdrop
            .update((output_size.w, output_size.h), BACKDROP);
        elements.push(OutputRenderElements::Solid(
            SolidColorRenderElement::from_buffer(
                &self.backdrop,
                (0, 0),
                1.0,
                1.0,
                Kind::Unspecified,
            ),
        ));
    }
}

fn render(fonts: &Fonts) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    let line1 = [Span::sans(
        "Are you sure you want to exit takhti?",
        FONT_SIZE,
        FG,
    )];
    let line2 = [
        Span::sans("Press ", FONT_SIZE, FG),
        Span::key(" Enter ", FONT_SIZE, FG, KEY_CHIP),
        Span::sans(" to confirm.", FONT_SIZE, FG),
    ];
    let (w1, h1) = fonts.measure(&line1);
    let (w2, h2) = fonts.measure(&line2);
    let gap = (FONT_SIZE * 0.6) as i32;

    let width = w1.max(w2) + 2 * (PADDING + BORDER);
    let height = h1 + gap + h2 + 2 * (PADDING + BORDER);
    let mut canvas = super::text::Canvas::new(width, height);
    canvas.fill(BG);
    canvas.border(BORDER, RED);
    canvas.draw_spans(fonts, (width - w1) / 2, BORDER + PADDING, &line1);
    canvas.draw_spans(fonts, (width - w2) / 2, BORDER + PADDING + h1 + gap, &line2);
    canvas.into_buffer()
}
