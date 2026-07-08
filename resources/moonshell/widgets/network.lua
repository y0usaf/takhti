-- moonshell.widgets.network (ported from nur.widgets.network)
-- Displays network connection status, SSID, and signal strength.
--
-- Usage:
--   local Network = require("moonshell.widgets.network")
--   local net = Network.new()
--   -- In a render function:
--   net:render()

local M = {}

function M.new(opts)
    opts = opts or {}

    local self = {}

    function self:render()
        local net = shell.services.network:get()
        if not net.connected then
            return ui.text("󰤭")
        end

        local icon
        local strength = net.strength or 0
        if net.ssid then
            -- Wi-Fi: pick icon by signal strength (0–100)
            if strength >= 75 then
                icon = "󰤨"
            elseif strength >= 50 then
                icon = "󰤥"
            elseif strength >= 25 then
                icon = "󰤢"
            else
                icon = "󰤟"
            end
        else
            -- Wired
            icon = "󰈀"
        end

        local label = net.ssid or "Wired"
        return ui.hbox({ gap = 4, children = {
            ui.text(icon),
            ui.text(label),
        }})
    end

    return self
end

return M
