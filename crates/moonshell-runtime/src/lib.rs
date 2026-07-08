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

/// Owns the `mlua::Lua` VM. One `Vm` per config lifetime — hot reload
/// (M2 §5) drops the whole thing and builds a fresh one, so config
/// state can never leak across reloads.
pub struct Vm {
    lua: Lua,
}

impl Vm {
    /// Boot a VM: create the empty `ui` global (Rust owns the table so
    /// native components can be registered into it later), then load
    /// the stdlib that populates it.
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();
        lua.globals().set("ui", lua.create_table()?)?;
        lua.load(STDLIB).set_name("moonshell/stdlib.lua").exec()?;
        Ok(Self { lua })
    }

    /// Register the `shell.*` API, wired to `ctx` (the action queue
    /// the binary drains — see [`api`]). Call before executing the
    /// user config.
    pub fn install_shell(&self, ctx: &Rc<ShellCtx>) -> LuaResult<()> {
        api::register_shell(&self.lua, ctx)
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
}
