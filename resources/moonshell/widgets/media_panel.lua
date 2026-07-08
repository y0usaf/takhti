-- moonshell.widgets.media_panel (ported from nur.widgets.media_panel)
-- A popup panel anchored top-center that shows the currently playing track
-- with playback controls. Click the bar trigger to open/close.
--
-- M2 note: opening/closing requires clicks (M4) and handle:close()
-- (tracked with M4's window-visibility work) — until then only
-- :trigger() renders, as a static button shell.
--
-- Usage:
--   local MediaPanel = require("moonshell.widgets.media_panel")
--   local mp = MediaPanel.new()
--   -- In bar render:
--   mp:trigger()   -- returns the clickable trigger for the bar

local M = {}
local C = require("moonshell.theme")

local PANEL_HEIGHT = 110
local PANEL_WIDTH  = 340

local function btn(label, on_click, opts)
    opts = opts or {}
    return ui.button({
        bg             = opts.bg or C.surface0,
        hover_bg       = opts.hover_bg or C.surface1,
        padding_left   = opts.px or 10,
        padding_right  = opts.px or 10,
        padding_top    = opts.py or 5,
        padding_bottom = opts.py or 5,
        on_click       = on_click,
        children       = { ui.text(label) },
    })
end

local function truncate(s, max)
    if not s or s == "" then return "" end
    if #s <= max then return s end
    return s:sub(1, max - 1) .. "…"
end

function M.new(opts)
    opts = opts or {}
    local self  = {}
    local panel = nil
    local open  = false

    local function open_panel()
        panel = shell.window({
            anchor       = opts.anchor or "top-center",
            height       = PANEL_HEIGHT,
            popup_width  = PANEL_WIDTH,
            margin_top   = opts.margin_top or C.bar_height,
            margin_right = opts.margin_right or 0,
            margin_left  = opts.margin_left or 0,
            exclusive    = false,
            layer        = "overlay",
        })

        panel:render(function()
            local m = shell.services.mpris:get()

            if m.player_name == "" then
                return ui.hbox({
                    fill    = true,
                    padding = 16,
                    children = {
                        ui.text("No media player active"),
                        ui.spacer(),
                        btn("✕", function() self:close() end, { px = 8, py = 4 }),
                    },
                })
            end

            local play_icon  = m.status == "Playing" and "󰏤" or "󰐊"
            local title      = truncate(m.title ~= "" and m.title or m.player_name, 32)
            local artist     = truncate(m.artist, 28)
            local album      = truncate(m.album, 28)

            return ui.hbox({
                fill    = true,
                padding = 12,
                gap     = 0,
                children = {
                    ui.vbox({
                        fill = true,
                        gap  = 6,
                        children = {
                            -- Track info
                            ui.hbox({ gap = 8, children = {
                                ui.vbox({ gap = 2, children = {
                                    ui.text(title),
                                    ui.text(artist ~= "" and artist or m.player_name),
                                    album ~= "" and ui.text(album) or ui.text(""),
                                }}),
                                ui.spacer(),
                                btn("✕", function() self:close() end, { px = 8, py = 4 }),
                            }}),

                            -- Playback controls
                            ui.hbox({ gap = 6, children = {
                                btn("󰒮", function() shell.services.mpris:previous() end,   { px = 12 }),
                                btn(play_icon, function() shell.services.mpris:play_pause() end, { px = 12 }),
                                btn("󰒭", function() shell.services.mpris:next() end,        { px = 12 }),
                                btn("󰓛", function() shell.services.mpris:stop() end,        { px = 12 }),
                            }}),
                        },
                    }),
                },
            })
        end)
    end

    function self:toggle()
        if open then
            self:close()
        else
            open = true
            open_panel()
        end
    end

    function self:close()
        if panel then
            panel:close()
            panel = nil
        end
        open = false
    end

    function self:trigger()
        local m = shell.services.mpris:get()
        if m.player_name == "" then
            return ui.text("")
        end

        local play_icon = m.status == "Playing" and "󰎈" or "󰏤"
        local label     = play_icon .. "  " .. truncate(
            m.title ~= "" and m.title or m.player_name, 22
        )
        local bg = open and C.surface1 or C.surface0

        return ui.button({
            bg             = bg,
            hover_bg       = C.surface1,
            padding_left   = 10,
            padding_right  = 10,
            padding_top    = 2,
            padding_bottom = 2,
            on_click       = function() self:toggle() end,
            children       = { ui.text(label) },
        })
    end

    return self
end

return M
