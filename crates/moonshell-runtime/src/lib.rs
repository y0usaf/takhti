//! The Lua runtime: VM lifecycle, the `ui.*` stdlib, and the bridge
//! from Lua element tables into `render`'s vocabulary.
//!
//! This crate is the only place Lua exists. It sees `render` (and later
//! `surface`/`services`); nothing sees it back — the binary drives a
//! [`Vm`] from its calloop loop (M2 §3).
//!
//! Error discipline (nur-inherited): everything Lua-facing stays
//! [`LuaResult`]; the binary converts to `anyhow` at its boundary.
//! `mlua::Error` is `!Send + !Sync`, so a bare `?` into anyhow does not
//! compile — convert with `map_err(|e| anyhow::anyhow!("{e}"))` there.

use std::rc::Rc;

use mlua::prelude::*;

pub mod api;
pub mod element;
pub mod exec;
pub mod painter;
pub mod services_bridge;
pub mod state;
pub mod timer;
pub mod watch;
pub mod window;

pub use api::{PendingWindow, ShellCtx};
pub use element::{from_table, TextDefaults};
pub use exec::ExecReply;
pub use mlua;
pub use painter::LuaPainter;
pub use timer::PendingTimer;
pub use watch::{PendingWatch, WatchCallback};

/// The `ui.*` stdlib: pure-Lua element constructors, embedded at build
/// time from the policy layer (`lua/` at the repo root, doctrine 01).
pub const STDLIB: &str = include_str!("../../../lua/moonshell/stdlib.lua");

/// The `shell`-dependent policy chunk: `shell.services` facades and
/// the theme-aware `shell.window` wrapper. Loaded by [`Vm::install_shell`]
/// right after the Rust API is registered.
pub const SHELL_EXT: &str = include_str!("../../../lua/moonshell/shell_ext.lua");

/// Bundled Lua modules, registered in `package.preload` at VM boot so
/// configs can `require("moonshell.theme")` etc. Each is also aliased
/// under nur's module names (`require("nur.theme")` → the *same*
/// instance — the alias loader delegates through `require`, so a
/// `theme:set(...)` from a nur config still drives moonshell's
/// defaults). This is what lets nur configs run unmodified (M2 accept).
pub const LUA_MODULES: &[(&str, &str)] = &[
    (
        "moonshell.theme",
        include_str!("../../../lua/moonshell/theme.lua"),
    ),
    (
        "moonshell.utils",
        include_str!("../../../lua/moonshell/utils.lua"),
    ),
    (
        "moonshell.services",
        include_str!("../../../lua/moonshell/services.lua"),
    ),
    (
        "moonshell.widgets.clock",
        include_str!("../../../lua/moonshell/widgets/clock.lua"),
    ),
    (
        "moonshell.widgets.battery",
        include_str!("../../../lua/moonshell/widgets/battery.lua"),
    ),
    (
        "moonshell.widgets.workspaces",
        include_str!("../../../lua/moonshell/widgets/workspaces.lua"),
    ),
    (
        "moonshell.widgets.network",
        include_str!("../../../lua/moonshell/widgets/network.lua"),
    ),
    (
        "moonshell.widgets.mpris",
        include_str!("../../../lua/moonshell/widgets/mpris.lua"),
    ),
    (
        "moonshell.widgets.volume_panel",
        include_str!("../../../lua/moonshell/widgets/volume_panel.lua"),
    ),
    (
        "moonshell.widgets.media_panel",
        include_str!("../../../lua/moonshell/widgets/media_panel.lua"),
    ),
];

/// Owns the `mlua::Lua` VM. One `Vm` per config lifetime — hot reload
/// (M2 §5) drops the whole thing and builds a fresh one, so config
/// state can never leak across reloads.
pub struct Vm {
    lua: Lua,
}

impl Vm {
    /// Boot a VM: create the empty `ui` global (Rust owns the table so
    /// native components can be registered into it later), register
    /// the bundled modules in `package.preload` (moonshell.* + the
    /// nur.* compat aliases), then load the stdlib that populates `ui`.
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();
        lua.globals().set("ui", lua.create_table()?)?;

        let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
        for &(name, source) in LUA_MODULES {
            preload.set(
                name,
                lua.create_function(move |lua, _modname: LuaValue| {
                    lua.load(source).set_name(name).eval::<LuaValue>()
                })?,
            )?;
            // nur.* alias: delegate through require so both names
            // resolve to one shared module instance.
            let alias = format!("nur.{}", &name["moonshell.".len()..]);
            let target = name;
            preload.set(
                alias,
                lua.create_function(move |lua, _modname: LuaValue| {
                    let require: LuaFunction = lua.globals().get("require")?;
                    require.call::<LuaValue>(target)
                })?,
            )?;
        }

        lua.load(STDLIB).set_name("moonshell/stdlib.lua").exec()?;
        Ok(Self { lua })
    }

    /// Register the `shell.*` API, wired to `ctx` (the action queue
    /// the binary drains — see [`api`]), then load the policy chunk
    /// that builds on it (`shell.services` facades, the theme-aware
    /// `shell.window` wrapper). Call before executing the user config.
    pub fn install_shell(&self, ctx: &Rc<ShellCtx>) -> LuaResult<()> {
        api::register_shell(&self.lua, ctx)?;
        self.lua
            .load(SHELL_EXT)
            .set_name("moonshell/shell_ext.lua")
            .exec()
    }

    /// The raw VM, for API registration and chunk evaluation.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Execute a config chunk. `name` shows up in Lua tracebacks —
    /// pass the config path.
    pub fn exec(&self, code: &str, name: &str) -> LuaResult<()> {
        self.lua.load(code).set_name(name).exec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_boots_with_ui_table() {
        let vm = Vm::new().unwrap();
        let ui: LuaTable = vm.lua().globals().get("ui").unwrap();
        for key in [
            "hbox",
            "vbox",
            "hstack",
            "vstack",
            "text",
            "label",
            "spacer",
            "icon",
            "button",
            "separator",
            "progress_bar",
            "circular_progress",
            "image",
            "slider",
            "input",
            "overlay",
            "stack",
            "scroll",
            "when",
            "map",
            "fragment",
            "bar_layout",
        ] {
            assert!(
                ui.get::<LuaFunction>(key).is_ok(),
                "ui.{key} missing from stdlib"
            );
        }
    }

    #[test]
    fn exec_runs_a_chunk() {
        let vm = Vm::new().unwrap();
        vm.exec("x = 1 + 1", "test.lua").unwrap();
        let x: i64 = vm.lua().globals().get("x").unwrap();
        assert_eq!(x, 2);
    }

    #[test]
    fn exec_error_carries_chunk_name() {
        let vm = Vm::new().unwrap();
        let err = vm.exec("nope(", "init.lua").unwrap_err().to_string();
        assert!(err.contains("init.lua"), "{err}");
    }

    #[test]
    fn luajit_is_the_vm() {
        // The locked decision is LuaJIT (FFI escape hatch), not PUC Lua.
        let vm = Vm::new().unwrap();
        let jit: LuaValue = vm.lua().globals().get("jit").unwrap();
        assert!(jit.is_table(), "expected the LuaJIT `jit` table");
    }

    #[test]
    fn fresh_vm_has_no_stale_globals() {
        let a = Vm::new().unwrap();
        a.exec("leak = 42", "a.lua").unwrap();
        let b = Vm::new().unwrap();
        let leak: LuaValue = b.lua().globals().get("leak").unwrap();
        assert!(leak.is_nil());
    }

    #[test]
    fn bundled_modules_load_under_both_names() {
        let vm = Vm::new().unwrap();
        vm.exec(
            r#"
            local m = require("moonshell.theme")
            local n = require("nur.theme")
            same = rawequal(m, n)
            base = m.base
            utils_same = rawequal(require("moonshell.utils"), require("nur.utils"))
            "#,
            "t.lua",
        )
        .unwrap();
        let g = vm.lua().globals();
        assert!(
            g.get::<bool>("same").unwrap(),
            "nur.theme must alias the same instance as moonshell.theme"
        );
        assert_eq!(g.get::<i64>("base").unwrap(), 0x1e1e2e);
        assert!(g.get::<bool>("utils_same").unwrap());
    }

    #[test]
    fn theme_set_flows_into_window_defaults() {
        // theme:set from a *nur* config must drive the shell.window
        // wrapper (which requires moonshell.theme) — the alias-identity
        // guarantee doing real work.
        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();
        vm.exec(
            r#"
            require("nur.theme"):set({ base = 0x282828 })
            shell.window({})
            "#,
            "t.lua",
        )
        .unwrap();
        let pending = ctx.take_pending();
        assert_eq!(
            pending[0].shared.borrow().bg,
            moonshell_render::Rgba::new(0x28, 0x28, 0x28, 0xff)
        );
    }

    #[test]
    fn service_facades_have_the_contract_shape() {
        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();
        vm.exec(
            r#"
            local audio = shell.services.audio
            vol = audio:get().volume
            audio:subscribe(function() seen = audio:get().muted end)
            audio:set({ volume = 0.5, muted = true })
            vol2 = audio:get().volume
            audio:toggle_mute() -- placeholder no-op must not error
            cpu = shell.services.sysinfo:get().cpu_percent
            player = shell.services.mpris:get().player_name
            "#,
            "t.lua",
        )
        .unwrap();
        let g = vm.lua().globals();
        assert_eq!(g.get::<f64>("vol").unwrap(), 1.0);
        assert_eq!(g.get::<f64>("vol2").unwrap(), 0.5);
        assert!(g.get::<bool>("seen").unwrap(), "subscriber never fired");
        assert_eq!(g.get::<i64>("cpu").unwrap(), 0);
        assert_eq!(g.get::<String>("player").unwrap(), "");
    }

    #[test]
    fn nur_simple_bar_runs_unmodified() {
        // The M2 acceptance criterion: nur's examples/simple-bar/init.lua
        // (vendored byte-for-byte in examples/simple-bar/) executes and
        // its render callback yields a parseable element tree.
        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();
        vm.exec(
            include_str!("../../../examples/simple-bar/init.lua"),
            "simple-bar/init.lua",
        )
        .unwrap();

        // One bar window, one clock interval.
        let pending = ctx.take_pending();
        assert_eq!(pending.len(), 1, "expected exactly the bar window");
        assert_eq!(ctx.take_timers().len(), 1, "expected the clock interval");

        // Fire the render callback and bridge the result into render's
        // vocabulary — the full Lua→Element path, minus pixels.
        let shared = pending[0].shared.borrow();
        let key = shared.render_key.as_ref().expect("bar:render never ran");
        let f: LuaFunction = vm.lua().registry_value(key).unwrap();
        let tree: LuaTable = f.call(()).unwrap();
        let root = element::from_table(&tree, shared.text).unwrap();
        match root {
            moonshell_render::Element::HBox(flex) => {
                assert_eq!(flex.children.len(), 3, "bar_layout: left/center/right");
            }
            other => panic!("expected bar_layout's hbox root, got {other:?}"),
        }
    }
}
