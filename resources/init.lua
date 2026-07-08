-- tomoe default configuration.
-- Copy to ~/.config/tomoe/init.lua and customise.
--
-- The compositor core is mechanism-only; ALL window management below comes
-- from the `wm` module — Lua code built on the same public API your config
-- uses. Don't require it and write your own to replace the WM wholesale.
--
-- Deeper examples live in resources/examples/ (try one with
-- `tomoe --config <file>`): extension-surface-init.lua exercises rules,
-- processes, IPC, reload persistence, tomoe.ui, and screencast policy;
-- zoomer-init.lua runs the floating/zooming canvas WM.

local wm = require("wm")
wm.gaps = 8

-- Screencast source picker: answers screen-share requests (OBS, browsers)
-- with a compositor-drawn menu; single candidates resolve without asking.
-- Remove to fall back to the portal's TOMOE_SCREENCAST_OUTPUT /
-- TOMOE_PORTAL_CHOOSER heuristics, or register your own
-- tomoe.on_screencast_request to replace the policy.
require("screencast")

tomoe.settings {
  -- What "Mod" means in binds and pointer events: "super" (default),
  -- "alt", "ctrl", or "shift". Declare it once; write binds against Mod.
  mod = "super",
  -- Per-output display settings, keyed by output name. `resolution` is
  -- "<preferred|max|WxH>" optionally followed by "@<Hz|max>"; unlisted
  -- outputs use their EDID-preferred mode. Some monitors advertise a low
  -- compatibility mode as preferred — "max@max" gets their best.
  -- `position` places the output explicitly (physical pixels, may be
  -- negative); outputs without one pack left-to-right after the placed
  -- ones. `mirror = "DP-1"` shows the same region as that output instead
  -- of extending; `disabled = true` turns the connector off entirely.
  -- `vrr = true` enables variable refresh rate (adaptive sync) when the
  -- monitor supports it.
  -- displays = {
  --   ["DP-1"] = { resolution = "max@max", position = { 0, 0 }, vrr = true },
  --   ["HDMI-A-1"] = { resolution = "1920x1080@60", mirror = "DP-1" },
  --   ["eDP-1"] = { disabled = true },
  -- },
  -- Focus the window under the pointer as it moves (sloppy focus: leaving
  -- onto empty space keeps focus). Default: click-to-focus.
  -- focus_follows_mouse = true,
  -- Allow tearing (async page flips) for fullscreen windows that request
  -- it via wp_tearing_control — lowest latency for games, at the cost of
  -- visible tear lines. Default: off.
  -- tearing = true,
  -- Wait for rendering to finish before queueing each frame to KMS. Set
  -- this on NVIDIA: fenced frames queued before the render completes hang
  -- the driver (whole-session freeze). Costs a little latency. Default: off.
  -- wait_for_frame_completion = true,
  -- xkb keymap and key repeat. Unset fields use the xkb defaults
  -- (including XKB_DEFAULT_* environment variables).
  -- keyboard = {
  --   layout = "us,de",              -- comma-separated layouts
  --   variant = ",nodeadkeys",       -- one variant per layout
  --   options = "caps:escape,grp:alt_shift_toggle",
  --   repeat_delay = 600,            -- ms before a held key repeats
  --   repeat_rate = 25,              -- repeats per second
  -- },
  -- libinput config by device class. Unset fields keep the device's
  -- libinput default, so removing a line and reloading undoes it.
  -- touchpad = {
  --   tap = true,
  --   natural_scroll = true,
  --   dwt = true,                    -- disable while typing
  --   accel_speed = 0.0,             -- -1.0 (slowest) .. 1.0 (fastest)
  --   accel_profile = "adaptive",    -- or "flat"
  --   scroll_method = "two_finger",  -- "edge", "on_button_down", "none"
  --   click_method = "clickfinger",  -- or "button_areas"
  --   tap_drag = true, tap_drag_lock = false,
  --   middle_emulation = false, left_handed = false,
  --   disabled = false, disabled_on_external_mouse = false,
  -- },
  -- mouse = {
  --   accel_profile = "flat",
  --   natural_scroll = false,
  -- },
  -- Per-device overrides on top of touchpad/mouse, keyed by the libinput
  -- device name (`libinput list-devices`; also logged on hotplug).
  -- devices = {
  --   ["Logitech G Pro"] = { accel_speed = -0.3 },
  -- },
  border = {
    width = 2,
    focused = "#7aa2f7",
    unfocused = "#3b4261",
    -- Window corner radius in physical pixels (0 = square). Fullscreen
    -- windows never round.
    -- radius = 12,
  },
}

-- ─── Window rules ──────────────────────────────────────────────────────────────────────
-- Declarative per-window policy. Matcher fields select windows (`app_id`
-- and `title` are Lua patterns — anchor with ^$ for exact; `match` is a
-- predicate); `apply` runs when a matching window opens; every other field
-- is a data property the WM reads via tomoe.rules_for. The wm module
-- honors `workspace = n`, `fullscreen = true`, and `focus = false`.
-- tomoe.rule { app_id = "^mpv$", fullscreen = true }
-- tomoe.rule { app_id = "firefox", workspace = 2 }
-- tomoe.rule { title = "Picture%-in%-Picture", focus = false }
-- tomoe.rule { app_id = "^foot$", apply = function(win) win:raise() end }

-- ─── Processes ───────────────────────────────────────────────────────────────
-- Declarative manifest, diffed by id across config reloads: `once` runs
-- session-bootstrap tasks, `service` keeps daemons (bars, notifications)
-- alive, `tomoe.process.spawn { ... }` / `tomoe.spawn("...")` are
-- fire-and-forget for event handlers. `command` is an argv array (or a
-- string for `sh -c`); omitted, the id itself is the command.
-- tomoe.process.once("fcitx5", { command = { "fcitx5", "-d" } })
-- tomoe.process.service("waybar", { restart = "on_exit" })
-- tomoe.process.service("mako", {
--   restart = "on_failure",         -- or "never" | "on_exit" (default)
--   reload = "keep_if_unchanged",   -- or "always_restart"
-- })

-- ─── Launch / close ──────────────────────────────────────────────────────────
-- The optional third argument labels the bind in the hotkey overlay
-- (Mod+Shift+/); Lua-function binds without one are omitted from it.
tomoe.bind("Mod+Return", function() tomoe.spawn("foot") end, "Spawn foot")
tomoe.bind("Mod+d", function() tomoe.spawn("fuzzel") end, "Run an Application")
tomoe.bind("Mod+q", wm.close_focused, "Close Window")
tomoe.bind("Mod+f", wm.toggle_fullscreen, "Toggle Fullscreen")
tomoe.bind("Mod+Shift+e", "quit")
tomoe.bind("Mod+Shift+slash", "show-hotkey-overlay")

-- ─── Focus ───────────────────────────────────────────────────────────────────
tomoe.bind("Mod+j", wm.focus_next, "Focus Next Window")
tomoe.bind("Mod+k", wm.focus_prev, "Focus Previous Window")

-- ─── Workspaces ──────────────────────────────────────────────────────────────
for i = 1, wm.workspace_count do
  tomoe.bind("Mod+" .. i, function() wm.switch(i) end,
    i == 1 and "Switch to Workspace 1-9" or nil)
  tomoe.bind("Mod+Shift+" .. i, function() wm.move_focused(i) end,
    i == 1 and "Move Window to Workspace 1-9" or nil)
end
