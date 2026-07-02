# takhti — parity plan

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
- Physical-first coordinate doctrine (`coords.rs`), fractional-scale +
  viewporter, dmabuf, layer-shell, xdg-decoration (+ KDE legacy),
  data-device/clipboard, primary selection, xdg-output,
  pointer-constraints + relative-pointer, presentation-time
- Lua runtime: binds (Mod-agnostic), spawn, settings (gaps/scale/border/
  displays/focus-follows-mouse), window handles + queued ops, hooks
  (open/close/focus/outputs/pointer button+axis, hover enter/leave),
  pointer grabs, world camera (`set_view`), hot reload (fresh VM +
  `on_window_open` replay), watchdog
- Dogfood WMs: `wm.lua` (9 workspaces, dwindle, focus cycling),
  `zoomer.lua` (pan/zoom canvas, planes, drag move/resize)
- Compositor UI: hotkey overlay, exit confirm, config-error banner;
  xcursor themes; no-hooks full-screen fallback

## Gap inventory by reference

### vs niri (performance & session correctness)

- [x] presentation-time protocol (feedback rides the DRM frame as user data,
      fired from the vblank with the hardware timestamp; winit approximates
      with the compositor clock at submit)
- [ ] Direct scanout for fullscreen surfaces; hardware cursor plane
      (cursor is composited today — `backend/tty.rs:619`)
- [ ] VRR (adaptive sync) per output
- [ ] Tearing control (`wp_tearing_control_v1`) + async page flips
- [x] Output hotplug (udev → connector scan diff → connect/disconnect,
      reposition, `outputs_changed`)
- [ ] Per-output config: explicit position, mirror, disable, per-output
      scale (single global scale today; placement policy noted in
      DESIGN.md coordinate doctrine)
- [x] pointer-constraints + relative-pointer (games) — niri-shape: lock
      swallows motion, confine clamps at the surface/region edge, both keep
      sending relative motion; smithay deactivates on focus change
- [ ] idle-notify + idle-inhibit
- [ ] ext-session-lock-v1
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
      `takhti.grab_pointer` (the core releases the client's click grab),
      unconsumed drags are dropped
- [x] Focus-follows-mouse (`settings.focus_follows_mouse`, sloppy focus,
      no restack) + `on_pointer_enter/leave` hover events, suppressed
      while any pointer grab is active
- [ ] Window rules (`takhti.rule { app_id = ..., ... }`)
- [ ] Animation engine (springs + beziers on layout positions,
      AnimatedVariable-style; open/close/move/workspace-switch)
- [ ] Rounded corners (shader element, pixel-aligned per doctrine §5)
- [ ] Drop shadows
- [ ] Dual-kawase blur (windows + blur-behind for layer surfaces)
- [ ] Special workspaces / groups — Lua-level per policy split; needs
      mechanism audit (hide/show + per-window state suffice?)

### vs ShojiWM (extension surface)

- [ ] Process API: `takhti.process.once/service/spawn` declarative
      manifest, diffed by id, restart policies
      (`ref/ShojiWM/knowledges/process-api.md`)
- [ ] IPC: JSON socket + `takhti msg` CLI + event stream +
      `takhti.ipc.serve` for user-defined endpoints (bars/launchers)
- [ ] Hot reload with state persistence: `takhti.on_reload` /
      persist-restore so workspaces survive a reload without replay hacks
      (`ref/ShojiWM/packages/config/src/index.tsx` onEnable/onDisable)
- [x] Request events surfaced to Lua: maximize/minimize/fullscreen via
      `takhti.on_window_request` (ShojiWM's `onWindow*Request` family);
      activate requests wait on xdg-activation (M4)
- [ ] Input-device change events (per-device *config* landed with M1 §5;
      the add/remove events + device query surface remain)
- [ ] Output reconfigure API (re-run config on hotplug, query available
      modes)
- [ ] LuaLS `---@meta` annotation files shipped for editor DX
- [ ] Layer-surface events (`on_layer_create/update/destroy`) and
      reserved-insets query (usable_area exists; insets breakdown doesn't)
- [ ] Scriptable decorations (long-term; Hyprland-style built-in borders/
      titlebar first, ShojiWM-style Lua-driven SSD tree later — study
      `ref/ShojiWM/knowledges/shared-edge-tree-plan.md` before designing)

### Ecosystem (all three have it)

- [ ] XWayland via xwayland-satellite
- [ ] Screencopy (wlr-screencopy) + xdg-desktop-portal backend — build
      our own portal like ShojiWM did; avoid the xdpw 30fps bug
      (`ref/ShojiWM/knowledges/screencast-30fps-xdpw-bug.md`)
- [ ] Foreign-toplevel (wlr + ext-foreign-toplevel-list) for bars/docks
- [ ] Gamma control / night light
- [ ] text-input + input-method (IME) — after core parity
- [ ] Touch + tablet-v2 — deferred, no hardware pressure yet
- [ ] Virtual keyboard — deferred

## Milestones (ordered)

### M1 — Finish Phase 3: a WM you can actually use daily

The stubs that break real apps, plus the input plumbing Lua policy needs.

1. ~~Popup grabs~~ done (highest impact: context menus)
2. ~~Fullscreen/maximize/minimize requests~~ done — `on_window_request`
   Lua events with a sane native default (fullscreen honored on the
   window's output); activate requests deferred to xdg-activation (M4)
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

### M2 — Session hardening (Phase 2 leftovers)

1. ~~Output hotplug end-to-end~~ done — udev → connector →
   `outputs_changed` → Lua reconfigure (ShojiWM's docked-monitor pattern
   is writable now)
2. Per-output position/mirror/disable/scale in `settings.displays`
3. ext-session-lock + idle-notify/idle-inhibit (swaylock/swayidle work)
4. Direct scanout for fullscreen + cursor plane; VRR; tearing control
   (mind `ref/ShojiWM/knowledges/fullscreen-direct-scanout-tearing.md`
   for the NVIDIA/async-flip pitfalls)

*Accept: laptop lid/dock cycles, lock screen, fullscreen game with direct
scanout confirmed via drm_info; no idle redraw storms.*

### M3 — Phase 4: extension-surface parity with ShojiWM

1. Process API (once/service/spawn manifest, restart/reload policies)
2. IPC socket + `takhti msg` + event stream + `takhti.ipc.serve`
3. Hot reload with `on_reload` persist/restore (replace the
   window-replay hack)
4. Window rules
5. LuaLS meta files; example configs exercising all of the above

*Accept: waybar-equivalent driven purely over user IPC; config reload
preserves workspace assignments; services survive and diff correctly.*

### M4 — Ecosystem

1. xwayland-satellite integration
2. wlr-screencopy + our own portal (ScreenCast over PipeWire)
3. Foreign-toplevel, xdg-activation, gamma/night-light

*Accept: X11 app runs, OBS captures at monitor refresh, taskbar sees
windows.*

### M5 — Phase 5: eye-candy

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
