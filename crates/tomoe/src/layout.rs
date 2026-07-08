//! Native layout mechanisms. Layout *policy* lives in Lua (see
//! resources/wm.lua); this module only keeps rendering-adjacent bookkeeping.

use crate::state::Tomoe;

impl Tomoe {
    /// Update per-window border buffers (size + focus color) for mapped
    /// windows. Runs on the render path (backends and capture), right before
    /// border elements are built, so buffer sizes always match the live
    /// committed geometry — event-driven refresh missed the client's
    /// ack-commit after an initial configure, leaving stale slab sizes.
    /// Cheap when nothing changed: `SolidColorBuffer::update` only bumps its
    /// commit counter (damage) if size or color actually differ.
    pub fn refresh_borders(&mut self) {
        let settings = self.lua.settings();
        let width = settings.border_width;
        let radius = settings.corner_radius;
        let focused = self.focused_window();
        // Corner radius is a shader uniform — invisible to damage tracking —
        // so a changed setting bumps every window's damage-injection element
        // exactly once (rendered by scene_elements when rounding is on).
        if radius != self.applied_corner_radius {
            self.applied_corner_radius = radius;
            for damage in self.corner_damage.values_mut() {
                damage.damage_all();
            }
        }
        let windows: Vec<_> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(geo) = self.space.element_geometry(&window) else {
                continue;
            };
            let color = if Some(&window) == focused.as_ref() {
                settings.border_focused
            } else {
                settings.border_unfocused
            };
            self.corner_damage.entry(window.clone()).or_default();
            let buffers = self.border_buffers.entry(window.clone()).or_default();
            // Top, bottom, left, right slabs — a hollow frame rather than one
            // full-size rect, so transparent windows don't tint all over.
            buffers[0].update((geo.size.w + 2 * width, width), color);
            buffers[1].update((geo.size.w + 2 * width, width), color);
            buffers[2].update((width, geo.size.h), color);
            buffers[3].update((width, geo.size.h), color);
        }
    }
}
