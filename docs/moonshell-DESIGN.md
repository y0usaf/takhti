# moonshell — design & roadmap

> **moonshell** (*lua* is Portuguese for "moon" — this is the Lua shell) —
> a GPU-free, Lua-scriptable Wayland desktop shell: bars, launchers, OSDs,
> notification popups, lock screens, written in `~/.config/moonshell/init.lua`.
> It exists because every incumbent (AGS, QuickShell, EWW) carries a full
> UI toolkit and a heavyweight script engine to draw a 32-pixel bar.
> Successor to **nur** (`~/Dev/nur`): same Lua contract, lean backend.

## Vision

- **As scriptable as QuickShell** — the config is a program; widgets,
  services, and layouts are user-definable, with escape hatches
  (LuaJIT FFI, generic D-Bus, canvas drawing) that exceed QML's sandbox.
- **As light as a terminal emulator** — idle RSS under 25 MB (hard
  ceiling 40 MB with a full bar), zero idle wakeups, no GPU driver in
  the process. CPU rendering into wl_shm; the compositor owns effects.
- **As portable as waybar** — compositor-agnostic: tomoe, niri,
  Hyprland, Sway are all just IPC backends. Being tomoe's sibling must
  never make it tomoe's captive.

nur (`~/Dev/nur`) is the reference implementation for the Lua API,
stdlib, widgets, and services logic; its ARCHITECTURE.md documents the
contract being inherited. QuickShell/AGS are studied for surface
breadth, not code.

## Doctrine conformance

| Doctrine | Status | Notes |
|---|---|---|
| 01 extension-first core | follows | Core exposes surfaces/elements/services primitives; every shipped widget, theme, and service facade is a Lua module in `lua/` on the same public API users get. If clock/battery/workspaces can't be userland, the API isn't done. |
| 02 snapshot in, actions out | diverges (recorded) | Lua is the *application* here, not a guest extension. Render callbacks return plain data (element tables); service state arrives as immutable snapshots pushed into reactive values. No watchdog: a config that hangs, hangs only its own shell — process isolation from the compositor is the containment. |
| 03 daemon + thin client | follows (as the client) | moonshell *is* the thin client of compositor daemons. It consumes `tomoe-ipc` as a versioned git dep, and niri/Hyprland/Sway sockets natively. It owns no state that outlives it beyond user config. |
| 04 declarative front, idempotent executor | n/a | Config is a live program by design. The home-manager module only places files — idempotent by construction, no manifest pipeline. |
| 05 one declaration mechanism | follows | Element types: one enum variant + one `from_table` arm + one draw arm, always. Services: one `start → State entity → publish on shell.services.*` shape. Widgets: one Lua module registry. No hand-wired specials. |
| 06 bare core must boot | follows | `moonshell` with no config maps one layer surface and draws a version string — proving SCTK, shm, render, and text all work with zero policy. CI-checked via the flake (M0). |
| 07 nix source of truth | follows | `nix build` / `nix flake check` are the gate; `cargo fmt`/`clippy` sanctioned exceptions per canon. |

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Rendering | **CPU: tiny-skia into wl_shm buffers** — no Vulkan/GPU in-process | Shell surfaces redraw on state change, not per-frame; CPU is free at that cadence. GPU drivers allocate tens of MB resident per process — the single biggest lever on the memory goal. Effects (blur-behind, shadows) are the compositor's job (tomoe Phase 5). |
| Text | **cosmic-text** | Shaping, bidi, font fallback, emoji — the one problem that punishes hand-rolling. Also provides the editor core for M5 text input. Dominates the memory budget (glyph caches); budgeted, not fought. |
| Scripting | **mlua + LuaJIT, vendored** | Same pick as tomoe: µs-level calls, single binary. LuaJIT FFI is a *feature*, not an accident — users bind C libraries from config at runtime, no recompile. This is the extensibility trump card over QML. |
| Lua contract | **nur's API preserved: `shell.*` + `ui.*` globals, element-table shapes, `lua/` stdlib layout** | moonshell is nur's successor; nur configs must port with at most mechanical edits. The API was always the product — GPUI was an implementation detail. |
| Event loop | **calloop, single thread; no tokio** | Timers, sockets, D-Bus, inotify are all calloop sources. No runtime threads idling in the RSS. Matches tomoe's loop discipline. |
| Wayland | **smithay-client-toolkit; wlr-layer-shell** (+ xdg_popup, ext-session-lock later) | Client-side sibling of tomoe's Smithay knowledge; no toolkit between us and the protocol. |
| Layout | **Hand-rolled flex-lite (hbox/vbox/stack, gap/padding/grow)** | nur's element vocabulary is exactly this; taffy adoption deferred until a real grid/wrap need appears (see Deferred). |
| Services | **Native event-driven: D-Bus via rustbus (pure-Rust, sync, fd-exposed) + sysfs; no subprocess polling in steady state** | nur's `wpctl`/`nmcli`/`playerctl` polling costs memory, wakeups, and latency. Fixing that is part of the point. rustbus, not zbus (revised at M3 §3): zbus structurally requires an async executor (async-io reactor thread or tokio), which the single-thread calloop decision forbids — rustbus exposes the socket fd and drains nonblocking, so D-Bus rides calloop like every other source. |
| Compositor integration | **IPC only, auto-detected: tomoe (`$TOMOE_SOCKET`), niri, Hyprland, Sway** | Compositor-agnosticism is the product. No shared Rust with tomoe beyond the `tomoe-ipc` wire crate. |

## Architecture

```
crates/
  moonshell/     # binary: config resolution, calloop bootstrap, CLI      (core)
  surface/       # SCTK: layer surfaces, shm pools, damage regions,
                 # frame-callback scheduling, seat/pointer/keyboard input (mechanism)
  render/        # element tree → tiny-skia; cosmic-text glyph/line cache;
                 # per-element damage diffing                             (mechanism)
  runtime/       # mlua VM lifecycle, shell.*/ui.* API, reactive state,
                 # element-table parsing, hot reload                      (mechanism)
  services/      # battery/network/audio/mpris/tray/notifications/
                 # compositor IPC; no Lua dependency                      (mechanism)
lua/             # stdlib (ui.* constructors), widgets, themes            (policy —
                 # the builtins layer, doctrine 01)
```

The dependency arrow is one-way: `runtime` sees `surface`/`render`/
`services`; nothing sees `runtime`. Services expose plain state +
change notification (calloop channel), not Lua types — the bridge into
`shell.services.*` lives entirely in `runtime`.

## Extension surface contract

- **Read path:** service state is cloned into Lua-side reactive values
  (`shell.state`) on change; render functions read those and return
  element tables. Lua never holds references into Rust state.
- **Write path:** element trees, window options, and service actions
  (`:set_volume(...)`) are data handed to the core; applied after the
  callback returns.
- **Escape hatches (the QuickShell rivalry, in order of power):**
  LuaJIT FFI (any C library from config); generic D-Bus proxy API in
  Lua (services we didn't write become userland); `ui.canvas` exposing
  tiny-skia paths (widgets we didn't ship become userland); process/
  socket/file-watch primitives on calloop.
- **Named exception:** no dispatch watchdog (doctrine 02 divergence
  above).

## Interconnection

### tomoe (`~/Dev/tomoe`)

The doctrine-03 pair: tomoe is the state-owning daemon, moonshell the
thin client. Contract (mirrored in tomoe's DESIGN.md/PLAN.md):

- moonshell is the **first external consumer of `tomoe-ipc`**, pulled
  as a git dependency. Every capability moonshell needs from tomoe
  forces a versioned, public protocol addition — repo separation makes
  the wire the only channel.
- Workspace/window vocabulary for bars comes from tomoe's *Lua* layer
  (`wm.lua` broadcasts via `tomoe.ipc.broadcast`), since workspaces are
  policy there, plus core events (`focus_change`, `window_open/close`).
  Cross-compositor widgets normalize this against niri/Hyprland/Sway
  equivalents and, eventually, wlr-foreign-toplevel-management.
- Integration ships as **content, not code**: tomoe provides a default
  bar config for moonshell (as it ships `wm.lua`); a combined
  home-manager module composes both flakes.
- Shared Lua conventions (settings-table shape, `on_*` hook naming,
  the `Mod` convention, reload contract) belong in a `~/Dev/doctrines/`
  conventions doc, not shared code (roadmap M2).
- No shared `lua-host` crate up front: tomoe's Lua host is entangled
  with frame-loop timing moonshell doesn't have. Extract only after
  real duplication is observed in both working codebases.

### nur (`~/Dev/nur`)

moonshell is nur's successor — the Lua API, `lua/` stdlib, widget
modules, and services *logic* migrate here; GPUI, the `APP_PTR`/cx
bridge, and the tokio runtime do not. nur winds down once moonshell
reaches widget parity (M3); until then it remains the working reference
and the daily driver.

## Deferred (and why)

- **GPU rendering / shader effects on widgets** — conceded to the
  compositor (tomoe M6 blur/shadows). Revisit only if a real use case
  survives that division of labor.
- **taffy** — adopt if/when grid or wrapping layouts are demanded;
  flex-lite covers the bar/launcher/OSD genre.
- **IME (zwp-text-input) for the M5 text field** — caret/selection via
  cosmic-text first; IME is real work and tracked honestly rather than
  implied. Same posture as tomoe (its IME is also post-parity).
- **GTK/Qt theme or CSS compatibility** — never. Themes are Lua tables
  (`lua/themes/`), ported from nur.
- **Rich embedded components (tables, charts, code editors)** — out of
  genre; `ui.canvas` + FFI is the answer for outliers.
- **Windows/X11/macOS** — Wayland layer-shell only.

## Roadmap

- [x] **M0 — bare core boots.** Flake + devshell; SCTK layer surface,
  double-buffered shm, tiny-skia clear + one cosmic-text line; damage-only
  redraw; frame callbacks only while dirty. The no-config binary is the
  doctrine-06 artifact, wired into `nix flake check`.
  *Accept: a static bar idles under 20 MB RSS (smem) with zero wakeups
  in powertop; unplugging/replugging the output survives.*
  *Done 2026-07-08: 6.9 MB idle RSS, 0 wakeups (ctx-switch proxy;
  see PLAN.md), unplug/replug verified under headless sway; RSS gate
  runs in CI as the `boot` flake check.*
- [x] **M1 — element vocabulary.** hbox/vbox/stack/text/spacer/separator/
  icon (SVG via resvg)/image/progress/circular-progress; flex-lite layout
  (gap, padding, grow, align); per-element damage diffing.
  *Accept: nur's `examples/simple-bar` element tree, fed as a static
  table, renders visually identical to nur-on-GPUI.*
  *Done 2026-07-08: `examples/simple_bar.rs` renders the full tree
  correctly on tomoe; 14.5 MB RSS release, 0 idle wakeups. Pixel A/B
  vs nur was blocked (nur lacks a tomoe backend and maps blank — the
  M3 gap itself); parity verified against nur's source-level layout
  semantics instead. See PLAN.md M1 §4.*
- [x] **M2 — Lua runtime.** mlua/LuaJIT; `shell.window/state/interval/
  once/exec/quit`, `ui.*` stdlib ported verbatim from nur; hot reload
  (inotify on the config tree, fresh VM, windows re-created); Lua
  conventions doc landed in `~/Dev/doctrines/`.
  *Accept: nur's `simple-bar/init.lua` runs unmodified; editing it
  live-reloads without restart; idle RSS still under 25 MB.*
  *Done 2026-07-08: the fixture runs byte-for-byte unmodified (nur
  module names alias to the moonshell modules; placeholder
  `shell.services.*` facades until M3); live edit → reload verified;
  17.4 MB RSS release with the full bar + 1 Hz clock, wakeups only
  from the timer. Conventions doc: `~/Dev/doctrines/conventions/lua.md`.
  See PLAN.md M2 §1–§6.*
- [ ] **M3 — services, natively.** D-Bus (rustbus): UPower, MPRIS,
  NetworkManager, notifications daemon, SNI tray, PowerProfiles; sysfs
  battery fallback;
  compositor backends: **tomoe** (`$TOMOE_SOCKET`, subscribe stream),
  niri, Hyprland, Sway. Widget parity with nur (clock/battery/
  workspaces/network/mpris) as `lua/` modules. **nur wind-down decision
  point.**
  *Accept: workspaces widget live-updates on both tomoe and niri; zero
  subprocess spawns in steady state (execsnoop-clean for 5 min).*
- [ ] **M4 — interactivity.** Pointer/keyboard input routed through the
  element tree; hover; `ui.button` with `on_click`; scroll; sliders;
  xdg_popup for menus/tooltips; keyboard-interactive layer surfaces.
  *Accept: volume slider drags; tray icon opens its menu; hover
  tooltips appear and damage correctly.*
- [ ] **M5 — launcher-grade.** `ui.input` (cosmic-text editor: caret,
  selection, no IME yet); virtualized `ui.list`; ext-session-lock
  surface support.
  *Accept: an app launcher fuzzy-filters all `.desktop` entries with
  imperceptible keystroke latency; a Lua lock screen locks and unlocks.*
- [ ] **M6 — escape hatches.** LuaJIT FFI enabled + documented; generic
  D-Bus proxy API in Lua; `ui.canvas` (tiny-skia paths from Lua);
  process/socket/file primitives; LuaLS `---@meta` files.
  *Accept: one nontrivial widget (e.g. a CPU graph via canvas, or a
  daemon integration via generic D-Bus) built purely in userland, no
  Rust changes.*
