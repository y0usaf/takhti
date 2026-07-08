-- moonshell.widgets.workspaces (ported from nur.widgets.workspaces)
-- Renders a row of workspace indicators from shell.services.compositor.
--
-- Usage:
--   local Workspaces = require("moonshell.widgets.workspaces")
--   local ws = Workspaces.new()
--   -- In a render function:
--   ws:render()

local M = {}

function M.new(opts)
    opts = opts or {}

    local self = {}

    function self:render()
        local comp = shell.services.compositor:get()
        local items = {}
        for _, ws in ipairs(comp.workspaces or {}) do
            items[#items + 1] = ui.text({
                content = ws.name or tostring(ws.id),
                -- TODO: highlight active workspace via style props once supported
            })
        end
        return ui.hbox({ gap = 4, children = items })
    end

    return self
end

return M
