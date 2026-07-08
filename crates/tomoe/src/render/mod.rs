//! Render element construction, all in integer physical coordinates.
//!
//! Every element is positioned at `Point<i32, Physical>` so client buffers
//! are sampled 1:1 on the pixel grid — the whole point of the physical-first
//! coordinate space. smithay's `space_render_elements` is not used: it
//! positions from integer *logical* locations and rounds each element
//! independently, which drifts decorations a pixel away from windows at
//! fractional scales.
//!
//! Everything is generic over [`TomoeRenderer`] so the same scene code
//! drives the winit backend's `GlesRenderer` and the TTY backend's
//! multi-GPU `MultiRenderer`. Shader elements (rounded corners) draw
//! through the underlying Gles context ([`renderer::AsGlesRenderer`]), so
//! `OutputRenderElements` implements `RenderElement` for exactly those two
//! renderers (see `macros.rs`) instead of generically.

pub mod clipped_surface;
pub mod damage;
mod macros;
pub mod renderer;
pub mod shaders;

use std::collections::HashMap;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Kind, RenderElementStates};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer, Texture};
use smithay::desktop::utils::{
    surface_presentation_feedback_flags_from_states, take_presentation_feedback_surface_tree,
    OutputPresentationFeedback,
};
use smithay::desktop::{layer_map_for_output, PopupManager, Window, WindowSurface};
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{IsAlive, Physical, Point, Rectangle, Scale};
use smithay::wayland::compositor::with_states;
use smithay::wayland::session_lock::LockSurface;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;

use crate::coords;
use crate::cursor::Cursor;
use crate::space::PhysicalSpace;
use clipped_surface::ClippedSurfaceRenderElement;
use damage::ExtraDamage;
use renderer::AsGlesRenderer;
use shaders::Shaders;

/// Renderer bounds for tomoe's render elements, satisfied by both
/// `GlesRenderer` (winit) and `MultiRenderer` (TTY). The associated type
/// pins `TextureId`, since associated type bounds can't be written in a
/// supertrait list directly. `AsGlesRenderer` reaches the Gles context
/// shader elements draw through.
pub trait TomoeRenderer:
    ImportAll + ImportMem + Renderer<TextureId = Self::TomoeTextureId> + AsGlesRenderer
{
    type TomoeTextureId: Texture + Clone + Send + 'static;
}

impl<R> TomoeRenderer for R
where
    R: ImportAll + ImportMem + AsGlesRenderer,
    R::TextureId: Texture + Clone + Send + 'static,
{
    type TomoeTextureId = R::TextureId;
}

crate::tomoe_render_elements! {
    OutputRenderElements<R> => {
        Solid = SolidColorRenderElement,
        Memory = MemoryRenderBufferRenderElement<R>,
        Surface = WaylandSurfaceRenderElement<R>,
        // Window content clipped to rounded-corner geometry.
        ClippedSurface = ClippedSurfaceRenderElement<R>,
        // Damage injection for uniform-driven effects (corner radius).
        Damage = ExtraDamage,
        // Camera-zoomed variants (view_zoom != 1 only): window content and
        // border slabs scaled around the view origin.
        ZoomedSurface = RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
        ZoomedClippedSurface = RescaleRenderElement<ClippedSurfaceRenderElement<R>>,
        ZoomedSolid = RescaleRenderElement<SolidColorRenderElement>,
    }
}

/// Collect wp-presentation feedback for everything rendered on `output`.
/// Scanout attribution mirrors the frame-callback policy (`send_frames`):
/// every surface drawn is attributed to the output being presented.
pub fn take_presentation_feedback(
    space: &PhysicalSpace,
    output: &Output,
    lock_surface: Option<&LockSurface>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    let flags = |surface: &_, _: &_| {
        surface_presentation_feedback_flags_from_states(surface, None, render_element_states)
    };
    for window in space.elements() {
        window.take_presentation_feedback(&mut feedback, |_, _| Some(output.clone()), flags);
    }
    for layer in layer_map_for_output(output).layers() {
        layer.take_presentation_feedback(&mut feedback, |_, _| Some(output.clone()), flags);
    }
    if let Some(surface) = lock_surface {
        take_presentation_feedback_surface_tree(
            surface.wl_surface(),
            &mut feedback,
            |_, _| Some(output.clone()),
            flags,
        );
    }
    feedback
}

/// Border frame slabs drawn around windows (buffers are persisted in `Tomoe`
/// so the damage tracker sees stable element ids). Four slabs per window so
/// nothing is drawn behind the window itself — transparent surfaces would
/// otherwise show the border color across their whole area.
///
/// Widths and positions are physical pixels; the solid buffers are sized in
/// physical too, so they render with scale 1.0 (buffer units == pixels) and
/// can never land off-grid.
// Window keys hash by their stable id despite interior mutability.
#[allow(clippy::mutable_key_type)]
pub fn border_elements<R: TomoeRenderer>(
    space: &PhysicalSpace,
    border_buffers: &HashMap<Window, [SolidColorBuffer; 4]>,
    width: i32,
    output_loc: Point<i32, Physical>,
) -> Vec<OutputRenderElements<R>> {
    if width <= 0 {
        return Vec::new();
    }
    let zoom = space.view_zoom();
    // Borders live in world space with the windows they frame.
    let cam_loc = output_loc + space.view_offset();
    let mut elements = Vec::new();
    for window in space.elements() {
        let Some(buffers) = border_buffers.get(window) else {
            continue;
        };
        let Some(geo) = space.element_geometry(window) else {
            continue;
        };
        let loc = geo.loc - cam_loc;
        let offsets: [Point<i32, Physical>; 4] = [
            Point::from((-width, -width)),     // top
            Point::from((-width, geo.size.h)), // bottom
            Point::from((-width, 0)),          // left
            Point::from((geo.size.w, 0)),      // right
        ];
        for (buffer, offset) in buffers.iter().zip(offsets) {
            let solid = SolidColorRenderElement::from_buffer(
                buffer,
                loc + offset,
                1.0,
                1.0,
                Kind::Unspecified,
            );
            elements.push(if zoom == 1.0 {
                OutputRenderElements::Solid(solid)
            } else {
                OutputRenderElements::ZoomedSolid(RescaleRenderElement::from_element(
                    solid,
                    Point::from((-output_loc.x, -output_loc.y)),
                    zoom,
                ))
            });
        }
    }
    elements
}

/// Cursor elements at `pos` (output-local physical): the client-provided
/// surface, the xcursor theme frame, or the block fallback, in that order.
/// Shared by the TTY on-screen path and the capture paths; the buffers behind
/// every branch are persistent, so damage trackers see stable element ids
/// (never regenerate cursor buffers per frame — see the standing lessons).
pub fn cursor_elements<R: TomoeRenderer>(
    renderer: &mut R,
    cursor_status: &CursorImageStatus,
    cursor: &Cursor,
    fallback: &SolidColorBuffer,
    pos: Point<f64, Physical>,
    scale: f64,
) -> Vec<OutputRenderElements<R>> {
    let mut elements = Vec::new();
    match cursor_status {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Surface(cursor_surface) if cursor_surface.alive() => {
            let hotspot = with_states(cursor_surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|data| data.lock().unwrap().hotspot)
            })
            .unwrap_or_default();
            // The hotspot is in the cursor surface's coordinates (logical).
            let hotspot_phys = coords::logical_point_to_physical(hotspot.to_f64(), scale);
            let surface_pos = (pos - hotspot_phys.to_f64()).to_i32_round();
            elements.extend(
                render_elements_from_surface_tree(
                    renderer,
                    cursor_surface,
                    surface_pos,
                    scale,
                    1.0,
                    Kind::Cursor,
                )
                .into_iter()
                .map(OutputRenderElements::Surface),
            );
        }
        _ => {
            if let Some(element) = cursor.element(renderer, pos) {
                elements.push(OutputRenderElements::Memory(element));
            } else {
                elements.push(OutputRenderElements::Solid(
                    SolidColorRenderElement::from_buffer(
                        fallback,
                        pos.to_i32_round::<i32>(),
                        1.0,
                        1.0,
                        Kind::Cursor,
                    ),
                ));
            }
        }
    }
    elements
}

/// Committed fullscreen state (same check as the TTY tearing candidate).
fn is_fullscreen(window: &Window) -> bool {
    window.toplevel().is_some_and(|toplevel| {
        toplevel.with_committed_state(|state| {
            state
                .map(|s| s.states.contains(xdg_toplevel::State::Fullscreen))
                .unwrap_or(false)
        })
    })
}

/// Build the full scene for one output (everything except the cursor), in
/// render order: earlier elements draw on top. `ui` and `borders` are built
/// by the caller because they need parts of `Tomoe` this borrow can't reach.
///
/// `corner_radius` > 0 rounds window corners: the toplevel surface tree
/// draws through the clipped-surface shader (popups never clip). Fullscreen
/// windows are exempt — nothing to round, and clipping would block direct
/// scanout. `corner_damage` holds the per-window damage-injection elements
/// bumped when the radius setting changes (`Tomoe::refresh_borders`).
// Window keys hash by their stable id despite interior mutability.
#[allow(clippy::mutable_key_type)]
pub fn scene_elements<R: TomoeRenderer>(
    renderer: &mut R,
    space: &PhysicalSpace,
    output: &Output,
    ui: Vec<OutputRenderElements<R>>,
    borders: Vec<OutputRenderElements<R>>,
    corner_radius: i32,
    corner_damage: &HashMap<Window, ExtraDamage>,
) -> Vec<OutputRenderElements<R>> {
    let scale = space.scale();
    let render_scale = Scale::from(scale);
    let Some(output_geo) = space.output_geometry(output) else {
        return ui;
    };

    let layer_elements = |renderer: &mut R, kinds: [WlrLayer; 2]| {
        let layers = layer_map_for_output(output);
        let mut elements = Vec::new();
        for kind in kinds {
            // Top → bottom within each layer kind.
            for layer in layers.layers_on(kind).rev() {
                let Some(geo) = layers.layer_geometry(layer) else {
                    continue;
                };
                let loc = coords::logical_point_to_physical(geo.loc.to_f64(), scale);
                elements.extend(layer.render_elements(renderer, loc, render_scale, 1.0));
            }
        }
        elements
    };

    let mut elements = ui;
    elements.extend(layer_elements(renderer, [WlrLayer::Overlay, WlrLayer::Top]));

    // Rounded corners: the shader program lives on the Gles context; its
    // absence (compile failure, missing init) simply disables rounding.
    let radius = corner_radius.max(0) as f32;
    let clip_program = if radius > 0. {
        Shaders::get(renderer).and_then(|s| s.clipped_surface.clone())
    } else {
        None
    };

    // Windows top → bottom. The stored location is the geometry origin; the
    // buffer origin shifts by the client's (logical) geometry offset, rounded
    // once onto the grid. Windows live in world space: the camera pans them
    // by an integer offset (still pixel-exact) and, at zoom != 1 only, scales
    // them around the view origin via RescaleRenderElement.
    let zoom = space.view_zoom();
    let cam_loc = output_geo.loc + space.view_offset();
    let origin = Point::from((-output_geo.loc.x, -output_geo.loc.y));
    for window in space.elements().rev() {
        let Some(geo) = space.element_geometry(window) else {
            continue;
        };
        if space
            .world_rect_to_screen(geo)
            .intersection(output_geo.to_f64())
            .is_none()
        {
            continue;
        }
        let buffer_offset =
            coords::logical_point_to_physical(window.geometry().loc.to_f64(), scale);
        let loc = geo.loc - buffer_offset - cam_loc;

        let WindowSurface::Wayland(toplevel) = window.underlying_surface();
        let surface = toplevel.wl_surface();

        // Popups draw above the window and never clip (they overhang the
        // geometry by design). Same order and offset math as smithay's
        // `Window::render_elements`, which can't express per-tree clipping.
        for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
            let offset = coords::logical_point_to_physical(
                (window.geometry().loc + popup_offset - popup.geometry().loc).to_f64(),
                scale,
            );
            for element in render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                loc + offset,
                render_scale,
                1.0,
                Kind::Unspecified,
            ) {
                elements.push(if zoom == 1.0 {
                    OutputRenderElements::Surface(element)
                } else {
                    OutputRenderElements::ZoomedSurface(RescaleRenderElement::from_element(
                        element, origin, zoom,
                    ))
                });
            }
        }

        let program = if is_fullscreen(window) {
            None
        } else {
            clip_program.clone()
        };
        // The clip rect is the window geometry in output-local physical
        // pixels — the same space the elements are positioned in.
        let clip_geo = Rectangle::new(geo.loc - cam_loc, geo.size);

        for element in render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<R>>(
            renderer,
            surface,
            loc,
            render_scale,
            1.0,
            Kind::Unspecified,
        ) {
            match &program {
                Some(program)
                    if ClippedSurfaceRenderElement::will_clip(
                        &element,
                        render_scale,
                        clip_geo,
                        radius,
                    ) =>
                {
                    let clipped = ClippedSurfaceRenderElement::new(
                        element,
                        render_scale,
                        clip_geo,
                        program.clone(),
                        radius,
                    );
                    elements.push(if zoom == 1.0 {
                        OutputRenderElements::ClippedSurface(clipped)
                    } else {
                        OutputRenderElements::ZoomedClippedSurface(
                            RescaleRenderElement::from_element(clipped, origin, zoom),
                        )
                    });
                }
                _ => {
                    elements.push(if zoom == 1.0 {
                        OutputRenderElements::Surface(element)
                    } else {
                        OutputRenderElements::ZoomedSurface(RescaleRenderElement::from_element(
                            element, origin, zoom,
                        ))
                    });
                }
            }
        }

        // Radius changes bump this window's ExtraDamage (uniform changes
        // don't touch any commit counter the tracker could see).
        if program.is_some() {
            if let Some(damage) = corner_damage.get(window) {
                let rect = if zoom == 1.0 {
                    clip_geo
                } else {
                    let screen = space.world_rect_to_screen(geo);
                    Rectangle::new(screen.loc - output_geo.loc.to_f64(), screen.size).to_i32_up()
                };
                elements.push(OutputRenderElements::Damage(damage.render(rect)));
            }
        }
    }

    // Borders render below windows (visible as a ring around each).
    elements.extend(borders);
    elements.extend(layer_elements(
        renderer,
        [WlrLayer::Bottom, WlrLayer::Background],
    ));
    elements
}
