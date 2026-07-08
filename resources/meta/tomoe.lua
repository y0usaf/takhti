---@meta tomoe
-- LuaLS definitions for the tomoe core API — the single source the generated
-- reference (docs/lua-api.md) is compiled from (see src/docgen.rs; a parity
-- test keeps this file in lockstep with the registered API). For editor
-- completion and type checking, add to .luarc.json:
--   { "workspace.library": ["<tomoe>/resources/meta"] }
--
-- Conventions the whole API shares:
--  * Geometry is integer physical pixels in world coordinates.
--  * Reads (windows, outputs, view, pointer) come from a snapshot taken
--    before each Lua entry; writes are queued and applied when it returns.

tomoe = {}

-- ─── Core ─────────────────────────────────────────────────────────────────────

---Apply settings; partial tables merge over previous calls (`displays` and
---`devices` are rebuilt per call). See Settings.
---@param settings Settings
function tomoe.settings(settings) end

---Bind a key combo to a Lua function or an action string: "quit" (exit
---dialog), "quit!" (exit immediately), "close-window", "show-hotkey-overlay",
---"reload-config", "screenshot" (region overlay), "screenshot-screen",
---"spawn <shell command>". Combos are "Mod+Shift+q": modifiers super|win|logo,
---alt, ctrl|control, shift, or mod (= `settings.mod`); keys are XKB keysym
---names ("Return", "equal", "F11").
---@param combo string
---@param action string|fun()
---@param desc string? # label shown in the hotkey overlay
function tomoe.bind(combo, action, desc) end

---Run a shell command, fire-and-forget (the core tracks and reaps the child).
---@param cmd string
function tomoe.spawn(cmd) end

---Ask to exit: shows the confirm dialog (the "quit!" action skips it).
function tomoe.quit() end

---Drop keyboard focus so no window receives keys.
function tomoe.clear_focus() end

-- ─── Windows ──────────────────────────────────────────────────────────────────

---All windows (mapped and unmapped), ordered by id (creation order).
---@return Window[]
function tomoe.windows() end

---Look up a window by its stable id (e.g. ids persisted through
---tomoe.on_reload); nil if it no longer exists.
---@param id integer
---@return Window?
function tomoe.window(id) end

---The keyboard-focused window, if any.
---@return Window?
function tomoe.focused_window() end

---A toplevel window. Reads reflect the snapshot taken before this Lua entry;
---write methods queue operations applied when it returns.
---@class Window
local Window = {}

---Stable numeric id, unique for the compositor session.
---@return integer
function Window:id() end

---The application id ("foot", "org.mozilla.firefox").
---@return string
function Window:app_id() end

---The window title.
---@return string
function Window:title() end

---Position and size, or nil while unmapped.
---@return Geometry?
function Window:geometry() end

---True once the window is shown on screen (initial commit done, not hidden).
---@return boolean
function Window:is_mapped() end

---True if the window has keyboard focus.
---@return boolean
function Window:is_focused() end

---xdg fullscreen state the client last acked — what the window *is*; pending
---requests arrive via on_window_request.
---@return boolean
function Window:is_fullscreen() end

---xdg maximized state the client last acked.
---@return boolean
function Window:is_maximized() end

---Move/resize (w and h are clamped to ≥ 1).
---@param x integer
---@param y integer
---@param w integer
---@param h integer
function Window:set_geometry(x, y, w, h) end

---Show the window (map it on screen).
function Window:show() end

---Hide the window without closing it (e.g. other workspaces).
function Window:hide() end

---Give the window keyboard focus.
function Window:focus() end

---Restack on top of all other windows.
function Window:raise() end

---Set the xdg Fullscreen flag. Protocol state only — pair with set_geometry
---to actually cover an output.
---@param on boolean
function Window:set_fullscreen(on) end

---Set the xdg Maximized flag; geometry stays yours.
---@param on boolean
function Window:set_maximized(on) end

---Ask the client to close (it may ignore the request).
function Window:close() end

-- ─── Outputs & camera ─────────────────────────────────────────────────────────

---Connected outputs with their geometry.
---@return Output[]
function tomoe.outputs() end

---Usable area (geometry minus layer-shell exclusive zones, e.g. bars) of the
---output at 1-based `index`; defaults to the first output.
---@param index integer?
---@return Geometry
function tomoe.usable_area(index) end

---The camera over the window canvas: screen = (world − offset) · zoom.
---@return View
function tomoe.view() end

---Move the camera; omitted fields keep their current value. zoom is clamped
---to [1/16, 16]; at zoom 1 the integer offset keeps every pixel 1:1.
---@param view View
function tomoe.set_view(view) end

---Pointer position: x, y in world coordinates, sx, sy in screen coordinates.
---@return PointerPosition
function tomoe.pointer() end

-- ─── Hooks ────────────────────────────────────────────────────────────────────

---Run `fn` when a window maps.
---@param fn fun(win: Window)
function tomoe.on_window_open(fn) end

---Run `fn` when a window closes.
---@param fn fun(win: Window)
function tomoe.on_window_close(fn) end

---Run `fn` when keyboard focus changes; win is nil when focus was cleared.
---@param fn fun(win: Window?)
function tomoe.on_focus_change(fn) end

---Run `fn` when outputs are added, removed, or reconfigured.
---@param fn fun()
function tomoe.on_outputs_changed(fn) end

---Run `fn` on pointer button events; return truthy to consume the event
---(it is not forwarded to the client under the pointer).
---@param fn fun(ev: PointerButtonEvent): boolean?
function tomoe.on_pointer_button(fn) end

---Run `fn` on scroll events; return truthy to consume.
---@param fn fun(ev: PointerAxisEvent): boolean?
function tomoe.on_pointer_axis(fn) end

---Run `fn` when a client requests a state change or an interactive drag —
---from the window's own client (xdg-shell), xdg-activation, or a taskbar
---(wlr-foreign-toplevel-management). Return truthy to consume: the consumer
---takes over responding, typically via set_fullscreen + set_geometry, or
---grab_pointer for move/resize. Unconsumed requests get the native default
---(drags are dropped, xdg-activation "activate" focuses the window,
---"urgent" is a no-op, a taskbar "close" asks the client to close).
---@param fn fun(ev: WindowRequestEvent): boolean?
function tomoe.on_window_request(fn) end

---Run `fn` when the pointer enters a window.
---@param fn fun(win: Window)
function tomoe.on_pointer_enter(fn) end

---Run `fn` when the pointer leaves a window.
---@param fn fun(win: Window)
function tomoe.on_pointer_leave(fn) end

---Decide what a screencast portal request captures (the ScreenCast portal
---asks over IPC on SelectSources). Answer by returning a selection table
---(`{ output = "DP-1" }` or `{ window = win }`) or `false` to deny — or
---call `req:defer()` and answer later with `req:resolve(sel)` /
---`req:deny()` from another callback (e.g. a tomoe.ui.menu selection): the
---portal waits, the compositor never does. Single slot: registering again
---replaces the handler. With no handler the portal falls back to its
---environment-variable heuristics. The default menu picker ships as the
---"screencast" module.
---@param fn fun(req: ScreencastRequest): ScreencastSelection|false|nil
function tomoe.on_screencast_request(fn) end

---Route pointer motion to `on_motion` (world coordinates) instead of clients
---until every button is released, then call `on_release`. Typically started
---from an on_pointer_button hook that returned true to consume the click.
---@param on_motion fun(ev: GrabEvent)
---@param on_release fun()?
function tomoe.grab_pointer(on_motion, on_release) end

---End the active grab without running its release callback.
function tomoe.ungrab_pointer() end

---Persist config state across reloads. `save` runs in the outgoing config
---just before the new one takes over and must return a JSON-compatible
---value (window handles don't survive — persist ids and look them back up
---with tomoe.window); `restore` runs in the new config with that value
---after it loads. Keyed by `name` so independent modules persist
---independently. When any restore hook runs, the core skips the
---on_window_open replay of existing windows — restored state supersedes it.
---@param name string
---@param save fun(): any
---@param restore fun(state: any)
function tomoe.on_reload(name, save, restore) end

-- ─── Window rules ────────────────────────────────────────────────────────

---Declare a window rule: matcher fields select windows, `apply` runs when a
---matching window opens (after the on_window_open hooks, so it can refine
---the WM's placement), and every other field is a data property the WM
---reads via tomoe.rules_for. Rules accumulate in declaration order.
---@param rule Rule
function tomoe.rule(rule) end

---Merge the data properties of every rule matching `win`, later
---declarations winning. Matcher fields and `apply` are excluded.
---@param win Window
---@return table<string, any>
function tomoe.rules_for(win) end

-- ─── Processes ────────────────────────────────────────────────────────────

---Declarative process manifest, diffed by id on config reload: entries are
---kept, restarted, or stopped as the diff dictates.
tomoe.process = {}

---Declare a one-shot process. Without `command`/`shell`, the id is the command.
---@param id string
---@param opts ProcessOnceOpts?
function tomoe.process.once(id, opts) end

---Declare a supervised service, restarted per its `restart` policy.
---@param id string
---@param opts ProcessServiceOpts?
function tomoe.process.service(id, opts) end

---Spawn once, imperative fire-and-forget (for event handlers).
---@param opts SpawnOpts
function tomoe.process.spawn(opts) end

-- ─── IPC ──────────────────────────────────────────────────────────────────────

---User-extensible endpoints on the compositor's JSON socket (the `tomoe msg`
---CLI). Params, results, and payloads are JSON-compatible values.
tomoe.ipc = {}

---Handle requests for `method` (e.g. `tomoe msg workspace/switch '{"n":2}'`).
---Re-registering a method overwrites the previous handler; a Lua error is
---returned to the requesting client.
---@param method string
---@param handler fun(params: any): any
function tomoe.ipc.serve(method, handler) end

---Push an event to every subscribed IPC client (`subscribe` method).
---@param event string
---@param payload any? # JSON-compatible; defaults to null
function tomoe.ipc.broadcast(event, payload) end

-- ─── Compositor UI ────────────────────────────────────────────────────────────

---Compositor-drawn retained widgets: declare one, the core renders it and
---routes input to it, and only selection events re-enter Lua. Modal widgets
---(confirm, menu) own the keyboard and swallow clicks; sheets are dismissed
---by any input; toasts expire on their own and ignore input. Widgets close
---silently (no events) on config reload and session lock. The exit dialog,
---hotkey overlay, and config-error banner are builtins on this registry.
tomoe.ui = {}

---Modal confirm dialog: Enter fires on_confirm; any other key or a click
---fires on_cancel. Returns nil (and warns) when `text` is missing.
---@param opts ConfirmOpts
---@return UiWidget?
function tomoe.ui.confirm(opts) end

---Modal menu: Up/Down (or k/j) or pointer hover navigate, Enter or a left
---click on a row fires on_select with the 1-based index and the item text,
---Esc or a click outside the menu fires on_cancel. Returns nil (and warns)
---when `items` is empty.
---@param opts MenuOpts
---@return UiWidget?
function tomoe.ui.menu(opts) end

---Transient notification stacked at the top of each output; auto-hides
---after `duration` seconds.
---@param opts ToastOpts
---@return UiWidget?
function tomoe.ui.toast(opts) end

---Non-modal overlay of (key chip, label) rows — the hotkey-overlay shape.
---Dismissed by any key press or click. Returns nil (and warns) when `rows`
---is empty.
---@param opts SheetOpts
---@return UiWidget?
function tomoe.ui.sheet(opts) end

---Handle returned by the tomoe.ui constructors. Widgets close themselves
---when they fire an event or expire; the handle removes one early.
---@class UiWidget
local UiWidget = {}

---Close the widget without firing any callback.
function UiWidget:close() end

---@class ConfirmOpts
---@field text string
---@field on_confirm fun()?
---@field on_cancel fun()?

---@class MenuOpts
---@field title string?
---@field items string[]
---@field on_select (fun(index: integer, item: string))?
---@field on_cancel fun()?

---@class ToastOpts
---@field text string
---@field duration number? # seconds (default 4)
---@field urgent boolean? # red border instead of accent

---@class SheetOpts
---@field title string?
---@field rows string[][] # { {"Mod+Q", "Quit"}, ... }

-- ─── Types ────────────────────────────────────────────────────────────────────

---A rectangle in integer physical pixels, world coordinates.
---@class Geometry
---@field x integer
---@field y integer
---@field w integer
---@field h integer

---@class Output
---@field name string # connector name ("DP-1")
---@field x integer
---@field y integer
---@field w integer
---@field h integer
---@field usable Geometry # geometry minus layer-shell exclusive zones

---@class View
---@field x integer # world offset in physical pixels
---@field y integer
---@field zoom number # 1/16 .. 16

---@class PointerPosition
---@field x number # world coordinates
---@field y number
---@field sx number # screen coordinates
---@field sy number

---@class Mods
---@field alt boolean
---@field ctrl boolean
---@field shift boolean
---@field super boolean
---@field mod boolean # whichever modifier `settings.mod` selects

---Fields shared by pointer events.
---@class PointerEvent
---@field x number # world coordinates
---@field y number
---@field sx number # screen coordinates
---@field sy number
---@field mods Mods
---@field window Window? # window under the pointer

---@class PointerButtonEvent : PointerEvent
---@field button string|integer # "left", "right", "middle", "side", "extra", "forward", "back", or a raw kernel code
---@field pressed boolean

---@class PointerAxisEvent : PointerEvent
---@field dx number # scroll delta
---@field dy number

---A client asked for a state change or an interactive drag. "activate" and
---"urgent" come from xdg-activation (another process presented a token asking
---to focus the window; "urgent" when the token had no input serial — a
---notification ping rather than a sanctioned focus steal) and from taskbars
---(wlr-foreign-toplevel-management), which can also send "close",
---"minimize", "unminimize", and the fullscreen/maximize kinds. Unconsumed,
---"activate" focuses the window natively, "close" sends the client a close
---request, and "urgent"/"minimize"/"unminimize" do nothing.
---@class WindowRequestEvent
---@field window Window
---@field type "fullscreen"|"unfullscreen"|"maximize"|"unmaximize"|"minimize"|"unminimize"|"close"|"move"|"resize"|"activate"|"urgent"
---@field output string? # the output a fullscreen request targeted
---@field edges string? # the edge/corner a resize drags, e.g. "bottom_right"

---A window rule (`tomoe.rule`). `app_id`, `title`, and `match` select
---windows — all given must match; a rule with none matches every window.
---Every field not listed here is a data property collected by
---tomoe.rules_for; the default wm module honors `workspace` (integer),
---`fullscreen` (boolean), and `focus = false`.
---@class Rule
---@field app_id string? # Lua pattern matched against the app id (anchor with ^$ for exact)
---@field title string? # Lua pattern matched against the title
---@field match (fun(win: Window): boolean)? # arbitrary predicate
---@field apply (fun(win: Window))? # runs when a matching window opens, after on_window_open hooks

---A pending screencast source request (tomoe.on_screencast_request).
---@class ScreencastRequest
---@field app_id string # the requesting application; "" when unknown
---@field types ScreencastTypes # source kinds the request allows
---@field outputs Output[] # candidate outputs
---@field windows Window[] # candidate windows (mapped toplevels)
local ScreencastRequest = {}

---Answer the request with a selection. A request answers exactly once;
---later calls are ignored with a warning.
---@param sel ScreencastSelection
function ScreencastRequest:resolve(sel) end

---Cancel the request (the application sees a cancelled dialog).
function ScreencastRequest:deny() end

---Keep the request open past the hook's return, to resolve/deny it later
---from another callback (menu selection, IPC handler). A config reload
---abandons deferred requests (the portal falls back).
function ScreencastRequest:defer() end

---@class ScreencastTypes
---@field monitor boolean
---@field window boolean

---What to cast: exactly one field.
---@class ScreencastSelection
---@field output string? # output name ("DP-1")
---@field window Window|integer? # window handle or id

---Motion event during a tomoe.grab_pointer grab.
---@class GrabEvent
---@field x number # pointer position in world coordinates
---@field y number
---@field dx number # motion delta since the last event
---@field dy number

---`tomoe.settings{...}` — every field optional.
---@class Settings
---@field gaps integer # gap between windows in physical pixels (default 8)
---@field scale number # output scale advertised to clients, snapped to N/120 (default 1.0)
---@field mod "super"|"alt"|"ctrl"|"shift" # what "Mod" means in combos and ev.mods.mod (default "super")
---@field focus_follows_mouse boolean # sloppy focus: focus the window under the pointer (default false)
---@field tearing boolean # allow async page flips for fullscreen windows that request tearing (default false)
---@field wait_for_frame_completion boolean # NVIDIA workaround: CPU-wait for rendering before queueing to KMS (default false)
---@field watchdog_ms integer # wall-clock budget of one Lua entry before the watchdog aborts it; 0 disables and restores LuaJIT compilation (default 1000)
---@field winit_size integer[] # { w, h } of the nested dev window (winit backend)
---@field border Border
---@field keyboard Keyboard
---@field displays table<string, Display> # per-output settings, keyed by connector name ("DP-1")
---@field touchpad InputDevice # class-wide touchpad settings (tty backend)
---@field mouse InputDevice # class-wide mouse settings (tty backend)
---@field devices table<string, InputDevice> # per-device overrides, keyed by libinput device name

---@class Border
---@field width integer # thickness in physical pixels; 1 is one device pixel at any scale (default 2)
---@field focused string # "#rrggbb" or "#rrggbbaa"
---@field unfocused string # "#rrggbb" or "#rrggbbaa"
---@field radius integer # window corner radius in physical pixels; 0 disables rounding, fullscreen windows never round (default 0)

---xkb keymap + key repeat. Empty strings mean the xkb defaults (including
---the XKB_DEFAULT_* environment variables).
---@class Keyboard
---@field rules string
---@field model string
---@field layout string # comma-separated layouts, e.g. "us,de"
---@field variant string # comma-separated variants, one per layout
---@field options string # xkb options, e.g. "caps:escape,grp:alt_shift_toggle"
---@field repeat_delay integer # ms a key is held before repeating (default 600)
---@field repeat_rate integer # repeats per second (default 25)

---Per-output settings (`settings.displays`, tty backend).
---@class Display
---@field resolution string # "preferred" | "max" | "WxH", optionally "@<Hz|max>" ("2560x1440@144")
---@field position integer[]? # { x, y } in physical pixels; unset outputs pack left-to-right
---@field disabled boolean # leave the connector off entirely
---@field mirror string? # show the same world region as the named output
---@field vrr boolean # variable refresh rate, when the connector supports it

---libinput device settings. Unset fields keep the device's libinput default,
---so removing a line from the config and reloading actually undoes it.
---@class InputDevice
---@field disabled boolean
---@field disabled_on_external_mouse boolean # touchpads: no events while a mouse is plugged in
---@field tap boolean
---@field tap_drag boolean
---@field tap_drag_lock boolean
---@field natural_scroll boolean
---@field accel_speed number # -1.0 (slowest) .. 1.0 (fastest)
---@field accel_profile "flat"|"adaptive"
---@field dwt boolean # disable-while-typing (touchpads)
---@field left_handed boolean
---@field middle_emulation boolean
---@field scroll_method "none"|"two_finger"|"edge"|"on_button_down"
---@field scroll_button integer # kernel button code held to scroll with "on_button_down"
---@field click_method "button_areas"|"clickfinger"

---How to launch a process: `command` as an argv array or a shell string
---(`shell` is the explicit shell form and wins over `command`).
---@class SpawnOpts
---@field command string|string[] # argv array, or a string run via the shell
---@field shell string # shell command (alternative to `command`)
---@field cwd string # working directory
---@field env table<string, string> # extra environment variables

---@class ProcessOnceOpts : SpawnOpts
---@field run "once_per_session"|"once_per_config_version" # default "once_per_session"

---@class ProcessServiceOpts : SpawnOpts
---@field restart "never"|"on_failure"|"on_exit" # default "on_exit"
---@field reload "keep_if_unchanged"|"always_restart" # what a config reload does to the running process (default "keep_if_unchanged")
