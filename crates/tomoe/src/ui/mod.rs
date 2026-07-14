//! Compositor-drawn UI: modal dialogs and transient overlays, rendered as
//! memory-buffer elements above all client content (no Wayland protocol
//! involved). Everything but the screenshot overlay lives in the retained
//! widget registry (`widgets.rs`, the `tomoe.ui` surface).

mod screenshot_ui;
pub mod text;
pub mod widgets;

use smithay::output::Output;
use smithay::utils::{Physical, Point, Size};
use tracing::warn;

pub use screenshot_ui::ScreenshotUi;
pub use widgets::Widgets;

use crate::render::{OutputRenderElements, TomoeRenderer};
use text::Fonts;

// Shared palette (premultiplied RGBA).
pub const BG: [f32; 4] = [0.11, 0.11, 0.13, 1.0];
pub const FG: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub const RED: [f32; 4] = [0.75, 0.18, 0.22, 1.0];
pub const ACCENT: [f32; 4] = [0.48, 0.63, 0.97, 1.0];
pub const KEY_CHIP: [f32; 4] = [0.27, 0.27, 0.31, 1.0];
pub const BACKDROP: [f32; 4] = [0.0, 0.0, 0.0, 0.4];

pub struct Ui {
    /// None if no usable font was found; UI elements are skipped.
    fonts: Option<Fonts>,
    pub widgets: Widgets,
    pub screenshot: ScreenshotUi,
}

impl Ui {
    pub fn new() -> Self {
        let fonts = match Fonts::load() {
            Ok(fonts) => Some(fonts),
            Err(err) => {
                warn!("no fonts for compositor UI (dialogs disabled): {err:#}");
                None
            }
        };
        Self {
            fonts,
            widgets: Widgets::default(),
            screenshot: ScreenshotUi::new(),
        }
    }

    /// Build render elements for one output, topmost first (callers prepend
    /// these before window content). Coordinates are output-local.
    /// `include_screenshot_ui` is false on the capture paths so screenshots
    /// (and screencopy/screencast frames) never contain the selection
    /// overlay itself.
    pub fn render_elements<R: TomoeRenderer>(
        &mut self,
        renderer: &mut R,
        output: &Output,
        output_size: Size<i32, Physical>,
        include_screenshot_ui: bool,
    ) -> Vec<OutputRenderElements<R>> {
        let mut elements = Vec::new();
        // The selection overlay works without fonts (only its hint bar needs
        // them), so it renders before the fonts check.
        if include_screenshot_ui {
            self.screenshot.render_elements(
                self.fonts.as_ref(),
                renderer,
                output,
                output_size,
                &mut elements,
            );
        } else {
            self.screenshot
                .render_frozen(renderer, output, &mut elements);
        }
        let Some(fonts) = &self.fonts else {
            return elements;
        };
        self.widgets
            .render_elements(fonts, renderer, output_size, &mut elements);
        elements
    }

    /// Pointer hover over the topmost modal menu (`Widgets::hover_menu`);
    /// no-op without fonts (nothing is drawn to hover over).
    pub fn hover_menu(
        &mut self,
        output_size: Size<i32, Physical>,
        point: Point<f64, Physical>,
    ) -> bool {
        match &self.fonts {
            Some(fonts) => self.widgets.hover_menu(fonts, output_size, point),
            None => false,
        }
    }

    /// Click hit-test on the topmost modal menu (`Widgets::menu_click`).
    pub fn menu_click(
        &mut self,
        output_size: Size<i32, Physical>,
        point: Point<f64, Physical>,
    ) -> Option<Option<usize>> {
        self.widgets
            .menu_click(self.fonts.as_ref()?, output_size, point)
    }
}
