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

**M2 in progress**: §1 done (2026-07-08) — `runtime` crate boots a
vendored-LuaJIT VM, loads the ported `ui.*` stdlib from
`lua/moonshell/stdlib.lua`, and parses nur's element-table contract
into `render::Element`. §2 done (2026-07-08) — `surface` is
multi-window (`Shell` + `WindowId` handles). §3 done (2026-07-08) —
config resolution, `shell.window`/`shell.state`, `LuaPainter` render
callbacks; a Lua bar runs live on tomoe at **2.3 MB RSS (release),
0 voluntary ctx switches / 5 s**. §4 done (2026-07-08) — `shell.
interval`/`once` (queued `PendingTimer` → calloop timers, armed only
while one exists), `shell.exec`/`exec_async` (worker thread + calloop
channel), `shell.quit`, `shell.get_window` (named-window registry),
`shell.displays` (snapshot fed from `surface::Shell::displays()`).
Live on tomoe: all APIs verified by a self-checking config; **2.2 MB
RSS idle, 0 voluntary ctx switches / 5 s (no timers); 6.6 MB with a
1 s clock ticking**. §5 done (2026-07-08) — hot reload: one inotify
instance (calloop `Generic` on a dup'd fd) watches the config tree;
`.lua` changes debounce (100 ms) into a full VM swap; `shell.reload()`
takes the same path by flag; `shell.watch_file` rides the same watcher.
Live on tomoe: **7.3 MB RSS idle (release), 0 voluntary ctx
switches / 5 s; 10 successive hot reloads → zero RSS growth**. Next
open item: **M2 §6 — stdlib completion + acceptance** (see the M2
breakdown below).

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

M2 breakdown (the Lua runtime):

1. [x] `runtime` crate: VM + element bridge + `ui.*` stdlib
   (2026-07-08). mlua 0.10, `luajit`+`vendored` (the FFI escape hatch
   is the locked decision; vendored → single binary, no system Lua).
   `Vm` owns the `Lua`; one `Vm` per config lifetime — hot reload (§5)
   drops it wholesale, nothing leaks across reloads. `element.rs`
   `from_table` is the doctrine-05 fourth arm: nur's element-table
   contract → `render::Element`, with per-element default merging
   (`apply_style` over the variant's `Default`), `fill`→`grow` mapping
   (numeric `grow` accepted, wins), hbox/button defaulting
   `align=center` (nur's unconditional `items_center`), `button`
   parsed as its M4 visual shell (handlers ignored), scroll/slider/
   input rejected with milestone-naming errors, unsupported style
   props accepted-and-ignored so nur configs load. `TextDefaults`
   threads window `fg`/`font_size` inheritance through the parse — §3
   fills it from `shell.window` opts, no API break. Stdlib ported
   minus `ui.bar_layout` + the theme-aware `shell.window` wrapper
   (both need `moonshell.theme` and the `shell` global — §6).
2. [x] `surface` multi-window (2026-07-08): `Shell` (the calloop
   dispatch data) owns the connection, globals, one shared recycled
   `SlotPool`, and a `BTreeMap<WindowId, Window>`; per-window
   `LayerOptions` in surface's own vocabulary (`Layer`/`Anchors`/
   `Keyboard`/`Margins` — no SCTK types on the public API), per-window
   painter + configure/scale/damage/frame state. `Shell::connect()`
   returns the shell + its event loop so callers insert their own
   sources (§4 timers, §5 inotify) with `&mut Shell` dispatch data;
   `create_window`/`destroy_window`/`mark_dirty`/`mark_all_dirty`/
   `quit` work at config time and from inside source callbacks alike.
   M0 lessons preserved per window: close-remap gated on
   was-configured + output-exists, `new_output` remaps all unmapped,
   frame callbacks only while a commit is in flight. Boot check =
   every window drew once. `examples/two_bars.rs` is the acceptance
   fixture (two bars, timer destroys one from a callback, then quits —
   the exact §3/§4 access pattern); verified exit-0 under headless
   sway and the live tomoe session; flake `boot` gate still green.
3. [x] `shell.window` + `shell.state` + render callbacks in the
   binary (2026-07-08). Lua never touches `surface::Shell` (no stable
   `&mut` exists while Lua runs): `runtime::api::ShellCtx` is the
   action queue — `shell.window` pushes a `PendingWindow` (parsed
   `LayerOptions` + `Rc<RefCell<WindowShared>>` carrying the render
   key and paint defaults), `state:set`/`handle:render` raise a dirty
   flag; the binary drains both after config exec and once per loop
   pass via the new `Shell::run_with(event_loop, tick)` hook.
   `LuaPainter` (in `runtime`) calls the render fn at paint time,
   parses the table with the window's `TextDefaults` (`fg`/
   `font_size`), wraps it in a full-canvas Stack painting the window
   `bg`, and feeds the shared `Scene`; a failing callback logs and
   paints bare bg (a remapped surface must still get a buffer). One
   `Renderer` is shared `Rc<RefCell<…>>` across all windows — the
   font system is the dominant allocation, one copy not N. Window
   opts speak nur's vocabulary (bar mode `position`/popup mode
   `anchor`+`popup_width`+margins, `layer`, `keyboard`, `exclusive`,
   `bg`/`fg`/`font_size`, `transparent`); divergences: bars stretch
   via anchor (size 0) instead of reading display bounds — multi-
   monitor correct — and unknown `anchor` strings error instead of
   silently becoming top-right. Config resolution: `--config` >
   `$MOONSHELL_CONFIG` (both must exist) > `$XDG_CONFIG_HOME`/
   `~/.config/moonshell/init.lua` (optional) > bare version bar.
   Verified live on tomoe (grim: text, colors, spacer layout correct;
   `--boot-check` exit 0 with a config): **2.3 MB RSS release, 0
   voluntary ctx switches / 5 s.**
4. [x] timers & process (2026-07-08): `shell.interval`/`once` queue
   `PendingTimer`s (WeakLua + registry key — a live timer can't keep a
   dropped VM alive; fire-after-drop asks calloop to remove the
   source, so §5's reload is self-cleaning); the binary's drain arms
   them as calloop `Timer` sources — armed only while one exists (the
   zero-idle-wakeup gate), interval period clamped ≥ 1 ms, callback
   errors logged but keep the timer. `shell.exec` = blocking `sh -c`
   trimmed stdout (nur's contract); `shell.exec_async` = named worker
   thread → calloop channel; the reply is plain Send data (id +
   output), the Lua half lives in `ShellCtx.exec_callbacks` keyed by
   id. `shell.quit` = ctx flag; `run_with` reordered to
   tick→redraw→exit-check→dispatch so a config-time quit can't block
   on an event that never comes. `shell.get_window(name)` = registry
   of `WindowShared` by explicit `name` (unnamed windows aren't
   registered; handle methods beyond `:render` — nur's `close`/`hide`/
   `show`/`toggle` — deferred until the visibility story exists in
   `surface`, tracked for §6/M4). `shell.displays()` reads a snapshot
   the binary refreshes each drain *and* before config exec —
   `Shell::connect` now does two roundtrips so output geometry exists
   by then (`surface` grew `DisplayInfo` + `Shell::displays()`,
   xdg-output logical size preferred, mode/scale fallback). nur's
   `shell.clipboard_*` needs a data-control protocol on layer
   surfaces — deferred to M4, recorded here. Measured live on tomoe:
   2.2 MB RSS idle + 0 voluntary ctx switches / 5 s (no timers),
   6.6 MB with a 1 s clock (glyph-cache growth from digits).
5. [x] hot reload (2026-07-08): `watcher.rs` (binary) owns one inotify
   instance (`inotify` 0.11, default-features off — no tokio/stream),
   inserted as a calloop `Generic` on a *dup'd* fd so source teardown
   order can't invalidate it. Config tree = every non-hidden dir under
   the config file's parent (canonicalized in `resolve_config`), new
   subdirs picked up from CREATE events, dir watches never removed
   (bounded by distinct dirs ever seen — documented tradeoff). A
   `.lua` change debounces (100 ms one-shot timer, throttle-style)
   into `ctx.request_reload()`; the tick's reload destroys Lua-created
   windows (tracked in `Engine.windows` — the old drain closure became
   `Engine`, which owns the `Vm` so it can swap it), clears file
   watches, `ctx.reset_for_reload()` (pending/timers/watches/named/
   exec-callbacks/dirty; the exec channel, displays, quit survive —
   they belong to the loop), then fresh VM + re-exec. *Boot*-time
   config errors still fail hard; *reload*-time errors log and leave
   the shell windowless with the watcher alive — the next save
   retries (verified live: break → alive + error logged → fix →
   recovers). `shell.reload()` raises the same flag; `shell.
   watch_file(path, fn)` queues a `PendingWatch` (WeakLua callback,
   timer discipline) that the drain registers — the watcher watches
   the *parent dir* and matches by canonicalized full path, so the
   editor rename-replace dance can't kill it. inotify init failure
   degrades to manual `shell.reload()` with a warning. Measured live
   on tomoe (release): 7.3 MB RSS idle, 0 voluntary ctx switches /
   5 s, 10 successive reloads with zero RSS growth.
6. [ ] stdlib completion + acceptance: port `theme.lua`/`utils.lua` +
   widget modules' `package.preload` registration; land `ui.bar_layout`
   and the theme-aware `shell.window` wrapper; Lua conventions doc in
   `~/Dev/design/`. **Accept-criterion conflict to resolve**: nur's
   `simple-bar/init.lua` reads `shell.services.*` (M3) — §6 ships
   placeholder service facades (static state, `:get()`/`:subscribe()`
   shaped) so the config runs unmodified; real backends replace them
   in M3. Measure RSS (< 25 MB) + idle wakeups here.

Nothing Lua-shaped was built in M0/M1 — the render core was provably
tiny before the runtime landed on top.

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
- From M2 §1: mlua's vendored LuaJIT (`luajit-src`) shells out to
  `make`/`cc` at build time — builds must run inside the devshell/
  sandbox (stdenv provides both); a bare `cargo build` on the host
  fails in `luajit-src` with a bare `NotFound`
- From M2 §1: the bridge speaks *nur's* contract even where render
  defaults differ (separator defaults horizontal in the bridge,
  vertical in `render`; progress `bg` is the *track*, pulled back out
  of the parsed style) — compat lives in one layer, `render` keeps its
  own sensible defaults
- From M2 §1: Lua color strings (`"#rrggbb"`) inside Rust raw strings
  need `r##"..."##` — `"#` terminates a plain `r#"..."#` literal
  mid-string
- From M2 §3: mlua 0.10's `Lua` is `Clone` + `'static` — painters (and
  anything living *outside* the VM) may hold `Lua` clones, but
  callbacks stored *inside* the VM (state subscribers) must go through
  `WeakLua` + `LuaRegistryKey`, or the strong cycle keeps the VM alive
  across hot reload
- From M2 §4: `WeakLua` (and the Lua half of any callback) is `!Send`
  — whatever crosses a thread boundary must be plain data; ship an id
  + payload over the channel and keep the WeakLua/registry-key half in
  a loop-thread map keyed by that id
- From M2 §4: anything Lua can request at config time must be
  satisfiable before the event loop runs — `shell.quit` needed the
  loop reordered (tick before dispatch), `shell.displays` needed
  roundtrips inside `Shell::connect`; audit new `shell.*` calls for
  the "top-level call, no loop yet" case
- From M2 §5: never inotify-watch a file directly — editors save by
  rename-replace and the watch dies with the old inode; watch the
  parent dir and match events by canonicalized full path
- From M2 §5: every calloop source sharing loop state via
  `Rc<RefCell<Engine>>` is safe only because Lua callbacks touch
  `ShellCtx` (a separate Rc), never `Engine` — a `shell.*` function
  that reached back into `Engine` would re-borrow and panic; keep the
  action-queue indirection when adding API
- From M1 §3: shm buffers alternate, so a partial-damage frame must
  either fully repaint (current: correctness by determinism —
  over-reported damage is always safe) or track per-slot buffer age
  before clipping the repaint. And a remapped/resized surface has no
  content to diff against — any frame-diff cache needs an invalidation
  signal from the surface layer (`Canvas.fresh`), or the first paint
  after remap reports `None` and the surface never gets a buffer
