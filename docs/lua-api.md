# tomoe Lua API

<!-- Generated from resources/meta/tomoe.lua and the built-in modules
(resources/wm.lua, zoomer.lua, screencast.lua) by src/docgen.rs. Do not
edit; regenerate with `TOMOE_REGEN_DOCS=1 cargo test -p tomoe docgen`. -->

The config is a Lua program (`~/.config/tomoe/init.lua`, hot-reloaded on
save); the `tomoe` global is the entire core API, and all WM policy is Lua
on top of it. Geometry is integer physical pixels in world coordinates.
Reads see a snapshot taken before each Lua entry; writes are queued and
applied when it returns. Point LuaLS at `resources/meta/` for completion
and type checking in your editor.

## Core
- `tomoe.settings(settings)` — Apply settings; partial tables merge over previous calls (`displays` and `devices` are rebuilt per call). See Settings.
- `tomoe.bind(combo, action, desc)` — Bind a key combo to a Lua function or an action string: "quit" (exit dialog), "quit!" (exit immediately), "close-window", "show-hotkey-overlay", "reload-config", "screenshot" (region overlay), "screenshot-screen", "spawn <shell command>". Combos are "Mod+Shift+q": modifiers super|win|logo, alt, ctrl|control, shift, or mod (= `settings.mod`); keys are XKB keysym names ("Return", "equal", "F11").
- `tomoe.spawn(cmd)` — Run a shell command, fire-and-forget (the core tracks and reaps the child).
- `tomoe.quit()` — Ask to exit: shows the confirm dialog (the "quit!" action skips it).
- `tomoe.clear_focus()` — Drop keyboard focus so no window receives keys.

## Windows
- `tomoe.windows() -> Window[]` — All windows (mapped and unmapped), ordered by id (creation order).
- `tomoe.window(id) -> Window?` — Look up a window by its stable id (e.g. ids persisted through tomoe.on_reload); nil if it no longer exists.
- `tomoe.focused_window() -> Window?` — The keyboard-focused window, if any.

### Window

A toplevel window. Reads reflect the snapshot taken before this Lua entry; write methods queue operations applied when it returns.

- `Window:id() -> integer` — Stable numeric id, unique for the compositor session.
- `Window:app_id() -> string` — The application id ("foot", "org.mozilla.firefox").
- `Window:title() -> string` — The window title.
- `Window:geometry() -> Geometry?` — Position and size, or nil while unmapped.
- `Window:is_mapped() -> boolean` — True once the window is shown on screen (initial commit done, not hidden).
- `Window:is_focused() -> boolean` — True if the window has keyboard focus.
- `Window:is_fullscreen() -> boolean` — xdg fullscreen state the client last acked — what the window *is*; pending requests arrive via on_window_request.
- `Window:is_maximized() -> boolean` — xdg maximized state the client last acked.
- `Window:set_geometry(x, y, w, h)` — Move/resize (w and h are clamped to ≥ 1).
- `Window:show()` — Show the window (map it on screen).
- `Window:hide()` — Hide the window without closing it (e.g. other workspaces).
- `Window:focus()` — Give the window keyboard focus.
- `Window:raise()` — Restack on top of all other windows.
- `Window:set_fullscreen(on)` — Set the xdg Fullscreen flag. Protocol state only — pair with set_geometry to actually cover an output.
- `Window:set_maximized(on)` — Set the xdg Maximized flag; geometry stays yours.
- `Window:close()` — Ask the client to close (it may ignore the request).

## Outputs & camera
- `tomoe.outputs() -> Output[]` — Connected outputs with their geometry.
- `tomoe.usable_area(index) -> Geometry` — Usable area (geometry minus layer-shell exclusive zones, e.g. bars) of the output at 1-based `index`; defaults to the first output.
- `tomoe.view() -> View` — The camera over the window canvas: screen = (world − offset) · zoom.
- `tomoe.set_view(view)` — Move the camera; omitted fields keep their current value. zoom is clamped to [1/16, 16]; at zoom 1 the integer offset keeps every pixel 1:1.
- `tomoe.pointer() -> PointerPosition` — Pointer position: x, y in world coordinates, sx, sy in screen coordinates.

## Hooks
- `tomoe.on_window_open(fn)` — Run `fn` when a window maps.
- `tomoe.on_window_close(fn)` — Run `fn` when a window closes.
- `tomoe.on_focus_change(fn)` — Run `fn` when keyboard focus changes; win is nil when focus was cleared.
- `tomoe.on_outputs_changed(fn)` — Run `fn` when outputs are added, removed, or reconfigured.
- `tomoe.on_pointer_button(fn)` — Run `fn` on pointer button events; return truthy to consume the event (it is not forwarded to the client under the pointer).
- `tomoe.on_pointer_axis(fn)` — Run `fn` on scroll events; return truthy to consume.
- `tomoe.on_window_request(fn)` — Run `fn` when a client requests a state change or an interactive drag — from the window's own client (xdg-shell), xdg-activation, or a taskbar (wlr-foreign-toplevel-management). Return truthy to consume: the consumer takes over responding, typically via set_fullscreen + set_geometry, or grab_pointer for move/resize. Unconsumed requests get the native default (drags are dropped, xdg-activation "activate" focuses the window, "urgent" is a no-op, a taskbar "close" asks the client to close).
- `tomoe.on_pointer_enter(fn)` — Run `fn` when the pointer enters a window.
- `tomoe.on_pointer_leave(fn)` — Run `fn` when the pointer leaves a window.
- `tomoe.on_screencast_request(fn)` — Decide what a screencast portal request captures (the ScreenCast portal asks over IPC on SelectSources). Answer by returning a selection table (`{ output = "DP-1" }` or `{ window = win }`) or `false` to deny — or call `req:defer()` and answer later with `req:resolve(sel)` / `req:deny()` from another callback (e.g. a tomoe.ui.menu selection): the portal waits, the compositor never does. Single slot: registering again replaces the handler. With no handler the portal falls back to its environment-variable heuristics. The default menu picker ships as the "screencast" module.
- `tomoe.grab_pointer(on_motion, on_release)` — Route pointer motion to `on_motion` (world coordinates) instead of clients until every button is released, then call `on_release`. Typically started from an on_pointer_button hook that returned true to consume the click.
- `tomoe.ungrab_pointer()` — End the active grab without running its release callback.
- `tomoe.on_reload(name, save, restore)` — Persist config state across reloads. `save` runs in the outgoing config just before the new one takes over and must return a JSON-compatible value (window handles don't survive — persist ids and look them back up with tomoe.window); `restore` runs in the new config with that value after it loads. Keyed by `name` so independent modules persist independently. When any restore hook runs, the core skips the on_window_open replay of existing windows — restored state supersedes it.

## Window rules
- `tomoe.rule(rule)` — Declare a window rule: matcher fields select windows, `apply` runs when a matching window opens (after the on_window_open hooks, so it can refine the WM's placement), and every other field is a data property the WM reads via tomoe.rules_for. Rules accumulate in declaration order.
- `tomoe.rules_for(win) -> table<string, any>` — Merge the data properties of every rule matching `win`, later declarations winning. Matcher fields and `apply` are excluded.

## Processes

Declarative process manifest, diffed by id on config reload: entries are kept, restarted, or stopped as the diff dictates.
- `tomoe.process.once(id, opts)` — Declare a one-shot process. Without `command`/`shell`, the id is the command.
- `tomoe.process.service(id, opts)` — Declare a supervised service, restarted per its `restart` policy.
- `tomoe.process.spawn(opts)` — Spawn once, imperative fire-and-forget (for event handlers).

## IPC

User-extensible endpoints on the compositor's JSON socket (the `tomoe msg` CLI). Params, results, and payloads are JSON-compatible values.
- `tomoe.ipc.serve(method, handler)` — Handle requests for `method` (e.g. `tomoe msg workspace/switch '{"n":2}'`). Re-registering a method overwrites the previous handler; a Lua error is returned to the requesting client.
- `tomoe.ipc.broadcast(event, payload)` — Push an event to every subscribed IPC client (`subscribe` method).

## Compositor UI

Compositor-drawn retained widgets: declare one, the core renders it and routes input to it, and only selection events re-enter Lua. Modal widgets (confirm, menu) own the keyboard and swallow clicks; sheets are dismissed by any input; toasts expire on their own and ignore input. Widgets close silently (no events) on config reload and session lock. The exit dialog, hotkey overlay, and config-error banner are builtins on this registry.
- `tomoe.ui.confirm(opts) -> UiWidget?` — Modal confirm dialog: Enter fires on_confirm; any other key or a click fires on_cancel. Returns nil (and warns) when `text` is missing.
- `tomoe.ui.menu(opts) -> UiWidget?` — Modal menu: Up/Down (or k/j) or pointer hover navigate, Enter or a left click on a row fires on_select with the 1-based index and the item text, Esc or a click outside the menu fires on_cancel. Returns nil (and warns) when `items` is empty.
- `tomoe.ui.toast(opts) -> UiWidget?` — Transient notification stacked at the top of each output; auto-hides after `duration` seconds.
- `tomoe.ui.sheet(opts) -> UiWidget?` — Non-modal overlay of (key chip, label) rows — the hotkey-overlay shape. Dismissed by any key press or click. Returns nil (and warns) when `rows` is empty.

### UiWidget

Handle returned by the tomoe.ui constructors. Widgets close themselves when they fire an event or expire; the handle removes one early.

- `UiWidget:close()` — Close the widget without firing any callback.

### ConfirmOpts

- `text: string`
- `on_confirm: fun()?`
- `on_cancel: fun()?`

### MenuOpts

- `title: string?`
- `items: string[]`
- `on_select: (fun(index: integer, item: string))?`
- `on_cancel: fun()?`

### ToastOpts

- `text: string`
- `duration: number?` — seconds (default 4)
- `urgent: boolean?` — red border instead of accent

### SheetOpts

- `title: string?`
- `rows: string[][]` — { {"Mod+Q", "Quit"}, ... }

## Types

### Geometry

A rectangle in integer physical pixels, world coordinates.

- `x: integer`
- `y: integer`
- `w: integer`
- `h: integer`

### Output

- `name: string` — connector name ("DP-1")
- `x: integer`
- `y: integer`
- `w: integer`
- `h: integer`
- `usable: Geometry` — geometry minus layer-shell exclusive zones

### View

- `x: integer` — world offset in physical pixels
- `y: integer`
- `zoom: number` — 1/16 .. 16

### PointerPosition

- `x: number` — world coordinates
- `y: number`
- `sx: number` — screen coordinates
- `sy: number`

### Mods

- `alt: boolean`
- `ctrl: boolean`
- `shift: boolean`
- `super: boolean`
- `mod: boolean` — whichever modifier `settings.mod` selects

### PointerEvent

Fields shared by pointer events.

- `x: number` — world coordinates
- `y: number`
- `sx: number` — screen coordinates
- `sy: number`
- `mods: Mods`
- `window: Window?` — window under the pointer

### PointerButtonEvent : PointerEvent

- `button: string|integer` — "left", "right", "middle", "side", "extra", "forward", "back", or a raw kernel code
- `pressed: boolean`

### PointerAxisEvent : PointerEvent

- `dx: number` — scroll delta
- `dy: number`

### WindowRequestEvent

A client asked for a state change or an interactive drag. "activate" and "urgent" come from xdg-activation (another process presented a token asking to focus the window; "urgent" when the token had no input serial — a notification ping rather than a sanctioned focus steal) and from taskbars (wlr-foreign-toplevel-management), which can also send "close", "minimize", "unminimize", and the fullscreen/maximize kinds. Unconsumed, "activate" focuses the window natively, "close" sends the client a close request, and "urgent"/"minimize"/"unminimize" do nothing.

- `window: Window`
- `type: "fullscreen"|"unfullscreen"|"maximize"|"unmaximize"|"minimize"|"unminimize"|"close"|"move"|"resize"|"activate"|"urgent"`
- `output: string?` — the output a fullscreen request targeted
- `edges: string?` — the edge/corner a resize drags, e.g. "bottom_right"

### Rule

A window rule (`tomoe.rule`). `app_id`, `title`, and `match` select windows — all given must match; a rule with none matches every window. Every field not listed here is a data property collected by tomoe.rules_for; the default wm module honors `workspace` (integer), `fullscreen` (boolean), and `focus = false`.

- `app_id: string?` — Lua pattern matched against the app id (anchor with ^$ for exact)
- `title: string?` — Lua pattern matched against the title
- `match: (fun(win: Window): boolean)?` — arbitrary predicate
- `apply: (fun(win: Window))?` — runs when a matching window opens, after on_window_open hooks

### ScreencastRequest

A pending screencast source request (tomoe.on_screencast_request).

- `app_id: string` — the requesting application; "" when unknown
- `types: ScreencastTypes` — source kinds the request allows
- `outputs: Output[]` — candidate outputs
- `windows: Window[]` — candidate windows (mapped toplevels)
- `ScreencastRequest:resolve(sel)` — Answer the request with a selection. A request answers exactly once; later calls are ignored with a warning.
- `ScreencastRequest:deny()` — Cancel the request (the application sees a cancelled dialog).
- `ScreencastRequest:defer()` — Keep the request open past the hook's return, to resolve/deny it later from another callback (menu selection, IPC handler). A config reload abandons deferred requests (the portal falls back).

### ScreencastTypes

- `monitor: boolean`
- `window: boolean`

### ScreencastSelection

What to cast: exactly one field.

- `output: string?` — output name ("DP-1")
- `window: Window|integer?` — window handle or id

### GrabEvent

Motion event during a tomoe.grab_pointer grab.

- `x: number` — pointer position in world coordinates
- `y: number`
- `dx: number` — motion delta since the last event
- `dy: number`

### Settings

`tomoe.settings{...}` — every field optional.

- `gaps: integer` — gap between windows in physical pixels (default 8)
- `scale: number` — output scale advertised to clients, snapped to N/120 (default 1.0)
- `mod: "super"|"alt"|"ctrl"|"shift"` — what "Mod" means in combos and ev.mods.mod (default "super")
- `focus_follows_mouse: boolean` — sloppy focus: focus the window under the pointer (default false)
- `tearing: boolean` — allow async page flips for fullscreen windows that request tearing (default false)
- `wait_for_frame_completion: boolean` — NVIDIA workaround: CPU-wait for rendering before queueing to KMS (default false)
- `watchdog_ms: integer` — wall-clock budget of one Lua entry before the watchdog aborts it; 0 disables and restores LuaJIT compilation (default 1000)
- `winit_size: integer[]` — { w, h } of the nested dev window (winit backend)
- `border: Border`
- `keyboard: Keyboard`
- `displays: table<string, Display>` — per-output settings, keyed by connector name ("DP-1")
- `touchpad: InputDevice` — class-wide touchpad settings (tty backend)
- `mouse: InputDevice` — class-wide mouse settings (tty backend)
- `devices: table<string, InputDevice>` — per-device overrides, keyed by libinput device name

### Border

- `width: integer` — thickness in physical pixels; 1 is one device pixel at any scale (default 2)
- `focused: string` — "#rrggbb" or "#rrggbbaa"
- `unfocused: string` — "#rrggbb" or "#rrggbbaa"
- `radius: integer` — window corner radius in physical pixels; 0 disables rounding, fullscreen windows never round (default 0)

### Keyboard

xkb keymap + key repeat. Empty strings mean the xkb defaults (including the XKB_DEFAULT_* environment variables).

- `rules: string`
- `model: string`
- `layout: string` — comma-separated layouts, e.g. "us,de"
- `variant: string` — comma-separated variants, one per layout
- `options: string` — xkb options, e.g. "caps:escape,grp:alt_shift_toggle"
- `repeat_delay: integer` — ms a key is held before repeating (default 600)
- `repeat_rate: integer` — repeats per second (default 25)

### Display

Per-output settings (`settings.displays`, tty backend).

- `resolution: string` — "preferred" | "max" | "WxH", optionally "@<Hz|max>" ("2560x1440@144")
- `position: integer[]?` — { x, y } in physical pixels; unset outputs pack left-to-right
- `disabled: boolean` — leave the connector off entirely
- `mirror: string?` — show the same world region as the named output
- `vrr: boolean` — variable refresh rate, when the connector supports it

### InputDevice

libinput device settings. Unset fields keep the device's libinput default, so removing a line from the config and reloading actually undoes it.

- `disabled: boolean`
- `disabled_on_external_mouse: boolean` — touchpads: no events while a mouse is plugged in
- `tap: boolean`
- `tap_drag: boolean`
- `tap_drag_lock: boolean`
- `natural_scroll: boolean`
- `accel_speed: number` — -1.0 (slowest) .. 1.0 (fastest)
- `accel_profile: "flat"|"adaptive"`
- `dwt: boolean` — disable-while-typing (touchpads)
- `left_handed: boolean`
- `middle_emulation: boolean`
- `scroll_method: "none"|"two_finger"|"edge"|"on_button_down"`
- `scroll_button: integer` — kernel button code held to scroll with "on_button_down"
- `click_method: "button_areas"|"clickfinger"`

### SpawnOpts

How to launch a process: `command` as an argv array or a shell string (`shell` is the explicit shell form and wins over `command`).

- `command: string|string[]` — argv array, or a string run via the shell
- `shell: string` — shell command (alternative to `command`)
- `cwd: string` — working directory
- `env: table<string, string>` — extra environment variables

### ProcessOnceOpts : SpawnOpts

- `run: "once_per_session"|"once_per_config_version"` — default "once_per_session"

### ProcessServiceOpts : SpawnOpts

- `restart: "never"|"on_failure"|"on_exit"` — default "on_exit"
- `reload: "keep_if_unchanged"|"always_restart"` — what a config reload does to the running process (default "keep_if_unchanged")

## wm

The default window manager: classic dwindle tiling with numbered workspaces, built entirely on the public API. Preloaded as module "wm"; requiring it installs its hooks. All fields are plain data — mutate them and call `arrange()`. Honors these window-rule properties (`tomoe.rule`): `workspace = n` opens the window on workspace n, `fullscreen = true` opens it fullscreen, `focus = false` opens it without stealing focus.

- `gaps: integer` — gap between windows in physical pixels (default 8)
- `workspace_count: integer` — number of workspaces (default 9)
- `active: integer` — index of the visible workspace
- `workspaces: Window[][]` — workspaces[i] = ordered list of window objects
- `fullscreen: table<integer, true>` — fullscreen[window id] = true: excluded from tiling, covers its output

- `wm.arrange()` — Retile the active workspace (classic dwindle: split the remaining area along its longer side). Fullscreen windows keep their output-covering geometry and stay on top.
- `wm.set_fullscreen(win, on, output_name)` — Fullscreen `win` on the output containing it (or on `output_name`), or restore it back into the tiling.
- `wm.toggle_fullscreen()` — Toggle fullscreen for the focused window.
- `wm.switch(n)` — Switch to workspace `n`, focusing its most recent window.
- `wm.move_focused(n)` — Move the focused window to workspace `n`.
- `wm.focus_next()` — Focus the next window on the active workspace.
- `wm.focus_prev()` — Focus the previous window on the active workspace.
- `wm.close_focused()` — Close the focused window.

## zoomer

A zoomable-canvas WM: windows float on an infinite world canvas viewed through the compositor camera, with independent planes each remembering its own view. Preloaded as module "zoomer"; call `setup` to install its hooks and binds (drag/pan/zoom map in the file header).

- `zoomer.setup(opts) -> zoomer` — Create the planes and install hooks and default binds (Mod+drag to move/resize/pan, Mod+scroll to zoom, Mod+1..9 planes — see the header).
- `zoomer.switch_plane(n)` — Switch to plane `n`, saving the current plane's camera.
- `zoomer.next_plane(step)` — Step to the next (`step` = 1) or previous (`step` = -1) plane, wrapping.
- `zoomer.toggle_fit()` — Fit the focused window to the visible area, or restore its saved geometry.

### ZoomerOpts

Options for `zoomer.setup`; every field optional.

- `planes: integer` — number of planes (default 4)
- `zoom_step: number` — zoom factor per scroll step or keypress (default 1.2)
- `pan_step: integer` — keyboard pan distance in screen pixels (default 160)
- `open_size: number` — new windows take this fraction of the visible area (default 0.6)
- `min_size: integer` — minimum window dimension in pixels (default 64)
- `cascade: integer` — cascade offset between new windows (default 32)

## screencast

Compositor-drawn screencast source picker: answers ScreenCast portal requests with a tomoe.ui.menu of the allowed sources (single candidates resolve without asking). Preloaded as module "screencast"; requiring it installs its tomoe.on_screencast_request hook. Per-app policy composes with window rules: when a mapped window of the requesting app matches a rule carrying a `screencast` property, `screencast = false` denies the request and `screencast = "DP-1"` casts that output without asking.

