//! The logical↔physical coordinate boundary.
//!
//! Takhti's canonical coordinate space is **integer physical pixels**: layout,
//! the Lua API, and rendering all speak physical. Wayland protocol objects
//! (xdg configure sizes, wl_output geometry, pointer events) speak *logical*
//! coordinates. Every conversion between the two lives in this module and
//! nowhere else — that is what keeps client buffers sampled 1:1 on the pixel
//! grid (sharp text) regardless of output scale.
//!
//! Currently one scale applies to all outputs (`takhti.settings { scale }`),
//! so protocol-logical is simply physical divided by that scale. Per-output
//! scales later only change which scale each helper is called with.

use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

/// Snap a scale to the closest value representable by wp-fractional-scale-v1
/// (multiples of 1/120). An unrepresentable scale would guarantee resampling:
/// the client renders at the protocol's approximation while we sample at the
/// exact value. Non-positive/non-finite input falls back to 1.
pub fn snap_scale(scale: f64) -> f64 {
    if !scale.is_finite() || scale <= 0.0 {
        return 1.0;
    }
    ((scale * 120.0).round() / 120.0).max(1.0 / 120.0)
}

/// Quantize a desired physical window size to what a client can actually
/// produce. xdg-shell configure sizes are integer *logical*, so at scale `s`
/// the achievable buffer sizes are `round(n * s)` for integer `n`. Returns
/// the logical size to configure the client with and the physical size its
/// buffer will have — layout bookkeeping must use the latter.
pub fn configure_size(
    size: Size<i32, Physical>,
    scale: f64,
) -> (Size<i32, Logical>, Size<i32, Physical>) {
    let logical: Size<i32, Logical> = Size::from((
        ((size.w as f64 / scale).round() as i32).max(1),
        ((size.h as f64 / scale).round() as i32).max(1),
    ));
    let achievable = logical_size_to_physical(logical.to_f64(), scale);
    (logical, achievable)
}

/// Physical position → protocol-logical, for pointer events and focus
/// coordinates handed to the seat. Exact (no rounding): only *differences*
/// of these values reach clients, and those are exact surface-local coords.
pub fn point_to_protocol(pos: Point<f64, Physical>, scale: f64) -> Point<f64, Logical> {
    Point::from((pos.x / scale, pos.y / scale))
}

/// Protocol-logical position → physical, the inverse of
/// [`point_to_protocol`] (exact, no rounding). Used to bring the seat's
/// pointer location back into compositor space.
pub fn point_to_physical(pos: Point<f64, Logical>, scale: f64) -> Point<f64, Physical> {
    Point::from((pos.x * scale, pos.y * scale))
}

/// A logical size (client-reported, e.g. `Window::geometry()`) in physical
/// pixels, rounded to the grid.
pub fn logical_size_to_physical(size: Size<f64, Logical>, scale: f64) -> Size<i32, Physical> {
    Size::from((
        (size.w * scale).round() as i32,
        (size.h * scale).round() as i32,
    ))
}

/// A logical point (client-reported offsets like `Window::geometry().loc` or
/// layer-shell positions) in physical pixels, rounded to the grid.
pub fn logical_point_to_physical(pos: Point<f64, Logical>, scale: f64) -> Point<i32, Physical> {
    Point::from((
        (pos.x * scale).round() as i32,
        (pos.y * scale).round() as i32,
    ))
}

/// A physical rect in logical coordinates, for protocol objects that demand
/// integer logical rects (output positions, enter/leave overlaps). Rounded;
/// callers must not feed the result back into layout.
pub fn rect_to_logical(rect: Rectangle<i32, Physical>, scale: f64) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((
            (rect.loc.x as f64 / scale).round() as i32,
            (rect.loc.y as f64 / scale).round() as i32,
        )),
        Size::from((
            ((rect.size.w as f64 / scale).round() as i32).max(1),
            ((rect.size.h as f64 / scale).round() as i32).max(1),
        )),
    )
}

/// A logical rect (layer-shell geometry, exclusive zones) on the physical
/// grid.
pub fn rect_to_physical(rect: Rectangle<i32, Logical>, scale: f64) -> Rectangle<i32, Physical> {
    Rectangle::new(
        logical_point_to_physical(rect.loc.to_f64(), scale),
        logical_size_to_physical(rect.size.to_f64(), scale),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_scale_is_identity_on_representable_values() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            assert_eq!(snap_scale(scale), scale);
        }
    }

    #[test]
    fn snap_scale_rounds_to_n_over_120() {
        assert_eq!(snap_scale(1.3333), 160.0 / 120.0);
        assert_eq!(snap_scale(1.1), 132.0 / 120.0);
        assert_eq!(snap_scale(0.0), 1.0);
        assert_eq!(snap_scale(f64::NAN), 1.0);
        assert_eq!(snap_scale(-2.0), 1.0);
    }

    #[test]
    fn configure_size_is_identity_at_scale_1() {
        let (logical, physical) = configure_size(Size::from((1280, 720)), 1.0);
        assert_eq!((logical.w, logical.h), (1280, 720));
        assert_eq!((physical.w, physical.h), (1280, 720));
    }

    #[test]
    fn configure_size_quantizes_at_fractional_scale() {
        // At 1.25 the achievable widths are round(n * 1.25); 126 physical is
        // representable (n=101 -> 126.25 -> 126), 127 quantizes to nearest.
        let (logical, physical) = configure_size(Size::from((127, 127)), 1.25);
        assert_eq!(logical.w, 102); // round(127 / 1.25) = round(101.6)
        assert_eq!(physical.w, 128); // round(102 * 1.25) = round(127.5)
                                     // The invariant that matters: achievable == round(logical * scale),
                                     // i.e. the client's buffer covers exactly this many pixels.
        assert_eq!(physical.w, (logical.w as f64 * 1.25).round() as i32);
        assert_eq!(physical.h, (logical.h as f64 * 1.25).round() as i32);
    }

    #[test]
    fn configure_size_never_returns_zero() {
        let (logical, physical) = configure_size(Size::from((1, 1)), 3.0);
        assert!(logical.w >= 1 && logical.h >= 1);
        assert!(physical.w >= 1 && physical.h >= 1);
    }

    #[test]
    fn protocol_point_differences_are_exact_surface_locals() {
        let scale = 1.5;
        let cursor: Point<f64, Physical> = Point::from((300.0, 450.0));
        let surface: Point<f64, Physical> = Point::from((150.0, 150.0));
        let local = point_to_protocol(cursor, scale) - point_to_protocol(surface, scale);
        assert_eq!(local, Point::from((100.0, 200.0)));
    }
}
