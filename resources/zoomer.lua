-- zoomer: a floating, zooming, scrolling canvas WM (after ~chld/shko).
--
-- Windows float on an infinite world-coordinate canvas viewed through the
-- compositor camera (takhti.set_view). Planes are independent canvases,
-- each remembering its own camera.
--
--   takhti.settings { mod = "alt" }    -- pick your Mod key (default: super)
--   require("zoomer").setup {}
--
--   Mod+left-drag    move window          Mod+Tab          next plane
--   Mod+right-drag   resize window        Mod+Shift+Tab    previous plane
--   Mod+middle-drag  pan the canvas       Mod+1..9         plane n
--   Mod+scroll       zoom around cursor   Mod+period       reset view
--   Mod+= / Mod+-    zoom around center   Mod+f            fit window to view
--   Mod+arrows       pan the canvas
--
-- Built entirely on the public API: geometry ops, raise, pointer hooks,
-- grab_pointer, window requests (CSD titlebar/edge drags start the same
-- move/resize grabs), and the view camera. Replace it wholesale if you like.

local M = {}

local cfg = {
  planes = 4,
  zoom_step = 1.2,
  -- Keyboard pan distance in screen pixels (scaled by zoom into world).
  pan_step = 160,
  -- New windows take this fraction of the visible area.
  open_size = 0.6,
  min_size = 64,
  cascade = 32,
}

local planes = {}
local current = 1
local cascade = 0
-- Saved geometry per window id for the fit toggle.
local fit_saved = {}

local function round(x)
  return math.floor(x + 0.5)
end

local function clamp(x, lo, hi)
  return math.max(lo, math.min(hi, x))
end

local function plane_of(win)
  local id = win:id()
  for p, plane in ipairs(planes) do
    for i, w in ipairs(plane.wins) do
      if w:id() == id then
        return p, i
      end
    end
  end
end

-- The world rect currently visible through the camera.
local function visible_rect()
  local u = takhti.usable_area()
  local v = takhti.view()
  return {
    x = u.x / v.zoom + v.x,
    y = u.y / v.zoom + v.y,
    w = u.w / v.zoom,
    h = u.h / v.zoom,
  }
end

local function focus_top(plane)
  local win = plane.wins[#plane.wins]
  if win then
    win:raise()
    win:focus()
  else
    takhti.clear_focus()
  end
end

-- ─── Planes ──────────────────────────────────────────────────────────────────

local function switch_plane(n)
  if n == current or not planes[n] then
    return
  end
  planes[current].view = takhti.view()
  for _, win in ipairs(planes[current].wins) do
    win:hide()
  end
  current = n
  local plane = planes[current]
  for _, win in ipairs(plane.wins) do
    win:show()
  end
  takhti.set_view(plane.view)
  focus_top(plane)
end

local function next_plane(step)
  switch_plane(((current - 1 + step) % #planes) + 1)
end

-- ─── Window lifecycle ────────────────────────────────────────────────────────

takhti.on_window_open(function(win)
  local plane = planes[current]
  if not plane then
    return
  end
  table.insert(plane.wins, win)
  local vis = visible_rect()
  local w = math.max(cfg.min_size, round(vis.w * cfg.open_size))
  local h = math.max(cfg.min_size, round(vis.h * cfg.open_size))
  local x = round(vis.x + (vis.w - w) / 2) + cascade * cfg.cascade
  local y = round(vis.y + (vis.h - h) / 2) + cascade * cfg.cascade
  cascade = (cascade + 1) % 5
  win:set_geometry(x, y, w, h)
  win:show()
  win:raise()
  win:focus()
end)

takhti.on_window_close(function(win)
  fit_saved[win:id()] = nil
  local p, i = plane_of(win)
  if not p then
    return
  end
  table.remove(planes[p].wins, i)
  if p == current then
    focus_top(planes[p])
  end
end)

-- Keep stacking bookkeeping honest with click-to-focus: the focused window
-- moves to the top of its plane's order.
takhti.on_focus_change(function(win)
  if not win then
    return
  end
  local p, i = plane_of(win)
  if p then
    local w = table.remove(planes[p].wins, i)
    table.insert(planes[p].wins, w)
  end
end)

-- ─── Mouse: drag to move/resize, pan, zoom ──────────────────────────────────

local function begin_move(win)
  local g = win:geometry()
  if not g then
    return
  end
  win:raise()
  win:focus()
  local x, y = g.x, g.y
  takhti.grab_pointer(function(m)
    x = x + m.dx
    y = y + m.dy
    win:set_geometry(round(x), round(y), g.w, g.h)
  end)
end

-- edges names which sides follow the pointer ("bottom_right", "top", ...);
-- opposite edges stay anchored. Defaults to bottom_right (Mod+right-drag).
local function begin_resize(win, edges)
  local g = win:geometry()
  if not g then
    return
  end
  win:raise()
  win:focus()
  edges = edges or "bottom_right"
  if edges == "none" then
    edges = "bottom_right"
  end
  local left = edges:find("left", 1, true) ~= nil
  local right = edges:find("right", 1, true) ~= nil
  local top = edges:find("top", 1, true) ~= nil
  local bottom = edges:find("bottom", 1, true) ~= nil
  local w, h = g.w, g.h
  takhti.grab_pointer(function(m)
    if left then
      w = w - m.dx
    elseif right then
      w = w + m.dx
    end
    if top then
      h = h - m.dy
    elseif bottom then
      h = h + m.dy
    end
    local cw = math.max(cfg.min_size, round(w))
    local ch = math.max(cfg.min_size, round(h))
    local x = left and g.x + g.w - cw or g.x
    local y = top and g.y + g.h - ch or g.y
    win:set_geometry(x, y, cw, ch)
  end)
end

local function begin_pan()
  local v = takhti.view()
  local x, y = v.x, v.y
  takhti.grab_pointer(function(m)
    -- Canvas follows the cursor: the world point under it stays put.
    x = x - m.dx
    y = y - m.dy
    takhti.set_view { x = round(x), y = round(y) }
  end)
end

-- Zoom keeping the world point at screen position (sx, sy) fixed.
local function zoom_around(factor, wx, wy, sx, sy)
  local v = takhti.view()
  local zoom = clamp(v.zoom * factor, 1 / 16, 16)
  takhti.set_view {
    x = round(wx - sx / zoom),
    y = round(wy - sy / zoom),
    zoom = zoom,
  }
end

local function zoom_center(factor)
  local u = takhti.usable_area()
  local v = takhti.view()
  local sx, sy = u.x + u.w / 2, u.y + u.h / 2
  zoom_around(factor, sx / v.zoom + v.x, sy / v.zoom + v.y, sx, sy)
end

-- Pan by a screen-pixel step (constant apparent speed at any zoom).
local function pan(dx, dy)
  local v = takhti.view()
  takhti.set_view {
    x = v.x + round(dx / v.zoom),
    y = v.y + round(dy / v.zoom),
  }
end

takhti.on_pointer_button(function(ev)
  if not ev.pressed or not ev.mods.mod then
    return
  end
  if ev.button == "left" and ev.window then
    begin_move(ev.window)
    return true
  elseif ev.button == "right" and ev.window then
    begin_resize(ev.window)
    return true
  elseif ev.button == "middle" then
    begin_pan()
    return true
  end
end)

takhti.on_pointer_axis(function(ev)
  if not ev.mods.mod or ev.dy == 0 then
    return
  end
  zoom_around(ev.dy < 0 and cfg.zoom_step or 1 / cfg.zoom_step, ev.x, ev.y, ev.sx, ev.sy)
  return true
end)

-- Client-initiated drags (CSD titlebars, resize edges) reuse the same grabs.
takhti.on_window_request(function(ev)
  if ev.type == "move" then
    begin_move(ev.window)
    return true
  elseif ev.type == "resize" then
    begin_resize(ev.window, ev.edges)
    return true
  end
end)

-- ─── Fit toggle ──────────────────────────────────────────────────────────────

local function toggle_fit()
  local win = takhti.focused_window()
  if not win then
    return
  end
  local id = win:id()
  local saved = fit_saved[id]
  if saved then
    fit_saved[id] = nil
    win:set_geometry(saved.x, saved.y, saved.w, saved.h)
  else
    local g = win:geometry()
    if not g then
      return
    end
    fit_saved[id] = g
    local vis = visible_rect()
    win:set_geometry(round(vis.x), round(vis.y), round(vis.w), round(vis.h))
    win:raise()
  end
end

-- ─── Setup ───────────────────────────────────────────────────────────────────

function M.setup(opts)
  for k, v in pairs(opts or {}) do
    cfg[k] = v
  end
  planes = {}
  for _ = 1, math.max(1, cfg.planes) do
    table.insert(planes, { view = { x = 0, y = 0, zoom = 1 }, wins = {} })
  end
  current = 1

  takhti.bind("Mod+Tab", function() next_plane(1) end, "next plane")
  takhti.bind("Mod+Shift+Tab", function() next_plane(-1) end, "previous plane")
  for n = 1, math.min(9, #planes) do
    takhti.bind("Mod+" .. n, function() switch_plane(n) end)
  end
  takhti.bind("Mod+equal", function() zoom_center(cfg.zoom_step) end, "zoom in")
  takhti.bind("Mod+minus", function() zoom_center(1 / cfg.zoom_step) end, "zoom out")
  takhti.bind("Mod+period", function()
    takhti.set_view { x = 0, y = 0, zoom = 1 }
  end, "reset view")
  takhti.bind("Mod+f", toggle_fit, "fit window to view")
  takhti.bind("Mod+Left", function() pan(-cfg.pan_step, 0) end)
  takhti.bind("Mod+Right", function() pan(cfg.pan_step, 0) end)
  takhti.bind("Mod+Up", function() pan(0, -cfg.pan_step) end)
  takhti.bind("Mod+Down", function() pan(0, cfg.pan_step) end)

  return M
end

M.switch_plane = switch_plane
M.next_plane = next_plane
M.toggle_fit = toggle_fit

return M
