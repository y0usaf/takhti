//! Keybind cheat-sheet overlay (niri's "Important Hotkeys"), built from the
//! live bind list. Non-modal: any input dismisses it.

use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Physical, Size};

use super::text::{Canvas, Fonts, Span};
use super::{ACCENT, BG, FG, KEY_CHIP};
use crate::input::{Action, Bind};
use crate::render::{OutputRenderElements, TakhtiRenderer};

const PADDING: i32 = 28;
const BORDER: i32 = 4;
const TITLE_SIZE: f32 = 24.0;
const ROW_SIZE: f32 = 17.0;
const ROW_GAP: i32 = 8;
const COLUMN_GAP: i32 = 20;

pub struct HotkeyOverlay {
    open: bool,
    buffer: Option<(MemoryRenderBuffer, Size<i32, Physical>)>,
}

impl HotkeyOverlay {
    pub fn new() -> Self {
        Self {
            open: false,
            buffer: None,
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

    /// Drop the cached rendering (binds changed, e.g. config reload).
    pub fn invalidate(&mut self) {
        self.buffer = None;
    }

    pub fn render_elements<R: TakhtiRenderer>(
        &mut self,
        fonts: &Fonts,
        renderer: &mut R,
        output_size: Size<i32, Physical>,
        binds: &[Bind],
        elements: &mut Vec<OutputRenderElements<R>>,
    ) {
        if !self.open {
            return;
        }

        let (buffer, size) = self.buffer.get_or_insert_with(|| render(fonts, binds));
        let loc = (
            ((output_size.w - size.w).max(0) / 2) as f64,
            ((output_size.h - size.h).max(0) / 2) as f64,
        );
        if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc,
            buffer,
            Some(0.95),
            None,
            None,
            Kind::Unspecified,
        ) {
            elements.push(OutputRenderElements::Memory(element));
        }
    }
}

/// Human-readable title for a bind's row, or None to omit it from the overlay.
fn title(bind: &Bind) -> Option<String> {
    if let Some(desc) = &bind.desc {
        return Some(desc.clone());
    }
    match &bind.action {
        Action::Quit => Some("Exit".to_string()),
        Action::ConfirmQuit => Some("Exit Immediately".to_string()),
        Action::CloseWindow => Some("Close Window".to_string()),
        Action::Spawn(cmd) => Some(format!("Spawn {cmd}")),
        Action::ShowHotkeyOverlay => Some("Show Important Hotkeys".to_string()),
        Action::ReloadConfig => Some("Reload the Config File".to_string()),
        // Lua functions are opaque; configs label them via the third
        // `takhti.bind` argument.
        Action::LuaFn(_) | Action::ChangeVt(_) => None,
    }
}

/// "Super+Shift+slash" -> "Super + Shift + /".
fn pretty_combo(combo: &str) -> String {
    combo
        .split('+')
        .map(|part| pretty_key(part.trim()))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn pretty_key(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "return" => "Enter".to_string(),
        "slash" => "/".to_string(),
        "comma" => ",".to_string(),
        "period" => ".".to_string(),
        "space" => "Space".to_string(),
        "minus" => "-".to_string(),
        "equal" => "=".to_string(),
        _ => {
            let mut chars = key.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

fn render(fonts: &Fonts, binds: &[Bind]) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    let rows: Vec<(String, String)> = binds
        .iter()
        .filter_map(|bind| Some((format!(" {} ", pretty_combo(&bind.combo)), title(bind)?)))
        .collect();

    let title_spans = [Span::sans("Important Hotkeys", TITLE_SIZE, FG)];
    let (title_w, title_h) = fonts.measure(&title_spans);

    let mut key_w = 0;
    let mut action_w = 0;
    let mut row_h = 0;
    for (key, action) in &rows {
        let (kw, kh) = fonts.measure(&[Span::key(key, ROW_SIZE, FG, KEY_CHIP)]);
        let (aw, ah) = fonts.measure(&[Span::sans(action, ROW_SIZE, FG)]);
        key_w = key_w.max(kw);
        action_w = action_w.max(aw);
        row_h = row_h.max(kh.max(ah));
    }

    let content_w = title_w.max(key_w + COLUMN_GAP + action_w);
    let title_gap = (TITLE_SIZE * 0.8) as i32;
    let content_h = title_h + title_gap + rows.len() as i32 * (row_h + ROW_GAP) - ROW_GAP;
    let width = content_w + 2 * (PADDING + BORDER);
    let height = content_h + 2 * (PADDING + BORDER);

    let mut canvas = Canvas::new(width, height);
    canvas.fill(BG);
    canvas.border(BORDER, ACCENT);

    let x0 = BORDER + PADDING;
    canvas.draw_spans(fonts, (width - title_w) / 2, BORDER + PADDING, &title_spans);
    let mut y = BORDER + PADDING + title_h + title_gap;
    for (key, action) in &rows {
        let (kw, _) = fonts.measure(&[Span::key(key, ROW_SIZE, FG, KEY_CHIP)]);
        // Right-align keys against the column edge, niri-style.
        canvas.draw_spans(
            fonts,
            x0 + key_w - kw,
            y,
            &[Span::key(key, ROW_SIZE, FG, KEY_CHIP)],
        );
        canvas.draw_spans(
            fonts,
            x0 + key_w + COLUMN_GAP,
            y,
            &[Span::sans(action, ROW_SIZE, FG)],
        );
        y += row_h + ROW_GAP;
    }
    canvas.into_buffer()
}
