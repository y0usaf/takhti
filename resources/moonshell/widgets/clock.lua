-- moonshell.widgets.clock (ported from nur.widgets.clock)
-- A reactive clock label.
--
-- Usage:
--   local Clock = require("moonshell.widgets.clock")
--   local clock = Clock.new({ format = "%H:%M" })
--   -- In a render function:
--   clock:render()

local M = {}

function M.new(opts)
    opts = opts or {}
    local format = opts.format or "%H:%M"
    local interval_ms = opts.interval or 1000

    local self = {}
    self._state = shell.state(os.date(format))

    shell.interval(interval_ms, function()
        self._state:set(os.date(format))
    end)

    function self:render()
        return ui.text({
            content = self._state:get(),
            size    = opts.size,
        })
    end

    return self
end

return M
