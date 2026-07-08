//! The element vocabulary — the data types Lua render callbacks will
//! eventually describe (M2). In M1 they are plain Rust values so the
//! layout/draw core is provably correct before the runtime lands.
//!
//! Doctrine 05 (one declaration mechanism): adding an element type is
//! always the same four arms —
//! 1. a variant here (every variant struct carries a [`Style`]),
//! 2. a measure arm in `layout.rs`,
//! 3. a draw arm in `draw.rs`,
//! 4. (M2) a `from_table` arm in the runtime bridge.
//!
//! All lengths in element props are **logical pixels**; the layout pass
//! multiplies by the output scale exactly once.

use crate::Rgba;

/// Per-side lengths (logical px) for padding.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub const fn all(v: f32) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

/// Cross-axis placement of a flex child within its row/column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// Main-axis distribution when no child grows (leftover space exists).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    #[default]
    Vertical,
}

/// Style properties common to every element (the uniform slot in the
/// declaration shape). `width`/`height` override the intrinsic size;
/// `grow` claims a weighted share of the parent's leftover main-axis
/// space (0 = none).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    pub bg: Option<Rgba>,
    pub border_radius: f32,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub grow: f32,
}

/// Shared body of `hbox`/`vbox` — the flex-lite container.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flex {
    pub style: Style,
    pub gap: f32,
    pub padding: Edges,
    pub justify: Justify,
    pub align: Align,
    pub children: Vec<Element>,
}

/// Children overlaid on top of each other, each given the full rect.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stack {
    pub style: Style,
    pub children: Vec<Element>,
}

/// A single shaped line of text. `line_height` defaults to
/// `size * 1.3` when unset.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub style: Style,
    pub content: String,
    pub size: f32,
    pub line_height: Option<f32>,
    pub color: Rgba,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            style: Style::default(),
            content: String::new(),
            size: 14.0,
            line_height: None,
            color: Rgba::new(0xff, 0xff, 0xff, 0xff),
        }
    }
}

/// Empty grow-1 gap (nur's `ui.spacer()`).
#[derive(Clone, Debug, PartialEq)]
pub struct Spacer {
    pub style: Style,
}

impl Default for Spacer {
    fn default() -> Self {
        Self {
            style: Style {
                grow: 1.0,
                ..Style::default()
            },
        }
    }
}

/// Thin rule; stretches to fill the container's cross axis.
#[derive(Clone, Debug, PartialEq)]
pub struct Separator {
    pub style: Style,
    pub orientation: Orientation,
    pub thickness: f32,
    pub color: Rgba,
}

impl Default for Separator {
    fn default() -> Self {
        Self {
            style: Style::default(),
            orientation: Orientation::default(),
            thickness: 1.0,
            color: Rgba::new(0x45, 0x47, 0x5a, 0xff),
        }
    }
}

/// Horizontal progress bar. Sizing comes from `style.width`/`.height`
/// (or `grow` to fill); defaults to 4 px tall.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub style: Style,
    /// 0.0–1.0, clamped at draw time.
    pub value: f32,
    pub color: Rgba,
    pub track: Rgba,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            style: Style {
                height: Some(4.0),
                border_radius: 2.0,
                ..Style::default()
            },
            value: 0.0,
            color: Rgba::new(0x89, 0xb4, 0xfa, 0xff),
            track: Rgba::new(0x31, 0x32, 0x44, 0xff),
        }
    }
}

/// Ring-style progress: a track circle plus a value arc from 12 o'clock.
#[derive(Clone, Debug, PartialEq)]
pub struct CircularProgress {
    pub style: Style,
    /// 0.0–1.0, clamped at draw time.
    pub value: f32,
    /// Ring diameter (logical px).
    pub size: f32,
    /// Ring stroke width (logical px).
    pub thickness: f32,
    pub color: Rgba,
    pub track: Rgba,
}

impl Default for CircularProgress {
    fn default() -> Self {
        Self {
            style: Style::default(),
            value: 0.0,
            size: 16.0,
            thickness: 2.0,
            color: Rgba::new(0x89, 0xb4, 0xfa, 0xff),
            track: Rgba::new(0x31, 0x32, 0x44, 0xff),
        }
    }
}

/// The element tree node. Icon (SVG via resvg) and Image land in the
/// next M1 slice (PLAN.md).
#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    HBox(Flex),
    VBox(Flex),
    Stack(Stack),
    Text(Text),
    Spacer(Spacer),
    Separator(Separator),
    Progress(Progress),
    CircularProgress(CircularProgress),
}

impl Element {
    /// The uniform style slot — one match, every variant.
    pub fn style(&self) -> &Style {
        match self {
            Element::HBox(e) | Element::VBox(e) => &e.style,
            Element::Stack(e) => &e.style,
            Element::Text(e) => &e.style,
            Element::Spacer(e) => &e.style,
            Element::Separator(e) => &e.style,
            Element::Progress(e) => &e.style,
            Element::CircularProgress(e) => &e.style,
        }
    }
}
