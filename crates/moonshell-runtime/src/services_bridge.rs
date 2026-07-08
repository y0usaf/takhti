//! Push native service snapshots into the Lua facades.
//!
//! The one place `services` state crosses into Lua: `services` itself
//! stays Lua-free (one-way arrow), and the Lua-facing shape —
//! `shell.services.<name>` with `:get/:set/:map/:subscribe` — is
//! declared once in `lua/moonshell/services.lua` (doctrine 05).
//! Backends replace the *backing* of those facades by calling `:set`
//! with a fresh snapshot table; every widget subscription and dirty
//! mark then rides the ordinary `shell.state` path.
//!
//! Absent facades are an error to the caller: the binary logs at
//! debug (a reload mid-flight or a config that nuked `shell` is not
//! worth a crash — the next snapshot retries).

use mlua::prelude::*;
use moonshell_services::compositor::CompositorState;

/// `shell.services.compositor:set(snapshot)`.
pub fn push_compositor(lua: &Lua, state: &CompositorState) -> LuaResult<()> {
    let t = lua.create_table()?;
    t.set("connected", state.connected)?;
    t.set("active_workspace", state.active_workspace)?;
    let ws = lua.create_table()?;
    for (i, w) in state.workspaces.iter().enumerate() {
        let wt = lua.create_table()?;
        wt.set("id", w.id)?;
        wt.set("name", w.name.as_str())?;
        wt.set("active", w.active)?;
        wt.set("windows", w.windows)?;
        ws.set(i + 1, wt)?;
    }
    t.set("workspaces", ws)?;
    if let Some(title) = &state.active_window {
        t.set("active_window", title.as_str())?;
    }
    set_service(lua, "compositor", t)
}

fn set_service(lua: &Lua, name: &str, snapshot: LuaTable) -> LuaResult<()> {
    let shell: LuaTable = lua.globals().get("shell")?;
    let services: LuaTable = shell.get("services")?;
    let facade: LuaTable = services.get(name)?;
    let set: LuaFunction = facade.get("set")?;
    set.call((facade, snapshot))
}
