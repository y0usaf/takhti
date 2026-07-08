//! M1 §4 acceptance fixture: nur's `examples/simple-bar` element tree,
//! fed as a static Rust table (no Lua yet — that's M2).
//!
//! Every section mirrors the corresponding nur source
//! (`~/Dev/nur/examples/simple-bar/init.lua` plus the widgets it
//! composes); service reads are frozen to representative values because
//! the acceptance criterion is layout/spacing/color parity with
//! nur-on-GPUI, not live data. Mapping notes:
//!
//! - nur `hbox` defaults to `items_center` → every `HBox` here sets
//!   `align: Center` explicitly (moonshell's default is `Start`).
//! - nur `fill = true` (GPUI `flex_1`) → `style.grow = 1.0`.
//! - nur `button` (M4 element here) reduces statically to a styled
//!   `HBox`: bg `surface0`, padding 10/2, gap 4 — the visual shell
//!   without the click handler.
//! - nur ships no bundled icon SVGs, so `ui.icon("battery-full")` falls
//!   back to name-as-text there; moonshell's `Icon` implements the same
//!   fallback contract, so the two degrade identically.
//! - Colors/typography/spacing are nur's Catppuccin Mocha theme tokens
//!   (`~/Dev/nur/lua/nur/theme.lua`).
//!
//! Run on a live session (`cargo run --example simple_bar`) and compare
//! against nur running `examples/simple-bar`.

use moonshell_render::element::{Align, Edges, Flex, Icon, Justify, Style, Text};
use moonshell_render::{Element, Renderer, Rgba, Scene, SceneDamage};
use moonshell_surface::{Canvas, Damage, DamageRect, LayerOptions, Painter};

// ── nur.theme (Catppuccin Mocha) ────────────────────────────────────────
const fn rgb(v: u32) -> Rgba {
    Rgba::new((v >> 16) as u8, (v >> 8) as u8, v as u8, 0xff)
}
const BASE: Rgba = rgb(0x1e1e2e); // window bg
const FG: Rgba = rgb(0xcdd6f4); // theme.text, window fg
const SURFACE0: Rgba = rgb(0x313244);
const FONT_SIZE: f32 = 13.0;
const BAR_HEIGHT: u32 = 32;
const BAR_PADDING: f32 = 12.0;
const WIDGET_GAP: f32 = 8.0;

// ── element shorthands (the ui.* constructors, statically) ─────────────
fn text(content: &str) -> Element {
    Element::Text(Text {
        content: content.into(),
        size: FONT_SIZE,
        color: FG,
        ..Text::default()
    })
}

fn hbox(gap: f32, children: Vec<Element>) -> Element {
    Element::HBox(Flex {
        gap,
        align: Align::Center,
        children,
        ..Flex::default()
    })
}

/// nur `ui.button` visual shell: bg surface0, px 10, py 2, gap 4.
fn button(label: &str) -> Element {
    Element::HBox(Flex {
        style: Style {
            bg: Some(SURFACE0),
            ..Style::default()
        },
        gap: 4.0,
        padding: Edges {
            top: 2.0,
            right: 10.0,
            bottom: 2.0,
            left: 10.0,
        },
        align: Align::Center,
        children: vec![text(label)],
        ..Flex::default()
    })
}

/// One of `ui.bar_layout`'s three regions: gap 8, fill, per-region justify.
fn section(justify: Justify, children: Vec<Element>) -> Element {
    Element::HBox(Flex {
        style: Style {
            grow: 1.0,
            ..Style::default()
        },
        gap: WIDGET_GAP,
        justify,
        align: Align::Center,
        children,
        ..Flex::default()
    })
}

/// The full bar tree, exactly as simple-bar's render function returns it.
fn bar_tree() -> Element {
    // Workspaces widget: hbox gap 4 of workspace names (no active
    // highlight — nur's widget has the same TODO).
    let workspaces = hbox(
        4.0,
        ["1", "2", "3", "4", "5"].iter().map(|n| text(n)).collect(),
    );

    // Left: workspaces | active window title ("  " .. title).
    let left = vec![workspaces, text("  moonshell — PLAN.md")];

    // Center: media trigger (playing) | clock.
    let center = vec![button("󰎈  Lua Moon — Aurora"), text("12:34:56")];

    // Network widget: connected wifi, strength ≥ 75.
    let network = hbox(4.0, vec![text("󰤨"), text("HomeNet")]);

    // Battery widget: 85% → "battery-full" icon (falls back to text,
    // matching nur's asset-less fallback) + percent label.
    let battery = hbox(
        4.0,
        vec![
            Element::Icon(Icon {
                name: "battery-full".into(),
                color: Some(FG),
                ..Icon::default()
            }),
            text("85%"),
        ],
    );

    // Right: CPU | RAM | network | volume trigger | battery.
    let right = vec![
        text("󰻠 4%"),
        text("󰍛 38%"),
        network,
        button("󰕾 42%"),
        battery,
    ];

    // ui.bar_layout: outer hbox with bar_padding, three fill regions.
    Element::HBox(Flex {
        style: Style {
            bg: Some(BASE),
            ..Style::default()
        },
        padding: Edges {
            left: BAR_PADDING,
            right: BAR_PADDING,
            ..Edges::default()
        },
        align: Align::Center,
        children: vec![
            section(Justify::Start, left),
            section(Justify::Center, center),
            section(Justify::End, right),
        ],
        ..Flex::default()
    })
}

struct SimpleBar {
    renderer: Renderer,
    scene: Scene,
    root: Element,
}

impl Painter for SimpleBar {
    fn paint(&mut self, canvas: Canvas<'_>) -> Damage {
        let Canvas {
            buf,
            width,
            height,
            scale,
            fresh,
        } = canvas;
        if fresh {
            self.scene.invalidate();
        }
        match self.scene.render(
            &mut self.renderer,
            buf,
            width,
            height,
            scale as f32,
            &self.root,
        ) {
            SceneDamage::None => Damage::None,
            SceneDamage::Full => Damage::Full,
            SceneDamage::Rects(rects) => Damage::Rects(
                rects
                    .into_iter()
                    .map(|r| DamageRect {
                        x: r.x,
                        y: r.y,
                        width: r.w,
                        height: r.h,
                    })
                    .collect(),
            ),
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moonshell=info")),
        )
        .init();

    let mut boot_check = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--boot-check" => boot_check = true,
            other => anyhow::bail!("unknown argument: {other} (try --boot-check)"),
        }
    }

    let options = LayerOptions {
        namespace: "moonshell-simple-bar".into(),
        height: BAR_HEIGHT,
        exit_after_first_draw: boot_check,
        ..LayerOptions::default()
    };
    let painter = SimpleBar {
        renderer: Renderer::new(),
        scene: Scene::new(),
        root: bar_tree(),
    };
    moonshell_surface::run(options, Box::new(painter))?;
    Ok(())
}
