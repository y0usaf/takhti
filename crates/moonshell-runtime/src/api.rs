//! The `shell.*` Lua API and the action queue between Lua and the
//! binary's event loop.
//!
//! Lua never touches `surface::Shell` directly (it is the calloop
//! dispatch data — no stable `&mut` exists while Lua runs). Instead,
//! `shell.window(...)` queues a [`PendingWindow`] and `state:set(...)`
//! raises the dirty flag; the binary drains both after every Lua entry
//! point (config exec now; timers and hot reload in M2 §4–5) while it
//! *does* hold `&mut Shell`. Snapshot in, actions out — the doctrine-02
//! shape, even though Lua is the application here.
//!
//! Registered by [`crate::Vm::install_shell`]:
//! - `shell.window(opts) -> handle` — queue a layer-surface window;
//!   `handle:render(fn)` attaches the render callback. A `name` in the
//!   opts also registers the handle for `shell.get_window`.
//! - `shell.state(initial) -> state` — reactive value; `:get()`,
//!   `:set(v)`, `:map(fn)`, `:subscribe(fn)`.
//! - `shell.interval(ms, fn)` / `shell.once(ms, fn)` — queue a
//!   [`PendingTimer`]; the binary arms it as a calloop timer.
//! - `shell.exec(cmd) -> stdout` / `shell.exec_async(cmd, fn)` — see
//!   [`crate::exec`].
//! - `shell.quit()` — raise the quit flag; the drain stops the loop.
//! - `shell.get_window(name) -> handle | nil` — named-window registry.
//! - `shell.displays() -> {{name,x,y,width,height,scale,is_primary},…}`
//!   — reads the output snapshot the binary refreshes each drain.
//! - `shell.reload()` — raise the reload flag; the drain destroys the
//!   Lua windows, drops the VM, and re-execs the config.
//! - `shell.watch_file(path, fn)` — queue a [`PendingWatch`]; the
//!   binary registers it with its inotify watcher.
//!
//! `shell.clipboard_*` needs a data-control protocol and is deferred
//! to M4 (tracked in PLAN.md).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use mlua::prelude::*;
use moonshell_surface::{DisplayInfo, LayerOptions};

use crate::exec::{self, ExecCallback, ExecReply};
use crate::state::LuaState;
use crate::timer::PendingTimer;
use crate::watch::{PendingWatch, WatchCallback};
use crate::window::{self, WindowHandle, WindowShared};

/// A `shell.window` call waiting for the binary to create the actual
/// layer surface (with a `LuaPainter` wired to `shared`).
pub struct PendingWindow {
    pub options: LayerOptions,
    /// Render callback + window paint defaults, shared with the
    /// [`WindowHandle`] Lua already holds.
    pub shared: Rc<RefCell<WindowShared>>,
}

/// The Lua↔loop bridge state: queued actions (windows, timers), the
/// notify-all dirty flag, the quit flag, the named-window registry,
/// the display snapshot, and the exec-reply channel. One per
/// [`crate::Vm`]; both sides hold it by `Rc`.
pub struct ShellCtx {
    pending: RefCell<Vec<PendingWindow>>,
    timers: RefCell<Vec<PendingTimer>>,
    watches: RefCell<Vec<PendingWatch>>,
    named: RefCell<HashMap<String, Rc<RefCell<WindowShared>>>>,
    displays: RefCell<Vec<DisplayInfo>>,
    dirty: Cell<bool>,
    quit: Cell<bool>,
    reload: Cell<bool>,
    exec_tx: calloop::channel::Sender<ExecReply>,
    exec_rx: RefCell<Option<calloop::channel::Channel<ExecReply>>>,
    /// In-flight `exec_async` callbacks by reply id — the `!Send` Lua
    /// half that must not cross into the worker thread.
    exec_callbacks: RefCell<HashMap<u64, ExecCallback>>,
    next_exec_id: Cell<u64>,
}

impl ShellCtx {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Rc<Self> {
        let (exec_tx, exec_rx) = calloop::channel::channel();
        Rc::new(Self {
            pending: RefCell::default(),
            timers: RefCell::default(),
            watches: RefCell::default(),
            named: RefCell::default(),
            displays: RefCell::default(),
            dirty: Cell::new(false),
            quit: Cell::new(false),
            reload: Cell::new(false),
            exec_tx,
            exec_rx: RefCell::new(Some(exec_rx)),
            exec_callbacks: RefCell::default(),
            next_exec_id: Cell::new(0),
        })
    }

    /// Something visible changed (state set, render fn attached) —
    /// every window repaints on the next drain. Unchanged element
    /// trees early-out in the scene diff, so notify-all stays cheap.
    pub fn mark_dirty(&self) {
        self.dirty.set(true);
    }

    /// Take-and-clear the dirty flag.
    pub fn take_dirty(&self) -> bool {
        self.dirty.replace(false)
    }

    /// Drain queued window creations.
    pub fn take_pending(&self) -> Vec<PendingWindow> {
        self.pending.take()
    }

    /// Drain queued timers (the binary arms them as calloop sources).
    pub fn take_timers(&self) -> Vec<PendingTimer> {
        self.timers.take()
    }

    /// Drain queued file watches (the binary registers them with its
    /// inotify watcher).
    pub fn take_watches(&self) -> Vec<PendingWatch> {
        self.watches.take()
    }

    /// Take-and-clear the quit flag.
    pub fn take_quit(&self) -> bool {
        self.quit.replace(false)
    }

    /// Raise the reload flag from the Rust side — the config-tree
    /// watcher's path into the same drain `shell.reload()` takes.
    pub fn request_reload(&self) {
        self.reload.set(true);
    }

    /// Take-and-clear the reload flag.
    pub fn take_reload(&self) -> bool {
        self.reload.replace(false)
    }

    /// Wipe everything belonging to the outgoing VM before a fresh one
    /// boots: queued actions, the named-window registry, in-flight
    /// exec callbacks, the dirty flag. The exec channel, the display
    /// snapshot, and the quit flag survive — they belong to the loop,
    /// not the VM. Armed calloop timers self-clean (their `WeakLua`
    /// dies with the VM).
    pub fn reset_for_reload(&self) {
        self.pending.borrow_mut().clear();
        self.timers.borrow_mut().clear();
        self.watches.borrow_mut().clear();
        self.named.borrow_mut().clear();
        self.exec_callbacks.borrow_mut().clear();
        self.dirty.set(false);
    }

    /// Refresh the snapshot `shell.displays()` reads. The binary calls
    /// this before config exec and once per drain — snapshot in,
    /// actions out.
    pub fn set_displays(&self, displays: Vec<DisplayInfo>) {
        *self.displays.borrow_mut() = displays;
    }

    /// The receiving end of the `exec_async` reply channel; the binary
    /// inserts it into the event loop once. `None` on the second take.
    pub fn take_exec_channel(&self) -> Option<calloop::channel::Channel<ExecReply>> {
        self.exec_rx.borrow_mut().take()
    }

    /// Route a finished `exec_async` back into Lua. Must run on the
    /// loop thread; unknown ids (VM replaced, map cleared) are ignored.
    pub fn dispatch_exec_reply(&self, reply: ExecReply) {
        let cb = self.exec_callbacks.borrow_mut().remove(&reply.id);
        if let Some(cb) = cb {
            // Borrow released first: the callback may call exec_async.
            cb.call(reply.output);
        }
    }
}

/// Register the `shell` global. See the module docs for the surface.
pub fn register_shell(lua: &Lua, ctx: &Rc<ShellCtx>) -> LuaResult<()> {
    let shell = lua.create_table()?;

    let c = ctx.clone();
    shell.set(
        "window",
        lua.create_function(move |_lua, config: LuaTable| {
            let opts = window::parse_options(&config)?;
            let shared = Rc::new(RefCell::new(WindowShared {
                render_key: None,
                bg: opts.bg,
                text: opts.text,
            }));
            if let Some(name) = opts.name {
                c.named.borrow_mut().insert(name, shared.clone());
            }
            c.pending.borrow_mut().push(PendingWindow {
                options: opts.layer,
                shared: shared.clone(),
            });
            c.mark_dirty();
            Ok(WindowHandle::new(shared, c.clone()))
        })?,
    )?;

    let c = ctx.clone();
    shell.set(
        "state",
        lua.create_function(move |_lua, initial: LuaValue| Ok(LuaState::new(initial, c.clone())))?,
    )?;

    let c = ctx.clone();
    shell.set(
        "interval",
        lua.create_function(move |lua, (ms, f): (u64, LuaFunction)| {
            c.timers
                .borrow_mut()
                .push(PendingTimer::new(lua, ms, true, f)?);
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    shell.set(
        "once",
        lua.create_function(move |lua, (ms, f): (u64, LuaFunction)| {
            c.timers
                .borrow_mut()
                .push(PendingTimer::new(lua, ms, false, f)?);
            Ok(())
        })?,
    )?;

    shell.set(
        "exec",
        lua.create_function(|_lua, cmd: String| exec::run_blocking(&cmd))?,
    )?;

    let c = ctx.clone();
    shell.set(
        "exec_async",
        lua.create_function(move |lua, (cmd, f): (String, LuaFunction)| {
            let id = c.next_exec_id.get();
            c.next_exec_id.set(id + 1);
            c.exec_callbacks.borrow_mut().insert(
                id,
                ExecCallback {
                    weak: lua.weak(),
                    key: lua.create_registry_value(f)?,
                },
            );
            exec::spawn(cmd, id, c.exec_tx.clone());
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    shell.set(
        "watch_file",
        lua.create_function(move |lua, (path, f): (String, LuaFunction)| {
            c.watches.borrow_mut().push(PendingWatch {
                path: path.into(),
                callback: WatchCallback::new(lua, f)?,
            });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    shell.set(
        "reload",
        lua.create_function(move |_lua, ()| {
            c.reload.set(true);
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    shell.set(
        "quit",
        lua.create_function(move |_lua, ()| {
            c.quit.set(true);
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    shell.set(
        "get_window",
        lua.create_function(move |_lua, name: String| {
            Ok(c.named
                .borrow()
                .get(&name)
                .map(|shared| WindowHandle::new(shared.clone(), c.clone())))
        })?,
    )?;

    let c = ctx.clone();
    shell.set(
        "displays",
        lua.create_function(move |lua, ()| {
            let out = lua.create_table()?;
            for (i, d) in c.displays.borrow().iter().enumerate() {
                let t = lua.create_table()?;
                t.set("name", d.name.clone())?;
                t.set("x", d.x)?;
                t.set("y", d.y)?;
                t.set("width", d.width)?;
                t.set("height", d.height)?;
                t.set("scale", d.scale)?;
                // nur's convention: the first display is primary.
                t.set("is_primary", i == 0)?;
                out.set(i + 1, t)?;
            }
            Ok(out)
        })?,
    )?;

    lua.globals().set("shell", shell)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vm;

    fn vm_with_shell() -> (Vm, Rc<ShellCtx>) {
        let vm = Vm::new().unwrap();
        let ctx = ShellCtx::new();
        vm.install_shell(&ctx).unwrap();
        (vm, ctx)
    }

    #[test]
    fn window_queues_pending_and_marks_dirty() {
        let (vm, ctx) = vm_with_shell();
        vm.exec("w = shell.window({ height = 24 })", "t.lua")
            .unwrap();
        assert!(ctx.take_dirty());
        let pending = ctx.take_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].options.height, 24);
        assert!(pending[0].shared.borrow().render_key.is_none());
    }

    #[test]
    fn render_attaches_callback_and_marks_dirty() {
        let (vm, ctx) = vm_with_shell();
        vm.exec("w = shell.window({})", "t.lua").unwrap();
        let pending = ctx.take_pending();
        ctx.take_dirty();
        vm.exec("w:render(function() return ui.hbox({}) end)", "t.lua")
            .unwrap();
        assert!(ctx.take_dirty());
        assert!(pending[0].shared.borrow().render_key.is_some());
    }

    #[test]
    fn take_pending_drains() {
        let (vm, ctx) = vm_with_shell();
        vm.exec("shell.window({}); shell.window({})", "t.lua")
            .unwrap();
        assert_eq!(ctx.take_pending().len(), 2);
        assert!(ctx.take_pending().is_empty());
    }

    #[test]
    fn interval_and_once_queue_timers() {
        let (vm, ctx) = vm_with_shell();
        vm.exec(
            "shell.interval(1000, function() end); shell.once(50, function() end)",
            "t.lua",
        )
        .unwrap();
        let timers = ctx.take_timers();
        assert_eq!(timers.len(), 2);
        assert_eq!(timers[0].delay.as_millis(), 1000);
        assert_eq!(timers[0].period.unwrap().as_millis(), 1000);
        assert_eq!(timers[1].delay.as_millis(), 50);
        assert!(timers[1].period.is_none());
        assert!(ctx.take_timers().is_empty());
    }

    #[test]
    fn timer_fire_calls_the_callback() {
        let (vm, ctx) = vm_with_shell();
        vm.exec(
            "hits = 0; shell.interval(1, function() hits = hits + 1 end)",
            "t.lua",
        )
        .unwrap();
        let t = ctx.take_timers().pop().unwrap();
        assert!(t.fire());
        assert!(t.fire());
        assert_eq!(vm.lua().globals().get::<i64>("hits").unwrap(), 2);
    }

    #[test]
    fn timer_callback_error_keeps_the_timer() {
        let (vm, ctx) = vm_with_shell();
        vm.exec("shell.once(1, function() error('boom') end)", "t.lua")
            .unwrap();
        let t = ctx.take_timers().pop().unwrap();
        assert!(
            t.fire(),
            "a transient callback error must not drop the timer"
        );
    }

    #[test]
    fn timer_fire_after_vm_drop_asks_for_removal() {
        let (vm, ctx) = vm_with_shell();
        vm.exec("shell.interval(1, function() end)", "t.lua")
            .unwrap();
        let t = ctx.take_timers().pop().unwrap();
        drop(vm);
        assert!(!t.fire(), "a dead VM must drop its timers");
    }

    #[test]
    fn exec_captures_trimmed_stdout() {
        let (vm, _ctx) = vm_with_shell();
        vm.exec(r#"out = shell.exec("echo '  hi  '")"#, "t.lua")
            .unwrap();
        assert_eq!(vm.lua().globals().get::<String>("out").unwrap(), "hi");
    }

    #[test]
    fn exec_async_replies_through_the_channel() {
        let (vm, ctx) = vm_with_shell();
        let channel = ctx.take_exec_channel().unwrap();
        assert!(ctx.take_exec_channel().is_none(), "channel is take-once");

        let mut event_loop: calloop::EventLoop<bool> = calloop::EventLoop::try_new().unwrap();
        let c = ctx.clone();
        event_loop
            .handle()
            .insert_source(channel, move |event, _, done: &mut bool| {
                if let calloop::channel::Event::Msg(reply) = event {
                    c.dispatch_exec_reply(reply);
                    *done = true;
                }
            })
            .unwrap();

        vm.exec(
            r#"shell.exec_async("echo async-done", function(out) got = out end)"#,
            "t.lua",
        )
        .unwrap();

        let mut done = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done && std::time::Instant::now() < deadline {
            event_loop
                .dispatch(Some(std::time::Duration::from_millis(50)), &mut done)
                .unwrap();
        }
        assert!(done, "exec_async reply never arrived");
        assert_eq!(
            vm.lua().globals().get::<String>("got").unwrap(),
            "async-done"
        );
    }

    #[test]
    fn quit_raises_the_flag() {
        let (vm, ctx) = vm_with_shell();
        assert!(!ctx.take_quit());
        vm.exec("shell.quit()", "t.lua").unwrap();
        assert!(ctx.take_quit());
        assert!(!ctx.take_quit());
    }

    #[test]
    fn get_window_finds_named_windows_only() {
        let (vm, _ctx) = vm_with_shell();
        vm.exec(
            r#"
            shell.window({ name = "bar" })
            shell.window({})
            found = shell.get_window("bar")
            missing = shell.get_window("nope")
            "#,
            "t.lua",
        )
        .unwrap();
        let g = vm.lua().globals();
        assert!(g.get::<LuaValue>("found").unwrap().is_userdata());
        assert!(g.get::<LuaValue>("missing").unwrap().is_nil());
    }

    #[test]
    fn get_window_render_reaches_the_same_shared_state() {
        let (vm, ctx) = vm_with_shell();
        vm.exec(
            r#"
            shell.window({ name = "bar" })
            shell.get_window("bar"):render(function() return ui.hbox({}) end)
            "#,
            "t.lua",
        )
        .unwrap();
        let pending = ctx.take_pending();
        assert!(pending[0].shared.borrow().render_key.is_some());
    }

    #[test]
    fn displays_reads_the_snapshot() {
        let (vm, ctx) = vm_with_shell();
        ctx.set_displays(vec![DisplayInfo {
            name: "DP-1".into(),
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            scale: 2,
        }]);
        vm.exec(
            r#"
            local d = shell.displays()
            count = #d
            name = d[1].name
            width = d[1].width
            scale = d[1].scale
            primary = d[1].is_primary
            "#,
            "t.lua",
        )
        .unwrap();
        let g = vm.lua().globals();
        assert_eq!(g.get::<i64>("count").unwrap(), 1);
        assert_eq!(g.get::<String>("name").unwrap(), "DP-1");
        assert_eq!(g.get::<i64>("width").unwrap(), 2560);
        assert_eq!(g.get::<i64>("scale").unwrap(), 2);
        assert!(g.get::<bool>("primary").unwrap());
    }

    #[test]
    fn watch_file_queues_a_pending_watch() {
        let (vm, ctx) = vm_with_shell();
        vm.exec("shell.watch_file('/etc/hostname', function() end)", "t.lua")
            .unwrap();
        let watches = ctx.take_watches();
        assert_eq!(watches.len(), 1);
        assert_eq!(watches[0].path, std::path::Path::new("/etc/hostname"));
        assert!(ctx.take_watches().is_empty());
    }

    #[test]
    fn reload_raises_the_flag() {
        let (vm, ctx) = vm_with_shell();
        assert!(!ctx.take_reload());
        vm.exec("shell.reload()", "t.lua").unwrap();
        assert!(ctx.take_reload());
        assert!(!ctx.take_reload());
    }

    #[test]
    fn reset_for_reload_wipes_vm_state() {
        let (vm, ctx) = vm_with_shell();
        vm.exec(
            r#"
            shell.window({ name = "bar" })
            shell.interval(1000, function() end)
            shell.watch_file("x", function() end)
            "#,
            "t.lua",
        )
        .unwrap();
        ctx.reset_for_reload();
        assert!(ctx.take_pending().is_empty());
        assert!(ctx.take_timers().is_empty());
        assert!(ctx.take_watches().is_empty());
        assert!(!ctx.take_dirty());
        vm.exec("missing = shell.get_window('bar')", "t.lua")
            .unwrap();
        assert!(vm
            .lua()
            .globals()
            .get::<LuaValue>("missing")
            .unwrap()
            .is_nil());
    }

    #[test]
    fn state_set_marks_dirty() {
        let (vm, ctx) = vm_with_shell();
        vm.exec("s = shell.state(1)", "t.lua").unwrap();
        ctx.take_dirty(); // creation does not dirty; only set does
        assert!(!ctx.take_dirty());
        vm.exec("s:set(2)", "t.lua").unwrap();
        assert!(ctx.take_dirty());
        vm.exec("v = s:get()", "t.lua").unwrap();
        let v: i64 = vm.lua().globals().get("v").unwrap();
        assert_eq!(v, 2);
    }
}
