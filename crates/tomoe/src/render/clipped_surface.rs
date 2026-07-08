//! Rounded-corner clipping for window surfaces (niri's
//! `ClippedSurfaceRenderElement`, made physical-first).
//!
//! Wraps a `WaylandSurfaceRenderElement` and draws it with a custom texture
//! program that clips to the window geometry and rounds the corners in the
//! fragment shader — no offscreen intermediate, so client pixels still
//! sample 1:1 (coordinate doctrine §5). Geometry is the window rect in
//! output-local integer physical pixels, the same space every element
//! position uses; corner radii are physical pixels.

use glam::{Mat3, Vec2};
use smithay::backend::renderer::buffer_y_inverted;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

use super::renderer::AsGlesFrame as _;
use super::shaders::mat3_uniform;
use super::TomoeRenderer;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

#[derive(Debug)]
pub struct ClippedSurfaceRenderElement<R: TomoeRenderer> {
    inner: WaylandSurfaceRenderElement<R>,
    program: GlesTexProgram,
    /// Corner radius in physical pixels (all four corners).
    corner_radius: f32,
    /// Window geometry in output-local physical pixels: the clip rect.
    geometry: Rectangle<i32, Physical>,
    /// Scale the inner element was created with (needed to query its
    /// physical geometry; tomoe outputs render at their space scale).
    scale: Scale<f64>,
}

impl<R: TomoeRenderer> ClippedSurfaceRenderElement<R> {
    pub fn new(
        elem: WaylandSurfaceRenderElement<R>,
        scale: Scale<f64>,
        geometry: Rectangle<i32, Physical>,
        program: GlesTexProgram,
        corner_radius: f32,
    ) -> Self {
        Self {
            inner: elem,
            program,
            corner_radius,
            geometry,
            scale,
        }
    }

    /// Would clipping `elem` to `geometry` with `corner_radius` change any
    /// pixel? False for elements entirely inside the rounded region — those
    /// render through the plain (scanout-capable) path.
    pub fn will_clip(
        elem: &WaylandSurfaceRenderElement<R>,
        scale: Scale<f64>,
        geometry: Rectangle<i32, Physical>,
        corner_radius: f32,
    ) -> bool {
        let elem_geo = elem.geometry(scale);
        if corner_radius == 0. {
            !geometry.contains_rect(elem_geo)
        } else {
            let corners = rounded_corners(geometry, corner_radius);
            let geo = Rectangle::subtract_rects_many([geometry], corners);
            !Rectangle::subtract_rects_many([elem_geo], geo).is_empty()
        }
    }

    fn compute_uniforms(&self) -> Vec<Uniform<'static>> {
        let elem_geo = self.inner.geometry(self.scale);

        let elem_geo_loc = Vec2::new(elem_geo.loc.x as f32, elem_geo.loc.y as f32);
        let elem_geo_size = Vec2::new(elem_geo.size.w as f32, elem_geo.size.h as f32);

        let geo_loc = Vec2::new(self.geometry.loc.x as f32, self.geometry.loc.y as f32);
        let geo_size = Vec2::new(self.geometry.size.w as f32, self.geometry.size.h as f32);

        let buf_size = self.inner.buffer_size();
        let buf_size = Vec2::new(buf_size.w as f32, buf_size.h as f32);

        let view = self.inner.view();
        let src_loc = Vec2::new(view.src.loc.x as f32, view.src.loc.y as f32);
        let src_size = Vec2::new(view.src.size.w as f32, view.src.size.h as f32);

        let transform = self.inner.transform();
        // HACK (from niri): ??? for some reason flipped ones are fine.
        let transform = match transform {
            Transform::_90 => Transform::_270,
            Transform::_270 => Transform::_90,
            x => x,
        };
        let transform_matrix = Mat3::from_translation(Vec2::new(0.5, 0.5))
            * Mat3::from_cols_array(transform.matrix().as_ref())
            * Mat3::from_translation(-Vec2::new(0.5, 0.5));

        let y_invert = if buffer_y_inverted(self.inner.buffer()).unwrap_or(false) {
            Mat3::from_scale(Vec2::new(1., -1.))
        } else {
            Mat3::IDENTITY
        };

        // v_coords (0..1 across the sampled buffer region) → geometry-unit
        // coordinates (0..1 across the window geometry).
        let input_to_geo = transform_matrix * Mat3::from_scale(elem_geo_size / geo_size)
            * Mat3::from_translation((elem_geo_loc - geo_loc) / elem_geo_size)
            // Apply viewporter src.
            * Mat3::from_scale(buf_size / src_size)
            * Mat3::from_translation(-src_loc / buf_size)
            * y_invert;

        let radius = self.corner_radius;
        vec![
            Uniform::new("geo_size", (geo_size.x, geo_size.y)),
            Uniform::new("corner_radius", [radius, radius, radius, radius]),
            mat3_uniform("input_to_geo", input_to_geo),
        ]
    }
}

/// The four corner squares of `geo` that rounding carves into.
pub fn rounded_corners(
    geo: Rectangle<i32, Physical>,
    radius: f32,
) -> [Rectangle<i32, Physical>; 4] {
    let r = radius.ceil() as i32;
    let size = Size::from((r, r));
    [
        Rectangle::new(geo.loc, size),
        Rectangle::new(Point::from((geo.loc.x + geo.size.w - r, geo.loc.y)), size),
        Rectangle::new(
            Point::from((geo.loc.x + geo.size.w - r, geo.loc.y + geo.size.h - r)),
            size,
        ),
        Rectangle::new(Point::from((geo.loc.x, geo.loc.y + geo.size.h - r)), size),
    ]
}

impl<R: TomoeRenderer> Element for ClippedSurfaceRenderElement<R> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        // Radius changes are damaged separately (`ExtraDamage` per window).
        let damage = self.inner.damage_since(scale, commit);

        // Intersect with geometry, since we're clipping by it. Damage is
        // element-local, geometry output-local: rebase.
        let mut geo = self.geometry;
        geo.loc -= self.geometry(scale).loc;
        damage
            .into_iter()
            .filter_map(|rect| rect.intersection(geo))
            .collect()
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let regions = self.inner.opaque_regions(scale);

        // Intersect with geometry, since we're clipping by it.
        let elem_loc = self.geometry(scale).loc;
        let mut geo = self.geometry;
        geo.loc -= elem_loc;
        let regions = regions
            .into_iter()
            .filter_map(|rect| rect.intersection(geo));

        // Subtract the rounded corners.
        if self.corner_radius == 0. {
            regions.collect()
        } else {
            let corners = rounded_corners(self.geometry, self.corner_radius).map(|mut rect| {
                rect.loc -= elem_loc;
                rect
            });
            OpaqueRegions::from_slice(&Rectangle::subtract_rects_many(regions, corners))
        }
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for ClippedSurfaceRenderElement<GlesRenderer> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.compute_uniforms());
        let res = RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        );
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // Clipped content must composite; never hand it to a plane.
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>>
    for ClippedSurfaceRenderElement<TtyRenderer<'render>>
{
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        frame
            .as_gles_frame()
            .override_default_tex_program(self.program.clone(), self.compute_uniforms());
        let res = RenderElement::draw(&self.inner, frame, src, dst, damage, opaque_regions, cache);
        frame.as_gles_frame().clear_tex_program_override();
        res
    }

    fn underlying_storage(
        &self,
        _renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        // Clipped content must composite; never hand it to a plane.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_squares_sit_in_the_corners() {
        let geo = Rectangle::<i32, Physical>::new(Point::from((10, 20)), Size::from((100, 50)));
        let [tl, tr, br, bl] = rounded_corners(geo, 8.0);
        assert_eq!(
            tl,
            Rectangle::new(Point::from((10, 20)), Size::from((8, 8)))
        );
        assert_eq!(tr.loc, Point::from((102, 20)));
        assert_eq!(br.loc, Point::from((102, 62)));
        assert_eq!(bl.loc, Point::from((10, 62)));
        for rect in [tl, tr, br, bl] {
            assert!(geo.contains_rect(rect));
        }
    }

    #[test]
    fn fractional_radius_rounds_up() {
        let geo = Rectangle::<i32, Physical>::new(Point::from((0, 0)), Size::from((40, 40)));
        let [tl, ..] = rounded_corners(geo, 7.3);
        assert_eq!(tl.size, Size::from((8, 8)));
    }
}
