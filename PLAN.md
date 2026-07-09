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
  `on_window_open` replay is the fallback when no restore ran), window
  rules (`tomoe.rule` matcher registry + `tomoe.rules_for` merge +
  apply-fns at window open), dispatch watchdog (doctrine 02: every Lua
  entry — hooks, binds, IPC handlers, config load — is time-boxed by
  `settings.watchdog_ms`, default 1000; a count debug hook checks a
  wall-clock deadline and aborts the entry with a normal Lua error.
  LuaJIT lesson: compiled traces never check hooks — a bare
  `while true do end` escaped the hook entirely — so installing the
  watchdog also forces `jit.off(); jit.flush()` and the VM runs
  interpreted while it's enabled; `watchdog_ms = 0` opts out and
  restores full JIT
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
- Compositor UI: `tomoe.ui` retained-widget registry (doctrine 05) —
  `confirm`/`menu`/`toast`/`sheet` widgets, core-rendered and
  core-routed (modal keyboard routing, sheet dismissal, toast expiry);
  only selection events re-enter Lua. Hotkey overlay, exit confirm, and
  config-error banner are builtins on the same registry; screenshot UI
  stays native (declared exemption until the API grows drag regions);
  xcursor themes; no-hooks full-screen fallback
- Session lock (ext-session-lock-v1, swaylock incl. crashed-locker
  fallback) and idle (ext-idle-notify + zwp-idle-inhibit, swayidle) —
  `lock.rs` + idle plumbing in `state.rs`
- xdg-activation: token validation + activate/urgent `on_window_request`
  events, native default focus, pre-map token stash (`handlers.rs`)
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
- Foreign toplevels (`foreign_toplevel.rs`): ext-foreign-toplevel-list-v1
  (handles published on window map, title/app_id pushed on commit,
  `closed` on unmap — feeds bars and the portal's window enumeration) +
  wlr-foreign-toplevel-management-v3 (the taskbar control surface:
  states/outputs diff-refreshed once per loop iteration, control
  requests routed through `on_window_request` policy — new "close"/
  "unminimize" kinds — with xdg-matching native defaults)
- ScreenCast portal: `xdg-desktop-portal-tomoe` binary (zbus + pipewire,
  ShojiWM-shape: `PW_STREAM_FLAG_DRIVER | ALLOC_BUFFERS`, single thread,
  wlr-screencopy `ready` queues the PW buffer and kicks the next capture
  → vblank-paced); monitor sources (dmabuf with shm fallback) *and
  window sources* (ext-foreign-toplevel-list enumeration +
  ext-image-copy-capture streaming, shm, resize renegotiation via
  `update_params`); source choice asks the compositor first
  (`screencast_select` over IPC → `tomoe.on_screencast_request`, M4 §7),
  with the env-var heuristics (`TOMOE_SCREENCAST_OUTPUT` /
  `TOMOE_SCREENCAST_WINDOW` / `TOMOE_PORTAL_CHOOSER` dmenu lines /
  single-source auto) demoted to the no-hook fallback — the former
  doctrine-05 exemption is retired; compositor exports
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
- [x] xdg-activation (niri-shape validation: serial-less tokens are
      urgency-only, serials checked against keyboard/pointer last-enter,
      10 s timeout + prune timer; requests route through
      `on_window_request` as "activate"/"urgent" so Lua policy decides —
      wm.lua consumes "activate" by switching to the window's workspace
      — native default focuses ("activate") or ignores ("urgent");
      tokens presented pre-map are stashed and honored at `add_window`)
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
- [x] Window rules (`tomoe.rule { app_id = ..., ... }`) — doctrine split:
      the core owns the matcher registry (app_id/title as Lua patterns,
      `match` predicate; all given must match) and runs `apply` functions
      when a matching window opens (after on_window_open hooks, so they
      refine WM placement — also in the native fallback and the reload
      replay); what data props *mean* stays policy — `tomoe.rules_for(win)`
      merges them (later rules win) and wm.lua honors `workspace = n`,
      `fullscreen = true`, `focus = false`. Per-window *core* props
      (tearing override, border colors) still need a queued-op surface —
      revisit with M6 eye-candy
- [x] Animation engine core (springs + beziers, niri's `animation` module
      with the clock stripped to explicit `now`): `animation.rs` —
      `Config`/`Animation` (spring | easing incl. cubic-bezier) +
      window-keyed `Animations` state on `Tomoe`. Doctrine split: Lua sets
      *target* geometry, the core animates the **rendered**
      position/alpha toward it — layout, input hit-testing, and the Lua
      snapshot always see the target. Wired: window_move (set_geometry
      delta → offset decays to 0, retargets compose so windows never
      jump) and window_open (alpha fade on map/show — show gives
      workspace switch-in fades for free). `settings.animations`
      (`false`, or per-property `{ spring = {damping_ratio, stiffness,
      epsilon} }` / `{ ease = {duration_ms, curve} }`). Rendering:
      offsets are integer-physical per frame (coordinate doctrine),
      alpha rides the element (smithay damage-tracks alpha changes);
      captures sample the same animated scene. Keepalive: backends call
      `animations.advance` per frame and re-queue while running (tty:
      `queue_redraw` → vblank-paced; winit: request_redraw). Verified
      nested: 400ms linear fade → grim mid-capture at expected ≈75%
      brightness, settles at 100%. Remaining slices: close animations
      (needs buffer retention past unmap), resize animation (shader,
      niri-style), workspace-switch slide (Lua-visible mechanism TBD),
      velocity-preserving retargets (`initial_velocity` is plumbed but
      always 0), TTY live check
- [x] Rounded corners (`settings.border.radius`, physical px) — niri-shape
      shader infra in `render/`: `shaders.rs` compiles per Gles context
      (winit init + TTY primary-GPU bring-up), `clipped_surface.rs` wraps
      the toplevel surface tree in a custom tex program (no offscreen —
      doctrine §5; popups never clip), `tomoe_render_elements!` macro
      replaces smithay's (RenderElement for the two concrete renderers,
      since draw needs the GlesFrame), per-window `ExtraDamage` bumps on
      radius change (uniforms are invisible to damage tracking).
      Fullscreen windows never round (keeps direct scanout — clipped
      elements return no underlying storage). Verified nested: all four
      corners AA'd, grim/screencopy path included. TTY live check pending.
      Follow-up: borders stay square — rounded borders need the border
      shader (with shadows); per-window radius needs the core-props op
      surface (below)
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
- [x] LuaLS `---@meta` annotation files shipped for editor DX —
      `resources/meta/tomoe.lua` covers the whole core API; parity +
      golden tests in `docgen.rs` hold it (and `docs/lua-api.md`) to the
      registered surface (`TOMOE_REGEN_DOCS=1` regenerates)
- [ ] Layer-surface events (`on_layer_create/update/destroy`) and
      reserved-insets query (usable_area exists; insets breakdown doesn't)
- [x] `tomoe.ui` — retained-widget overlay API (menu/confirm/toast/
      sheet): Lua declares the widget once (`ui/widgets.rs` registry),
      the core renders and damages it; only selection events re-enter
      Lua (as `Action::UiEvent` through `do_action`). Modal input
      routing generalized in `input.rs` (topmost modal owns the
      keyboard and swallows clicks; sheets dismiss on any input; toasts
      expire via a scheduled repaint). Exit dialog, hotkey overlay, and
      config-error banner ported as builtins (doctrine 05); screenshot
      UI stays native as the declared exemption
- [x] Portal source policy through the extension API:
      `tomoe.on_screencast_request` (snapshot in: `req.app_id`,
      `req.types`, `req.outputs`, `req.windows`; actions out: return a
      selection/false, or `req:defer()` + `req:resolve/deny` later) with
      the backend as a thin IPC client (`screencast_select`, 120 s
      timeout) — retires the `TOMOE_PORTAL_CHOOSER` env-var exemption;
      default `tomoe.ui.menu` picker ships as the preloaded `screencast`
      module, composing with `tomoe.rule { app_id = ..., screencast = ... }`
- [ ] Relative geometry/unit algebra across the Lua policy surface: accept
      `%` alongside exact physical `px` (and evaluate whether a logical
      `dp` unit is useful), with explicit reference boxes (`output`,
      `usable`, `parent`, `view`), anchors/alignment, aspect ratios,
      `contain`/`cover`/`stretch`, and min/max/clamp constraints. Apply the
      same vocabulary to window placement/resizing, tiling splits, gaps,
      margins, compositor UI, and later decorations; keep protocol,
      renderer, damage, capture, and final `Window:set_geometry` boundaries
      in deterministic integer physical pixels. Store relative layout
      intent so output hotplug/mode/scale/usable-area changes re-resolve it;
      define rounding/remainder ownership so adjacent tiles never gap or
      overlap. This is broader than niri's proportional columns and should
      follow ShojiWM's parent-relative/flex semantics without giving up
      Tomoe's physical-first coordinate doctrine.
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
- [x] Foreign-toplevel for bars/docks — ext-foreign-toplevel-list (with
      portal window capture) + wlr-foreign-toplevel-management v3
      (`protocols/wlr_foreign_toplevel.rs`, niri-shape but id-keyed and
      diff-based); every control request rides `on_window_request`
- [x] Gamma control / night light — wlr-gamma-control-unstable-v1
      (`protocols/gamma_control.rs`, niri's implementation verbatim in
      shape): one active control per output, every error path fails the
      resource *and* resets the LUT so a crashed daemon never leaves the
      screen tinted. LUT programming in the tty backend: atomic GAMMA_LUT
      blobs (`GammaProps`, previous blob kept for VT-switch restore) with
      the legacy gamma ioctl as fallback; gamma reset at connector
      connect, `pending_gamma_change` stashed while the session is
      inactive and applied/restored on `ActivateSession`; disconnect
      sends `failed`. Winit reports no gamma support. Live check pending
      (wlsunset/gammastep on a real session)
- [ ] text-input + input-method (IME) — after core parity
- [ ] Touch + tablet-v2 — deferred, no hardware pressure yet
- [ ] Virtual keyboard — deferred

### moonshell-driven (companion shell; see DESIGN.md "Companion project")

moonshell (`~/Dev/moonshell`, its PLAN.md "Interconnection tracker" is
the mirror of this list) is the first external `tomoe-ipc` consumer.
Items it pulls on:

- [x] Workspace/window event vocabulary sufficient for a bar — landed
      2026-07-08 with moonshell M3 §1, vocabulary only, zero wire
      change (doctrine 03's wire/vocabulary split, first real test):
      `wm.lua` serves **`wm_state`** (a bar's initial fetch) and
      broadcasts it after every workspace mutation (switch,
      move_focused, open/close, reload-restore). Payload:
      `{ active = n, workspaces = { { id, windows = count }, … } }` —
      all workspaces reported, display policy stays in the bar.
      Focused title rides core events (`windows` + `window_open/close`
      + `focus_change`). Verified live against moonshell's tomoe
      backend (nested winit instance). Known gap: no core event for
      *title changes* after open — a bar's focused-window title can go
      stale; add a `window_props_changed`-style core event when it
      itches
- [x] Window control surface for a taskbar (activate/close/minimize):
      landed as wlr-foreign-toplevel-management (M5 §1); equivalent
      `tomoe-ipc` methods can still come later if moonshell prefers the
      socket over the protocol
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
     engaged` log + present cadence in a real game; per-window tearing
     override still open — M4 rules carry WM-level data only, a core
     window-prop op is future work

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
4. ~~Window rules~~ done — `tomoe.rule` / `tomoe.rules_for` (mechanics in
   the Hyprland gap list above); the `screencast = ...` rule composition
   arrives with §7
5. ~~LuaLS meta files + example configs~~ done — parity-tested
   `resources/meta/tomoe.lua`; `resources/examples/` ships runnable
   configs (`tomoe --config <file>`): `extension-surface-init.lua`
   exercises the whole M4 surface (rules, process manifest, IPC
   endpoints + broadcasts via wrapped wm entry points, a scratchpad
   persisted through `on_reload`, `tomoe.ui` power menu, custom
   deferred-menu screencast policy) and `zoomer-init.lua` runs the
   canvas WM. Both are load-tested headless (`lua.rs`
   `example_configs_load` — loading is side-effect free by
   construction: spawns live in binds, manifest/ui/ipc registrations
   only queue), and the test asserts the extension-surface example
   actually registers rules/hooks/manifest/IPC/screencast/reload
   state, so the examples can't rot against the API
6. ~~`tomoe.ui` registry~~ done — retained widgets (`confirm`, `menu`,
   `toast`, `sheet`) on `ui/widgets.rs`, IDs session-unique so builtins
   survive reloads. Contract: constructors return a handle (`:close()`),
   callbacks live in the VM keyed by widget id, widgets close silently
   on config reload (Lua-owned only) and session lock; menu selection
   passes (1-based index, item). Builtins ported per doctrine 05: exit
   dialog = Confirm + `Action::ConfirmQuit` handler, hotkey overlay =
   Sheet built from binds at open (re-press toggles), config-error
   banner = urgent Toast; `screenshot_ui` stays native as the declared
   exemption until the API grows drag-region interaction. Menu pointer
   interaction landed: hovering a row moves the selection (menu geometry
   is cached separately from the pixel cache — selection changes recolor
   rows, never move them — so hit tests don't wait on a repaint), a left
   click on a row selects, elsewhere on the menu is swallowed, outside
   cancels; confirms stay keyboard-deliberate (Enter is the only way to
   confirm). Hit-testing rebases the pointer per output (widgets render
   centered on every output). Verified live on winit (toast/menu/
   confirm/sheet via IPC, handle close, builtin toggle); menu selection
   end-to-end (Enter or row click → on_select → broadcast) still needs
   a hands-on check — hit math is unit-tested, the routing untested by
   real input
7. ~~Portal source policy over IPC~~ done — `tomoe.on_screencast_request`
   (single slot, last registration wins): the portal's SelectSources
   sends `screencast_select` `{app_id, types}` over IPC (thin client,
   120 s read timeout) and the hook answers by returning
   `{ output = name }` / `{ window = win }` / `false`, or `req:defer()`
   + `req:resolve/deny` from a later Lua entry (menu callback) — replies
   ride a queued-op drain (`ipc::flush_screencast_replies` from
   `after_lua`), so the frame loop never waits. Window picks resolve to
   the foreign-toplevel identifier at flush time (gone window ⇒ deny);
   a config reload answers pending requests with `fallback` (resolvers
   died with the VM); disconnecting clients drop their pending entries.
   Default policy ships as the preloaded `screencast` module (required
   by the default init.lua): rules (`screencast = false | "DP-1"` on a
   window of the requesting app) → single candidate auto-resolve →
   deferred `tomoe.ui.menu`. No hook → `{action = "fallback"}` and the
   backend keeps the env-var heuristics (doctrine 06). Verified live on
   winit: hook resolve + post-reload fallback over `tomoe msg`; unit
   tests cover deny/defer/double-answer and the module's three tiers.
   **Pending live check**: real-session OBS pick through the portal
   (menu → PipeWire stream). Same hook pattern extends later to
   Screenshot and GlobalShortcuts (which maps 1:1 onto the bind
   registry)

*Accept: waybar-equivalent driven purely over user IPC; config reload
preserves workspace assignments; services survive and diff correctly;
the screencast picker is compositor-drawn via `tomoe.ui`, declared in
`init.lua`, and `TOMOE_PORTAL_CHOOSER` is no longer needed on a default
setup. — All landed.*

### M5 — Ecosystem remainder

1. ~~Foreign-toplevel for bars/docks~~ done —
   wlr-foreign-toplevel-management v3 (`protocols/wlr_foreign_toplevel.rs`,
   niri's module shape adapted from surface-keyed to window-id-keyed):
   the state stores the last-sent snapshot (title/app_id/maximized/
   fullscreen/activated/outputs) per window and a single diff refresh
   runs once per event-loop iteration (`refresh_wlr_foreign_toplevels`,
   `main.rs` loop callback — focus changes, commits, Lua ops, and unmaps
   all converge there; focused window refreshes last so listeners see
   deactivate-before-activate). "Activated" = keyboard focus (waybar/
   sfwbar treat it as *the* focused window, so at most one carries it,
   per niri). Control requests all ride `on_window_request` — activate
   (default: focus, same as xdg-activation), close (new kind; default:
   send_close), set/unset_fullscreen (defaults: the xdg fullscreen_default
   pair), set/unset_maximized (default: ack), set/unset_minimized
   ("minimize"/new "unminimize", no native default). `output_bound` sends
   the enters a late-bound wl_output missed. Verified live on winit:
   lswt lists windows; wlrctl focus moves keyboard focus (IPC-confirmed),
   close closes, minimize no-ops. Note: wlrctl 0.2.2 aborts on *empty*
   state arrays (its `wl_array_copy` trips libwayland≥1.24's sanity check
   on demarshaled arrays: data≠NULL, alloc=0) — not our bug; niri sends
   the same empty arrays and a state-less window is only expressible as
   an empty array
2. ~~xdg-activation~~ done — mechanics in the niri gap list above;
   activate/urgent ride the existing `on_window_request` policy path, so
   wlr-foreign-toplevel-management's activate (§1) can reuse it directly.
   Compositor-side token creation for `tomoe.spawn` (exporting
   `XDG_ACTIVATION_TOKEN` to children, niri-style) is still open — only
   matters once launchers run as compositor spawns. Live check pending:
   a real notification-click focus (needs a daemon + client that redeem
   tokens; global + request path verified nested)
3. ~~Gamma control / night light~~ done — wlr-gamma-control (mechanics in
   the Ecosystem gap list above); night-light policy stays in the daemon
   (wlsunset/gammastep), which can ship as a `tomoe.process.service`
   entry in user config. Live check pending: wlsunset on a tty session

*Accept: taskbar sees windows, activation focuses them, night light
works — all landed; live night-light run pending.*

### M6 — Phase 5: eye-candy

1. ~~Rounded corners~~ done — first slice, built the shader-element
   infrastructure the rest of M6 rides on (details in the Hyprland gap
   list above): `render/` now has per-context shader compilation
   (`shaders.rs`), Gles access through the renderer abstraction
   (`renderer.rs`: `AsGlesRenderer`/`AsGlesFrame` for GlesRenderer +
   TtyRenderer), the concrete-renderer element macro
   (`tomoe_render_elements!`), and damage injection for uniform-driven
   effects (`damage.rs`). TTY live check pending (verified nested on
   winit incl. screencopy)
2. ~~Animation engine (springs/beziers) driving layout positions +
   opacity~~ done — first slice (mechanics in the Hyprland gap list
   above): move offsets + open fades, `settings.animations`, per-frame
   advance/keepalive in both backends and the capture path. Open
   follow-ups (close/resize/workspace-switch animations, spring
   velocity on retarget, TTY live check) tracked in the gap list
3. [in progress] Borders polish, shadows, and remaining core props.
   - [x] Rounded borders: one persistent physical-first solid shader ring per
     mapped window, shared by GLES/TTY/capture; stable IDs and parameter
     damage, animation/camera zoom following, and fullscreen omission for
     direct scanout. Outer radius = window radius + border width.
   - [x] Shadows: persistent physical-first rounded SDF shader elements,
     Hyprland-shape range/color/power falloff, shared by GLES/TTY/capture;
     animation/camera following and fullscreen omission preserve direct scanout.
     Configured by `settings.shadow`; nested + TTY visual checks pending.
   - [x] Per-window radius/tearing/border-color queued-op surface:
     `Window:set_properties { radius, tearing, border = { focused,
     unfocused } }` replaces overrides atomically (empty table clears them),
     applies through the shared GLES/TTY/capture path, and lets rules grant
     hint-less XWayland tearing while retaining fullscreen/hardware gates.
     Overrides reset on config reload before restore/open replay, so removed
     rules cannot leak policy. Exercised by the extension-surface example;
     TTY tearing + visual decoration check pending.
4. Dual-kawase blur incl. blur-behind for layer surfaces (re-read
   `ref/ShojiWM/knowledges/effect-invalidation.md` first)

*Accept: side-by-side with Hyprland defaults, no visible fidelity gap;
UFO test still flat at high refresh with animations running.*

### M7 — Resolution-independent policy geometry

1. Design and document a common Lua geometry value/algebra: `%` and `px`
   units, explicit reference boxes (`output`, `usable`, `parent`, `view`),
   anchors/alignment, aspect-ratio fitting (`contain`/`cover`/`stretch`),
   and min/max/clamp. Specify whether percentages include borders/gaps and
   specify deterministic physical-pixel rounding before implementation.
2. Add resolver primitives and relative layout intent to the Lua/core
   boundary. Re-resolve on output hotplug, mode, per-output scale, exclusive
   zone/usable-area, and parent/view changes; preserve exact integer-pixel
   `Window:set_geometry` as the low-level escape hatch and compositor
   boundary.
3. Dogfood the algebra in `wm.lua` and `zoomer.lua` for placement, resize,
   tiling splits, gaps, and margins; then use the same values for
   `tomoe.ui` and scriptable decorations rather than creating subsystem-
   specific percentage syntaxes.
4. Test ultrawide, 16:9, portrait, mixed-output, and fractional-scale cases,
   including `5120x1440 -> 50% x 50% -> cover/crop 16:9 = 1280x720`, and
   assert tiled rectangles exactly cover their parent without gaps or
   overlap after rounding.

*Accept: one declarative geometry vocabulary makes policy portable across
display shapes and scales, while rendering, protocols, capture, and all
final geometry remain crisp deterministic physical pixels.*

## Standing lessons from `ref/` (re-read before touching these areas)

- TTY loop wake/maintenance ordering:
  `ref/ShojiWM/knowledges/firefox-wayland-display-maintenance.md`
- Never regenerate cursor buffers per frame (false-damage redraw storm):
  `ref/ShojiWM/knowledges/tty-backend-notes.md`
- Redraw-loop spec: `ref/niri/docs/wiki/Development:-Redraw-Loop.md`
- Effect damage/invalidation policy design:
  `ref/ShojiWM/knowledges/effect-invalidation.md`
- LuaJIT debug hooks fire only from the interpreter — compiled traces
  never check them, so any hook-based execution limit silently misses a
  hot loop. Force `jit.off(); jit.flush()` while the hook is installed
  (see `LuaRuntime::watchdog`)
- Never bind the legacy wl_drm global (`bind_wl_display`): smithay's
  implementation posts a fatal "invalid format" protocol error at
  Xwayland, killing xwayland-satellite; linux-dmabuf covers every
  current client. Also: offer dmabuf **v4** (default feedback) on every
  backend — wf-recorder and friends hard-bind v4 and die on v3
