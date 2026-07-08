# moonshell — build plan

Working tracker toward the DESIGN.md vision: **QuickShell's
extensibility, a terminal emulator's footprint, waybar's portability**.
DESIGN.md holds doctrine and locked decisions; this file holds the
concrete work list and the order it lands in. Milestones M0–M6 and
their acceptance criteria live in DESIGN.md's roadmap; this file breaks
them into steps and records what each one shakes loose (tomoe-PLAN
style).

## Where we are (2026-07)

**M0 done** (2026-07-08): flake + workspace (`moonshell`/`surface`/
`render` crates), bare binary maps a top-anchored layer surface with
exclusive zone and draws its version string via cosmic-text into
wl_shm. `nix flake check` = build+tests, fmt, clippy(-D warnings), and
the doctrine-06 `boot` check (headless sway + `moonshell --boot-check`
+ RSS gate). Measured: **6.9 MB idle RSS live / 6.3 MB in the sandbox
gate (budget 20 MB); 0 voluntary ctx switches over 5 s idle** (powertop
unavailable — ctx-switch delta is the standing wakeup proxy); output
disable/enable under headless sway survives (unmap → wait → remap).

**M1 done** (2026-07-08): §1 (element vocabulary + flex-lite layout +
draw pass), §2 (icon/image elements + asset cache), §3 (per-element
damage diffing via `Scene`), §4 (simple-bar acceptance fixture) — see
the M1 breakdown below. Measured on the fixture: **14.5 MB RSS
(release) with a full bar tree, 0 voluntary ctx switches over 5 s
idle** — under the 25 MB full-bar budget with room for the Lua VM.
Next open item: **M2 — Lua runtime** (mlua/LuaJIT, `shell.*`/`ui.*`,
hot reload, conventions doc).

Two working inputs exist:

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

1. [x] Flake: `nix flake check` runs build+tests, fmt, clippy
   (-D warnings), and `boot` — headless sway (unwrapped; the wrapper
   needs a dbus session) + `moonshell --boot-check` + a 20 MB RSS gate.
   The sandbox has no fonts; the check writes a `FONTCONFIG_FILE`
   pointing at dejavu so the text path is exercised (fontdb honors it).
2. [x] `surface`: registry/seat/output binds; top-anchored layer
   surface with exclusive zone; `SlotPool` double buffer; `Painter`
   trait is the mechanism/policy boundary (returns `Damage`).
3. [x] `render`: tiny-skia clear/rect + cosmic-text line into ARGB8888
   (R<->B swizzle once in `Rgba::to_skia`); integer-physical sizing;
   fontless systems skip text instead of panicking.
4. [x] Damage: painter-reported rects → `damage_buffer`; frame callback
   requested only when a commit is in flight; fully idle = zero
   scheduled wakeups.
5. [x] Measured 2026-07-08: idle RSS 6.9 MB live (6284 kB in the CI
   gate), 0 voluntary ctx switches over 5 s idle. powertop/smem not
   installed — /proc VmRSS + ctx-switch delta are the standing proxies.

M1 breakdown (element vocabulary; all in `render`, no Lua):

1. [x] Element tree + flex-lite layout + draw pass (2026-07-08):
   `element.rs` (HBox/VBox/Stack/Text/Spacer/Separator/Progress/
   CircularProgress — every variant carries a uniform `Style`; doctrine
   05 shape documented in the module header), `layout.rs` (measure +
   place: flex-grow semantics, gap/padding/justify/align, logical→
   physical scale multiplied exactly once), `draw.rs` (lockstep walk,
   edge-rounding to integer px, rounded rects + bezier arcs in the
   Renderer). Bare binary renders its version bar through the tree —
   the doctrine-06 artifact now exercises the M1 path.
2. [x] `icon` (SVG via resvg) + `image` elements (2026-07-08):
   `assets.rs` `AssetCache` inside `Renderer` — pixmaps cached per
   (source, physical size, tint), misses negative-cached, stored
   premultiplied in buffer byte order so `blit` is a plain src-over.
   Icon contract from nur: `path` > XDG theme lookup (`{name}.svg` in
   hicolor/Adwaita/breeze scalable) > name-as-text fallback; tint
   keeps alpha, replaces color. Image intrinsic size = native file px
   1:1 (crisp), style overrides rescale (bilinear). Deps: resvg 0.47
   (default-features off — matches tiny-skia 0.12), image 0.25
   (png+jpeg only).
3. [x] Per-element damage diffing (2026-07-08): `scene.rs` `Scene`
   caches the previous element+layout trees; diff walks both pairs in
   lockstep (equal subtree → skip; equal container shell + rect →
   recurse; else damage old+new subtree *bounds* — children can
   overflow containers). Rects use the draw pass's edge rounding,
   +1 px inflation (AA/glyph-overhang insurance), canvas clamp,
   overlap coalescing → `SceneDamage::{None,Full,Rects}`. Canvas is
   still repainted in full when damage ≠ None (buffers alternate;
   partial repaint needs per-slot buffer-age tracking — deferred until
   profiling demands it); identical tree + geometry early-outs before
   layout, so steady state does zero shaping work. `surface` grew
   `Canvas.fresh` (no committed content at this buffer size: first
   draw/remap/resize) — painter invalidates its scene, damage upgraded
   to Full.
4. [x] Accept (2026-07-08): `crates/moonshell/examples/simple_bar.rs` —
   nur's simple-bar tree as a static Rust table (mapping notes in the
   file header: `items_center` → `align: Center`, `fill` → `grow: 1.0`,
   `button` → styled hbox shell, theme tokens from nur's Mocha).
   Renders correctly on the live tomoe session across all three
   regions (grim-verified at 5120×1440). **Pixel A/B against
   nur-on-GPUI is blocked on this machine**: nur has no tomoe
   compositor backend, so its render callback dies and the bar maps
   blank — the very gap moonshell M3 exists to fill; parity was
   checked against nur's source-level layout semantics instead
   (`crates/runtime/src/bridge/element.rs`). Revisit pixel A/B under a
   shared compositor if it ever matters. Shook loose: icon
   name-fallback text overflowed its box and overpainted siblings
   (also outside the reported damage bounds) — `text_line` grew a
   `max_w` clip, fallback now fits the box (regression test
   `icon_fallback_text_clips_to_box`). Measured: 14.5 MB RSS release,
   0 voluntary ctx switches / 5 s.

Then M2 brings Lua in. Nothing Lua-shaped gets built in M0/M1 — the
render core must be provably tiny before the runtime lands on top.

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
- From M0: cosmic-text 0.19 `set_text` only marks dirty — call
  `shape_until_scroll` before `layout_runs`/`draw`, or advance is 0 and
  nothing renders (the unit test exists because the live bar shipped
  blank)
- From M0: never remap unconditionally in `LayerShellHandler::closed` —
  a compositor with zero outputs closes the new surface immediately and
  the create/close loop storms (~150k remaps/s observed). Remap only if
  the old surface was ever configured and an output exists; otherwise
  wait for `new_output`
- From M1 §1: round layout rect *edges* (x0/x1), not (x, w) — rounding
  width opens one-pixel seams between adjacent children. Text is
  measured with the same `shape()` the draw pass uses, so layout and
  paint can never disagree about advance width. Layout stays f32;
  integer conversion happens once, in `draw.rs`
- From M1 §2: cached asset pixmaps live pre-swizzled ([B,G,R,A]) and
  premultiplied, so src-over blitting is channel-agnostic — swizzle
  exactly once at decode/rasterize time, mirroring `Rgba::to_skia`.
  resvg minor versions pin tiny-skia minors (0.47 ↔ 0.12); bump them
  together
- From M1 §4: text drawn by elements must stay inside their layout
  rect — the damage diff reports subtree *bounds*, so any overpaint is
  also un-damaged pixels the compositor will trust; clip at the draw
  call (`text_line(max_w)`), don't rely on measure == paint width for
  fallback/degenerate content
- From M1 §4: `pkill -f` patterns in dev scripts must not match the
  invoking shell's own command line (quote a bracketed char:
  `[s]imple_bar`)
- From M1 §3: shm buffers alternate, so a partial-damage frame must
  either fully repaint (current: correctness by determinism —
  over-reported damage is always safe) or track per-slot buffer age
  before clipping the repaint. And a remapped/resized surface has no
  content to diff against — any frame-diff cache needs an invalidation
  signal from the surface layer (`Canvas.fresh`), or the first paint
  after remap reports `None` and the surface never gets a buffer
