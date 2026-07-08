-- Simple bar — Catppuccin Mocha palette.
--
-- Left:   workspaces | active window title
-- Center: media trigger (opens panel) | clock
-- Right:  CPU | RAM | network | volume trigger (opens panel) | battery

local Clock       = require("nur.widgets.clock")
local Battery     = require("nur.widgets.battery")
local Workspaces  = require("nur.widgets.workspaces")
local Network     = require("nur.widgets.network")
local VolumePanel = require("nur.widgets.volume_panel")
local MediaPanel  = require("nur.widgets.media_panel")

local clock       = Clock.new({ format = "%H:%M:%S" })
local battery     = Battery.new()
local workspaces  = Workspaces.new()
local network     = Network.new()
local vol_panel   = VolumePanel.new()
local media_panel = MediaPanel.new()

local theme = require("nur.theme")

-- To switch theme, uncomment one:
-- theme:set(theme.presets.gruvbox_dark)
-- theme:set(theme.presets.tokyo_night)
-- theme:set(theme.presets.nord)

local bar = shell.window({
    position  = "top",
    height    = theme.bar_height,
    exclusive = true,
})

bar:render(function()
    local si   = shell.services.sysinfo:get()
    local comp = shell.services.compositor:get()

    -- ── Left ──────────────────────────────────────────────────────────────
    local left = { workspaces:render() }
    if comp.active_window and comp.active_window ~= "" then
        left[#left + 1] = ui.text("  " .. comp.active_window)
    end

    -- ── Center ────────────────────────────────────────────────────────────
    local center = {
        media_panel:trigger(),
        clock:render(),
    }

    -- ── Right ─────────────────────────────────────────────────────────────
    local right = {
        ui.text("󰻠 " .. si.cpu_percent .. "%"),
        ui.text("󰍛 " .. si.memory_percent .. "%"),
        network:render(),
        vol_panel:trigger(),
        battery:render(),
    }

    return ui.bar_layout(left, center, right)
end)
