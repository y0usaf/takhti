//! Access to the underlying `GlesRenderer`/`GlesFrame` behind tomoe's two
//! renderer types (niri shape: `AsGlesRenderer`/`AsGlesFrame`).
//!
//! Custom shader elements draw through Gles-specific APIs
//! (`override_default_tex_program`), which the generic `Renderer` trait
//! cannot express. On TTY, rendering always happens on the primary GPU's
//! Gles context — `MultiRenderer::as_mut` returns exactly that — so one
//! shader compilation per Gles context covers every path.

use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer};

use crate::backend::tty::{TtyFrame, TtyRenderer};

/// Get the underlying `GlesRenderer`.
pub trait AsGlesRenderer {
    fn as_gles_renderer(&mut self) -> &mut GlesRenderer;
}

impl AsGlesRenderer for GlesRenderer {
    fn as_gles_renderer(&mut self) -> &mut GlesRenderer {
        self
    }
}

impl AsGlesRenderer for TtyRenderer<'_> {
    fn as_gles_renderer(&mut self) -> &mut GlesRenderer {
        self.as_mut()
    }
}

/// Get the underlying `GlesFrame`.
pub trait AsGlesFrame<'frame, 'buffer>
where
    Self: 'frame,
{
    fn as_gles_frame(&mut self) -> &mut GlesFrame<'frame, 'buffer>;
}

impl<'frame, 'buffer> AsGlesFrame<'frame, 'buffer> for GlesFrame<'frame, 'buffer> {
    fn as_gles_frame(&mut self) -> &mut GlesFrame<'frame, 'buffer> {
        self
    }
}

impl<'frame, 'buffer> AsGlesFrame<'frame, 'buffer> for TtyFrame<'_, 'frame, 'buffer> {
    fn as_gles_frame(&mut self) -> &mut GlesFrame<'frame, 'buffer> {
        self.as_mut()
    }
}
