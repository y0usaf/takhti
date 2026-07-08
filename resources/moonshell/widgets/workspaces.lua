-- moonshell.widgets.workspaces (ported from nur.widgets.workspaces)
-- Renders a row of workspace indicators from shell.services.compositor.
--
-- Usage:
--   local Workspaces = require("moonshell.widgets.workspaces")
--   local ws = Workspaces.new()          -- occupied + active workspaces
--   local ws = Workspaces.new({ show_empty = true })  -- all of them
--   -- In a render function:
--   ws:render()

local theme = require("moonshell.theme")

local M = {}

function M.new(opts)
    opts = opts or {}

    local self = {}

    function self:render()
        local comp = shell.services.compositor:get()
        local items = {}
        for _, ws in ipairs(comp.workspaces or {}) do
            local occupied = (ws.windows or 0) > 0
            if ws.active or occupied or opts.show_empty then
                items[#items + 1] = ui.text({
                    content = ws.name or tostring(ws.id),
                    color = ws.active and (opts.active_color or theme.accent)
                        or occupied and (opts.color or theme.text)
                        or (opts.empty_color or theme.overlay0),
                })
            end
        end
        return ui.hbox({ gap = opts.gap or 4, children = items })
    end

    return self
end

return M
