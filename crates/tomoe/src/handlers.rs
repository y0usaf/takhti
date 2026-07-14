use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, LayerSurface,
    PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window,
    WindowSurfaceType,
};
use smithay::output::Output;
use smithay::input::pointer::{CursorImageStatus, Focus, MotionEvent, PointerHandle};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, Serial, Size, SERIAL_COUNTER};
use smithay::wayland::buffer::BufferHandler;
use smithay::reexports::calloop::Interest;
use smithay::wayland::compositor::{
    add_blocker, add_pre_commit_hook, get_parent, is_sync_subsurface, with_states,
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState,
    SurfaceAttributes,
};
use smithay::wayland::dmabuf::{
    get_dmabuf, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler};
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::ext_data_control::{
    DataControlHandler as ExtDataControlHandler, DataControlState as ExtDataControlState,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::wlr_data_control::{
    DataControlHandler as WlrDataControlHandler, DataControlState as WlrDataControlState,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceCachedState,
    LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1;
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::{
    self, OrgKdeKwinServerDecoration,
};
use smithay::reexports::wayland_server::WEnum;
use smithay::wayland::shell::kde::decoration::{KdeDecorationHandler, KdeDecorationState};
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;
use smithay::utils::IsAlive;
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler,
    OutputCaptureSourceState, ToplevelCaptureSourceHandler, ToplevelCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, Frame as CaptureFrame, ImageCopyCaptureHandler, ImageCopyCaptureState,
    Session as CaptureSession, SessionRef,
};
use smithay::input::dnd::DndGrabHandler;

use crate::backend::Backend;
use crate::protocols::gamma_control::{GammaControlHandler, GammaControlManagerState};
use crate::protocols::screencopy::{Screencopy, ScreencopyHandler, ScreencopyManagerState};
use crate::protocols::wlr_foreign_toplevel::{
    ForeignRequest, WlrForeignToplevelHandler, WlrForeignToplevelState,
};
use smithay::wayland::fractional_scale::FractionalScaleHandler;
use smithay::wayland::idle_inhibit::IdleInhibitHandler;
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::{
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_dmabuf,
    delegate_drm_syncobj, delegate_ext_data_control, delegate_fractional_scale,
    delegate_idle_inhibit, delegate_idle_notify, delegate_kde_decoration, delegate_layer_shell,
    delegate_output, delegate_pointer_constraints, delegate_presentation,
    delegate_primary_selection, delegate_relative_pointer, delegate_seat, delegate_session_lock,
    delegate_shm, delegate_viewporter, delegate_xdg_activation, delegate_xdg_decoration,
    delegate_xdg_shell,
};
use tracing::{debug, trace, warn};

use crate::state::{ClientState, Tomoe};

// ─── wl_compositor ────────────────────────────────────────────────────────────

impl CompositorHandler for Tomoe {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // Gate dmabuf commits on buffer readiness so we never sample a buffer
        // whose GPU work hasn't finished: the client's explicit-sync acquire
        // point when it uses linux-drm-syncobj, else the dmabuf's implicit
        // fences. Without this, drivers that need explicit sync (NVIDIA) show
        // stalls/freezes in GPU-heavy clients.
        add_pre_commit_hook::<Self, _>(surface, move |tomoe, _dh, surface| {
            let mut acquire_point = None;
            let maybe_dmabuf = with_states(surface, |states| {
                acquire_point.clone_from(
                    &states
                        .cached_state
                        .get::<DrmSyncobjCachedState>()
                        .pending()
                        .acquire_point,
                );
                states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .and_then(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).cloned().ok(),
                        _ => None,
                    })
            });
            let Some(dmabuf) = maybe_dmabuf else { return };
            let Some(client) = surface.client() else {
                return;
            };
            let sid = surface.id().protocol_id();
            let unblock = move |tomoe: &mut Tomoe| {
                let dh = tomoe.display_handle.clone();
                tomoe
                    .client_compositor_state(&client)
                    .blocker_cleared(tomoe, &dh);
            };
            if let Some(acquire_point) = acquire_point {
                if let Ok((blocker, source)) = acquire_point.generate_blocker() {
                    let res = tomoe.loop_handle.insert_source(source, move |_, _, tomoe| {
                        debug!("surface {sid}: acquire fence signaled, unblocking");
                        unblock(tomoe);
                        Ok(())
                    });
                    if res.is_ok() {
                        debug!("surface {sid}: commit blocked on explicit-sync acquire point");
                        add_blocker(surface, blocker);
                    } else {
                        warn!("surface {sid}: failed to insert acquire fence source");
                    }
                    return;
                }
                warn!("surface {sid}: failed to create acquire point blocker");
            }
            if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                let res = tomoe.loop_handle.insert_source(source, move |_, _, tomoe| {
                    debug!("surface {sid}: implicit fences signaled, unblocking");
                    unblock(tomoe);
                    Ok(())
                });
                if res.is_ok() {
                    debug!("surface {sid}: commit blocked on implicit dmabuf fences");
                    add_blocker(surface, blocker);
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        trace!("commit applied for surface {}", surface.id().protocol_id());
        on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for_surface(&root) {
                window.on_commit();
                // Title/app_id changes flow to foreign-toplevel listeners; a
                // resize renegotiates any capture session on this window.
                self.refresh_foreign_toplevel(&window);
                if !self.capture_sessions.is_empty() {
                    crate::capture::refresh_capture_sessions(self);
                }
            }
            // A lock surface committing its first buffer can progress a
            // pending lock; later commits just redraw (below).
            if self.is_lock_surface(&root) {
                self.maybe_continue_to_locking();
                self.update_lock_focus();
            }
        }

        self.popups.commit(surface);
        self.xdg_shell_handle_commit(surface);
        self.layer_shell_handle_commit(surface);

        // Client content changed; the damage tracker decides what repaints.
        self.queue_redraw_all();
    }
}
delegate_compositor!(Tomoe);

impl BufferHandler for Tomoe {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for Tomoe {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(Tomoe);

// ─── xdg-shell ────────────────────────────────────────────────────────────────

impl XdgShellHandler for Tomoe {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);
        self.unmapped_windows.push(window);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            warn!("error tracking popup: {err:?}");
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let seat: Seat<Self> = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        // KeyboardFocus is a plain WlSurface, so the root works directly as
        // the grab's focus whether it belongs to a window or a layer surface.
        let mut grab = match self.popups.grab_popup(root, kind, &seat, serial) {
            Ok(grab) => grab,
            Err(err) => {
                debug!("denying popup grab: {err:?}");
                return;
            }
        };
        // If either device is grabbed by someone else (a Lua drag, another
        // popup chain), refuse and dismiss rather than stacking grabs.
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    // ── Interactive move/resize: forwarded to the Lua grab machinery ──
    //
    // A client-initiated drag (CSD titlebar, resize edge) arrives while the
    // triggering button is held, i.e. while smithay's implicit click grab
    // pins pointer focus to the client. The request becomes an
    // `on_window_request` event; a hook that consumes it typically calls
    // `tomoe.grab_pointer`, and the core then releases the click grab so
    // motion routes to Lua instead of the client. Unconsumed requests are
    // dropped — a tiled layout has no native drag, and ignoring is the
    // protocol-sanctioned response.

    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        let seat: Seat<Self> = Seat::from_resource(&seat).unwrap();
        self.interactive_request(&surface, &seat, serial, "move", None);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat: Seat<Self> = Seat::from_resource(&seat).unwrap();
        self.interactive_request(&surface, &seat, serial, "resize", Some(edges_name(edges)));
    }

    // ── Window-state requests: Lua policy first, native default second ──
    //
    // Each request becomes an `on_window_request` event; a hook returning
    // truthy consumes it and takes over responding (its queued ops carry the
    // configures). Unconsumed requests fall back to the native default.
    // Unmapped toplevels have no Lua id yet, so they always take the default;
    // policy can still react on map via `win:is_fullscreen()`.

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let id = self.window_id_for_surface(surface.wl_surface());
        let consumed = id.is_some_and(|id| self.emit_window_request(id, "maximize", None, None));
        if !consumed {
            // Tiled layout: just ack so the client doesn't hang waiting.
            surface.send_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let id = self.window_id_for_surface(surface.wl_surface());
        let consumed = id.is_some_and(|id| self.emit_window_request(id, "unmaximize", None, None));
        if !consumed {
            surface.send_configure();
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, wl_output: Option<WlOutput>) {
        let id = self.window_id_for_surface(surface.wl_surface());
        let output_name = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .map(|o| o.name());
        let consumed =
            id.is_some_and(|id| self.emit_window_request(id, "fullscreen", output_name, None));
        if !consumed {
            self.fullscreen_default(&surface, wl_output.as_ref(), id);
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let id = self.window_id_for_surface(surface.wl_surface());
        let consumed =
            id.is_some_and(|id| self.emit_window_request(id, "unfullscreen", None, None));
        if !consumed {
            self.unfullscreen_default(&surface, id);
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        // xdg-shell has no minimized state to ack; ignoring unconsumed
        // requests is the protocol-sanctioned response.
        if let Some(id) = self.window_id_for_surface(surface.wl_surface()) {
            self.emit_window_request(id, "minimize", None, None);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.pending_activations.remove(surface.wl_surface());
        self.unmapped_windows
            .retain(|w| w.toplevel().map(|t| t.wl_surface()) != Some(surface.wl_surface()));
        let window = self
            .windows
            .values()
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(surface.wl_surface()))
            .cloned();
        if let Some(window) = window {
            self.window_closed(&window);
        }
    }
}
delegate_xdg_shell!(Tomoe);

// ─── xdg-activation ───────────────────────────────────────────────────────────
//
// Focus stealing with consent (niri-shape): a focused client hands a token to
// another process (via env or D-Bus), and presenting that token with a valid,
// recent input serial lets the target window take focus — the mechanism
// behind "clicking a notification focuses the app". Each request becomes an
// `on_window_request` event ("activate", or "urgent" for serial-less tokens)
// so Lua policy can switch workspaces first; the native default focuses the
// window (activate) or does nothing (urgent — tomoe has no urgency state).

/// Tokens older than this are ignored, and the prune timer in `Tomoe::new`
/// drops them from the registry (niri's value).
pub const XDG_ACTIVATION_TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Marker for tokens created without an input serial (notification daemons,
/// tray clicks). Not specified, but common client behavior treats these as
/// urgency pings rather than focus steals — so do we (and niri).
struct UrgentOnlyMarker;

/// Decide whether the input serial on an activation token is acceptable.
/// Serial-less tokens are accepted so the caller can mark them urgency-only;
/// they never become focus activations.
fn accepts_activation_serial(
    serial: Option<Serial>,
    keyboard_last_enter: Option<Serial>,
    pointer_last_enter: Option<Serial>,
    honor_invalid_serial: bool,
) -> bool {
    let Some(serial) = serial else {
        return true;
    };
    honor_invalid_serial
        || keyboard_last_enter.is_some_and(|last_enter| serial.is_no_older_than(&last_enter))
        || pointer_last_enter.is_some_and(|last_enter| serial.is_no_older_than(&last_enter))
}

#[cfg(test)]
mod activation_tests {
    use super::*;

    #[test]
    fn invalid_serial_requires_explicit_opt_in() {
        let last_enter = Some(Serial::from(100));
        assert!(!accepts_activation_serial(
            Some(Serial::from(99)),
            last_enter,
            None,
            false,
        ));
        assert!(accepts_activation_serial(
            Some(Serial::from(99)),
            last_enter,
            None,
            true,
        ));
    }

    #[test]
    fn valid_and_serial_less_tokens_keep_existing_behavior() {
        let last_enter = Some(Serial::from(100));
        assert!(accepts_activation_serial(
            Some(Serial::from(100)),
            last_enter,
            None,
            false,
        ));
        assert!(accepts_activation_serial(
            Some(Serial::from(101)),
            None,
            last_enter,
            false,
        ));
        assert!(accepts_activation_serial(None, last_enter, None, false));
    }
}

impl XdgActivationHandler for Tomoe {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        // Tokens without a serial are urgency-only (see UrgentOnlyMarker).
        let Some((serial, seat)) = data.serial else {
            data.user_data.insert_if_missing(|| UrgentOnlyMarker);
            return true;
        };
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return false;
        };

        // Discord and Telegram replace a valid supplied token with one made
        // from an older serial when launched from a tray/notification. Keep
        // the strict default, but provide an explicit compatibility escape
        // hatch matching niri's opt-in behavior. The seat resource is still
        // validated above, so this does not accept a token from an unknown
        // seat.
        let honor_invalid_serial = self.lua.settings().honor_xdg_activation_with_invalid_serial;
        accepts_activation_serial(
            Some(serial),
            seat.get_keyboard()
                .and_then(|keyboard| keyboard.last_enter()),
            seat.get_pointer().and_then(|pointer| pointer.last_enter()),
            honor_invalid_serial,
        )
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed() < XDG_ACTIVATION_TOKEN_TIMEOUT {
            if let Some(id) = self.window_id_for_surface(&surface) {
                let urgent = token_data.user_data.get::<UrgentOnlyMarker>().is_some();
                let kind = if urgent { "urgent" } else { "activate" };
                let consumed = self.emit_window_request(id, kind, None, None);
                if !consumed && !urgent {
                    let window = self.windows.get(&id).cloned();
                    if let Some(window) = window {
                        self.focus_window(Some(&window));
                        self.queue_redraw_all();
                    }
                }
            } else if self.window_for_surface(&surface).is_some() {
                // Still unmapped: honor the token when the window maps
                // (`add_window`), the common startup-notification path.
                self.pending_activations.insert(surface, token_data);
            }
        }

        self.activation_state.remove_token(&token);
    }
}
delegate_xdg_activation!(Tomoe);

// ─── xdg-decoration / kde-server-decoration ───────────────────────────────────
//
// Windows are tiled and the compositor draws the borders, so we prefer
// server-side decorations: clients that honor either protocol (Firefox,
// GTK/Qt apps, ...) skip their own titlebar instead of drawing one that
// ignores the layout's uniform look.

impl XdgDecorationHandler for Tomoe {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        // Usually this rides along with the initial configure; only flush it
        // ourselves when the client decorated an already-configured toplevel.
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: zxdg_toplevel_decoration_v1::Mode) {
        // Grant whatever the client asks for rather than forcing our
        // preference — forcing a mode mid-handshake breaks older SDL2 apps
        // (https://github.com/libsdl-org/SDL/issues/8173), and a client that
        // insists on CSD will draw it regardless.
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(Tomoe);

impl KdeDecorationHandler for Tomoe {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.kde_decoration_state
    }

    fn request_mode(
        &mut self,
        _surface: &WlSurface,
        decoration: &OrgKdeKwinServerDecoration,
        mode: WEnum<org_kde_kwin_server_decoration::Mode>,
    ) {
        // Same policy as xdg-decoration: acknowledge the client's choice.
        if let WEnum::Value(mode) = mode {
            decoration.mode(mode);
        }
    }
}
delegate_kde_decoration!(Tomoe);

/// Lua-friendly name for the edge/corner an xdg resize drags.
fn edges_name(edges: xdg_toplevel::ResizeEdge) -> &'static str {
    use xdg_toplevel::ResizeEdge;
    match edges {
        ResizeEdge::Top => "top",
        ResizeEdge::Bottom => "bottom",
        ResizeEdge::Left => "left",
        ResizeEdge::Right => "right",
        ResizeEdge::TopLeft => "top_left",
        ResizeEdge::TopRight => "top_right",
        ResizeEdge::BottomLeft => "bottom_left",
        ResizeEdge::BottomRight => "bottom_right",
        _ => "none",
    }
}

impl Tomoe {
    /// Shared tail of xdg move/resize requests. The serial must match the
    /// live click grab (a stale serial means the button is already up) and
    /// the grab must have started on the requesting client, else the request
    /// is dropped. It is then handed to Lua; if a hook consumed it by
    /// starting a pointer grab, smithay's click grab is released and client
    /// focus cleared, so the client sees a leave (ending its local drag
    /// state) and subsequent motion routes to the Lua grab (input.rs).
    fn interactive_request(
        &mut self,
        surface: &ToplevelSurface,
        seat: &Seat<Self>,
        serial: Serial,
        kind: &str,
        edges: Option<&str>,
    ) {
        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        if !pointer.has_grab(serial) {
            return;
        }
        let same_client = pointer
            .grab_start_data()
            .and_then(|data| data.focus)
            .is_some_and(|(focus, _)| focus.id().same_client_as(&surface.wl_surface().id()));
        if !same_client {
            return;
        }
        let Some(id) = self.window_id_for_surface(surface.wl_surface()) else {
            return;
        };
        let consumed = self.emit_window_request(id, kind, None, edges);
        if consumed && self.lua.pointer_grab_active() {
            let serial = SERIAL_COUNTER.next_serial();
            let time = self.start_time.elapsed().as_millis() as u32;
            pointer.unset_grab(self, serial, time);
            let location = pointer.current_location();
            pointer.motion(
                self,
                None,
                &MotionEvent {
                    location,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
        }
    }

    /// Native fullscreen default: honor the request on the client-requested
    /// output, else the window's own, else the first. Remembers the previous
    /// geometry so `unfullscreen_default` can restore it.
    fn fullscreen_default(
        &mut self,
        toplevel: &ToplevelSurface,
        wl_output: Option<&WlOutput>,
        id: Option<u64>,
    ) {
        let window = self.window_for_surface(toplevel.wl_surface());
        let output = wl_output
            .and_then(Output::from_resource)
            .or_else(|| {
                let geo = window
                    .as_ref()
                    .and_then(|w| self.space.element_geometry(w))?;
                // Windows live in world space, outputs in screen space.
                let center = self.space.world_to_screen(Point::from((
                    geo.loc.x as f64 + geo.size.w as f64 / 2.0,
                    geo.loc.y as f64 + geo.size.h as f64 / 2.0,
                )));
                self.space.output_under(center).cloned()
            })
            .or_else(|| self.space.outputs().next().cloned());
        let output_geo = output.as_ref().and_then(|o| self.space.output_geometry(o));
        let Some(output_geo) = output_geo else {
            // Nowhere to honor it; still ack so the client doesn't hang.
            toplevel.send_configure();
            return;
        };
        if let (Some(id), Some(window)) = (id, &window) {
            if let Some(prev) = self.space.element_geometry(window) {
                self.fullscreen_prev.entry(id).or_insert(prev);
            }
        }
        let (logical, _achievable) =
            crate::coords::configure_size(output_geo.size, self.space.scale());
        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.size = Some(logical);
            state.fullscreen_output = wl_output.cloned();
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        // Only place mapped windows; unmapped ones get their spot on map.
        if let (Some(_), Some(window)) = (id, window) {
            let world = self.space.screen_to_world(output_geo.loc.to_f64());
            self.space.map_element(
                window.clone(),
                Point::from((world.x.round() as i32, world.y.round() as i32)),
            );
            self.space.raise_element(&window);
        }
        self.queue_redraw_all();
    }

    /// Undo `fullscreen_default`: drop the state and restore the remembered
    /// geometry (no remembered geometry → the client picks its own size).
    fn unfullscreen_default(&mut self, toplevel: &ToplevelSurface, id: Option<u64>) {
        let prev = id.and_then(|id| self.fullscreen_prev.remove(&id));
        let scale = self.space.scale();
        toplevel.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
            state.fullscreen_output = None;
            state.size = prev.map(|geo| crate::coords::configure_size(geo.size, scale).0);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        if let Some(prev) = prev {
            if let Some(window) = self.window_for_surface(toplevel.wl_surface()) {
                self.space.map_element(window, prev.loc);
            }
        }
        self.queue_redraw_all();
    }

    /// Handle the xdg-shell part of a surface commit: initial configure and
    /// mapping toplevels once their first buffer arrives.
    pub fn xdg_shell_handle_commit(&mut self, surface: &WlSurface) {
        if let Some(idx) = self
            .unmapped_windows
            .iter()
            .position(|w| w.toplevel().map(|t| t.wl_surface()) == Some(surface))
        {
            let toplevel = self.unmapped_windows[idx].toplevel().unwrap().clone();
            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .initial_configure_sent
            });
            if !initial_configure_sent {
                toplevel.send_configure();
                return;
            }
            let has_buffer = with_renderer_surface_state(surface, |state| state.buffer().is_some())
                .unwrap_or(false);
            if has_buffer {
                let window = self.unmapped_windows.remove(idx);
                self.add_window(window);
            }
            return;
        }

        // Initial configure for popups.
        if let Some(popup) = self.popups.find_popup(surface) {
            if let PopupKind::Xdg(popup) = &popup {
                if !popup.is_initial_configure_sent() {
                    let _ = popup.send_configure();
                }
            }
        }
    }

    /// Handle the layer-shell part of a surface commit.
    pub fn layer_shell_handle_commit(&mut self, surface: &WlSurface) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            let layer = layer_map_for_output(&output)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .cloned();
            let Some(layer) = layer else { continue };

            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<LayerSurfaceData>()
                    .map(|data| data.lock().unwrap().initial_configure_sent)
            })
            .unwrap_or(true);

            layer_map_for_output(&output).arrange();
            if !initial_configure_sent {
                layer.layer_surface().send_configure();
            } else {
                // Give keyboard focus to layers that ask for it (launchers).
                let interactivity = with_states(surface, |states| {
                    let mut guard = states.cached_state.get::<LayerSurfaceCachedState>();
                    guard.current().keyboard_interactivity
                });
                if matches!(
                    interactivity,
                    KeyboardInteractivity::Exclusive | KeyboardInteractivity::OnDemand
                ) && !self.is_locked()
                {
                    let serial = SERIAL_COUNTER.next_serial();
                    if let Some(keyboard) = self.seat.get_keyboard() {
                        keyboard.set_focus(self, Some(surface.clone()), serial);
                    }
                }
            }
            // Exclusive zones may have changed; Lua is notified only if so.
            self.outputs_changed(false);
            return;
        }
    }

    /// Rewrite a popup's pending geometry so it stays on screen, applying the
    /// positioner's constraint adjustments (flip/slide/resize). Clients only
    /// *request* adjustments; solving them against the available area is the
    /// compositor's job — without this, a menu near the bottom edge opens
    /// downward off-screen instead of flipping up.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let scale = self.space.scale();

        // Build the usable area in the space the positioner works in:
        // logical, relative to the parent's window-geometry origin.
        let mut target = if let Some(window) = self.window_for_surface(&root) {
            let Some(window_geo) = self.space.element_geometry(&window) else {
                return;
            };
            // Windows live in world space, outputs are screen-fixed; compare
            // in world space (the same transform refresh() applies), against
            // the output showing the largest part of the window.
            let zoom = self.space.view_zoom();
            let Some(output_world) = self
                .space
                .outputs()
                .filter_map(|output| self.space.output_geometry(output))
                .map(|geo| {
                    Rectangle::new(
                        self.space.screen_to_world(geo.loc.to_f64()).to_i32_round(),
                        Size::from((
                            (geo.size.w as f64 / zoom).round() as i32,
                            (geo.size.h as f64 / zoom).round() as i32,
                        )),
                    )
                })
                .max_by_key(|geo| {
                    geo.intersection(window_geo)
                        .map_or(0, |o| o.size.w as i64 * o.size.h as i64)
                })
            else {
                return;
            };
            let mut rect = output_world;
            rect.loc -= window_geo.loc;
            crate::coords::rect_to_logical(rect, scale)
        } else {
            // Layer-shell parent: layer maps arrange in output-local logical
            // coordinates, screen-fixed.
            let mut found = None;
            for output in self.space.outputs() {
                let map = layer_map_for_output(output);
                let Some(layer) = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL) else {
                    continue;
                };
                let (Some(layer_geo), Some(output_geo)) = (
                    map.layer_geometry(layer),
                    self.space.output_geometry(output),
                ) else {
                    continue;
                };
                let mut rect =
                    Rectangle::from_size(crate::coords::rect_to_logical(output_geo, scale).size);
                rect.loc -= layer_geo.loc;
                found = Some(rect);
                break;
            }
            let Some(rect) = found else { return };
            rect
        };

        // Nested popups (submenus) position relative to their parent popup,
        // not the root toplevel.
        target.loc -= get_popup_toplevel_coords(&kind);

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

// ─── wlr-layer-shell ──────────────────────────────────────────────────────────

impl WlrLayerShellHandler for Tomoe {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        let Some(output) = output else {
            warn!("no output for new layer surface");
            return;
        };
        let layer = LayerSurface::new(surface, namespace);
        crate::state::send_scale(layer.wl_surface(), self.space.scale());
        if let Err(err) = layer_map_for_output(&output).map_layer(&layer) {
            warn!("error mapping layer surface: {err}");
        }
        self.outputs_changed(false);
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        // A layer-parented popup has no xdg parent at XdgShellHandler::
        // new_popup time (layer-shell get_popup sets it), so unconstraining
        // there found no root; redo it now that the parent is known.
        self.unconstrain_popup(&popup);
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            let mut map = layer_map_for_output(&output);
            let layer = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        // Return keyboard focus to the topmost window, then notify Lua.
        let next = self.space.elements().next_back().cloned();
        self.focus_window(next.as_ref());
        self.outputs_changed(false);
    }
}
delegate_layer_shell!(Tomoe);

// ─── seat / input focus ───────────────────────────────────────────────────────

impl SeatHandler for Tomoe {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Tomoe> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        self.queue_redraw_all();
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }
}
delegate_seat!(Tomoe);
delegate_relative_pointer!(Tomoe);
delegate_presentation!(Tomoe);
crate::delegate_tearing_control!(Tomoe);

// ─── pointer-constraints ──────────────────────────────────────────────────────
//
// The lock/confine enforcement lives in the relative-motion path (input.rs);
// smithay deactivates a constraint itself when pointer focus leaves its
// surface, so only activation and the cursor position hint are handled here.

impl PointerConstraintsHandler for Tomoe {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        self.maybe_activate_pointer_constraint();
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        let is_active = with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active())
        });
        if !is_active {
            return;
        }
        // The hint is surface-local; recover the surface origin from the
        // current hit-test and only honor hints from the constrained surface.
        let scale = self.space.scale();
        let pos = crate::coords::point_to_physical(pointer.current_location(), scale);
        let Some((under, origin)) = self.surface_under(pos) else {
            return;
        };
        if &under != surface {
            return;
        }
        let target = crate::coords::point_to_physical(origin + location, scale);
        let target = self.clamp_to_outputs(target);
        pointer.set_location(crate::coords::point_to_protocol(target, scale));
        // The cursor is composited, so moving it damages the output.
        self.queue_redraw_all();
    }
}
delegate_pointer_constraints!(Tomoe);

// ─── selection / data device ──────────────────────────────────────────────────

impl SelectionHandler for Tomoe {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Tomoe {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl WaylandDndGrabHandler for Tomoe {}
impl DndGrabHandler for Tomoe {}
delegate_data_device!(Tomoe);

impl PrimarySelectionHandler for Tomoe {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}
delegate_primary_selection!(Tomoe);

impl WlrDataControlHandler for Tomoe {
    fn data_control_state(&mut self) -> &mut WlrDataControlState {
        &mut self.wlr_data_control_state
    }
}
delegate_data_control!(Tomoe);

impl ExtDataControlHandler for Tomoe {
    fn data_control_state(&mut self) -> &mut ExtDataControlState {
        &mut self.ext_data_control_state
    }
}
delegate_ext_data_control!(Tomoe);

// ─── outputs ──────────────────────────────────────────────────────────────────

impl OutputHandler for Tomoe {
    fn output_bound(&mut self, output: Output, wl_output: WlOutput) {
        // A client bound a fresh wl_output: foreign-toplevel handles that
        // already entered this output owe it an output_enter.
        self.wlr_foreign_toplevel_state
            .on_output_bound(&output, &wl_output);
    }
}
delegate_output!(Tomoe);

// ─── wlr-foreign-toplevel-management ──────────────────────────────────────
//
// The taskbar control surface. Every request rides the same
// `on_window_request` policy path the window's own client gets — a hook
// that consumes "activate"/"fullscreen"/... governs taskbars too — and the
// unconsumed defaults match the corresponding xdg/activation paths.

impl WlrForeignToplevelHandler for Tomoe {
    fn wlr_foreign_toplevel_state(&mut self) -> &mut WlrForeignToplevelState {
        &mut self.wlr_foreign_toplevel_state
    }

    fn foreign_toplevel_request(&mut self, id: u64, request: ForeignRequest) {
        // Retired handles resolve to ids no window carries: no-op.
        let Some(window) = self.windows.get(&id).cloned() else {
            return;
        };
        let toplevel = window.toplevel().cloned();
        match request {
            ForeignRequest::Activate => {
                if !self.emit_window_request(id, "activate", None, None) {
                    self.focus_window(Some(&window));
                    self.queue_redraw_all();
                }
            }
            ForeignRequest::Close => {
                if !self.emit_window_request(id, "close", None, None) {
                    if let Some(toplevel) = &toplevel {
                        toplevel.send_close();
                    }
                }
            }
            ForeignRequest::SetFullscreen(wl_output) => {
                let output_name = wl_output
                    .as_ref()
                    .and_then(Output::from_resource)
                    .map(|o| o.name());
                if !self.emit_window_request(id, "fullscreen", output_name, None) {
                    if let Some(toplevel) = &toplevel {
                        self.fullscreen_default(toplevel, wl_output.as_ref(), Some(id));
                    }
                }
            }
            ForeignRequest::UnsetFullscreen => {
                if !self.emit_window_request(id, "unfullscreen", None, None) {
                    if let Some(toplevel) = &toplevel {
                        self.unfullscreen_default(toplevel, Some(id));
                    }
                }
            }
            ForeignRequest::SetMaximized => {
                if !self.emit_window_request(id, "maximize", None, None) {
                    if let Some(toplevel) = &toplevel {
                        toplevel.send_configure();
                    }
                }
            }
            ForeignRequest::UnsetMaximized => {
                if !self.emit_window_request(id, "unmaximize", None, None) {
                    if let Some(toplevel) = &toplevel {
                        toplevel.send_configure();
                    }
                }
            }
            // No native minimized state; policy may hide the window, the
            // unconsumed default is to ignore (same as xdg minimize).
            ForeignRequest::SetMinimized => {
                self.emit_window_request(id, "minimize", None, None);
            }
            ForeignRequest::UnsetMinimized => {
                self.emit_window_request(id, "unminimize", None, None);
            }
        }
    }
}
crate::delegate_wlr_foreign_toplevel!(Tomoe);

// ─── wp-viewporter / wp-fractional-scale ──────────────────────────────────────

impl FractionalScaleHandler for Tomoe {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Tell the client the exact scale up front so its very first buffer
        // is already at native pixel density.
        crate::state::send_scale(&surface, self.space.scale());
    }
}
delegate_fractional_scale!(Tomoe);
delegate_viewporter!(Tomoe);

// ─── dmabuf ───────────────────────────────────────────────────────────────────

impl DmabufHandler for Tomoe {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self.backend.import_dmabuf(&dmabuf) {
            let _ = notifier.successful::<Tomoe>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(Tomoe);

// ─── linux-drm-syncobj (explicit sync) ────────────────────────────────────────

impl DrmSyncobjHandler for Tomoe {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj_state.as_mut()
    }
}
delegate_drm_syncobj!(Tomoe);

// ─── wlr-screencopy ───────────────────────────────────────────────────────────

impl ScreencopyHandler for Tomoe {
    fn frame(&mut self, manager: &ZwlrScreencopyManagerV1, screencopy: Screencopy) {
        let output_exists = self.space.outputs().any(|o| o == screencopy.output());
        if !output_exists {
            // Dropping the screencopy sends `failed`.
            trace!("screencopy output no longer exists");
            return;
        }

        if screencopy.with_damage() {
            // Completed from the redraw loop once the output has damage.
            self.screencopy_state.push(manager, screencopy);
        } else {
            crate::capture::render_screencopy(self, manager, screencopy);
        }
    }

    fn screencopy_state(&mut self) -> &mut ScreencopyManagerState {
        &mut self.screencopy_state
    }
}
crate::delegate_screencopy!(Tomoe);

// ─── wlr-gamma-control ──────────────────────────────────────────────────────
//
// LUT programming lives in the tty backend (GAMMA_LUT property with a
// legacy-ioctl fallback); winit has no gamma, so clients get `failed()`.

impl GammaControlHandler for Tomoe {
    fn gamma_control_manager_state(&mut self) -> &mut GammaControlManagerState {
        &mut self.gamma_control_state
    }

    fn get_gamma_size(&mut self, output: &Output) -> Option<u32> {
        let Backend::Tty(data) = &self.backend else {
            return None;
        };
        match data.get_gamma_size(output) {
            Ok(0) => None, // Setting gamma is not supported.
            Ok(size) => Some(size),
            Err(err) => {
                warn!(
                    "error getting gamma size for output {}: {err:#}",
                    output.name()
                );
                None
            }
        }
    }

    fn set_gamma(&mut self, output: &Output, ramp: Option<Vec<u16>>) -> Option<()> {
        let Backend::Tty(data) = &mut self.backend else {
            return None;
        };
        match data.set_gamma(output, ramp) {
            Ok(()) => Some(()),
            Err(err) => {
                warn!("error setting gamma for output {}: {err:#}", output.name());
                None
            }
        }
    }
}
crate::delegate_gamma_control!(Tomoe);

// ─── ext-session-lock ─────────────────────────────────────────────────────────
//
// The state machine (and why the confirmation is deferred) lives in lock.rs.

impl SessionLockHandler for Tomoe {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        self.lock_session(confirmation);
    }

    fn unlock(&mut self) {
        self.unlock_session();
        // Unlocking is user activity: wake idle listeners.
        self.notify_activity();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        self.new_lock_surface(surface, &output);
    }
}
delegate_session_lock!(Tomoe);

// ─── ext-idle-notify / zwp-idle-inhibit ───────────────────────────────────────
//
// Timers are smithay's; tomoe feeds them activity (input.rs) and the
// inhibited flag (state.rs::refresh_idle_inhibit, every loop iteration).

impl IdleNotifierHandler for Tomoe {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}
delegate_idle_notify!(Tomoe);

impl IdleInhibitHandler for Tomoe {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibiting_surfaces.insert(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibiting_surfaces.remove(&surface);
    }
}
delegate_idle_inhibit!(Tomoe);

// ─── ext-foreign-toplevel-list ────────────────────────────────────────────────
//
// Handle lifecycle (publish/refresh/retire) lives in foreign_toplevel.rs.

impl ForeignToplevelListHandler for Tomoe {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_state
    }
}
smithay::delegate_foreign_toplevel_list!(Tomoe);

// ─── ext-image-capture-source / ext-image-copy-capture ───────────────────────

impl ImageCaptureSourceHandler for Tomoe {}
smithay::delegate_image_capture_source!(Tomoe);

impl OutputCaptureSourceHandler for Tomoe {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_source_state
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        // The source resolves back to its output through this weak handle
        // (capture.rs::source_output).
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}
smithay::delegate_output_capture_source!(Tomoe);

impl ToplevelCaptureSourceHandler for Tomoe {
    fn toplevel_capture_source_state(&mut self) -> &mut ToplevelCaptureSourceState {
        &mut self.toplevel_capture_source_state
    }

    fn toplevel_source_created(
        &mut self,
        source: ImageCaptureSource,
        toplevel: ForeignToplevelHandle,
    ) {
        // The source resolves back to its window through the handle and the
        // window id riding on it (capture.rs::source_target).
        source
            .user_data()
            .insert_if_missing(|| toplevel.downgrade());
    }
}
smithay::delegate_toplevel_capture_source!(Tomoe);

impl ImageCopyCaptureHandler for Tomoe {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        crate::capture::constraints_for_source(self, source)
    }

    fn new_session(&mut self, session: CaptureSession) {
        self.capture_sessions.push(session);
    }

    fn frame(&mut self, session: &SessionRef, frame: CaptureFrame) {
        // Defer to the redraw path (capture.rs::complete_capture_frames):
        // answering immediately would let a capture→ready→capture client
        // spin unthrottled; completing after on-screen renders paces casts
        // to vblank.
        self.pending_capture_frames.push((session.clone(), frame));
        self.queue_redraw_all();
    }

    fn session_destroyed(&mut self, _session: SessionRef) {
        self.capture_sessions.retain(|s| s.as_ref().alive());
    }
}
smithay::delegate_image_copy_capture!(Tomoe);
