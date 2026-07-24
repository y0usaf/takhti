//! Lua table → [`Element`] — the doctrine-05 fourth arm.
//!
//! Lua render functions return nested tables describing the element tree
//! (nur's contract, `crates/runtime/src/bridge/element.rs` there):
//!
//! ```lua
//! return ui.hbox({ gap = 8, children = {
//!   ui.text("Hello"),
//!   ui.spacer(),
//!   ui.icon("battery-full"),
//! }})
//! ```
//!
//! [`from_table`] walks the tree recursively into `render`'s element
//! vocabulary. Adding an element type is always the same four arms:
//! variant in `render/element.rs`, measure arm, draw arm, and a match
//! arm here (plus the pure-Lua constructor in `resources/moonshell/stdlib.lua`).
//!
//! Contract notes vs nur:
//! - `fill = true` (GPUI `flex_1`) maps to `style.grow = 1.0`; a numeric
//!   `grow` key is also accepted (moonshell extension) and wins.
//! - nur's `hbox` is unconditionally `items_center`, so `hbox`/`button`
//!   default to `align = "center"`; `vbox` defaults to `"start"`. An
//!   explicit `align` key overrides either.
//! - `button` parses as its visual shell (a styled hbox, default gap 4);
//!   `on_click`/`on_right_click`/`hover_bg` are ignored until M4 wires
//!   input through the element tree.
//! - `scroll`/`slider`/`input` are declared in the stdlib but rejected
//!   here with an error naming the milestone that lands them (M4/M5).
//! - Props the CPU renderer does not support yet (`weight`, `italic`,
//!   `font_family`, `opacity`, `border`, `min_*`/`max_*`, `overflow`,
//!   `cursor`) are accepted and ignored so nur configs load; growing
//!   `render` to honor them is tracked work, not a parse error.
//! - Colors: `0xRRGGBB` numbers or `"#rgb"`/`"#rrggbb"` strings (nur),
//!   plus `"#rrggbbaa"` (moonshell extension — the renderer has alpha).

use mlua::prelude::*;
use moonshell_render::element::{
    Align, CircularProgress, Edges, Flex, Icon, Image, Justify, Orientation, Progress, Separator,
    Spacer, Stack, Style, Text,
};
use moonshell_render::{Element, Rgba};

/// Inherited text defaults, applied to `text` nodes (and icon tint
/// fallback) that don't set their own `size`/`color`. `shell.window`'s
/// `fg`/`font_size` options flow in here (M2 §3); the `Default` values
/// match `render::Text::default()`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextDefaults {
    pub color: Rgba,
    pub size: f32,
}

impl Default for TextDefaults {
    fn default() -> Self {
        Self {
            color: Rgba::new(0xff, 0xff, 0xff, 0xff),
            size: 14.0,
        }
    }
}

/// Parse a Lua element table (and its children, recursively) into an
/// [`Element`]. Errors carry the offending type/key so config mistakes
/// point at themselves.
pub fn from_table(table: &LuaTable, inherited: TextDefaults) -> LuaResult<Element> {
    let type_name: String = table.get("type")?;

    match type_name.as_str() {
        "hbox" | "hstack" => Ok(Element::HBox(parse_flex(
            table,
            Align::Center,
            0.0,
            inherited,
        )?)),

        "vbox" | "vstack" => Ok(Element::VBox(parse_flex(
            table,
            Align::Start,
            0.0,
            inherited,
        )?)),

        // M4 lands the click handler; until then a button is its visual
        // shell — nur's button div is a gap-4, items-center flex row.
        "button" => Ok(Element::HBox(parse_flex(
            table,
            Align::Center,
            4.0,
            inherited,
        )?)),

        "overlay" | "stack" => Ok(Element::Stack(Stack {
            style: apply_style(table, Style::default())?,
            children: parse_children(table, inherited)?,
        })),

        "text" | "label" => {
            let content: String = match table.get::<Option<String>>("content")? {
                Some(c) => c,
                None => table.get::<Option<String>>("text")?.unwrap_or_default(),
            };
            Ok(Element::Text(Text {
                style: apply_style(table, Style::default())?,
                content,
                size: table.get::<Option<f32>>("size")?.unwrap_or(inherited.size),
                line_height: table.get::<Option<f32>>("line_height")?,
                color: parse_color(table, "color")?.unwrap_or(inherited.color),
            }))
        }

        "spacer" => {
            let d = Spacer::default();
            Ok(Element::Spacer(Spacer {
                style: apply_style(table, d.style)?,
            }))
        }

        "separator" => {
            let d = Separator::default();
            Ok(Element::Separator(Separator {
                style: apply_style(table, d.style)?,
                // nur's default is horizontal (render's is vertical) —
                // the bridge speaks nur's contract.
                orientation: match table.get::<Option<String>>("orientation")?.as_deref() {
                    Some("vertical") => Orientation::Vertical,
                    _ => Orientation::Horizontal,
                },
                thickness: table
                    .get::<Option<f32>>("thickness")?
                    .unwrap_or(d.thickness),
                color: parse_color(table, "color")?.unwrap_or(d.color),
            }))
        }

        "progress_bar" => {
            let d = Progress::default();
            // nur's `bg` on a progress bar is the *track* color, not a
            // container background — pull it back out of the style.
            let mut style = apply_style(table, d.style)?;
            let track = style.bg.take().unwrap_or(d.track);
            Ok(Element::Progress(Progress {
                style,
                value: table.get::<Option<f32>>("value")?.unwrap_or(d.value),
                color: parse_color(table, "color")?.unwrap_or(d.color),
                track,
            }))
        }

        "circular_progress" => {
            let d = CircularProgress::default();
            Ok(Element::CircularProgress(CircularProgress {
                style: apply_style(table, d.style)?,
                value: table.get::<Option<f32>>("value")?.unwrap_or(d.value),
                size: table.get::<Option<f32>>("size")?.unwrap_or(d.size),
                thickness: table
                    .get::<Option<f32>>("thickness")?
                    .unwrap_or(d.thickness),
                color: parse_color(table, "color")?.unwrap_or(d.color),
                track: parse_color(table, "track")?.unwrap_or(d.track),
            }))
        }

        "icon" => {
            let d = Icon::default();
            Ok(Element::Icon(Icon {
                style: apply_style(table, d.style)?,
                name: table.get("name")?,
                path: table
                    .get::<Option<String>>("path")?
                    .map(std::path::PathBuf::from),
                size: table.get::<Option<f32>>("size")?.unwrap_or(d.size),
                color: parse_color(table, "color")?,
            }))
        }

        "image" => Ok(Element::Image(Image {
            style: apply_style(table, Style::default())?,
            src: std::path::PathBuf::from(table.get::<String>("src")?),
        })),

        "scroll" | "slider" => Err(LuaError::RuntimeError(format!(
            "'{type_name}' needs interactivity and arrives with M4; not yet supported"
        ))),
        "input" => Err(LuaError::RuntimeError(
            "'input' needs the text editor and arrives with M5; not yet supported".into(),
        )),

        other => Err(LuaError::RuntimeError(format!(
            "Unknown element type: '{other}'. Valid types: hbox, vbox, stack, text, spacer, \
             icon, image, button, separator, progress_bar, circular_progress."
        ))),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Merge the common style keys over an element-type base style, so
/// per-element defaults (e.g. a progress bar's 4 px height) survive
/// absent keys.
fn apply_style(table: &LuaTable, base: Style) -> LuaResult<Style> {
    let mut s = base;
    if let Some(bg) = parse_color(table, "bg")? {
        s.bg = Some(bg);
    }
    if let Some(r) = table.get::<Option<f32>>("border_radius")? {
        s.border_radius = r;
    }
    if let Some(w) = table.get::<Option<f32>>("width")? {
        s.width = Some(w);
    }
    if let Some(h) = table.get::<Option<f32>>("height")? {
        s.height = Some(h);
    }
    if table.get::<Option<bool>>("fill")?.unwrap_or(false) {
        s.grow = 1.0;
    }
    if let Some(g) = table.get::<Option<f32>>("grow")? {
        s.grow = g;
    }
    Ok(s)
}

/// The shared body of `hbox`/`vbox`/`button`.
fn parse_flex(
    table: &LuaTable,
    default_align: Align,
    default_gap: f32,
    inherited: TextDefaults,
) -> LuaResult<Flex> {
    Ok(Flex {
        style: apply_style(table, Style::default())?,
        gap: table.get::<Option<f32>>("gap")?.unwrap_or(default_gap),
        padding: parse_padding(table)?,
        justify: match table.get::<Option<String>>("justify")?.as_deref() {
            Some("center") => Justify::Center,
            Some("end") => Justify::End,
            _ => Justify::Start,
        },
        align: match table.get::<Option<String>>("align")?.as_deref() {
            Some("start") => Align::Start,
            Some("center") => Align::Center,
            Some("end") => Align::End,
            _ => default_align,
        },
        children: parse_children(table, inherited)?,
    })
}

/// `padding` (uniform) with `padding_top/right/bottom/left` overrides.
fn parse_padding(table: &LuaTable) -> LuaResult<Edges> {
    let p: f32 = table.get::<Option<f32>>("padding")?.unwrap_or(0.0);
    Ok(Edges {
        top: table.get::<Option<f32>>("padding_top")?.unwrap_or(p),
        right: table.get::<Option<f32>>("padding_right")?.unwrap_or(p),
        bottom: table.get::<Option<f32>>("padding_bottom")?.unwrap_or(p),
        left: table.get::<Option<f32>>("padding_left")?.unwrap_or(p),
    })
}

/// Sequential `children` table. Nil entries (from `ui.when`) and
/// non-table entries are skipped, matching nur.
/// Collect `on_click` handlers from an element table tree, keyed by
/// the child-index path ("" = root, "0.2" = root.children[0].children[2]).
/// Walks the same recursion shape as [`parse_children`] (non-table
/// entries skipped identically), so paths align with the parsed
/// [`Element`] tree — and with [`Scene::hit_path`].
///
/// [`Scene::hit_path`]: moonshell_render::Scene::hit_path
pub fn collect_handlers(
    table: &LuaTable,
    path: &mut Vec<usize>,
    out: &mut Vec<(String, LuaFunction)>,
) -> LuaResult<()> {
    if let Some(f) = table.get::<Option<LuaFunction>>("on_click")? {
        let key = path
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(".");
        out.push((key, f));
    }
    if let LuaValue::Table(children) = table.get::<LuaValue>("children")? {
        let mut index = 0;
        for i in 1..=children.raw_len() {
            if let LuaValue::Table(ct) = children.get::<LuaValue>(i)? {
                path.push(index);
                collect_handlers(&ct, path, out)?;
                path.pop();
                index += 1;
            }
        }
    }
    Ok(())
}

fn parse_children(table: &LuaTable, inherited: TextDefaults) -> LuaResult<Vec<Element>> {
    let val: LuaValue = table.get("children")?;
    match val {
        LuaValue::Table(t) => {
            let len = t.raw_len();
            let mut out = Vec::with_capacity(len);
            for i in 1..=len {
                match t.get::<LuaValue>(i)? {
                    LuaValue::Table(ct) => out.push(from_table(&ct, inherited)?),
                    _ => continue,
                }
            }
            Ok(out)
        }
        LuaValue::Nil => Ok(Vec::new()),
        _ => Err(LuaError::RuntimeError(
            "`children` must be a sequential table".into(),
        )),
    }
}

/// Parse a color prop: `0xRRGGBB` number, `"#rgb"`, `"#rrggbb"`,
/// `"#rrggbbaa"` (the `#` is optional), or `"transparent"` (nur's
/// window-config vocabulary). `None` when absent.
pub(crate) fn parse_color(table: &LuaTable, key: &str) -> LuaResult<Option<Rgba>> {
    let val: LuaValue = table.get(key)?;
    match val {
        LuaValue::Integer(n) => Ok(Some(rgb_u32(n as u32))),
        LuaValue::Number(n) => Ok(Some(rgb_u32(n as u32))),
        LuaValue::String(s) => {
            let s = s.to_str().map_err(|e| {
                LuaError::RuntimeError(format!("invalid color string for '{key}': {e}"))
            })?;
            if s.eq_ignore_ascii_case("transparent") {
                return Ok(Some(Rgba::new(0, 0, 0, 0)));
            }
            let hex = s.strip_prefix('#').unwrap_or(&s);
            let parsed = match hex.len() {
                3 => {
                    let mut expanded = String::with_capacity(6);
                    for c in hex.chars() {
                        expanded.push(c);
                        expanded.push(c);
                    }
                    u32::from_str_radix(&expanded, 16).map(rgb_u32)
                }
                6 => u32::from_str_radix(hex, 16).map(rgb_u32),
                8 => u32::from_str_radix(hex, 16)
                    .map(|c| Rgba::new((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8)),
                _ => {
                    return Err(LuaError::RuntimeError(format!(
                        "invalid color format for '{key}': expected #RGB, #RRGGBB or #RRGGBBAA, \
                         got \"{s}\""
                    )));
                }
            };
            parsed
                .map(Some)
                .map_err(|e| LuaError::RuntimeError(format!("invalid hex color for '{key}': {e}")))
        }
        LuaValue::Nil => Ok(None),
        _ => Err(LuaError::RuntimeError(format!(
            "'{key}' must be a number (0xRRGGBB) or string (\"#rrggbb\")"
        ))),
    }
}

fn rgb_u32(c: u32) -> Rgba {
    Rgba::new((c >> 16) as u8, (c >> 8) as u8, c as u8, 0xff)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vm;

    /// Evaluate a Lua expression (with the stdlib loaded) and parse the
    /// resulting table with default inheritance.
    fn eval(code: &str) -> LuaResult<Element> {
        let vm = Vm::new().unwrap();
        let table: LuaTable = vm.lua().load(code).eval()?;
        from_table(&table, TextDefaults::default())
    }

    fn eval_ok(code: &str) -> Element {
        eval(code).unwrap()
    }

    // --- constructors round-trip through the stdlib ---

    #[test]
    fn text_from_string_constructor() {
        let el = eval_ok(r#"ui.text("hello")"#);
        let Element::Text(t) = el else {
            panic!("expected Text")
        };
        assert_eq!(t.content, "hello");
        assert_eq!(t.size, 14.0); // inherited default
        assert_eq!(t.color, Rgba::new(0xff, 0xff, 0xff, 0xff));
    }

    #[test]
    fn text_props_override_inherited() {
        let el = eval_ok(r##"ui.text({ content = "x", size = 13, color = "#cdd6f4" })"##);
        let Element::Text(t) = el else {
            panic!("expected Text")
        };
        assert_eq!(t.size, 13.0);
        assert_eq!(t.color, Rgba::new(0xcd, 0xd6, 0xf4, 0xff));
    }

    #[test]
    fn text_inherits_window_defaults() {
        let vm = Vm::new().unwrap();
        let table: LuaTable = vm.lua().load(r#"ui.text("x")"#).eval().unwrap();
        let inherited = TextDefaults {
            color: Rgba::new(1, 2, 3, 0xff),
            size: 13.0,
        };
        let Element::Text(t) = from_table(&table, inherited).unwrap() else {
            panic!("expected Text")
        };
        assert_eq!(t.size, 13.0);
        assert_eq!(t.color, Rgba::new(1, 2, 3, 0xff));
    }

    #[test]
    fn label_alias_and_text_key() {
        let Element::Text(t) = eval_ok(r#"ui.label({ text = "via text key" })"#) else {
            panic!("expected Text")
        };
        assert_eq!(t.content, "via text key");
    }

    #[test]
    fn hbox_defaults_align_center() {
        let Element::HBox(f) = eval_ok("ui.hbox({})") else {
            panic!("expected HBox")
        };
        assert_eq!(f.align, Align::Center);
        assert_eq!(f.justify, Justify::Start);
        assert_eq!(f.gap, 0.0);
    }

    #[test]
    fn vbox_defaults_align_start() {
        let Element::VBox(f) = eval_ok("ui.vbox({})") else {
            panic!("expected VBox")
        };
        assert_eq!(f.align, Align::Start);
    }

    #[test]
    fn align_key_overrides_default() {
        let Element::HBox(f) = eval_ok(r#"ui.hbox({ align = "end" })"#) else {
            panic!("expected HBox")
        };
        assert_eq!(f.align, Align::End);
    }

    #[test]
    fn fill_maps_to_grow() {
        let Element::HBox(f) = eval_ok("ui.hbox({ fill = true })") else {
            panic!("expected HBox")
        };
        assert_eq!(f.style.grow, 1.0);
    }

    #[test]
    fn grow_key_wins_over_fill() {
        let Element::HBox(f) = eval_ok("ui.hbox({ fill = true, grow = 2 })") else {
            panic!("expected HBox")
        };
        assert_eq!(f.style.grow, 2.0);
    }

    #[test]
    fn padding_uniform_and_overrides() {
        let Element::HBox(f) = eval_ok("ui.hbox({ padding = 4, padding_top = 10 })") else {
            panic!("expected HBox")
        };
        assert_eq!(
            f.padding,
            Edges {
                top: 10.0,
                right: 4.0,
                bottom: 4.0,
                left: 4.0
            }
        );
    }

    #[test]
    fn justify_center_and_end() {
        let Element::HBox(f) = eval_ok(r#"ui.hbox({ justify = "center" })"#) else {
            panic!("expected HBox")
        };
        assert_eq!(f.justify, Justify::Center);
        let Element::HBox(f) = eval_ok(r#"ui.hbox({ justify = "end" })"#) else {
            panic!("expected HBox")
        };
        assert_eq!(f.justify, Justify::End);
    }

    #[test]
    fn spacer_keeps_grow_one() {
        let Element::Spacer(s) = eval_ok("ui.spacer()") else {
            panic!("expected Spacer")
        };
        assert_eq!(s.style.grow, 1.0);
    }

    #[test]
    fn button_is_visual_hbox_shell() {
        let el = eval_ok(
            r##"ui.button({ bg = "#313244", padding_left = 10, on_click = function() end })"##,
        );
        let Element::HBox(f) = el else {
            panic!("expected HBox")
        };
        assert_eq!(f.gap, 4.0); // nur's button default gap
        assert_eq!(f.align, Align::Center);
        assert_eq!(f.style.bg, Some(Rgba::new(0x31, 0x32, 0x44, 0xff)));
        assert_eq!(f.padding.left, 10.0);
    }

    #[test]
    fn separator_defaults_horizontal() {
        let Element::Separator(s) = eval_ok("ui.separator({})") else {
            panic!("expected Separator")
        };
        assert_eq!(s.orientation, Orientation::Horizontal);
        assert_eq!(s.thickness, 1.0);
        assert_eq!(s.color, Rgba::new(0x45, 0x47, 0x5a, 0xff));
    }

    #[test]
    fn separator_vertical() {
        let Element::Separator(s) = eval_ok(r#"ui.separator({ orientation = "vertical" })"#) else {
            panic!("expected Separator")
        };
        assert_eq!(s.orientation, Orientation::Vertical);
    }

    #[test]
    fn progress_bar_bg_is_track() {
        let Element::Progress(p) = eval_ok(r##"ui.progress_bar({ value = 0.5, bg = "#101010" })"##)
        else {
            panic!("expected Progress")
        };
        assert_eq!(p.value, 0.5);
        assert_eq!(p.track, Rgba::new(0x10, 0x10, 0x10, 0xff));
        assert_eq!(p.style.bg, None); // bg consumed by the track
        assert_eq!(p.style.height, Some(4.0)); // element default kept
        assert_eq!(p.style.border_radius, 2.0);
    }

    #[test]
    fn circular_progress_defaults() {
        let Element::CircularProgress(c) = eval_ok("ui.circular_progress({ value = 0.42 })") else {
            panic!("expected CircularProgress")
        };
        assert_eq!(c.value, 0.42);
        assert_eq!(c.size, 16.0);
        assert_eq!(c.thickness, 2.0);
    }

    #[test]
    fn icon_by_name_and_props() {
        let Element::Icon(i) = eval_ok(r#"ui.icon("battery-full")"#) else {
            panic!("expected Icon")
        };
        assert_eq!(i.name, "battery-full");
        assert_eq!(i.size, 16.0);
        assert_eq!(i.path, None);

        let Element::Icon(i) =
            eval_ok(r#"ui.icon({ name = "x", path = "/tmp/x.svg", size = 20, color = 0xff0000 })"#)
        else {
            panic!("expected Icon")
        };
        assert_eq!(i.path, Some(std::path::PathBuf::from("/tmp/x.svg")));
        assert_eq!(i.size, 20.0);
        assert_eq!(i.color, Some(Rgba::new(0xff, 0, 0, 0xff)));
    }

    #[test]
    fn image_src_and_size() {
        let Element::Image(i) = eval_ok(r#"ui.image({ src = "/tmp/a.png", width = 24 })"#) else {
            panic!("expected Image")
        };
        assert_eq!(i.src, std::path::PathBuf::from("/tmp/a.png"));
        assert_eq!(i.style.width, Some(24.0));
    }

    #[test]
    fn image_from_string() {
        let Element::Image(i) = eval_ok(r#"ui.image("/tmp/a.png")"#) else {
            panic!("expected Image")
        };
        assert_eq!(i.src, std::path::PathBuf::from("/tmp/a.png"));
    }

    #[test]
    fn overlay_stacks_children() {
        let Element::Stack(s) = eval_ok(
            r#"ui.overlay({ width = 100, height = 50, children = { ui.text("a"), ui.text("b") } })"#,
        ) else {
            panic!("expected Stack")
        };
        assert_eq!(s.children.len(), 2);
        assert_eq!(s.style.width, Some(100.0));
        assert_eq!(s.style.height, Some(50.0));
    }

    // --- children handling ---

    #[test]
    fn when_false_children_skipped() {
        let Element::HBox(f) = eval_ok(
            r#"ui.hbox({ children = {
                ui.text("always"),
                ui.when(false, ui.text("never")),
                ui.when(true, ui.text("sometimes")),
            } })"#,
        ) else {
            panic!("expected HBox")
        };
        assert_eq!(f.children.len(), 2);
    }

    #[test]
    fn map_builds_children() {
        let Element::HBox(f) = eval_ok(
            r#"ui.hbox({ children = ui.map({ "1", "2", "3" }, function(n) return ui.text(n) end) })"#,
        ) else {
            panic!("expected HBox")
        };
        assert_eq!(f.children.len(), 3);
        assert_eq!(
            f.children[0],
            Element::Text(Text {
                content: "1".into(),
                ..Text::default()
            })
        );
    }

    #[test]
    fn nil_children_field_is_empty() {
        let Element::HBox(f) = eval_ok("ui.hbox({})") else {
            panic!("expected HBox")
        };
        assert!(f.children.is_empty());
    }

    #[test]
    fn non_table_children_errors() {
        assert!(eval(r#"{ type = "hbox", children = "nope" }"#).is_err());
    }

    // --- colors ---

    #[test]
    fn color_number_string_short_and_alpha() {
        let cases = [
            (
                r#"ui.text({ content = "x", color = 0xff0000 })"#,
                Rgba::new(0xff, 0, 0, 0xff),
            ),
            (
                r##"ui.text({ content = "x", color = "#ff0000" })"##,
                Rgba::new(0xff, 0, 0, 0xff),
            ),
            (
                r##"ui.text({ content = "x", color = "#f00" })"##,
                Rgba::new(0xff, 0, 0, 0xff),
            ),
            (
                r##"ui.text({ content = "x", color = "#ff000080" })"##,
                Rgba::new(0xff, 0, 0, 0x80),
            ),
        ];
        for (code, want) in cases {
            let Element::Text(t) = eval_ok(code) else {
                panic!("expected Text")
            };
            assert_eq!(t.color, want, "{code}");
        }
    }

    #[test]
    fn invalid_color_errors() {
        assert!(eval(r##"ui.text({ content = "x", color = "#xyz" })"##).is_err());
        assert!(eval(r##"ui.text({ content = "x", color = "#ff00" })"##).is_err());
        assert!(eval(r#"ui.text({ content = "x", color = true })"#).is_err());
    }

    // --- errors ---

    #[test]
    fn unknown_type_errors_with_valid_list() {
        let err = eval(r#"{ type = "nonexistent" }"#).unwrap_err().to_string();
        assert!(err.contains("nonexistent"));
        assert!(err.contains("hbox"));
        assert!(err.contains("progress_bar"));
    }

    #[test]
    fn deferred_types_name_their_milestone() {
        let err = eval("ui.slider({})").unwrap_err().to_string();
        assert!(err.contains("M4"));
        let err = eval("ui.scroll({})").unwrap_err().to_string();
        assert!(err.contains("M4"));
        let err = eval("ui.input({})").unwrap_err().to_string();
        assert!(err.contains("M5"));
    }

    #[test]
    fn missing_type_errors() {
        assert!(eval("{}").is_err());
    }

    // --- the acceptance shape: simple-bar's tree through the stdlib ---

    #[test]
    fn simple_bar_shape_parses() {
        let el = eval_ok(
            r##"
            ui.hbox({ bg = "#1e1e2e", padding_left = 12, padding_right = 12, children = {
                ui.hbox({ gap = 8, fill = true, children = {
                    ui.hbox({ gap = 4, children = {
                        ui.text("1"), ui.text("2"), ui.text("3"),
                    } }),
                    ui.text("  moonshell"),
                } }),
                ui.hbox({ gap = 8, fill = true, justify = "center", children = {
                    ui.button({ bg = "#313244", padding_left = 10, padding_right = 10,
                                children = { ui.text("media") } }),
                    ui.text("12:34:56"),
                } }),
                ui.hbox({ gap = 8, fill = true, justify = "end", children = {
                    ui.text("cpu 4%"),
                    ui.hbox({ gap = 4, children = {
                        ui.icon({ name = "battery-full", color = "#cdd6f4" }),
                        ui.text("85%"),
                    } }),
                } }),
            } })
            "##,
        );
        let Element::HBox(root) = el else {
            panic!("expected HBox root")
        };
        assert_eq!(root.children.len(), 3);
        assert_eq!(root.style.bg, Some(Rgba::new(0x1e, 0x1e, 0x2e, 0xff)));
        for region in &root.children {
            assert_eq!(region.style().grow, 1.0);
        }
        // The button reduced to a styled hbox shell.
        let Element::HBox(center) = &root.children[1] else {
            panic!("expected center HBox")
        };
        let Element::HBox(button) = &center.children[0] else {
            panic!("expected button shell")
        };
        assert_eq!(button.style.bg, Some(Rgba::new(0x31, 0x32, 0x44, 0xff)));
    }
}
