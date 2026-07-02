-- Default window manager for takhti, implemented entirely on the public Lua API.
-- This is the same surface user configs use: replace this module wholesale by
-- not requiring it, or extend it by wrapping its functions.
--
-- All geometry (window/output coordinates, gaps) is in *physical pixels*:
-- integers stay integers at any output scale, so layouts can never cause
-- blurry, misaligned windows. Set the scale with takhti.settings{ scale = n }.
--
--   local wm = require("wm")
--   wm.gaps = 4
--   takhti.bind("Mod+1", function() wm.switch(1) end)

local M = {
  gaps = 8,
  workspace_count = 9,
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

-- Classic dwindle: split the remaining area along its longer side.
-- Fullscreen windows keep their output-covering geometry and stay on top.
function M.arrange()
  local area = takhti.usable_area()
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
  local outs = takhti.outputs()
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

function M.toggle_fullscreen()
  local win = takhti.focused_window()
  if win then
    M.set_fullscreen(win, not M.fullscreen[win:id()])
  end
end

function M.switch(n)
  if n == M.active or n < 1 or n > M.workspace_count then
    return
  end
  for _, win in ipairs(M.workspaces[M.active]) do
    win:hide()
  end
  M.active = n
  M.arrange()
  local last = M.workspaces[n][#M.workspaces[n]]
  if last then
    last:focus()
  else
    -- Don't leave a hidden window from the old workspace holding the keyboard.
    takhti.clear_focus()
  end
end

function M.move_focused(n)
  if n == M.active or n < 1 or n > M.workspace_count then
    return
  end
  local win = takhti.focused_window()
  if not win then
    return
  end
  remove(M.workspaces[M.active], win)
  win:hide()
  table.insert(M.workspaces[n], win)
  M.arrange()
  local last = M.workspaces[M.active][#M.workspaces[M.active]]
  if last then
    last:focus()
  else
    takhti.clear_focus()
  end
end

local function cycle(dir)
  local wins = M.workspaces[M.active]
  if #wins == 0 then
    return
  end
  local focused = takhti.focused_window()
  local idx = focused and find(wins, focused) or 1
  idx = ((idx - 1 + dir) % #wins) + 1
  wins[idx]:focus()
end

function M.focus_next()
  cycle(1)
end

function M.focus_prev()
  cycle(-1)
end

function M.close_focused()
  local win = takhti.focused_window()
  if win then
    win:close()
  end
end

takhti.on_window_open(function(win)
  table.insert(M.workspaces[M.active], win)
  if win:is_fullscreen() then
    -- The client asked for fullscreen before mapping (mpv, games).
    M.set_fullscreen(win, true)
  else
    M.arrange()
  end
  win:focus()
end)

takhti.on_window_close(function(win)
  M.fullscreen[win:id()] = nil
  for i = 1, M.workspace_count do
    remove(M.workspaces[i], win)
  end
  M.arrange()
  local last = M.workspaces[M.active][#M.workspaces[M.active]]
  if last then
    last:focus()
  end
end)

-- Client state requests (F11, video players, …). Consuming the event (truthy
-- return) makes this config responsible for responding; maximize/minimize
-- fall through to the native default (ack / ignore) — a tiled layout has no
-- separate maximized or minimized state.
takhti.on_window_request(function(ev)
  if ev.type == "fullscreen" then
    M.set_fullscreen(ev.window, true, ev.output)
    return true
  elseif ev.type == "unfullscreen" then
    M.set_fullscreen(ev.window, false)
    return true
  end
end)

takhti.on_outputs_changed(function()
  M.arrange()
end)

return M
