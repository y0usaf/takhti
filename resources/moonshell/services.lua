-- moonshell.services — the `shell.services.*` facade layer.
--
-- Every service is declared the same way (doctrine 05):
--
--   services.define(name, initial_state, action_names)
--
-- which yields a facade with nur's service-handle shape:
--   :get()            → current state table
--   :set(v)           → replace state (backends push snapshots here)
--   :map(fn)          → fn(state)
--   :subscribe(fn)    → fn() after every :set (no args, nur's contract)
-- plus one method per action name.
--
-- M3 status: `compositor`, `keyboard`, `battery`, `network`, and `mpris`
-- are live — native backends (crates/services) push snapshots through this
-- facade's :set() from the binary. The rest are still *placeholders*
-- — static initial state, actions warn once and do nothing — so nur
-- configs (which read shell.services.* unconditionally) run
-- unmodified. Later M3 sections replace each backing the same way;
-- the Lua-facing shape is final. Actions (focus_workspace,
-- play_pause, set_volume, …) become real once the write path lands
-- (queued service actions — with M4 interactivity, when something
-- can click them).

local M = {}

local function warn_once_fn(service, action)
    local warned = false
    return function()
        if not warned then
            warned = true
            io.stderr:write(
                ("moonshell: shell.services.%s:%s() is a placeholder until M3 — ignored\n")
                    :format(service, action)
            )
        end
    end
end

--- Declare a service facade.
---@param name string        key under shell.services
---@param initial table      initial state snapshot
---@param actions string[]?  action method names (placeholder no-ops)
function M.define(name, initial, actions)
    local facade = { _state = shell.state(initial) }

    function facade:get() return self._state:get() end
    function facade:set(v) self._state:set(v) end
    function facade:map(fn) return self._state:map(fn) end
    function facade:subscribe(fn) return self._state:subscribe(fn) end

    for _, action in ipairs(actions or {}) do
        facade[action] = warn_once_fn(name, action)
    end

    M[name] = facade
    return facade
end

-- ── Placeholder services (state shapes match nur's service structs) ─────

M.define("sysinfo", {
    cpu_percent    = 0,
    memory_percent = 0,
})

-- Backed natively since M3 §1. Snapshot shape (nur's, plus `connected`):
--   connected        boolean — IPC up (false: compositor gone/undetected)
--   active_workspace integer
--   workspaces       { { id, name, active, windows }, ... } — all of them;
--                    which to display is the widget's policy
--   active_window    string? — focused window title
M.define("compositor", {
    connected        = false,
    active_workspace = 1,
    workspaces       = {},
    active_window    = nil,
}, { "focus_workspace" })

-- Keyboard activity pulses from compositor IPC. Only coarse hand side and
-- monotonically increasing sequence cross the boundary; key values do not.
M.define("keyboard", {
    sequence = 0,
    hand     = "right",
})

-- Backed natively since M3 §3 (UPower over the system D-Bus; sysfs
-- polling fallback). Snapshot shape (nur's, plus `available`):
--   available boolean — a battery exists; false on desktops, where a
--             widget should render nothing (the bundled one does).
--             percent/charging then keep these render-safe defaults.
--   percent   integer 0–100
--   charging  boolean
M.define("battery", {
    available = false,
    percent   = 100,
    charging  = false,
})

-- Backed natively since M3 §4 (NetworkManager over the system D-Bus;
-- sysfs operstate polling fallback — link only, no ssid/strength
-- there). Snapshot shape (nur's):
--   connected boolean
--   ssid      string? — nil on ethernet, hidden SSIDs, disconnect
--   strength  integer 0–100, 0 when not on WiFi
M.define("network", {
    connected = false,
    ssid      = nil,
    strength  = 0,
})

M.define("audio", {
    volume = 1.0,
    muted  = false,
}, { "set_volume", "toggle_mute" })

-- Backed natively since M3 §4 (session D-Bus, playerctld-style
-- most-recently-active player tracking). Snapshot shape (nur's).
-- `position` is exact at status/track transitions and Seeked, frozen
-- between them — a live progress bar interpolates from its own clock.
-- Actions stay placeholders until the write path lands (M4).
M.define("mpris", {
    player_name = "",
    status      = "",
    title       = "",
    artist      = "",
    album       = "",
    art_url     = "",
    length      = 0,
    position    = 0,
    volume      = 1.0,
}, { "play_pause", "next", "previous", "stop" })

-- Backed natively since FUSION F3: the org.freedesktop.Notifications
-- daemon is hosted in-process; the active set arrives as a snapshot.
-- Popups are Lua policy (see moonshell.notifications, the builtin
-- module the default config loads — replaceable from config).
--   notifications  { { id, app, summary, body, urgent }, ... } oldest first
M.define("notifications", {
    notifications = {},
})

-- Backed natively since FUSION F3: the compositor owns the SNI
-- watcher/host. Item menus arrive with F4 interactivity.
--   items  { { service, id, title, status, icon_name }, ... }
M.define("tray", {
    items = {},
})

return M
