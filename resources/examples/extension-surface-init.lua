-- Example: the extension surface end-to-end — window rules, the process
-- manifest, user IPC, reload persistence, compositor-drawn UI, and a
-- custom screencast policy, all layered on the default wm module. Try it
-- without touching your real config:
--
--   tomoe --config resources/examples/extension-surface-init.lua
--
-- Point LuaLS at resources/meta/ for completion and type checking while
-- editing; docs/lua-api.md is the same surface as prose.

local wm = require("wm")
wm.gaps = 8

tomoe.settings {
  mod = "super",
  border = { width = 2, focused = "#7aa2f7", unfocused = "#3b4261", radius = 12 },
  shadow = { range = 12, color = "#00000099", power = 3 },
}

-- ─── Window rules ────────────────────────────────────────────────────────────
-- Matcher fields (`app_id`/`title` are Lua patterns, `match` a predicate)
-- select windows; data properties are read by the WM via tomoe.rules_for
-- (wm.lua honors workspace/fullscreen/focus); `apply` runs when a matching
-- window opens, after the on_window_open hooks.
tomoe.rule { app_id = "firefox", workspace = 2 }
tomoe.rule {
  app_id = "^mpv$",
  fullscreen = true,
  -- Product policy uses the same queued public surface as user config. This
  -- grants XWayland games tearing without a wp_tearing_control hint and gives
  -- video windows their own decoration treatment.
  apply = function(win)
    win:set_properties {
      radius = 0,
      tearing = true,
      border = { focused = "#bb9af7", unfocused = "#565f89" },
    }
  end,
}
tomoe.rule { title = "Picture%-in%-Picture", focus = false }
tomoe.rule {
  app_id = "^foot$",
  match = function(win)
    return win:title() ~= "scratchpad"
  end,
  apply = function(win)
    win:raise()
  end,
}

-- ─── Processes ───────────────────────────────────────────────────────────────
-- Declarative manifest, diffed by id across config reloads: with
-- keep_if_unchanged the bar survives every reload that doesn't change its
-- spec, and deleting a declaration stops the process on the next reload.
tomoe.process.service("waybar", {
  restart = "on_exit",
  reload = "keep_if_unchanged",
})
tomoe.process.once("wallpaper", {
  command = { "swaybg", "-i", (os.getenv("HOME") or "") .. "/.wallpaper.png" },
  run = "once_per_session",
})

-- ─── IPC ─────────────────────────────────────────────────────────────────────
-- User endpoints on the compositor socket, driven by any client:
--
--   tomoe msg workspace/switch '{"n": 2}'
--   tomoe msg workspace/state
--   tomoe msg subscribe            # streams the broadcasts below
--
-- Handlers run as normal Lua entries: snapshot reads, queued writes, and
-- errors go back to the requesting client instead of crashing anything.
local function workspace_state()
  local occupied = {}
  for i, ws in ipairs(wm.workspaces) do
    if #ws > 0 then
      occupied[#occupied + 1] = i
    end
  end
  return { active = wm.active, occupied = occupied }
end

tomoe.ipc.serve("workspace/state", workspace_state)
tomoe.ipc.serve("workspace/switch", function(params)
  local n = assert(tonumber(params and params.n), "params.n required")
  wm.switch(n)
  return workspace_state()
end)

-- Event vocabulary for bars: wrap the wm entry points (policy is plain
-- Lua — extend it by wrapping) and push state after anything that can
-- change it. `tomoe msg subscribe` or any tomoe-ipc client receives these.
local function announce()
  tomoe.ipc.broadcast("workspace/changed", workspace_state())
end
for _, fn in ipairs({ "switch", "move_focused" }) do
  local inner = wm[fn]
  wm[fn] = function(...)
    inner(...)
    announce()
  end
end
tomoe.on_window_open(announce)
tomoe.on_window_close(announce)
tomoe.on_focus_change(function(win)
  tomoe.ipc.broadcast("focus/changed", {
    title = win and win:title() or nil,
    app_id = win and win:app_id() or nil,
  })
end)

-- ─── Scratchpad + reload persistence ─────────────────────────────────────────
-- Mark the focused window as the scratchpad (Mod+Shift+minus), then toggle
-- it with Mod+minus: hiding pulls it out of the tiling, showing drops it
-- back in on the active workspace. Its identity survives config reloads
-- via tomoe.on_reload — save runs in the outgoing VM and returns
-- JSON-compatible values (persist ids, never handles), restore runs in the
-- fresh VM. Keyed persistence composes: wm.lua persists its workspaces
-- under its own key, this state rides under "scratchpad".
local scratch = { id = nil, hidden = false }

tomoe.on_reload("scratchpad", function()
  return { id = scratch.id, hidden = scratch.hidden }
end, function(saved)
  local win = saved.id and tomoe.window(saved.id)
  if win then
    scratch.id = saved.id
    scratch.hidden = saved.hidden == true
    if scratch.hidden then
      -- wm's restore re-tiled every surviving window; pull the hidden
      -- scratchpad back out.
      for _, ws in ipairs(wm.workspaces) do
        for i, w in ipairs(ws) do
          if w:id() == scratch.id then
            table.remove(ws, i)
            break
          end
        end
      end
      win:hide()
      wm.arrange()
    end
  end
end)

tomoe.bind("Mod+Shift+minus", function()
  local win = tomoe.focused_window()
  if not win then
    return
  end
  scratch.id, scratch.hidden = win:id(), false
  tomoe.ui.toast { text = "scratchpad: " .. win:title() }
end, "Mark focused window as scratchpad")

tomoe.bind("Mod+minus", function()
  local win = scratch.id and tomoe.window(scratch.id)
  if not win then
    tomoe.ui.toast { text = "no scratchpad window", urgent = true }
    return
  end
  if scratch.hidden then
    table.insert(wm.workspaces[wm.active], win)
    wm.arrange()
    win:raise()
    win:focus()
  else
    for _, ws in ipairs(wm.workspaces) do
      for i, w in ipairs(ws) do
        if w:id() == scratch.id then
          table.remove(ws, i)
          break
        end
      end
    end
    win:hide()
    wm.arrange()
  end
  scratch.hidden = not scratch.hidden
end, "Toggle scratchpad")

-- ─── Compositor UI ───────────────────────────────────────────────────────────
-- Retained widgets: declare once, the core renders and routes input, only
-- the selection re-enters Lua. Modal (menu/confirm) widgets own the
-- keyboard; Esc or a click outside cancels.
tomoe.bind("Mod+Escape", function()
  tomoe.ui.menu {
    title = "Power",
    items = { "Lock", "Exit tomoe" },
    on_select = function(_, item)
      if item == "Lock" then
        tomoe.spawn("swaylock")
      elseif item == "Exit tomoe" then
        tomoe.quit() -- asks via the (builtin) confirm dialog
      end
    end,
  }
end, "Power menu")

-- ─── Screencast policy ───────────────────────────────────────────────────────
-- Replaces the default picker (`require("screencast")`): deny unknown
-- apps, cast the first output without asking when a monitor is acceptable,
-- and ask via a menu — req:defer() keeps the request open past the hook's
-- return; the portal waits, the compositor never does.
tomoe.on_screencast_request(function(req)
  if req.app_id == "" then
    return false
  end
  if req.types.monitor and #req.outputs > 0 then
    return { output = req.outputs[1].name }
  end
  if #req.windows == 0 then
    return false
  end
  req:defer()
  local items = {}
  for i, win in ipairs(req.windows) do
    items[i] = win:title()
  end
  tomoe.ui.menu {
    title = "Share which window with " .. req.app_id .. "?",
    items = items,
    on_select = function(i)
      req:resolve({ window = req.windows[i] })
    end,
    on_cancel = function()
      req:deny()
    end,
  }
end)

-- ─── Binds (the usual daily-driver set) ──────────────────────────────────────
tomoe.bind("Mod+Return", function()
  tomoe.spawn("foot")
end, "Spawn foot")
tomoe.bind("Mod+q", wm.close_focused, "Close Window")
tomoe.bind("Mod+f", wm.toggle_fullscreen, "Toggle Fullscreen")
tomoe.bind("Mod+j", wm.focus_next, "Focus Next Window")
tomoe.bind("Mod+k", wm.focus_prev, "Focus Previous Window")
tomoe.bind("Mod+Shift+e", "quit")
tomoe.bind("Mod+Shift+slash", "show-hotkey-overlay")

for i = 1, wm.workspace_count do
  tomoe.bind("Mod+" .. i, function()
    wm.switch(i)
  end, i == 1 and "Switch to Workspace 1-9" or nil)
  tomoe.bind("Mod+Shift+" .. i, function()
    wm.move_focused(i)
  end, i == 1 and "Move Window to Workspace 1-9" or nil)
end
