# tomoe

**tomoe** (巴, after Tomoe River — the fountain-pen paper you return to again
and again) is a Wayland compositor where the window manager is yours to
rewrite. The Rust core exposes mechanism — windows, outputs, input, a camera
over an infinite canvas — and every policy decision above it (workspaces,
tiling, focus order, even what a titlebar drag does) is Lua, hot-reloaded on
save. Built on [Smithay](https://github.com/Smithay/smithay). Under active
development, not yet stable.

---

**Contents** —
[Design](#design) ·
[Architecture](#architecture) ·
[Building](#building) ·
[Running](#running) ·
[Configuration](#configuration) ·
[IPC](#ipc) ·
[Lua API reference](#lua-api-reference) ·
[Credits](#credits) ·
[License](#license)

---

## Design

### Mechanism, not policy

The core implements what a compositor *must* own — protocol handling,
rendering, input routing, output management — and stops there. Everything
recognizable as window management ships as plain Lua built on the same public
API user configs use: the default tiling WM is `require("wm")`, and a second,
entirely different paradigm (`require("zoomer")`, a pannable/zoomable floating
canvas) exists to prove the point. If the built-in WM couldn't be written on
the public API, the API wouldn't be done. Replacing the WM wholesale means
not requiring the module and writing your own; with no hooks registered at
all, windows map full-screen, so a broken config still shows something.

### The extension contract

Lua never touches compositor state directly. Reads (windows, outputs,
[camera](#the-camera), pointer) come from a snapshot refreshed before every
Lua entry; writes are queued operations applied when the callback returns.
The render loop never waits on config code. Policy is driven by
[hooks](docs/lua-api.md#hooks) — window open/close, focus, output changes,
pointer buttons and scroll, client requests like fullscreen or an interactive
move — where returning a truthy value consumes the event and makes your
config responsible for answering it. Editing the config re-runs it in a fresh
VM and replays window-open events, so the new policy adopts the windows that
already exist; a config error is surfaced as an on-screen notification.

### Pixel-exact rendering

Sharp rendering at any scale is enforced structurally rather than by
convention:

1. The canonical coordinate space is **integer physical pixels** — layout,
   the entire Lua API, and render positions alike. Integers on the physical
   grid cannot misalign, so there is no rounding to forget.
2. All logical↔physical conversion lives in one module (`coords.rs`);
   protocol objects that speak logical coordinates convert exactly once at
   that boundary.
3. Client sizes are quantized where the protocol demands it, while
   compositor-drawn pixels are free at any physical integer — which is why a
   1-device-pixel border works at every scale.
4. Scale snaps to N/120, the only granularity `wp-fractional-scale-v1` can
   express; with `wp-viewporter` advertised, clients render at native density
   from their first buffer and are sampled 1:1.

### The camera

Windows live on a world-coordinate canvas viewed through a per-space camera:
`screen = (world − offset) · zoom`. The offset is integer physical, so
panning and the identity view keep every pixel on the grid; zooming is the
one sanctioned resampling path, meant for transient states like an overview.
Input stays in screen space and hit-testing inverts the camera, so clients
receive exact buffer-local coordinates at any pan or zoom. The
[`zoomer`](docs/lua-api.md#zoomer) module drives all of this from Lua — see
[`tomoe.set_view`](docs/lua-api.md#outputs--camera).

## Architecture

```
crates/
  tomoe/                     the compositor
    src/
      main.rs                CLI (incl. `tomoe msg`), backend selection
      state.rs               central state; applies queued Lua operations
      handlers.rs            Smithay delegate impls (xdg-shell, seat, …)
      backend/               winit (nested dev) · tty (DRM/GBM/libinput)
      space.rs, coords.rs    physical-pixel space + the one conversion boundary
      lua.rs                 the `tomoe` API table, snapshots, hooks
      input.rs               combo parsing, bind dispatch, libinput config
      render.rs, capture.rs  render elements; screencopy/screencast paths
      ipc.rs, process.rs     JSON IPC server · process manifest supervisor
      lock.rs, xwayland.rs   session lock · xwayland-satellite integration
      ui/                    hotkey overlay, exit dialog, screenshot UI, …
  tomoe-ipc/                 wire contract for the IPC socket (see IPC)
  xdg-desktop-portal-tomoe/  ScreenCast portal backend, paced at output refresh
resources/                   default config, wm.lua, zoomer.lua, LuaLS stubs
docs/lua-api.md              generated API reference
```

## Building

With Nix, the flake is the source of truth:

```sh
nix build            # package
nix develop          # dev shell (cargo build / cargo test inside)
```

Without Nix: a stable Rust toolchain, `pkg-config`, `libclang`, and dev
headers for EGL/GBM (mesa), wayland, libinput, libseat, libxkbcommon,
libudev, libdisplay-info, dbus, and pipewire. LuaJIT is vendored — no system
Lua needed.

```sh
cargo build --release            # target/release/{tomoe,xdg-desktop-portal-tomoe}
cargo install --path crates/tomoe
```

## Running

Nested inside an existing session (a window, for development):

```sh
tomoe --backend winit
```

As a real session, from a TTY or a display manager:

```sh
tomoe --backend tty
```

On startup tomoe exports `WAYLAND_DISPLAY`, `DISPLAY` (X11 apps connect
through xwayland-satellite, spawned on first use), and `TOMOE_SOCKET` (see
[IPC](#ipc)) into the systemd user environment, and activates
`graphical-session.target` through the installed `tomoe-session.target`.
Screencasting needs the portal backend installed: the
`xdg-desktop-portal-tomoe` binary plus the `tomoe.portal`,
`tomoe-portals.conf`, and D-Bus service files — the flake's `postInstall`
shows the exact layout; non-Nix installs replicate it from `resources/`.

## Configuration

The config is a Lua program at `~/.config/tomoe/init.lua`, re-run on every
save ([see the extension contract](#the-extension-contract)). Without one, a
built-in default keeps the session usable. A minimal config:

```lua
local wm = require("wm")            -- the default tiling WM; omit to replace it
wm.gaps = 4

tomoe.settings {
  mod = "super",                    -- what "Mod" means, declared once
  displays = {
    ["DP-1"] = { resolution = "max@max", position = { 0, 0 }, vrr = true },
  },
  border = { width = 2, focused = "#7aa2f7", unfocused = "#3b4261" },
}

tomoe.process.service("waybar", { restart = "on_exit" })

tomoe.bind("Mod+Return", function() tomoe.spawn("foot") end, "Terminal")
tomoe.bind("Mod+q", wm.close_focused, "Close window")
for i = 1, 9 do
  tomoe.bind("Mod+" .. i, function() wm.switch(i) end)
end
```

Every settings field — per-output modes and mirroring, xkb keymaps, libinput
per-device overrides, tearing — is enumerated in the
[API reference](docs/lua-api.md#settings). The default config
(`resources/init.lua`) doubles as annotated documentation.

### The wm and zoomer modules

[`wm`](docs/lua-api.md#wm) is dwindle tiling with nine workspaces, focus
cycling, and fullscreen handling — a few hundred lines of ordinary Lua whose
state (`wm.workspaces`, `wm.active`) is plain data your config can inspect
and mutate. [`zoomer`](docs/lua-api.md#zoomer) turns the same compositor into
a floating, zooming canvas: `Mod+drag` moves and resizes, `Mod+scroll` zooms
around the cursor, and numbered planes each remember their camera. Both are
preloaded; `require` one, neither, or your own.

### Processes

`tomoe.process` is a declarative manifest diffed by id across config
reloads: `once` entries bootstrap the session, `service` entries keep daemons
alive per their restart policy, and reloading a config that no longer
declares a process stops it. Fire-and-forget spawns exist for event handlers.
Details in the [API reference](docs/lua-api.md#processes).

### Editor support

`resources/meta/tomoe.lua` ships LuaLS stubs for the entire API. Point
lua-language-server at it for completion and type checking:

```jsonc
// .luarc.json
{ "workspace.library": ["/path/to/tomoe/resources/meta"] }
```

## IPC

A JSON socket at `$TOMOE_SOCKET` (newline-delimited request/response plus an
event stream after `subscribe`), with a CLI:

```sh
tomoe msg windows                          # built-ins: version, windows,
tomoe msg outputs                          #   outputs, view, subscribe, quit
tomoe msg my/endpoint '{"arg": 1}'         # anything the config serves
```

The wire contract lives in the small, versioned `tomoe-ipc` crate; the
*vocabulary* is open — configs register endpoints with
[`tomoe.ipc.serve`](docs/lua-api.md#ipc) and push events to subscribers with
`tomoe.ipc.broadcast`, so bars and scripts talk to your WM policy, not just
to the compositor core.

## Lua API reference

[docs/lua-api.md](docs/lua-api.md) — one page covering the `tomoe` global
([core](docs/lua-api.md#core), [windows](docs/lua-api.md#windows),
[outputs & camera](docs/lua-api.md#outputs--camera),
[hooks](docs/lua-api.md#hooks), [processes](docs/lua-api.md#processes),
[IPC](docs/lua-api.md#ipc), all [types](docs/lua-api.md#types)) and the
built-in modules. It is generated from the LuaLS stubs and the module
sources, and parity tests hold it to the API the runtime actually registers:
`cargo test` fails if they drift.

## Credits

An original implementation, designed by studying
[niri](https://github.com/YaLTeR/niri) (damage-driven rendering, pixel
exactness), [Hyprland](https://github.com/hyprwm/Hyprland) (the feature bar),
and [ShojiWM](https://github.com/bea4dev/ShojiWM) (config as a program).

## License

[AGPL-3.0-or-later](LICENSE).
