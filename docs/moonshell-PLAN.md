# moonshell — build plan

Working tracker toward the DESIGN.md vision: **QuickShell's
extensibility, a terminal emulator's footprint, waybar's portability**.
DESIGN.md holds doctrine and locked decisions; this file holds the
concrete work list and the order it lands in. Milestones M0–M6 and
their acceptance criteria live in DESIGN.md's roadmap; this file breaks
them into steps and records what each one shakes loose (tomoe-PLAN
style).

## Where we are (2026-07)

Repo created. Nothing built. Two working inputs exist:

- **nur** (`~/Dev/nur`) — the reference implementation and current
  daily driver. Its Lua API (`shell.*`, `ui.*`), `lua/` stdlib, widget
  modules, and services logic are inherited; its GPUI backend, cx/
  APP_PTR bridge, and tokio runtime are not. See nur's ARCHITECTURE.md
  for the contract, nur's CLAUDE.md for its pitfalls (most evaporate
  without GPUI).
- **tomoe** (`~/Dev/tomoe`) — the sibling compositor. Its `tomoe-ipc`
  crate (wire contract: ndjson frames, `WIRE_VERSION`, socket
  discovery, blocking client) is the integration point; its IPC server
  and event stream are already live (tomoe PLAN.md M4 §2 done).

## Gap inventory by reference

### vs nur (the port)

- [ ] `shell.window / get_window / state / interval / once / exec /
      quit / clipboard / displays / reload` — API surface (M2)
- [ ] `ui.*` stdlib + `theme.lua` + `utils.lua` ported verbatim (M2)
- [ ] Widgets: clock, battery, workspaces, network, mpris (M3)
- [ ] Services: applications (.desktop scan + inotify), battery,
      audio, network, bluetooth, mpris, notifications daemon,
      power-profiles, sysinfo, system tray (SNI), compositor
      auto-detect (M3) — **re-implemented event-driven** (zbus/sysfs),
      not ported: nur's CLI-polling backends (`wpctl`, `nmcli`,
      `playerctl`, `bluetoothctl`, `powerprofilesctl`) are the memory/
      wakeup cost we're eliminating
- [ ] Compositor backends: Hyprland, niri, Sway (port), **tomoe (new)**
      (M3)
- [ ] nix: home-manager module + `mkBar`-style lib helpers (post-M3,
      composed with tomoe's module)

### vs QuickShell/AGS (the rivalry)

- [ ] Clickable/hoverable elements, sliders, scroll (M4)
- [ ] Popups/tooltips (xdg_popup on layer surfaces) (M4)
- [ ] Text input — cosmic-text editor; IME deferred, recorded in
      DESIGN.md (M5)
- [ ] Virtualized lists (launcher-scale) (M5)
- [ ] Session lock surfaces (ext-session-lock) (M5)
- [ ] Process/Socket/FileView-equivalent io primitives (M6)
- [ ] Generic D-Bus from Lua — exceeds QuickShell's surface (M6)
- [ ] LuaJIT FFI + `ui.canvas` — the over-the-ceiling hatches QML
      doesn't have (M6)
- Conceded, permanently: widget shaders/blur (compositor's job),
  WebEngine-style embedding, Qt component breadth (see DESIGN.md
  Deferred)

### vs the memory goal (the discipline)

- [ ] RSS measured in CI from M0 on (smem in the flake check; fail
      over budget: 20 MB bare, 25 MB full bar, 40 MB hard)
- [ ] Zero idle wakeups: frame callbacks requested only while dirty;
      calloop timers only while a `shell.interval` exists
- [ ] Zero steady-state subprocesses (M3 accept)
- [ ] Glyph/scale cache budget: cosmic-text caches are the dominant
      allocation — measure before optimizing

## Interconnection tracker (mirrored in tomoe PLAN.md)

- [ ] M3: tomoe compositor backend — `$TOMOE_SOCKET` discovery via
      `tomoe-ipc` git dep, `subscribe` stream (`window_open/close`,
      `focus_change`, `outputs_changed`), workspace state from
      `wm.lua`'s `tomoe.ipc.broadcast` events. What the workspace
      vocabulary should be is designed *with* tomoe (its PLAN.md
      "moonshell-driven" section) — first real test of doctrine 03's
      wire/vocabulary split.
- [ ] M2: shared Lua conventions doc in `~/Dev/design/` (settings-table
      shape, `on_*` naming, reload contract) — written when the second
      consumer (us) exists, kept out of both codebases.
- [ ] post-M3: tomoe ships a default moonshell bar config as content;
      combined home-manager module composes both flakes.
- [ ] M3+: taskbar widget rides ext-foreign-toplevel-list-ish data per
      compositor; on tomoe, window control (activate/close) needs
      either wlr-foreign-toplevel-management (tomoe PLAN M5 §1) or
      equivalent `tomoe-ipc` methods — decide there, consume here.

## Milestone order & first steps

M0 → M6 as in DESIGN.md. M0 breakdown (the doctrine-06 spike):

1. Flake: rust toolchain, devshell with Wayland libs; `nix flake check`
   runs fmt + clippy + the bare-boot check
2. `surface`: registry/seat/output/layer-shell bind via SCTK; one
   anchored top surface with exclusive zone; shm pool, double buffer
3. `render`: tiny-skia clear + rect; cosmic-text one-line draw with
   fontconfig-discovered font; integer-physical sizing (tomoe's
   coordinate doctrine applies — buffer scale first, fractional-scale
   via wp-viewporter later)
4. Damage: track dirty rects, `wl_surface.damage_buffer` precisely,
   request frame callbacks only while dirty
5. Measure: smem RSS + powertop wakeups, record numbers here

Then M1 element vocabulary, then M2 brings Lua in. Nothing Lua-shaped
gets built in M0/M1 — the render core must be provably tiny before the
runtime lands on top.

## Standing lessons (imported)

- From tomoe: never regenerate buffers per frame (false-damage redraw
  storms — `ref/ShojiWM/knowledges/tty-backend-notes.md` via tomoe);
  integer-physical pixel discipline (tomoe DESIGN.md coordinate
  doctrine) — CPU rendering makes misalignment *blurry text*, the one
  unforgivable sin in a bar
- From nur: keep Lua-facing functions `LuaResult`, convert to anyhow at
  one boundary; store `LuaRegistryKey` (’static), never `LuaFunction`,
  for callbacks that outlive the stack frame — these survive the GPUI
  removal
- From nur's TODO list: `watch_file`/hot-reload was the most-wanted
  missing feature — it's in M2, not later
