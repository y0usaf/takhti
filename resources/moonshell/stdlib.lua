-- moonshell standard library (ported from nur's lua/nur/stdlib.lua).
-- Loaded automatically before the user's init.lua.
-- Populates the `ui` table (created empty by Rust) with pure-Lua element
-- constructors. Rust-backed native components are added separately.
--
-- The `shell`-dependent half of nur's stdlib (the theme-aware
-- shell.window wrapper, shell.services) lives in shell_ext.lua, loaded
-- after the Rust `shell.*` API is registered.

-- ---------------------------------------------------------------------------
-- Layout
-- ---------------------------------------------------------------------------

-- ui.hbox(props) / ui.hstack(props)
-- Horizontal flex row.  `props.children` is a sequential table of elements.
-- Style props: bg, border_radius, width, height, fill (bool) / grow (number),
-- gap, padding/padding_*, justify ("start"|"center"|"end"), align.
function ui.hbox(props)
    props = props or {}
    props.type = "hbox"
    return props
end
ui.hstack = ui.hbox

-- ui.vbox(props) / ui.vstack(props)
-- Vertical flex column.  Same style props as hbox.
function ui.vbox(props)
    props = props or {}
    props.type = "vbox"
    return props
end
ui.vstack = ui.vbox

-- ui.spacer()
-- A flexible gap that expands to fill available space.
function ui.spacer()
    return { type = "spacer" }
end

-- ---------------------------------------------------------------------------
-- Text & icons
-- ---------------------------------------------------------------------------

-- ui.text(content_or_props)
-- `content` may be a plain string, a reactive state (auto-read), or a props
-- table with a `content` / `text` key.
-- Style props: size, color (0xRRGGBB or "#rrggbb"), line_height (f32).
function ui.text(content_or_props)
    if type(content_or_props) == "string" then
        return { type = "text", content = content_or_props }
    elseif type(content_or_props) == "userdata" then
        -- reactive state: unwrap current value
        return { type = "text", content = tostring(content_or_props:get()) }
    else
        content_or_props.type = "text"
        return content_or_props
    end
end

-- Alias
ui.label = ui.text

-- ui.icon(name_or_props)
-- Render a named SVG icon (explicit `path` first, then XDG icon theme
-- lookup, then the name as text).
function ui.icon(name_or_props)
    if type(name_or_props) == "string" then
        return { type = "icon", name = name_or_props }
    else
        name_or_props.type = "icon"
        return name_or_props
    end
end

-- ui.button(props)
-- A clickable container.
-- Props: on_click, hover_bg, gap, padding/padding_*, plus all common style
-- props. Until M4 lands interactivity, buttons render as their visual
-- shell; click handlers are ignored.
function ui.button(props)
    props = props or {}
    props.type = "button"
    return props
end

-- ---------------------------------------------------------------------------
-- Visual elements
-- ---------------------------------------------------------------------------

-- ui.separator(props)
-- A horizontal or vertical dividing line.
-- Props: orientation ("horizontal"|"vertical"), color (u32), thickness (f32)
function ui.separator(props)
    props = props or {}
    props.type = "separator"
    return props
end

-- ui.progress_bar(props)
-- A horizontal bar showing a fill percentage.
-- Props: value (0.0-1.0), color (u32), bg (u32), height (f32),
--        border_radius (f32), width (f32, optional)
function ui.progress_bar(props)
    props = props or {}
    props.type = "progress_bar"
    return props
end

-- ui.circular_progress(props)
-- A ring-style progress indicator.
-- Props: value (0.0-1.0), size (f32), thickness (f32), color (u32),
--        track (u32)
function ui.circular_progress(props)
    props = props or {}
    props.type = "circular_progress"
    return props
end

-- ui.image(props)
-- Display an image from a file path.
-- Props: src (string), width (f32), height (f32)
function ui.image(props)
    if type(props) == "string" then
        return { type = "image", src = props }
    end
    props = props or {}
    props.type = "image"
    return props
end

-- ui.slider(props)
-- An interactive slider. Arrives with M4 interactivity.
-- Props: value (0.0-1.0), on_change (fn(value)), color (u32), bg (u32),
--        track_height (f32), thumb_size (f32), width (f32)
function ui.slider(props)
    props = props or {}
    props.type = "slider"
    return props
end

-- ui.input(props)
-- A text input field. Arrives with M5.
-- Props: value (string), placeholder (string), width (f32), height (f32),
--        font_size (f32), plus common style props
function ui.input(props)
    props = props or {}
    props.type = "input"
    return props
end

-- ---------------------------------------------------------------------------
-- Containers
-- ---------------------------------------------------------------------------

-- ui.overlay(props) / ui.stack(props)
-- Z-axis stacking of children; each child gets the full rect.
-- Props: children (table), width (f32), height (f32)
function ui.overlay(props)
    props = props or {}
    props.type = "overlay"
    return props
end
ui.stack = ui.overlay

-- ui.scroll(props)
-- A scrollable container. Arrives with M4 interactivity.
-- Props: children (table), max_height (f32),
--        direction ("vertical"|"horizontal")
function ui.scroll(props)
    props = props or {}
    props.type = "scroll"
    return props
end

-- ---------------------------------------------------------------------------
-- Conditional & list helpers
-- ---------------------------------------------------------------------------

-- ui.when(condition, element)
-- Conditional rendering. Returns the element when condition is truthy,
-- nil otherwise.  Nil children are automatically skipped by the renderer.
--
--   children = {
--       ui.text("Always visible"),
--       ui.when(battery < 20, ui.text("Low battery!")),
--   }
function ui.when(condition, element)
    if condition then
        return element
    end
    return nil
end

-- ui.map(list, fn)
-- Transform a list into element tables.  The callback receives (item, index).
-- Nil results are filtered out automatically.
--
--   children = ui.map(workspaces, function(ws, i)
--       return ui.text(ws.name)
--   end)
function ui.map(list, fn)
    local result = {}
    for i, item in ipairs(list) do
        local el = fn(item, i)
        if el ~= nil then
            result[#result + 1] = el
        end
    end
    return result
end

-- ui.fragment(children)
-- Returns the children list as-is. Use this to group multiple elements
-- without introducing a wrapper container.
function ui.fragment(children)
    return children
end

-- ---------------------------------------------------------------------------
-- Convenience helpers
-- ---------------------------------------------------------------------------

-- Build a horizontal bar section with left / center / right regions.
-- Spacing comes from the theme so bar_layout respects user customization.
function ui.bar_layout(left, center, right)
    local theme = require("moonshell.theme")
    local pad = theme.bar_padding
    local gap = theme.widget_gap
    return ui.hbox({ fill = true, padding_left = pad, padding_right = pad, children = {
        ui.hbox({ gap = gap, fill = true,                    children = left   or {} }),
        ui.hbox({ gap = gap, fill = true, justify = "center", children = center or {} }),
        ui.hbox({ gap = gap, fill = true, justify = "end",   children = right  or {} }),
    }})
end
