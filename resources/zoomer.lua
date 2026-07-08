-- zoomer: a floating, zooming, scrolling canvas WM.
--
-- Windows float on an infinite world-coordinate canvas viewed through the
-- compositor camera (tomoe.set_view). Planes are independent canvases,
-- each remembering its own camera.
--
--   tomoe.settings { mod = "alt" }    -- pick your Mod key (default: super)
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

---A zoomable-canvas WM: windows float on an infinite world canvas viewed
---through the compositor camera, with independent planes each remembering
---its own view. Preloaded as module "zoomer"; call `setup` to install its
---hooks and binds (drag/pan/zoom map in the file header).
---@class zoomer
local M = {}

---Options for `zoomer.setup`; every field optional.
---@class ZoomerOpts
---@field planes integer # number of planes (default 4)
---@field zoom_step number # zoom factor per scroll step or keypress (default 1.2)
---@field pan_step integer # keyboard pan distance in screen pixels (default 160)
---@field open_size number # new windows take this fraction of the visible area (default 0.6)
---@field min_size integer # minimum window dimension in pixels (default 64)
---@field cascade integer # cascade offset between new windows (default 32)
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
  local u = tomoe.usable_area()
  local v = tomoe.view()
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
    tomoe.clear_focus()
  end
end

-- ─── Planes ──────────────────────────────────────────────────────────────────

local function switch_plane(n)
  if n == current or not planes[n] then
    return
  end
  planes[current].view = tomoe.view()
  for _, win in ipairs(planes[current].wins) do
    win:hide()
  end
  current = n
  local plane = planes[current]
  for _, win in ipairs(plane.wins) do
    win:show()
  end
  tomoe.set_view(plane.view)
  focus_top(plane)
end

local function next_plane(step)
  switch_plane(((current - 1 + step) % #planes) + 1)
end

-- ─── Window lifecycle ────────────────────────────────────────────────────────

tomoe.on_window_open(function(win)
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

tomoe.on_window_close(function(win)
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
tomoe.on_focus_change(function(win)
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
  tomoe.grab_pointer(function(m)
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
  tomoe.grab_pointer(function(m)
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
  local v = tomoe.view()
  local x, y = v.x, v.y
  tomoe.grab_pointer(function(m)
    -- Canvas follows the cursor: the world point under it stays put.
    x = x - m.dx
    y = y - m.dy
    tomoe.set_view { x = round(x), y = round(y) }
  end)
end

-- Zoom keeping the world point at screen position (sx, sy) fixed.
local function zoom_around(factor, wx, wy, sx, sy)
  local v = tomoe.view()
  local zoom = clamp(v.zoom * factor, 1 / 16, 16)
  tomoe.set_view {
    x = round(wx - sx / zoom),
    y = round(wy - sy / zoom),
    zoom = zoom,
  }
end

local function zoom_center(factor)
  local u = tomoe.usable_area()
  local v = tomoe.view()
  local sx, sy = u.x + u.w / 2, u.y + u.h / 2
  zoom_around(factor, sx / v.zoom + v.x, sy / v.zoom + v.y, sx, sy)
end

-- Pan by a screen-pixel step (constant apparent speed at any zoom).
local function pan(dx, dy)
  local v = tomoe.view()
  tomoe.set_view {
    x = v.x + round(dx / v.zoom),
    y = v.y + round(dy / v.zoom),
  }
end

tomoe.on_pointer_button(function(ev)
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

tomoe.on_pointer_axis(function(ev)
  if not ev.mods.mod or ev.dy == 0 then
    return
  end
  zoom_around(ev.dy < 0 and cfg.zoom_step or 1 / cfg.zoom_step, ev.x, ev.y, ev.sx, ev.sy)
  return true
end)

-- Client-initiated drags (CSD titlebars, resize edges) reuse the same grabs.
tomoe.on_window_request(function(ev)
  if ev.type == "move" then
    begin_move(ev.window)
    return true
  elseif ev.type == "resize" then
    begin_resize(ev.window, ev.edges)
    return true
  end
end)

-- Survive config reloads: persist plane membership (window ids in stacking
-- order), per-plane cameras, and the fit-toggle geometry, and rebuild into
-- the planes the new config's setup() created.
tomoe.on_reload("zoomer", function()
  if planes[current] then
    planes[current].view = tomoe.view()
  end
  local ps = {}
  for _, plane in ipairs(planes) do
    local ids = {}
    for _, win in ipairs(plane.wins) do
      ids[#ids + 1] = win:id()
    end
    ps[#ps + 1] = { view = plane.view, wins = ids }
  end
  local fit = {}
  for id, g in pairs(fit_saved) do
    fit[#fit + 1] = { id = id, x = g.x, y = g.y, w = g.w, h = g.h }
  end
  return { current = current, planes = ps, fit = fit }
end, function(state)
  if #planes == 0 then
    -- The new config never called setup(); nothing to restore into.
    return
  end
  local seen = {}
  for i, plane in ipairs(planes) do
    local saved = state.planes and state.planes[i]
    if saved then
      if saved.view and saved.view.zoom then
        plane.view = saved.view
      end
      for _, id in ipairs(saved.wins or {}) do
        local win = tomoe.window(id)
        if win then
          table.insert(plane.wins, win)
          seen[id] = true
        end
      end
    end
  end
  current = clamp(state.current or 1, 1, #planes)
  -- Windows the old config didn't track (or whose plane fell beyond a
  -- reduced plane count) land on the current plane.
  for _, win in ipairs(tomoe.windows()) do
    if not seen[win:id()] then
      table.insert(planes[current].wins, win)
    end
  end
  fit_saved = {}
  for _, f in ipairs(state.fit or {}) do
    if seen[f.id] then
      fit_saved[f.id] = { x = f.x, y = f.y, w = f.w, h = f.h }
    end
  end
  for i, plane in ipairs(planes) do
    for _, win in ipairs(plane.wins) do
      if i == current then
        win:show()
      else
        win:hide()
      end
    end
  end
  tomoe.set_view(planes[current].view)
  focus_top(planes[current])
end)

-- ─── Fit toggle ──────────────────────────────────────────────────────────────

local function toggle_fit()
  local win = tomoe.focused_window()
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

---Create the planes and install hooks and default binds (Mod+drag to
---move/resize/pan, Mod+scroll to zoom, Mod+1..9 planes — see the header).
---@param opts ZoomerOpts?
---@return zoomer
function M.setup(opts)
  for k, v in pairs(opts or {}) do
    cfg[k] = v
  end
  planes = {}
  for _ = 1, math.max(1, cfg.planes) do
    table.insert(planes, { view = { x = 0, y = 0, zoom = 1 }, wins = {} })
  end
  current = 1

  tomoe.bind("Mod+Tab", function() next_plane(1) end, "next plane")
  tomoe.bind("Mod+Shift+Tab", function() next_plane(-1) end, "previous plane")
  for n = 1, math.min(9, #planes) do
    tomoe.bind("Mod+" .. n, function() switch_plane(n) end)
  end
  tomoe.bind("Mod+equal", function() zoom_center(cfg.zoom_step) end, "zoom in")
  tomoe.bind("Mod+minus", function() zoom_center(1 / cfg.zoom_step) end, "zoom out")
  tomoe.bind("Mod+period", function()
    tomoe.set_view { x = 0, y = 0, zoom = 1 }
  end, "reset view")
  tomoe.bind("Mod+f", toggle_fit, "fit window to view")
  tomoe.bind("Mod+Left", function() pan(-cfg.pan_step, 0) end)
  tomoe.bind("Mod+Right", function() pan(cfg.pan_step, 0) end)
  tomoe.bind("Mod+Up", function() pan(0, -cfg.pan_step) end)
  tomoe.bind("Mod+Down", function() pan(0, cfg.pan_step) end)

  return M
end

---Switch to plane `n`, saving the current plane's camera.
---@param n integer
M.switch_plane = switch_plane

---Step to the next (`step` = 1) or previous (`step` = -1) plane, wrapping.
---@param step integer
M.next_plane = next_plane

---Fit the focused window to the visible area, or restore its saved geometry.
M.toggle_fit = toggle_fit

return M
