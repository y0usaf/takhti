//! Compile `docs/lua-api.md` from the LuaLS meta stubs
//! (`resources/meta/tomoe.lua`) and the built-in Lua modules, and hold both
//! to the registered API with parity tests: `cargo test` fails when the docs
//! drift from what the runtime actually exposes, or when the generated page
//! is stale. Regenerate with `TOMOE_REGEN_DOCS=1 cargo test -p tomoe docgen`.

use std::collections::BTreeSet;
use std::fmt::Write as _;

const META: &str = include_str!("../../../resources/meta/tomoe.lua");
const WM: &str = include_str!("../../../resources/wm.lua");
const ZOOMER: &str = include_str!("../../../resources/zoomer.lua");

struct Field {
    name: String,
    ty: String,
    doc: String,
}

enum Item {
    /// `-- ─── Title ───` marker (meta file only).
    Section(String),
    /// Documented table declaration (`tomoe.process = {}`): section prose.
    Text(String),
    /// `---@class` block; fields from its `---@field` lines.
    Class {
        name: String,
        parent: Option<String>,
        doc: String,
        fields: Vec<Field>,
    },
    /// `function a.b(args)` / `function A:b(args)` / documented `M.x = y`.
    Func {
        name: String,
        args: String,
        rets: Vec<String>,
        doc: String,
    },
}

/// `---@field name type [# doc]`
fn parse_field(s: &str) -> Field {
    let (decl, doc) = match s.split_once(" # ") {
        Some((decl, doc)) => (decl.trim(), doc.trim()),
        None => (s.trim(), ""),
    };
    let (name, ty) = decl.split_once(' ').unwrap_or((decl, ""));
    Field {
        name: name.to_string(),
        ty: ty.trim().to_string(),
        doc: doc.to_string(),
    }
}

/// `-- ─── Title ───…`
fn section_title(line: &str) -> Option<String> {
    let rest = line.strip_prefix("-- ")?;
    if !rest.starts_with('─') {
        return None;
    }
    let title = rest.trim_matches(|c| c == '─' || c == ' ');
    (!title.is_empty()).then(|| title.to_string())
}

/// `function tomoe.x(args) end`, `function Window:x(args) end`, or (in a
/// module) `function M.x(args)` — mapped to `<module>.x`.
fn function_decl(line: &str, module: Option<&str>) -> Option<(String, String)> {
    let rest = line.strip_prefix("function ")?;
    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    let name = rest[..open].trim();
    let args = rest[open + 1..close].to_string();
    let name = match module {
        Some(m) => format!("{m}.{}", name.strip_prefix("M.")?),
        None if name.starts_with("tomoe.") || name.contains(':') => name.to_string(),
        None => return None,
    };
    Some((name, args))
}

/// Module export by assignment: `M.name = local_fn`.
fn export_decl(line: &str, module: Option<&str>) -> Option<String> {
    let m = module?;
    let (name, value) = line.strip_prefix("M.")?.split_once(" = ")?;
    let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
    (ident(name) && ident(value)).then(|| format!("{m}.{name}"))
}

/// One pass over a Lua source: `---` doc blocks attach to the declaration
/// that ends them; `---@class` blocks need no declaration line.
fn parse(src: &str, module: Option<&str>) -> Vec<Item> {
    let mut items = Vec::new();
    let mut doc: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();
    let mut rets: Vec<String> = Vec::new();
    let mut class: Option<(String, Option<String>, Vec<Field>)> = None;

    fn flush_class(
        class: &mut Option<(String, Option<String>, Vec<Field>)>,
        doc: &[String],
        items: &mut Vec<Item>,
    ) -> bool {
        if let Some((name, parent, fields)) = class.take() {
            items.push(Item::Class {
                name,
                parent,
                doc: doc.join(" "),
                fields,
            });
            true
        } else {
            false
        }
    }

    for raw in src.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("---") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if let Some(decl) = rest.strip_prefix("@class ") {
                let (name, parent) = match decl.split_once(':') {
                    Some((name, parent)) => (name, Some(parent.trim().to_string())),
                    None => (decl, None),
                };
                class = Some((name.trim().to_string(), parent, Vec::new()));
            } else if let Some(field) = rest.strip_prefix("@field ") {
                if let Some((_, _, fields)) = &mut class {
                    fields.push(parse_field(field));
                }
            } else if let Some(param) = rest.strip_prefix("@param ") {
                let name = param.split_whitespace().next().unwrap_or("");
                params.push(name.trim_end_matches('?').to_string());
            } else if let Some(ret) = rest.strip_prefix("@return ") {
                rets.push(ret.trim().to_string());
            } else if !rest.starts_with('@') {
                doc.push(rest.to_string());
            }
            continue;
        }

        if flush_class(&mut class, &doc, &mut items) {
        } else if let Some(title) = section_title(line) {
            if module.is_none() {
                items.push(Item::Section(title));
            }
        } else if let Some((name, args)) = function_decl(line, module) {
            items.push(Item::Func {
                name,
                args,
                rets: std::mem::take(&mut rets),
                doc: doc.join(" "),
            });
        } else if let Some(name) = export_decl(line, module) {
            if !doc.is_empty() {
                items.push(Item::Func {
                    name,
                    args: params.join(", "),
                    rets: std::mem::take(&mut rets),
                    doc: doc.join(" "),
                });
            }
        } else if !doc.is_empty() && line.ends_with("= {}") {
            items.push(Item::Text(doc.join(" ")));
        }
        doc.clear();
        params.clear();
        rets.clear();
    }
    // A class block at end-of-file has no declaration line after it.
    flush_class(&mut class, &doc, &mut items);
    items
}

/// Ensure a blank line before a heading or paragraph.
fn para(out: &mut String) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

fn render_fields(out: &mut String, fields: &[Field]) {
    for f in fields {
        let ty = if f.ty.is_empty() {
            String::new()
        } else {
            format!(": {}", f.ty)
        };
        let dash = if f.doc.is_empty() {
            String::new()
        } else {
            format!(" — {}", f.doc)
        };
        writeln!(out, "- `{}{}`{}", f.name, ty, dash).unwrap();
    }
}

fn render_class(
    out: &mut String,
    name: &str,
    parent: &Option<String>,
    doc: &str,
    fields: &[Field],
) {
    para(out);
    match parent {
        Some(p) => writeln!(out, "### {name} : {p}").unwrap(),
        None => writeln!(out, "### {name}").unwrap(),
    }
    if !doc.is_empty() {
        para(out);
        writeln!(out, "{doc}").unwrap();
    }
    para(out);
    render_fields(out, fields);
}

fn render_func(out: &mut String, name: &str, args: &str, rets: &[String], doc: &str) {
    let ret = if rets.is_empty() {
        String::new()
    } else {
        format!(" -> {}", rets.join(", "))
    };
    let dash = if doc.is_empty() {
        String::new()
    } else {
        format!(" — {doc}")
    };
    writeln!(out, "- `{name}({args}){ret}`{dash}").unwrap();
}

/// Meta rendering: items in file order, `-- ───` markers as `##` sections.
fn render_meta(out: &mut String, items: &[Item]) {
    for item in items {
        match item {
            Item::Section(title) => {
                para(out);
                writeln!(out, "## {title}").unwrap();
            }
            Item::Text(text) => {
                para(out);
                writeln!(out, "{text}").unwrap();
            }
            Item::Class {
                name,
                parent,
                doc,
                fields,
            } => render_class(out, name, parent, doc, fields),
            Item::Func {
                name,
                args,
                rets,
                doc,
            } => render_func(out, name, args, rets, doc),
        }
    }
}

/// Module rendering: the class named after the module is its intro, then the
/// functions, then any remaining types (e.g. an options table).
fn render_module(out: &mut String, items: &[Item], module: &str) {
    para(out);
    writeln!(out, "## {module}").unwrap();
    for item in items {
        if let Item::Class {
            name, doc, fields, ..
        } = item
        {
            if name == module {
                para(out);
                writeln!(out, "{doc}").unwrap();
                para(out);
                render_fields(out, fields);
            }
        }
    }
    para(out);
    for item in items {
        if let Item::Func {
            name,
            args,
            rets,
            doc,
        } = item
        {
            render_func(out, name, args, rets, doc);
        }
    }
    for item in items {
        if let Item::Class {
            name,
            parent,
            doc,
            fields,
        } = item
        {
            if name != module {
                render_class(out, name, parent, doc, fields);
            }
        }
    }
}

fn generate() -> String {
    let mut out = String::new();
    out.push_str("# tomoe Lua API\n\n");
    out.push_str(
        "<!-- Generated from resources/meta/tomoe.lua, resources/wm.lua, and\n\
         resources/zoomer.lua by src/docgen.rs. Do not edit; regenerate with\n\
         `TOMOE_REGEN_DOCS=1 cargo test -p tomoe docgen`. -->\n\n",
    );
    out.push_str(
        "The config is a Lua program (`~/.config/tomoe/init.lua`, hot-reloaded on\n\
         save); the `tomoe` global is the entire core API, and all WM policy is Lua\n\
         on top of it. Geometry is integer physical pixels in world coordinates.\n\
         Reads see a snapshot taken before each Lua entry; writes are queued and\n\
         applied when it returns. Point LuaLS at `resources/meta/` for completion\n\
         and type checking in your editor.\n",
    );
    render_meta(&mut out, &parse(META, None));
    render_module(&mut out, &parse(WM, Some("wm")), "wm");
    render_module(&mut out, &parse(ZOOMER, Some("zoomer")), "zoomer");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/lua-api.md");

    fn documented(items: &[Item], prefix: &str) -> BTreeSet<String> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Func { name, .. } if name.starts_with(prefix) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every function the runtime registers is documented, and nothing is
    /// documented that doesn't exist.
    #[test]
    fn meta_matches_registered_api() {
        let rt = crate::lua::LuaRuntime::new().unwrap();
        let registered: BTreeSet<String> = rt
            .lua()
            .load(
                r#"
                local names = {}
                for k, v in pairs(tomoe) do
                  if type(v) == "function" then
                    names[#names + 1] = "tomoe." .. k
                  elseif type(v) == "table" then
                    for k2, v2 in pairs(v) do
                      if type(v2) == "function" then
                        names[#names + 1] = "tomoe." .. k .. "." .. k2
                      end
                    end
                  end
                end
                return names
                "#,
            )
            .eval::<Vec<String>>()
            .unwrap()
            .into_iter()
            .collect();
        let items = parse(META, None);
        assert_eq!(
            documented(&items, "tomoe."),
            registered,
            "resources/meta/tomoe.lua is out of sync with the registered `tomoe` table"
        );

        let methods: mlua::Table = rt
            .test_window()
            .unwrap()
            .metatable()
            .unwrap()
            .get("__index")
            .unwrap();
        let mut registered = BTreeSet::new();
        for pair in methods.pairs::<String, mlua::Value>() {
            registered.insert(format!("Window:{}", pair.unwrap().0));
        }
        assert_eq!(
            documented(&items, "Window:"),
            registered,
            "resources/meta/tomoe.lua is out of sync with the Window methods"
        );
    }

    /// The built-in modules' exports match their doc annotations: functions
    /// against documented functions, data fields against `---@field` lines.
    #[test]
    fn modules_match_their_annotations() {
        let rt = crate::lua::LuaRuntime::new().unwrap();
        for (module, src) in [("wm", WM), ("zoomer", ZOOMER)] {
            let table: mlua::Table = rt
                .lua()
                .load(format!("return require(\"{module}\")"))
                .eval()
                .unwrap();
            let mut functions = BTreeSet::new();
            let mut data = BTreeSet::new();
            for pair in table.pairs::<String, mlua::Value>() {
                let (key, value) = pair.unwrap();
                match value {
                    mlua::Value::Function(_) => {
                        functions.insert(format!("{module}.{key}"));
                    }
                    _ => {
                        data.insert(key);
                    }
                }
            }
            let items = parse(src, Some(module));
            assert_eq!(
                documented(&items, &format!("{module}.")),
                functions,
                "resources/{module}.lua exports out of sync with its doc comments"
            );
            let fields: BTreeSet<String> = items
                .iter()
                .filter_map(|item| match item {
                    Item::Class { name, fields, .. } if name == module => Some(fields),
                    _ => None,
                })
                .flatten()
                .map(|f| f.name.clone())
                .collect();
            assert_eq!(
                fields, data,
                "resources/{module}.lua data fields out of sync with its @field lines"
            );
        }
    }

    /// docs/lua-api.md matches what the sources generate.
    #[test]
    fn docgen() {
        let generated = generate();
        if std::env::var_os("TOMOE_REGEN_DOCS").is_some() {
            std::fs::write(DOCS_PATH, &generated).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(DOCS_PATH).unwrap_or_default();
        assert!(
            on_disk == generated,
            "docs/lua-api.md is stale — regenerate with \
             `TOMOE_REGEN_DOCS=1 cargo test -p tomoe docgen`"
        );
    }
}
