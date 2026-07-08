use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};
use smithay::backend::renderer::element::solid::SolidColorBuffer;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::desktop::{layer_map_for_output, PopupManager, Window, WindowSurfaceType};
use smithay::input::keyboard::XkbConfig;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatState};
use smithay::output::{Output, Scale as OutputScale};
use smithay::reexports::calloop::{LoopHandle, LoopSignal};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{
    Clock, ClockSource, IsAlive, Logical, Monotonic, Physical, Point, Rectangle, Size, Transform,
    SERIAL_COUNTER,
};
use smithay::wayland::compositor::{
    get_parent, send_surface_state, with_states, CompositorClientState, CompositorState,
};
use smithay::wayland::fractional_scale::{with_fractional_scale, FractionalScaleManagerState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::drm_syncobj::DrmSyncobjState;
use smithay::wayland::output::OutputManagerState;
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeDecorationsMode;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraintsState};
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::ext_data_control::DataControlState as ExtDataControlState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::selection::wlr_data_control::DataControlState as WlrDataControlState;
use smithay::wayland::shell::kde::decoration::KdeDecorationState;
use smithay::wayland::shell::wlr_layer::{Layer as WlrLayer, WlrLayerShellState};
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::idle_inhibit::IdleInhibitManagerState;
use smithay::wayland::idle_notify::IdleNotifierState;
use smithay::wayland::foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListState};
use smithay::wayland::image_capture_source::{
    ImageCaptureSourceState, OutputCaptureSourceState, ToplevelCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    Frame as CaptureFrame, ImageCopyCaptureState, Session, SessionRef,
};
use smithay::wayland::session_lock::{LockSurface, SessionLockManagerState};
use smithay::wayland::shell::xdg::{XdgShellState, XdgToplevelSurfaceData};
use smithay::wayland::shm::ShmState;
use tracing::{info, warn};

use crate::backend::Backend;
use crate::coords;
use crate::cursor::Cursor;
use crate::input::{Action, Bind};
use crate::lock::LockState;
use crate::lua::{KeyboardSettings, LuaRuntime, OutputProps, WinProps, WindowOp};
use crate::process::{Launch, ProcessDecl, ProcessManager, ProcessSpec};
use crate::space::PhysicalSpace;
use crate::ui::Ui;

/// Identity of the loaded config file for change detection. Comparing the
/// canonical path catches symlink-target swaps whose mtime doesn't change
/// (Nix store generations); mtime catches in-place edits and atomic renames.
#[derive(Debug, PartialEq, Eq)]
struct ConfigFingerprint {
    canonical: PathBuf,
    mtime: SystemTime,
}

fn config_fingerprint(path: Option<&Path>) -> Option<ConfigFingerprint> {
    let canonical = path?.canonicalize().ok()?;
    let mtime = std::fs::metadata(&canonical).ok()?.modified().ok()?;
    Some(ConfigFingerprint { canonical, mtime })
}

pub struct Tomoe {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Tomoe>,
    pub loop_signal: LoopSignal,
    pub start_time: Instant,

    pub backend: Backend,

    pub space: PhysicalSpace,
    pub popups: PopupManager,
    /// Toplevels that exist but have not yet committed their first buffer.
    pub unmapped_windows: Vec<Window>,
    /// All managed windows by id — the extension surface's handle space.
    pub windows: HashMap<u64, Window>,
    next_window_id: u64,
    /// Last geometry requested by Lua, for `show()` without `set_geometry()`.
    desired_loc: HashMap<u64, Point<i32, Physical>>,
    /// Geometry before the *native* fullscreen fallback kicked in, restored
    /// on unfullscreen. Unused when Lua policy consumes the requests.
    pub(crate) fullscreen_prev: HashMap<u64, Rectangle<i32, Physical>>,
    /// Reentrancy guard: true while running Lua hooks / applying their ops.
    pub(crate) in_lua: bool,
    /// Buttons whose press a Lua hook consumed: their release is swallowed
    /// too, so clients never see half a click.
    pub(crate) consumed_buttons: std::collections::HashSet<u32>,
    /// Window under the pointer as of the last motion, for enter/leave
    /// diffing and focus-follows-mouse.
    pub(crate) hovered_window: Option<u64>,
    /// Persistent border buffers (stable element ids for damage tracking).
    /// Four slabs per window (top, bottom, left, right) so transparent
    /// windows don't show border color through their whole surface.
    pub border_buffers: HashMap<Window, [SolidColorBuffer; 4]>,
    pub cursor: Cursor,

    pub compositor_state: CompositorState,
    pub layer_shell_state: WlrLayerShellState,
    pub xdg_shell_state: XdgShellState,
    /// Held to keep the zxdg-decoration global alive; handlers drive policy.
    #[allow(dead_code)]
    pub xdg_decoration_state: XdgDecorationState,
    pub kde_decoration_state: KdeDecorationState,
    pub shm_state: ShmState,
    /// Held to keep the xdg-output global alive.
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    /// Held to keep the wp-viewporter global alive.
    #[allow(dead_code)]
    pub viewporter_state: ViewporterState,
    /// Held to keep the wp-fractional-scale global alive.
    #[allow(dead_code)]
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub seat_state: SeatState<Tomoe>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    /// zwlr-data-control: clipboard managers and `wl-copy`/`wl-paste`.
    pub wlr_data_control_state: WlrDataControlState,
    /// ext-data-control: the standardized successor; newer wl-clipboard.
    pub ext_data_control_state: ExtDataControlState,
    /// Held to keep the pointer-constraints global alive; the activation and
    /// lock/confine logic lives in the input path.
    #[allow(dead_code)]
    pub pointer_constraints_state: PointerConstraintsState,
    /// Held to keep the relative-pointer global alive.
    #[allow(dead_code)]
    pub relative_pointer_manager_state: RelativePointerManagerState,
    /// Held to keep the presentation-time global alive.
    #[allow(dead_code)]
    pub presentation_state: PresentationState,
    pub dmabuf_state: DmabufState,
    /// linux-drm-syncobj-v1 (explicit sync). Created by the TTY backend when
    /// the primary GPU supports `syncobj_eventfd`; absent on winit.
    pub syncobj_state: Option<DrmSyncobjState>,
    /// wlr-screencopy: per-manager frame queues (grim, xdg-desktop-portal-wlr).
    pub screencopy_state: crate::protocols::screencopy::ScreencopyManagerState,
    /// Held to keep the ext-image-capture-source dispatch alive.
    #[allow(dead_code)]
    pub image_capture_source_state: ImageCaptureSourceState,
    pub output_capture_source_state: OutputCaptureSourceState,
    pub toplevel_capture_source_state: ToplevelCaptureSourceState,
    pub image_copy_capture_state: ImageCopyCaptureState,
    /// Live ext-image-copy-capture sessions; dropping one sends `stopped`.
    pub capture_sessions: Vec<Session>,
    /// Capture frames waiting on the next on-screen render
    /// (`capture.rs::complete_capture_frames` — pacing, not correctness).
    pub pending_capture_frames: Vec<(SessionRef, CaptureFrame)>,
    pub foreign_toplevel_state: ForeignToplevelListState,
    /// Foreign-toplevel handles by window id, mapped windows only
    /// (`foreign_toplevel.rs`).
    pub foreign_toplevels: HashMap<u64, ForeignToplevelHandle>,
    pub session_lock_state: SessionLockManagerState,
    /// ext-session-lock progression (see `lock.rs`).
    pub lock_state: LockState,
    /// Lock surfaces by output; entries only exist while a lock is underway.
    pub lock_surfaces: HashMap<Output, LockSurface>,
    /// Persistent locked-backdrop buffers (stable element ids for damage
    /// tracking), created on first locked render of each output.
    pub lock_backdrops: HashMap<Output, SolidColorBuffer>,
    /// Outputs whose latest frame on screen is a locked one; a pending lock
    /// confirms only when this covers every output.
    pub lock_rendered: HashSet<Output>,
    pub idle_notifier_state: IdleNotifierState<Tomoe>,
    /// Held to keep the zwp-idle-inhibit global alive; handlers track the
    /// inhibiting surfaces below.
    #[allow(dead_code)]
    pub idle_inhibit_state: IdleInhibitManagerState,
    /// Surfaces holding a live idle inhibitor; only *visible* ones actually
    /// inhibit (re-checked every loop iteration in `refresh_idle_inhibit`).
    pub idle_inhibiting_surfaces: HashSet<WlSurface>,
    /// Debounce: activity was already notified this event-loop iteration
    /// (reset in the main loop callback).
    pub notified_activity: bool,

    pub seat: Seat<Tomoe>,
    pub cursor_status: CursorImageStatus,
    /// Block cursor drawn when no xcursor theme loaded and no client surface;
    /// persistent so damage trackers see a stable element id.
    pub cursor_fallback: SolidColorBuffer,
    /// xwayland-satellite integration (X11 sockets + on-demand spawn); None
    /// when the binary is missing or socket setup failed.
    pub satellite: Option<crate::xwayland::Satellite>,
    /// Monotonic clock for presentation-time feedback (the same clock the
    /// wp-presentation global advertises).
    pub clock: Clock<Monotonic>,

    pub lua: LuaRuntime,
    pub binds: Vec<Bind>,
    /// Keyboard settings as last applied to the seat, so `after_lua` (which
    /// runs on every Lua entry) only rebuilds the keymap on real changes.
    applied_keyboard: KeyboardSettings,

    /// Child processes: the `tomoe.process` manifest reconciler plus
    /// fire-and-forget spawns (`process.rs`).
    pub process: ProcessManager,
    /// Compositor-owned manifest entries (session bring-up units): the same
    /// declaration shape as `tomoe.process`, merged over the user's entries
    /// on every reconcile — ids carry a reserved "tomoe:" prefix.
    builtin_processes: HashMap<String, ProcessDecl>,
    /// The 1 Hz supervision timer is registered (dropped when idle so an
    /// empty manifest costs no wakeups).
    process_timer_active: bool,

    pub ui: Ui,
    /// IPC socket server state (`ipc.rs`): connected clients + subscriptions.
    pub ipc: crate::ipc::IpcState,
    /// `--config` argument; the effective path is re-resolved on each check.
    config_cli_path: Option<PathBuf>,
    config_fingerprint: Option<ConfigFingerprint>,
}

impl Tomoe {
    pub fn new(
        loop_handle: LoopHandle<'static, Tomoe>,
        loop_signal: LoopSignal,
        display_handle: DisplayHandle,
    ) -> Result<Self> {
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        // Advertise server-side decorations (both the xdg and the legacy KDE
        // protocol) so clients like Firefox/Librewolf drop their CSD titlebar
        // and every window gets the same compositor-drawn border.
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let kde_decoration_state =
            KdeDecorationState::new::<Self>(&display_handle, KdeDecorationsMode::Server);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);
        // Both are required for sharp fractional-scale rendering: the client
        // learns the exact scale and commits a buffer that covers exactly the
        // configured logical size, no resampling on either side.
        let viewporter_state = ViewporterState::new::<Self>(&display_handle);
        let fractional_scale_manager_state =
            FractionalScaleManagerState::new::<Self>(&display_handle);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        // Data-control globals let clipboard tools (`wl-copy`, clipboard
        // managers) read and set the selection without keyboard focus.
        let wlr_data_control_state = WlrDataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );
        let ext_data_control_state = ExtDataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );
        let pointer_constraints_state = PointerConstraintsState::new::<Self>(&display_handle);
        let relative_pointer_manager_state =
            RelativePointerManagerState::new::<Self>(&display_handle);
        let presentation_state =
            PresentationState::new::<Self>(&display_handle, Monotonic::ID as u32);
        let dmabuf_state = DmabufState::new();
        let screencopy_state = crate::protocols::screencopy::ScreencopyManagerState::new::<Self, _>(
            &display_handle,
            |_| true,
        );
        // Passive hint global: records per-surface tearing preferences that
        // the tty backend consults on the fullscreen flip path. No state to
        // keep — the display owns the global, the hints live in surface data.
        crate::protocols::tearing_control::TearingControlManagerState::new::<Self>(&display_handle);
        let image_capture_source_state = ImageCaptureSourceState::new();
        let output_capture_source_state = OutputCaptureSourceState::new::<Self>(&display_handle);
        let toplevel_capture_source_state =
            ToplevelCaptureSourceState::new::<Self>(&display_handle);
        let image_copy_capture_state = ImageCopyCaptureState::new::<Self>(&display_handle);
        let foreign_toplevel_state = ForeignToplevelListState::new::<Self>(&display_handle);
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&display_handle, |_| true);
        let idle_notifier_state = IdleNotifierState::new(&display_handle, loop_handle.clone());
        let idle_inhibit_state = IdleInhibitManagerState::new::<Self>(&display_handle);

        let mut seat = seat_state.new_wl_seat(&display_handle, "tomoe");
        seat.add_keyboard(XkbConfig::default(), 600, 25)
            .map_err(|err| anyhow!("error adding keyboard: {err:?}"))?;
        seat.add_pointer();

        let lua =
            LuaRuntime::new().map_err(|err| anyhow!("error initializing Lua runtime: {err}"))?;

        Ok(Self {
            display_handle,
            loop_handle,
            loop_signal,
            start_time: Instant::now(),
            backend: Backend::Uninit,
            space: PhysicalSpace::new(),
            popups: PopupManager::default(),
            unmapped_windows: Vec::new(),
            windows: HashMap::new(),
            next_window_id: 1,
            desired_loc: HashMap::new(),
            fullscreen_prev: HashMap::new(),
            in_lua: false,
            consumed_buttons: std::collections::HashSet::new(),
            hovered_window: None,
            border_buffers: HashMap::new(),
            cursor: Cursor::load(),
            compositor_state,
            layer_shell_state,
            xdg_shell_state,
            xdg_decoration_state,
            kde_decoration_state,
            shm_state,
            output_manager_state,
            viewporter_state,
            fractional_scale_manager_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            wlr_data_control_state,
            ext_data_control_state,
            pointer_constraints_state,
            relative_pointer_manager_state,
            presentation_state,
            dmabuf_state,
            syncobj_state: None,
            screencopy_state,
            image_capture_source_state,
            output_capture_source_state,
            toplevel_capture_source_state,
            image_copy_capture_state,
            capture_sessions: Vec::new(),
            pending_capture_frames: Vec::new(),
            foreign_toplevel_state,
            foreign_toplevels: HashMap::new(),
            session_lock_state,
            lock_state: LockState::default(),
            lock_surfaces: HashMap::new(),
            lock_backdrops: HashMap::new(),
            lock_rendered: HashSet::new(),
            idle_notifier_state,
            idle_inhibit_state,
            idle_inhibiting_surfaces: HashSet::new(),
            notified_activity: false,
            seat,
            cursor_status: CursorImageStatus::default_named(),
            cursor_fallback: SolidColorBuffer::new((8, 16), [1.0, 1.0, 1.0, 1.0]),
            satellite: None,
            clock: Clock::new(),
            lua,
            binds: Vec::new(),
            applied_keyboard: KeyboardSettings::default(),
            process: ProcessManager::default(),
            builtin_processes: HashMap::new(),
            process_timer_active: false,
            ui: Ui::new(),
            ipc: crate::ipc::IpcState::default(),
            config_cli_path: None,
            config_fingerprint: None,
        })
    }

    /// Load the user config (or the built-in default), then apply binds/settings.
    pub fn load_config(&mut self, cli_path: Option<PathBuf>) {
        self.config_cli_path = cli_path;
        let path = crate::lua::resolve_config_path(self.config_cli_path.as_deref());
        self.config_fingerprint = config_fingerprint(path.as_deref());
        self.process.begin_generation(path.as_deref());
        if let Err(err) = self.lua.load(path.as_deref()) {
            warn!("config error (continuing with defaults): {err:#}");
            self.show_config_error(
                "Failed to load the config file. Running with defaults; check the log for details.",
            );
        }
        self.apply_binds();
        self.lua.mark_processes_dirty();
        self.after_lua();
    }

    fn apply_binds(&mut self) {
        self.binds.clear();
        let mod_key = self.lua.settings().mod_key;
        for pending in self.lua.take_binds() {
            match crate::input::parse_combo(&pending.combo, mod_key) {
                Ok((mods, keysym)) => self.binds.push(Bind {
                    combo: pending.combo,
                    mods,
                    keysym,
                    action: pending.action,
                    desc: pending.desc,
                }),
                Err(err) => warn!("invalid keybind {:?}: {err:#}", pending.combo),
            }
        }
        self.ui.hotkey_overlay.invalidate();
    }

    /// Config watcher tick (main.rs timer): reload if the file changed.
    pub fn check_config_reload(&mut self) {
        let path = crate::lua::resolve_config_path(self.config_cli_path.as_deref());
        let fingerprint = config_fingerprint(path.as_deref());
        if fingerprint != self.config_fingerprint {
            info!("config file changed, reloading");
            self.reload_config();
        }
    }

    /// Reload the config into a fresh Lua VM. If it fails to load, the
    /// running VM (and thus the current config) is left untouched.
    pub fn reload_config(&mut self) {
        let path = crate::lua::resolve_config_path(self.config_cli_path.as_deref());
        self.config_fingerprint = config_fingerprint(path.as_deref());

        let mut new_lua = match LuaRuntime::new() {
            Ok(lua) => lua,
            Err(err) => {
                warn!("error creating Lua runtime for reload: {err}");
                return;
            }
        };
        if let Err(err) = new_lua.load(path.as_deref()) {
            warn!("config reload error (keeping the running config): {err:#}");
            self.show_config_error(
                "Failed to load the config file. Keeping the running config; check the log for details.",
            );
            return;
        }

        // Persist config-owned state out of the old VM (`tomoe.on_reload`
        // save hooks) — only now, after the new config loaded, so a broken
        // config never disturbs the running one. Values cross as JSON; ops a
        // save hook might queue die with the VM (save is a read-only affair).
        self.sync_snapshot();
        let saved = self.lua.save_reload_state();

        self.lua = new_lua;
        self.apply_binds();
        self.ui.config_error.hide();
        // A new config generation: `once_per_config_version` entries become
        // due, and the manifest is force-taken even if the fresh VM declared
        // no processes — removal from the config is itself a diff that must
        // stop the removed services.
        self.process.begin_generation(path.as_deref());
        self.lua.mark_processes_dirty();

        // The fresh VM has no WM state. Preferred path: hand the old VM's
        // saved state to the new config's `tomoe.on_reload` restore hooks.
        // Fallback (no restore ran — the config doesn't persist): replay
        // every existing window through on_window_open, oldest first, so it
        // can rebuild its layout. Never both — a restored WM replaying opens
        // would track every window twice.
        self.sync_snapshot();
        let was_in_lua = self.in_lua;
        self.in_lua = true;
        let restored = self.lua.restore_reload_state(&saved);
        if restored == 0 && self.lua.has_window_open_hooks() {
            let mut ids: Vec<u64> = self.windows.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                self.lua.emit_window_open(id);
            }
        }
        self.in_lua = was_in_lua;
        self.after_lua();
        info!("config reloaded ({restored} on_reload state(s) restored)");
    }

    /// Show the config-error banner and schedule the repaint that removes it
    /// (rendering is damage-driven, so expiry alone would never repaint).
    fn show_config_error(&mut self, message: &str) {
        self.ui.config_error.show(message);
        self.queue_redraw_all();
        let timer = Timer::from_duration(
            crate::ui::ConfigErrorNotification::TIMEOUT + Duration::from_millis(50),
        );
        let _ = self.loop_handle.insert_source(timer, |_, _, tomoe| {
            tomoe.queue_redraw_all();
            TimeoutAction::Drop
        });
    }

    // ── Snapshot & extension-surface plumbing ──

    /// Refresh the Lua-visible snapshot. Returns true if outputs changed.
    pub fn sync_snapshot(&mut self) -> bool {
        let windows = self.collect_win_props();
        let outputs = self.collect_output_props();
        let view_offset = self.space.view_offset();
        let view = (view_offset.x, view_offset.y, self.space.view_zoom());
        let scale = self.space.scale();
        let pointer = self
            .seat
            .get_pointer()
            .map(|p| {
                let screen = coords::point_to_physical(p.current_location(), scale);
                let world = self.space.screen_to_world(screen);
                (world.x, world.y, screen.x, screen.y)
            })
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        self.lua.sync(windows, outputs, view, pointer)
    }

    /// Current window properties, as both the Lua snapshot and IPC see them.
    pub fn collect_win_props(&self) -> HashMap<u64, WinProps> {
        let focused_surface = self.seat.get_keyboard().and_then(|kb| kb.current_focus());
        let mut windows = HashMap::new();
        for (id, window) in &self.windows {
            let (app_id, title) = window
                .toplevel()
                .map(|toplevel| {
                    with_states(toplevel.wl_surface(), |states| {
                        let data = states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .unwrap()
                            .lock()
                            .unwrap();
                        (
                            data.app_id.clone().unwrap_or_default(),
                            data.title.clone().unwrap_or_default(),
                        )
                    })
                })
                .unwrap_or_default();
            let geometry = self
                .space
                .element_geometry(window)
                .map(|geo| (geo.loc.x, geo.loc.y, geo.size.w, geo.size.h));
            let focused = window.toplevel().map(|t| t.wl_surface().clone()) == focused_surface;
            let (fullscreen, maximized) = window
                .toplevel()
                .map(|t| {
                    t.with_committed_state(|state| {
                        state
                            .map(|s| {
                                (
                                    s.states.contains(xdg_toplevel::State::Fullscreen),
                                    s.states.contains(xdg_toplevel::State::Maximized),
                                )
                            })
                            .unwrap_or_default()
                    })
                })
                .unwrap_or_default();
            windows.insert(
                *id,
                WinProps {
                    app_id,
                    title,
                    geometry,
                    mapped: geometry.is_some(),
                    focused,
                    fullscreen,
                    maximized,
                },
            );
        }
        windows
    }

    /// Current output properties, as both the Lua snapshot and IPC see them.
    pub fn collect_output_props(&self) -> Vec<OutputProps> {
        let scale = self.space.scale();
        let mut outputs = Vec::new();
        for output in self.space.outputs() {
            let Some(geo) = self.space.output_geometry(output) else {
                continue;
            };
            // Layer-shell exclusive zones are logical; Lua speaks physical.
            let zone =
                coords::rect_to_physical(layer_map_for_output(output).non_exclusive_zone(), scale);
            outputs.push(OutputProps {
                name: output.name(),
                geometry: (geo.loc.x, geo.loc.y, geo.size.w, geo.size.h),
                usable: (
                    geo.loc.x + zone.loc.x,
                    geo.loc.y + zone.loc.y,
                    zone.size.w,
                    zone.size.h,
                ),
            });
        }
        outputs
    }

    /// Apply queued Lua ops and actions after any Lua entry point.
    pub fn after_lua(&mut self) {
        let was_in_lua = self.in_lua;
        self.in_lua = true;
        loop {
            let ops = self.lua.take_ops();
            if ops.is_empty() {
                break;
            }
            for op in ops {
                self.apply_op(op);
            }
        }
        let actions = self.lua.take_actions();
        for action in actions {
            self.do_action(action);
        }
        self.in_lua = was_in_lua;
        crate::ipc::flush_lua_broadcasts(self);
        self.reconcile_processes();
        self.apply_keyboard_settings();
        crate::backend::tty::apply_libinput_settings(self);
        // Displays before scale: scale math reads output geometry. A mode
        // change re-emits outputs_changed (recursion bottoms out: the re-pick
        // is idempotent, so the nested pass sees no change).
        if crate::backend::tty::apply_display_settings(self) {
            self.outputs_changed(false);
        }
        self.apply_scale();
        self.sync_snapshot();
        self.queue_redraw_all();
    }

    /// Declare a compositor-owned process-manifest entry. Built-ins ride the
    /// same manifest and diff semantics as user `tomoe.process` declarations
    /// (doctrine 01); the id is namespaced "tomoe:" so user entries can't
    /// collide. Takes effect on the next `reconcile_processes` (which every
    /// Lua entry reaches via `after_lua`).
    pub fn declare_builtin_process(&mut self, id: &str, decl: ProcessDecl) {
        self.builtin_processes.insert(format!("tomoe:{id}"), decl);
        self.lua.mark_processes_dirty();
    }

    /// Drive children to match the manifest: fire-and-forget spawns, then the
    /// user manifest with the builtin entries merged over it.
    pub fn reconcile_processes(&mut self) {
        for spec in self.lua.take_spawns() {
            self.process.spawn_detached(&spec);
        }
        if let Some(mut manifest) = self.lua.take_process_manifest() {
            manifest.extend(
                self.builtin_processes
                    .iter()
                    .map(|(id, decl)| (id.clone(), decl.clone())),
            );
            self.process.reconcile(&manifest);
        }
        self.ensure_process_timer();
    }

    /// Keep the 1 Hz process-supervision timer alive exactly while there are
    /// children to poll (`process.rs`); it drops itself when the last child
    /// is reaped, so an idle session pays no wakeups for this.
    fn ensure_process_timer(&mut self) {
        if self.process_timer_active || !self.process.needs_supervision() {
            return;
        }
        const TICK: Duration = Duration::from_secs(1);
        let timer = Timer::from_duration(TICK);
        match self.loop_handle.insert_source(timer, |_, _, tomoe| {
            if tomoe.process.tick() {
                TimeoutAction::ToDuration(TICK)
            } else {
                tomoe.process_timer_active = false;
                TimeoutAction::Drop
            }
        }) {
            Ok(_) => self.process_timer_active = true,
            Err(err) => warn!("error inserting process supervision timer: {err}"),
        }
    }

    /// Apply a changed `settings.keyboard` to the seat: recompile the xkb
    /// keymap and update key-repeat info. Works on any backend (the keymap is
    /// compositor-side). A keymap that fails to compile keeps the previous
    /// one, but still counts as applied — the retry trigger is the next
    /// settings *change*, not the next Lua entry.
    fn apply_keyboard_settings(&mut self) {
        let kb = self.lua.settings().keyboard;
        if kb == self.applied_keyboard {
            return;
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            let config = XkbConfig {
                rules: &kb.rules,
                model: &kb.model,
                layout: &kb.layout,
                variant: &kb.variant,
                options: kb.options.clone(),
            };
            if let Err(err) = keyboard.set_xkb_config(self, config) {
                warn!("error applying settings.keyboard (keeping the previous keymap): {err:?}");
            }
            keyboard.change_repeat_info(kb.repeat_rate, kb.repeat_delay);
        }
        self.applied_keyboard = kb;
    }

    /// Apply a changed `settings.scale` to the space, outputs, and surfaces.
    /// Snapshot geometry stays physical, so Lua configs are scale-agnostic;
    /// they observe the change as an output-geometry change on the next event.
    fn apply_scale(&mut self) {
        let snapped = coords::snap_scale(self.lua.settings().scale);
        if snapped == self.space.scale() {
            return;
        }
        self.space.set_scale(snapped);
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        for output in &outputs {
            let logical_loc = self
                .space
                .output_geometry(output)
                .map(|geo| coords::rect_to_logical(geo, snapped).loc)
                .unwrap_or_default();
            output.change_current_state(
                None,
                None,
                Some(OutputScale::Fractional(snapped)),
                Some(logical_loc),
            );
        }
        let surfaces: Vec<WlSurface> = self
            .windows
            .values()
            .chain(self.unmapped_windows.iter())
            .filter_map(|w| w.toplevel().map(|t| t.wl_surface().clone()))
            .collect();
        for surface in surfaces {
            send_scale(&surface, snapped);
        }
    }

    /// Schedule a repaint on every output (damage-driven; cheap if already queued).
    pub fn queue_redraw_all(&mut self) {
        if let Backend::Winit(data) = &mut self.backend {
            data.backend.window().request_redraw();
            return;
        }
        crate::backend::tty::queue_redraw_all(self);
    }

    fn apply_op(&mut self, op: WindowOp) {
        let window = |tomoe: &Self, id: u64| tomoe.windows.get(&id).cloned();
        match op {
            WindowOp::SetGeometry(id, (x, y, w, h)) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                // Lua speaks physical pixels; xdg configure takes integer
                // logical, so the achievable size is quantized once here.
                let (logical, _achievable) =
                    coords::configure_size(Size::from((w, h)), self.space.scale());
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some(logical);
                    });
                    toplevel.send_pending_configure();
                }
                self.desired_loc.insert(id, (x, y).into());
                self.space.map_element(window, (x, y));
            }
            WindowOp::Show(id) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                let loc = self.desired_loc.get(&id).copied().unwrap_or_default();
                self.space.map_element(window, loc);
            }
            WindowOp::Hide(id) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                self.space.unmap(&window);
            }
            WindowOp::Focus(id) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                self.focus_window(Some(&window));
            }
            WindowOp::ClearFocus => self.focus_window(None),
            WindowOp::Close(id) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
            }
            WindowOp::Raise(id) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                self.space.raise_element(&window);
            }
            WindowOp::SetFullscreen(id, on) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        if on {
                            state.states.set(xdg_toplevel::State::Fullscreen);
                        } else {
                            state.states.unset(xdg_toplevel::State::Fullscreen);
                            state.fullscreen_output = None;
                        }
                    });
                    toplevel.send_pending_configure();
                }
            }
            WindowOp::SetMaximized(id, on) => {
                let Some(window) = window(self, id) else {
                    return;
                };
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        if on {
                            state.states.set(xdg_toplevel::State::Maximized);
                        } else {
                            state.states.unset(xdg_toplevel::State::Maximized);
                        }
                    });
                    toplevel.send_pending_configure();
                }
            }
            WindowOp::SetView(x, y, zoom) => {
                self.space.set_view((x, y).into(), zoom);
            }
        }
    }

    /// A toplevel committed its first buffer: hand it to the extension surface.
    pub fn add_window(&mut self, window: Window) {
        let id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.insert(id, window.clone());
        self.publish_foreign_toplevel(id, &window);
        if let Some(toplevel) = window.toplevel() {
            send_scale(toplevel.wl_surface(), self.space.scale());
        }

        if self.lua.has_window_open_hooks() {
            self.sync_snapshot();
            let was_in_lua = self.in_lua;
            self.in_lua = true;
            self.lua.emit_window_open(id);
            self.in_lua = was_in_lua;
            self.after_lua();
        } else {
            // Mechanism-level fallback: no WM loaded, map full-screen so a
            // broken config still shows windows.
            let output = self.space.outputs().next().cloned();
            if let Some(output) = output {
                if let Some(geo) = self.space.output_geometry(&output) {
                    let scale = self.space.scale();
                    let zone = coords::rect_to_physical(
                        layer_map_for_output(&output).non_exclusive_zone(),
                        scale,
                    );
                    let (logical, _achievable) = coords::configure_size(zone.size, scale);
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.with_pending_state(|state| {
                            state.size = Some(logical);
                        });
                        toplevel.send_pending_configure();
                    }
                    self.space.map_element(window.clone(), geo.loc + zone.loc);
                }
            }
            self.focus_window(Some(&window));
            self.queue_redraw_all();
        }
        // After Lua so subscribers see the geometry policy just assigned.
        crate::ipc::notify_window_open(self, id);
    }

    /// A toplevel was destroyed.
    pub fn window_closed(&mut self, window: &Window) {
        let id = self
            .windows
            .iter()
            .find(|(_, w)| *w == window)
            .map(|(id, _)| *id);
        self.border_buffers.remove(window);
        self.space.unmap(window);
        let Some(id) = id else { return };
        self.windows.remove(&id);
        self.retire_foreign_toplevel(id);
        // A session capturing this window stops now (its source is gone).
        crate::capture::refresh_capture_sessions(self);
        self.desired_loc.remove(&id);
        self.fullscreen_prev.remove(&id);
        // No leave event for a window that no longer exists; the next motion
        // re-diffs against whatever is under the pointer now.
        if self.hovered_window == Some(id) {
            self.hovered_window = None;
        }

        // Emission is gated on *close* hooks; a config may register only these.
        if self.lua.has_window_close_hooks() {
            self.sync_snapshot();
            let was_in_lua = self.in_lua;
            self.in_lua = true;
            self.lua.emit_window_close(id);
            self.in_lua = was_in_lua;
            self.after_lua();
        }
        // Mechanism fallback: no WM is placing windows, so focus the topmost.
        if !self.lua.has_window_open_hooks() {
            let next = self.space.elements().next_back().cloned();
            self.focus_window(next.as_ref());
        }
        crate::ipc::notify_window_close(self, id);
        self.queue_redraw_all();
    }

    /// Outputs or usable areas changed; notify Lua (only on real change unless forced).
    pub fn outputs_changed(&mut self, force: bool) {
        // Capture sessions negotiate buffers per output size; renegotiate (or
        // stop sessions for removed outputs) before policy reacts.
        crate::capture::refresh_capture_sessions(self);
        // Lock surfaces track output sizes; a pending lock may also complete
        // (or newly need surfaces) when the output set changes.
        self.refresh_lock_state();
        let changed = self.sync_snapshot();
        if !(changed || force) {
            return;
        }
        let was_in_lua = self.in_lua;
        self.in_lua = true;
        self.lua.emit_outputs_changed();
        self.in_lua = was_in_lua;
        self.after_lua();
        crate::ipc::notify_outputs_changed(self);
    }

    /// A client asked for a window-state change (fullscreen/maximize/…) or
    /// an interactive drag (move/resize); hand it to Lua policy. Returns true
    /// if a hook consumed the request, in which case the caller must not
    /// apply its native default.
    pub fn emit_window_request(
        &mut self,
        id: u64,
        kind: &str,
        output: Option<String>,
        edges: Option<&str>,
    ) -> bool {
        if !self.lua.has_window_request_hooks() {
            return false;
        }
        self.sync_snapshot();
        let was_in_lua = self.in_lua;
        self.in_lua = true;
        let consumed = self.lua.emit_window_request(id, kind, output, edges);
        self.in_lua = was_in_lua;
        self.after_lua();
        consumed
    }

    // ── Lookup helpers ──

    /// Extension-surface id of a *mapped* window (unmapped toplevels have
    /// no id yet — they haven't been handed to Lua).
    pub fn window_id_for_surface(&self, surface: &WlSurface) -> Option<u64> {
        self.windows
            .iter()
            .find(|(_, w)| w.toplevel().map(|t| t.wl_surface()) == Some(surface))
            .map(|(id, _)| *id)
    }

    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.windows
            .values()
            .chain(self.unmapped_windows.iter())
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(surface))
            .cloned()
    }

    /// Find the surface under a physical-space position. The returned
    /// location is protocol-logical (for the seat): clients receive
    /// `pointer_location - surface_location`, and with both sides converted
    /// by the same division that difference is an exact surface-local coord.
    pub fn surface_under(
        &self,
        pos: Point<f64, Physical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let scale = self.space.scale();
        let output = self.space.output_under(pos)?.clone();
        let output_geo = self.space.output_geometry(&output)?;

        // While locked, the pointer can only ever land on the output's lock
        // surface — checked before anything else so no window, layer, or
        // popup is reachable.
        if self.is_locked() {
            let surface = self.lock_surfaces.get(&output)?;
            let rel = coords::point_to_protocol(pos - output_geo.loc.to_f64(), scale);
            let output_protocol_loc = coords::point_to_protocol(output_geo.loc.to_f64(), scale);
            let (surface, loc) = smithay::desktop::utils::under_from_surface_tree(
                surface.wl_surface(),
                rel,
                (0, 0),
                WindowSurfaceType::ALL,
            )?;
            return Some((surface, loc.to_f64() + output_protocol_loc));
        }
        // Layer maps arrange in output-local logical coordinates.
        let rel = coords::point_to_protocol(pos - output_geo.loc.to_f64(), scale);
        let output_protocol_loc = coords::point_to_protocol(output_geo.loc.to_f64(), scale);
        let layers = layer_map_for_output(&output);

        let layer_hit = |kinds: [WlrLayer; 2]| {
            for layer_kind in kinds {
                if let Some(layer) = layers.layer_under(layer_kind, rel) {
                    let Some(layer_loc) = layers.layer_geometry(layer).map(|geo| geo.loc) else {
                        continue;
                    };
                    if let Some((surface, loc)) =
                        layer.surface_under(rel - layer_loc.to_f64(), WindowSurfaceType::ALL)
                    {
                        return Some((surface, (loc + layer_loc).to_f64() + output_protocol_loc));
                    }
                }
            }
            None
        };

        if let Some(hit) = layer_hit([WlrLayer::Overlay, WlrLayer::Top]) {
            return Some(hit);
        }

        // Windows live in world space; the pointer is screen space. Hit-test
        // in world coordinates, and compensate the surface origin handed to
        // the seat so `pointer_location - origin` is still the exact
        // world-local (client buffer) coordinate at any pan/zoom.
        let world = self.space.screen_to_world(pos);
        if let Some((window, location)) = self.space.element_under(world) {
            let local = coords::point_to_protocol(world - location.to_f64(), scale);
            if let Some((surface, surface_loc)) =
                window.surface_under(local, WindowSurfaceType::ALL)
            {
                let compensated_loc =
                    coords::point_to_protocol(pos - world + location.to_f64(), scale);
                return Some((surface, surface_loc.to_f64() + compensated_loc));
            }
        }

        layer_hit([WlrLayer::Bottom, WlrLayer::Background])
    }

    /// Clamp a physical position onto the union bounding box of all outputs
    /// (the same clamp pointer motion applies).
    /// Keep the pointer on an output. Outputs sit at arbitrary positions
    /// (explicit `settings.displays` placement, possibly negative, possibly
    /// with gaps), so this clamps to the nearest output rect, not to a
    /// bounding box a gap could hide in. Inside any output it's a no-op.
    pub(crate) fn clamp_to_outputs(&self, pos: Point<f64, Physical>) -> Point<f64, Physical> {
        let mut best = pos;
        let mut best_d2 = f64::INFINITY;
        for output in self.space.outputs() {
            let Some(geo) = self.space.output_geometry(output) else {
                continue;
            };
            let clamped: Point<f64, Physical> = Point::from((
                pos.x
                    .clamp(geo.loc.x as f64, (geo.loc.x + geo.size.w) as f64 - 1.0),
                pos.y
                    .clamp(geo.loc.y as f64, (geo.loc.y + geo.size.h) as f64 - 1.0),
            ));
            let d2 = (clamped.x - pos.x).powi(2) + (clamped.y - pos.y).powi(2);
            if d2 < best_d2 {
                best_d2 = d2;
                best = clamped;
            }
        }
        best
    }

    /// Activate a pending pointer constraint on the surface under the pointer,
    /// if that surface holds pointer focus and the pointer is inside the
    /// constraint region. Called after motion and when a constraint is
    /// created; deactivation is smithay's (on pointer-focus change).
    pub(crate) fn maybe_activate_pointer_constraint(&self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let pos = coords::point_to_physical(pointer.current_location(), self.space.scale());
        let Some((surface, surface_loc)) = self.surface_under(pos) else {
            return;
        };
        if pointer.current_focus().as_ref() != Some(&surface) {
            return;
        }
        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint else { return };
            if constraint.is_active() {
                return;
            }
            if let Some(region) = constraint.region() {
                let within = pointer.current_location() - surface_loc;
                if !region.contains(within.to_i32_round()) {
                    return;
                }
            }
            constraint.activate();
        });
    }

    pub fn focused_window(&self) -> Option<Window> {
        let keyboard = self.seat.get_keyboard()?;
        let surface = keyboard.current_focus()?;
        self.windows
            .values()
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&surface))
            .cloned()
    }

    pub fn focus_window(&mut self, window: Option<&Window>) {
        self.focus_window_impl(window, true);
    }

    /// Focus without restacking: focus-follows-mouse must not pull a
    /// partially covered window above the one it's hovered under.
    pub(crate) fn focus_window_no_raise(&mut self, window: Option<&Window>) {
        self.focus_window_impl(window, false);
    }

    fn focus_window_impl(&mut self, window: Option<&Window>, raise: bool) {
        // While locked, keyboard focus belongs to the lock surface; window
        // focus (input-driven or Lua ops) must not steal it.
        if self.is_locked() {
            return;
        }
        for w in self.space.elements() {
            w.set_activated(Some(w) == window);
        }
        for w in self.space.elements() {
            if let Some(toplevel) = w.toplevel() {
                toplevel.send_pending_configure();
            }
        }
        if raise {
            if let Some(window) = window {
                self.space.raise_element(window);
            }
        }
        let focus = window
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, focus, serial);
        }

        let id = window.and_then(|win| {
            self.windows
                .iter()
                .find(|(_, w)| *w == win)
                .map(|(id, _)| *id)
        });

        // Notify Lua about input-driven focus changes (not its own Focus ops).
        if !self.in_lua {
            self.sync_snapshot();
            self.in_lua = true;
            self.lua.emit_focus_change(id);
            self.in_lua = false;
            self.after_lua();
        }
        // IPC subscribers hear about every focus change, Lua-driven included
        // (the emitter dedupes repeats).
        crate::ipc::notify_focus_change(self, id);
    }

    pub fn do_action(&mut self, action: Action) {
        match action {
            Action::Quit => {
                self.ui.exit_dialog.show();
                self.queue_redraw_all();
            }
            Action::ConfirmQuit => self.loop_signal.stop(),
            Action::ShowHotkeyOverlay => {
                self.ui.hotkey_overlay.show();
                self.queue_redraw_all();
            }
            Action::ReloadConfig => self.reload_config(),
            Action::Spawn(cmd) => {
                self.process.spawn_detached(&ProcessSpec {
                    launch: Launch::Shell(cmd),
                    cwd: None,
                    env: Default::default(),
                });
                self.ensure_process_timer();
            }
            Action::CloseWindow => {
                if let Some(window) = self.focused_window() {
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.send_close();
                    }
                }
            }
            Action::LuaFn(idx) => {
                self.lua.call_bind(idx);
                self.after_lua();
            }
            Action::ChangeVt(vt) => self.change_vt(vt),
            Action::Screenshot => self.open_screenshot_ui(),
            Action::ScreenshotScreen => self.screenshot_screen(),
            Action::ScreenshotConfirm => self.screenshot_confirm(),
        }
    }

    /// The output under the pointer, falling back to the first output.
    fn output_under_pointer(&self) -> Option<smithay::output::Output> {
        let pos = self
            .seat
            .get_pointer()
            .map(|p| coords::point_to_physical(p.current_location(), self.space.scale()));
        pos.and_then(|pos| self.space.output_under(pos))
            .or_else(|| self.space.outputs().next())
            .cloned()
    }

    /// Open the interactive region-selection overlay on the output under the
    /// pointer; `input.rs` intercepts everything until it closes.
    fn open_screenshot_ui(&mut self) {
        // The overlay is modal over the session; a locked screen has no
        // session to select from (captures would only show the lock scene).
        if self.is_locked() {
            return;
        }
        let Some(output) = self.output_under_pointer() else {
            warn!("screenshot: no output to capture");
            return;
        };
        self.ui.screenshot.open(output);
        self.queue_redraw_all();
    }

    /// Confirm the screenshot UI: capture its selection (or the whole output
    /// when nothing is selected). Closes the overlay *before* capturing so
    /// the rendered scene never contains it.
    pub fn screenshot_confirm(&mut self) {
        let Some(output) = self.ui.screenshot.output().cloned() else {
            return;
        };
        let region = self
            .space
            .output_geometry(&output)
            .and_then(|geo| self.ui.screenshot.selection_rect(geo.size));
        self.ui.screenshot.close();
        self.queue_redraw_all();
        if let Err(err) = crate::screenshot::screenshot(self, &output, region) {
            warn!("error taking screenshot: {err:#}");
        }
    }

    // ── Idle (ext-idle-notify / zwp-idle-inhibit) ──

    /// Reset every idle-notification timer: the user did something. Called
    /// from the input path for every event, so it debounces to once per
    /// event-loop iteration (the flag resets in the main loop callback).
    pub fn notify_activity(&mut self) {
        if self.notified_activity {
            return;
        }
        self.notified_activity = true;
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
    }

    /// Recompute whether idle is inhibited: some surface holds a live
    /// inhibitor *and* is actually visible. Clients aren't trusted to
    /// destroy inhibitors when hidden or dying, so this runs every loop
    /// iteration. A locked session never counts as inhibited — whatever
    /// video was playing is not on screen.
    pub fn refresh_idle_inhibit(&mut self) {
        self.idle_inhibiting_surfaces.retain(|s| s.alive());
        let is_inhibited = !self.is_locked()
            && self
                .idle_inhibiting_surfaces
                .iter()
                .any(|surface| self.surface_visible(surface));
        self.idle_notifier_state.set_is_inhibited(is_inhibited);
    }

    /// Visibility proxy for idle inhibitors: the surface's root is a window
    /// currently mapped in the space, or a layer surface mapped on some
    /// output. (No per-surface scanout tracking here — an off-screen but
    /// mapped window still counts, occlusion doesn't.)
    fn surface_visible(&self, surface: &WlSurface) -> bool {
        if !surface.alive() {
            return false;
        }
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if let Some(window) = self.window_for_surface(&root) {
            return self.space.element_geometry(&window).is_some();
        }
        self.space.outputs().any(|output| {
            layer_map_for_output(output)
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
    }

    /// Screenshot the output under the pointer (falling back to the first
    /// output) and save it as a PNG.
    fn screenshot_screen(&mut self) {
        let Some(output) = self.output_under_pointer() else {
            warn!("screenshot: no output to capture");
            return;
        };
        if let Err(err) = crate::screenshot::screenshot(self, &output, None) {
            warn!("error taking screenshot: {err:#}");
        }
    }
}

/// Advertise the output scale to a surface: the integer buffer-scale hint
/// (wl_surface v6 preferred_buffer_scale) plus the exact fractional scale for
/// wp-fractional-scale clients. Sent on map, on scale changes, and when a
/// client binds the fractional-scale extension.
pub fn send_scale(surface: &WlSurface, scale: f64) {
    with_states(surface, |states| {
        send_surface_state(surface, states, scale.ceil() as i32, Transform::Normal);
        with_fractional_scale(states, |fractional| {
            fractional.set_preferred_scale(scale);
        });
    });
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
