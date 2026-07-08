-- moonshell.theme (ported from nur's lua/nur/theme.lua)
-- Centralized design token system. Widgets read colors, spacing, and
-- typography from this module instead of hardcoding values.
--
-- Usage:
--   local theme = require("moonshell.theme")
--   ui.button({ bg = theme.surface0, hover_bg = theme.surface1, ... })
--
-- Customization (call before creating windows):
--   local theme = require("moonshell.theme")
--   theme:set({
--       base     = 0x282828,
--       text     = 0xebdbb2,
--       accent   = 0xfe8019,
--   })
--
-- Or load a preset:
--   theme:set(require("moonshell.theme").presets.gruvbox_dark)

local M = {}

-- ── Catppuccin Mocha (default) ──────────────────────────────────────────

-- Background layers (darkest → lightest)
M.base     = 0x1e1e2e
M.mantle   = 0x181825
M.crust    = 0x11111b
M.surface0 = 0x313244
M.surface1 = 0x45475a
M.surface2 = 0x585b70

-- Foreground
M.text     = 0xcdd6f4
M.subtext1 = 0xbac2de
M.subtext0 = 0xa6adc8
M.overlay2 = 0x9399b2
M.overlay1 = 0x7f849c
M.overlay0 = 0x6c7086

-- Accent palette
M.rosewater = 0xf5e0dc
M.flamingo  = 0xf2cdcd
M.pink      = 0xf5c2e7
M.mauve     = 0xcba6f7
M.red       = 0xf38ba8
M.maroon    = 0xeba0ac
M.peach     = 0xfab387
M.yellow    = 0xf9e2af
M.green     = 0xa6e3a1
M.teal      = 0x94e2d5
M.sky       = 0x89dcfe
M.sapphire  = 0x74c7ec
M.blue      = 0x89b4fa
M.lavender  = 0xb4befe

-- Semantic aliases
M.accent    = M.blue
M.success   = M.green
M.warning   = M.yellow
M.error     = M.red
M.info      = M.sapphire

-- Typography
M.font_size   = 13
M.font_family = nil  -- nil = use system monospace

-- Spacing
M.bar_height   = 32
M.bar_padding  = 12
M.widget_gap   = 8
M.panel_padding = 16

-- Window defaults (used by stdlib's shell.window wrapper)
M.window = {
    bg        = nil, -- filled by _sync_window below
    fg        = nil,
    font_size = nil,
    font_family = nil,
}

-- ── API ─────────────────────────────────────────────────────────────────

-- Keep M.window in sync with top-level tokens.
local function _sync_window()
    M.window.bg          = M.base
    M.window.fg          = M.text
    M.window.font_size   = M.font_size
    M.window.font_family = M.font_family
end

--- Merge user overrides into the theme. Nested tables (like `window`) are
--- shallow-merged so you can override individual keys.
---
---@param overrides table
function M:set(overrides)
    for k, v in pairs(overrides) do
        if k == "window" and type(v) == "table" and type(self[k]) == "table" then
            for wk, wv in pairs(v) do
                self[k][wk] = wv
            end
        else
            self[k] = v
        end
    end
    _sync_window()
end

--- Return a 0xRRGGBB hex string suitable for shell.window({ bg = ... }).
---@param color number  A 0xRRGGBB integer
---@return string
function M.hex(color)
    return string.format("#%06x", color)
end

-- Initial sync
_sync_window()

-- ── Built-in presets ────────────────────────────────────────────────────

M.presets = {}

M.presets.catppuccin_mocha = {
    base = 0x1e1e2e, mantle = 0x181825, crust = 0x11111b,
    surface0 = 0x313244, surface1 = 0x45475a, surface2 = 0x585b70,
    text = 0xcdd6f4, subtext1 = 0xbac2de, subtext0 = 0xa6adc8,
    red = 0xf38ba8, green = 0xa6e3a1, blue = 0x89b4fa,
    yellow = 0xf9e2af, mauve = 0xcba6f7, peach = 0xfab387,
    accent = 0x89b4fa, success = 0xa6e3a1, warning = 0xf9e2af, error = 0xf38ba8,
}

M.presets.catppuccin_latte = {
    base = 0xeff1f5, mantle = 0xe6e9ef, crust = 0xdce0e8,
    surface0 = 0xccd0da, surface1 = 0xbcc0cc, surface2 = 0xacb0be,
    text = 0x4c4f69, subtext1 = 0x5c5f77, subtext0 = 0x6c6f85,
    red = 0xd20f39, green = 0x40a02b, blue = 0x1e66f5,
    yellow = 0xdf8e1d, mauve = 0x8839ef, peach = 0xfe640b,
    accent = 0x1e66f5, success = 0x40a02b, warning = 0xdf8e1d, error = 0xd20f39,
}

M.presets.gruvbox_dark = {
    base = 0x282828, mantle = 0x1d2021, crust = 0x1d2021,
    surface0 = 0x3c3836, surface1 = 0x504945, surface2 = 0x665c54,
    text = 0xebdbb2, subtext1 = 0xd5c4a1, subtext0 = 0xbdae93,
    red = 0xfb4934, green = 0xb8bb26, blue = 0x83a598,
    yellow = 0xfabd2f, mauve = 0xd3869b, peach = 0xfe8019,
    accent = 0xfe8019, success = 0xb8bb26, warning = 0xfabd2f, error = 0xfb4934,
}

M.presets.tokyo_night = {
    base = 0x1a1b26, mantle = 0x16161e, crust = 0x16161e,
    surface0 = 0x292e42, surface1 = 0x3b4261, surface2 = 0x545c7e,
    text = 0xc0caf5, subtext1 = 0xa9b1d6, subtext0 = 0x9aa5ce,
    red = 0xf7768e, green = 0x9ece6a, blue = 0x7aa2f7,
    yellow = 0xe0af68, mauve = 0xbb9af7, peach = 0xff9e64,
    accent = 0x7aa2f7, success = 0x9ece6a, warning = 0xe0af68, error = 0xf7768e,
}

M.presets.nord = {
    base = 0x2e3440, mantle = 0x2e3440, crust = 0x2e3440,
    surface0 = 0x3b4252, surface1 = 0x434c5e, surface2 = 0x4c566a,
    text = 0xeceff4, subtext1 = 0xe5e9f0, subtext0 = 0xd8dee9,
    red = 0xbf616a, green = 0xa3be8c, blue = 0x81a1c1,
    yellow = 0xebcb8b, mauve = 0xb48ead, peach = 0xd08770,
    accent = 0x81a1c1, success = 0xa3be8c, warning = 0xebcb8b, error = 0xbf616a,
}

return M
