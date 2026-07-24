use anyhow::{bail, Result};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::input::keyboard::keysyms;
use smithay::input::keyboard::xkb::{keysym_from_name, KEYSYM_CASE_INSENSITIVE, KEYSYM_NO_FLAGS};
use smithay::input::keyboard::{FilterResult, Keysym, ModifiersState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent};
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};

use crate::state::Tomoe;
use crate::ui::widgets::{UiEvent, WidgetKind};

/// Linux BTN_LEFT (input-event-codes.h).
const BTN_LEFT: u32 = 0x110;

/// Linux keycodes mapped to left bongo paw; remaining keys map right.
/// Only coarse hand classification leaves compositor IPC.
fn keyboard_hand(key_code: u32) -> &'static str {
    const LEFT_KEYS: &[u32] = &[
        1, 2, 3, 4, 5, 6, 7, 15, 16, 17, 18, 19, 20, 29, 30, 31, 32, 33, 34, 41, 42, 44, 45, 46,
        47, 48, 56, 58, 125,
    ];
    if LEFT_KEYS.contains(&key_code) {
        "left"
    } else {
        "right"
    }
}

/// A dispatchable compositor action. Lua binds queue these; built-in binds
/// carry them directly.
#[derive(Debug, Clone)]
pub enum Action {
    /// Request exit: opens the confirmation dialog.
    Quit,
    /// Exit immediately (Enter in the dialog, or the "quit!" action string).
    ConfirmQuit,
    CloseWindow,
    Spawn(String),
    ShowHotkeyOverlay,
    ReloadConfig,
    /// Interactive region screenshot (stage 2 UI; screenshots the pointer's
    /// output until the selection overlay lands).
    Screenshot,
    /// Screenshot the output under the pointer.
    ScreenshotScreen,
    /// Confirm the screenshot selection UI (internal: dispatched by the
    /// overlay's key handling, not bindable from config).
    ScreenshotConfirm,
    /// Call the Lua function registered for bind index `idx`.
    LuaFn(usize),
    /// Switch virtual terminal (TTY backend; Ctrl+Alt+F1..F12).
    ChangeVt(i32),
    /// A retained widget fired an event (internal: dispatched by the modal
    /// input routing, not bindable from config).
    UiEvent(u64, UiEvent),
}

impl Action {
    /// Parse a built-in action string from config, e.g. "close-window" or "spawn foot".
    /// Built-in action strings. Kept deliberately small: everything else is a
    /// Lua function (e.g. the default WM's `wm.switch(n)`).
    pub fn parse(s: &str) -> Result<Self> {
        if let Some(cmd) = s.strip_prefix("spawn ") {
            return Ok(Action::Spawn(cmd.trim().to_string()));
        }
        Ok(match s {
            "quit" => Action::Quit,
            "quit!" => Action::ConfirmQuit,
            "close-window" => Action::CloseWindow,
            "show-hotkey-overlay" => Action::ShowHotkeyOverlay,
            "reload-config" => Action::ReloadConfig,
            "screenshot" => Action::Screenshot,
            "screenshot-screen" => Action::ScreenshotScreen,
            _ => bail!("unknown action {s:?} (define a Lua function instead)"),
        })
    }
}

/// The modifier "Mod" resolves to in binds and pointer events
/// (`tomoe.settings { mod = "alt" }`). Default: Super.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModKey {
    #[default]
    Super,
    Alt,
    Ctrl,
    Shift,
}

impl ModKey {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "super" | "logo" | "win" => ModKey::Super,
            "alt" => ModKey::Alt,
            "ctrl" | "control" => ModKey::Ctrl,
            "shift" => ModKey::Shift,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub logo: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    fn matches(&self, state: &ModifiersState) -> bool {
        self.logo == state.logo
            && self.shift == state.shift
            && self.ctrl == state.ctrl
            && self.alt == state.alt
    }
}

pub struct Bind {
    /// The combo as written in the config, for the hotkey overlay.
    pub combo: String,
    pub mods: Mods,
    pub keysym: Keysym,
    pub action: Action,
    /// Overlay label (third argument to `tomoe.bind`).
    pub desc: Option<String>,
}

/// Parse "Super+Shift+q" style combos. "Mod" resolves to `mod_key`.
pub fn parse_combo(combo: &str, mod_key: ModKey) -> Result<(Mods, Keysym)> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let (key, mod_parts) = parts.split_last().unwrap();
    let mut mods = Mods::default();
    for part in mod_parts {
        let part = match part.to_ascii_lowercase().as_str() {
            "mod" => match mod_key {
                ModKey::Super => "super".to_string(),
                ModKey::Alt => "alt".to_string(),
                ModKey::Ctrl => "ctrl".to_string(),
                ModKey::Shift => "shift".to_string(),
            },
            other => other.to_string(),
        };
        match part.as_str() {
            "super" | "logo" | "win" => mods.logo = true,
            "shift" => mods.shift = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            other => bail!("unknown modifier {other:?}"),
        }
    }
    let mut keysym = keysym_from_name(key, KEYSYM_CASE_INSENSITIVE);
    if keysym.raw() == 0 {
        keysym = keysym_from_name(key, KEYSYM_NO_FLAGS);
    }
    if keysym.raw() == 0 {
        bail!("unknown key {key:?}");
    }
    Ok((mods, keysym))
}

impl Tomoe {
    /// Route a key to the topmost modal widget (`tomoe.ui` confirm/menu,
    /// including the builtin exit dialog). Returns None when no modal widget
    /// is open (normal key handling proceeds); Some(action) means the key is
    /// swallowed, optionally dispatching a widget event.
    fn modal_widget_key(&mut self, pressed: bool, raw_syms: &[Keysym]) -> Option<Option<Action>> {
        let mut redraw = false;
        let result = {
            let entry = self.ui.widgets.top_modal_mut()?;
            if !pressed {
                // Releases are swallowed too: the widget is modal.
                Some(None)
            } else {
                let id = entry.id;
                let enter =
                    raw_syms.contains(&Keysym::Return) || raw_syms.contains(&Keysym::KP_Enter);
                let escape = raw_syms.contains(&Keysym::Escape);
                match &mut entry.kind {
                    // Enter confirms, everything else dismisses.
                    WidgetKind::Confirm { .. } => {
                        let event = if enter {
                            UiEvent::Confirm
                        } else {
                            UiEvent::Cancel
                        };
                        Some(Some(Action::UiEvent(id, event)))
                    }
                    WidgetKind::Menu {
                        items, selected, ..
                    } => {
                        if enter && !items.is_empty() {
                            Some(Some(Action::UiEvent(id, UiEvent::Select(*selected))))
                        } else if escape {
                            Some(Some(Action::UiEvent(id, UiEvent::Cancel)))
                        } else {
                            let up =
                                raw_syms.contains(&Keysym::Up) || raw_syms.contains(&Keysym::k);
                            let down =
                                raw_syms.contains(&Keysym::Down) || raw_syms.contains(&Keysym::j);
                            let len = items.len();
                            if len > 0 && (up || down) {
                                *selected = if up {
                                    (*selected + len - 1) % len
                                } else {
                                    (*selected + 1) % len
                                };
                                entry.invalidate();
                                redraw = true;
                            }
                            Some(None)
                        }
                    }
                    // Non-modal kinds never reach here (top_modal_mut).
                    _ => Some(None),
                }
            }
        };
        if redraw {
            self.queue_redraw_all();
        }
        result
    }

    /// (alt, ctrl, shift, super) — for pointer event hooks.
    fn current_mods(&self) -> (bool, bool, bool, bool) {
        self.seat
            .get_keyboard()
            .map(|kb| {
                let m = kb.modifier_state();
                (m.alt, m.ctrl, m.shift, m.logo)
            })
            .unwrap_or_default()
    }

    /// The output under a screen-space physical point, with the point
    /// rebased to that output's local coordinates — for widget hit-testing
    /// (widgets render centered per output).
    fn output_local_point(
        &self,
        pos: smithay::utils::Point<f64, smithay::utils::Physical>,
    ) -> Option<(
        smithay::utils::Size<i32, smithay::utils::Physical>,
        smithay::utils::Point<f64, smithay::utils::Physical>,
    )> {
        for output in self.space.outputs() {
            let Some(geo) = self.space.output_geometry(output) else {
                continue;
            };
            if geo.to_f64().contains(pos) {
                return Some((geo.size, pos - geo.loc.to_f64()));
            }
        }
        None
    }

    /// Like [`Self::output_local_point`], also naming the output — the
    /// shell-surface hit path needs the per-output texture key.
    fn output_named_local_point(
        &self,
        pos: smithay::utils::Point<f64, smithay::utils::Physical>,
    ) -> Option<(String, smithay::utils::Point<f64, smithay::utils::Physical>)> {
        for output in self.space.outputs() {
            let Some(geo) = self.space.output_geometry(output) else {
                continue;
            };
            if geo.to_f64().contains(pos) {
                return Some((output.name(), pos - geo.loc.to_f64()));
            }
        }
        None
    }

    /// The pointer position in the screenshot overlay's output-local
    /// physical coordinates, clamped to the output bounds. None when the
    /// overlay is closed or its output lost its geometry.
    fn screenshot_pointer_local(
        &self,
    ) -> Option<smithay::utils::Point<i32, smithay::utils::Physical>> {
        let output = self.ui.screenshot.output()?;
        let geo = self.space.output_geometry(output)?;
        let pointer = self.seat.get_pointer()?;
        let pos = self.space.point_to_physical(pointer.current_location());
        let local = pos - geo.loc.to_f64();
        Some(smithay::utils::Point::from((
            (local.x.round() as i32).clamp(0, geo.size.w),
            (local.y.round() as i32).clamp(0, geo.size.h),
        )))
    }

    /// Common tail of relative and absolute pointer motion: route to the
    /// active Lua grab (world coordinates), or to clients via the seat.
    /// `relative` is delivered alongside the motion for relative-pointer
    /// clients (games); it is dropped on the Lua-grab path, whose focus is
    /// None anyway.
    fn pointer_moved(
        &mut self,
        pos: smithay::utils::Point<f64, smithay::utils::Physical>,
        time: u32,
        relative: Option<RelativeMotionEvent>,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let location = self.space.point_to_protocol(pos);

        // Screenshot selection overlay: the cursor keeps moving (no client
        // focus, like the Lua-grab path below) and an active drag tracks it.
        if self.ui.screenshot.is_open() {
            let serial = SERIAL_COUNTER.next_serial();
            pointer.motion(
                self,
                None,
                &MotionEvent {
                    location,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
            if self.ui.screenshot.is_dragging() {
                if let Some(local) = self.screenshot_pointer_local() {
                    self.ui.screenshot.drag_to(local);
                }
            }
            // The cursor is composited, so moving it damages the output.
            self.queue_redraw_all();
            return;
        }

        if self.lua.pointer_grab_active() && !self.is_locked() {
            let prev = self.space.point_to_physical(pointer.current_location());
            // Move the cursor but drop client focus: the drag is ours.
            let serial = SERIAL_COUNTER.next_serial();
            pointer.motion(
                self,
                None,
                &MotionEvent {
                    location,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
            let world = self.space.screen_to_world(pos);
            let prev_world = self.space.screen_to_world(prev);
            self.sync_snapshot();
            let was_in_lua = self.in_lua;
            self.in_lua = true;
            self.lua.emit_grab_motion(
                world.x,
                world.y,
                world.x - prev_world.x,
                world.y - prev_world.y,
            );
            self.in_lua = was_in_lua;
            self.after_lua();
            return;
        }

        // A modal menu tracks the pointer: hovering a row moves the
        // selection (a left click then selects it, in the button path).
        if self.ui.widgets.top_modal_id().is_some() {
            if let Some((size, local)) = self.output_local_point(pos) {
                if self.ui.hover_menu(size, local) {
                    self.queue_redraw_all();
                }
            }
        }

        let serial = SERIAL_COUNTER.next_serial();
        let under = self.surface_under(pos);
        pointer.motion(
            self,
            under.clone(),
            &MotionEvent {
                location,
                serial,
                time,
            },
        );
        if let Some(relative) = relative {
            pointer.relative_motion(self, under, &relative);
        }
        pointer.frame(self);
        // Client drags hold hover steady, like click-to-focus does: no
        // enter/leave churn (or focus theft) while a button is down. While
        // locked there is no hover: windows aren't hittable at all.
        if !pointer.is_grabbed() && !self.is_locked() {
            self.update_hover(pos);
        }
        // The motion may have landed on a surface with a pending constraint.
        self.maybe_activate_pointer_constraint();
        // The cursor is composited, so moving it damages the output.
        self.queue_redraw_all();
    }

    /// Diff the window under the pointer against the last motion: fire
    /// `on_pointer_enter`/`on_pointer_leave` on change, then apply the
    /// `focus_follows_mouse` setting.
    fn update_hover(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Physical>) {
        let world = self.space.screen_to_world(pos);
        let hovered = self.space.element_under(world).map(|(w, _)| w.clone());
        let hovered_id = hovered.as_ref().and_then(|win| {
            self.windows
                .iter()
                .find(|(_, w)| *w == win)
                .map(|(id, _)| *id)
        });
        if hovered_id == self.hovered_window {
            return;
        }
        let prev = std::mem::replace(&mut self.hovered_window, hovered_id);

        if self.lua.has_hover_hooks() {
            self.sync_snapshot();
            let was_in_lua = self.in_lua;
            self.in_lua = true;
            if let Some(prev) = prev {
                self.lua.emit_pointer_leave(prev);
            }
            if let Some(id) = hovered_id {
                self.lua.emit_pointer_enter(id);
            }
            self.in_lua = was_in_lua;
            self.after_lua();
        }

        // Sloppy focus: entering a window focuses it (without restacking);
        // leaving onto empty space or a layer surface keeps the focus.
        if self.lua.settings().focus_follows_mouse {
            if let Some(win) = hovered {
                if self.focused_window().as_ref() != Some(&win) {
                    self.focus_window_no_raise(Some(&win));
                }
            }
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        // Every real input event is user activity for idle-notify (device
        // add/remove is a hotplug, not the user at the desk).
        if !matches!(
            event,
            InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. }
        ) {
            self.notify_activity();
        }
        match event {
            InputEvent::Keyboard { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = event.time_msec();
                let key_code = event.key_code();
                let key_state = event.state();
                let Some(keyboard) = self.seat.get_keyboard() else {
                    return;
                };
                let pressed = key_state == KeyState::Pressed;
                // Transient widgets (sheets) are dismissed by any key press,
                // but only if they were already up before this event, so the
                // opening bind itself doesn't immediately close them (and
                // re-pressing the bind toggles them away via do_action).
                let dismissable_was_open = self.ui.widgets.dismissable_ids();
                // Intercept(None) swallows a key without dispatching an action.
                let action = keyboard.input::<Option<Action>, _>(
                    self,
                    key_code,
                    key_state,
                    serial,
                    time,
                    |tomoe, mods, handle| {
                        let raw_syms = handle.raw_syms();
                        if pressed {
                            // VT switching always works, even over the dialog.
                            for sym in raw_syms.iter() {
                                let raw = sym.raw();
                                if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12)
                                    .contains(&raw)
                                {
                                    let vt = (raw - keysyms::KEY_XF86Switch_VT_1 + 1) as i32;
                                    return FilterResult::Intercept(Some(Action::ChangeVt(vt)));
                                }
                            }
                        }
                        // Locked session: keys go straight to the lock
                        // surface (password entry). Binds, Lua, and the
                        // compositor UI are unreachable; only VT switching
                        // (above) still works.
                        if tomoe.is_locked() {
                            return FilterResult::Forward;
                        }
                        // The screenshot overlay is modal: Esc cancels,
                        // Enter/Space capture (Space with no selection means
                        // the whole screen), everything else — releases
                        // included — is swallowed.
                        if tomoe.ui.screenshot.is_open() {
                            if pressed {
                                if raw_syms.contains(&Keysym::Escape) {
                                    tomoe.ui.screenshot.close();
                                    tomoe.queue_redraw_all();
                                    return FilterResult::Intercept(None);
                                }
                                if raw_syms.contains(&Keysym::Return)
                                    || raw_syms.contains(&Keysym::KP_Enter)
                                    || raw_syms.contains(&Keysym::space)
                                {
                                    return FilterResult::Intercept(Some(
                                        Action::ScreenshotConfirm,
                                    ));
                                }
                            }
                            return FilterResult::Intercept(None);
                        }
                        // Modal widgets (confirm/menu, incl. the exit
                        // dialog): while one is open, no key reaches clients.
                        if let Some(action) = tomoe.modal_widget_key(pressed, &raw_syms) {
                            return FilterResult::Intercept(action);
                        }
                        if pressed {
                            for bind in &tomoe.binds {
                                if bind.mods.matches(mods) && raw_syms.contains(&bind.keysym) {
                                    return FilterResult::Intercept(Some(bind.action.clone()));
                                }
                            }
                        }
                        FilterResult::Forward
                    },
                );
                if pressed && !self.is_locked() {
                    crate::ipc::notify_keyboard_activity(self, keyboard_hand(key_code.into()));
                }
                if let Some(Some(action)) = action {
                    self.do_action(action);
                }
                if pressed {
                    let mut dismissed = false;
                    for id in dismissable_was_open {
                        dismissed |= self.close_widget(id);
                    }
                    if dismissed {
                        self.queue_redraw_all();
                    }
                }
            }
            InputEvent::PointerMotion { event } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return;
                };
                let prev_pos = self.space.point_to_physical(pointer.current_location());
                let scale = self.space.scale_at(prev_pos);
                let relative = RelativeMotionEvent {
                    delta: event.delta(),
                    delta_unaccel: event.delta_unaccel(),
                    utime: event.time(),
                };

                // Pointer constraints (games): an *active* constraint implies
                // its surface holds pointer focus — smithay deactivates on
                // focus change — so checking the surface under the pointer
                // suffices. A locked pointer sends only relative motion.

                let under = self.surface_under(prev_pos);
                let mut locked = false;
                let mut confine_region = None;
                if let Some((surface, surface_loc)) = &under {
                    with_pointer_constraint(surface, &pointer, |constraint| {
                        let Some(constraint) = constraint else { return };
                        if !constraint.is_active() {
                            return;
                        }
                        // Constraints don't apply outside their region.
                        if let Some(region) = constraint.region() {
                            let within = pointer.current_location() - *surface_loc;
                            if !region.contains(within.to_i32_round()) {
                                return;
                            }
                        }
                        match &*constraint {
                            PointerConstraint::Locked(_) => locked = true,
                            PointerConstraint::Confined(confine) => {
                                confine_region = Some(confine.region().cloned());
                            }
                        }
                    });
                }
                if locked {
                    pointer.relative_motion(self, under, &relative);
                    pointer.frame(self);
                    return;
                }

                // Deltas are logical-paced (a swipe crosses the same fraction
                // of the screen at any scale); position math is physical.
                let pos = prev_pos + crate::coords::point_to_physical(event.delta(), scale);
                let pos = self.clamp_to_outputs(pos);

                // A confined pointer stops at the surface (or region) edge;
                // the blocked motion still reaches the client as relative.
                if let Some(region) = confine_region {
                    let (surface, surface_loc) = under.as_ref().unwrap();
                    let new_under = self.surface_under(pos);
                    let mut prevent = new_under.as_ref().map(|(s, _)| s) != Some(surface);
                    if let Some(region) = &region {
                        let within = self.space.point_to_protocol(pos) - *surface_loc;
                        if !region.contains(within.to_i32_round()) {
                            prevent = true;
                        }
                    }
                    if prevent {
                        pointer.relative_motion(self, under, &relative);
                        pointer.frame(self);
                        return;
                    }
                }

                self.pointer_moved(pos, event.time_msec(), Some(relative));
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output) = self.space.outputs().next().cloned() else {
                    return;
                };
                let Some(output_geo) = self.space.output_geometry(&output) else {
                    return;
                };
                // Absolute events are normalized device coordinates; mapping
                // them over the *physical* rect lands on exact pixels (the
                // Logical type on position_transformed is smithay's, not a
                // statement about our space).
                let size = smithay::utils::Size::from((output_geo.size.w, output_geo.size.h));
                let norm = event.position_transformed(size);
                let pos: smithay::utils::Point<f64, smithay::utils::Physical> =
                    smithay::utils::Point::from((norm.x, norm.y)) + output_geo.loc.to_f64();
                // Synthesize a relative event from the position difference so
                // relative-pointer clients work on the winit backend too.
                let relative = self.seat.get_pointer().map(|pointer| {
                    let delta = self.space.point_to_protocol(pos) - pointer.current_location();
                    RelativeMotionEvent {
                        delta,
                        delta_unaccel: delta,
                        utime: event.time(),
                    }
                });
                self.pointer_moved(pos, event.time_msec(), relative);
            }
            InputEvent::PointerButton { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let Some(pointer) = self.seat.get_pointer() else {
                    return;
                };
                let button = event.button_code();
                let pressed = event.state() == ButtonState::Pressed;
                if pressed {
                    // Clicks dismiss transient widgets (sheets), like any
                    // key press.
                    let mut ui_dismissed = false;
                    for id in self.ui.widgets.dismissable_ids() {
                        ui_dismissed |= self.close_widget(id);
                    }
                    if ui_dismissed {
                        self.queue_redraw_all();
                    }
                }

                // Screenshot selection overlay: left-drag selects, clicks
                // never reach clients while it is open.
                if self.ui.screenshot.is_open() {
                    if button == BTN_LEFT {
                        if pressed {
                            if let Some(local) = self.screenshot_pointer_local() {
                                self.ui.screenshot.begin_drag(local);
                            }
                        } else {
                            self.ui.screenshot.end_drag();
                        }
                        self.queue_redraw_all();
                    }
                    return;
                }

                // Modal widgets swallow clicks entirely. A left click on a
                // menu row selects it; one elsewhere on the menu is ignored;
                // anything else cancels the topmost widget (confirms stay
                // keyboard-deliberate: Enter is the only way to confirm).
                // The matching release is swallowed via consumed_buttons if
                // the widget closed before it arrived.
                if let Some(id) = self.ui.widgets.top_modal_id() {
                    if pressed {
                        self.consumed_buttons.insert(button);
                        let hit = (button == BTN_LEFT)
                            .then(|| {
                                let pos = self.space.point_to_physical(pointer.current_location());
                                let (size, local) = self.output_local_point(pos)?;
                                self.ui.menu_click(size, local)
                            })
                            .flatten();
                        match hit {
                            Some(Some(row)) => {
                                self.do_action(Action::UiEvent(id, UiEvent::Select(row)))
                            }
                            Some(None) => {}
                            None => self.do_action(Action::UiEvent(id, UiEvent::Cancel)),
                        }
                    }
                    return;
                }

                // Native shell surfaces (FUSION F4): a press inside a
                // surface rect is consumed by the shell — the deepest
                // on_click along the element hit path fires in Lua.
                if pressed {
                    let pos = self.space.point_to_physical(pointer.current_location());
                    if let Some((name, local)) = self.output_named_local_point(pos) {
                        if let Some((shared, path)) =
                            self.shell.click_target(&name, (local.x, local.y))
                        {
                            self.consumed_buttons.insert(button);
                            if button == BTN_LEFT {
                                self.sync_snapshot();
                                let was_in_lua = self.in_lua;
                                self.in_lua = true;
                                self.lua.click_shell(&shared, &path);
                                self.in_lua = was_in_lua;
                                self.after_lua();
                            }
                            return;
                        }
                    }
                }

                // A Lua pointer grab ends on any release; presses during it
                // stay compositor-side.
                if self.lua.pointer_grab_active() {
                    if !pressed {
                        let was_in_lua = self.in_lua;
                        self.in_lua = true;
                        self.lua.end_pointer_grab();
                        self.in_lua = was_in_lua;
                        self.after_lua();
                        // A grab from a hook-consumed press was invisible to
                        // smithay and clients alike: swallow the release
                        // whole. One that took over a client-initiated drag
                        // (xdg move/resize) rode on a forwarded press, so its
                        // release must reach the seat to keep the pressed-
                        // button accounting balanced — a stale entry would
                        // make every later click grab immortal. Focus was
                        // cleared at takeover, so no client sees it.
                        if !self.consumed_buttons.remove(&button) {
                            pointer.button(
                                self,
                                &ButtonEvent {
                                    button,
                                    state: event.state(),
                                    serial,
                                    time: event.time_msec(),
                                },
                            );
                            pointer.frame(self);
                        }
                    }
                    return;
                }

                // Swallow the release matching a hook-consumed press.
                if !pressed && self.consumed_buttons.remove(&button) {
                    return;
                }

                if self.lua.has_pointer_button_hooks() && !self.is_locked() {
                    let screen = self.space.point_to_physical(pointer.current_location());
                    let world = self.space.screen_to_world(screen);
                    let window_id = self
                        .space
                        .element_under(world)
                        .map(|(w, _)| w.clone())
                        .and_then(|win| {
                            self.windows
                                .iter()
                                .find(|(_, w)| **w == win)
                                .map(|(id, _)| *id)
                        });
                    let mods = self.current_mods();
                    self.sync_snapshot();
                    let was_in_lua = self.in_lua;
                    self.in_lua = true;
                    let consumed = self.lua.emit_pointer_button(crate::lua::PointerButtonData {
                        button,
                        pressed,
                        world: (world.x, world.y),
                        screen: (screen.x, screen.y),
                        mods,
                        window: window_id,
                    });
                    self.in_lua = was_in_lua;
                    self.after_lua();
                    if consumed {
                        if pressed {
                            self.consumed_buttons.insert(button);
                        }
                        return;
                    }
                }

                if pressed && !pointer.is_grabbed() && !self.is_locked() {
                    let screen = self.space.point_to_physical(pointer.current_location());
                    let world = self.space.screen_to_world(screen);
                    let under = self.space.element_under(world).map(|(w, _)| w.clone());
                    self.focus_window(under.as_ref());
                }
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: event.state(),
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                let horizontal_v120 = event.amount_v120(Axis::Horizontal);
                let vertical_v120 = event.amount_v120(Axis::Vertical);
                let horizontal = event
                    .amount(Axis::Horizontal)
                    .unwrap_or_else(|| horizontal_v120.unwrap_or(0.0) / 120.0 * 15.0);
                let vertical = event
                    .amount(Axis::Vertical)
                    .unwrap_or_else(|| vertical_v120.unwrap_or(0.0) / 120.0 * 15.0);
                if self.lua.has_pointer_axis_hooks() && !self.is_locked() {
                    let Some(pointer) = self.seat.get_pointer() else {
                        return;
                    };
                    let screen = self.space.point_to_physical(pointer.current_location());
                    let world = self.space.screen_to_world(screen);
                    let window_id = self
                        .space
                        .element_under(world)
                        .map(|(w, _)| w.clone())
                        .and_then(|win| {
                            self.windows
                                .iter()
                                .find(|(_, w)| **w == win)
                                .map(|(id, _)| *id)
                        });
                    let mods = self.current_mods();
                    self.sync_snapshot();
                    let was_in_lua = self.in_lua;
                    self.in_lua = true;
                    let consumed = self.lua.emit_pointer_axis(crate::lua::PointerAxisData {
                        dx: horizontal,
                        dy: vertical,
                        world: (world.x, world.y),
                        screen: (screen.x, screen.y),
                        mods,
                        window: window_id,
                    });
                    self.in_lua = was_in_lua;
                    self.after_lua();
                    if consumed {
                        return;
                    }
                }
                let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
                if horizontal != 0.0 {
                    frame = frame.relative_direction(
                        Axis::Horizontal,
                        event.relative_direction(Axis::Horizontal),
                    );
                    frame = frame.value(Axis::Horizontal, horizontal);
                    if let Some(v120) = horizontal_v120 {
                        frame = frame.v120(Axis::Horizontal, v120 as i32);
                    }
                }
                if vertical != 0.0 {
                    frame = frame.relative_direction(
                        Axis::Vertical,
                        event.relative_direction(Axis::Vertical),
                    );
                    frame = frame.value(Axis::Vertical, vertical);
                    if let Some(v120) = vertical_v120 {
                        frame = frame.v120(Axis::Vertical, v120 as i32);
                    }
                }
                if event.source() == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }
                let Some(pointer) = self.seat.get_pointer() else {
                    return;
                };
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_resolves_to_configured_modifier() {
        let (mods, _) = parse_combo("Mod+Return", ModKey::Alt).unwrap();
        assert!(mods.alt && !mods.logo);
        let (mods, _) = parse_combo("Mod+Return", ModKey::Super).unwrap();
        assert!(mods.logo && !mods.alt);
        let (mods, _) = parse_combo("Mod+Shift+e", ModKey::Ctrl).unwrap();
        assert!(mods.ctrl && mods.shift);
    }

    #[test]
    fn literal_modifiers_ignore_mod_key() {
        let (mods, _) = parse_combo("Super+Shift+q", ModKey::Alt).unwrap();
        assert!(mods.logo && mods.shift && !mods.alt);
    }

    #[test]
    fn mod_key_parses_aliases() {
        assert_eq!(ModKey::parse("ALT"), Some(ModKey::Alt));
        assert_eq!(ModKey::parse("win"), Some(ModKey::Super));
        assert_eq!(ModKey::parse("control"), Some(ModKey::Ctrl));
        assert_eq!(ModKey::parse("hyper"), None);
    }
}
