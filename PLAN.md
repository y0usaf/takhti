# tomoe — parity plan

Working tracker toward the DESIGN.md vision: **niri's performance,
Hyprland's features, ShojiWM's configurability**. DESIGN.md holds doctrine
and locked decisions; this file holds the concrete gap list and the order
we close it in. Check items off as they land; re-audit against `ref/`
when a milestone completes.

## Where we are (2026-07)

Done and working:

- winit + TTY (DRM/GBM/libinput/libseat) backends, VT switching,
  multi-output (side-by-side), per-display mode selection,
  explicit sync (`backend/tty.rs`)
- Multi-GPU (niri-shape: every seat GPU opened, rendering pinned to the
  primary render node via `GpuManager`, cross-device copies for scanout;
  `--drm-device` overrides the render GPU) and output/GPU hotplug
  (udev → `DrmScanner` diff → connect/disconnect → `outputs_changed`);
  zero connected outputs is a wait-state, not an error
- niri-style per-output redraw state machine + estimated-vblank timer,
  damage-tracked rendering, frame-callback throttling
- Plane offloading: fullscreen direct scanout (primary plane, zero-copy)
  + hardware cursor plane via DrmCompositor frame flags; per-surface
  render/scanout dmabuf feedback steers clients onto flippable formats
  (NVIDIA compressed modifiers excluded). Overlay planes off. Tearing:
  `wp_tearing_control_v1` + async page flips via the smithay fork
  (`y0usaf/smithay#tomoe-tearing`), gated by `settings.tearing`, with
  the estimated-vblank bypass so tearing presents follow commit rate.
  Live verification pending (see M3 §4)
- Physical-first coordinate doctrine (`coords.rs`), fractional-scale +
  viewporter, dmabuf, layer-shell, xdg-decoration (+ KDE legacy),
  data-device/clipboard, primary selection, xdg-output,
  pointer-constraints + relative-pointer, presentation-time
- Lua runtime: binds (Mod-agnostic), spawn, settings (gaps/scale/border/
  displays/focus-follows-mouse), window handles + queued ops, hooks
  (open/close/focus/outputs/pointer button+axis, hover enter/leave),
  pointer grabs, world camera (`set_view`), hot reload with state
  persistence (`tomoe.on_reload(name, save, restore)`: save runs in the
  outgoing VM, values cross as JSON, restore runs in the fresh VM;
  `on_window_open` replay is the fallback when no restore ran). NB: the
  doctrine-02 dispatch *watchdog* is still unimplemented (no mlua
  hook/instruction limit anywhere) despite earlier claims here — a hung
  hook or `tomoe.ipc.serve` handler blocks the loop
- Process API (`tomoe.process.once/service/spawn`, ShojiWM-shape):
  declarative manifest diffed by id across reloads, restart/reload
  policies, 1 Hz supervision poll that also reaps fire-and-forget
  children (`process.rs`); builtin entries (reserved `tomoe:` id prefix,
  `Tomoe::declare_builtin_process`) ride the same manifest — the session
  bring-up chain is the first consumer
- IPC (doctrine 03): `tomoe-ipc` wire crate + `ipc.rs` calloop server —
  `tomoe msg` CLI, builtins (`version`/`windows`/`outputs`/`view`/
  `subscribe`/`quit`), `tomoe.ipc.serve` user endpoints running as normal
  Lua entries, `tomoe.ipc.broadcast` + core event stream (`window_open`,
  `window_close`, `focus_change`, `outputs_changed`); socket at
  `$XDG_RUNTIME_DIR/tomoe.<display>.sock`, `$TOMOE_SOCKET` exported and
  pushed to the activation environment
- Dogfood WMs: `wm.lua` (9 workspaces, dwindle, focus cycling),
  `zoomer.lua` (pan/zoom canvas, planes, drag move/resize)
- Compositor UI: hotkey overlay, exit confirm, config-error banner,
  screenshot UI; xcursor themes; no-hooks full-screen fallback — four
  hand-wired units in `ui/`, which is doctrine-05 debt: past the
  third unit the registry is due (paid down by M4's `tomoe.ui`)
- Session lock (ext-session-lock-v1, swaylock incl. crashed-locker
  fallback) and idle (ext-idle-notify + zwp-idle-inhibit, swayidle) —
  `lock.rs` + idle plumbing in `state.rs`
- XWayland via xwayland-satellite (niri-shape: tomoe owns the X11
  sockets/lock file, exports `DISPLAY` at startup, spawns satellite
  on-demand via `-listenfd` on first connection, respawns on next
  connection after a crash; `xwayland.rs`)
- Capture: wlr-screencopy v3 (shm + dmabuf, `copy` immediate,
  `copy_with_damage` queued per manager and completed from the redraw
  loop) and ext-image-copy-capture-v1 + ext-image-capture-source-v1
  (smithay handlers; output *and toplevel* sources — per-window renders
  crop to the xdg geometry, cursor embeds only while hovering, sessions
  renegotiate on window resize and stop on close; capture frames are
  queued and completed from the redraw loop, so casts pace to vblank
  instead of spinning); everything renders through one shared path in
  `capture.rs` on the primary GPU
- ext-foreign-toplevel-list-v1: handles published on window map, title/
  app_id pushed on commit, `closed` on unmap (`foreign_toplevel.rs`) —
  feeds bars and the portal's window enumeration
- ScreenCast portal: `xdg-desktop-portal-tomoe` binary (zbus + pipewire,
  ShojiWM-shape: `PW_STREAM_FLAG_DRIVER | ALLOC_BUFFERS`, single thread,
  wlr-screencopy `ready` queues the PW buffer and kicks the next capture
  → vblank-paced); monitor sources (dmabuf with shm fallback) *and
  window sources* (ext-foreign-toplevel-list enumeration +
  ext-image-copy-capture streaming, shm, resize renegotiation via
  `update_params`); no-GUI source choice (`TOMOE_SCREENCAST_OUTPUT` /
  `TOMOE_SCREENCAST_WINDOW` / `TOMOE_PORTAL_CHOOSER` dmenu lines /
  single-source auto) — the env-var chooser is a **recorded doctrine-05
  exemption**: screencast policy is the one piece of WM policy declared
  outside Lua, retired by M4 §7; compositor exports
  `XDG_CURRENT_DESKTOP=tomoe` and, on TTY, pushes `WAYLAND_DISPLAY`/
  `DISPLAY`/`XDG_CURRENT_DESKTOP`/`TOMOE_PORTAL_CHOOSER` into the
  systemd + D-Bus activation environment, pre-starts the GTK portal
  backend detached (it is a Wayland client of ours — blocking before the
  event loop deadlocks; and it must be up before the frontend activates,
  or NixOS's After= override deadlocks the frontend's synchronous
  backend calls), and try-restarts a stale frontend, so bus-activated
  backends find the session from a bare TTY launch; nix package ships
  `.portal` + `tomoe-portals.conf` + D-Bus service +
  `tomoe-session.target`. Session bring-up (start `tomoe-session.target`
  — its BindsTo pulls `graphical-session.target` up, systemd refuses
  starting that directly — then GTK backend, then frontend try-restart)
  runs as the builtin `tomoe:session-units` manifest entry; compositor
  exit stops the target (graphical-session.target follows,
  StopWhenUnneeded) and unsets the session env vars

## Gap inventory by reference

### vs niri (performance & session correctness)

- [x] presentation-time protocol (feedback rides the DRM frame as user data,
      fired from the vblank with the hardware timestamp; winit approximates
      with the compositor clock at submit)
- [x] Direct scanout for fullscreen surfaces; hardware cursor plane —
      `FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY | ALLOW_CURSOR_PLANE_SCANOUT`
      on the DrmCompositor render, plus per-surface render/scanout dmabuf
      feedback (niri-shape, NVIDIA compressed modifiers filtered from the
      scanout tranches per the standing lessons); engagement logged
      edge-triggered ("direct scanout engaged"). **Pending live
      verification**: fullscreen client on the primary plane via drm_info +
      the engage log; cursor-only motion not re-compositing
- [x] VRR (adaptive sync) per output (`settings.displays[..].vrr`, applied
      at connector bring-up and live-toggled on reload; explicit
      `use_vrr(false)` when unsupported/unwanted per niri's stale
      VRR_ENABLED workaround). Live verification pending (needs a
      VRR-capable monitor)
- [x] Tearing control (`wp_tearing_control_v1`) + async page flips
      (smithay fork `y0usaf/smithay#tomoe-tearing` = niri's pin + one
      async-flip commit; `settings.tearing` gates hint-requesting
      fullscreen windows, `TOMOE_FORCE_TEARING=1` for hint-less X11 games;
      live verification pending)
- [x] Output hotplug (udev → connector scan diff → connect/disconnect,
      reposition, `outputs_changed`)
- [x] Per-output position/mirror/disable (`settings.displays`: explicit
      physical `position`s anchor the layout and the rest packs after
      them; `mirror` maps at its target's position so it shows the same
      world region; `disabled` connectors stay stashed per device so a
      settings change re-enables them without a replug)
- [ ] Per-output scale (single global scale today; mixed-scale placement
      policy noted in DESIGN.md coordinate doctrine)
- [x] pointer-constraints + relative-pointer (games) — niri-shape: lock
      swallows motion, confine clamps at the surface/region edge, both keep
      sending relative motion; smithay deactivates on focus change
- [x] idle-notify + idle-inhibit (smithay's timers; activity fed from every
      input event, VT resume, and unlock, debounced per loop iteration;
      inhibitors honored only while their surface is mapped and the session
      unlocked)
- [x] ext-session-lock-v1 (niri-shape: `locked` confirmed only after every
      output rendered a locked frame; dark-red backdrop fallback; dead-locker
      replacement; locked scene also fed to all capture paths)
- [ ] xdg-activation
- [x] Primary selection (focus follows keyboard focus, same as the clipboard)
- [x] libinput device config (tap, accel, natural scroll, DWT, scroll/click
      method…) via `settings.touchpad`/`settings.mouse`, per-device
      overrides in `settings.devices["<libinput name>"]`; unset fields
      revert to libinput defaults so reloads are idempotent
- [x] xkb config (rules/model/layout/variant/options) + key repeat via
      `settings.keyboard`, re-applied on change from any Lua entry
- [ ] Steady-state zoom re-advertises effective fractional scale
      (DESIGN.md camera follow-up)

### vs Hyprland (WM depth & eye-candy)

- [x] Popup grabs (anvil-shape: `grab_popup` + PopupKeyboardGrab/
      PopupPointerGrab, refused when another grab holds the device)
- [x] Fullscreen/maximize/minimize requests plumbed as Lua
      `on_window_request` events, policy decides; native default honors
      fullscreen on the window's output, acks maximize, ignores minimize
- [x] xdg interactive move/resize forwarded to Lua as `on_window_request`
      ("move"/"resize" + edges); a consuming hook takes over via
      `tomoe.grab_pointer` (the core releases the client's click grab),
      unconsumed drags are dropped
- [x] Focus-follows-mouse (`settings.focus_follows_mouse`, sloppy focus,
      no restack) + `on_pointer_enter/leave` hover events, suppressed
      while any pointer grab is active
- [ ] Window rules (`tomoe.rule { app_id = ..., ... }`)
- [ ] Animation engine (springs + beziers on layout positions,
      AnimatedVariable-style; open/close/move/workspace-switch)
- [ ] Rounded corners (shader element, pixel-aligned per doctrine §5)
- [ ] Drop shadows
- [ ] Dual-kawase blur (windows + blur-behind for layer surfaces)
- [ ] Special workspaces / groups — Lua-level per policy split; needs
      mechanism audit (hide/show + per-window state suffice?)

### vs ShojiWM (extension surface)

- [x] Process API: `tomoe.process.once/service/spawn` declarative
      manifest, diffed by id, restart policies
      (`ref/ShojiWM/knowledges/process-api.md`) — `process.rs`
      ProcessManager + Lua manifest in `Shared`; 1 Hz polling supervision
      timer (only alive while children exist) doubles as crash-loop rate
      limit and zombie reaper; `tomoe.spawn` now reaps too
- [x] IPC: JSON socket + `tomoe msg` CLI + event stream +
      `tomoe.ipc.serve` for user-defined endpoints (bars/launchers) —
      `tomoe-ipc` wire crate (WIRE_VERSION, ShojiWM-shape ndjson frames)
      + calloop-hosted server in `ipc.rs`
- [x] Hot reload with state persistence: `tomoe.on_reload(name, save,
      restore)` (ShojiWM's onDisable/onEnable-with-persist shape, keyed by
      name so modules persist independently); `tomoe.window(id)` maps
      persisted ids back to handles; dogfooded by `wm.lua` (workspaces/
      active/fullscreen) and `zoomer.lua` (planes/cameras/fit)
- [x] Request events surfaced to Lua: maximize/minimize/fullscreen via
      `tomoe.on_window_request` (ShojiWM's `onWindow*Request` family);
      activate requests wait on xdg-activation (M5)
- [ ] Input-device change events (per-device *config* landed with M1 §5;
      the add/remove events + device query surface remain)
- [ ] Output reconfigure API (re-run config on hotplug, query available
      modes)
- [ ] LuaLS `---@meta` annotation files shipped for editor DX
- [ ] Layer-surface events (`on_layer_create/update/destroy`) and
      reserved-insets query (usable_area exists; insets breakdown doesn't)
- [ ] `tomoe.ui` — retained-widget overlay API (menu/confirm/toast):
      Lua declares the widget once, core renders and damages it; only
      selection events re-enter Lua (no per-frame Lua draw — keeps the
      doctrine-02 watchdog meaningful and fits render-on-damage). Modal
      input routing generalized from `screenshot_ui`. Registry per
      doctrine 05 — four hand-wired units already exist in `ui/`
- [ ] Portal source policy through the extension API:
      `tomoe.on_screencast_request` (snapshot in: app_id, requested
      types, candidate outputs/windows; actions out: resolve/deny/defer)
      with the backend as a thin IPC client — retires the
      `TOMOE_PORTAL_CHOOSER` env-var exemption
- [ ] Scriptable decorations (long-term; Hyprland-style built-in borders/
      titlebar first, ShojiWM-style Lua-driven SSD tree later — study
      `ref/ShojiWM/knowledges/shared-edge-tree-plan.md` before designing)

### Ecosystem (all three have it)

- [x] **XWayland via xwayland-satellite (expedited → M2)** — landed; the
      rough edge it shook loose was the legacy `wl_drm` global (smithay's
      wl_drm kills Xwayland with a fatal "invalid format" protocol error),
      so `bind_wl_display` is gone from both backends — clients use
      linux-dmabuf. No Lua toggle yet; auto-enabled when the binary
      supports `--test-listenfd-support`
- [x] **Screencopy (expedited → M2)** — wlr-screencopy v3 (niri port) and
      ext-image-copy-capture-v1 (smithay handlers) both landed; verified
      with grim (shm copy) and wf-recorder (damage-queued frames at
      content rate). wf-recorder also forced linux-dmabuf v4 (default
      feedback) on the winit backend — it hard-binds v4
- [x] **Own xdg-desktop-portal backend (expedited → M2)** — landed:
      `crates/xdg-desktop-portal-tomoe`, ScreenCast over PipeWire built
      like ShojiWM's (DRIVER flag, vblank-paced), sidestepping the xdpw
      30fps bug (`ref/ShojiWM/knowledges/screencast-30fps-xdpw-bug.md`).
      Existing wlr portals remain usable as fallbacks alongside it
      (`tomoe-portals.conf` routes only ScreenCast to us)
- [ ] Foreign-toplevel for bars/docks — ext-foreign-toplevel-list landed
      (with portal window capture); wlr-foreign-toplevel-management (the
      activate/close/minimize control surface bars actually use) still open
- [ ] Gamma control / night light
- [ ] text-input + input-method (IME) — after core parity
- [ ] Touch + tablet-v2 — deferred, no hardware pressure yet
- [ ] Virtual keyboard — deferred

### moonshell-driven (companion shell; see DESIGN.md "Companion project")

moonshell (`~/Dev/moonshell`, its PLAN.md "Interconnection tracker" is
the mirror of this list) is the first external `tomoe-ipc` consumer.
Items it pulls on:

- [ ] Workspace/window event vocabulary sufficient for a bar (workspace
      list + active, focused window title/app_id) — vocabulary only, no
      wire change: `wm.lua` broadcasts via `tomoe.ipc.broadcast`, core
      events (`focus_change`, `window_open/close`) already carry the
      rest. Design lands with moonshell M3
- [ ] Window control surface for a taskbar (activate/close/minimize):
      either wlr-foreign-toplevel-management (M5 §1) or equivalent
      `tomoe-ipc` methods — decide when moonshell's taskbar widget
      needs it; xdg-activation (M5 §2) is a prerequisite for activate
- [ ] Default moonshell bar config shipped as content alongside
      `wm.lua`, once moonshell M2 (Lua runtime) lands
- [ ] Combined home-manager module composing both flakes (post
      moonshell M3)

## Milestones (ordered)

### M1 — Finish Phase 3: a WM you can actually use daily

The stubs that break real apps, plus the input plumbing Lua policy needs.

1. ~~Popup grabs~~ done (highest impact: context menus)
2. ~~Fullscreen/maximize/minimize requests~~ done — `on_window_request`
   Lua events with a sane native default (fullscreen honored on the
   window's output); activate requests deferred to xdg-activation (M5)
3. ~~xdg move/resize grab forwarding~~ done — `on_window_request`
   "move"/"resize" events hand the drag to Lua's pointer-grab machinery
   (dogfooded by zoomer's CSD titlebar/edge drags)
4. ~~Focus-follows-mouse setting + `on_pointer_enter/leave` window
   events~~ done — hover diffing in `pointer_moved`; FFM is sloppy focus
   without restacking, hover state holds still during grabs/drags
5. ~~xkb config + libinput config~~ done — `settings.keyboard` (xkb +
   repeat, seat-side so it works on both backends) and
   `settings.touchpad`/`mouse`/`devices` per-device tables (tty backend
   tracks live libinput devices, re-applies on settings change and on
   hotplug/VT re-add)
6. ~~Primary selection; pointer-constraints + relative-pointer~~ done —
   primary selection piggybacks on keyboard-focus changes; constraints are
   enforced in the relative-motion path (lock = relative-only, confine =
   clamp at the edge), activation on motion/creation, cursor position hints
   honored
7. ~~presentation-time~~ done — feedback is the DRM frame's user data,
   presented from the vblank (HwClock when the kernel stamps it); verified
   against weston-presentation-shm on winit

*Accept: daily-drive with real apps — menus, dialogs, fullscreen video,
CSD titlebar drags — all behave; keyboard layout and touchpad configured
from Lua.*

### M2 — Daily-drive blockers: XWayland + screensharing (expedited)

Pulled forward from the old M4: with M1 done, these two are the only
things left between tomoe and full-time use. Session hardening and the
extension surface can wait; a compositor you can't share a screen from
or run X11 apps on cannot be daily driven.

1. ~~XWayland via xwayland-satellite~~ done — sockets/lock owned by
   tomoe, `DISPLAY` exported at startup, on-demand spawn via
   `-listenfd`, spawner thread waits and re-arms the watch on exit.
   What it shook loose: the legacy `wl_drm` global (removed — fatal
   "invalid format" error against Xwayland), verified with xeyes
   rendering through satellite
2. ~~wlr-screencopy~~ done — v3 with shm + dmabuf, immediate `copy` and
   redraw-loop-completed `copy_with_damage` (per-manager queues with own
   damage trackers, sync-fence-deferred `ready`). Verified: grim
   screenshots, wf-recorder records damage-paced frames. Side fix: winit
   backend now offers linux-dmabuf v4 with default feedback (wf-recorder
   binds v4 unconditionally)
3. ~~ext-image-copy-capture-v1~~ done — smithay's image_capture_source +
   image_copy_capture handlers; output sources via WeakOutput user-data,
   shm + dmabuf constraints (render formats of the primary GPU),
   constraint renegotiation and session stop on output changes. Globals
   verified advertised; frame path shares the screencopy render helpers.
   No cursor sessions yet (clients get embedded cursors via
   `paint_cursors`; separate cursor streams can come with the portal)
4. ~~Our own xdg-desktop-portal backend~~ done —
   `xdg-desktop-portal-tomoe` (zbus + pipewire, monitor sources): the
   pipeline is the ShojiWM port (DRIVER + ALLOC_BUFFERS, wayland fd on
   the PW loop, screencopy `ready` → `queue_buffer` → next capture, so
   pacing follows vblank, not the audio quantum); GBM dmabuf buffers
   with memfd/shm fallback; discovery ships in the nix package
   (`tomoe.portal`, `tomoe-portals.conf`, D-Bus activation) and the
   compositor now exports `XDG_CURRENT_DESKTOP=tomoe`. Verified: builds
   through the flake, claims the bus, exports impl.portal.ScreenCast

*Accept: X11 app runs under satellite ✓; grim screenshots ✓; OBS/browser
screenshare captures at monitor refresh via our portal (built + bus
verified; end-to-end OBS fps check pending a real tomoe session), and
at least one third-party portal backend also works end-to-end (pending a
real-session xdg-desktop-portal-wlr run — the protocols it rides are in).*

### M3 — Session hardening (Phase 2 leftovers)

1. ~~Output hotplug end-to-end~~ done — udev → connector →
   `outputs_changed` → Lua reconfigure (ShojiWM's docked-monitor pattern
   is writable now)
2. ~~Per-output position/mirror/disable~~ done — `settings.displays`
   gains `position = {x, y}` (physical), `mirror = "<output>"`, and
   `disabled = true`, all live-reloadable; per-output *scale* still
   open (blocked on the DESIGN.md mixed-scale placement policy)
3. ~~ext-session-lock + idle-notify/idle-inhibit~~ done — `lock.rs` state
   machine (WaitingForSurfaces 1s grace → Locking → confirmed from the
   redraw path once every output shows a locked frame; dropping the
   confirmation is the abort path and sends `finished`); the locked scene
   replaces the normal one at all three scene-assembly sites (tty, winit,
   captures — screenshots/screenshares can't leak session content); input
   is gated to the lock surface while locked (VT switching stays live);
   idle timers are smithay's, fed activity from input/VT-resume/unlock and
   inhibited only by visible surfaces. Verified: swaylock lock → unlock,
   SIGKILL'd locker leaves the session locked on the backdrop and a new
   locker replaces it, grim captures only the locked scene, swayidle's
   timeout fires
4. Direct scanout for fullscreen + cursor plane; VRR; tearing control
   (mind `ref/ShojiWM/knowledges/fullscreen-direct-scanout-tearing.md`
   for the NVIDIA/async-flip pitfalls)
   - ~~Direct scanout + cursor plane~~ landed (plane flags + per-surface
     scanout dmabuf feedback in `backend/tty.rs`; overlay planes stay off,
     niri default). Scanout needs the fullscreen buffer opaque *and*
     spanning the output, or a black clear color — tomoe keeps its grey
     `CLEAR_COLOR`, so a translucent fullscreen client won't flip; swap
     the clear color to black under a fullscreen window if that ever
     matters. Live checks pending (drm_info, engage/disengage log)
   - ~~VRR per output~~ landed — `settings.displays[..].vrr` →
     `vrr_supported`/`use_vrr` at `connector_connected` (with the
     disable-anyway workaround for stale VRR_ENABLED) and a live diff in
     `apply_display_settings` (toggle queues a redraw — `use_vrr` is
     pending state applied on the next commit — without re-emitting
     `outputs_changed`, since no geometry changes). Tomoe's
     estimated-vblank timer needs no VRR awareness: it is only the
     no-damage safety net, not a presentation-time predictor like niri's
     frame clock. Live check pending (VRR-capable monitor +
     `drm_info` VRR_ENABLED)
   - ~~Tearing control~~ landed — smithay fork
     `github.com/y0usaf/smithay` branch `tomoe-tearing` (niri's pin
     ff5fa7df + one commit: `DrmSurface::page_flip(.., allow_tearing)`,
     `supports_async_page_flip()`, `DrmCompositor::queue_frame_tearing`
     with driver-rejection → synced-flip retry; rebase the branch when
     bumping the pin). Tomoe side: `wp_tearing_control_v1` global
     (passive hint store in surface data, ShojiWM port), and the tty
     render path tears when `settings.tearing` (or
     `TOMOE_FORCE_TEARING=1`, the testing/X11 escape hatch — satellite
     clients can't send the hint) + a fullscreen window on the output
     wants it + the cursor is hidden/elsewhere + the driver caps say so.
     Tearing frames drop `ALLOW_CURSOR_PLANE_SCANOUT` (async flips may
     only touch the primary plane — EINVAL otherwise), and
     `queue_redraw` promotes tearing surfaces out of
     `WaitingForEstimatedVBlank` straight to `Queued` so presents follow
     the client's commit rate, not the refresh period (§5 of the ShojiWM
     doc — the frame-bunching trap). Live checks pending: `tearing
     engaged` log + present cadence in a real game; per-window rule
     override arrives with M4 window rules

*Accept: laptop lid/dock cycles, lock screen, fullscreen game with direct
scanout confirmed via drm_info; no idle redraw storms.*

### M4 — Phase 4: extension-surface parity with ShojiWM

1. ~~Process API~~ done (`process.rs` + `tomoe.process` in Lua,
   ShojiWM-shape): manifest keyed by id, `once` (`run =
   "once_per_session"|"once_per_config_version"`), `service` (`restart =
   "never"|"on_failure"|"on_exit"`, `reload =
   "keep_if_unchanged"|"always_restart"`), fire-and-forget `spawn`;
   `command` = argv array or shell string (default: the id itself), `cwd`
   (config-relative), `env`. Reconcile runs from `after_lua` when the
   manifest changed; reload force-reconciles (fewer declarations = a diff
   that stops services) and bumps the config generation. Supervision is a
   1 Hz `try_wait` poll (per ShojiWM) — the tick period rate-limits crash
   loops; the timer exists only while children do (no idle wakeups);
   services are killed on compositor exit. ~~Session supervision +
   builtin consumers~~ landed: `tomoe-session.target` ships in the nix
   package (`share/systemd/user/`), started by the builtin
   `tomoe:session-units` `once` entry — one shell so ordering holds
   (target → GTK backend → frontend try-restart), running as a
   supervised manifest child instead of an orphaned background shell;
   exit stops the target and `unset-environment`s the session vars.
   Builtin mechanism: `Tomoe::declare_builtin_process` merges
   compositor-owned decls (reserved `tomoe:` prefix wins over user ids)
   into every reconcile. xwayland-satellite deliberately stays native —
   its fd handoff (`pre_exec` + `-listenfd`) and socket-activated
   respawn don't fit `ProcessSpec`, and growing the manifest for one
   consumer isn't worth it. Live checks pending:
   `graphical-session.target` active after a TTY launch and gone after
   exit; waybar-style service surviving a config reload with
   `keep_if_unchanged`; restart-after-crash cadence
2. ~~IPC socket + `tomoe msg` + event stream + `tomoe.ipc.serve`~~ done —
   doctrine-03 split: `crates/tomoe-ipc` holds the wire contract only
   (newline-delimited JSON `{id?, method, params?}` / `{id, result|error}`
   / `{event, payload}`, `WIRE_VERSION`, socket discovery, blocking
   client), the event *vocabulary* grows freely in the compositor/config.
   Server (`ipc.rs`) hosts each connection as its own calloop source
   (non-blocking reads, buffered writes, 1 MiB slow-reader drop à la
   niri). Builtins: `version`, `windows`, `outputs`, `view`, `subscribe`
   (opt-in event stream, optional `{events = [...]}` filter), `quit`;
   every other method dispatches to `tomoe.ipc.serve` handlers, which run
   as normal Lua entries (snapshot before, `after_lua` after; errors go
   back to the caller). `tomoe.ipc.broadcast` queues events (payload →
   JSON at call time), drained in `after_lua`. Core events: `window_open`
   / `window_close` / `focus_change` (deduped) / `outputs_changed`.
   Socket `$XDG_RUNTIME_DIR/tomoe.<display>.sock`, exported as
   `$TOMOE_SOCKET` (also into the systemd/D-Bus activation env for bars).
   Verified live on winit: all builtins, Lua echo endpoint, subscribe
   stream (broadcast + window open/close/focus on a real foot window),
   error paths, `quit` + socket unlink
3. ~~Hot reload with `on_reload` persist/restore~~ done —
   `tomoe.on_reload(name, save, restore)`: save hooks run in the outgoing
   VM (only after the new config loaded, so a broken config never
   disturbs the running one) and return JSON-compatible values — the only
   representation that outlives the VM; restore hooks run in the fresh VM
   with their key's value after load. The `on_window_open` replay remains
   as the fallback for configs that don't persist (restored + replayed
   would double-track windows, so it's either/or). `tomoe.window(id)`
   looks persisted ids back up. Verified live: window moved to workspace
   2 stays there across a reload (IPC-driven wm state dump)
4. Window rules
5. LuaLS meta files; example configs exercising all of the above
6. `tomoe.ui` registry — retained widgets (menu/confirm/toast) + modal
   input routing generalized from `screenshot_ui`; port `hotkey_overlay`
   and `exit_confirm_dialog` onto it as builtins per doctrine 05 ("when
   the third arrives, build it — and port the first two");
   `screenshot_ui` stays native as a declared exemption until the API
   grows drag-region interaction. Independent of the IPC work — can land
   any time, and immediately pays for itself beyond the portal (alt-tab
   switcher, workspace OSD, launcher)
7. Portal source policy over IPC: `tomoe.on_screencast_request` — the
   backend becomes a thin IPC client (asks the compositor on
   SelectSources, bounded by a timeout), the hook gets a snapshot
   (app_id, types, candidate outputs/windows) and answers with an action
   (resolve/deny) or `req:defer()` + later `req:resolve(selection)` for
   interactive picks, so the frame loop never waits on a picker. Default
   policy ships as a builtin `tomoe.ui.menu` picker (doctrine 01);
   per-app behavior composes with window rules
   (`tomoe.rule { app_id = ..., screencast = ... }`). Bare core / no
   hook: the backend keeps today's auto-pick + env-var heuristics
   (doctrine 06), which stops being the *declared* mechanism and becomes
   the fallback. Same hook pattern extends later to Screenshot and
   GlobalShortcuts (which maps 1:1 onto the bind registry)

*Accept: waybar-equivalent driven purely over user IPC; config reload
preserves workspace assignments; services survive and diff correctly;
the screencast picker is compositor-drawn via `tomoe.ui`, declared in
`init.lua`, and `TOMOE_PORTAL_CHOOSER` is no longer needed on a default
setup.*

### M5 — Ecosystem remainder

1. Foreign-toplevel for bars/docks — ext-foreign-toplevel-list done
   (pulled forward for portal window capture); wlr-foreign-toplevel-
   management remains
2. xdg-activation
3. Gamma control / night light

*Accept: taskbar sees windows, activation focuses them, night light
works.*

### M6 — Phase 5: eye-candy

1. Animation engine (springs/beziers) driving layout positions + opacity
2. Borders polish, rounded corners, shadows as shader elements —
   pixel-aligned intermediates per coordinate doctrine §5
3. Dual-kawase blur incl. blur-behind for layer surfaces

*Accept: side-by-side with Hyprland defaults, no visible fidelity gap;
UFO test still flat at high refresh with animations running.*

## Standing lessons from `ref/` (re-read before touching these areas)

- TTY loop wake/maintenance ordering:
  `ref/ShojiWM/knowledges/firefox-wayland-display-maintenance.md`
- Never regenerate cursor buffers per frame (false-damage redraw storm):
  `ref/ShojiWM/knowledges/tty-backend-notes.md`
- Redraw-loop spec: `ref/niri/docs/wiki/Development:-Redraw-Loop.md`
- Effect damage/invalidation policy design:
  `ref/ShojiWM/knowledges/effect-invalidation.md`
- Never bind the legacy wl_drm global (`bind_wl_display`): smithay's
  implementation posts a fatal "invalid format" protocol error at
  Xwayland, killing xwayland-satellite; linux-dmabuf covers every
  current client. Also: offer dmabuf **v4** (default feedback) on every
  backend — wf-recorder and friends hard-bind v4 and die on v3
