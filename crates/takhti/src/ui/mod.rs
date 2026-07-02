//! Compositor-drawn UI: modal dialogs and transient overlays, rendered as
//! memory-buffer elements above all client content (no Wayland protocol
//! involved — same approach as niri).

mod config_error_notification;
mod exit_confirm_dialog;
mod hotkey_overlay;
pub mod text;

use smithay::utils::{Physical, Size};
use tracing::warn;

pub use config_error_notification::ConfigErrorNotification;
pub use exit_confirm_dialog::ExitConfirmDialog;
pub use hotkey_overlay::HotkeyOverlay;

use crate::input::Bind;
use crate::render::{OutputRenderElements, TakhtiRenderer};
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
    pub exit_dialog: ExitConfirmDialog,
    pub hotkey_overlay: HotkeyOverlay,
    pub config_error: ConfigErrorNotification,
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
            exit_dialog: ExitConfirmDialog::new(),
            hotkey_overlay: HotkeyOverlay::new(),
            config_error: ConfigErrorNotification::new(),
        }
    }

    /// Build render elements for one output, topmost first (callers prepend
    /// these before window content). Coordinates are output-local.
    pub fn render_elements<R: TakhtiRenderer>(
        &mut self,
        renderer: &mut R,
        output_size: Size<i32, Physical>,
        binds: &[Bind],
    ) -> Vec<OutputRenderElements<R>> {
        let Some(fonts) = &self.fonts else {
            return Vec::new();
        };
        let mut elements = Vec::new();
        self.exit_dialog
            .render_elements(fonts, renderer, output_size, &mut elements);
        self.config_error
            .render_elements(fonts, renderer, output_size, &mut elements);
        self.hotkey_overlay
            .render_elements(fonts, renderer, output_size, binds, &mut elements);
        elements
    }
}
