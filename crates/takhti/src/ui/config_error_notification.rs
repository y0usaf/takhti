//! Transient banner shown when (re)loading the config fails. The previous
//! config keeps running; this only informs. Auto-hides after a few seconds.

use std::time::{Duration, Instant};

use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Physical, Size};

use super::text::{Canvas, Fonts, Span};
use super::{BG, FG, RED};
use crate::render::{OutputRenderElements, TakhtiRenderer};

const PADDING: i32 = 16;
const BORDER: i32 = 4;
const FONT_SIZE: f32 = 17.0;
const MARGIN_TOP: i32 = 24;

pub struct ConfigErrorNotification {
    shown_at: Option<Instant>,
    message: String,
    buffer: Option<(MemoryRenderBuffer, Size<i32, Physical>)>,
}

impl ConfigErrorNotification {
    pub const TIMEOUT: Duration = Duration::from_secs(5);

    pub fn new() -> Self {
        Self {
            shown_at: None,
            message: String::new(),
            buffer: None,
        }
    }

    pub fn show(&mut self, message: &str) {
        if self.message != message {
            self.message = message.to_string();
            self.buffer = None;
        }
        self.shown_at = Some(Instant::now());
    }

    pub fn hide(&mut self) {
        self.shown_at = None;
    }

    pub fn render_elements<R: TakhtiRenderer>(
        &mut self,
        fonts: &Fonts,
        renderer: &mut R,
        output_size: Size<i32, Physical>,
        elements: &mut Vec<OutputRenderElements<R>>,
    ) {
        let Some(shown_at) = self.shown_at else {
            return;
        };
        if shown_at.elapsed() > Self::TIMEOUT {
            self.shown_at = None;
            return;
        }

        let message = &self.message;
        let (buffer, size) = self.buffer.get_or_insert_with(|| render(fonts, message));
        let size = *size;
        let loc = (
            ((output_size.w - size.w).max(0) / 2) as f64,
            MARGIN_TOP as f64,
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
    }
}

fn render(fonts: &Fonts, message: &str) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    let spans = [Span::sans(message, FONT_SIZE, FG)];
    let (w, h) = fonts.measure(&spans);
    let width = w + 2 * (PADDING + BORDER);
    let height = h + 2 * (PADDING + BORDER);
    let mut canvas = Canvas::new(width, height);
    canvas.fill(BG);
    canvas.border(BORDER, RED);
    canvas.draw_spans(fonts, BORDER + PADDING, BORDER + PADDING, &spans);
    canvas.into_buffer()
}
