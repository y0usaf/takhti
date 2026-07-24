//! Retained-widget registry: the `tomoe.ui` surface (doctrine 05 — one
//! declaration mechanism for compositor-drawn UI). Lua (or the core, for
//! builtins) declares a widget once; the core renders, damages, and routes
//! input to it, and only selection events re-enter Lua. The exit dialog,
//! hotkey overlay, and config-error banner are builtins on this same
//! registry; the screenshot overlay stays native (declared exemption) until
//! the API grows drag-region interaction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Physical, Point, Size};

use moonshell_render::element::{
    Align as MoonAlign, Edges, Flex, Style as MoonStyle, Text as TreeText,
};
use moonshell_render::{Element, Rgba};

use super::element_tree::{Engine, TreeTexture};
use super::text::{Canvas, Fonts, Span};
use super::{ACCENT, BACKDROP, BG, FG, KEY_CHIP, RED};
use crate::input::{Action, Bind};
use crate::render::{OutputRenderElements, TomoeRenderer};

const PADDING: i32 = 28;
const BORDER: i32 = 4;
const TITLE_SIZE: f32 = 24.0;
const ROW_SIZE: f32 = 17.0;
const ROW_GAP: i32 = 8;
const COLUMN_GAP: i32 = 20;
const CONFIRM_PADDING: i32 = 32;
const CONFIRM_BORDER: i32 = 8;
const CONFIRM_SIZE: f32 = 20.0;
const MENU_SIZE: f32 = 18.0;
const MENU_ROW_PAD: i32 = 5;
const TOAST_PADDING: i32 = 16;
const TOAST_SIZE: f32 = 17.0;
const TOAST_MARGIN_TOP: i32 = 24;
const TOAST_GAP: i32 = 8;

/// Widget ids are unique for the compositor session (not per Lua VM):
/// builtin widgets outlive config reloads, so a fresh VM must never mint an
/// id that collides with a surviving widget's.
pub fn alloc_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What a widget does when an event fires on it.
pub enum WidgetHandler {
    /// Purely informational (toasts, sheets).
    None,
    /// Dispatch a core action on confirm/select — how builtins hook in
    /// (e.g. the exit dialog confirms into `Action::ConfirmQuit`).
    Action(Action),
    /// Config-owned: events re-enter Lua. The callbacks live in the VM,
    /// keyed by widget id (`Shared.ui_callbacks`), never in core state.
    Lua,
}

/// Identities for compositor-owned widgets, so the core can toggle/replace
/// its own without tracking ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    ExitDialog,
    HotkeyOverlay,
    ConfigError,
}

/// Widget content. Confirm and Menu are modal (they own the keyboard and
/// swallow clicks); Sheet is dismissed by any input; Toast expires on its
/// own and ignores input entirely.
pub enum WidgetKind {
    /// Enter confirms, any other key or click cancels.
    Confirm { text: String },
    /// Up/Down (or k/j) or pointer hover navigate, Enter or a left click on
    /// a row selects, Esc or a click outside the menu cancels.
    Menu {
        title: Option<String>,
        items: Vec<String>,
        selected: usize,
    },
    /// Transient notification, auto-hides at `deadline`.
    Toast {
        text: String,
        deadline: Instant,
        urgent: bool,
    },
    /// Rows of (key chip, label) — the hotkey-overlay shape.
    Sheet {
        title: Option<String>,
        rows: Vec<(String, String)>,
    },
}

impl WidgetKind {
    pub fn modal(&self) -> bool {
        matches!(self, WidgetKind::Confirm { .. } | WidgetKind::Menu { .. })
    }

    /// Dismissed by any key press or click (that wasn't the one opening it).
    pub fn dismissable(&self) -> bool {
        matches!(self, WidgetKind::Sheet { .. })
    }
}

/// A widget declaration crossing the Lua boundary (actions out): durations
/// instead of deadlines, no core-side state.
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetSpec {
    Confirm {
        text: String,
    },
    Menu {
        title: Option<String>,
        items: Vec<String>,
    },
    Toast {
        text: String,
        duration: Duration,
        urgent: bool,
    },
    Sheet {
        title: Option<String>,
        rows: Vec<(String, String)>,
    },
}

/// An input event a widget fired. `Select` is 0-based (Lua callbacks get it
/// 1-based).
#[derive(Debug, Clone, Copy)]
pub enum UiEvent {
    Confirm,
    Cancel,
    Select(usize),
}

pub struct WidgetEntry {
    pub id: u64,
    pub kind: WidgetKind,
    pub handler: WidgetHandler,
    pub tag: Option<Tag>,
    /// Cached rendering; dropped when content changes (menu navigation).
    buffer: Option<(MemoryRenderBuffer, Size<i32, Physical>)>,
    /// Element-engine texture for tree-backed kinds (Confirm, FUSION F1).
    /// Never invalidated by hand: the scene cache diffs trees itself.
    tree: Option<TreeTexture>,
    /// Cached menu geometry for pointer hit-testing. Unlike `buffer` it
    /// survives `invalidate`: selection changes recolor rows, never move
    /// them, so hit tests don't wait on a repaint.
    layout: Option<MenuLayout>,
}

impl WidgetEntry {
    pub fn new(id: u64, kind: WidgetKind, handler: WidgetHandler, tag: Option<Tag>) -> Self {
        Self {
            id,
            kind,
            handler,
            tag,
            buffer: None,
            tree: None,
            layout: None,
        }
    }

    /// Drop the cached rendering (content changed).
    pub fn invalidate(&mut self) {
        self.buffer = None;
    }

    /// Hit-test an output-local point against this widget, if it is a menu
    /// (menus render centered on the output): `Some(Some(i))` = over row i,
    /// `Some(None)` = over the widget but not a row, `None` = outside it or
    /// not a menu.
    fn menu_hit(
        &mut self,
        fonts: &Fonts,
        output_size: Size<i32, Physical>,
        point: Point<f64, Physical>,
    ) -> Option<Option<usize>> {
        let WidgetKind::Menu { title, items, .. } = &self.kind else {
            return None;
        };
        let layout = self
            .layout
            .get_or_insert_with(|| menu_layout(fonts, title.as_deref(), items));
        layout.hit(output_size, point)
    }
}

/// Menu geometry shared by rendering and pointer hit-testing: the widget
/// size plus each row's (y, height) in buffer-local coordinates.
struct MenuLayout {
    size: Size<i32, Physical>,
    title_w: i32,
    rows: Vec<(i32, i32)>,
}

impl MenuLayout {
    /// Hit-test an output-local point against this menu, centered on an
    /// output of `output_size` (the render placement). Return shape as in
    /// `WidgetEntry::menu_hit`.
    fn hit(
        &self,
        output_size: Size<i32, Physical>,
        point: Point<f64, Physical>,
    ) -> Option<Option<usize>> {
        let x = point.x - ((output_size.w - self.size.w).max(0) / 2) as f64;
        let y = point.y - ((output_size.h - self.size.h).max(0) / 2) as f64;
        if x < 0.0 || y < 0.0 || x >= self.size.w as f64 || y >= self.size.h as f64 {
            return None;
        }
        // Rows span the full inner width, like their highlight bar.
        let row = (x >= BORDER as f64 && x < (self.size.w - BORDER) as f64)
            .then(|| {
                self.rows
                    .iter()
                    .position(|&(ry, rh)| y >= ry as f64 && y < (ry + rh) as f64)
            })
            .flatten();
        Some(row)
    }
}

fn menu_layout(fonts: &Fonts, title: Option<&str>, items: &[String]) -> MenuLayout {
    let (title_w, title_h) = title
        .map(|t| fonts.measure(&[Span::sans(t, TITLE_SIZE, FG)]))
        .unwrap_or((0, 0));
    let title_gap = if title.is_some() {
        (TITLE_SIZE * 0.8) as i32
    } else {
        0
    };

    let mut item_w = 0;
    let mut text_h = 0;
    for item in items {
        let (w, h) = fonts.measure(&[Span::sans(item, MENU_SIZE, FG)]);
        item_w = item_w.max(w);
        text_h = text_h.max(h);
    }
    let row_h = text_h + 2 * MENU_ROW_PAD;

    let content_w = title_w.max(item_w);
    let content_h = title_h + title_gap + items.len() as i32 * (row_h + ROW_GAP)
        - if items.is_empty() { 0 } else { ROW_GAP };
    let width = content_w + 2 * (PADDING + BORDER);
    let height = content_h + 2 * (PADDING + BORDER);

    let mut y = BORDER + PADDING + title_h + title_gap;
    let rows = items
        .iter()
        .map(|_| {
            let row = (y, row_h);
            y += row_h + ROW_GAP;
            row
        })
        .collect();
    MenuLayout {
        size: Size::from((width, height)),
        title_w,
        rows,
    }
}

/// The registry. Insertion order is stacking order: the last entry renders
/// topmost, and the topmost modal entry owns the keyboard.
#[derive(Default)]
pub struct Widgets {
    entries: Vec<WidgetEntry>,
    backdrop: SolidColorBuffer,
}

impl Widgets {
    pub fn open(&mut self, entry: WidgetEntry) {
        self.entries.push(entry);
    }

    pub fn close(&mut self, id: u64) -> Option<WidgetEntry> {
        let i = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(i))
    }

    pub fn close_tag(&mut self, tag: Tag) -> bool {
        let Some(i) = self.entries.iter().position(|e| e.tag == Some(tag)) else {
            return false;
        };
        self.entries.remove(i);
        true
    }

    pub fn tag_open(&self, tag: Tag) -> bool {
        self.entries.iter().any(|e| e.tag == Some(tag))
    }

    pub fn top_modal_mut(&mut self) -> Option<&mut WidgetEntry> {
        self.entries.iter_mut().rev().find(|e| e.kind.modal())
    }

    pub fn top_modal_id(&self) -> Option<u64> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.kind.modal())
            .map(|e| e.id)
    }

    pub fn dismissable_ids(&self) -> Vec<u64> {
        self.entries
            .iter()
            .filter(|e| e.kind.dismissable())
            .map(|e| e.id)
            .collect()
    }

    /// Close everything (session lock). Callers drop the Lua callbacks of
    /// the returned entries.
    pub fn drain(&mut self) -> Vec<WidgetEntry> {
        std::mem::take(&mut self.entries)
    }

    /// Close every Lua-owned widget: their callbacks died with the VM
    /// (config reload). Builtins (Action/None handlers) survive.
    pub fn close_lua(&mut self) {
        self.entries
            .retain(|e| !matches!(e.handler, WidgetHandler::Lua));
    }

    /// Pointer hover over the topmost modal menu: move the selection to the
    /// row under `point` (output-local). Returns true when it changed (the
    /// caller queues a redraw); off-row motion keeps the last selection.
    pub fn hover_menu(
        &mut self,
        fonts: &Fonts,
        output_size: Size<i32, Physical>,
        point: Point<f64, Physical>,
    ) -> bool {
        let Some(entry) = self.entries.iter_mut().rev().find(|e| e.kind.modal()) else {
            return false;
        };
        let Some(Some(row)) = entry.menu_hit(fonts, output_size, point) else {
            return false;
        };
        let WidgetKind::Menu { selected, .. } = &mut entry.kind else {
            return false;
        };
        if *selected == row {
            return false;
        }
        *selected = row;
        entry.invalidate();
        true
    }

    /// Hit-test a click against the topmost modal widget when it is a menu
    /// (return shape as in `WidgetEntry::menu_hit`).
    pub fn menu_click(
        &mut self,
        fonts: &Fonts,
        output_size: Size<i32, Physical>,
        point: Point<f64, Physical>,
    ) -> Option<Option<usize>> {
        self.entries
            .iter_mut()
            .rev()
            .find(|e| e.kind.modal())?
            .menu_hit(fonts, output_size, point)
    }

    /// Build render elements, topmost first (earlier elements draw on top).
    /// Expired toasts are culled here — they carry no callbacks, so
    /// render-time removal needs no Lua cleanup; a timer scheduled at open
    /// queues the repaint that reaches this cull.
    pub fn render_elements<R: TomoeRenderer>(
        &mut self,
        fonts: &Fonts,
        engine: &mut Engine,
        renderer: &mut R,
        output_size: Size<i32, Physical>,
        scale: f32,
        elements: &mut Vec<OutputRenderElements<R>>,
    ) {
        let now = Instant::now();
        self.entries.retain(|e| match &e.kind {
            WidgetKind::Toast { deadline, .. } => *deadline > now,
            _ => true,
        });
        let top_modal = self.entries.iter().rposition(|e| e.kind.modal());
        let mut toast_y = TOAST_MARGIN_TOP;
        for i in (0..self.entries.len()).rev() {
            let entry = &mut self.entries[i];
            let (buffer, size) = match &entry.kind {
                // Tree-backed kinds raster through the element engine
                // at the output's (fractional) scale; the scene cache
                // makes the unchanged case free and small changes
                // upload only their diffed rects (FUSION F1).
                WidgetKind::Confirm { text } => {
                    let root = confirm_tree(text);
                    let size = engine.measure(&root, scale);
                    let tex = entry.tree.get_or_insert_with(TreeTexture::new);
                    tex.update(engine, &root, size, scale);
                    (tex.buffer(), size)
                }
                _ => {
                    if entry.buffer.is_none() {
                        entry.buffer = Some(render_widget(fonts, &entry.kind));
                    }
                    let Some((buffer, size)) = &entry.buffer else {
                        continue;
                    };
                    (buffer, *size)
                }
            };
            let loc = match entry.kind {
                WidgetKind::Toast { .. } => {
                    let loc = (((output_size.w - size.w).max(0) / 2) as f64, toast_y as f64);
                    toast_y += size.h + TOAST_GAP;
                    loc
                }
                _ => (
                    ((output_size.w - size.w).max(0) / 2) as f64,
                    ((output_size.h - size.h).max(0) / 2) as f64,
                ),
            };
            let alpha = matches!(entry.kind, WidgetKind::Sheet { .. }).then_some(0.95);
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                loc,
                buffer,
                alpha,
                None,
                None,
                Kind::Unspecified,
            ) {
                elements.push(OutputRenderElements::Memory(element));
            }
            // Dim everything beneath the topmost modal widget.
            if Some(i) == top_modal {
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
    }
}

fn render_widget(fonts: &Fonts, kind: &WidgetKind) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    match kind {
        // Confirm rasters through the element engine in
        // `render_elements` (FUSION F1); it never reaches the Canvas path.
        WidgetKind::Confirm { .. } => {
            unreachable!("confirm renders through the element engine")
        }
        WidgetKind::Menu {
            title,
            items,
            selected,
        } => render_menu(fonts, title.as_deref(), items, *selected),
        WidgetKind::Toast { text, urgent, .. } => render_toast(fonts, text, *urgent),
        WidgetKind::Sheet { title, rows } => render_sheet(fonts, title.as_deref(), rows),
    }
}

/// The confirm dialog as an element tree (FUSION F1's proof-of-fusion):
/// the border is a padded outer box, the key chip a text background —
/// all logical px, resolved to integer physical pixels in layout.
fn confirm_tree(text: &str) -> Element {
    let chip_text = |content: &str, chip: Option<[f32; 4]>| {
        Element::Text(TreeText {
            style: MoonStyle {
                bg: chip.map(rgba),
                ..Default::default()
            },
            content: content.into(),
            size: CONFIRM_SIZE,
            color: rgba(FG),
            ..Default::default()
        })
    };
    let line2 = Element::HBox(Flex {
        align: MoonAlign::Center,
        children: vec![
            chip_text("Press ", None),
            chip_text(" Enter ", Some(KEY_CHIP)),
            chip_text(" to confirm.", None),
        ],
        ..Default::default()
    });
    Element::VBox(Flex {
        style: MoonStyle {
            bg: Some(rgba(RED)),
            ..Default::default()
        },
        padding: Edges::all(CONFIRM_BORDER as f32),
        children: vec![Element::VBox(Flex {
            style: MoonStyle {
                bg: Some(rgba(BG)),
                ..Default::default()
            },
            padding: Edges::all(CONFIRM_PADDING as f32),
            gap: CONFIRM_SIZE * 0.6,
            align: MoonAlign::Center,
            children: vec![chip_text(text, None), line2],
            ..Default::default()
        })],
        ..Default::default()
    })
}

/// Palette color → element-engine color. The palette's straight and
/// premultiplied forms coincide (every tree color is alpha 1.0).
fn rgba(c: [f32; 4]) -> Rgba {
    Rgba::new(
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8,
        (c[3] * 255.0) as u8,
    )
}

fn render_menu(
    fonts: &Fonts,
    title: Option<&str>,
    items: &[String],
    selected: usize,
) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    let layout = menu_layout(fonts, title, items);
    let (width, height) = (layout.size.w, layout.size.h);
    let mut canvas = Canvas::new(width, height);
    canvas.fill(BG);
    canvas.border(BORDER, ACCENT);

    if let Some(title) = title {
        canvas.draw_spans(
            fonts,
            (width - layout.title_w) / 2,
            BORDER + PADDING,
            &[Span::sans(title, TITLE_SIZE, FG)],
        );
    }
    for (i, item) in items.iter().enumerate() {
        let (y, row_h) = layout.rows[i];
        // Highlight bar across the full inner width; dark text on accent.
        let color = if i == selected {
            canvas.fill_rect(BORDER, y, width - 2 * BORDER, row_h, ACCENT);
            BG
        } else {
            FG
        };
        canvas.draw_spans(
            fonts,
            BORDER + PADDING,
            y + MENU_ROW_PAD,
            &[Span::sans(item, MENU_SIZE, color)],
        );
    }
    canvas.into_buffer()
}

fn render_toast(
    fonts: &Fonts,
    text: &str,
    urgent: bool,
) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    let spans = [Span::sans(text, TOAST_SIZE, FG)];
    let (w, h) = fonts.measure(&spans);
    let width = w + 2 * (TOAST_PADDING + BORDER);
    let height = h + 2 * (TOAST_PADDING + BORDER);
    let mut canvas = Canvas::new(width, height);
    canvas.fill(BG);
    canvas.border(BORDER, if urgent { RED } else { ACCENT });
    canvas.draw_spans(
        fonts,
        BORDER + TOAST_PADDING,
        BORDER + TOAST_PADDING,
        &spans,
    );
    canvas.into_buffer()
}

fn render_sheet(
    fonts: &Fonts,
    title: Option<&str>,
    rows: &[(String, String)],
) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    let (title_w, title_h) = title
        .map(|t| fonts.measure(&[Span::sans(t, TITLE_SIZE, FG)]))
        .unwrap_or((0, 0));
    let title_gap = if title.is_some() {
        (TITLE_SIZE * 0.8) as i32
    } else {
        0
    };

    let mut key_w = 0;
    let mut label_w = 0;
    let mut row_h = 0;
    for (key, label) in rows {
        let (kw, kh) = fonts.measure(&[Span::key(key, ROW_SIZE, FG, KEY_CHIP)]);
        let (lw, lh) = fonts.measure(&[Span::sans(label, ROW_SIZE, FG)]);
        key_w = key_w.max(kw);
        label_w = label_w.max(lw);
        row_h = row_h.max(kh.max(lh));
    }

    let content_w = title_w.max(key_w + COLUMN_GAP + label_w);
    let content_h = title_h + title_gap + rows.len() as i32 * (row_h + ROW_GAP)
        - if rows.is_empty() { 0 } else { ROW_GAP };
    let width = content_w + 2 * (PADDING + BORDER);
    let height = content_h + 2 * (PADDING + BORDER);

    let mut canvas = Canvas::new(width, height);
    canvas.fill(BG);
    canvas.border(BORDER, ACCENT);

    let x0 = BORDER + PADDING;
    let mut y = BORDER + PADDING;
    if let Some(title) = title {
        canvas.draw_spans(
            fonts,
            (width - title_w) / 2,
            y,
            &[Span::sans(title, TITLE_SIZE, FG)],
        );
        y += title_h + title_gap;
    }
    for (key, label) in rows {
        let (kw, _) = fonts.measure(&[Span::key(key, ROW_SIZE, FG, KEY_CHIP)]);
        // Right-align keys against the column edge.
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
            &[Span::sans(label, ROW_SIZE, FG)],
        );
        y += row_h + ROW_GAP;
    }
    canvas.into_buffer()
}

// ─── Hotkey-overlay content (builtin sheet) ──────────────────────────────────

/// Build the hotkey-overlay rows from the live bind list.
pub fn hotkey_rows(binds: &[Bind]) -> Vec<(String, String)> {
    binds
        .iter()
        .filter_map(|bind| {
            Some((
                format!(" {} ", pretty_combo(&bind.combo)),
                bind_title(bind)?,
            ))
        })
        .collect()
}

/// Human-readable label for a bind's row, or None to omit it from the overlay.
fn bind_title(bind: &Bind) -> Option<String> {
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
        Action::Screenshot => Some("Take a Screenshot".to_string()),
        Action::ScreenshotScreen => Some("Screenshot the Screen".to_string()),
        // Lua functions are opaque; configs label them via the third
        // `tomoe.bind` argument.
        Action::LuaFn(_)
        | Action::ChangeVt(_)
        | Action::ScreenshotConfirm
        | Action::UiEvent(..) => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 100×100 menu with two 20-tall rows at y=40 and y=70, centered on a
    /// 300×300 output (so the widget occupies 100..200 on both axes).
    fn layout() -> (MenuLayout, Size<i32, Physical>) {
        let layout = MenuLayout {
            size: Size::from((100, 100)),
            title_w: 0,
            rows: vec![(40, 20), (70, 20)],
        };
        (layout, Size::from((300, 300)))
    }

    #[test]
    fn menu_hit_outside() {
        let (layout, out) = layout();
        assert!(layout.hit(out, Point::from((50.0, 150.0))).is_none());
        assert!(layout.hit(out, Point::from((150.0, 250.0))).is_none());
        // Exactly on the far edge is outside (half-open rect).
        assert!(layout.hit(out, Point::from((200.0, 150.0))).is_none());
    }

    #[test]
    fn menu_hit_inside_but_not_a_row() {
        let (layout, out) = layout();
        // Padding above the first row.
        assert_eq!(layout.hit(out, Point::from((150.0, 120.0))), Some(None));
        // Gap between the rows (row 0 ends at local y=60, row 1 starts at 70).
        assert_eq!(layout.hit(out, Point::from((150.0, 165.0))), Some(None));
        // Row height, but inside the left border (rows start at x=BORDER).
        assert_eq!(layout.hit(out, Point::from((101.0, 150.0))), Some(None));
    }

    #[test]
    fn menu_hit_rows() {
        let (layout, out) = layout();
        assert_eq!(layout.hit(out, Point::from((150.0, 145.0))), Some(Some(0)));
        assert_eq!(layout.hit(out, Point::from((150.0, 175.0))), Some(Some(1)));
        // Rows span the full inner width, like their highlight bar.
        assert_eq!(
            layout.hit(out, Point::from((BORDER as f64 + 100.0, 175.0))),
            Some(Some(1))
        );
    }
}
