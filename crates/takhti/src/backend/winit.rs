use std::time::Duration;

use anyhow::{anyhow, Result};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::platform::wayland::WindowAttributesExtWayland;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::Monotonic;
use smithay::wayland::presentation::Refresh;
use tracing::debug;

use crate::backend::Backend;
use crate::state::Takhti;

pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub damage_tracker: OutputDamageTracker,
    pub output: Output,
}

pub fn init(takhti: &mut Takhti) -> Result<()> {
    let (width, height) = takhti.lua.settings().winit_size;
    let attrs = WinitWindow::default_attributes()
        .with_inner_size(LogicalSize::new(width as f64, height as f64))
        .with_title("takhti")
        .with_name("takhti", "");
    let (backend, winit_source) =
        winit::init_from_attributes::<GlesRenderer>(attrs).map_err(|err| anyhow!("{err:?}"))?;

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "takhti".into(),
            model: "winit".into(),
            serial_number: "".into(),
        },
    );
    let _global = output.create_global::<Takhti>(&takhti.display_handle);
    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        None,
        Some(Scale::Fractional(takhti.space.scale())),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    takhti.space.map_output(&output, (0, 0));

    let damage_tracker = OutputDamageTracker::from_output(&output);
    takhti.backend = Backend::Winit(WinitData {
        backend,
        damage_tracker,
        output,
    });

    // EGL hardware-acceleration for clients (legacy wl_drm + dmabuf global).
    let display_handle = takhti.display_handle.clone();
    let winit = takhti.backend.winit();
    if let Err(err) = winit.backend.renderer().bind_wl_display(&display_handle) {
        debug!("error binding legacy EGL display (expected on modern systems): {err}");
    }
    let formats = winit.backend.renderer().dmabuf_formats();
    let _dmabuf_global = takhti
        .dmabuf_state
        .create_global::<Takhti>(&display_handle, formats);

    takhti
        .loop_handle
        .clone()
        .insert_source(winit_source, move |event, _, takhti| match event {
            WinitEvent::Resized { size, .. } => {
                takhti.backend.winit().output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
                takhti.outputs_changed(true);
            }
            WinitEvent::Input(event) => takhti.process_input_event(event),
            WinitEvent::Redraw => redraw(takhti),
            WinitEvent::CloseRequested => takhti.loop_signal.stop(),
            WinitEvent::Focus(_) => {}
        })
        .map_err(|err| anyhow!("error inserting winit event source: {err}"))?;

    // First paint; afterwards redraws are requested on damage only.
    takhti.backend.winit().backend.window().request_redraw();

    Ok(())
}

pub fn redraw(takhti: &mut Takhti) {
    let (output_loc, output_size) = {
        let Backend::Winit(winit) = &takhti.backend else {
            return;
        };
        takhti
            .space
            .output_geometry(&winit.output)
            .map(|geo| (geo.loc, geo.size))
            .unwrap_or_default()
    };

    let Takhti {
        backend,
        space,
        start_time,
        ui,
        binds,
        lua,
        border_buffers,
        clock,
        ..
    } = takhti;
    let Backend::Winit(winit) = backend else {
        return;
    };
    let borders = crate::render::border_elements(
        space,
        border_buffers,
        lua.settings().border_width,
        output_loc,
    );

    // Compositor UI (dialogs/overlays) first: earlier elements render on top.
    let ui_elements = ui.render_elements(winit.backend.renderer(), output_size, binds);
    let output = winit.output.clone();
    let elements = crate::render::scene_elements(
        winit.backend.renderer(),
        space,
        &output,
        ui_elements,
        borders,
    );

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
        let mut feedback =
            crate::render::take_presentation_feedback(space, &winit.output, &res.states);
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
    let output = winit.output.clone();
    for window in space.elements() {
        window.send_frame(&output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }

    // Damage-driven: the next repaint comes from queue_redraw_all() (commits,
    // Lua ops, input), mirroring the TTY backend so missing damage sources
    // show up as visible freezes during development.
}
