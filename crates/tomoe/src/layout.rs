//! Native layout mechanisms. Layout *policy* lives in Lua (see
//! resources/wm.lua); this module only keeps rendering-adjacent bookkeeping.

use crate::state::Tomoe;

impl Tomoe {
    /// Update persistent per-window shader borders for mapped windows. Runs
    /// on every render path so size, focus color, width, and radius match the
    /// latest committed geometry. Unchanged parameters preserve the commit.
    pub fn refresh_borders(&mut self) {
        let settings = self.lua.settings();
        let width = settings.border_width.max(0);
        let global_radius = settings.corner_radius.max(0);
        let focused = self.focused_window();
        // Global radius changes affect windows without an override. Per-window
        // changes are damaged when their queued op is applied.
        if global_radius != self.applied_corner_radius {
            self.applied_corner_radius = global_radius;
            for damage in self.corner_damage.values_mut() {
                damage.damage_all();
            }
        }
        let windows: Vec<_> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(geo) = self.space.element_geometry(&window) else {
                continue;
            };
            let props = self
                .window_properties
                .iter()
                .find_map(|(id, props)| (self.windows.get(id) == Some(&window)).then_some(props));
            let radius = props.and_then(|p| p.radius).unwrap_or(global_radius);
            self.window_radii.insert(window.clone(), radius);
            let color = if Some(&window) == focused.as_ref() {
                props
                    .and_then(|p| p.border_focused)
                    .unwrap_or(settings.border_focused)
            } else {
                props
                    .and_then(|p| p.border_unfocused)
                    .unwrap_or(settings.border_unfocused)
            };
            self.corner_damage.entry(window.clone()).or_default();
            let size = (geo.size.w + 2 * width, geo.size.h + 2 * width).into();
            self.borders.entry(window.clone()).or_default().update(
                size,
                color,
                width,
                radius + width,
            );
            self.shadows.entry(window.clone()).or_default().update(
                geo.size,
                settings.shadow_color,
                settings.shadow_range,
                radius,
                settings.shadow_power,
            );
        }
    }
}
