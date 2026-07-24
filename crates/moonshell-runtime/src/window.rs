//! `shell.window` options and the window handle userdata.
//!
//! [`parse_options`] speaks nur's window-config vocabulary and produces
//! `surface`'s [`LayerOptions`] plus the paint defaults (`bg`, and
//! `fg`/`font_size` as [`TextDefaults`]) that flow into the painter.
//!
//! Two modes, exactly as in nur:
//! - **bar mode** (default): `position = "top"|"bottom"|"left"|"right"`
//!   docks a full-length bar; `height` (or `width` for side bars) is
//!   its thickness; `exclusive = true` (default) reserves that space.
//! - **popup mode**: an `anchor` string (`"top-right"`, `"bottom-left"`,
//!   `"top"`, …) anchors a fixed-size surface (`popup_width` x
//!   `height`) with `margin_*` offsets.
//!
//! Divergence from nur: bar mode stretches along the anchored axis
//! (size 0 → the compositor supplies the real size per output) instead
//! of reading the primary display's bounds — multi-monitor correct by
//! construction. Unknown `anchor` strings are an error here (nur fell
//! back to top-right silently); config mistakes should point at
//! themselves.
//!
//! `name` is parsed and used as the surface namespace; when set, the
//! `shell.window` call also registers the handle in [`ShellCtx`]'s
//! named-window registry behind `shell.get_window`.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::prelude::*;
use moonshell_render::Rgba;
use moonshell_surface::{Anchors, Keyboard, Layer, LayerOptions, Margins};

use crate::api::ShellCtx;
use crate::element::{parse_color, TextDefaults};

/// nur's default window palette (Catppuccin Mocha base/text) and font
/// size — applied when the config omits `bg`/`fg`/`font_size`.
const DEFAULT_BG: Rgba = Rgba::new(0x1e, 0x1e, 0x2e, 0xff);
const DEFAULT_FG: Rgba = Rgba::new(0xcd, 0xd6, 0xf4, 0xff);
const DEFAULT_FONT_SIZE: f32 = 13.0;

/// Everything a `shell.window(opts)` table resolves to.
#[derive(Debug)]
pub struct WindowOpts {
    pub layer: LayerOptions,
    /// Explicit `name` from the opts table — registers the window for
    /// `shell.get_window(name)`. (The namespace defaults to
    /// "moonshell" either way.)
    pub name: Option<String>,
    /// Window background, painted behind the Lua element tree.
    pub bg: Rgba,
    /// `fg`/`font_size`, inherited by text nodes that don't set their own.
    pub text: TextDefaults,
}

/// Per-window state shared between the Lua-side [`WindowHandle`] and
/// the loop-side `LuaPainter`.
pub struct WindowShared {
    /// The stored render callback (`handle:render(fn)`); `None` paints
    /// the bare background.
    pub render_key: Option<LuaRegistryKey>,
    pub bg: Rgba,
    pub text: TextDefaults,
    /// `on_click` handlers of the last rendered tree, keyed by element
    /// path (see `element::collect_handlers`); refreshed per render.
    pub handlers: std::collections::HashMap<String, LuaRegistryKey>,
}

/// The userdata `shell.window` returns. `handle:render(fn)` stores the
/// render function (as a registry key — it outlives this stack frame)
/// and requests a repaint.
pub struct WindowHandle {
    shared: Rc<RefCell<WindowShared>>,
    ctx: Rc<ShellCtx>,
}

impl WindowHandle {
    pub(crate) fn new(shared: Rc<RefCell<WindowShared>>, ctx: Rc<ShellCtx>) -> Self {
        Self { shared, ctx }
    }
}

impl LuaUserData for WindowHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("render", |lua, this, f: LuaFunction| {
            let key = lua.create_registry_value(f)?;
            this.shared.borrow_mut().render_key = Some(key);
            this.ctx.mark_dirty();
            Ok(())
        });
    }
}

/// Parse a `shell.window(opts)` table. See the module docs for the
/// vocabulary.
pub fn parse_options(t: &LuaTable) -> LuaResult<WindowOpts> {
    // Thickness of a bar / height of a popup. nur reads `height` first,
    // `width` as the fallback (side bars), default 32.
    let size: f32 = match t.get::<Option<f32>>("height")? {
        Some(h) => h,
        None => t.get::<Option<f32>>("width")?.unwrap_or(32.0),
    };
    let size_px = size.round().max(0.0) as u32;

    let exclusive: bool = t.get::<Option<bool>>("exclusive")?.unwrap_or(true);
    let layer = match t
        .get::<Option<String>>("layer")?
        .unwrap_or_default()
        .as_str()
    {
        "background" => Layer::Background,
        "bottom" => Layer::Bottom,
        "overlay" => Layer::Overlay,
        _ => Layer::Top,
    };
    let keyboard = match t
        .get::<Option<String>>("keyboard")?
        .unwrap_or_default()
        .as_str()
    {
        "exclusive" => Keyboard::Exclusive,
        "on_demand" => Keyboard::OnDemand,
        _ => Keyboard::None,
    };

    let (anchors, width, height, margins) =
        if let Some(anchor) = t.get::<Option<String>>("anchor")? {
            // Popup mode: fixed size at an anchored corner/edge.
            let anchors = parse_anchor(&anchor)?;
            let popup_width: f32 = t.get::<Option<f32>>("popup_width")?.unwrap_or(320.0);
            let margins = Margins {
                top: t.get::<Option<f32>>("margin_top")?.unwrap_or(0.0).round() as i32,
                right: t.get::<Option<f32>>("margin_right")?.unwrap_or(0.0).round() as i32,
                bottom: t
                    .get::<Option<f32>>("margin_bottom")?
                    .unwrap_or(0.0)
                    .round() as i32,
                left: t.get::<Option<f32>>("margin_left")?.unwrap_or(0.0).round() as i32,
            };
            (
                anchors,
                popup_width.round().max(0.0) as u32,
                size_px,
                margins,
            )
        } else {
            // Bar mode: anchored across an edge, stretching along it.
            let position = t.get::<Option<String>>("position")?.unwrap_or_default();
            let (anchors, w, h) = match position.as_str() {
                "bottom" => (
                    Anchors {
                        bottom: true,
                        left: true,
                        right: true,
                        ..Anchors::default()
                    },
                    0,
                    size_px,
                ),
                "left" => (
                    Anchors {
                        top: true,
                        bottom: true,
                        left: true,
                        ..Anchors::default()
                    },
                    size_px,
                    0,
                ),
                "right" => (
                    Anchors {
                        top: true,
                        bottom: true,
                        right: true,
                        ..Anchors::default()
                    },
                    size_px,
                    0,
                ),
                // nur: anything else (including absent) is a top bar.
                _ => (
                    Anchors {
                        top: true,
                        left: true,
                        right: true,
                        ..Anchors::default()
                    },
                    0,
                    size_px,
                ),
            };
            (anchors, w, h, Margins::default())
        };

    let name: Option<String> = t.get("name")?;

    Ok(WindowOpts {
        layer: LayerOptions {
            namespace: name.clone().unwrap_or_else(|| "moonshell".into()),
            layer,
            anchors,
            width,
            height,
            exclusive_zone: if exclusive { size_px as i32 } else { 0 },
            margins,
            keyboard,
        },
        name,
        bg: parse_color(t, "bg")?.unwrap_or(DEFAULT_BG),
        text: TextDefaults {
            color: parse_color(t, "fg")?.unwrap_or(DEFAULT_FG),
            size: t
                .get::<Option<f32>>("font_size")?
                .unwrap_or(DEFAULT_FONT_SIZE),
        },
    })
}

fn parse_anchor(s: &str) -> LuaResult<Anchors> {
    let a = |top, bottom, left, right| Anchors {
        top,
        bottom,
        left,
        right,
    };
    match s {
        "top-left" => Ok(a(true, false, true, false)),
        "top-right" => Ok(a(true, false, false, true)),
        "bottom-left" => Ok(a(false, true, true, false)),
        "bottom-right" => Ok(a(false, true, false, true)),
        "top" | "top-center" => Ok(a(true, false, false, false)),
        "bottom" | "bottom-center" => Ok(a(false, true, false, false)),
        "left" => Ok(a(false, false, true, false)),
        "right" => Ok(a(false, false, false, true)),
        other => Err(LuaError::RuntimeError(format!(
            "invalid anchor: '{other}'. Valid: top-left, top-right, bottom-left, bottom-right, \
             top, bottom, left, right (with optional -center on top/bottom)."
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vm;

    fn parse(lua_table: &str) -> WindowOpts {
        let vm = Vm::new().unwrap();
        let t: LuaTable = vm.lua().load(lua_table).eval().unwrap();
        parse_options(&t).unwrap()
    }

    #[test]
    fn defaults_are_a_top_bar() {
        let o = parse("{}");
        assert!(o.layer.anchors.top && o.layer.anchors.left && o.layer.anchors.right);
        assert!(!o.layer.anchors.bottom);
        assert_eq!((o.layer.width, o.layer.height), (0, 32));
        assert_eq!(o.layer.exclusive_zone, 32);
        assert_eq!(o.layer.layer, Layer::Top);
        assert_eq!(o.layer.keyboard, Keyboard::None);
        assert_eq!(o.bg, DEFAULT_BG);
        assert_eq!(o.text.color, DEFAULT_FG);
        assert_eq!(o.text.size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn bottom_bar_with_height() {
        let o = parse(r#"{ position = "bottom", height = 24, exclusive = false }"#);
        assert!(o.layer.anchors.bottom && !o.layer.anchors.top);
        assert_eq!((o.layer.width, o.layer.height), (0, 24));
        assert_eq!(o.layer.exclusive_zone, 0);
    }

    #[test]
    fn side_bars_use_width_as_thickness() {
        let o = parse(r#"{ position = "left", width = 48 }"#);
        assert!(o.layer.anchors.left && o.layer.anchors.top && o.layer.anchors.bottom);
        assert!(!o.layer.anchors.right);
        assert_eq!((o.layer.width, o.layer.height), (48, 0));
        assert_eq!(o.layer.exclusive_zone, 48);

        let o = parse(r#"{ position = "right", width = 48 }"#);
        assert!(o.layer.anchors.right && !o.layer.anchors.left);
    }

    #[test]
    fn popup_anchor_mode() {
        let o = parse(
            r#"{ anchor = "top-right", popup_width = 300, height = 400,
                 exclusive = false, margin_top = 8, margin_right = 12 }"#,
        );
        assert!(o.layer.anchors.top && o.layer.anchors.right);
        assert!(!o.layer.anchors.left && !o.layer.anchors.bottom);
        assert_eq!((o.layer.width, o.layer.height), (300, 400));
        assert_eq!(o.layer.margins.top, 8);
        assert_eq!(o.layer.margins.right, 12);
        assert_eq!(o.layer.exclusive_zone, 0);
    }

    #[test]
    fn popup_width_defaults_to_320() {
        let o = parse(r#"{ anchor = "bottom-left", height = 100, exclusive = false }"#);
        assert_eq!(o.layer.width, 320);
    }

    #[test]
    fn invalid_anchor_errors() {
        let vm = Vm::new().unwrap();
        let t: LuaTable = vm.lua().load(r#"{ anchor = "middle" }"#).eval().unwrap();
        let err = parse_options(&t).unwrap_err().to_string();
        assert!(err.contains("middle"), "{err}");
        assert!(err.contains("top-right"), "{err}");
    }

    #[test]
    fn layer_and_keyboard_parse() {
        let o = parse(r#"{ layer = "overlay", keyboard = "on_demand" }"#);
        assert_eq!(o.layer.layer, Layer::Overlay);
        assert_eq!(o.layer.keyboard, Keyboard::OnDemand);
    }

    #[test]
    fn theme_colors_flow_into_paint_defaults() {
        let o = parse(r##"{ bg = "#101018", fg = "#aabbcc", font_size = 14 }"##);
        assert_eq!(o.bg, Rgba::new(0x10, 0x10, 0x18, 0xff));
        assert_eq!(o.text.color, Rgba::new(0xaa, 0xbb, 0xcc, 0xff));
        assert_eq!(o.text.size, 14.0);
    }

    #[test]
    fn transparent_bg() {
        let o = parse(r#"{ bg = "transparent" }"#);
        assert_eq!(o.bg, Rgba::new(0, 0, 0, 0));
    }

    #[test]
    fn name_becomes_namespace() {
        let o = parse(r#"{ name = "launcher" }"#);
        assert_eq!(o.layer.namespace, "launcher");
        assert_eq!(o.name.as_deref(), Some("launcher"));
    }

    #[test]
    fn unnamed_windows_have_no_registry_name() {
        let o = parse("{}");
        assert_eq!(o.layer.namespace, "moonshell");
        assert!(o.name.is_none());
    }
}
