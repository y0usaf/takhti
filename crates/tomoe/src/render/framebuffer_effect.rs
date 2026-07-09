use std::cell::RefCell;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{ffi, GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Frame as _, FrameContext, Offscreen, Texture as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};

use super::blur::{Blur, BlurOptions};
use super::renderer::AsGlesFrame as _;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

#[derive(Debug, Clone, Copy, PartialEq)]
struct EffectState {
    geometry: Rectangle<i32, Physical>,
    visible_geometry: Rectangle<i32, Physical>,
    options: BlurOptions,
}

/// Persistent identity for a blur-behind rectangle. Keep one per layer surface
/// so Smithay's damage tracker and framebuffer-effect cache survive frames.
#[derive(Debug)]
pub struct FramebufferEffect {
    id: Id,
    commit: CommitCounter,
    last: Option<EffectState>,
}

#[derive(Debug)]
pub struct FramebufferEffectElement {
    id: Id,
    commit: CommitCounter,
    /// Source box, including the blur's sampling halo. Smithay uses this
    /// geometry to invalidate framebuffer effects when content behind changes.
    geometry: Rectangle<i32, Physical>,
    /// The rectangle that receives the blurred result. Pixels in the capture
    /// halo are sampled but never drawn.
    visible_geometry: Rectangle<i32, Physical>,
    options: BlurOptions,
}

#[derive(Debug)]
struct Inner {
    framebuffer: Option<GlesTexture>,
    blur: Option<Blur>,
    intermediate: Option<GlesTexture>,
}

impl Default for FramebufferEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl FramebufferEffect {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
            last: None,
        }
    }

    pub fn render(
        &mut self,
        visible_geometry: Rectangle<i32, Physical>,
        options: BlurOptions,
        anti_artifact_margin: i32,
    ) -> FramebufferEffectElement {
        let geometry = expand_rect(visible_geometry, anti_artifact_margin);
        let state = EffectState {
            geometry,
            visible_geometry,
            options,
        };
        if self.last != Some(state) {
            self.commit.increment();
            self.last = Some(state);
        }
        FramebufferEffectElement {
            id: self.id.clone(),
            commit: self.commit,
            geometry,
            visible_geometry,
            options,
        }
    }
}

fn expand_rect(rect: Rectangle<i32, Physical>, amount: i32) -> Rectangle<i32, Physical> {
    let amount = amount.max(0);
    let left = rect.loc.x.saturating_sub(amount);
    let top = rect.loc.y.saturating_sub(amount);
    let right = rect
        .loc
        .x
        .saturating_add(rect.size.w)
        .saturating_add(amount);
    let bottom = rect
        .loc
        .y
        .saturating_add(rect.size.h)
        .saturating_add(amount);
    Rectangle::new(
        (left, top).into(),
        (right.saturating_sub(left), bottom.saturating_sub(top)).into(),
    )
}

impl Element for FramebufferEffectElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(smithay::utils::Size::from((
            self.geometry.size.w as f64,
            self.geometry.size.h as f64,
        )))
    }
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }
    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let output_rect = Rectangle::from_size(frame.output_size());
        let Some(clamped) = dst.intersection(output_rect) else {
            return Ok(());
        };
        let transformed = frame
            .transformation()
            .transform_rect_in(clamped, &output_rect.size);
        let size = transformed
            .size
            .to_logical(1)
            .to_buffer(1, Transform::Normal);

        let mut renderer = frame.renderer();
        let cell = cache.get_or_insert::<RefCell<Inner>, _>(|| {
            RefCell::new(Inner {
                framebuffer: None,
                blur: Blur::new(renderer.as_mut()),
                intermediate: None,
            })
        });
        let mut inner = cell.borrow_mut();
        inner.intermediate = None;
        if inner
            .framebuffer
            .as_ref()
            .is_some_and(|texture| texture.size() != size)
        {
            inner.framebuffer = None;
        }
        if inner.framebuffer.is_none() {
            inner.framebuffer = Some(renderer.as_mut().create_buffer(Fourcc::Abgr8888, size)?);
        }
        let framebuffer = inner.framebuffer.as_ref().cloned();
        let Some(framebuffer) = framebuffer else {
            return Ok(());
        };
        if let Some(blur) = inner.blur.as_mut() {
            blur.prepare_textures(
                |format, texture_size| renderer.as_mut().create_buffer(format, texture_size),
                &framebuffer,
                self.options,
            )
            .map_err(|err| {
                tracing::warn!("error preparing blur textures: {err:#}");
                GlesError::BlitError
            })?;
        }
        drop(renderer);

        frame.with_context(|gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}
            let mut current_fbo = 0i32;
            gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo);
            gl.Disable(ffi::SCISSOR_TEST);
            let mut fbo = 0;
            gl.GenFramebuffers(1, &mut fbo);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                framebuffer.tex_id(),
                0,
            );
            gl.BlitFramebuffer(
                transformed.loc.x,
                transformed.loc.y,
                transformed.loc.x + transformed.size.w,
                transformed.loc.y + transformed.size.h,
                0,
                0,
                size.w,
                size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::LINEAR,
            );
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
            gl.Enable(ffi::SCISSOR_TEST);
            gl.DeleteFramebuffers(1, &fbo);
            if gl.GetError() == ffi::NO_ERROR {
                Ok(())
            } else {
                Err(GlesError::BlitError)
            }
        })??;

        if let Some(blur) = inner.blur.as_mut() {
            let mut renderer = frame.renderer();
            match blur.render(renderer.as_mut(), &framebuffer, self.options) {
                Ok(texture) => inner.intermediate = Some(texture),
                Err(err) => tracing::warn!("error rendering blur: {err:#}"),
            }
        }
        Ok(())
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let Some(inner) = cache.and_then(|cache| cache.get::<RefCell<Inner>>()) else {
            return Ok(());
        };
        let inner = inner.borrow();
        let Some(texture) = inner.intermediate.as_ref() else {
            return Ok(());
        };
        let output_rect = Rectangle::from_size(frame.output_size());
        let Some(visible) = self.visible_geometry.intersection(output_rect) else {
            return Ok(());
        };
        let Some(capture) = dst.intersection(output_rect) else {
            return Ok(());
        };
        let transformed_capture = frame
            .transformation()
            .transform_rect_in(capture, &output_rect.size);
        let transformed_visible = frame
            .transformation()
            .transform_rect_in(visible, &output_rect.size);
        let source: Rectangle<f64, Buffer> = Rectangle::new(
            (
                f64::from(transformed_visible.loc.x - transformed_capture.loc.x),
                f64::from(transformed_visible.loc.y - transformed_capture.loc.y),
            )
                .into(),
            (
                f64::from(transformed_visible.size.w),
                f64::from(transformed_visible.size.h),
            )
                .into(),
        );
        let visible_damage = damage
            .iter()
            .filter_map(|rect| {
                let global = Rectangle::new(rect.loc + dst.loc, rect.size);
                global.intersection(visible).map(|mut rect| {
                    rect.loc -= visible.loc;
                    rect
                })
            })
            .collect::<Vec<_>>();
        if visible_damage.is_empty() {
            return Ok(());
        }
        frame.render_texture_from_to(
            texture,
            source,
            visible,
            &visible_damage,
            &[],
            frame.transformation().invert(),
            1.0,
            None,
            &[],
        )
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        RenderElement::<GlesRenderer>::capture_framebuffer(
            self,
            frame.as_gles_frame(),
            src,
            dst,
            cache,
        )?;
        Ok(())
    }
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        RenderElement::<GlesRenderer>::draw(
            self,
            frame.as_gles_frame(),
            src,
            dst,
            damage,
            opaque,
            cache,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::Element;
    use smithay::utils::{Point, Size};

    #[test]
    fn geometry_and_options_bump_commit_without_changing_identity() {
        let mut effect = FramebufferEffect::new();
        let geometry = Rectangle::new(Point::from((10, 20)), Size::from((300, 40)));
        let options = BlurOptions {
            passes: 3,
            offset: 1.0,
        };

        let first = effect.render(geometry, options, 8);
        let first_id = first.id().clone();
        let first_commit = first.current_commit();
        let unchanged = effect.render(geometry, options, 8);
        assert_eq!(unchanged.id(), &first_id);
        assert_eq!(unchanged.current_commit(), first_commit);

        let moved_geometry = Rectangle::new(Point::from((11, 20)), geometry.size);
        let moved = effect.render(moved_geometry, options, 8);
        assert_eq!(moved.id(), &first_id);
        assert!(moved.current_commit() > first_commit);
        let moved_commit = moved.current_commit();

        let retuned = effect.render(
            moved_geometry,
            BlurOptions {
                passes: 4,
                offset: 2.0,
            },
            8,
        );
        assert_eq!(retuned.id(), &first_id);
        assert!(retuned.current_commit() > moved_commit);
    }

    #[test]
    fn framebuffer_geometry_includes_sampling_halo() {
        let mut effect = FramebufferEffect::new();
        let visible = Rectangle::new(Point::from((100, 200)), Size::from((300, 40)));
        let options = BlurOptions {
            passes: 2,
            offset: 1.0,
        };

        let halo = 80;
        let element = effect.render(visible, options, halo);
        assert_eq!(
            element.geometry(Scale::from(1.0)),
            Rectangle::new(
                Point::from((visible.loc.x - halo, visible.loc.y - halo)),
                Size::from((visible.size.w + halo * 2, visible.size.h + halo * 2)),
            )
        );
        assert_eq!(element.visible_geometry, visible);
    }
}
