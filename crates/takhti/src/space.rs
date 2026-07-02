//! Window/output bookkeeping in integer physical pixels.
//!
//! Replaces `smithay::desktop::Space`, which stores integer *logical*
//! coordinates. Logical integers land on fractional physical positions at
//! fractional scales, and independently-rounded elements drift a pixel apart
//! (blurry buffers, border seams). Here everything is `Point<i32, Physical>`,
//! so grid alignment is guaranteed by the type, not by rounding discipline.
//! Conversions to protocol-logical happen in `crate::coords` only.

use smithay::desktop::space::SpaceElement;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{IsAlive, Physical, Point, Rectangle, Size};

use crate::coords;

#[derive(Default)]
pub struct PhysicalSpace {
    /// Uniform output scale (snapped to N/120). Uniform-for-now: per-output
    /// scales only require storing this per entry in `outputs`.
    scale: f64,
    /// Camera over the window canvas: `screen = (world - offset) * zoom`.
    /// Outputs, layer-shell, UI, and the cursor are screen-fixed; only
    /// windows (and their borders) live in world space. The offset is
    /// integer so the identity/pan case keeps every element on the pixel
    /// grid; zoom != 1 is the one sanctioned resampling path (transient by
    /// design: configs snap back to 1 for crisp steady-state).
    view_offset: Point<i32, Physical>,
    view_zoom: f64,
    outputs: Vec<(Output, Point<i32, Physical>)>,
    /// Mapped windows with their geometry origin, bottom → top.
    windows: Vec<(Window, Point<i32, Physical>)>,
}

impl PhysicalSpace {
    pub fn new() -> Self {
        Self {
            scale: 1.0,
            view_offset: Point::from((0, 0)),
            view_zoom: 1.0,
            outputs: Vec::new(),
            windows: Vec::new(),
        }
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }

    pub fn set_scale(&mut self, scale: f64) {
        self.scale = coords::snap_scale(scale);
    }

    // ── View (camera) ──

    pub fn view_offset(&self) -> Point<i32, Physical> {
        self.view_offset
    }

    pub fn view_zoom(&self) -> f64 {
        self.view_zoom
    }

    pub fn set_view(&mut self, offset: Point<i32, Physical>, zoom: f64) {
        self.view_offset = offset;
        self.view_zoom = if zoom.is_finite() {
            zoom.clamp(1.0 / 16.0, 16.0)
        } else {
            1.0
        };
    }

    /// Screen position (pointer, output-space) → world position (windows).
    pub fn screen_to_world(&self, pos: Point<f64, Physical>) -> Point<f64, Physical> {
        Point::from((
            pos.x / self.view_zoom + self.view_offset.x as f64,
            pos.y / self.view_zoom + self.view_offset.y as f64,
        ))
    }

    /// World position → screen position, the inverse of [`screen_to_world`].
    pub fn world_to_screen(&self, pos: Point<f64, Physical>) -> Point<f64, Physical> {
        Point::from((
            (pos.x - self.view_offset.x as f64) * self.view_zoom,
            (pos.y - self.view_offset.y as f64) * self.view_zoom,
        ))
    }

    /// A world rect on the screen (for culling against output rects).
    pub fn world_rect_to_screen(&self, rect: Rectangle<i32, Physical>) -> Rectangle<f64, Physical> {
        Rectangle::new(
            self.world_to_screen(rect.loc.to_f64()),
            Size::from((
                rect.size.w as f64 * self.view_zoom,
                rect.size.h as f64 * self.view_zoom,
            )),
        )
    }

    // ── Outputs ──

    pub fn map_output(&mut self, output: &Output, loc: impl Into<Point<i32, Physical>>) {
        let loc = loc.into();
        if let Some(entry) = self.outputs.iter_mut().find(|(o, _)| o == output) {
            entry.1 = loc;
        } else {
            self.outputs.push((output.clone(), loc));
        }
    }

    pub fn unmap_output(&mut self, output: &Output) {
        self.outputs.retain(|(o, _)| o != output);
    }

    pub fn outputs(&self) -> impl DoubleEndedIterator<Item = &Output> {
        self.outputs.iter().map(|(o, _)| o)
    }

    /// Output rect in physical pixels: mapped position + transformed mode size.
    pub fn output_geometry(&self, output: &Output) -> Option<Rectangle<i32, Physical>> {
        let loc = self
            .outputs
            .iter()
            .find(|(o, _)| o == output)
            .map(|(_, loc)| *loc)?;
        let mode = output.current_mode()?;
        let size = output.current_transform().transform_size(mode.size);
        Some(Rectangle::new(loc, size))
    }

    pub fn output_under(&self, pos: Point<f64, Physical>) -> Option<&Output> {
        self.outputs.iter().map(|(o, _)| o).find(|output| {
            self.output_geometry(output)
                .is_some_and(|geo| geo.to_f64().contains(pos))
        })
    }

    // ── Windows ──

    /// Map a window at `loc`, or move it if already mapped. New windows go on
    /// top; moving preserves stacking order.
    pub fn map_element(&mut self, window: Window, loc: impl Into<Point<i32, Physical>>) {
        let loc = loc.into();
        if let Some(entry) = self.windows.iter_mut().find(|(w, _)| *w == window) {
            entry.1 = loc;
        } else {
            self.windows.push((window, loc));
        }
    }

    pub fn unmap(&mut self, window: &Window) {
        if let Some(idx) = self.windows.iter().position(|(w, _)| w == window) {
            let (window, _) = self.windows.remove(idx);
            for (output, _) in &self.outputs {
                window.output_leave(output);
            }
        }
    }

    /// Mapped windows, bottom → top.
    pub fn elements(&self) -> impl DoubleEndedIterator<Item = &Window> {
        self.windows.iter().map(|(w, _)| w)
    }

    pub fn raise_element(&mut self, window: &Window) {
        if let Some(idx) = self.windows.iter().position(|(w, _)| w == window) {
            let entry = self.windows.remove(idx);
            self.windows.push(entry);
        }
    }

    pub fn element_location(&self, window: &Window) -> Option<Point<i32, Physical>> {
        self.windows
            .iter()
            .find(|(w, _)| w == window)
            .map(|(_, loc)| *loc)
    }

    /// Window rect in physical pixels. The size derives from the client's
    /// committed geometry (logical), rounded once onto the grid — the same
    /// rounding the renderer applies, so layout and sampling always agree.
    pub fn element_geometry(&self, window: &Window) -> Option<Rectangle<i32, Physical>> {
        let loc = self.element_location(window)?;
        let size = coords::logical_size_to_physical(window.geometry().size.to_f64(), self.scale);
        Some(Rectangle::new(loc, size))
    }

    /// Topmost window whose rect contains `pos`, with its location.
    pub fn element_under(
        &self,
        pos: Point<f64, Physical>,
    ) -> Option<(&Window, Point<i32, Physical>)> {
        self.windows.iter().rev().find_map(|(window, loc)| {
            self.element_geometry(window)
                .is_some_and(|geo| geo.to_f64().contains(pos))
                .then_some((window, *loc))
        })
    }

    /// Drop dead windows, refresh output enter/leave, and let windows update
    /// their internal state (mirrors `Space::refresh`).
    pub fn refresh(&mut self) {
        self.windows.retain(|(w, _)| w.alive());
        for (window, _) in &self.windows {
            let Some(geo) = self.element_geometry(window) else {
                continue;
            };
            for (output, _) in &self.outputs {
                let Some(output_geo) = self.output_geometry(output) else {
                    continue;
                };
                // Outputs are screen-fixed; windows are world. Compare in
                // world space so the overlap stays in window-local pixels.
                let output_world = Rectangle::new(
                    self.screen_to_world(output_geo.loc.to_f64()).to_i32_round(),
                    Size::from((
                        (output_geo.size.w as f64 / self.view_zoom).ceil() as i32,
                        (output_geo.size.h as f64 / self.view_zoom).ceil() as i32,
                    )),
                );
                if let Some(mut overlap) = output_world.intersection(geo) {
                    // output_enter expects the overlap relative to the window.
                    overlap.loc -= geo.loc;
                    window.output_enter(output, coords::rect_to_logical(overlap, self.scale));
                } else {
                    window.output_leave(output);
                }
            }
            SpaceElement::refresh(window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_transforms_are_inverse() {
        let mut space = PhysicalSpace::new();
        space.set_view((320, -140).into(), 1.5);
        let screen: Point<f64, Physical> = Point::from((123.0, 456.0));
        let world = space.screen_to_world(screen);
        let back = space.world_to_screen(world);
        assert!((back.x - screen.x).abs() < 1e-9);
        assert!((back.y - screen.y).abs() < 1e-9);
    }

    #[test]
    fn identity_view_is_a_noop() {
        let space = PhysicalSpace::new();
        let pos: Point<f64, Physical> = Point::from((10.5, 20.25));
        assert_eq!(space.screen_to_world(pos), pos);
        assert_eq!(space.world_to_screen(pos), pos);
    }

    #[test]
    fn pan_translates_without_scaling() {
        let mut space = PhysicalSpace::new();
        space.set_view((100, 50).into(), 1.0);
        let world = space.screen_to_world(Point::from((0.0, 0.0)));
        assert_eq!(world, Point::from((100.0, 50.0)));
        let rect = space.world_rect_to_screen(Rectangle::new((100, 50).into(), (640, 480).into()));
        assert_eq!(rect.loc, Point::from((0.0, 0.0)));
        assert_eq!(rect.size, Size::from((640.0, 480.0)));
    }

    #[test]
    fn zoom_clamps_and_rejects_nonsense() {
        let mut space = PhysicalSpace::new();
        space.set_view((0, 0).into(), 1000.0);
        assert_eq!(space.view_zoom(), 16.0);
        space.set_view((0, 0).into(), f64::NAN);
        assert_eq!(space.view_zoom(), 1.0);
        space.set_view((0, 0).into(), 0.0);
        assert_eq!(space.view_zoom(), 1.0 / 16.0);
    }

    #[test]
    fn zoomed_world_rect_scales_around_view_origin() {
        let mut space = PhysicalSpace::new();
        space.set_view((0, 0).into(), 2.0);
        let rect = space.world_rect_to_screen(Rectangle::new((100, 100).into(), (200, 100).into()));
        assert_eq!(rect.loc, Point::from((200.0, 200.0)));
        assert_eq!(rect.size, Size::from((400.0, 200.0)));
    }
}
