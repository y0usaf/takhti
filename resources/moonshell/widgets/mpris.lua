-- moonshell.widgets.mpris (ported from nur.widgets.mpris)
-- Displays the currently playing media track from an MPRIS player.
--
-- Usage:
--   local Mpris = require("moonshell.widgets.mpris")
--   local media = Mpris.new()
--   -- In a render function:
--   media:render()
--
-- Actions available via shell.services.mpris:
--   :play_pause()  :next()  :previous()  :stop()

local M = {}

-- Truncate a string to at most `max` characters, appending "…" if cut.
local function truncate(s, max)
    if not s or s == "" then return "" end
    if #s <= max then return s end
    return s:sub(1, max - 1) .. "…"
end

function M.new(opts)
    opts = opts or {}
    local max_len = opts.max_len or 30

    local self = {}

    function self:render()
        local m = shell.services.mpris:get()
        if m.status == "" or m.player_name == "" then
            return ui.text("")
        end

        local icon = m.status == "Playing" and "󰎈" or "󰏤"
        local label
        if m.title ~= "" and m.artist ~= "" then
            label = truncate(m.artist .. " – " .. m.title, max_len)
        elseif m.title ~= "" then
            label = truncate(m.title, max_len)
        else
            label = m.player_name
        end

        return ui.hbox({ gap = 4, children = {
            ui.text(icon),
            ui.text(label),
        }})
    end

    return self
end

return M
