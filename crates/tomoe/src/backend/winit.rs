use std::time::Duration;

use crate::backend::Backend;
use crate::state::Tomoe;
use anyhow::{anyhow, Context, Result};
use smithay::backend::drm::DrmNode;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::ImportDma;
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::wayland::WindowAttributesExtWayland;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::Monotonic;
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::presentation::Refresh;
use tracing::debug;

pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub damage_tracker: OutputDamageTracker,
    pub output: Output,
}

/// The DRM render node behind the winit EGL context (for dmabuf feedback and
/// capture constraints).
pub fn render_node(renderer: &GlesRenderer) -> Result<DrmNode> {
    let display = renderer.egl_context().display();
    let device = EGLDevice::device_for_display(display).context("error getting EGL device")?;
    device
        .try_get_render_node()
        .context("error getting EGL device render node")?
        .context("EGL device has no render node")
}

pub fn init(tomoe: &mut Tomoe) -> Result<()> {
    let (width, height) = tomoe.lua.settings().winit_size;
    let attrs = WinitWindow::default_attributes()
        .with_inner_size(LogicalSize::new(width as f64, height as f64))
        .with_title("tomoe")
        .with_name("tomoe", "");
    let (backend, winit_source) =
        winit::init_from_attributes::<GlesRenderer>(attrs).map_err(|err| anyhow!("{err:?}"))?;

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "tomoe".into(),
            model: "winit".into(),
            serial_number: "".into(),
        },
    );
    let _global = output.create_global::<Tomoe>(&tomoe.display_handle);
    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        None,
        Some(Scale::Fractional(tomoe.space.scale())),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    tomoe.space.map_output(&output, (0, 0));

    let damage_tracker = OutputDamageTracker::from_output(&output);
    tomoe.backend = Backend::Winit(WinitData {
        backend,
        damage_tracker,
        output,
    });

    // EGL hardware-acceleration for clients via linux-dmabuf. The legacy
    // wl_drm global is deliberately not bound (niri-shape): smithay's wl_drm
    // rejects Xwayland's format requests with a fatal protocol error, and
    // every current client (Xwayland included) speaks dmabuf.
    //
    // Prefer a v4 global (default feedback pointing at the EGL device's
    // render node) — clients like wf-recorder hard-require v4 — and fall
    // back to v3 when the node can't be determined.
    let display_handle = tomoe.display_handle.clone();
    let winit = tomoe.backend.winit();
    let formats = winit.backend.renderer().dmabuf_formats();
    match render_node(winit.backend.renderer()) {
        Ok(node) => match DmabufFeedbackBuilder::new(node.dev_id(), formats.clone()).build() {
            Ok(feedback) => {
                let _global = tomoe
                    .dmabuf_state
                    .create_global_with_default_feedback::<Tomoe>(&display_handle, &feedback);
            }
            Err(err) => {
                debug!("error building dmabuf feedback, using dmabuf v3: {err}");
                let _global = tomoe
                    .dmabuf_state
                    .create_global::<Tomoe>(&display_handle, formats);
            }
        },
        Err(err) => {
            debug!("error getting EGL render node, using dmabuf v3: {err:#}");
            let _global = tomoe
                .dmabuf_state
                .create_global::<Tomoe>(&display_handle, formats);
        }
    }

    tomoe
        .loop_handle
        .clone()
        .insert_source(winit_source, move |event, _, tomoe| match event {
            WinitEvent::Resized { size, .. } => {
                tomoe.backend.winit().output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
                tomoe.outputs_changed(true);
            }
            WinitEvent::Input(event) => tomoe.process_input_event(event),
            WinitEvent::Redraw => redraw(tomoe),
            WinitEvent::CloseRequested => tomoe.loop_signal.stop(),
            WinitEvent::Focus(_) => {}
        })
        .map_err(|err| anyhow!("error inserting winit event source: {err}"))?;

    // First paint; afterwards redraws are requested on damage only.
    tomoe.backend.winit().backend.window().request_redraw();

    Ok(())
}

pub fn redraw(tomoe: &mut Tomoe) {
    // Render-path refresh: border buffers re-derive from live geometry/focus
    // here, not on scattered events, so they can never be stale for a frame.
    tomoe.refresh_borders();
    let (output_loc, output_size) = {
        let Backend::Winit(winit) = &tomoe.backend else {
            return;
        };
        tomoe
            .space
            .output_geometry(&winit.output)
            .map(|geo| (geo.loc, geo.size))
            .unwrap_or_default()
    };

    let locked = tomoe.is_locked();
    let Tomoe {
        backend,
        space,
        start_time,
        ui,
        binds,
        lua,
        border_buffers,
        clock,
        lock_surfaces,
        lock_backdrops,
        ..
    } = tomoe;
    let Backend::Winit(winit) = backend else {
        return;
    };
    let output = winit.output.clone();
    let elements = if locked {
        // Locked: the lock surface over a solid backdrop, nothing else.
        crate::lock::lock_elements(
            winit.backend.renderer(),
            &output,
            output_size,
            space.scale(),
            lock_surfaces.get(&output),
            lock_backdrops,
        )
    } else {
        let borders = crate::render::border_elements(
            space,
            border_buffers,
            lua.settings().border_width,
            output_loc,
        );
        // Compositor UI (dialogs/overlays) first: earlier elements render on top.
        let ui_elements =
            ui.render_elements(winit.backend.renderer(), &output, output_size, binds, true);
        crate::render::scene_elements(
            winit.backend.renderer(),
            space,
            &output,
            ui_elements,
            borders,
        )
    };

    let res = {
        let (renderer, mut framebuffer) = winit.backend.bind().unwrap();
        winit
            .damage_tracker
            .render_output(
                renderer,
                &mut framebuffer,
                0,
                &elements,
                [0.05, 0.05, 0.05, 1.0],
            )
            .unwrap()
    };

    if let Some(damage) = res.damage {
        winit.backend.submit(Some(damage)).unwrap();
        // No real vblank here; approximate presentation as "now" at the
        // output's nominal refresh so presentation-time clients keep pacing.
        let mut feedback = crate::render::take_presentation_feedback(
            space,
            &winit.output,
            lock_surfaces.get(&winit.output),
            &res.states,
        );
        let refresh = winit
            .output
            .current_mode()
            .filter(|mode| mode.refresh > 0)
            .map(|mode| Refresh::fixed(Duration::from_secs_f64(1_000f64 / mode.refresh as f64)))
            .unwrap_or(Refresh::Unknown);
        feedback.presented::<_, Monotonic>(
            clock.now(),
            refresh,
            0,
            wp_presentation_feedback::Kind::Vsync,
        );
    }

    let time = start_time.elapsed();
    for window in space.elements() {
        window.send_frame(&output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    // Lock surfaces are outside the space; they animate on frame callbacks
    // like anything else.
    if let Some(surface) = lock_surfaces.get(&output) {
        smithay::desktop::utils::send_frames_surface_tree(
            surface.wl_surface(),
            &output,
            time,
            Some(Duration::ZERO),
            |_, _| Some(output.clone()),
        );
    }

    // While locking, confirmation waits until every output shows a locked
    // frame; this render just produced one for this output.
    tomoe.lock_frame_rendered(&output);

    // Complete queued with-damage screencopies against the just-rendered
    // scene, mirroring the TTY backend's post-present pass.
    // Ext-image-copy-capture frames complete here too (redraw-paced).
    crate::capture::render_queued_screencopies(tomoe, &output);
    crate::capture::complete_capture_frames(tomoe);

    // Damage-driven: the next repaint comes from queue_redraw_all() (commits,
    // Lua ops, input), mirroring the TTY backend so missing damage sources
    // show up as visible freezes during development.
}
