//! Interactive region-screenshot overlay.
//!
//! While open, `input.rs` gives it the pointer and keyboard: left-drag
//! selects a region, Enter/Space captures (the whole output when nothing is
//! selected), Esc cancels. The overlay dims everything but the selection and
//! shows a hint bar near the bottom of its output. Capture paths omit the
//! controls while retaining the optional frozen scene beneath them.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::output::Output;
use smithay::utils::{Physical, Point, Rectangle, Size};

use super::text::{Canvas, Fonts, Span};
use super::{ACCENT, BACKDROP, BG, FG, KEY_CHIP};
use crate::render::{OutputRenderElements, TomoeRenderer};

const PADDING: i32 = 12;
const BORDER: i32 = 2;
const FONT_SIZE: f32 = 17.0;
/// Gap between the hint bar and the bottom output edge.
const HINT_MARGIN: i32 = 32;
/// Selection border thickness.
const SELECTION_BORDER: i32 = 2;
/// Selections thinner than this in either dimension count as accidental
/// clicks and are discarded.
const MIN_SELECTION: i32 = 2;

enum State {
    Closed,
    Open {
        output: Output,
        /// Drag anchor and current corner, output-local physical pixels.
        /// Unordered: normalize via [`selection_to_rect`] before use.
        selection: Option<(Point<i32, Physical>, Point<i32, Physical>)>,
        dragging: bool,
        /// Scene captured when the UI opened. Rendered beneath the selection
        /// overlay so client updates cannot move while the user selects.
        frozen: Option<MemoryRenderBuffer>,
    },
}

pub struct ScreenshotUi {
    state: State,
    /// Dimming rects: the whole output, or the four sides around the
    /// selection hole.
    backdrop: [SolidColorBuffer; 4],
    /// The four edges of the selection border.
    border: [SolidColorBuffer; 4],
    /// Cached hint bar, keyed by whether a selection exists.
    hint: Option<(bool, MemoryRenderBuffer, Size<i32, Physical>)>,
}

impl ScreenshotUi {
    pub fn new() -> Self {
        Self {
            state: State::Closed,
            backdrop: Default::default(),
            border: Default::default(),
            hint: None,
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self.state, State::Open { .. })
    }

    /// Open (or re-target) the overlay on `output` with no selection. `frozen`
    /// is a full-output, tightly packed RGBA8 scene snapshot.
    pub fn open(&mut self, output: Output, frozen: Option<(Size<i32, Physical>, Vec<u8>)>) {
        let frozen = frozen.map(|(size, pixels)| {
            MemoryRenderBuffer::from_slice(
                &pixels,
                Fourcc::Abgr8888,
                (size.w, size.h),
                1,
                smithay::utils::Transform::Normal,
                None,
            )
        });
        self.state = State::Open {
            output,
            selection: None,
            dragging: false,
            frozen,
        };
    }

    pub fn close(&mut self) {
        self.state = State::Closed;
    }

    /// The output the overlay is shown on.
    pub fn output(&self) -> Option<&Output> {
        match &self.state {
            State::Open { output, .. } => Some(output),
            State::Closed => None,
        }
    }

    /// Push only the frozen scene, without selection controls. Capture paths
    /// use this so confirming returns exactly the frame the user selected.
    pub fn render_frozen<R: TomoeRenderer>(
        &self,
        renderer: &mut R,
        output: &Output,
        elements: &mut Vec<OutputRenderElements<R>>,
    ) {
        let State::Open {
            output: target,
            frozen: Some(frozen),
            ..
        } = &self.state
        else {
            return;
        };
        if target != output {
            return;
        }
        push_frozen(renderer, frozen, elements);
    }

    pub fn is_dragging(&self) -> bool {
        matches!(self.state, State::Open { dragging: true, .. })
    }

    /// Left button pressed: start a fresh selection at `pos` (output-local
    /// physical).
    pub fn begin_drag(&mut self, pos: Point<i32, Physical>) {
        if let State::Open {
            selection,
            dragging,
            ..
        } = &mut self.state
        {
            *selection = Some((pos, pos));
            *dragging = true;
        }
    }

    /// Pointer moved during a drag: track the selection's moving corner.
    pub fn drag_to(&mut self, pos: Point<i32, Physical>) {
        if let State::Open {
            selection: Some((_, end)),
            dragging: true,
            ..
        } = &mut self.state
        {
            *end = pos;
        }
    }

    /// Left button released: keep the selection unless it is degenerate
    /// (an accidental click rather than a drag).
    pub fn end_drag(&mut self) {
        if let State::Open {
            selection,
            dragging,
            ..
        } = &mut self.state
        {
            *dragging = false;
            if let Some((a, b)) = *selection {
                if (a.x - b.x).abs() < MIN_SELECTION || (a.y - b.y).abs() < MIN_SELECTION {
                    *selection = None;
                }
            }
        }
    }

    /// The selection as a normalized rect clamped to an output of size
    /// `bounds`, or None when there is no (usable) selection — callers then
    /// capture the whole output.
    pub fn selection_rect(&self, bounds: Size<i32, Physical>) -> Option<Rectangle<i32, Physical>> {
        let State::Open {
            selection: Some(selection),
            ..
        } = &self.state
        else {
            return None;
        };
        let rect = selection_to_rect(*selection, bounds);
        (rect.size.w >= MIN_SELECTION && rect.size.h >= MIN_SELECTION).then_some(rect)
    }

    /// Push the overlay's elements (topmost first) when open on `output`.
    /// The solid rects render without fonts; only the hint bar needs them.
    pub fn render_elements<R: TomoeRenderer>(
        &mut self,
        fonts: Option<&Fonts>,
        renderer: &mut R,
        output: &Output,
        output_size: Size<i32, Physical>,
        elements: &mut Vec<OutputRenderElements<R>>,
    ) {
        let State::Open {
            output: target,
            selection,
            frozen,
            ..
        } = &self.state
        else {
            return;
        };
        if target != output {
            return;
        }
        let selection = *selection;

        // Hint bar, centered near the bottom edge.
        if let Some(fonts) = fonts {
            let has_selection = selection.is_some();
            if !matches!(&self.hint, Some((variant, ..)) if *variant == has_selection) {
                let (buffer, size) = render_hint(fonts, has_selection);
                self.hint = Some((has_selection, buffer, size));
            }
            let (_, buffer, size) = self.hint.as_ref().unwrap();
            let loc = (
                ((output_size.w - size.w).max(0) / 2) as f64,
                (output_size.h - size.h - HINT_MARGIN).max(0) as f64,
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

        if let Some(selection) = selection {
            let rect = selection_to_rect(selection, output_size);
            let (x0, y0) = (rect.loc.x, rect.loc.y);
            let (x1, y1) = (x0 + rect.size.w, y0 + rect.size.h);

            // Accent border hugging the selection from the outside, clamped to
            // the output.
            let bx0 = (x0 - SELECTION_BORDER).max(0);
            let by0 = (y0 - SELECTION_BORDER).max(0);
            let bx1 = (x1 + SELECTION_BORDER).min(output_size.w);
            let by1 = (y1 + SELECTION_BORDER).min(output_size.h);
            let edges = [
                Rectangle::new(Point::from((bx0, by0)), Size::from((bx1 - bx0, y0 - by0))),
                Rectangle::new(Point::from((bx0, y1)), Size::from((bx1 - bx0, by1 - y1))),
                Rectangle::new(Point::from((bx0, y0)), Size::from((x0 - bx0, y1 - y0))),
                Rectangle::new(Point::from((x1, y0)), Size::from((bx1 - x1, y1 - y0))),
            ];
            for (buffer, edge) in self.border.iter_mut().zip(edges) {
                push_solid(elements, buffer, edge, ACCENT);
            }

            // Dim everything around the selection hole: bands above and below
            // spanning the full width, strips left and right of the selection.
            let sides = [
                Rectangle::new(Point::from((0, 0)), Size::from((output_size.w, y0))),
                Rectangle::new(
                    Point::from((0, y1)),
                    Size::from((output_size.w, output_size.h - y1)),
                ),
                Rectangle::new(Point::from((0, y0)), Size::from((x0, y1 - y0))),
                Rectangle::new(
                    Point::from((x1, y0)),
                    Size::from((output_size.w - x1, y1 - y0)),
                ),
            ];
            for (buffer, side) in self.backdrop.iter_mut().zip(sides) {
                push_solid(elements, buffer, side, BACKDROP);
            }
        } else {
            // Nothing selected yet: dim the whole output.
            push_solid(
                elements,
                &mut self.backdrop[0],
                Rectangle::from_size(output_size),
                BACKDROP,
            );
        }

        // Elements are topmost-first: the frozen scene follows the overlay and
        // covers all live session elements beneath it.
        if let Some(frozen) = frozen {
            push_frozen(renderer, frozen, elements);
        }
    }
}

fn push_frozen<R: TomoeRenderer>(
    renderer: &mut R,
    frozen: &MemoryRenderBuffer,
    elements: &mut Vec<OutputRenderElements<R>>,
) {
    if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        (0., 0.),
        frozen,
        None,
        None,
        None,
        Kind::Unspecified,
    ) {
        elements.push(OutputRenderElements::Memory(element));
    }
}

/// Normalize an unordered selection into a rect clamped to `bounds`.
fn selection_to_rect(
    (a, b): (Point<i32, Physical>, Point<i32, Physical>),
    bounds: Size<i32, Physical>,
) -> Rectangle<i32, Physical> {
    let x0 = a.x.min(b.x).clamp(0, bounds.w);
    let y0 = a.y.min(b.y).clamp(0, bounds.h);
    let x1 = a.x.max(b.x).clamp(0, bounds.w);
    let y1 = a.y.max(b.y).clamp(0, bounds.h);
    Rectangle::new(Point::from((x0, y0)), Size::from((x1 - x0, y1 - y0)))
}

/// Size a buffer to `rect` and push it as a solid element; empty rects
/// (selection touching an output edge) are skipped.
fn push_solid<R: TomoeRenderer>(
    elements: &mut Vec<OutputRenderElements<R>>,
    buffer: &mut SolidColorBuffer,
    rect: Rectangle<i32, Physical>,
    color: [f32; 4],
) {
    if rect.size.w <= 0 || rect.size.h <= 0 {
        return;
    }
    // SolidColorBuffer sizes are Logical-typed in smithay; ours are pixel
    // sizes rendered at scale 1.0, so the retype is a no-op.
    buffer.update((rect.size.w, rect.size.h), color);
    elements.push(OutputRenderElements::Solid(
        SolidColorRenderElement::from_buffer(
            buffer,
            (rect.loc.x, rect.loc.y),
            1.0,
            1.0,
            Kind::Unspecified,
        ),
    ));
}

fn render_hint(fonts: &Fonts, has_selection: bool) -> (MemoryRenderBuffer, Size<i32, Physical>) {
    let spans: Vec<Span> = if has_selection {
        vec![
            Span::key(" Enter ", FONT_SIZE, FG, KEY_CHIP),
            Span::sans(" / ", FONT_SIZE, FG),
            Span::key(" Space ", FONT_SIZE, FG, KEY_CHIP),
            Span::sans(" capture    Drag to reselect    ", FONT_SIZE, FG),
            Span::key(" Esc ", FONT_SIZE, FG, KEY_CHIP),
            Span::sans(" cancel", FONT_SIZE, FG),
        ]
    } else {
        vec![
            Span::sans("Drag to select    ", FONT_SIZE, FG),
            Span::key(" Space ", FONT_SIZE, FG, KEY_CHIP),
            Span::sans(" whole screen    ", FONT_SIZE, FG),
            Span::key(" Esc ", FONT_SIZE, FG, KEY_CHIP),
            Span::sans(" cancel", FONT_SIZE, FG),
        ]
    };
    let (w, h) = fonts.measure(&spans);
    let width = w + 2 * (PADDING + BORDER);
    let height = h + 2 * (PADDING + BORDER);
    let mut canvas = Canvas::new(width, height);
    canvas.fill(BG);
    canvas.border(BORDER, ACCENT);
    canvas.draw_spans(fonts, BORDER + PADDING, BORDER + PADDING, &spans);
    canvas.into_buffer()
}
