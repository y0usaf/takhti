//! Output capture: shared render-to-buffer helpers and the fulfillment paths
//! for wlr-screencopy and ext-image-copy-capture.
//!
//! Captures always render on the primary GPU's `GlesRenderer`:
//! dmabuf targets are bound directly as the framebuffer (zero-copy, completion
//! signaled via a sync fence), shm targets render into a throwaway texture and
//! read back synchronously. The scene is rebuilt with the same element
//! constructors as the on-screen paths, so per-queue damage trackers can diff
//! frames by stable element ids.

use std::collections::HashMap;
use std::ptr;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as AllocBuffer, Fourcc, Modifier};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::solid::SolidColorBuffer;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{AsRenderElements, RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen};
use smithay::desktop::Window;
use smithay::input::pointer::CursorImageStatus;
use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::image_capture_source::ImageCaptureSource;
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, DmabufConstraints, Frame, SessionRef,
};
use smithay::wayland::shm;
use tracing::{trace, warn};

use crate::backend::Backend;
use crate::protocols::screencopy::{Screencopy, ScreencopyBuffer};
use crate::render::OutputRenderElements;
use crate::space::PhysicalSpace;
use crate::state::Tomoe;
use crate::ui::Ui;

type CaptureElement = RelocateRenderElement<OutputRenderElements<GlesRenderer>>;

/// Split-borrow bundle of everything scene construction needs from [`Tomoe`],
/// so the capture entry points can hold the renderer (borrowed from
/// `backend`) at the same time.
struct SceneParts<'a> {
    space: &'a PhysicalSpace,
    ui: &'a mut Ui,
    border_buffers: &'a HashMap<Window, [SolidColorBuffer; 4]>,
    border_width: i32,
    corner_damage: &'a HashMap<Window, crate::render::damage::ExtraDamage>,
    corner_radius: i32,
    cursor: &'a crate::cursor::Cursor,
    cursor_status: &'a CursorImageStatus,
    cursor_fallback: &'a SolidColorBuffer,
    pointer_pos: Point<f64, smithay::utils::Logical>,
    /// Session locked: captures see the locked scene, never the session.
    locked: bool,
    lock_surfaces: &'a HashMap<Output, smithay::wayland::session_lock::LockSurface>,
    lock_backdrops: &'a mut HashMap<Output, SolidColorBuffer>,
}

impl<'a> SceneParts<'a> {
    /// The full scene for `output` shifted by `-region_loc`, wrapped so both
    /// full-output and region captures share one element type (a zero offset
    /// keeps element ids stable either way).
    fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        region_loc: Point<i32, Physical>,
        include_cursor: bool,
    ) -> Vec<CaptureElement> {
        let Some(geo) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        let scale = self.space.scale();

        let mut elements: Vec<OutputRenderElements<GlesRenderer>> = Vec::new();
        if include_cursor {
            let cursor_phys =
                crate::coords::point_to_physical(self.pointer_pos, scale) - geo.loc.to_f64();
            elements.extend(crate::render::cursor_elements(
                renderer,
                self.cursor_status,
                self.cursor,
                self.cursor_fallback,
                cursor_phys,
                scale,
            ));
        }
        if self.locked {
            // Locked sessions capture the locked scene — anything else would
            // leak session content to screenshots/screenshares.
            elements.extend(crate::lock::lock_elements(
                renderer,
                output,
                geo.size,
                scale,
                self.lock_surfaces.get(output),
                self.lock_backdrops,
            ));
        } else {
            // Captures never include the screenshot selection overlay: the
            // screenshot itself must not contain it, and screencopy/screencast
            // consumers shouldn't record it either.
            let ui_elements = self.ui.render_elements(renderer, output, geo.size, false);
            let borders = crate::render::border_elements(
                self.space,
                self.border_buffers,
                self.border_width,
                geo.loc,
            );
            elements.extend(crate::render::scene_elements(
                renderer,
                self.space,
                output,
                ui_elements,
                borders,
                self.corner_radius,
                self.corner_damage,
            ));
        }

        let offset = region_loc.upscale(-1);
        elements
            .into_iter()
            .map(|element| RelocateRenderElement::from_element(element, offset, Relocate::Relative))
            .collect()
    }

    /// One window's surface tree with its geometry origin at the buffer
    /// origin (CSD shadows outside the geometry crop away), unaffected by the
    /// camera. The cursor embeds only while it actually hovers the window. A
    /// locked session renders nothing — transparent frames instead of window
    /// content.
    fn window_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        include_cursor: bool,
    ) -> Vec<CaptureElement> {
        if self.locked {
            return Vec::new();
        }
        let scale = self.space.scale();
        let mut elements: Vec<OutputRenderElements<GlesRenderer>> = Vec::new();
        if include_cursor {
            if let Some(geo) = self.space.element_geometry(window) {
                // Windows live in world coordinates; the pointer in screen.
                let pointer_world = self
                    .space
                    .screen_to_world(crate::coords::point_to_physical(self.pointer_pos, scale));
                if geo.to_f64().contains(pointer_world) {
                    elements.extend(crate::render::cursor_elements(
                        renderer,
                        self.cursor_status,
                        self.cursor,
                        self.cursor_fallback,
                        pointer_world - geo.loc.to_f64(),
                        scale,
                    ));
                }
            }
        }
        // Same origin math as the on-screen path: the stored location is the
        // geometry origin, the buffer shifts by the client's geometry offset.
        let buffer_offset =
            crate::coords::logical_point_to_physical(window.geometry().loc.to_f64(), scale);
        elements.extend(
            window.render_elements::<OutputRenderElements<GlesRenderer>>(
                renderer,
                Point::from((0, 0)) - buffer_offset,
                Scale::from(scale),
                1.0,
            ),
        );
        elements
            .into_iter()
            .map(|element| {
                RelocateRenderElement::from_element(
                    element,
                    Point::from((0, 0)),
                    Relocate::Relative,
                )
            })
            .collect()
    }
}

/// Destructure [`Tomoe`] into scene parts + the leftovers the entry points
/// need alongside them.
macro_rules! split_tomoe {
    ($tomoe:expr) => {{
        // Captures render outside the backends' redraw, so they refresh
        // border buffers themselves before borrowing them immutably.
        $tomoe.refresh_borders();
        let border_width = $tomoe.lua.settings().border_width;
        let corner_radius = $tomoe.lua.settings().corner_radius;
        let pointer_pos = $tomoe
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        let locked = $tomoe.is_locked();
        let Tomoe {
            backend,
            space,
            ui,
            border_buffers,
            corner_damage,
            cursor,
            cursor_status,
            cursor_fallback,
            screencopy_state,
            loop_handle,
            clock,
            lock_surfaces,
            lock_backdrops,
            ..
        } = $tomoe;
        (
            SceneParts {
                space,
                ui,
                border_buffers,
                border_width,
                corner_damage,
                corner_radius,
                cursor,
                cursor_status,
                cursor_fallback,
                pointer_pos,
                locked,
                lock_surfaces,
                lock_backdrops,
            },
            backend,
            screencopy_state,
            loop_handle,
            Duration::from(clock.now()),
        )
    }};
}

// ─── wlr-screencopy fulfillment ───────────────────────────────────────────────

/// Immediate (`copy`) path: render the current scene into the client buffer
/// right away. Failure drops the [`Screencopy`], which sends `failed`.
pub fn render_screencopy(
    tomoe: &mut Tomoe,
    manager: &ZwlrScreencopyManagerV1,
    screencopy: Screencopy,
) {
    let (mut parts, backend, screencopy_state, loop_handle, now) = split_tomoe!(tomoe);
    let scale = parts.space.scale();

    backend.with_primary_gles(|renderer| {
        let output = screencopy.output().clone();
        let elements = parts.elements(
            renderer,
            &output,
            screencopy.region_loc(),
            screencopy.overlay_cursor(),
        );

        let Some(damage_tracker) = screencopy_state.damage_tracker(manager) else {
            warn!("screencopy queue missing for immediate copy");
            return;
        };
        let (_damages, states) = diff_damage(scale, &elements, damage_tracker, &screencopy);
        match render_into(
            renderer,
            damage_tracker,
            &elements,
            states,
            screencopy.buffer(),
        ) {
            Ok(sync) => screencopy.submit_after_sync(false, sync, now, loop_handle),
            Err(err) => {
                // Recreate the tracker to report full damage next check.
                *damage_tracker = OutputDamageTracker::new((0, 0), 1.0, Transform::Normal);
                warn!("error rendering for screencopy: {err:#}");
            }
        }
    });
}

/// Queued (`copy_with_damage`) path, run from the redraw loop after every
/// on-screen render of `output`: complete at most one frame per queue, and
/// only once its damage tracker sees a change.
pub fn render_queued_screencopies(tomoe: &mut Tomoe, output: &Output) {
    let (mut parts, backend, screencopy_state, loop_handle, now) = split_tomoe!(tomoe);
    let scale = parts.space.scale();

    backend.with_primary_gles(|renderer| {
        screencopy_state.with_queues_mut(|queue| {
            let (damage_tracker, screencopy) = queue.split();
            let Some(screencopy) = screencopy else { return };
            if screencopy.output() != output {
                return;
            }

            let elements = parts.elements(
                renderer,
                output,
                screencopy.region_loc(),
                screencopy.overlay_cursor(),
            );
            let (damages, states) = diff_damage(scale, &elements, damage_tracker, screencopy);
            let Some(damages) = damages else {
                trace!("screencopy: no damage yet, waiting for the next redraw");
                return;
            };
            // Report damage in buffer coordinates. Outputs are untransformed
            // (Transform::Normal), so this is physical-rect passthrough.
            let size = screencopy.buffer_size();
            let damages: Vec<Rectangle<i32, smithay::utils::Buffer>> = damages
                .iter()
                .map(|dmg| {
                    dmg.to_logical(1)
                        .to_buffer(1, Transform::Normal, &size.to_logical(1))
                })
                .collect();
            screencopy.damage(damages.into_iter());

            match render_into(
                renderer,
                damage_tracker,
                &elements,
                states,
                screencopy.buffer(),
            ) {
                Ok(sync) => {
                    queue.pop().submit_after_sync(false, sync, now, loop_handle);
                }
                Err(err) => {
                    *damage_tracker = OutputDamageTracker::new((0, 0), 1.0, Transform::Normal);
                    queue.pop();
                    warn!("error rendering for screencopy: {err:#}");
                }
            }
        });
    });
}

/// Re-arm a queue's damage tracker for the frame's mode and diff the scene
/// against the previous capture.
fn diff_damage<'a>(
    scale: f64,
    elements: &[CaptureElement],
    damage_tracker: &'a mut OutputDamageTracker,
    screencopy: &Screencopy,
) -> (
    Option<&'a Vec<Rectangle<i32, Physical>>>,
    RenderElementStates,
) {
    retrack(damage_tracker, screencopy.buffer_size(), scale);
    damage_tracker.damage_output(1, elements).unwrap()
}

/// Point a static-mode damage tracker at a (possibly changed) capture size.
fn retrack(damage_tracker: &mut OutputDamageTracker, size: Size<i32, Physical>, scale: f64) {
    let scale = Scale::from(scale);
    match damage_tracker.mode() {
        smithay::output::OutputModeSource::Static {
            size: last_size,
            scale: last_scale,
            transform: last_transform,
        } if *last_size == size && *last_scale == scale && *last_transform == Transform::Normal => {
        }
        _ => *damage_tracker = OutputDamageTracker::new(size, scale, Transform::Normal),
    }
}

fn render_into(
    renderer: &mut GlesRenderer,
    damage_tracker: &mut OutputDamageTracker,
    elements: &[impl RenderElement<GlesRenderer>],
    states: RenderElementStates,
    buffer: &ScreencopyBuffer,
) -> Result<Option<SyncPoint>> {
    match buffer {
        ScreencopyBuffer::Dmabuf(dmabuf) => {
            let sync = render_to_dmabuf(renderer, damage_tracker, dmabuf.clone(), elements, states)
                .context("error rendering to screencopy dmabuf")?;
            Ok(Some(sync))
        }
        ScreencopyBuffer::Shm(wl_buffer) => {
            render_to_shm(renderer, damage_tracker, wl_buffer, elements, states)
                .context("error rendering to screencopy shm buffer")?;
            Ok(None)
        }
    }
}

// ─── ext-image-copy-capture fulfillment ───────────────────────────────────────

/// What an ext-image-capture source points at.
enum CaptureTarget {
    Output(Output),
    Window(Window),
}

/// Resolve an ext-image-capture source back to its live output or window.
fn source_target(tomoe: &Tomoe, source: &ImageCaptureSource) -> Option<CaptureTarget> {
    if let Some(weak) = source.user_data().get::<WeakOutput>() {
        let output = weak.upgrade()?;
        return tomoe
            .space
            .outputs()
            .any(|o| *o == output)
            .then_some(CaptureTarget::Output(output));
    }
    let weak = source
        .user_data()
        .get::<smithay::wayland::foreign_toplevel_list::ForeignToplevelWeakHandle>()?;
    let handle = weak.upgrade()?;
    let id = handle
        .user_data()
        .get::<crate::foreign_toplevel::ForeignWindowId>()?
        .0;
    tomoe.windows.get(&id).cloned().map(CaptureTarget::Window)
}

/// Physical size of a window capture: the client's committed geometry at the
/// global scale (the same rounding placement uses), floored at 1×1 so a
/// pre-first-commit window still yields valid constraints.
fn window_capture_size(space: &PhysicalSpace, window: &Window) -> Size<i32, Physical> {
    let size =
        crate::coords::logical_size_to_physical(window.geometry().size.to_f64(), space.scale());
    Size::from((size.w.max(1), size.h.max(1)))
}

/// Buffer constraints for capturing `source`, or None if its target is gone.
pub fn constraints_for_source(
    tomoe: &mut Tomoe,
    source: &ImageCaptureSource,
) -> Option<BufferConstraints> {
    let size = match source_target(tomoe, source)? {
        CaptureTarget::Output(output) => tomoe.space.output_geometry(&output)?.size,
        CaptureTarget::Window(window) => window_capture_size(&tomoe.space, &window),
    };
    Some(BufferConstraints {
        size: Size::from((size.w, size.h)),
        shm: vec![wl_shm::Format::Xrgb8888],
        dma: tomoe.backend.dmabuf_constraints(),
    })
}

/// Re-announce constraints after any output or window change: stop sessions
/// whose target is gone, renegotiate buffers for sessions whose size changed.
pub fn refresh_capture_sessions(tomoe: &mut Tomoe) {
    if tomoe.capture_sessions.is_empty() {
        return;
    }
    let mut sessions = std::mem::take(&mut tomoe.capture_sessions);
    sessions.retain(|session| {
        let session = session.as_ref();
        if !smithay::utils::IsAlive::alive(&session) {
            return false;
        }
        match constraints_for_source(tomoe, &session.source()) {
            Some(constraints) => {
                let current = session.current_constraints().map(|c| c.size);
                if current != Some(constraints.size) {
                    session.update_constraints(constraints);
                }
                true
            }
            // Dropping the owned Session sends `stopped`.
            None => false,
        }
    });
    tomoe.capture_sessions = sessions;
    tomoe.image_copy_capture_state.cleanup();
}

/// Complete queued ext-image-copy-capture frames after an on-screen render.
/// Frames queue on the capture request instead of rendering right away: an
/// immediate answer would let a capture→ready→capture client (our portal)
/// spin unthrottled, while completing from the redraw path paces casts to
/// vblank. The request itself queues a redraw, so static content still
/// yields a first frame promptly.
pub fn complete_capture_frames(tomoe: &mut Tomoe) {
    if tomoe.pending_capture_frames.is_empty() {
        return;
    }
    let pending = std::mem::take(&mut tomoe.pending_capture_frames);
    for (session, frame) in pending {
        render_capture_frame(tomoe, &session, frame);
    }
}

/// Fulfill one ext-image-copy-capture frame: render the source's current
/// scene (an output's, or a single window's) into the attached buffer and
/// signal success, or fail with the closest protocol reason.
pub fn render_capture_frame(tomoe: &mut Tomoe, session: &SessionRef, frame: Frame) {
    let Some(target) = source_target(tomoe, &session.source()) else {
        frame.fail(CaptureFailureReason::Stopped);
        return;
    };
    let include_cursor = session.draw_cursor();

    let (mut parts, backend, _screencopy_state, _loop_handle, now) = split_tomoe!(tomoe);
    let scale = parts.space.scale();
    let size = match &target {
        CaptureTarget::Output(output) => {
            let Some(geo) = parts.space.output_geometry(output) else {
                frame.fail(CaptureFailureReason::Stopped);
                return;
            };
            geo.size
        }
        CaptureTarget::Window(window) => window_capture_size(parts.space, window),
    };

    let res = backend.with_primary_gles(|renderer| {
        let elements = match &target {
            CaptureTarget::Output(output) => {
                parts.elements(renderer, output, Point::from((0, 0)), include_cursor)
            }
            CaptureTarget::Window(window) => {
                parts.window_elements(renderer, window, include_cursor)
            }
        };
        // Fresh tracker per frame: clients rotate buffers, so cross-frame
        // damage diffing can't be trusted; render everything (age 0).
        let mut damage_tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        let (_damages, states) = damage_tracker.damage_output(1, &elements).unwrap();
        let states = states;

        let buffer = frame.buffer();
        if let Ok(dmabuf) = get_dmabuf(&buffer) {
            let sync = render_to_dmabuf(
                renderer,
                &mut damage_tracker,
                dmabuf.clone(),
                &elements,
                states,
            )?;
            // ready() has no fence to ride on, so wait for the GPU here.
            if let Err(err) = sync.wait() {
                warn!("error waiting for capture frame completion: {err:?}");
            }
            Ok(())
        } else {
            render_to_shm(renderer, &mut damage_tracker, &buffer, &elements, states)
        }
    });

    match res {
        Some(Ok(())) => frame.success(Transform::Normal, None, now),
        Some(Err(err)) => {
            warn!("error rendering capture frame: {err:#}");
            frame.fail(CaptureFailureReason::BufferConstraints);
        }
        None => frame.fail(CaptureFailureReason::Unknown),
    }
}

// ─── Render-to-buffer helpers ─────────────────────────────────────────────────

/// Render the current scene of `output` — cropped to `region` (output-local
/// physical coordinates) when given — into an offscreen texture and read the
/// pixels back as tightly packed RGBA8. Rows come out top-down: smithay's
/// texture render targets flip the projection, so `copy_framebuffer` returns
/// the same orientation [`render_to_shm`] copies verbatim into shm buffers.
pub fn capture_rgba(
    tomoe: &mut Tomoe,
    output: &Output,
    region: Option<Rectangle<i32, Physical>>,
) -> Result<(Size<i32, Physical>, Vec<u8>)> {
    let (mut parts, backend, _screencopy_state, _loop_handle, _now) = split_tomoe!(tomoe);
    let scale = parts.space.scale();
    let geo = parts
        .space
        .output_geometry(output)
        .context("output has no geometry")?;
    let region = region.unwrap_or_else(|| Rectangle::from_size(geo.size));
    ensure!(
        region.size.w > 0 && region.size.h > 0,
        "empty capture region"
    );

    let pixels = backend
        .with_primary_gles(|renderer| -> Result<Vec<u8>> {
            let elements = parts.elements(renderer, output, region.loc, true);
            let mut damage_tracker =
                OutputDamageTracker::new(region.size, scale, Transform::Normal);
            let (_damages, states) = damage_tracker.damage_output(1, &elements).unwrap();

            // GL RGBA8: mapped bytes come back in R,G,B,A order, PNG-ready.
            let fourcc = Fourcc::Abgr8888;
            let mut texture = create_texture(renderer, region.size, fourcc)?;
            let mut target = renderer
                .bind(&mut texture)
                .context("error binding texture")?;
            let _res = damage_tracker
                .render_output_with_states(
                    renderer,
                    &mut target,
                    0,
                    &elements,
                    Color32F::TRANSPARENT,
                    states,
                )
                .context("error rendering")?;

            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size(region.size.to_logical(1).to_buffer(1, Transform::Normal)),
                    fourcc,
                )
                .context("error copying framebuffer")?;
            let bytes = renderer
                .map_texture(&mapping)
                .context("error mapping texture")?;
            Ok(bytes.to_vec())
        })
        .context("no renderer available")??;

    Ok((region.size, pixels))
}

fn create_texture(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    fourcc: Fourcc,
) -> Result<GlesTexture> {
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    renderer
        .create_buffer(fourcc, buffer_size)
        .context("error creating texture")
}

fn render_to_dmabuf(
    renderer: &mut GlesRenderer,
    damage_tracker: &mut OutputDamageTracker,
    mut dmabuf: Dmabuf,
    elements: &[impl RenderElement<GlesRenderer>],
    states: RenderElementStates,
) -> Result<SyncPoint> {
    let (size, _scale, _transform) = damage_tracker.mode().clone().try_into()?;
    let size: Size<i32, Physical> = size;
    ensure!(
        dmabuf.width() == size.w as u32 && dmabuf.height() == size.h as u32,
        "invalid buffer size"
    );

    let mut target = renderer.bind(&mut dmabuf).context("error binding dmabuf")?;
    let res = damage_tracker
        .render_output_with_states(
            renderer,
            &mut target,
            0,
            elements,
            Color32F::TRANSPARENT,
            states,
        )
        .context("error rendering to dmabuf")?;
    Ok(res.sync)
}

fn render_to_shm(
    renderer: &mut GlesRenderer,
    damage_tracker: &mut OutputDamageTracker,
    buffer: &WlBuffer,
    elements: &[impl RenderElement<GlesRenderer>],
    states: RenderElementStates,
) -> Result<()> {
    shm::with_buffer_contents_mut(buffer, |shm_buffer, shm_len, buffer_data| {
        let (size, _scale, _transform) = damage_tracker.mode().clone().try_into()?;
        let size: Size<i32, Physical> = size;
        let fourcc = Fourcc::Xrgb8888;

        ensure!(
            buffer_data.format == wl_shm::Format::Xrgb8888
                && buffer_data.width == size.w
                && buffer_data.height == size.h
                && buffer_data.stride == size.w * 4
                && shm_len == buffer_data.stride as usize * buffer_data.height as usize,
            "invalid buffer format or size"
        );

        let mut texture = create_texture(renderer, size, fourcc)?;
        let mut target = renderer
            .bind(&mut texture)
            .context("error binding texture")?;

        let _res = damage_tracker
            .render_output_with_states(
                renderer,
                &mut target,
                0,
                elements,
                Color32F::TRANSPARENT,
                states,
            )
            .context("error rendering")?;

        let mapping = renderer
            .copy_framebuffer(
                &target,
                Rectangle::from_size(size.to_logical(1).to_buffer(1, Transform::Normal)),
                fourcc,
            )
            .context("error copying framebuffer")?;
        let bytes = renderer
            .map_texture(&mapping)
            .context("error mapping texture")?;

        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), shm_buffer.cast(), shm_len);
        }

        Ok(())
    })
    .context("expected shm buffer, but didn't get one")?
}

/// Modifiers we can render Xrgb8888 into, from an EGL context.
fn render_modifiers(egl: &smithay::backend::egl::EGLContext) -> Vec<Modifier> {
    let mut modifiers: Vec<Modifier> = egl
        .dmabuf_render_formats()
        .iter()
        .filter(|format| format.code == Fourcc::Xrgb8888)
        .map(|format| format.modifier)
        .collect();
    modifiers.sort_unstable_by_key(|m| u64::from(*m));
    modifiers.dedup();
    modifiers
}

impl Backend {
    /// Run `f` with the primary GPU's `GlesRenderer` (the winit renderer, or
    /// the primary render node on TTY). Captures always render here; outputs
    /// on other GPUs are composited on the primary anyway.
    pub fn with_primary_gles<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> Option<T> {
        match self {
            Backend::Uninit => None,
            Backend::Winit(data) => Some(f(data.backend.renderer())),
            Backend::Tty(data) => {
                let mut renderer = data
                    .gpu_manager
                    .single_renderer(&data.primary_render_node)
                    .ok()?;
                let gles: &mut GlesRenderer = renderer.as_mut();
                Some(f(gles))
            }
        }
    }

    /// Dmabuf allocation constraints for capture clients: the render node
    /// and the formats we can bind as a render target. None when no node can
    /// be determined — those clients fall back to shm.
    pub fn dmabuf_constraints(&mut self) -> Option<DmabufConstraints> {
        let (node, modifiers) = match self {
            Backend::Uninit => return None,
            Backend::Winit(data) => {
                let renderer = data.backend.renderer();
                let node = crate::backend::winit::render_node(renderer).ok()?;
                (node, render_modifiers(renderer.egl_context()))
            }
            Backend::Tty(data) => {
                let renderer = data
                    .gpu_manager
                    .single_renderer(&data.primary_render_node)
                    .ok()?;
                (
                    data.primary_render_node,
                    render_modifiers(renderer.as_ref().egl_context()),
                )
            }
        };
        if modifiers.is_empty() {
            return None;
        }
        Some(DmabufConstraints {
            node,
            formats: vec![(Fourcc::Xrgb8888, modifiers)],
        })
    }
}
