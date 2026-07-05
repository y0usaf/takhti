# moonshell

A Lua-scriptable Wayland desktop shell (bars, launchers, OSDs,
notifications, lock screens) built for footprint: CPU-rendered into
wl_shm, no GPU in the process, single-threaded calloop, LuaJIT. Target:
idle RSS under 25 MB where AGS/QuickShell sit at 100–250 MB.

Configs are programs: `~/.config/moonshell/init.lua`, using the
`shell.*` / `ui.*` API inherited from [nur](../nur) (moonshell is its
successor). Compositor-agnostic — tomoe, niri, Hyprland, and Sway are
IPC backends; [tomoe](../tomoe) is the sibling project and moonshell is
the first external consumer of its `tomoe-ipc` wire protocol.

Status: design phase. See `DESIGN.md` (vision, doctrine conformance,
locked decisions, roadmap) and `PLAN.md` (work tracker).
