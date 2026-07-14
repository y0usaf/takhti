-- Default window manager for tomoe, implemented entirely on the public Lua API.
-- This is the same surface user configs use: replace this module wholesale by
-- not requiring it, or extend it by wrapping its functions.
--
-- All geometry (window/output coordinates, gaps) is in *physical pixels*:
-- integers stay integers at any output scale, so layouts can never cause
-- blurry, misaligned windows. Set the scale with tomoe.settings{ scale = n }.
--
--   local wm = require("wm")
--   wm.gaps = 4
--   tomoe.bind("Mod+1", function() wm.switch(1) end)

---The default window manager: classic dwindle tiling with numbered
---workspaces, built entirely on the public API. Preloaded as module "wm";
---requiring it installs its hooks. All fields are plain data — mutate them
---and call `arrange()`. Honors these window-rule properties (`tomoe.rule`):
---`workspace = n` opens the window on workspace n, `fullscreen = true`
---opens it fullscreen, `focus = false` opens it without stealing focus.
---@class wm
---@field gaps integer # gap between windows in physical pixels (default 8)
---@field workspace_count integer # number of workspaces (default 9)
---@field honor_client_fullscreen boolean # honor application fullscreen requests; false keeps windows tiled unless a rule or WM command makes them fullscreen (default false)
---@field active integer # index of the visible workspace
---@field workspaces Window[][] # workspaces[i] = ordered list of window objects
---@field fullscreen table<integer, true> # fullscreen[window id] = true: excluded from tiling, covers its output
local M = {
  gaps = 8,
  workspace_count = 9,
  -- Applications do not choose layout policy: fullscreen rules and explicit
  -- WM commands still work, but client requests remain tiled by default.
  honor_client_fullscreen = false,
  active = 1,
  -- workspaces[i] = ordered list of window objects
  workspaces = {},
  -- fullscreen[window id] = true: excluded from tiling, covers its output.
  fullscreen = {},
}

for i = 1, M.workspace_count do
  M.workspaces[i] = {}
end

local function find(list, win)
  for i, w in ipairs(list) do
    if w:id() == win:id() then
      return i
    end
  end
  return nil
end

local function remove(list, win)
  local i = find(list, win)
  if i then
    table.remove(list, i)
  end
end

-- ── Bar-facing IPC (the moonshell vocabulary; PLAN.md "moonshell-driven") ──
-- Workspace state is policy, so it rides the *user* event vocabulary — the
-- wire crate stays frozen (doctrine 03). `wm_state` is both a served method
-- (a bar's initial fetch) and a broadcast event (fired after every workspace
-- mutation). Payload shape, kept mechanical so any bar can consume it:
--   { active = n, workspaces = { { id = i, windows = count }, ... } }
-- Which workspaces to *display* (all, occupied-only, …) is the bar's policy;
-- all of them are reported, with counts.

local function wm_state()
  local ws = {}
  for i = 1, M.workspace_count do
    ws[i] = { id = i, windows = #M.workspaces[i] }
  end
  return { active = M.active, workspaces = ws }
end

local function announce()
  tomoe.ipc.broadcast("wm_state", wm_state())
end

tomoe.ipc.serve("wm_state", wm_state)

---Retile the active workspace (classic dwindle: split the remaining area
---along its longer side). Fullscreen windows keep their output-covering
---geometry and stay on top.
function M.arrange()
  local area = tomoe.usable_area()
  local wins, full = {}, {}
  for _, win in ipairs(M.workspaces[M.active]) do
    if M.fullscreen[win:id()] then
      table.insert(full, win)
    else
      table.insert(wins, win)
    end
  end
  local n = #wins
  local g = M.gaps
  local x, y = area.x + g, area.y + g
  local w, h = area.w - 2 * g, area.h - 2 * g
  for i, win in ipairs(wins) do
    if i == n then
      win:set_geometry(x, y, w, h)
    elseif w >= h then
      local half = math.floor((w - g) / 2)
      win:set_geometry(x, y, half, h)
      x = x + half + g
      w = w - half - g
    else
      local half = math.floor((h - g) / 2)
      win:set_geometry(x, y, w, half)
      y = y + half + g
      h = h - half - g
    end
    win:show()
  end
  for _, win in ipairs(full) do
    win:show()
    win:raise()
  end
end

-- The output containing the window's center, or the named/first output.
local function output_for(win, name)
  local outs = tomoe.outputs()
  for _, o in ipairs(outs) do
    if o.name == name then
      return o
    end
  end
  local geo = win:geometry()
  if geo then
    local cx, cy = geo.x + geo.w / 2, geo.y + geo.h / 2
    for _, o in ipairs(outs) do
      if cx >= o.x and cx < o.x + o.w and cy >= o.y and cy < o.y + o.h then
        return o
      end
    end
  end
  return outs[1]
end

---Fullscreen `win` on the output containing it (or on `output_name`), or
---restore it back into the tiling.
---@param win Window
---@param on boolean
---@param output_name string?
function M.set_fullscreen(win, on, output_name)
  if on then
    local o = output_for(win, output_name)
    if not o then
      return
    end
    M.fullscreen[win:id()] = true
    win:set_fullscreen(true)
    win:set_geometry(o.x, o.y, o.w, o.h)
    win:focus()
  else
    M.fullscreen[win:id()] = nil
    win:set_fullscreen(false)
  end
  M.arrange()
end

---Toggle fullscreen for the focused window.
function M.toggle_fullscreen()
  local win = tomoe.focused_window()
  if win then
    M.set_fullscreen(win, not M.fullscreen[win:id()])
  end
end

---Switch to workspace `n`, focusing its most recent window.
---@param n integer
function M.switch(n)
  if n == M.active or n < 1 or n > M.workspace_count then
    return
  end
  for _, win in ipairs(M.workspaces[M.active]) do
    win:hide()
  end
  M.active = n
  M.arrange()
  announce()
  local last = M.workspaces[n][#M.workspaces[n]]
  if last then
    last:focus()
  else
    -- Don't leave a hidden window from the old workspace holding the keyboard.
    tomoe.clear_focus()
  end
end

---Move the focused window to workspace `n`.
---@param n integer
function M.move_focused(n)
  if n == M.active or n < 1 or n > M.workspace_count then
    return
  end
  local win = tomoe.focused_window()
  if not win then
    return
  end
  remove(M.workspaces[M.active], win)
  win:hide()
  table.insert(M.workspaces[n], win)
  M.arrange()
  announce()
  local last = M.workspaces[M.active][#M.workspaces[M.active]]
  if last then
    last:focus()
  else
    tomoe.clear_focus()
  end
end

local function cycle(dir)
  local wins = M.workspaces[M.active]
  if #wins == 0 then
    return
  end
  local focused = tomoe.focused_window()
  local idx = focused and find(wins, focused) or 1
  idx = ((idx - 1 + dir) % #wins) + 1
  wins[idx]:focus()
end

---Focus the next window on the active workspace.
function M.focus_next()
  cycle(1)
end

---Focus the previous window on the active workspace.
function M.focus_prev()
  cycle(-1)
end

---Close the focused window.
function M.close_focused()
  local win = tomoe.focused_window()
  if win then
    win:close()
  end
end

tomoe.on_window_open(function(win)
  local r = tomoe.rules_for(win)
  local target = M.active
  if type(r.workspace) == "number" then
    local n = math.floor(r.workspace)
    if n >= 1 and n <= M.workspace_count then
      target = n
    end
  end
  table.insert(M.workspaces[target], win)
  announce()
  if target ~= M.active then
    -- Ruled onto another workspace: keep it hidden, don't steal focus.
    win:hide()
    return
  end
  if r.fullscreen or (M.honor_client_fullscreen and win:is_fullscreen()) then
    M.set_fullscreen(win, true)
  else
    -- A client can request fullscreen before its first map. Undo the protocol
    -- state as well as tiling it; an explicit fullscreen rule still wins.
    if win:is_fullscreen() then
      win:set_fullscreen(false)
    end
    M.arrange()
  end
  if r.focus ~= false then
    win:focus()
  end
end)

tomoe.on_window_close(function(win)
  M.fullscreen[win:id()] = nil
  for i = 1, M.workspace_count do
    remove(M.workspaces[i], win)
  end
  M.arrange()
  announce()
  local last = M.workspaces[M.active][#M.workspaces[M.active]]
  if last then
    last:focus()
  end
end)

-- Client state requests (F11, video players, …). Consuming the event (truthy
-- return) makes this config responsible for responding. Fullscreen requests
-- are denied by default so applications cannot choose layout policy; set
-- honor_client_fullscreen = true to accept them. Maximize/minimize fall through
-- to the native default (ack / ignore) — a tiled layout has no separate state.
tomoe.on_window_request(function(ev)
  if ev.type == "fullscreen" then
    if M.honor_client_fullscreen then
      M.set_fullscreen(ev.window, true, ev.output)
    end
    return true
  elseif ev.type == "unfullscreen" then
    M.set_fullscreen(ev.window, false)
    return true
  elseif ev.type == "activate" then
    -- xdg-activation with a valid token (e.g. a notification click):
    -- switch to the window's workspace, then focus it.
    for i = 1, M.workspace_count do
      if find(M.workspaces[i], ev.window) then
        M.switch(i)
        ev.window:focus()
        return true
      end
    end
  end
end)

tomoe.on_outputs_changed(function()
  M.arrange()
end)

-- Survive config reloads: persist workspace assignments as window ids (the
-- only thing that outlives the config VM) and rebuild the tables from them,
-- instead of letting the core replay every window into the active workspace.
tomoe.on_reload("wm", function()
  local ws = {}
  for i = 1, M.workspace_count do
    local ids = {}
    for _, win in ipairs(M.workspaces[i]) do
      ids[#ids + 1] = win:id()
    end
    ws[i] = ids
  end
  local full = {}
  for id in pairs(M.fullscreen) do
    full[#full + 1] = id
  end
  return { active = M.active, workspaces = ws, fullscreen = full }
end, function(state)
  local seen = {}
  for i = 1, M.workspace_count do
    M.workspaces[i] = {}
    for _, id in ipairs(state.workspaces and state.workspaces[i] or {}) do
      local win = tomoe.window(id)
      if win then
        table.insert(M.workspaces[i], win)
        seen[id] = true
      end
    end
  end
  M.active = math.max(1, math.min(state.active or 1, M.workspace_count))
  M.fullscreen = {}
  for _, id in ipairs(state.fullscreen or {}) do
    if seen[id] then
      M.fullscreen[id] = true
    end
  end
  -- Windows the old config didn't track (it wasn't wm, or their workspace
  -- fell beyond a reduced workspace_count) join the active workspace.
  for _, win in ipairs(tomoe.windows()) do
    if not seen[win:id()] then
      table.insert(M.workspaces[M.active], win)
    end
  end
  for i = 1, M.workspace_count do
    if i ~= M.active then
      for _, win in ipairs(M.workspaces[i]) do
        win:hide()
      end
    end
  end
  M.arrange()
  -- Resync any connected bars: their wm_state snapshot predates the reload.
  announce()
end)

return M
