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

use crate::state::Takhti;

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
    /// Call the Lua function registered for bind index `idx`.
    LuaFn(usize),
    /// Switch virtual terminal (TTY backend; Ctrl+Alt+F1..F12).
    ChangeVt(i32),
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
            _ => bail!("unknown action {s:?} (define a Lua function instead)"),
        })
    }
}

/// The modifier "Mod" resolves to in binds and pointer events
/// (`takhti.settings { mod = "alt" }`). Default: Super.
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
    /// Overlay label (third argument to `takhti.bind`).
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

impl Takhti {
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
        let scale = self.space.scale();
        let location = crate::coords::point_to_protocol(pos, scale);

        if self.lua.pointer_grab_active() {
            let prev = crate::coords::point_to_physical(pointer.current_location(), scale);
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
        // enter/leave churn (or focus theft) while a button is down.
        if !pointer.is_grabbed() {
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
                // Transient UI is dismissed by any key press, but only if it
                // was already up before this event, so the opening bind
                // itself doesn't immediately close it (and re-pressing the
                // bind toggles it away).
                let exit_dialog_was_open = self.ui.exit_dialog.is_open();
                let hotkey_overlay_was_open = self.ui.hotkey_overlay.is_open();
                // Intercept(None) swallows a key without dispatching an action.
                let action = keyboard.input::<Option<Action>, _>(
                    self,
                    key_code,
                    key_state,
                    serial,
                    time,
                    |takhti, mods, handle| {
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
                        // The exit dialog is modal: while open, no key
                        // reaches clients. Enter confirms, the rest dismiss.
                        if takhti.ui.exit_dialog.is_open() {
                            if pressed
                                && (raw_syms.contains(&Keysym::Return)
                                    || raw_syms.contains(&Keysym::KP_Enter))
                            {
                                return FilterResult::Intercept(Some(Action::ConfirmQuit));
                            }
                            return FilterResult::Intercept(None);
                        }
                        if pressed {
                            for bind in &takhti.binds {
                                if bind.mods.matches(mods) && raw_syms.contains(&bind.keysym) {
                                    return FilterResult::Intercept(Some(bind.action.clone()));
                                }
                            }
                        }
                        FilterResult::Forward
                    },
                );
                if let Some(Some(action)) = action {
                    self.do_action(action);
                }
                if pressed && exit_dialog_was_open && self.ui.exit_dialog.is_open() {
                    self.ui.exit_dialog.hide();
                    self.queue_redraw_all();
                }
                if pressed && hotkey_overlay_was_open && self.ui.hotkey_overlay.is_open() {
                    self.ui.hotkey_overlay.hide();
                    self.queue_redraw_all();
                }
            }
            InputEvent::PointerMotion { event } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return;
                };
                let scale = self.space.scale();
                let relative = RelativeMotionEvent {
                    delta: event.delta(),
                    delta_unaccel: event.delta_unaccel(),
                    utime: event.time(),
                };

                // Pointer constraints (games): an *active* constraint implies
                // its surface holds pointer focus — smithay deactivates on
                // focus change — so checking the surface under the pointer
                // suffices. A locked pointer sends only relative motion.
                let prev_pos = crate::coords::point_to_physical(pointer.current_location(), scale);
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
                let pos = crate::coords::point_to_physical(
                    pointer.current_location() + event.delta(),
                    scale,
                );
                let pos = self.clamp_to_outputs(pos);

                // A confined pointer stops at the surface (or region) edge;
                // the blocked motion still reaches the client as relative.
                if let Some(region) = confine_region {
                    let (surface, surface_loc) = under.as_ref().unwrap();
                    let new_under = self.surface_under(pos);
                    let mut prevent = new_under.as_ref().map(|(s, _)| s) != Some(surface);
                    if let Some(region) = &region {
                        let within = crate::coords::point_to_protocol(pos, scale) - *surface_loc;
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
                    let delta = crate::coords::point_to_protocol(pos, self.space.scale())
                        - pointer.current_location();
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
                    // Clicks dismiss transient UI, like any key press.
                    let mut ui_dismissed = false;
                    if self.ui.exit_dialog.is_open() {
                        self.ui.exit_dialog.hide();
                        ui_dismissed = true;
                    }
                    if self.ui.hotkey_overlay.is_open() {
                        self.ui.hotkey_overlay.hide();
                        ui_dismissed = true;
                    }
                    if ui_dismissed {
                        self.queue_redraw_all();
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

                if self.lua.has_pointer_button_hooks() {
                    let scale = self.space.scale();
                    let screen =
                        crate::coords::point_to_physical(pointer.current_location(), scale);
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

                if pressed && !pointer.is_grabbed() {
                    let screen = crate::coords::point_to_physical(
                        pointer.current_location(),
                        self.space.scale(),
                    );
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
                let horizontal = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                    event.amount_v120(Axis::Horizontal).unwrap_or(0.0) / 120.0 * 15.0
                });
                let vertical = event.amount(Axis::Vertical).unwrap_or_else(|| {
                    event.amount_v120(Axis::Vertical).unwrap_or(0.0) / 120.0 * 15.0
                });
                if self.lua.has_pointer_axis_hooks() {
                    let Some(pointer) = self.seat.get_pointer() else {
                        return;
                    };
                    let scale = self.space.scale();
                    let screen =
                        crate::coords::point_to_physical(pointer.current_location(), scale);
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
                    frame = frame.value(Axis::Horizontal, horizontal);
                }
                if vertical != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical);
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
