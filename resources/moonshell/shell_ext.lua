-- moonshell shell extensions — the policy half of the `shell` global.
-- Loaded by the runtime right after the Rust `shell.*` API is
-- registered (it needs the `shell` global, so it can't live in
-- stdlib.lua, which loads at VM boot).

-- ---------------------------------------------------------------------------
-- Service facades
-- ---------------------------------------------------------------------------

-- shell.services.* — reactive service state + actions. Placeholder
-- facades until M3's native backends land; see moonshell/services.lua.
shell.services = require("moonshell.services")

-- ---------------------------------------------------------------------------
-- Theme-aware window defaults
-- ---------------------------------------------------------------------------

-- Wrap the Rust shell.window() so that omitted bg/fg/font_size/font_family
-- fall back to the theme instead of hardcoded Rust defaults.
local _raw_window = shell.window
function shell.window(config)
    local theme = require("moonshell.theme")
    config = config or {}
    if not config.bg then
        config.bg = theme.hex(theme.window.bg)
    end
    if not config.fg then
        config.fg = theme.hex(theme.window.fg)
    end
    if not config.font_size then
        config.font_size = theme.window.font_size
    end
    if not config.font_family and theme.window.font_family then
        config.font_family = theme.window.font_family
    end
    return _raw_window(config)
end
