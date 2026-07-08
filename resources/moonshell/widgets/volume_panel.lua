-- moonshell.widgets.volume_panel (ported from nur.widgets.volume_panel)
-- A popup panel anchored to the top-right corner that appears when the
-- volume widget is clicked. Shows a volume slider (−/+ buttons + bar) and
-- the current mute state. Closes itself when clicked again.
--
-- M2 note: opening/closing requires clicks (M4) and handle:close()
-- (tracked with M4's window-visibility work) — until then only
-- :trigger() renders, as a static button shell.
--
-- Usage:
--   local VolumePanel = require("moonshell.widgets.volume_panel")
--   local vp = VolumePanel.new()
--   -- In bar render:
--   vp:trigger()   -- returns the clickable trigger element for the bar
--   -- VolumePanel manages its own popup window internally.

local M = {}
local C = require("moonshell.theme")

local PANEL_HEIGHT = 120
local PANEL_WIDTH  = 300

local function btn(label, on_click, opts)
    opts = opts or {}
    return ui.button({
        bg             = opts.bg or C.surface0,
        hover_bg       = opts.hover_bg or C.surface1,
        padding_left   = opts.px or 10,
        padding_right  = opts.px or 10,
        padding_top    = opts.py or 4,
        padding_bottom = opts.py or 4,
        on_click       = on_click,
        children       = { ui.text(label) },
    })
end

-- Build a simple visual volume bar from filled/empty blocks.
local function vol_bar(pct, width)
    width = width or 20
    local filled = math.floor(pct / 100 * width + 0.5)
    filled = math.max(0, math.min(width, filled))
    return string.rep("█", filled) .. string.rep("░", width - filled)
end

function M.new(opts)
    opts = opts or {}
    local self    = {}
    local panel   = nil   -- window handle or nil when closed
    local open    = false

    -- Open the popup window and attach its render function.
    local function open_panel()
        panel = shell.window({
            anchor         = opts.anchor or "top-right",
            height         = PANEL_HEIGHT,
            popup_width    = PANEL_WIDTH,
            margin_top     = opts.margin_top or C.bar_height,
            margin_right   = opts.margin_right or 0,
            margin_left    = opts.margin_left or 0,
            exclusive      = false,
            layer          = "overlay",
        })

        panel:render(function()
            local aud     = shell.services.audio:get()
            local vol_pct = math.floor(aud.volume * 100)
            local muted   = aud.muted
            local bar_str = vol_bar(muted and 0 or vol_pct, 22)
            local mute_label = muted and "󰖁  Unmute" or "󰕾  Mute"
            local mute_bg    = muted and C.red or C.surface0

            return ui.hbox({
                fill    = true,
                padding = 16,
                gap     = 0,
                children = {
                    ui.vbox({
                        fill = true,
                        gap  = 10,
                        children = {
                            -- Title row
                            ui.hbox({ gap = 8, children = {
                                ui.text("󰕾  Volume"),
                                ui.spacer(),
                                ui.text(muted and "muted" or (vol_pct .. "%")),
                            }}),

                            -- Volume bar
                            ui.text(bar_str),

                            -- Controls row
                            ui.hbox({ gap = 6, children = {
                                btn("−", function()
                                    local v = shell.services.audio:get()
                                    shell.services.audio:set_volume(math.max(0, v.volume - 0.05))
                                end, { px = 12, py = 4 }),

                                btn(mute_label, function()
                                    shell.services.audio:toggle_mute()
                                end, { bg = mute_bg, px = 12, py = 4 }),

                                btn("+", function()
                                    local v = shell.services.audio:get()
                                    shell.services.audio:set_volume(math.min(1.5, v.volume + 0.05))
                                end, { px = 12, py = 4 }),

                                ui.spacer(),

                                btn("✕", function()
                                    self:close()
                                end, { px = 8, py = 4 }),
                            }}),
                        },
                    }),
                },
            })
        end)
    end

    -- Toggle the panel open/closed.
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

    -- Returns the clickable bar trigger element.
    function self:trigger()
        local aud      = shell.services.audio:get()
        local vol_pct  = math.floor(aud.volume * 100)
        local icon     = aud.muted and "󰖁" or "󰕾"
        local label    = icon .. " " .. vol_pct .. "%"
        local bg       = open and C.surface1 or C.surface0

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
