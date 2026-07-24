# FUSION — moonshell merges into tomoe

Work tracker for fusing moonshell (`~/Dev/moonshell`) into tomoe as its
shell subsystem: one repo, one process, one Lua program driving both
window management and shell UI. **moonshell survives as the name of
that subsystem** — the `moonshell-*` crates and the shell builtins
layer — the way `wm` and `zoomer` name policy modules today.
Milestones are ordered; each has accept criteria. Work top-to-bottom,
checking items off as they land (same contract as PLAN.md).

**North star (the whole point, gate every milestone on it):**
everything above mechanism is Lua on a public API. Every bar, OSD,
notification popup, launcher, titlebar, and tab strip ships as an
ordinary Lua module built on the same `ui.*`/`shell.*` surface user
configs get — if a shipped widget or decoration needs Rust the user
can't reach, the API isn't done (tomoe's extension-first rule, applied
to the shell). The only sanctioned native-UI exemption remains the
screenshot UI (already declared in PLAN.md).

## The decision (record before any code moves)

**What:** moonshell stops being a standalone, compositor-agnostic
Wayland client and becomes tomoe's in-process UI engine. Its element
vocabulary (`ui.*`: hbox/vbox/stack/text/icon/image/progress/…),
CPU raster stack (tiny-skia + cosmic-text), and native services
(rustbus D-Bus: UPower/MPRIS/NetworkManager/notifications/SNI tray;
sysfs battery) move into the compositor. Bars, launchers, OSDs, the
notification daemon, and — the motivating capstone — **window
decorations such as tab strips** are compositor-rendered from Lua
element tables, composited atomically with window geometry.

**Why fusion beats the alternatives considered:**
- Tab layouts need compositor-side rendering. A client process drawing
  tab strips always trails window geometry by a frame during drags,
  resizes, animations, and zoomer pans, and dies with the shell
  process. No repo arrangement fixes that; in-process rendering does.
- One Lua VM means the bar reads `wm.workspaces` directly — the
  IPC round-trip, the `wm_state` broadcast vocabulary, and the
  stale-title gap all dissolve. WM policy and shell UI compose in one
  program with one reload contract.
- One process, one flake, one CI, one conventions doc, one LuaLS stub
  tree. GNOME Shell (mutter + shell, one process, JS) is the
  architectural precedent; ours is Lua and leaner.

**What is knowingly given up (recorded, not implied):**
- moonshell's compositor-agnosticism (niri/Hyprland/Sway backends) is
  **discontinued**. External bars (waybar etc.) still work on tomoe via
  layer-shell and `tomoe-ipc` — fusion adds a native shell, it removes
  no protocol.
- moonshell's standalone memory target (25 MB RSS vs AGS/QuickShell)
  is moot in-process; the fused win is *zero additional processes and
  zero IPC wakeups* for the native shell.
- Doctrine 03 as previously applied (moonshell as the thin client
  proving the wire): the native shell is now policy inside the daemon.
  `tomoe-ipc` remains the versioned wire for *external* clients and
  keeps its own discipline; it just loses its captive first consumer.
- moonshell's DESIGN.md "locked decisions" rows "no shared Rust beyond
  tomoe-ipc" and "integration ships as content, not code" are
  **reversed by this document**.

**Doctrine conformance of the fused shell** (goes into tomoe's
DESIGN.md table at F0):
- 01 extension-first: strengthened — every shipped widget/bar/OSD/
  decoration is Lua on the same `ui.*` API user configs get.
- 02 snapshot in, actions out: the shell now *gains* the watchdog and
  snapshot/queue contract it lacked standalone (render callbacks
  return element tables; service state arrives as snapshots).
- 05 one declaration mechanism: strengthened — `ui.*` element tables
  become the single vocabulary for all compositor UI: builtin dialogs,
  bars, OSDs, launcher, decorations. `tomoe.ui`'s bespoke widget enum
  is retired into it.
- 06 bare core boots: unchanged — with no config, no shell surfaces;
  windows still map full-screen.

---

## F0 — repo merge, docs made honest

Mechanical unification. Nothing is fused yet; both binaries build from
one workspace and all design docs tell the truth about what's coming.

- [x] Merge moonshell's git history into tomoe (subtree merge or
      `git merge --allow-unrelated-histories` after path-rewriting into
      `crates/`; history must survive `git log --follow` on moved
      files). moonshell's crates land as workspace members, keeping
      the name: `crates/moonshell-render` (was `render`),
      `crates/moonshell-runtime` (was `runtime`),
      `crates/moonshell-services` (was `services`),
      `crates/moonshell-surface` (was `surface`, kept only until F6),
      `crates/moonshell` (transitional standalone binary, kept only
      until F6; after F6 the name lives on in the subsystem crates and
      the Lua layer). `lua/` merges into `resources/` layout.
- [x] One flake: tomoe's flake builds everything; moonshell's flake
      checks (the M0 `boot` RSS gate included, while the standalone
      binary exists) fold into `nix flake check`. Delete moonshell's
      flake.
- [x] Write `DESIGN.md` for tomoe from
      `~/Dev/doctrines/templates/DESIGN.md` — it is referenced by
      ARCHITECTURE.md and moonshell's docs but never existed. It
      records: the fusion decision (lift "The decision" above),
      the doctrine-conformance table including the shell rows, and the
      divergences listed there.
- [x] Mark `~/Dev/moonshell` superseded: README pointer to tomoe,
      archive the repo. Update moonshell rows in both PLAN.md
      interconnection trackers to "superseded by FUSION.md". Update
      `~/Dev/nur` wind-down note to point here.
- [x] Regenerate ARCHITECTURE.md (`scripts/gen-arch.sh`) for the
      enlarged workspace; `nix flake check` green including
      `arch-fresh`.
      *Accept: one repo; `nix build` produces tomoe, the portal, and
      the transitional moonshell binary; `nix flake check` green; no
      doc anywhere still claims the repos are separate or that
      moonshell is compositor-agnostic.*
      *Done 2026-07-24: filter-repo path rewrite + two-parent merge
      (`git log --follow` traces moved files into moonshell history);
      one workspace (tomoe-ipc now a path dep; moonshell crates keep
      AGPL-3.0-only explicitly), one flake (fmt/clippy/boot folded in —
      the clippy -D warnings gate now covers tomoe code too, made clean
      here); `nix flake check` green incl. the 20 MB boot RSS gate and
      arch-fresh; moonshell repo archived on GitHub with a pointer
      README; nur carries the wind-down note; superseded docs live as
      `docs/moonshell-*.md` with banners.*

## F1 — element engine renders in-process

The shared raster stack becomes a tomoe render element. This is pure
mechanism: no Lua surface change yet.

- [ ] `moonshell-render` decoupled from `moonshell-surface`/SCTK (it should
      already be close: element tree in, tiny-skia pixmap out). Its
      damage diffing reports dirty rects per tree.
- [ ] New tomoe render element: an element-tree texture. CPU raster
      into a pixmap, GLES texture upload of dirty regions only,
      composited like any other element (respects camera transform,
      output scale, damage tracking). Rasterization happens on state
      change, never per-frame; a raster deadline is enforced the same
      way as the Lua watchdog (a slow tree logs and skips the frame's
      update rather than stalling the render loop).
- [ ] Proof-of-fusion: one existing native UI piece (exit dialog or
      hotkey overlay) re-rendered through the element engine, its
      bespoke `ui/widgets` path deleted. cosmic-text replaces the
      hand-rolled `ui/text` Canvas for that piece.
      *Accept: the ported piece is pixel-stable at fractional scales
      (the physical-pixel doctrine holds: element layout resolves to
      integer physical pixels before raster); damage-only redraw
      verified — an animating clock in a corner uploads only its own
      rect.*

## F2 — one Lua program: shell surfaces from the config

The `ui.*` vocabulary lands in tomoe's VM; a config can declare a bar.

- [ ] `ui.*` element constructors and the reactive-state bridge from
      `moonshell-runtime` registered in tomoe's Lua runtime, under the
      existing snapshot/queue/watchdog contract. Namespaces: the
      inherited moonshell contract is kept first-class, not aliased —
      `ui.*` for the element vocabulary and `shell.*` for the shell
      API (`shell.window{}`, `shell.state`, `shell.services.*`),
      alongside the `tomoe` global for compositor policy. The `shell`
      global *is* moonshell; nur-era configs keep porting with at most
      mechanical edits.
- [ ] Compositor-internal shell surfaces: anchored to output edges,
      with exclusive zones that integrate with the same usable-area
      computation layer-shell clients use; layered correctly against
      layer-shell (a native bar and waybar can coexist); per-output;
      surviving output hotplug by re-resolving anchors.
- [ ] Hot reload: shell surfaces participate in the existing config
      reload contract (fresh VM, surfaces re-created, errors surface
      as the on-screen banner).
- [ ] LuaLS stubs + docs/lua-api.md extended; parity tests hold them
      to the runtime (existing `cargo test` gate).
      *Accept: the nur/moonshell simple-bar config runs inside tomoe
      with at most namespace-level edits, live-reloads on save, and
      the bar reads `wm.active` directly from the wm module — zero
      IPC involved.*

## F3 — services in the daemon

moonshell M3's native services attach to tomoe's calloop.

- [ ] `moonshell-services` sources (rustbus D-Bus: UPower, MPRIS,
      NetworkManager, PowerProfiles; sysfs battery) ride tomoe's event
      loop; state published into Lua as snapshots on change
      (`shell.services.*`).
- [ ] Notification daemon (org.freedesktop.Notifications) hosted
      in-process; notification popups are a Lua builtin module on
      `ui.*` (doctrine 01 — replaceable from config).
- [ ] SNI tray host; tray state exposed to Lua (menus consumed in F4).
- [ ] Delete `moonshell-services`' compositor-backend abstraction (tomoe/
      niri/Hyprland/Sway IPC clients) — workspaces are now read
      in-VM; the `wm_state` broadcast stays for external consumers.
      *Accept: a bar with clock/battery/workspaces/network/mpris
      live-updates with zero subprocess spawns in steady state
      (execsnoop-clean for 5 min) and zero idle wakeups beyond timers;
      `notify-send` pops a Lua-rendered notification.*

## F4 — interactive compositor UI

Input routing through element trees, unified with tomoe's existing
input dispatch.

- [ ] Pointer routing into shell surfaces and element trees: hit-test,
      hover state, `on_click`, scroll; damage on hover transitions.
      One input path shared by shell surfaces and (F5) decorations.
- [ ] `ui.button`, sliders (volume slider gate), tooltips and menus
      via the element engine (tomoe's existing menu widget retires
      into it — doctrine 05).
- [ ] Keyboard focus policy for shell surfaces (launcher-style
      exclusive keyboard, à la layer-shell keyboard interactivity,
      without stealing from the lock screen path).
      *Accept: volume slider drags; tray icon opens its menu; hover
      tooltips appear and damage correctly.*

## F5 — scriptable decorations and tab layouts (the capstone)

The feature this fusion exists for. Study
`ref/ShojiWM/knowledges/shared-edge-tree-plan.md` before designing
(existing PLAN.md note).

- [ ] Decoration slots: wm policy attaches an element tree to a window
      or window group (`win:set_decoration{ edge = "top", height = h,
      tree = function(snapshot) return ui.hbox{...} end }` — exact API
      designed at implementation). The core rasterizes it and
      composites it **atomically with the window's geometry**: moves,
      interactive resizes, animations, and zoomer pan/zoom never let
      the strip detach. Decoration size participates in the client's
      configure (tile height minus strip).
- [ ] Input on decorations routes to Lua hooks with element-local
      coordinates (click → focus tab, middle-click → close, drag →
      the existing interactive-move hook path).
- [ ] `tabs` Lua module shipped as a builtin: a tiling container mode
      in wm.lua where N windows occupy one slot with a compositor-drawn
      tab strip; only the active window's buffer is presented; strip
      shows titles + urgency; fully restyleable from user config since
      it is ordinary `ui.*` Lua.
- [ ] Baseline `titlebar` module on the same mechanism (the
      Hyprland-style SSD case from PLAN.md), proving the mechanism is
      not tabs-shaped.
      *Accept: a tabbed group survives torture — drag the group,
      resize interactively, switch tabs during a window-open
      animation, zoomer-zoom out and back — with the strip glued at
      every frame; a user config restyles the strip (colors, height,
      close buttons) without Rust changes.*

## F6 — retirement and polish

- [ ] Delete `crates/moonshell` (standalone binary) and
      `moonshell-surface` (SCTK client stack), and every SCTK/wl_shm
      dependency from the workspace. The `moonshell-render/-runtime/
      -services` crates and the `shell.*` API remain — they are the
      product, not transition scaffolding.
- [ ] Fold this tracker's outcomes into PLAN.md; regenerate
      ARCHITECTURE.md; README rewritten to present the fused product:
      tomoe the compositor with moonshell, its built-in Lua shell —
      one process, one config.
- [ ] Home-manager module: the previously planned "combined module
      composing both flakes" becomes one module.
- [ ] moonshell M5/M6 remainders re-homed as ordinary PLAN.md items:
      `ui.input` (cosmic-text editor) + virtualized `ui.list` for a
      launcher; LuaJIT FFI + generic D-Bus proxy + `ui.canvas` escape
      hatches (they compose with tomoe's existing extension surface).
      *Accept: `nix flake check` green; no dead crates; every
      "moonshell" reference in the docs describes the shell subsystem
      as it now exists, none the retired standalone client.*

## Resolved decisions

- **Name:** moonshell survives — it is the name of the shell subsystem
  (`moonshell-*` crates), the shell builtins layer, and the identity
  behind the `shell.*` Lua global. The product is "tomoe with
  moonshell". (Decided 2026-07-24.)
- **Namespaces:** `ui.*` + `shell.*` kept first-class (the inherited
  moonshell contract), `tomoe.*` for compositor policy. (Decided with
  the above.)

## Open decisions (defaults chosen; revisit only if they itch)

- **Raster threading:** raster on the main thread with a deadline
  (default, simplest); move to a worker with texture handoff only if a
  real bar blows the frame budget on real hardware.
