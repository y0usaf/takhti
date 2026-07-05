# tomoe

A Wayland compositor where the window manager is yours to rewrite.

Tomoe (巴, after Tomoe River — the fountain-pen paper you return to again and
again) is built in Rust on [Smithay](https://github.com/Smithay/smithay), with
an embedded LuaJIT runtime as its configuration language. The config is a
program, not a settings file: the Rust core exposes mechanism — windows,
outputs, input, a camera over the window canvas — and **all** window-management
policy (workspaces, tiling, focus order) is Lua built on that same public API.
If the built-in WM can't be written on the API, the API isn't done.

## Highlights

- **Config as a program** — event hooks, keybinds bound to Lua functions,
  pointer grabs, declarative process management, user-defined IPC endpoints,
  hot reload on save.
- **Replaceable WM policy** — the default dwindle-tiling WM (`require("wm")`)
  and a zoomable-canvas WM (`require("zoomer")`) ship as plain Lua modules;
  extend them or replace them wholesale.
- **Pixel-exact rendering** — all geometry is integer physical pixels, so
  client buffers are sampled 1:1 at any output scale; fractional scaling via
  wp-fractional-scale-v1 + wp-viewporter.
- **Daily-driver plumbing** — DRM/GBM/libinput TTY backend and a nested winit
  backend, direct scanout, VRR, output mirroring, tearing control, session
  lock, screenshots, screencasting portal, XWayland via xwayland-satellite.
- **JSON IPC** — a versioned socket with request/response and event streams
  (`tomoe msg`), extensible from the config with `tomoe.ipc.serve`.

Tomoe is under active development and not yet stable; see `PLAN.md` for the
current milestones.

## Building

Nix is the source of truth for builds:

```sh
nix build            # the compositor package
nix develop          # hacking shell (cargo build / cargo test inside)
```

Run nested in an existing session with `tomoe --backend winit`, or on a TTY
with `./run-tty.sh`.

## Configuration

The config lives at `~/.config/tomoe/init.lua`; without one, a built-in
default keeps the session usable. The complete Lua API reference is
[docs/lua-api.md](docs/lua-api.md) — generated from the LuaLS stubs in
`resources/meta/` and held in lockstep with the actual runtime by tests.
Point lua-language-server at `resources/meta/` for completion and type
checking while editing your config.

## Credits

Tomoe stands on the work of three compositors, studied closely (vendored under
`ref/`) and used as the source of truth for patterns:

- **[niri](https://github.com/YaLTeR/niri)** — the performance blueprint:
  render-on-damage, the per-output redraw state machine, direct scanout, and
  the battle-tested Smithay revision tomoe pins. Its discipline around
  pixel-exact scaling inspired tomoe's physical-first coordinate doctrine.
- **[Hyprland](https://github.com/hyprwm/Hyprland)** — the feature bar:
  animations, effects, window rules, and the expectation that a tiling
  compositor can also be pleasant to look at.
- **[ShojiWM](https://github.com/bea4dev/ShojiWM)** — the configurability
  model: config as a scriptable program, hook-driven WM policy, declarative
  process management, and user-extensible IPC.

## License

GPL-3.0-or-later.
