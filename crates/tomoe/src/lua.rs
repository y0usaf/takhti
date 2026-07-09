//! Embedded Lua config runtime — the compositor's extension surface.
//!
//! Contract: Lua never borrows compositor state. Reads go through a snapshot
//! the core refreshes before every Lua entry; writes are queued as ops
//! (`WindowOp`) or actions and applied when the callback returns. The render
//! loop never waits on config code.
//!
//! All WM policy (workspaces, tiling, focus order) lives in Lua: the default
//! implementation ships as `resources/wm.lua`, preloaded as module `"wm"`,
//! and uses only this public API.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mlua::{
    Function, HookTriggers, Lua, LuaSerdeExt, MetaMethod, RegistryKey, Table, UserData,
    UserDataFields, UserDataMethods, Value, VmState,
};
use tracing::{info, warn};

use crate::input::Action;
use crate::process::{Launch, ProcessDecl, ProcessSpec, ReloadPolicy, RestartPolicy, RunPolicy};
use crate::ui::widgets::{self, UiEvent, WidgetSpec};

const DEFAULT_CONFIG: &str = include_str!("../../../resources/init.lua");
const WM_LUA: &str = include_str!("../../../resources/wm.lua");
const ZOOMER_LUA: &str = include_str!("../../../resources/zoomer.lua");
const SCREENCAST_LUA: &str = include_str!("../../../resources/screencast.lua");

/// Which of a connector's advertised modes to use, parsed from
/// `"<preferred|max|WxH>[@<Hz|max>]"` (e.g. "max@max", "2560x1440@144").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Resolution {
    pub size: SizeSetting,
    pub refresh: RefreshSetting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeSetting {
    /// The monitor's EDID-preferred mode. Beware: some monitors (notably
    /// super-ultrawides) advertise a conservative compatibility mode here,
    /// well below their native resolution and refresh rate.
    #[default]
    Preferred,
    /// Highest resolution by area.
    Max,
    /// Exact width and height in physical pixels.
    Exact(u16, u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshSetting {
    /// No `@` part given: the preferred mode's own refresh for
    /// `SizeSetting::Preferred`, otherwise the highest at that size.
    #[default]
    Auto,
    /// Highest refresh rate at the chosen size.
    Max,
    /// Exact rate in millihertz (wl_output units; `@144` → 144000).
    Exact(i32),
}

impl std::str::FromStr for Resolution {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let (size, refresh) = match s.split_once('@') {
            Some((size, refresh)) => (size, Some(refresh)),
            None => (s, None),
        };
        let size = match size {
            "preferred" => SizeSetting::Preferred,
            "max" => SizeSetting::Max,
            _ => {
                let (w, h) = size.split_once('x').ok_or(())?;
                SizeSetting::Exact(w.parse().map_err(drop)?, h.parse().map_err(drop)?)
            }
        };
        let refresh = match refresh {
            None => RefreshSetting::Auto,
            Some("max") => RefreshSetting::Max,
            Some(hz) => {
                let hz: f64 = hz.parse().map_err(drop)?;
                if !hz.is_finite() || hz <= 0.0 || hz > 10_000.0 {
                    return Err(());
                }
                RefreshSetting::Exact((hz * 1000.0).round() as i32)
            }
        };
        Ok(Resolution { size, refresh })
    }
}

/// Per-output settings: `settings.displays[output_name]` (tty backend).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DisplaySettings {
    pub resolution: Resolution,
    /// Explicit position in physical pixels (may be negative). Unset
    /// outputs pack left-to-right after everything placed, in connect order.
    pub position: Option<(i32, i32)>,
    /// Leave the connector off entirely: no surface, no wl_output global.
    /// Flipping it back on reconnects without a replug.
    pub disabled: bool,
    /// Show the same world region as the named output by mapping at its
    /// position (sizes may differ; no rescaling). Overrides `position`.
    /// The target must itself be an active, non-mirroring output.
    pub mirror: Option<String>,
    /// Variable refresh rate (adaptive sync). Applied when the connector
    /// supports it; live-toggleable from a settings reload.
    pub vrr: bool,
}

/// xkb keymap + key-repeat settings: `settings.keyboard`. Empty strings mean
/// the xkb defaults (including `XKB_DEFAULT_*` environment variables).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardSettings {
    pub rules: String,
    pub model: String,
    /// Comma-separated layouts, e.g. "us,de".
    pub layout: String,
    /// Comma-separated variants, one per layout.
    pub variant: String,
    /// Comma-separated xkb options, e.g. "caps:escape,grp:alt_shift_toggle".
    pub options: Option<String>,
    /// Milliseconds a key is held before it starts repeating.
    pub repeat_delay: i32,
    /// Repeats per second once repeating.
    pub repeat_rate: i32,
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            rules: String::new(),
            model: String::new(),
            layout: String::new(),
            variant: String::new(),
            options: None,
            repeat_delay: 600,
            repeat_rate: 25,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelProfile {
    Flat,
    Adaptive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollMethod {
    NoScroll,
    TwoFinger,
    Edge,
    OnButtonDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickMethod {
    ButtonAreas,
    Clickfinger,
}

/// libinput device settings: `settings.touchpad` / `settings.mouse` apply by
/// device class, `settings.devices["<libinput name>"]` overrides per device.
/// Every field is optional; unset fields use the device's libinput default,
/// so removing a line from the config and reloading actually undoes it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct InputDeviceSettings {
    pub disabled: Option<bool>,
    /// Touchpads only: send no events while an external mouse is plugged in.
    pub disabled_on_external_mouse: Option<bool>,
    pub tap: Option<bool>,
    pub tap_drag: Option<bool>,
    pub tap_drag_lock: Option<bool>,
    pub natural_scroll: Option<bool>,
    /// -1.0 (slowest) ..= 1.0 (fastest).
    pub accel_speed: Option<f64>,
    pub accel_profile: Option<AccelProfile>,
    /// Disable-while-typing (touchpads).
    pub dwt: Option<bool>,
    pub left_handed: Option<bool>,
    pub middle_emulation: Option<bool>,
    pub scroll_method: Option<ScrollMethod>,
    /// Kernel button code held to scroll with `scroll_method = "on_button_down"`.
    pub scroll_button: Option<u32>,
    pub click_method: Option<ClickMethod>,
}

impl InputDeviceSettings {
    /// Layer per-device overrides on top of class (touchpad/mouse) settings.
    pub fn overridden_by(&self, over: &Self) -> Self {
        Self {
            disabled: over.disabled.or(self.disabled),
            disabled_on_external_mouse: over
                .disabled_on_external_mouse
                .or(self.disabled_on_external_mouse),
            tap: over.tap.or(self.tap),
            tap_drag: over.tap_drag.or(self.tap_drag),
            tap_drag_lock: over.tap_drag_lock.or(self.tap_drag_lock),
            natural_scroll: over.natural_scroll.or(self.natural_scroll),
            accel_speed: over.accel_speed.or(self.accel_speed),
            accel_profile: over.accel_profile.or(self.accel_profile),
            dwt: over.dwt.or(self.dwt),
            left_handed: over.left_handed.or(self.left_handed),
            middle_emulation: over.middle_emulation.or(self.middle_emulation),
            scroll_method: over.scroll_method.or(self.scroll_method),
            scroll_button: over.scroll_button.or(self.scroll_button),
            click_method: over.click_method.or(self.click_method),
        }
    }
}

/// The libinput-relevant slice of `Settings`, grouped for change detection
/// (the tty backend re-applies it to every device only when it changed).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InputConfig {
    pub touchpad: InputDeviceSettings,
    pub mouse: InputDeviceSettings,
    /// Per-device overrides, keyed by the libinput device name.
    pub devices: HashMap<String, InputDeviceSettings>,
}

#[derive(Debug, Clone)]
pub struct Settings {
    /// Gap between windows, in physical pixels (like all Lua geometry).
    pub gaps: i32,
    /// Per-output display settings, keyed by output name ("DP-1").
    /// Unlisted outputs use their EDID-preferred mode.
    pub displays: HashMap<String, DisplaySettings>,
    /// Output scale advertised to clients (snapped to N/120 by the core).
    /// Geometry stays physical at any scale; this only changes how big
    /// clients draw their content.
    pub scale: f64,
    /// Initial size of the nested dev window (winit backend only).
    pub winit_size: (i32, i32),
    /// Border thickness in physical pixels; 1 is one device pixel at any scale.
    pub border_width: i32,
    pub border_focused: [f32; 4],
    pub border_unfocused: [f32; 4],
    /// Window corner radius in physical pixels; 0 disables rounding.
    /// Fullscreen windows never round (keeps direct scanout).
    pub corner_radius: i32,
    /// Rounded drop-shadow parameters, in physical pixels.
    pub shadow_range: i32,
    pub shadow_color: [f32; 4],
    pub shadow_power: f32,
    /// What "Mod" means in bind combos and pointer-event mods.
    pub mod_key: crate::input::ModKey,
    /// Focus the window under the pointer as it moves (sloppy focus:
    /// leaving onto empty space keeps focus). Default: click-to-focus.
    pub focus_follows_mouse: bool,
    /// Allow tearing (async page flips) for fullscreen windows that request
    /// it via wp_tearing_control_v1. Off by default: tearing is jarring
    /// unless you asked for it. Per-window overrides come with window rules.
    pub tearing: bool,
    /// Wait for rendering to finish CPU-side before queueing every frame to
    /// KMS, even when the driver could fence it (IN_FENCE_FD). Works around
    /// an NVIDIA driver bug where a fenced atomic commit queued before the
    /// render completes hangs the whole display pipeline. Costs a little
    /// latency; off by default.
    pub wait_for_frame_completion: bool,
    /// xkb keymap + key repeat, applied to the seat keyboard.
    pub keyboard: KeyboardSettings,
    /// libinput device config (tty backend).
    pub input: InputConfig,
    /// Dispatch watchdog (doctrine 02): the wall-clock budget of a single
    /// Lua entry (hook, bind, IPC handler, config load), in milliseconds.
    /// A runaway entry is aborted with a Lua error so it cannot hang the
    /// compositor. 0 disables the watchdog (and restores LuaJIT trace
    /// compilation — the enforcing debug hook keeps the VM interpreted).
    pub watchdog_ms: u64,
    /// Animation configs (springs/easing on window moves and open fades).
    /// `animations = false` turns everything off.
    pub animations: crate::animation::AnimationSettings,
}

impl Settings {
    /// Configured resolution for the named output, or the default (preferred).
    pub fn resolution_for(&self, output: &str) -> Resolution {
        self.displays
            .get(output)
            .map(|d| d.resolution)
            .unwrap_or_default()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gaps: 8,
            displays: HashMap::new(),
            scale: 1.0,
            winit_size: (1280, 800),
            border_width: 2,
            border_focused: parse_color("#7aa2f7").unwrap(),
            border_unfocused: parse_color("#3b4261").unwrap(),
            corner_radius: 0,
            shadow_range: 12,
            shadow_color: parse_color("#00000099").unwrap(),
            shadow_power: 3.,
            mod_key: crate::input::ModKey::default(),
            focus_follows_mouse: false,
            tearing: false,
            wait_for_frame_completion: false,
            keyboard: KeyboardSettings::default(),
            input: InputConfig::default(),
            watchdog_ms: 1000,
            animations: Default::default(),
        }
    }
}

/// Parse one `settings.animations.<name>` value: `false` disables it, a
/// table configures a spring (`{ spring = { damping_ratio, stiffness,
/// epsilon } }`) or an easing (`{ ease = { duration_ms, curve } }`, curve a
/// named string or `{ x1, y1, x2, y2 }` cubic-bezier control points).
fn parse_animation_config(value: &Value, label: &str) -> Option<crate::animation::Config> {
    use crate::animation::{Config, CubicBezier, Curve, SpringParams};
    match value {
        Value::Boolean(false) => Some(Config::Off),
        Value::Table(t) => {
            if let Ok(spring) = t.get::<Table>("spring") {
                let damping_ratio = spring.get::<f64>("damping_ratio").unwrap_or(1.0);
                let stiffness = spring.get::<f64>("stiffness").unwrap_or(800.0);
                let epsilon = spring.get::<f64>("epsilon").unwrap_or(0.0001);
                return Some(Config::Spring(SpringParams::new(
                    damping_ratio,
                    stiffness,
                    epsilon,
                )));
            }
            if let Ok(ease) = t.get::<Table>("ease") {
                let duration_ms = ease.get::<u64>("duration_ms").unwrap_or(150).max(1);
                let curve = match ease.get::<Value>("curve") {
                    Ok(Value::String(name)) => match name.to_string_lossy().as_ref() {
                        "linear" => Curve::Linear,
                        "ease_out_quad" => Curve::EaseOutQuad,
                        "ease_out_cubic" => Curve::EaseOutCubic,
                        "ease_out_expo" => Curve::EaseOutExpo,
                        other => {
                            warn!(
                                "{label}.ease.curve: unknown curve {other:?} (expected \
                                 \"linear\", \"ease_out_quad\", \"ease_out_cubic\", \
                                 \"ease_out_expo\", or {{x1, y1, x2, y2}}); using \
                                 ease_out_cubic"
                            );
                            Curve::EaseOutCubic
                        }
                    },
                    Ok(Value::Table(pts)) => {
                        let p = |i| pts.get::<f64>(i).unwrap_or(0.0);
                        Curve::CubicBezier(CubicBezier::new(p(1), p(2), p(3), p(4)))
                    }
                    _ => Curve::EaseOutCubic,
                };
                return Some(Config::Easing {
                    duration: Duration::from_millis(duration_ms),
                    curve,
                });
            }
            warn!("{label}: expected {{ spring = {{...}} }}, {{ ease = {{...}} }}, or false");
            None
        }
        Value::Nil => None,
        other => {
            warn!(
                "{label}: expected a table or false, got {}",
                other.type_name()
            );
            None
        }
    }
}

/// Parse a `settings.touchpad`-shaped Lua table; `label` names it in warnings.
fn parse_input_device(table: &Table, label: &str) -> InputDeviceSettings {
    let mut s = InputDeviceSettings::default();
    if let Ok(Some(v)) = table.get::<Option<bool>>("disabled") {
        s.disabled = Some(v);
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("disabled_on_external_mouse") {
        s.disabled_on_external_mouse = Some(v);
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("tap") {
        s.tap = Some(v);
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("tap_drag") {
        s.tap_drag = Some(v);
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("tap_drag_lock") {
        s.tap_drag_lock = Some(v);
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("natural_scroll") {
        s.natural_scroll = Some(v);
    }
    if let Ok(v) = table.get::<f64>("accel_speed") {
        if v.is_finite() {
            s.accel_speed = Some(v.clamp(-1.0, 1.0));
        } else {
            warn!("{label}.accel_speed: expected a number in -1..=1");
        }
    }
    if let Ok(v) = table.get::<String>("accel_profile") {
        match v.as_str() {
            "flat" => s.accel_profile = Some(AccelProfile::Flat),
            "adaptive" => s.accel_profile = Some(AccelProfile::Adaptive),
            _ => warn!(
                "{label}.accel_profile: unknown profile {v:?} \
                 (expected \"flat\" or \"adaptive\")"
            ),
        }
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("dwt") {
        s.dwt = Some(v);
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("left_handed") {
        s.left_handed = Some(v);
    }
    if let Ok(Some(v)) = table.get::<Option<bool>>("middle_emulation") {
        s.middle_emulation = Some(v);
    }
    if let Ok(v) = table.get::<String>("scroll_method") {
        match v.as_str() {
            "none" => s.scroll_method = Some(ScrollMethod::NoScroll),
            "two_finger" => s.scroll_method = Some(ScrollMethod::TwoFinger),
            "edge" => s.scroll_method = Some(ScrollMethod::Edge),
            "on_button_down" => s.scroll_method = Some(ScrollMethod::OnButtonDown),
            _ => warn!(
                "{label}.scroll_method: unknown method {v:?} (expected \"none\", \
                 \"two_finger\", \"edge\", or \"on_button_down\")"
            ),
        }
    }
    if let Ok(v) = table.get::<u32>("scroll_button") {
        s.scroll_button = Some(v);
    }
    if let Ok(v) = table.get::<String>("click_method") {
        match v.as_str() {
            "button_areas" => s.click_method = Some(ClickMethod::ButtonAreas),
            "clickfinger" => s.click_method = Some(ClickMethod::Clickfinger),
            _ => warn!(
                "{label}.click_method: unknown method {v:?} \
                 (expected \"button_areas\" or \"clickfinger\")"
            ),
        }
    }
    s
}

/// Parse "#rrggbb" or "#rrggbbaa" into premultiplied RGBA floats.
pub fn parse_color(s: &str) -> Option<[f32; 4]> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    let parse = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        6 => Some([
            parse(0)? as f32 / 255.0,
            parse(2)? as f32 / 255.0,
            parse(4)? as f32 / 255.0,
            1.0,
        ]),
        8 => {
            let a = parse(6)? as f32 / 255.0;
            Some([
                parse(0)? as f32 / 255.0 * a,
                parse(2)? as f32 / 255.0 * a,
                parse(4)? as f32 / 255.0 * a,
                a,
            ])
        }
        _ => None,
    }
}

/// Parse a `tomoe.process` spec table: `command` (string → shell, array →
/// argv) or `shell` (string), plus `cwd` and `env`. When neither command
/// form is given, `default_cmd` (the manifest id) is the argv — so
/// `tomoe.process.service("waybar", { restart = "on_exit" })` just works.
fn parse_process_spec(
    table: Option<&Table>,
    default_cmd: Option<&str>,
    label: &str,
) -> Option<ProcessSpec> {
    let mut launch = None;
    if let Some(t) = table {
        if let Ok(Value::String(s)) = t.get::<Value>("shell") {
            launch = Some(Launch::Shell(s.to_string_lossy()));
        }
        if launch.is_none() {
            match t.get::<Value>("command") {
                Ok(Value::String(s)) => launch = Some(Launch::Shell(s.to_string_lossy())),
                Ok(Value::Table(arr)) => {
                    let mut argv = Vec::new();
                    for v in arr.sequence_values::<String>() {
                        match v {
                            Ok(s) => argv.push(s),
                            Err(_) => {
                                warn!("{label}.command: expected an array of strings");
                                return None;
                            }
                        }
                    }
                    if argv.is_empty() {
                        warn!("{label}.command: must not be empty");
                        return None;
                    }
                    launch = Some(Launch::Argv(argv));
                }
                Ok(Value::Nil) | Err(_) => {}
                Ok(other) => {
                    warn!(
                        "{label}.command: expected a string or array of strings, got {}",
                        other.type_name()
                    );
                    return None;
                }
            }
        }
    }
    let launch = match (launch, default_cmd) {
        (Some(launch), _) => launch,
        (None, Some(cmd)) => Launch::Argv(vec![cmd.to_string()]),
        (None, None) => {
            warn!("{label}: missing `command` (or `shell`)");
            return None;
        }
    };
    let mut spec = ProcessSpec {
        launch,
        cwd: None,
        env: Default::default(),
    };
    if let Some(t) = table {
        if let Ok(cwd) = t.get::<String>("cwd") {
            spec.cwd = Some(PathBuf::from(cwd));
        }
        if let Ok(env) = t.get::<Table>("env") {
            for pair in env.pairs::<String, String>() {
                match pair {
                    Ok((key, value)) => {
                        spec.env.insert(key, value);
                    }
                    Err(_) => warn!("{label}.env: expected string keys and values"),
                }
            }
        }
    }
    Some(spec)
}

/// Parse a policy-string field, accepting `-` or `_` as the separator.
/// Missing/nil yields the default; an unknown value warns and defaults.
fn parse_policy_field<T: Default>(
    table: &Table,
    field: &str,
    label: &str,
    parse: impl Fn(&str) -> Option<T>,
    expected: &str,
) -> T {
    match table.get::<Value>(field) {
        Ok(Value::String(s)) => {
            let normalized = s.to_string_lossy().replace('-', "_");
            parse(&normalized).unwrap_or_else(|| {
                warn!("{label}.{field}: unknown value {normalized:?} (expected {expected})");
                T::default()
            })
        }
        Ok(Value::Nil) | Err(_) => T::default(),
        Ok(other) => {
            warn!(
                "{label}.{field}: expected a string, got {}",
                other.type_name()
            );
            T::default()
        }
    }
}

// ─── Snapshot (Lua-readable state) ────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct WinProps {
    pub app_id: String,
    pub title: String,
    /// (x, y, w, h) in global *physical pixel* coordinates; None while
    /// unmapped. The whole Lua API speaks physical pixels: integers stay
    /// integers at any output scale, so configs never cause misalignment.
    pub geometry: Option<(i32, i32, i32, i32)>,
    pub mapped: bool,
    pub focused: bool,
    /// xdg state the client last acked: what the window *is*, not what a
    /// pending request asks for (requests arrive via `on_window_request`).
    pub fullscreen: bool,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutputProps {
    pub name: String,
    /// Physical pixels, like all Lua geometry.
    pub geometry: (i32, i32, i32, i32),
    /// Geometry minus layer-shell exclusive zones.
    pub usable: (i32, i32, i32, i32),
}

/// Queued operations, applied by the core after each Lua entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowProperties {
    /// Per-window overrides; `None` falls back to the corresponding setting.
    pub radius: Option<i32>,
    pub tearing: Option<bool>,
    pub border_focused: Option<[f32; 4]>,
    pub border_unfocused: Option<[f32; 4]>,
}

#[derive(Debug, Clone)]
pub enum WindowOp {
    SetGeometry(u64, (i32, i32, i32, i32)),
    /// Replace all per-window rendering/presentation overrides. Omitted fields
    /// fall back to global settings, so an empty table clears every override.
    SetProperties(u64, WindowProperties),
    Show(u64),
    Hide(u64),
    Focus(u64),
    /// Clear keyboard focus entirely (e.g. switching to an empty workspace).
    ClearFocus,
    Close(u64),
    /// Restack on top of all other windows.
    Raise(u64),
    /// Set/unset the xdg Fullscreen state flag. Only the protocol state:
    /// geometry stays policy — pair with `SetGeometry` to actually cover an
    /// output.
    SetFullscreen(u64, bool),
    /// Set/unset the xdg Maximized state flag; geometry stays policy.
    SetMaximized(u64, bool),
    /// Move the camera: world offset (physical pixels) and zoom factor.
    SetView(i32, i32, f64),
}

/// A queued `tomoe.ui` operation. Widget ids are allocated at call time
/// (`widgets::alloc_id`) so the handle returns synchronously; the core
/// creates/removes the retained widget when the Lua entry returns.
#[derive(Debug, Clone, PartialEq)]
pub enum UiOp {
    Open { id: u64, spec: WidgetSpec },
    Close(u64),
}

/// Data for a pointer button event handed to `on_pointer_button` hooks.
pub struct PointerButtonData {
    pub button: u32,
    pub pressed: bool,
    /// Pointer position in world coordinates (where windows live).
    pub world: (f64, f64),
    /// Pointer position in screen coordinates (raw output space).
    pub screen: (f64, f64),
    /// (alt, ctrl, shift, super)
    pub mods: (bool, bool, bool, bool),
    /// Window under the pointer, if any.
    pub window: Option<u64>,
}

/// Data for a pointer axis (scroll) event handed to `on_pointer_axis` hooks.
pub struct PointerAxisData {
    pub dx: f64,
    pub dy: f64,
    pub world: (f64, f64),
    pub screen: (f64, f64),
    pub mods: (bool, bool, bool, bool),
    pub window: Option<u64>,
}

/// Kernel input codes for the common mouse buttons, as Lua-friendly names.
fn button_name(code: u32) -> Option<&'static str> {
    Some(match code {
        0x110 => "left",
        0x111 => "right",
        0x112 => "middle",
        0x113 => "side",
        0x114 => "extra",
        0x115 => "forward",
        0x116 => "back",
        _ => return None,
    })
}

pub struct PendingBind {
    pub combo: String,
    pub action: Action,
    /// Human-readable label for the hotkey overlay.
    pub desc: Option<String>,
}

#[derive(Default)]
struct Hooks {
    window_open: Vec<RegistryKey>,
    window_close: Vec<RegistryKey>,
    focus_change: Vec<RegistryKey>,
    outputs_changed: Vec<RegistryKey>,
    pointer_button: Vec<RegistryKey>,
    pointer_axis: Vec<RegistryKey>,
    window_request: Vec<RegistryKey>,
    pointer_enter: Vec<RegistryKey>,
    pointer_leave: Vec<RegistryKey>,
}

/// A Lua-initiated pointer grab (`tomoe.grab_pointer`): motion is routed to
/// the handler instead of clients until every button is released.
struct PointerGrab {
    motion: RegistryKey,
    release: Option<RegistryKey>,
}

/// A `tomoe.on_reload` registration: `save` runs in the outgoing VM at
/// reload, `restore` runs in the fresh VM with the saved (JSON) value.
struct ReloadHooks {
    save: RegistryKey,
    restore: RegistryKey,
}

/// Callbacks of a `tomoe.ui` widget, keyed by widget id. They live in the
/// VM (never in core state), so a config reload naturally invalidates them
/// — the core closes Lua-owned widgets when the VM is swapped.
#[derive(Default)]
struct UiCallbacks {
    confirm: Option<RegistryKey>,
    cancel: Option<RegistryKey>,
    select: Option<RegistryKey>,
}

/// A queued answer to a portal `screencast_select` IPC request, keyed by
/// the pending token the IPC layer holds for the waiting portal client.
#[derive(Debug, Clone)]
pub enum ScreencastReply {
    /// Cast the named output.
    Output(String),
    /// Cast the window; the IPC layer maps the id to its foreign-toplevel
    /// identifier (a window gone by then becomes a deny).
    Window(u64),
    /// Cancel the request.
    Deny,
}

/// What `emit_screencast_request` did with the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreencastHookOutcome {
    /// A reply is queued (drained by `take_screencast_replies`).
    Answered,
    /// The hook called `req:defer()`; a reply arrives from a later Lua
    /// entry (menu callback, IPC handler).
    Deferred,
    /// No usable answer (no hook / hook error): the portal should fall
    /// back to its own heuristics.
    Fallback,
}

/// Interior state of a screencast request: shared between the userdata
/// (which deferred callbacks may hold long after the hook returned) and the
/// emitter interpreting the hook's return value.
struct ScreencastRequestState {
    token: u64,
    responded: Cell<bool>,
    deferred: Cell<bool>,
}

/// The `req` object of `tomoe.on_screencast_request`: a snapshot of the ask
/// plus resolve/deny/defer actions (doctrine 02: snapshot in, actions out).
struct LuaScreencastRequest {
    state: Rc<ScreencastRequestState>,
    app_id: String,
    monitor: bool,
    window: bool,
    shared: Rc<Shared>,
}

impl LuaScreencastRequest {
    /// Queue the reply; a request answers exactly once.
    fn respond(&self, reply: ScreencastReply) {
        if self.state.responded.replace(true) {
            warn!("screencast request already answered; ignoring");
            return;
        }
        self.shared
            .screencast_replies
            .borrow_mut()
            .push((self.state.token, reply));
    }
}

/// Parse a selection table: `{ output = "DP-1" }` or `{ window = win_or_id }`.
fn parse_screencast_selection(sel: &Table) -> Option<ScreencastReply> {
    if let Ok(name) = sel.get::<String>("output") {
        return Some(ScreencastReply::Output(name));
    }
    match sel.get::<Value>("window") {
        Ok(Value::UserData(ud)) => ud
            .borrow::<LuaWindow>()
            .ok()
            .map(|w| ScreencastReply::Window(w.id)),
        Ok(Value::Integer(id)) => u64::try_from(id).ok().map(ScreencastReply::Window),
        _ => None,
    }
}

impl UserData for LuaScreencastRequest {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("app_id", |_, this| Ok(this.app_id.clone()));
        fields.add_field_method_get("types", |lua, this| {
            let t = lua.create_table()?;
            t.set("monitor", this.monitor)?;
            t.set("window", this.window)?;
            Ok(t)
        });
        fields.add_field_method_get("outputs", |lua, this| {
            let outputs = this.shared.outputs.borrow();
            let list = lua.create_table()?;
            for (i, o) in outputs.iter().enumerate() {
                let t = lua.create_table()?;
                t.set("name", o.name.clone())?;
                t.set("x", o.geometry.0)?;
                t.set("y", o.geometry.1)?;
                t.set("w", o.geometry.2)?;
                t.set("h", o.geometry.3)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        fields.add_field_method_get("windows", |_, this| {
            let mut ids: Vec<u64> = this
                .shared
                .windows
                .borrow()
                .iter()
                .filter(|(_, props)| props.mapped)
                .map(|(id, _)| *id)
                .collect();
            ids.sort_unstable();
            Ok(ids
                .into_iter()
                .map(|id| LuaWindow {
                    id,
                    shared: this.shared.clone(),
                })
                .collect::<Vec<_>>())
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("resolve", |_, this, sel: Table| {
            match parse_screencast_selection(&sel) {
                Some(reply) => this.respond(reply),
                None => {
                    warn!(
                        "screencast resolve: expected {{ output = name }} or \
                         {{ window = win }}; denying"
                    );
                    this.respond(ScreencastReply::Deny);
                }
            }
            Ok(())
        });
        methods.add_method("deny", |_, this, ()| {
            this.respond(ScreencastReply::Deny);
            Ok(())
        });
        methods.add_method("defer", |_, this, ()| {
            this.state.deferred.set(true);
            Ok(())
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("ScreencastRequest({:?})", this.app_id))
        });
    }
}

/// Spec keys that select windows (or act on them) rather than describe them;
/// excluded from the property table `tomoe.rules_for` merges.
const RULE_RESERVED: [&str; 4] = ["app_id", "title", "match", "apply"];

/// A `tomoe.rule` registration. Matching is mechanism (the core evaluates
/// it); what the data properties *mean* is policy — the WM module reads them
/// via `tomoe.rules_for` and decides.
struct Rule {
    /// Lua pattern matched against the window's app id (unanchored; specs
    /// anchor with ^$ for exact matches).
    app_id: Option<String>,
    /// Lua pattern matched against the window's title.
    title: Option<String>,
    /// `match = fn(win) -> boolean`: arbitrary predicate.
    matcher: Option<RegistryKey>,
    /// The spec table itself; non-reserved keys are the rule's properties.
    spec: RegistryKey,
    /// `apply = fn(win)`: runs when a matching window opens, after the
    /// on_window_open hooks so it can refine the WM's placement.
    apply: Option<RegistryKey>,
}

/// Does rule `index` match window `id`? All given matchers must match; a
/// rule with none matches every window. Borrows of `shared.rules` are kept
/// short: matcher functions run user code that may declare further rules.
fn rule_matches(lua: &Lua, shared: &Rc<Shared>, index: usize, id: u64) -> bool {
    if !shared.windows.borrow().contains_key(&id) {
        return false;
    }
    let (app_id_pat, title_pat, matcher) = {
        let rules = shared.rules.borrow();
        let Some(rule) = rules.get(index) else {
            return false;
        };
        (
            rule.app_id.clone(),
            rule.title.clone(),
            rule.matcher
                .as_ref()
                .and_then(|k| lua.registry_value::<Function>(k).ok()),
        )
    };
    if app_id_pat.is_some() || title_pat.is_some() {
        let (app_id, title) = {
            let windows = shared.windows.borrow();
            let Some(props) = windows.get(&id) else {
                return false;
            };
            (props.app_id.clone(), props.title.clone())
        };
        let string_match = lua
            .globals()
            .get::<Table>("string")
            .and_then(|t| t.get::<Function>("match"));
        let string_match = match string_match {
            Ok(func) => func,
            Err(err) => {
                warn!("tomoe.rule: string.match unavailable: {err}");
                return false;
            }
        };
        for (subject, pattern) in [(app_id, app_id_pat), (title, title_pat)] {
            let Some(pattern) = pattern else { continue };
            match string_match.call::<Value>((subject, pattern.clone())) {
                Ok(Value::Nil) => return false,
                Ok(_) => {}
                Err(err) => {
                    warn!("tomoe.rule: pattern {pattern:?} error: {err}");
                    return false;
                }
            }
        }
    }
    if let Some(func) = matcher {
        let win = LuaWindow {
            id,
            shared: shared.clone(),
        };
        match func.call::<Value>(win) {
            Ok(value) => {
                if matches!(value, Value::Nil | Value::Boolean(false)) {
                    return false;
                }
            }
            Err(err) => {
                warn!("Lua rule match error: {err}");
                return false;
            }
        }
    }
    true
}

#[derive(Default)]
struct Shared {
    actions: RefCell<Vec<Action>>,
    binds: RefCell<Vec<PendingBind>>,
    bind_fns: RefCell<Vec<RegistryKey>>,
    settings: RefCell<Settings>,
    ops: RefCell<Vec<WindowOp>>,
    windows: RefCell<HashMap<u64, WinProps>>,
    outputs: RefCell<Vec<OutputProps>>,
    /// Camera snapshot (x, y, zoom); set_view updates it eagerly so reads
    /// within the same Lua entry observe pending changes.
    view: RefCell<(i32, i32, f64)>,
    /// Pointer snapshot: (world x, world y, screen x, screen y).
    pointer: RefCell<(f64, f64, f64, f64)>,
    hooks: RefCell<Hooks>,
    grab: RefCell<Option<PointerGrab>>,
    /// Process manifest (`tomoe.process.once/service`), keyed by id — the
    /// desired state the core's ProcessManager reconciles against.
    processes: RefCell<HashMap<String, ProcessDecl>>,
    /// Set on every manifest mutation; the core takes-and-reconciles.
    processes_dirty: Cell<bool>,
    /// Fire-and-forget spawns (`tomoe.spawn`, `tomoe.process.spawn`),
    /// drained like ops so the core can track and reap the children.
    spawns: RefCell<Vec<ProcessSpec>>,
    /// User IPC endpoints (`tomoe.ipc.serve`), keyed by method name. Live in
    /// the VM, so a reload naturally re-registers them from the new config.
    ipc_handlers: RefCell<HashMap<String, RegistryKey>>,
    /// Queued `tomoe.ipc.broadcast` events, drained like ops after each Lua
    /// entry. Payloads convert to JSON at call time — the queue never holds
    /// live Lua values.
    ipc_broadcasts: RefCell<Vec<(String, serde_json::Value)>>,
    /// `tomoe.on_reload` persist/restore hooks, keyed by name so independent
    /// modules persist independently across config reloads.
    reload_hooks: RefCell<HashMap<String, ReloadHooks>>,
    /// `tomoe.rule` declarations, in declaration order (later rules win when
    /// `rules_for` merges properties).
    rules: RefCell<Vec<Rule>>,
    /// Queued `tomoe.ui` widget declarations, drained like ops.
    ui_ops: RefCell<Vec<UiOp>>,
    /// Widget callbacks by id (see [`UiCallbacks`]).
    ui_callbacks: RefCell<HashMap<u64, UiCallbacks>>,
    /// The `tomoe.on_screencast_request` handler. Single slot (a request
    /// needs exactly one answer): registering again replaces it.
    screencast_hook: RefCell<Option<RegistryKey>>,
    /// Queued answers to pending portal source requests, drained like ops
    /// (`ipc::flush_screencast_replies`).
    screencast_replies: RefCell<Vec<(u64, ScreencastReply)>>,
    /// Wall-clock deadline of the Lua entry in flight; None when idle (or
    /// the watchdog is disabled). A Cell, not a RefCell: the debug hook
    /// reads it mid-execution and must never conflict with a borrow.
    watchdog_deadline: Cell<Option<Instant>>,
}

// ─── Window userdata ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct LuaWindow {
    id: u64,
    shared: Rc<Shared>,
}

impl LuaWindow {
    fn props(&self) -> WinProps {
        self.shared
            .windows
            .borrow()
            .get(&self.id)
            .cloned()
            .unwrap_or_default()
    }

    fn op(&self, op: WindowOp) {
        self.shared.ops.borrow_mut().push(op);
    }
}

impl UserData for LuaWindow {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("id", |_, this, ()| Ok(this.id));
        methods.add_method("app_id", |_, this, ()| Ok(this.props().app_id));
        methods.add_method("title", |_, this, ()| Ok(this.props().title));
        methods.add_method("is_mapped", |_, this, ()| Ok(this.props().mapped));
        methods.add_method("is_focused", |_, this, ()| Ok(this.props().focused));
        methods.add_method("is_fullscreen", |_, this, ()| Ok(this.props().fullscreen));
        methods.add_method("is_maximized", |_, this, ()| Ok(this.props().maximized));
        methods.add_method("geometry", |lua, this, ()| match this.props().geometry {
            Some((x, y, w, h)) => {
                let t = lua.create_table()?;
                t.set("x", x)?;
                t.set("y", y)?;
                t.set("w", w)?;
                t.set("h", h)?;
                Ok(Value::Table(t))
            }
            None => Ok(Value::Nil),
        });
        methods.add_method(
            "set_geometry",
            |_, this, (x, y, w, h): (i32, i32, i32, i32)| {
                this.op(WindowOp::SetGeometry(this.id, (x, y, w.max(1), h.max(1))));
                Ok(())
            },
        );
        methods.add_method("set_properties", |_, this, table: Table| {
            let radius = table.get::<Option<i32>>("radius")?.map(|v| v.max(0));
            let tearing = table.get::<Option<bool>>("tearing")?;
            let border = table.get::<Option<Table>>("border")?;
            let parse = |key: &str| -> mlua::Result<Option<[f32; 4]>> {
                let Some(border) = border.as_ref() else {
                    return Ok(None);
                };
                let Some(value) = border.get::<Option<String>>(key)? else {
                    return Ok(None);
                };
                parse_color(&value).map(Some).ok_or_else(|| {
                    mlua::Error::runtime(format!("invalid window border.{key} color {value:?}"))
                })
            };
            let props = WindowProperties {
                radius,
                tearing,
                border_focused: parse("focused")?,
                border_unfocused: parse("unfocused")?,
            };
            this.op(WindowOp::SetProperties(this.id, props));
            Ok(())
        });
        methods.add_method("show", |_, this, ()| {
            this.op(WindowOp::Show(this.id));
            Ok(())
        });
        methods.add_method("hide", |_, this, ()| {
            this.op(WindowOp::Hide(this.id));
            Ok(())
        });
        methods.add_method("focus", |_, this, ()| {
            this.op(WindowOp::Focus(this.id));
            Ok(())
        });
        methods.add_method("raise", |_, this, ()| {
            this.op(WindowOp::Raise(this.id));
            Ok(())
        });
        methods.add_method("set_fullscreen", |_, this, on: bool| {
            this.op(WindowOp::SetFullscreen(this.id, on));
            Ok(())
        });
        methods.add_method("set_maximized", |_, this, on: bool| {
            this.op(WindowOp::SetMaximized(this.id, on));
            Ok(())
        });
        methods.add_method("close", |_, this, ()| {
            this.op(WindowOp::Close(this.id));
            Ok(())
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Window({}, {:?})", this.id, this.props().app_id))
        });
    }
}

// ─── UI-widget userdata ───────────────────────────────────────────────────────────────

/// Handle returned by `tomoe.ui.*` constructors.
#[derive(Clone)]
struct UiHandle {
    id: u64,
    shared: Rc<Shared>,
}

impl UserData for UiHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("close", |_, this, ()| {
            this.shared.ui_ops.borrow_mut().push(UiOp::Close(this.id));
            Ok(())
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("UiWidget({})", this.id))
        });
    }
}

// ─── Runtime ──────────────────────────────────────────────────────────────────

pub struct LuaRuntime {
    lua: Lua,
    shared: Rc<Shared>,
    /// Whether the watchdog debug hook is currently installed in the VM.
    watchdog_hook_set: Cell<bool>,
}

/// How many VM instructions between watchdog deadline checks. Small enough
/// to bound overrun to well under a frame of interpreted Lua, large enough
/// that the `Instant::now()` check is noise.
const WATCHDOG_GRANULARITY: u32 = 10_000;

/// RAII disarm for the dispatch watchdog: dropping the guard of the entry
/// that armed the deadline clears it. Nested entries (rule apply inside
/// window-open, a widget callback firing from an IPC-driven event) hold a
/// no-op guard — the outermost entry owns the budget.
struct WatchdogGuard {
    // Owns its Rc (not `&'rt Shared`) so entry methods stay `&mut self`.
    shared: Option<Rc<Shared>>,
}

impl Drop for WatchdogGuard {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.watchdog_deadline.set(None);
        }
    }
}

impl LuaRuntime {
    // mlua::Result because mlua's error is not Send+Sync (single-threaded runtime);
    // the caller converts to anyhow at the boundary.
    pub fn new() -> mlua::Result<Self> {
        let lua = Lua::new();
        let shared = Rc::new(Shared::default());
        *shared.view.borrow_mut() = (0, 0, 1.0);

        let tomoe = lua.create_table()?;

        // tomoe.settings { gaps = 8, border = {...}, ... }
        let s = shared.clone();
        tomoe.set(
            "settings",
            lua.create_function(move |_, table: Table| {
                let mut settings = s.settings.borrow_mut();
                if let Ok(gaps) = table.get::<i32>("gaps") {
                    settings.gaps = gaps;
                }
                if let Ok(displays) = table.get::<Table>("displays") {
                    settings.displays.clear();
                    for pair in displays.pairs::<String, Table>() {
                        let Ok((name, display)) = pair else {
                            warn!("settings.displays: expected {{ [\"DP-1\"] = {{...}} }} entries");
                            continue;
                        };
                        let mut ds = DisplaySettings::default();
                        if let Ok(res) = display.get::<String>("resolution") {
                            match res.parse() {
                                Ok(r) => ds.resolution = r,
                                Err(()) => warn!(
                                    "displays[{name:?}]: invalid resolution {res:?} \
                                     (expected \"preferred\", \"max\", or \"WxH\", \
                                     optionally followed by \"@<Hz|max>\")"
                                ),
                            }
                        }
                        if let Ok(pos) = display.get::<Table>("position") {
                            match (pos.get::<i32>(1), pos.get::<i32>(2)) {
                                (Ok(x), Ok(y)) => ds.position = Some((x, y)),
                                _ => warn!(
                                    "displays[{name:?}]: invalid position \
                                     (expected {{ x, y }} in physical pixels)"
                                ),
                            }
                        }
                        // Entries are rebuilt from scratch on every settings
                        // call, so plain-bool truthiness (missing -> false)
                        // is the right default here.
                        if let Ok(v) = display.get::<bool>("disabled") {
                            ds.disabled = v;
                        }
                        if let Ok(m) = display.get::<String>("mirror") {
                            ds.mirror = Some(m);
                        }
                        if let Ok(v) = display.get::<bool>("vrr") {
                            ds.vrr = v;
                        }
                        settings.displays.insert(name, ds);
                    }
                }
                if let Ok(scale) = table.get::<f64>("scale") {
                    settings.scale = scale;
                }
                if let Ok(kb) = table.get::<Table>("keyboard") {
                    let k = &mut settings.keyboard;
                    if let Ok(v) = kb.get::<String>("rules") {
                        k.rules = v;
                    }
                    if let Ok(v) = kb.get::<String>("model") {
                        k.model = v;
                    }
                    if let Ok(v) = kb.get::<String>("layout") {
                        k.layout = v;
                    }
                    if let Ok(v) = kb.get::<String>("variant") {
                        k.variant = v;
                    }
                    if let Ok(v) = kb.get::<String>("options") {
                        k.options = Some(v);
                    }
                    if let Ok(v) = kb.get::<i32>("repeat_delay") {
                        k.repeat_delay = v.max(1);
                    }
                    if let Ok(v) = kb.get::<i32>("repeat_rate") {
                        k.repeat_rate = v.max(0);
                    }
                }
                if let Ok(t) = table.get::<Table>("touchpad") {
                    settings.input.touchpad = parse_input_device(&t, "settings.touchpad");
                }
                if let Ok(t) = table.get::<Table>("mouse") {
                    settings.input.mouse = parse_input_device(&t, "settings.mouse");
                }
                if let Ok(devices) = table.get::<Table>("devices") {
                    settings.input.devices.clear();
                    for pair in devices.pairs::<String, Table>() {
                        let Ok((name, t)) = pair else {
                            warn!(
                                "settings.devices: expected \
                                 {{ [\"<libinput device name>\"] = {{...}} }} entries"
                            );
                            continue;
                        };
                        let parsed = parse_input_device(&t, &format!("settings.devices[{name:?}]"));
                        settings.input.devices.insert(name, parsed);
                    }
                }
                // Option<bool>: mlua maps a missing key to `false` for plain
                // bool (Lua truthiness), which would reset the setting on
                // every later partial settings call.
                if let Ok(Some(ffm)) = table.get::<Option<bool>>("focus_follows_mouse") {
                    settings.focus_follows_mouse = ffm;
                }
                if let Ok(Some(tearing)) = table.get::<Option<bool>>("tearing") {
                    settings.tearing = tearing;
                }
                if let Ok(Some(wait)) = table.get::<Option<bool>>("wait_for_frame_completion") {
                    settings.wait_for_frame_completion = wait;
                }
                if let Ok(Some(ms)) = table.get::<Option<u64>>("watchdog_ms") {
                    settings.watchdog_ms = ms;
                    // Adjust the deadline of the *running* entry too, so a
                    // config that raises (or disables) the budget at the top
                    // of init.lua governs its own load. Only when armed —
                    // arming here without a guard would leak the deadline.
                    if s.watchdog_deadline.get().is_some() {
                        s.watchdog_deadline
                            .set((ms > 0).then(|| Instant::now() + Duration::from_millis(ms)));
                    }
                }
                if let Ok(m) = table.get::<String>("mod") {
                    match crate::input::ModKey::parse(&m) {
                        Some(key) => settings.mod_key = key,
                        None => warn!(
                            "settings.mod: unknown modifier {m:?} \
                             (expected \"super\", \"alt\", \"ctrl\", or \"shift\")"
                        ),
                    }
                }
                if let Ok(size) = table.get::<Table>("winit_size") {
                    settings.winit_size = (
                        size.get(1).unwrap_or(settings.winit_size.0),
                        size.get(2).unwrap_or(settings.winit_size.1),
                    );
                }
                match table.get::<Value>("animations") {
                    Ok(Value::Boolean(false)) => {
                        settings.animations = crate::animation::AnimationSettings::off();
                    }
                    Ok(Value::Boolean(true)) => {
                        settings.animations = Default::default();
                    }
                    Ok(Value::Table(anims)) => {
                        let a = &mut settings.animations;
                        for (key, slot) in [
                            ("window_move", &mut a.window_move),
                            ("window_open", &mut a.window_open),
                        ] {
                            if let Ok(value) = anims.get::<Value>(key) {
                                let label = format!("settings.animations.{key}");
                                if let Some(config) = parse_animation_config(&value, &label) {
                                    *slot = config;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if let Ok(shadow) = table.get::<Table>("shadow") {
                    if let Ok(range) = shadow.get::<i32>("range") {
                        settings.shadow_range = range.max(0);
                    }
                    if let Ok(color) = shadow.get::<String>("color") {
                        match parse_color(&color) {
                            Some(c) => settings.shadow_color = c,
                            None => warn!("invalid shadow.color {color:?}"),
                        }
                    }
                    if let Ok(power) = shadow.get::<f32>("power") {
                        if power.is_finite() {
                            settings.shadow_power = power.clamp(1., 4.);
                        } else {
                            warn!("invalid shadow.power {power:?}");
                        }
                    }
                }
                if let Ok(border) = table.get::<Table>("border") {
                    if let Ok(width) = border.get::<i32>("width") {
                        settings.border_width = width;
                    }
                    if let Ok(color) = border.get::<String>("focused") {
                        match parse_color(&color) {
                            Some(c) => settings.border_focused = c,
                            None => warn!("invalid border.focused color {color:?}"),
                        }
                    }
                    if let Ok(color) = border.get::<String>("unfocused") {
                        match parse_color(&color) {
                            Some(c) => settings.border_unfocused = c,
                            None => warn!("invalid border.unfocused color {color:?}"),
                        }
                    }
                    if let Ok(radius) = border.get::<i32>("radius") {
                        settings.corner_radius = radius.max(0);
                    }
                }
                Ok(())
            })?,
        )?;

        // tomoe.bind("Alt+Return", fn | "action string" [, "overlay description"])
        let s = shared.clone();
        tomoe.set(
            "bind",
            lua.create_function(
                move |lua, (combo, action, desc): (String, Value, Option<String>)| {
                    let action = match action {
                        Value::String(name) => match Action::parse(&name.to_string_lossy()) {
                            Ok(action) => action,
                            Err(err) => {
                                warn!("tomoe.bind({combo:?}): {err:#}");
                                return Ok(());
                            }
                        },
                        Value::Function(func) => {
                            let key = lua.create_registry_value(func)?;
                            let mut fns = s.bind_fns.borrow_mut();
                            fns.push(key);
                            Action::LuaFn(fns.len() - 1)
                        }
                        other => {
                            warn!(
                                "tomoe.bind({combo:?}): expected string or function, got {}",
                                other.type_name()
                            );
                            return Ok(());
                        }
                    };
                    s.binds.borrow_mut().push(PendingBind {
                        combo,
                        action,
                        desc,
                    });
                    Ok(())
                },
            )?,
        )?;

        // tomoe.spawn("foot") — fire-and-forget shell command. Queued (like
        // ops) so the core's process manager tracks and reaps the child.
        let s = shared.clone();
        tomoe.set(
            "spawn",
            lua.create_function(move |_, cmd: String| {
                s.spawns.borrow_mut().push(ProcessSpec {
                    launch: Launch::Shell(cmd),
                    cwd: None,
                    env: Default::default(),
                });
                Ok(())
            })?,
        )?;

        // tomoe.process — declarative process manifest.
        // `once`/`service` declare desired state diffed by id, so a config
        // reload keeps, restarts, or stops children as the diff dictates;
        // `spawn` is imperative fire-and-forget (event handlers).
        let process = lua.create_table()?;

        // tomoe.process.once(id, { command|shell, cwd, env,
        //   run = "once_per_session" (default) | "once_per_config_version" })
        let s = shared.clone();
        process.set(
            "once",
            lua.create_function(move |_, (id, opts): (String, Option<Table>)| {
                let label = format!("process.once({id:?})");
                let Some(spec) = parse_process_spec(opts.as_ref(), Some(&id), &label) else {
                    return Ok(());
                };
                let run = opts
                    .map(|t| {
                        parse_policy_field(
                            &t,
                            "run",
                            &label,
                            |v| match v {
                                "once_per_session" => Some(RunPolicy::OncePerSession),
                                "once_per_config_version" => Some(RunPolicy::OncePerConfigVersion),
                                _ => None,
                            },
                            "\"once_per_session\" or \"once_per_config_version\"",
                        )
                    })
                    .unwrap_or_default();
                s.processes
                    .borrow_mut()
                    .insert(id, ProcessDecl::Once { spec, run });
                s.processes_dirty.set(true);
                Ok(())
            })?,
        )?;

        // tomoe.process.service(id, { command|shell, cwd, env,
        //   restart = "never" | "on_failure" | "on_exit" (default),
        //   reload = "keep_if_unchanged" (default) | "always_restart" })
        let s = shared.clone();
        process.set(
            "service",
            lua.create_function(move |_, (id, opts): (String, Option<Table>)| {
                let label = format!("process.service({id:?})");
                let Some(spec) = parse_process_spec(opts.as_ref(), Some(&id), &label) else {
                    return Ok(());
                };
                let (restart, reload) = opts
                    .map(|t| {
                        (
                            parse_policy_field(
                                &t,
                                "restart",
                                &label,
                                |v| match v {
                                    "never" => Some(RestartPolicy::Never),
                                    "on_failure" => Some(RestartPolicy::OnFailure),
                                    "on_exit" => Some(RestartPolicy::OnExit),
                                    _ => None,
                                },
                                "\"never\", \"on_failure\", or \"on_exit\"",
                            ),
                            parse_policy_field(
                                &t,
                                "reload",
                                &label,
                                |v| match v {
                                    "keep_if_unchanged" => Some(ReloadPolicy::KeepIfUnchanged),
                                    "always_restart" => Some(ReloadPolicy::AlwaysRestart),
                                    _ => None,
                                },
                                "\"keep_if_unchanged\" or \"always_restart\"",
                            ),
                        )
                    })
                    .unwrap_or_default();
                s.processes.borrow_mut().insert(
                    id,
                    ProcessDecl::Service {
                        spec,
                        restart,
                        reload,
                    },
                );
                s.processes_dirty.set(true);
                Ok(())
            })?,
        )?;

        // tomoe.process.spawn { command = {...} | shell = "...", cwd, env }
        let s = shared.clone();
        process.set(
            "spawn",
            lua.create_function(move |_, opts: Table| {
                if let Some(spec) = parse_process_spec(Some(&opts), None, "process.spawn") {
                    s.spawns.borrow_mut().push(spec);
                }
                Ok(())
            })?,
        )?;

        tomoe.set("process", process)?;

        // tomoe.ipc — user-extensible IPC endpoints on the compositor's JSON
        // socket. `serve` registers a request handler,
        // `broadcast` pushes an event to every subscribed client. Params and
        // payloads cross the boundary as JSON-compatible values.
        let ipc = lua.create_table()?;

        // tomoe.ipc.serve("workspace/switch", function(params) ... end) —
        // re-registering a method name overwrites the previous handler.
        let s = shared.clone();
        ipc.set(
            "serve",
            lua.create_function(move |lua, (method, handler): (String, Function)| {
                let key = lua.create_registry_value(handler)?;
                s.ipc_handlers.borrow_mut().insert(method, key);
                Ok(())
            })?,
        )?;

        // tomoe.ipc.broadcast("workspace/active", payload)
        let s = shared.clone();
        ipc.set(
            "broadcast",
            lua.create_function(move |lua, (event, payload): (String, Option<Value>)| {
                let payload = match payload {
                    None | Some(Value::Nil) => serde_json::Value::Null,
                    Some(value) => match lua.from_value(value) {
                        Ok(json) => json,
                        Err(err) => {
                            warn!("tomoe.ipc.broadcast({event:?}): payload not JSON-compatible: {err}");
                            return Ok(());
                        }
                    },
                };
                s.ipc_broadcasts.borrow_mut().push((event, payload));
                Ok(())
            })?,
        )?;

        tomoe.set("ipc", ipc)?;

        // tomoe.ui — compositor-drawn retained widgets. Lua declares the
        // widget once; the core renders, damages, and routes input to it,
        // and only selection events re-enter Lua. The exit dialog, hotkey
        // overlay, and config-error banner are builtins on this registry.
        let ui = lua.create_table()?;

        /// Optional callback field from a widget spec table.
        fn callback(lua: &Lua, spec: &Table, field: &str, label: &str) -> Option<RegistryKey> {
            match spec.get::<Value>(field) {
                Ok(Value::Function(f)) => lua.create_registry_value(f).ok(),
                Ok(Value::Nil) | Err(_) => None,
                Ok(other) => {
                    warn!(
                        "{label}: {field} must be a function, got {}",
                        other.type_name()
                    );
                    None
                }
            }
        }

        // tomoe.ui.confirm { text, on_confirm, on_cancel } -> UiWidget
        let s = shared.clone();
        ui.set(
            "confirm",
            lua.create_function(move |lua, spec: Table| {
                let text = spec.get::<String>("text").unwrap_or_default();
                if text.is_empty() {
                    warn!("tomoe.ui.confirm: missing text");
                    return Ok(None);
                }
                let id = widgets::alloc_id();
                s.ui_callbacks.borrow_mut().insert(
                    id,
                    UiCallbacks {
                        confirm: callback(lua, &spec, "on_confirm", "tomoe.ui.confirm"),
                        cancel: callback(lua, &spec, "on_cancel", "tomoe.ui.confirm"),
                        select: None,
                    },
                );
                s.ui_ops.borrow_mut().push(UiOp::Open {
                    id,
                    spec: WidgetSpec::Confirm { text },
                });
                Ok(Some(UiHandle {
                    id,
                    shared: s.clone(),
                }))
            })?,
        )?;

        // tomoe.ui.menu { title?, items, on_select, on_cancel } -> UiWidget
        let s = shared.clone();
        ui.set(
            "menu",
            lua.create_function(move |lua, spec: Table| {
                let items: Vec<String> = spec.get("items").unwrap_or_default();
                if items.is_empty() {
                    warn!("tomoe.ui.menu: needs at least one item");
                    return Ok(None);
                }
                let title = spec.get::<Option<String>>("title").unwrap_or_default();
                let id = widgets::alloc_id();
                s.ui_callbacks.borrow_mut().insert(
                    id,
                    UiCallbacks {
                        confirm: None,
                        cancel: callback(lua, &spec, "on_cancel", "tomoe.ui.menu"),
                        select: callback(lua, &spec, "on_select", "tomoe.ui.menu"),
                    },
                );
                s.ui_ops.borrow_mut().push(UiOp::Open {
                    id,
                    spec: WidgetSpec::Menu { title, items },
                });
                Ok(Some(UiHandle {
                    id,
                    shared: s.clone(),
                }))
            })?,
        )?;

        // tomoe.ui.toast { text, duration?, urgent? } -> UiWidget
        let s = shared.clone();
        ui.set(
            "toast",
            lua.create_function(move |_, spec: Table| {
                let text = spec.get::<String>("text").unwrap_or_default();
                if text.is_empty() {
                    warn!("tomoe.ui.toast: missing text");
                    return Ok(None);
                }
                let duration = spec
                    .get::<Option<f64>>("duration")
                    .unwrap_or_default()
                    .filter(|d| d.is_finite() && *d > 0.0)
                    .unwrap_or(4.0);
                let urgent = spec
                    .get::<Option<bool>>("urgent")
                    .unwrap_or_default()
                    .unwrap_or(false);
                let id = widgets::alloc_id();
                s.ui_ops.borrow_mut().push(UiOp::Open {
                    id,
                    spec: WidgetSpec::Toast {
                        text,
                        duration: std::time::Duration::from_secs_f64(duration),
                        urgent,
                    },
                });
                Ok(Some(UiHandle {
                    id,
                    shared: s.clone(),
                }))
            })?,
        )?;

        // tomoe.ui.sheet { title?, rows = { {"Mod+Q", "Quit"}, ... } }
        let s = shared.clone();
        ui.set(
            "sheet",
            lua.create_function(move |_, spec: Table| {
                let mut rows = Vec::new();
                if let Ok(list) = spec.get::<Table>("rows") {
                    for pair in list.sequence_values::<Table>() {
                        let Ok(row) = pair else {
                            warn!("tomoe.ui.sheet: rows must be {{ {{key, label}}, ... }}");
                            continue;
                        };
                        let key: String = row.get(1).unwrap_or_default();
                        let label: String = row.get(2).unwrap_or_default();
                        rows.push((key, label));
                    }
                }
                if rows.is_empty() {
                    warn!("tomoe.ui.sheet: needs at least one row");
                    return Ok(None);
                }
                let title = spec.get::<Option<String>>("title").unwrap_or_default();
                let id = widgets::alloc_id();
                s.ui_ops.borrow_mut().push(UiOp::Open {
                    id,
                    spec: WidgetSpec::Sheet { title, rows },
                });
                Ok(Some(UiHandle {
                    id,
                    shared: s.clone(),
                }))
            })?,
        )?;

        tomoe.set("ui", ui)?;

        // tomoe.clear_focus() — drop keyboard focus (no window receives keys)
        let s = shared.clone();
        tomoe.set(
            "clear_focus",
            lua.create_function(move |_, ()| {
                s.ops.borrow_mut().push(WindowOp::ClearFocus);
                Ok(())
            })?,
        )?;

        // tomoe.quit()
        let s = shared.clone();
        tomoe.set(
            "quit",
            lua.create_function(move |_, ()| {
                s.actions.borrow_mut().push(Action::Quit);
                Ok(())
            })?,
        )?;

        // tomoe.windows() -> array of window objects
        let s = shared.clone();
        tomoe.set(
            "windows",
            lua.create_function(move |_, ()| {
                let mut ids: Vec<u64> = s.windows.borrow().keys().copied().collect();
                ids.sort_unstable();
                Ok(ids
                    .into_iter()
                    .map(|id| LuaWindow {
                        id,
                        shared: s.clone(),
                    })
                    .collect::<Vec<_>>())
            })?,
        )?;

        // tomoe.window(id) -> window | nil — look up a window by its stable
        // id (e.g. ids persisted through tomoe.on_reload).
        let s = shared.clone();
        tomoe.set(
            "window",
            lua.create_function(move |_, id: u64| {
                Ok(s.windows.borrow().contains_key(&id).then(|| LuaWindow {
                    id,
                    shared: s.clone(),
                }))
            })?,
        )?;

        // tomoe.rule { app_id = "^mpv$", fullscreen = true } — declare a
        // window rule: matcher fields (app_id/title Lua patterns, `match`
        // predicate) select windows, `apply` runs when a matching window
        // opens, every other field is a data property for the WM to read
        // via tomoe.rules_for.
        let s = shared.clone();
        tomoe.set(
            "rule",
            lua.create_function(move |lua, spec: Table| {
                let pattern = |field: &str| match spec.get::<Value>(field) {
                    Ok(Value::String(p)) => Some(p.to_string_lossy()),
                    Ok(Value::Nil) | Err(_) => None,
                    Ok(other) => {
                        warn!(
                            "tomoe.rule: {field} must be a Lua pattern string, got {}",
                            other.type_name()
                        );
                        None
                    }
                };
                let func = |field: &str| match spec.get::<Value>(field) {
                    Ok(Value::Function(f)) => lua.create_registry_value(f).ok(),
                    Ok(Value::Nil) | Err(_) => None,
                    Ok(other) => {
                        warn!(
                            "tomoe.rule: {field} must be a function, got {}",
                            other.type_name()
                        );
                        None
                    }
                };
                let rule = Rule {
                    app_id: pattern("app_id"),
                    title: pattern("title"),
                    matcher: func("match"),
                    apply: func("apply"),
                    spec: lua.create_registry_value(spec)?,
                };
                s.rules.borrow_mut().push(rule);
                Ok(())
            })?,
        )?;

        // tomoe.rules_for(win) -> merged data-property table of every rule
        // matching the window, later declarations winning. Matcher fields
        // and `apply` are excluded.
        let s = shared.clone();
        tomoe.set(
            "rules_for",
            lua.create_function(move |lua, win: mlua::UserDataRef<LuaWindow>| {
                let merged = lua.create_table()?;
                let len = s.rules.borrow().len();
                for i in 0..len {
                    if !rule_matches(lua, &s, i, win.id) {
                        continue;
                    }
                    let spec = {
                        let rules = s.rules.borrow();
                        let Some(rule) = rules.get(i) else { continue };
                        lua.registry_value::<Table>(&rule.spec)?
                    };
                    for pair in spec.pairs::<Value, Value>() {
                        let (key, value) = pair?;
                        if let Value::String(k) = &key {
                            if RULE_RESERVED.contains(&k.to_string_lossy().as_str()) {
                                continue;
                            }
                        }
                        merged.set(key, value)?;
                    }
                }
                Ok(merged)
            })?,
        )?;

        // tomoe.focused_window() -> window | nil
        let s = shared.clone();
        tomoe.set(
            "focused_window",
            lua.create_function(move |_, ()| {
                let id = s
                    .windows
                    .borrow()
                    .iter()
                    .find(|(_, props)| props.focused)
                    .map(|(id, _)| *id);
                Ok(id.map(|id| LuaWindow {
                    id,
                    shared: s.clone(),
                }))
            })?,
        )?;

        // tomoe.usable_area([output_index]) -> {x, y, w, h}
        let s = shared.clone();
        tomoe.set(
            "usable_area",
            lua.create_function(move |lua, idx: Option<usize>| {
                let outputs = s.outputs.borrow();
                let output = outputs.get(idx.map(|i| i.saturating_sub(1)).unwrap_or(0));
                let (x, y, w, h) = output.map(|o| o.usable).unwrap_or((0, 0, 1280, 800));
                let t = lua.create_table()?;
                t.set("x", x)?;
                t.set("y", y)?;
                t.set("w", w)?;
                t.set("h", h)?;
                Ok(t)
            })?,
        )?;

        // tomoe.outputs() -> array of {name, x, y, w, h, usable = {...}}
        let s = shared.clone();
        tomoe.set(
            "outputs",
            lua.create_function(move |lua, ()| {
                let outputs = s.outputs.borrow();
                let list = lua.create_table()?;
                for (i, o) in outputs.iter().enumerate() {
                    let t = lua.create_table()?;
                    t.set("name", o.name.clone())?;
                    t.set("x", o.geometry.0)?;
                    t.set("y", o.geometry.1)?;
                    t.set("w", o.geometry.2)?;
                    t.set("h", o.geometry.3)?;
                    let u = lua.create_table()?;
                    u.set("x", o.usable.0)?;
                    u.set("y", o.usable.1)?;
                    u.set("w", o.usable.2)?;
                    u.set("h", o.usable.3)?;
                    t.set("usable", u)?;
                    list.set(i + 1, t)?;
                }
                Ok(list)
            })?,
        )?;

        // tomoe.view() -> {x, y, zoom} — the camera over the window canvas.
        let s = shared.clone();
        tomoe.set(
            "view",
            lua.create_function(move |lua, ()| {
                let (x, y, zoom) = *s.view.borrow();
                let t = lua.create_table()?;
                t.set("x", x)?;
                t.set("y", y)?;
                t.set("zoom", zoom)?;
                Ok(t)
            })?,
        )?;

        // tomoe.set_view { x = ..., y = ..., zoom = ... } — omitted fields
        // keep their current value. screen = (world - offset) * zoom.
        let s = shared.clone();
        tomoe.set(
            "set_view",
            lua.create_function(move |_, table: Table| {
                let (mut x, mut y, mut zoom) = *s.view.borrow();
                if let Ok(v) = table.get::<i32>("x") {
                    x = v;
                }
                if let Ok(v) = table.get::<i32>("y") {
                    y = v;
                }
                if let Ok(v) = table.get::<f64>("zoom") {
                    if v.is_finite() && v > 0.0 {
                        zoom = v.clamp(1.0 / 16.0, 16.0);
                    } else {
                        warn!("set_view: ignoring invalid zoom {v:?}");
                    }
                }
                *s.view.borrow_mut() = (x, y, zoom);
                s.ops.borrow_mut().push(WindowOp::SetView(x, y, zoom));
                Ok(())
            })?,
        )?;

        // tomoe.pointer() -> {x, y, sx, sy} — world and screen position.
        let s = shared.clone();
        tomoe.set(
            "pointer",
            lua.create_function(move |lua, ()| {
                let (x, y, sx, sy) = *s.pointer.borrow();
                let t = lua.create_table()?;
                t.set("x", x)?;
                t.set("y", y)?;
                t.set("sx", sx)?;
                t.set("sy", sy)?;
                Ok(t)
            })?,
        )?;

        // tomoe.grab_pointer(on_motion [, on_release]) — route pointer
        // motion to Lua (in world coordinates) until every button is
        // released. Typically called from an on_pointer_button hook that
        // returns true to consume the click.
        let s = shared.clone();
        tomoe.set(
            "grab_pointer",
            lua.create_function(
                move |lua, (motion, release): (Function, Option<Function>)| {
                    let motion = lua.create_registry_value(motion)?;
                    let release = release.map(|f| lua.create_registry_value(f)).transpose()?;
                    *s.grab.borrow_mut() = Some(PointerGrab { motion, release });
                    Ok(())
                },
            )?,
        )?;

        // tomoe.ungrab_pointer() — end the grab without the release callback.
        let s = shared.clone();
        tomoe.set(
            "ungrab_pointer",
            lua.create_function(move |_, ()| {
                s.grab.borrow_mut().take();
                Ok(())
            })?,
        )?;

        // tomoe.on_reload(name, save, restore) — persist config state across
        // reloads: `save` runs in the outgoing VM and must return a
        // JSON-compatible value (window handles don't survive — persist
        // ids); `restore` runs in the fresh VM with that value after the new
        // config loads. Keyed by name so independent modules persist
        // independently. When any restore hook runs, the core skips the
        // on_window_open replay — restored state supersedes it.
        let s = shared.clone();
        tomoe.set(
            "on_reload",
            lua.create_function(
                move |lua, (name, save, restore): (String, Function, Function)| {
                    let save = lua.create_registry_value(save)?;
                    let restore = lua.create_registry_value(restore)?;
                    s.reload_hooks
                        .borrow_mut()
                        .insert(name, ReloadHooks { save, restore });
                    Ok(())
                },
            )?,
        )?;

        // Event hooks.
        for (name, field) in [
            ("on_window_open", 0usize),
            ("on_window_close", 1),
            ("on_focus_change", 2),
            ("on_outputs_changed", 3),
            ("on_pointer_button", 4),
            ("on_pointer_axis", 5),
            ("on_window_request", 6),
            ("on_pointer_enter", 7),
            ("on_pointer_leave", 8),
        ] {
            let s = shared.clone();
            tomoe.set(
                name,
                lua.create_function(move |lua, func: Function| {
                    let key = lua.create_registry_value(func)?;
                    let mut hooks = s.hooks.borrow_mut();
                    match field {
                        0 => hooks.window_open.push(key),
                        1 => hooks.window_close.push(key),
                        2 => hooks.focus_change.push(key),
                        3 => hooks.outputs_changed.push(key),
                        4 => hooks.pointer_button.push(key),
                        5 => hooks.pointer_axis.push(key),
                        6 => hooks.window_request.push(key),
                        7 => hooks.pointer_enter.push(key),
                        _ => hooks.pointer_leave.push(key),
                    }
                    Ok(())
                })?,
            )?;
        }

        // tomoe.on_screencast_request(fn) — decide what a screencast portal
        // request captures. Answer by returning a selection table or false
        // (deny), or req:defer() + req:resolve/req:deny from a later
        // callback. Single slot: registering again replaces the handler.
        let s = shared.clone();
        tomoe.set(
            "on_screencast_request",
            lua.create_function(move |lua, func: Function| {
                let key = lua.create_registry_value(func)?;
                *s.screencast_hook.borrow_mut() = Some(key);
                Ok(())
            })?,
        )?;

        lua.globals().set("tomoe", tomoe)?;

        // Preload the default WM library: `require("wm")` runs it lazily, so
        // configs that ship their own WM never pay for (or fight with) ours.
        let preload: Table = lua
            .globals()
            .get::<Table>("package")?
            .get::<Table>("preload")?;
        preload.set(
            "wm",
            lua.create_function(|lua, _: Value| {
                lua.load(WM_LUA).set_name("wm.lua").eval::<Value>()
            })?,
        )?;
        preload.set(
            "zoomer",
            lua.create_function(|lua, _: Value| {
                lua.load(ZOOMER_LUA).set_name("zoomer.lua").eval::<Value>()
            })?,
        )?;
        preload.set(
            "screencast",
            lua.create_function(|lua, _: Value| {
                lua.load(SCREENCAST_LUA)
                    .set_name("screencast.lua")
                    .eval::<Value>()
            })?,
        )?;

        Ok(Self {
            lua,
            shared,
            watchdog_hook_set: Cell::new(false),
        })
    }

    /// Arm the dispatch watchdog for one Lua entry (doctrine 02: every
    /// dispatch has a watchdog). A debug hook checks a wall-clock deadline
    /// every [`WATCHDOG_GRANULARITY`] instructions and aborts the entry
    /// with a Lua error once it passes, so a runaway hook cannot hang the
    /// compositor — the error surfaces through the entry's normal error
    /// path (log / IPC error / config-error banner).
    ///
    /// Trade-off, by construction of LuaJIT: hooks only fire from the
    /// interpreter — compiled traces never check them — so while the
    /// watchdog is enabled the VM runs with `jit.off()` (enforced when the
    /// hook is installed). Guarded entries run interpreted; LuaJIT's
    /// interpreter keeps typical hooks at µs cost. `settings.watchdog_ms =
    /// 0` opts out, removing the hook and restoring full JIT.
    fn watchdog(&self) -> WatchdogGuard {
        if self.shared.watchdog_deadline.get().is_some() {
            // Nested entry: the outermost guard owns the deadline.
            return WatchdogGuard { shared: None };
        }
        let ms = self.shared.settings.borrow().watchdog_ms;
        if ms == 0 {
            if self.watchdog_hook_set.replace(false) {
                self.lua.remove_hook();
                if let Err(err) = self.lua.load("jit.on()").exec() {
                    warn!("watchdog: failed to re-enable LuaJIT compilation: {err}");
                }
            }
            return WatchdogGuard { shared: None };
        }
        if !self.watchdog_hook_set.replace(true) {
            let shared = Rc::clone(&self.shared);
            // The hook must only touch Cells: it can fire while a Rust
            // callback inside the entry holds a RefCell borrow.
            self.lua.set_hook(
                HookTriggers::new().every_nth_instruction(WATCHDOG_GRANULARITY),
                move |_lua, _debug| match shared.watchdog_deadline.get() {
                    Some(deadline) if Instant::now() >= deadline => Err(mlua::Error::runtime(
                        "tomoe watchdog: Lua entry exceeded settings.watchdog_ms; \
                         aborted to keep the compositor responsive",
                    )),
                    _ => Ok(VmState::Continue),
                },
            );
            // Setting the hook is not enough under LuaJIT: compiled traces
            // never check hooks, so a hot loop would escape the deadline.
            // Force the interpreter (and drop any traces recorded while the
            // watchdog was off) for as long as the hook is installed.
            if let Err(err) = self.lua.load("jit.off(); jit.flush()").exec() {
                warn!("watchdog: failed to disable LuaJIT compilation: {err}");
            }
        }
        self.shared
            .watchdog_deadline
            .set(Some(Instant::now() + Duration::from_millis(ms)));
        WatchdogGuard {
            shared: Some(Rc::clone(&self.shared)),
        }
    }

    /// Execute the config at `path` (pre-resolved via `resolve_config_path`),
    /// or the embedded default if None.
    pub fn load(&mut self, path: Option<&Path>) -> Result<()> {
        let (code, name) = match path {
            Some(p) => (
                std::fs::read_to_string(p).with_context(|| format!("error reading {p:?}"))?,
                p.display().to_string(),
            ),
            None => (DEFAULT_CONFIG.to_string(), "<built-in default>".to_string()),
        };
        info!("loading config from {name}");
        let _watchdog = self.watchdog();
        self.lua
            .load(&code)
            .set_name(&name)
            .exec()
            .map_err(|err| anyhow::anyhow!("Lua error: {err}"))?;
        Ok(())
    }

    // ── Snapshot sync (called by the core before Lua entries) ──

    /// Replace the snapshot. Returns true if the outputs part changed.
    pub fn sync(
        &self,
        windows: HashMap<u64, WinProps>,
        outputs: Vec<OutputProps>,
        view: (i32, i32, f64),
        pointer: (f64, f64, f64, f64),
    ) -> bool {
        *self.shared.windows.borrow_mut() = windows;
        *self.shared.view.borrow_mut() = view;
        *self.shared.pointer.borrow_mut() = pointer;
        let changed = *self.shared.outputs.borrow() != outputs;
        *self.shared.outputs.borrow_mut() = outputs;
        changed
    }

    pub fn settings(&self) -> Settings {
        self.shared.settings.borrow().clone()
    }

    pub fn take_binds(&mut self) -> Vec<PendingBind> {
        self.shared.binds.take()
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        self.shared.actions.take()
    }

    pub fn take_ops(&mut self) -> Vec<WindowOp> {
        self.shared.ops.take()
    }

    pub fn take_ui_ops(&mut self) -> Vec<UiOp> {
        std::mem::take(&mut *self.shared.ui_ops.borrow_mut())
    }

    /// Drop a closed widget's callbacks without firing them.
    pub fn drop_ui_callbacks(&self, id: u64) {
        self.shared.ui_callbacks.borrow_mut().remove(&id);
    }

    /// Fire a widget's callback for `event` and drop its callbacks (widgets
    /// close when they fire). Menu selections pass (1-based index, item).
    pub fn emit_ui_event(&mut self, id: u64, event: UiEvent, item: Option<String>) {
        let Some(callbacks) = self.shared.ui_callbacks.borrow_mut().remove(&id) else {
            return;
        };
        let key = match event {
            UiEvent::Confirm => callbacks.confirm,
            UiEvent::Cancel => callbacks.cancel,
            UiEvent::Select(_) => callbacks.select,
        };
        let Some(key) = key else { return };
        let _watchdog = self.watchdog();
        match self.lua.registry_value::<Function>(&key) {
            Ok(func) => {
                let result = match event {
                    UiEvent::Select(i) => func.call::<()>((i + 1, item)),
                    _ => func.call::<()>(()),
                };
                if let Err(err) = result {
                    warn!("Lua ui-widget callback error: {err}");
                }
            }
            Err(err) => warn!("Lua ui-widget registry error: {err}"),
        }
    }

    pub fn take_spawns(&mut self) -> Vec<ProcessSpec> {
        self.shared.spawns.take()
    }

    /// The process manifest, if it changed since the last take. Forcing a
    /// take (reload: the fresh VM may declare *fewer* processes than the old
    /// one, which is itself a diff) goes through `mark_processes_dirty`.
    pub fn take_process_manifest(&mut self) -> Option<HashMap<String, ProcessDecl>> {
        self.shared
            .processes_dirty
            .take()
            .then(|| self.shared.processes.borrow().clone())
    }

    pub fn mark_processes_dirty(&self) {
        self.shared.processes_dirty.set(true);
    }

    pub fn take_ipc_broadcasts(&mut self) -> Vec<(String, serde_json::Value)> {
        self.shared.ipc_broadcasts.take()
    }

    pub fn has_screencast_hook(&self) -> bool {
        self.shared.screencast_hook.borrow().is_some()
    }

    pub fn take_screencast_replies(&mut self) -> Vec<(u64, ScreencastReply)> {
        self.shared.screencast_replies.take()
    }

    /// Run the `tomoe.on_screencast_request` hook for a portal source
    /// request. The caller wraps this like any other Lua entry and holds
    /// `token` for the waiting client; replies (immediate or deferred)
    /// arrive through `take_screencast_replies`.
    pub fn emit_screencast_request(
        &mut self,
        token: u64,
        app_id: String,
        monitor: bool,
        window: bool,
    ) -> ScreencastHookOutcome {
        let func = {
            let hook = self.shared.screencast_hook.borrow();
            let Some(key) = hook.as_ref() else {
                return ScreencastHookOutcome::Fallback;
            };
            match self.lua.registry_value::<Function>(key) {
                Ok(func) => func,
                Err(err) => {
                    warn!("Lua on_screencast_request registry error: {err}");
                    return ScreencastHookOutcome::Fallback;
                }
            }
        };
        let state = Rc::new(ScreencastRequestState {
            token,
            responded: Cell::new(false),
            deferred: Cell::new(false),
        });
        let req = LuaScreencastRequest {
            state: state.clone(),
            app_id,
            monitor,
            window,
            shared: self.shared.clone(),
        };
        let _watchdog = self.watchdog();
        let value = match func.call::<Value>(req) {
            Ok(value) => value,
            Err(err) => {
                warn!("Lua on_screencast_request error: {err}");
                return if state.responded.get() {
                    ScreencastHookOutcome::Answered
                } else {
                    ScreencastHookOutcome::Fallback
                };
            }
        };
        if state.responded.get() {
            return ScreencastHookOutcome::Answered;
        }
        let reply = match value {
            Value::Table(sel) => match parse_screencast_selection(&sel) {
                Some(reply) => reply,
                None => {
                    warn!(
                        "on_screencast_request: expected {{ output = name }} or \
                         {{ window = win }}; denying"
                    );
                    ScreencastReply::Deny
                }
            },
            Value::Boolean(false) => ScreencastReply::Deny,
            Value::Nil if state.deferred.get() => return ScreencastHookOutcome::Deferred,
            other => {
                warn!(
                    "on_screencast_request: returned {} without req:defer(); denying",
                    other.type_name()
                );
                ScreencastReply::Deny
            }
        };
        state.responded.set(true);
        self.shared
            .screencast_replies
            .borrow_mut()
            .push((token, reply));
        ScreencastHookOutcome::Answered
    }

    pub fn has_ipc_handler(&self, method: &str) -> bool {
        self.shared.ipc_handlers.borrow().contains_key(method)
    }

    /// Run a `tomoe.ipc.serve` handler. The caller wraps this like any other
    /// Lua entry (snapshot before, `after_lua` after). Errors come back as
    /// strings — they go to the requesting IPC client, not just the log.
    pub fn call_ipc_handler(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let func = {
            let handlers = self.shared.ipc_handlers.borrow();
            let key = handlers
                .get(method)
                .ok_or_else(|| format!("unknown method: {method}"))?;
            self.lua
                .registry_value::<Function>(key)
                .map_err(|err| format!("registry error: {err}"))?
        };
        let params = if params.is_null() {
            Value::Nil
        } else {
            self.lua
                .to_value(&params)
                .map_err(|err| format!("params conversion error: {err}"))?
        };
        let _watchdog = self.watchdog();
        let result = func
            .call::<Value>(params)
            .map_err(|err| format!("Lua error: {err}"))?;
        if matches!(result, Value::Nil) {
            return Ok(serde_json::Value::Null);
        }
        self.lua
            .from_value(result)
            .map_err(|err| format!("result not JSON-compatible: {err}"))
    }

    /// Run every `tomoe.on_reload` save hook (this is the outgoing VM),
    /// serializing the returned values to JSON — the only representation
    /// that can outlive the VM and cross into the fresh one.
    pub fn save_reload_state(&mut self) -> HashMap<String, serde_json::Value> {
        let funcs: Vec<(String, mlua::Result<Function>)> = {
            let hooks = self.shared.reload_hooks.borrow();
            hooks
                .iter()
                .map(|(name, h)| (name.clone(), self.lua.registry_value::<Function>(&h.save)))
                .collect()
        };
        let mut saved = HashMap::new();
        let _watchdog = self.watchdog();
        for (name, func) in funcs {
            let func = match func {
                Ok(func) => func,
                Err(err) => {
                    warn!("Lua on_reload({name:?}) registry error: {err}");
                    continue;
                }
            };
            match func.call::<Value>(()) {
                Ok(Value::Nil) => {}
                Ok(value) => match self.lua.from_value::<serde_json::Value>(value) {
                    Ok(json) => {
                        saved.insert(name, json);
                    }
                    Err(err) => warn!(
                        "on_reload({name:?}) save: value not JSON-compatible \
                         (persist ids, not window handles): {err}"
                    ),
                },
                Err(err) => warn!("Lua on_reload({name:?}) save error: {err}"),
            }
        }
        saved
    }

    /// Deliver saved state to this (fresh) VM's matching restore hooks.
    /// Returns how many hooks were invoked — zero means the caller should
    /// fall back to the on_window_open replay.
    pub fn restore_reload_state(&mut self, saved: &HashMap<String, serde_json::Value>) -> usize {
        let mut ran = 0;
        let _watchdog = self.watchdog();
        for (name, value) in saved {
            let func = {
                let hooks = self.shared.reload_hooks.borrow();
                let Some(h) = hooks.get(name) else { continue };
                match self.lua.registry_value::<Function>(&h.restore) {
                    Ok(func) => func,
                    Err(err) => {
                        warn!("Lua on_reload({name:?}) registry error: {err}");
                        continue;
                    }
                }
            };
            let value = match self.lua.to_value(value) {
                Ok(value) => value,
                Err(err) => {
                    warn!("on_reload({name:?}) restore: conversion error: {err}");
                    continue;
                }
            };
            // Count the attempt even on error: the hook may have mutated
            // state partway, so replaying on top could double-track windows.
            ran += 1;
            if let Err(err) = func.call::<()>(value) {
                warn!("Lua on_reload({name:?}) restore error: {err}");
            }
        }
        ran
    }

    /// Test-only access for docgen's API-parity checks.
    #[cfg(test)]
    pub(crate) fn lua(&self) -> &Lua {
        &self.lua
    }

    /// A Window userdata with no backing window, for enumerating its methods.
    #[cfg(test)]
    pub(crate) fn test_window(&self) -> mlua::Result<mlua::AnyUserData> {
        self.lua.create_userdata(LuaWindow {
            id: 0,
            shared: self.shared.clone(),
        })
    }

    pub fn has_window_open_hooks(&self) -> bool {
        !self.shared.hooks.borrow().window_open.is_empty()
    }

    pub fn has_window_rules(&self) -> bool {
        !self.shared.rules.borrow().is_empty()
    }

    pub fn has_window_close_hooks(&self) -> bool {
        !self.shared.hooks.borrow().window_close.is_empty()
    }

    pub fn has_pointer_button_hooks(&self) -> bool {
        !self.shared.hooks.borrow().pointer_button.is_empty()
    }

    pub fn has_pointer_axis_hooks(&self) -> bool {
        !self.shared.hooks.borrow().pointer_axis.is_empty()
    }

    pub fn has_window_request_hooks(&self) -> bool {
        !self.shared.hooks.borrow().window_request.is_empty()
    }

    pub fn has_hover_hooks(&self) -> bool {
        let hooks = self.shared.hooks.borrow();
        !hooks.pointer_enter.is_empty() || !hooks.pointer_leave.is_empty()
    }

    pub fn pointer_grab_active(&self) -> bool {
        self.shared.grab.borrow().is_some()
    }

    // ── Event emission ──

    pub fn call_bind(&mut self, idx: usize) {
        let key_valid = self.shared.bind_fns.borrow().len() > idx;
        if !key_valid {
            return;
        }
        let func = {
            let fns = self.shared.bind_fns.borrow();
            self.lua.registry_value::<Function>(&fns[idx])
        };
        let _watchdog = self.watchdog();
        match func {
            Ok(func) => {
                if let Err(err) = func.call::<()>(()) {
                    warn!("Lua bind error: {err}");
                }
            }
            Err(err) => warn!("Lua registry error: {err}"),
        }
    }

    pub fn emit_window_open(&mut self, id: u64) {
        // One budget for the hooks and the rule applies together.
        let _watchdog = self.watchdog();
        self.emit_window_event(id, |hooks| &hooks.window_open, "on_window_open");
        // Rule `apply` functions run after the hooks so they can refine
        // whatever placement the WM just queued (later ops win).
        self.apply_window_rules_inner(id);
    }

    /// Re-run only rule `apply` functions for an existing window. Config
    /// reload uses this after state restore: open hooks must not replay, but
    /// per-window rule properties belong to the fresh VM and must be rebuilt.
    pub fn reapply_window_rules(&mut self, id: u64) {
        let _watchdog = self.watchdog();
        self.apply_window_rules_inner(id);
    }

    /// Run the `apply` function of every rule matching the window — the
    /// function form of window rules. Rules are re-borrowed per iteration:
    /// matcher/apply functions are user code and may declare further rules.
    fn apply_window_rules_inner(&mut self, id: u64) {
        let len = self.shared.rules.borrow().len();
        for i in 0..len {
            if !rule_matches(&self.lua, &self.shared, i, id) {
                continue;
            }
            let func = {
                let rules = self.shared.rules.borrow();
                let Some(key) = rules.get(i).and_then(|r| r.apply.as_ref()) else {
                    continue;
                };
                match self.lua.registry_value::<Function>(key) {
                    Ok(func) => func,
                    Err(err) => {
                        warn!("Lua rule registry error: {err}");
                        continue;
                    }
                }
            };
            let win = LuaWindow {
                id,
                shared: self.shared.clone(),
            };
            if let Err(err) = func.call::<()>(win) {
                warn!("Lua rule apply error: {err}");
            }
        }
    }

    pub fn emit_window_close(&mut self, id: u64) {
        self.emit_window_event(id, |hooks| &hooks.window_close, "on_window_close");
    }

    pub fn emit_pointer_enter(&mut self, id: u64) {
        self.emit_window_event(id, |hooks| &hooks.pointer_enter, "on_pointer_enter");
    }

    pub fn emit_pointer_leave(&mut self, id: u64) {
        self.emit_window_event(id, |hooks| &hooks.pointer_leave, "on_pointer_leave");
    }

    pub fn emit_focus_change(&mut self, id: Option<u64>) {
        let keys: Vec<Function> = {
            let hooks = self.shared.hooks.borrow();
            hooks
                .focus_change
                .iter()
                .filter_map(|k| self.lua.registry_value::<Function>(k).ok())
                .collect()
        };
        let window = id.map(|id| LuaWindow {
            id,
            shared: self.shared.clone(),
        });
        let _watchdog = self.watchdog();
        for func in keys {
            if let Err(err) = func.call::<()>(window.clone()) {
                warn!("Lua on_focus_change error: {err}");
            }
        }
    }

    pub fn emit_outputs_changed(&mut self) {
        let keys: Vec<Function> = {
            let hooks = self.shared.hooks.borrow();
            hooks
                .outputs_changed
                .iter()
                .filter_map(|k| self.lua.registry_value::<Function>(k).ok())
                .collect()
        };
        let _watchdog = self.watchdog();
        for func in keys {
            if let Err(err) = func.call::<()>(()) {
                warn!("Lua on_outputs_changed error: {err}");
            }
        }
    }

    /// Shared fields of pointer event tables: positions, modifiers, window.
    fn pointer_event_table(
        &self,
        world: (f64, f64),
        screen: (f64, f64),
        mods: (bool, bool, bool, bool),
        window: Option<u64>,
    ) -> mlua::Result<Table> {
        let t = self.lua.create_table()?;
        t.set("x", world.0)?;
        t.set("y", world.1)?;
        t.set("sx", screen.0)?;
        t.set("sy", screen.1)?;
        let m = self.lua.create_table()?;
        m.set("alt", mods.0)?;
        m.set("ctrl", mods.1)?;
        m.set("shift", mods.2)?;
        m.set("super", mods.3)?;
        // "mod" mirrors whichever of the above `settings.mod` selects, so
        // configs stay modifier-agnostic.
        let mod_held = match self.shared.settings.borrow().mod_key {
            crate::input::ModKey::Alt => mods.0,
            crate::input::ModKey::Ctrl => mods.1,
            crate::input::ModKey::Shift => mods.2,
            crate::input::ModKey::Super => mods.3,
        };
        m.set("mod", mod_held)?;
        t.set("mods", m)?;
        if let Some(id) = window {
            t.set(
                "window",
                LuaWindow {
                    id,
                    shared: self.shared.clone(),
                },
            )?;
        }
        Ok(t)
    }

    /// Run event hooks with `ev`; a truthy return from any consumes the event.
    fn emit_consumable_hooks(
        &mut self,
        select: impl Fn(&Hooks) -> &Vec<RegistryKey>,
        ev: Table,
        name: &str,
    ) -> bool {
        let funcs: Vec<Function> = {
            let hooks = self.shared.hooks.borrow();
            select(&hooks)
                .iter()
                .filter_map(|k| self.lua.registry_value::<Function>(k).ok())
                .collect()
        };
        let mut consumed = false;
        let _watchdog = self.watchdog();
        for func in funcs {
            match func.call::<Value>(&ev) {
                Ok(value) => {
                    if !matches!(value, Value::Nil | Value::Boolean(false)) {
                        consumed = true;
                    }
                }
                Err(err) => warn!("Lua {name} error: {err}"),
            }
        }
        consumed
    }

    /// Returns true if a hook consumed the event (don't forward to clients).
    pub fn emit_pointer_button(&mut self, data: PointerButtonData) -> bool {
        let ev = match self.pointer_event_table(data.world, data.screen, data.mods, data.window) {
            Ok(ev) => ev,
            Err(err) => {
                warn!("Lua on_pointer_button error: {err}");
                return false;
            }
        };
        let ok = match button_name(data.button) {
            Some(name) => ev.set("button", name),
            None => ev.set("button", data.button),
        }
        .and_then(|()| ev.set("pressed", data.pressed));
        if let Err(err) = ok {
            warn!("Lua on_pointer_button error: {err}");
            return false;
        }
        self.emit_consumable_hooks(|hooks| &hooks.pointer_button, ev, "on_pointer_button")
    }

    /// Returns true if a hook consumed the event (don't forward to clients).
    pub fn emit_pointer_axis(&mut self, data: PointerAxisData) -> bool {
        let ev = match self.pointer_event_table(data.world, data.screen, data.mods, data.window) {
            Ok(ev) => ev,
            Err(err) => {
                warn!("Lua on_pointer_axis error: {err}");
                return false;
            }
        };
        if let Err(err) = ev.set("dx", data.dx).and_then(|()| ev.set("dy", data.dy)) {
            warn!("Lua on_pointer_axis error: {err}");
            return false;
        }
        self.emit_consumable_hooks(|hooks| &hooks.pointer_axis, ev, "on_pointer_axis")
    }

    /// A client asked for a state change or an interactive drag (`ev.type`
    /// is "fullscreen", "unfullscreen", "maximize", "unmaximize", "minimize",
    /// "move", "resize", "activate", or "urgent"; `ev.output` names the
    /// output a fullscreen request targeted; `ev.edges` names the edge/corner
    /// a resize drags, e.g. "bottom_right"). Returns true if a hook consumed
    /// the request — the consumer takes over responding, typically via
    /// `win:set_fullscreen` + `win:set_geometry`, or `tomoe.grab_pointer` for
    /// move/resize; unconsumed requests get the native default (drags are
    /// dropped, xdg-activation "activate" focuses, "urgent" is a no-op).
    pub fn emit_window_request(
        &mut self,
        id: u64,
        kind: &str,
        output: Option<String>,
        edges: Option<&str>,
    ) -> bool {
        let ev = match self.lua.create_table() {
            Ok(t) => t,
            Err(err) => {
                warn!("Lua on_window_request error: {err}");
                return false;
            }
        };
        let ok = ev
            .set(
                "window",
                LuaWindow {
                    id,
                    shared: self.shared.clone(),
                },
            )
            .and_then(|()| ev.set("type", kind))
            .and_then(|()| match output {
                Some(name) => ev.set("output", name),
                None => Ok(()),
            })
            .and_then(|()| match edges {
                Some(edges) => ev.set("edges", edges),
                None => Ok(()),
            });
        if let Err(err) = ok {
            warn!("Lua on_window_request error: {err}");
            return false;
        }
        self.emit_consumable_hooks(|hooks| &hooks.window_request, ev, "on_window_request")
    }

    /// Feed a motion event to the active grab (world coordinates + deltas).
    pub fn emit_grab_motion(&mut self, x: f64, y: f64, dx: f64, dy: f64) {
        let func = {
            let grab = self.shared.grab.borrow();
            let Some(grab) = grab.as_ref() else { return };
            self.lua.registry_value::<Function>(&grab.motion)
        };
        let func = match func {
            Ok(func) => func,
            Err(err) => {
                warn!("Lua grab registry error: {err}");
                return;
            }
        };
        let ev = match self.lua.create_table() {
            Ok(t) => t,
            Err(err) => {
                warn!("Lua grab motion error: {err}");
                return;
            }
        };
        let ok = ev
            .set("x", x)
            .and_then(|()| ev.set("y", y))
            .and_then(|()| ev.set("dx", dx))
            .and_then(|()| ev.set("dy", dy));
        if let Err(err) = ok {
            warn!("Lua grab motion error: {err}");
            return;
        }
        let _watchdog = self.watchdog();
        if let Err(err) = func.call::<()>(ev) {
            warn!("Lua grab motion error: {err}");
        }
    }

    /// End the active grab (button release), running its release callback.
    pub fn end_pointer_grab(&mut self) {
        let Some(grab) = self.shared.grab.borrow_mut().take() else {
            return;
        };
        let Some(release) = grab.release else { return };
        let _watchdog = self.watchdog();
        match self.lua.registry_value::<Function>(&release) {
            Ok(func) => {
                if let Err(err) = func.call::<()>(()) {
                    warn!("Lua grab release error: {err}");
                }
            }
            Err(err) => warn!("Lua grab registry error: {err}"),
        }
    }

    fn emit_window_event(
        &mut self,
        id: u64,
        select: impl Fn(&Hooks) -> &Vec<RegistryKey>,
        name: &str,
    ) {
        let keys: Vec<Function> = {
            let hooks = self.shared.hooks.borrow();
            select(&hooks)
                .iter()
                .filter_map(|k| self.lua.registry_value::<Function>(k).ok())
                .collect()
        };
        let window = LuaWindow {
            id,
            shared: self.shared.clone(),
        };
        let _watchdog = self.watchdog();
        for func in keys {
            if let Err(err) = func.call::<()>(window.clone()) {
                warn!("Lua {name} error: {err}");
            }
        }
    }
}

/// Resolve the config file location: an explicit CLI path wins, else the XDG
/// path if it exists, else None (the embedded default config). Re-resolved on
/// every reload check so creating/removing the user config is itself a change.
pub fn resolve_config_path(cli: Option<&Path>) -> Option<PathBuf> {
    cli.map(PathBuf::from).or_else(|| {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        let candidate = base.join("tomoe/init.lua");
        candidate.exists().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_widget_ops_queue() {
        let mut rt = LuaRuntime::new().unwrap();
        rt.lua
            .load(
                r#"
                local m = tomoe.ui.menu {
                    title = "t",
                    items = { "a", "b" },
                    on_select = function() end,
                }
                m:close()
                assert(tomoe.ui.menu { items = {} } == nil)
                assert(tomoe.ui.confirm { text = "sure?" })
                assert(tomoe.ui.toast { text = "hi", duration = 2 })
                assert(tomoe.ui.sheet { rows = { { "Mod+Q", "Quit" } } })
                "#,
            )
            .exec()
            .unwrap();
        let ops = rt.take_ui_ops();
        assert_eq!(ops.len(), 5);
        let UiOp::Open { id, spec } = &ops[0] else {
            panic!("expected Open, got {:?}", ops[0]);
        };
        assert_eq!(
            *spec,
            WidgetSpec::Menu {
                title: Some("t".into()),
                items: vec!["a".into(), "b".into()],
            }
        );
        assert_eq!(ops[1], UiOp::Close(*id));
        assert!(matches!(
            &ops[2],
            UiOp::Open { spec: WidgetSpec::Confirm { text }, .. } if text == "sure?"
        ));
        assert!(matches!(
            &ops[3],
            UiOp::Open {
                spec: WidgetSpec::Toast { duration, urgent: false, .. },
                ..
            } if *duration == std::time::Duration::from_secs(2)
        ));
        assert!(matches!(
            &ops[4],
            UiOp::Open { spec: WidgetSpec::Sheet { rows, .. }, .. } if rows.len() == 1
        ));
        // The menu's callbacks are registered; the closed... entry keeps them
        // until the core applies the Close op and drops them.
        assert!(rt.shared.ui_callbacks.borrow().contains_key(id));
        rt.drop_ui_callbacks(*id);
        assert!(!rt.shared.ui_callbacks.borrow().contains_key(id));
    }

    #[test]
    fn hover_api_registers() {
        let rt = LuaRuntime::new().unwrap();
        rt.lua
            .load(
                r#"
                tomoe.settings { focus_follows_mouse = true }
                tomoe.on_pointer_enter(function(win) end)
                tomoe.on_pointer_leave(function(win) end)
                "#,
            )
            .exec()
            .unwrap();
        assert!(rt.settings().focus_follows_mouse);
        assert!(rt.has_hover_hooks());
    }

    #[test]
    fn parse_wait_for_frame_completion() {
        let rt = LuaRuntime::new().unwrap();
        assert!(!rt.settings().wait_for_frame_completion);
        rt.lua
            .load(r#"tomoe.settings { wait_for_frame_completion = true }"#)
            .exec()
            .unwrap();
        assert!(rt.settings().wait_for_frame_completion);
        // A later partial settings call must not reset it (Option<bool>
        // parse: a missing key is not `false`).
        rt.lua
            .load(r#"tomoe.settings { gaps = 4 }"#)
            .exec()
            .unwrap();
        assert!(rt.settings().wait_for_frame_completion);
    }

    #[test]
    fn watchdog_aborts_runaway_entry() {
        let mut rt = LuaRuntime::new().unwrap();
        assert_eq!(rt.settings().watchdog_ms, 1000);
        rt.lua
            .load(
                r#"
                tomoe.settings { watchdog_ms = 30 }
                tomoe.ipc.serve("ok", function() return 1 end)
                tomoe.ipc.serve("spin", function() while true do end end)
                "#,
            )
            .exec()
            .unwrap();
        assert_eq!(rt.settings().watchdog_ms, 30);
        // A well-behaved entry passes untouched…
        assert!(rt.call_ipc_handler("ok", serde_json::Value::Null).is_ok());
        // …a runaway one is aborted (this also proves the jit.off()
        // enforcement: a compiled trace would never fire the hook).
        let err = rt
            .call_ipc_handler("spin", serde_json::Value::Null)
            .unwrap_err();
        assert!(err.contains("watchdog"), "unexpected error: {err}");
        // The deadline disarmed on exit: the next entry gets a fresh budget.
        assert!(rt.shared.watchdog_deadline.get().is_none());
        assert!(rt.call_ipc_handler("ok", serde_json::Value::Null).is_ok());
    }

    #[test]
    fn parse_input_settings() {
        let rt = LuaRuntime::new().unwrap();
        rt.lua
            .load(
                r#"
                tomoe.settings {
                  keyboard = {
                    layout = "us,de",
                    options = "caps:escape",
                    repeat_delay = 300,
                    repeat_rate = 50,
                  },
                  touchpad = {
                    tap = true,
                    natural_scroll = true,
                    accel_speed = 2.0, -- clamped to 1.0
                    accel_profile = "flat",
                    dwt = false,
                  },
                  mouse = {
                    accel_profile = "bogus", -- warns, stays unset
                    scroll_method = "on_button_down",
                    scroll_button = 274,
                  },
                  devices = {
                    ["Some Fancy Mouse"] = { natural_scroll = true },
                  },
                }
                "#,
            )
            .exec()
            .unwrap();
        let s = rt.settings();
        assert_eq!(s.keyboard.layout, "us,de");
        assert_eq!(s.keyboard.variant, "");
        assert_eq!(s.keyboard.options.as_deref(), Some("caps:escape"));
        assert_eq!(s.keyboard.repeat_delay, 300);
        assert_eq!(s.keyboard.repeat_rate, 50);

        let tp = s.input.touchpad;
        assert_eq!(tp.tap, Some(true));
        assert_eq!(tp.natural_scroll, Some(true));
        assert_eq!(tp.accel_speed, Some(1.0));
        assert_eq!(tp.accel_profile, Some(AccelProfile::Flat));
        assert_eq!(tp.dwt, Some(false));
        assert_eq!(tp.left_handed, None);

        let mouse = s.input.mouse;
        assert_eq!(mouse.accel_profile, None);
        assert_eq!(mouse.scroll_method, Some(ScrollMethod::OnButtonDown));
        assert_eq!(mouse.scroll_button, Some(274));

        // Per-device tables override class settings field-by-field.
        let per = &s.input.devices["Some Fancy Mouse"];
        let merged = mouse.overridden_by(per);
        assert_eq!(merged.natural_scroll, Some(true));
        assert_eq!(merged.scroll_button, Some(274));
    }

    #[test]
    fn parse_process_manifest() {
        let mut rt = LuaRuntime::new().unwrap();
        rt.lua
            .load(
                r#"
                tomoe.process.once("fcitx5", { command = {"fcitx5", "-d"} })
                tomoe.process.once("wall", {
                  shell = "swaybg -i ~/wall.png",
                  run = "once_per_config_version",
                })
                tomoe.process.service("waybar", { restart = "on-exit" })
                tomoe.process.service("mako", {
                  command = {"mako"},
                  restart = "on_failure",
                  reload = "always_restart",
                  cwd = "sub/dir",
                  env = { MAKO_DEBUG = "1" },
                })
                tomoe.process.spawn { command = {"notify-send", "hello"} }
                tomoe.spawn("foot")
                "#,
            )
            .exec()
            .unwrap();

        let manifest = rt.take_process_manifest().expect("manifest is dirty");
        assert!(rt.take_process_manifest().is_none(), "take clears dirty");
        assert_eq!(manifest.len(), 4);

        let ProcessDecl::Once { spec, run } = &manifest["fcitx5"] else {
            panic!("fcitx5 should be a once entry");
        };
        assert_eq!(
            spec.launch,
            Launch::Argv(vec!["fcitx5".into(), "-d".into()])
        );
        assert_eq!(*run, RunPolicy::OncePerSession);

        let ProcessDecl::Once { run, .. } = &manifest["wall"] else {
            panic!("wall should be a once entry");
        };
        assert_eq!(*run, RunPolicy::OncePerConfigVersion);

        // No command: the id is the command.
        let ProcessDecl::Service {
            spec,
            restart,
            reload,
        } = &manifest["waybar"]
        else {
            panic!("waybar should be a service entry");
        };
        assert_eq!(spec.launch, Launch::Argv(vec!["waybar".into()]));
        assert_eq!(*restart, RestartPolicy::OnExit);
        assert_eq!(*reload, ReloadPolicy::KeepIfUnchanged);

        let ProcessDecl::Service {
            spec,
            restart,
            reload,
        } = &manifest["mako"]
        else {
            panic!("mako should be a service entry");
        };
        assert_eq!(*restart, RestartPolicy::OnFailure);
        assert_eq!(*reload, ReloadPolicy::AlwaysRestart);
        assert_eq!(spec.cwd.as_deref(), Some(std::path::Path::new("sub/dir")));
        assert_eq!(spec.env.get("MAKO_DEBUG").map(String::as_str), Some("1"));

        let spawns = rt.take_spawns();
        assert_eq!(spawns.len(), 2);
        assert_eq!(
            spawns[0].launch,
            Launch::Argv(vec!["notify-send".into(), "hello".into()])
        );
        assert_eq!(spawns[1].launch, Launch::Shell("foot".into()));
    }

    #[test]
    fn ipc_serve_and_broadcast() {
        let mut rt = LuaRuntime::new().unwrap();
        rt.lua
            .load(
                r#"
                tomoe.ipc.serve("echo", function(params)
                  return { got = params.value, n = params.n + 1 }
                end)
                tomoe.ipc.serve("boom", function() error("nope") end)
                tomoe.ipc.broadcast("workspace/active", { name = "2" })
                tomoe.ipc.broadcast("ping")
                "#,
            )
            .exec()
            .unwrap();

        assert!(rt.has_ipc_handler("echo"));
        assert!(!rt.has_ipc_handler("missing"));

        let result = rt
            .call_ipc_handler("echo", serde_json::json!({ "value": "hi", "n": 1 }))
            .unwrap();
        assert_eq!(result, serde_json::json!({ "got": "hi", "n": 2 }));

        let err = rt
            .call_ipc_handler("boom", serde_json::Value::Null)
            .unwrap_err();
        assert!(err.contains("nope"), "{err}");

        let broadcasts = rt.take_ipc_broadcasts();
        assert_eq!(broadcasts.len(), 2);
        assert_eq!(broadcasts[0].0, "workspace/active");
        assert_eq!(broadcasts[0].1, serde_json::json!({ "name": "2" }));
        assert_eq!(broadcasts[1].1, serde_json::Value::Null);
        assert!(rt.take_ipc_broadcasts().is_empty());
    }

    /// State saved from one VM's `tomoe.on_reload` save hooks crosses into
    /// a fresh VM's restore hooks (the reload path), keyed by name; unmatched
    /// keys deliver nothing and mismatched configs fall back to replay (0).
    #[test]
    fn reload_state_round_trip() {
        let mut old = LuaRuntime::new().unwrap();
        old.lua
            .load(
                r#"
                tomoe.on_reload("wm", function()
                  return { active = 3, ids = { 10, 20 } }
                end, function() end)
                tomoe.on_reload("gone", function() return { x = 1 } end, function() end)
                tomoe.on_reload("nothing", function() return nil end, function() end)
                "#,
            )
            .exec()
            .unwrap();
        let saved = old.save_reload_state();
        assert_eq!(saved.len(), 2, "nil saves are dropped");
        assert_eq!(
            saved["wm"],
            serde_json::json!({ "active": 3, "ids": [10, 20] })
        );

        let mut new = LuaRuntime::new().unwrap();
        new.lua
            .load(
                r#"
                restored = nil
                tomoe.on_reload("wm", function() end, function(state)
                  restored = state
                end)
                "#,
            )
            .exec()
            .unwrap();
        assert_eq!(
            new.restore_reload_state(&saved),
            1,
            "only matching keys run"
        );
        let (active, first_id): (i64, i64) = new
            .lua
            .load("return restored.active, restored.ids[1]")
            .eval()
            .unwrap();
        assert_eq!((active, first_id), (3, 10));

        // A config with no on_reload restores nothing — the replay fallback.
        let mut plain = LuaRuntime::new().unwrap();
        assert_eq!(plain.restore_reload_state(&saved), 0);
    }

    /// Rules match on app_id/title patterns and predicates; `rules_for`
    /// merges data props (later rules win, reserved keys excluded); `apply`
    /// functions run from emit_window_open for matching windows only.
    #[test]
    fn window_rules() {
        let mut rt = LuaRuntime::new().unwrap();
        rt.lua
            .load(
                r##"
                tomoe.rule { app_id = "^mpv$", fullscreen = true }
                tomoe.rule { title = "Fire", workspace = 3, focus = false }
                tomoe.rule { app_id = "fire", workspace = 5 }
                tomoe.rule {
                  match = function(w) return w:app_id() == "foot" end,
                  apply = function(w)
                    w:set_geometry(1, 2, 300, 200)
                    w:set_properties {
                      radius = 18,
                      tearing = true,
                      border = { focused = "#ff0000", unfocused = "#00800080" },
                    }
                  end,
                }
                "##,
            )
            .exec()
            .unwrap();
        assert!(rt.has_window_rules());

        let props = |app_id: &str, title: &str| WinProps {
            app_id: app_id.into(),
            title: title.into(),
            mapped: true,
            ..Default::default()
        };
        let windows = HashMap::from([
            (1, props("mpv", "video")),
            (2, props("firefox", "Mozilla Firefox")),
            (3, props("foot", "~")),
        ]);
        rt.sync(windows, Vec::new(), (0, 0, 1.0), (0.0, 0.0, 0.0, 0.0));

        let (mpv_ok, ff_ok, foot_ok): (bool, bool, bool) = rt
            .lua
            .load(
                r#"
                local r1 = tomoe.rules_for(tomoe.window(1))
                local r2 = tomoe.rules_for(tomoe.window(2))
                local r3 = tomoe.rules_for(tomoe.window(3))
                return r1.fullscreen == true and r1.workspace == nil,
                       r2.workspace == 5 and r2.focus == false
                         and r2.title == nil and r2.apply == nil,
                       r3.workspace == nil and r3.fullscreen == nil
                "#,
            )
            .eval()
            .unwrap();
        assert!(mpv_ok, "anchored app_id pattern + data prop");
        assert!(ff_ok, "later rule wins the merge; reserved keys excluded");
        assert!(foot_ok, "predicate rules contribute no data props here");

        // apply runs only for the matching window, via emit_window_open.
        rt.emit_window_open(1);
        assert!(rt.take_ops().is_empty(), "mpv rule has no apply");
        rt.emit_window_open(3);
        let ops = rt.take_ops();
        assert!(matches!(
            ops.first(),
            Some(WindowOp::SetGeometry(3, (1, 2, 300, 200)))
        ));
        let Some(WindowOp::SetProperties(3, props)) = ops.get(1) else {
            panic!("missing queued properties: {ops:?}");
        };
        assert_eq!(props.radius, Some(18));
        assert_eq!(props.tearing, Some(true));
        assert_eq!(props.border_focused, parse_color("#ff0000"));
        assert_eq!(props.border_unfocused, parse_color("#00800080"));

        // Replacement semantics make clearing deterministic: an empty table
        // drops every override back to global settings.
        rt.lua
            .load("tomoe.window(3):set_properties({})")
            .exec()
            .unwrap();
        assert!(matches!(
            rt.take_ops().as_slice(),
            [WindowOp::SetProperties(
                3,
                WindowProperties {
                    radius: None,
                    tearing: None,
                    border_focused: None,
                    border_unfocused: None
                }
            )]
        ));
        assert!(rt
            .lua
            .load(r#"tomoe.window(3):set_properties { border = { focused = "nope" } }"#)
            .exec()
            .is_err());
    }

    /// Hook return values map to replies; defer keeps the request open for
    /// a later Lua entry; a request answers exactly once.
    #[test]
    fn screencast_request_hook() {
        let mut rt = LuaRuntime::new().unwrap();
        assert!(!rt.has_screencast_hook());
        assert_eq!(
            rt.emit_screencast_request(0, "obs".into(), true, false),
            ScreencastHookOutcome::Fallback,
            "no hook registered"
        );

        // Synchronous resolve by return value.
        rt.lua
            .load(
                r#"tomoe.on_screencast_request(function(req)
                     assert(req.app_id == "obs")
                     assert(req.types.monitor and not req.types.window)
                     return { output = "DP-1" }
                   end)"#,
            )
            .exec()
            .unwrap();
        assert!(rt.has_screencast_hook());
        assert_eq!(
            rt.emit_screencast_request(1, "obs".into(), true, false),
            ScreencastHookOutcome::Answered
        );
        let replies = rt.take_screencast_replies();
        assert!(
            matches!(&replies[..], [(1, ScreencastReply::Output(name))] if name == "DP-1"),
            "{replies:?}"
        );

        // Deny by returning false (re-registering replaces the handler).
        rt.lua
            .load(r#"tomoe.on_screencast_request(function() return false end)"#)
            .exec()
            .unwrap();
        assert_eq!(
            rt.emit_screencast_request(2, String::new(), true, true),
            ScreencastHookOutcome::Answered
        );
        assert!(matches!(
            rt.take_screencast_replies()[..],
            [(2, ScreencastReply::Deny)]
        ));

        // Deferred: the reply arrives from a later Lua entry.
        rt.lua
            .load(
                r#"
                local pending
                tomoe.on_screencast_request(function(req)
                  req:defer()
                  pending = req
                end)
                function _resolve_later() pending:resolve({ window = 7 }) end
                "#,
            )
            .exec()
            .unwrap();
        assert_eq!(
            rt.emit_screencast_request(3, "firefox".into(), true, true),
            ScreencastHookOutcome::Deferred
        );
        assert!(rt.take_screencast_replies().is_empty());
        rt.lua.load("_resolve_later()").exec().unwrap();
        assert!(matches!(
            rt.take_screencast_replies()[..],
            [(3, ScreencastReply::Window(7))]
        ));
        // A second answer to the same request is ignored.
        rt.lua.load("_resolve_later()").exec().unwrap();
        assert!(rt.take_screencast_replies().is_empty());
    }

    /// The shipped "screencast" module: rules and single candidates resolve
    /// without a menu; multiple candidates defer to a tomoe.ui.menu.
    #[test]
    fn screencast_module_policy() {
        let mut rt = LuaRuntime::new().unwrap();
        rt.lua.load(r#"require("screencast")"#).exec().unwrap();
        let output = |name: &str| OutputProps {
            name: name.into(),
            geometry: (0, 0, 1920, 1080),
            usable: (0, 0, 1920, 1080),
        };

        // One candidate output: resolved without asking.
        rt.sync(
            HashMap::new(),
            vec![output("DP-1")],
            (0, 0, 1.0),
            (0.0, 0.0, 0.0, 0.0),
        );
        assert_eq!(
            rt.emit_screencast_request(1, "obs".into(), true, false),
            ScreencastHookOutcome::Answered
        );
        let replies = rt.take_screencast_replies();
        assert!(
            matches!(&replies[..], [(1, ScreencastReply::Output(name))] if name == "DP-1"),
            "{replies:?}"
        );
        assert!(rt.take_ui_ops().is_empty(), "no menu for a single source");

        // Two outputs: deferred to a menu; selecting resolves the request.
        rt.sync(
            HashMap::new(),
            vec![output("DP-1"), output("DP-2")],
            (0, 0, 1.0),
            (0.0, 0.0, 0.0, 0.0),
        );
        assert_eq!(
            rt.emit_screencast_request(2, "obs".into(), true, false),
            ScreencastHookOutcome::Deferred
        );
        let ops = rt.take_ui_ops();
        let [UiOp::Open {
            id,
            spec: WidgetSpec::Menu { items, .. },
        }] = &ops[..]
        else {
            panic!("expected a menu, got {ops:?}");
        };
        assert_eq!(items.len(), 2);
        rt.emit_ui_event(*id, UiEvent::Select(1), Some(items[1].clone()));
        let replies = rt.take_screencast_replies();
        assert!(
            matches!(&replies[..], [(2, ScreencastReply::Output(name))] if name == "DP-2"),
            "{replies:?}"
        );

        // A window rule with a screencast prop answers for the app.
        rt.lua
            .load(r#"tomoe.rule { app_id = "^obs$", screencast = false }"#)
            .exec()
            .unwrap();
        rt.sync(
            HashMap::from([(
                5,
                WinProps {
                    app_id: "obs".into(),
                    mapped: true,
                    ..Default::default()
                },
            )]),
            vec![output("DP-1"), output("DP-2")],
            (0, 0, 1.0),
            (0.0, 0.0, 0.0, 0.0),
        );
        assert_eq!(
            rt.emit_screencast_request(3, "obs".into(), true, false),
            ScreencastHookOutcome::Answered
        );
        assert!(matches!(
            rt.take_screencast_replies()[..],
            [(3, ScreencastReply::Deny)]
        ));
    }

    #[test]
    fn parse_resolution() {
        let parse = |s: &str| s.parse::<Resolution>();
        let res = |size, refresh| Resolution { size, refresh };

        assert_eq!(
            parse("preferred"),
            Ok(res(SizeSetting::Preferred, RefreshSetting::Auto))
        );
        assert_eq!(
            parse("max"),
            Ok(res(SizeSetting::Max, RefreshSetting::Auto))
        );
        assert_eq!(
            parse("max@max"),
            Ok(res(SizeSetting::Max, RefreshSetting::Max))
        );
        assert_eq!(
            parse("2560x1440"),
            Ok(res(SizeSetting::Exact(2560, 1440), RefreshSetting::Auto))
        );
        assert_eq!(
            parse("2560x1440@144"),
            Ok(res(
                SizeSetting::Exact(2560, 1440),
                RefreshSetting::Exact(144_000)
            ))
        );
        assert_eq!(
            parse("1920x1080@59.94"),
            Ok(res(
                SizeSetting::Exact(1920, 1080),
                RefreshSetting::Exact(59_940)
            ))
        );
        assert_eq!(
            parse("preferred@max"),
            Ok(res(SizeSetting::Preferred, RefreshSetting::Max))
        );

        for bad in [
            "",
            "1920",
            "1920x",
            "x1080",
            "1920x1080@",
            "max@",
            "60@max",
            "1920x1080@-60",
        ] {
            assert_eq!(parse(bad), Err(()), "{bad:?} should not parse");
        }
    }

    /// Every shipped example config must load cleanly against the current
    /// API — examples that rot are worse than none. Loading is side-effect
    /// free by construction (spawns live in binds, the process manifest and
    /// ui/ipc registrations only queue), so this runs headless.
    #[test]
    fn example_configs_load() {
        const EXAMPLES: &[(&str, &str)] = &[
            (
                "extension-surface-init.lua",
                include_str!("../../../resources/examples/extension-surface-init.lua"),
            ),
            (
                "zoomer-init.lua",
                include_str!("../../../resources/examples/zoomer-init.lua"),
            ),
        ];
        for (name, code) in EXAMPLES {
            let rt = LuaRuntime::new().unwrap();
            rt.lua
                .load(*code)
                .set_name(*name)
                .exec()
                .unwrap_or_else(|err| panic!("{name} failed to load: {err}"));
        }

        // The extension-surface example claims to exercise the whole M4
        // surface (PLAN.md M4 §5); hold it to that.
        let mut rt = LuaRuntime::new().unwrap();
        rt.lua
            .load(EXAMPLES[0].1)
            .set_name(EXAMPLES[0].0)
            .exec()
            .unwrap();
        assert!(rt.has_window_rules(), "rules declared");
        assert!(rt.has_window_open_hooks(), "hooks installed");
        assert!(
            rt.shared.screencast_hook.borrow().is_some(),
            "screencast policy registered"
        );
        assert!(
            rt.shared.reload_hooks.borrow().contains_key("scratchpad"),
            "on_reload persistence registered"
        );
        let manifest = rt.take_process_manifest().expect("manifest declared");
        assert!(manifest.contains_key("waybar") && manifest.contains_key("wallpaper"));
        // The IPC endpoint answers headless from the wm module's tables.
        let state = rt
            .call_ipc_handler("workspace/state", serde_json::Value::Null)
            .unwrap();
        assert_eq!(state["active"], 1);
        assert!(rt.has_ipc_handler("workspace/switch"));
    }
}
